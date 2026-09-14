use super::{
    Tgid,
    pid::PidT,
    signal::{InterruptResult, Interruptable, SigId},
};
use crate::{
    clock::timespec::TimeSpec,
    memory::uaccess::{UserCopyable, copy_to_user},
    process::{Task, Tid, pid_namespace::PidIdentity},
    sched::syscall_ctx::ProcessCtx,
    sync::CondVar,
};
use alloc::{collections::BTreeMap, sync::Arc};
use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
    sync::condvar::WakeupType,
};
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RUsage {
    pub ru_utime: TimeSpec, // user time used
    pub ru_stime: TimeSpec, // system time used
    pub ru_maxrss: i64,     // maximum resident set size
    pub ru_ixrss: i64,      // integral shared memory size
    pub ru_idrss: i64,      // integral unshared data size
    pub ru_isrss: i64,      // integral unshared stack size
    pub ru_minflt: i64,     // page reclaims
    pub ru_majflt: i64,     // page faults
    pub ru_nswap: i64,      // swaps
    pub ru_inblock: i64,    // block input operations
    pub ru_oublock: i64,    // block output operations
    pub ru_msgsnd: i64,     // messages sent
    pub ru_msgrcv: i64,     // messages received
    pub ru_nsignals: i64,   // signals received
    pub ru_nvcsw: i64,      // voluntary context switches
    pub ru_nivcsw: i64,     // involuntary context switches
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct WaitFlags: u32 {
       const WNOHANG    = 0x00000001;
       const WSTOPPED   = 0x00000002;
       const WEXITED    = 0x00000004;
       const WCONTINUED = 0x00000008;
       const WNOWAIT    = 0x01000000;
       const WNOTHREAD  = 0x20000000;
       const WALL       = 0x40000000;
       const WCLONE     = 0x80000000;
    }
}

// AArch64 siginfo_t: the union starts at offset 16 and occupies 112 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SigInfo {
    signo: i32,
    errno: i32,
    code: i32,
    pad: i32,
    pid: i32,
    uid: u32,
    status: i32,
    pad2: i32,
    utime: u64,
    stime: u64,
    padding: [u8; 80],
}
unsafe impl UserCopyable for SigInfo {}
const _: () = assert!(core::mem::size_of::<SigInfo>() == 128);

#[derive(Clone, Copy, Debug)]
pub enum ChildState {
    NormalExit { code: u32 },
    SignalExit { signal: SigId, core: bool },
    Stop { signal: SigId },
    Continue,
}
#[derive(Clone, Copy, Debug)]
pub struct TraceTrap {
    signal: SigId,
    mask: i32,
}
impl TraceTrap {
    pub fn new(signal: SigId, mask: i32) -> Self {
        Self { signal, mask }
    }
}
#[derive(Clone, Copy)]
enum WaitEvent {
    Child(ChildState),
    Ptrace(TraceTrap),
}
#[derive(Clone)]
struct Event {
    pid: Arc<PidIdentity>,
    lifetime: Arc<crate::process::pid_namespace::ProcessLifetime>,
    pgid: Arc<PidIdentity>,
    uid: libkernel::proc::ids::Uid,
    event: WaitEvent,
}
#[derive(Clone, Copy)]
enum Selection {
    Any,
    Process(u32),
    Group(Tid),
}
impl Selection {
    fn group(task: &Task, number: u32) -> Result<Self> {
        Ok(Self::Group(if number == 0 {
            Tid(task.process.pgid.lock_save_irq().0)
        } else {
            task.pid_ns()
                .resolve(number)
                .ok_or(KernelError::NoChildProcess)?
        }))
    }
    fn matches(self, task: &Task, pid: &PidIdentity, pgid: &PidIdentity) -> bool {
        let visible = pid.in_ns(&task.pid_ns());
        visible != 0
            && match self {
                Self::Any => true,
                Self::Process(p) => visible == p,
                Self::Group(p) => pgid.global == p,
            }
    }
}
impl Event {
    fn is_exit(&self) -> bool {
        matches!(
            self.event,
            WaitEvent::Child(ChildState::NormalExit { .. } | ChildState::SignalExit { .. })
        )
    }
    fn reap(&self) {
        if self.is_exit() {
            self.lifetime.reap();
        }
    }
    fn selected(&self, task: &Task, selection: Selection) -> bool {
        selection.matches(task, &self.pid, &self.pgid)
    }
    fn eligible(&self, flags: WaitFlags) -> bool {
        match self.event {
            WaitEvent::Ptrace(_) => true,
            WaitEvent::Child(ChildState::NormalExit { .. } | ChildState::SignalExit { .. }) => {
                flags.contains(WaitFlags::WEXITED)
            }
            WaitEvent::Child(ChildState::Stop { .. }) => flags.contains(WaitFlags::WSTOPPED),
            WaitEvent::Child(ChildState::Continue) => flags.contains(WaitFlags::WCONTINUED),
        }
    }
    fn status(&self) -> i32 {
        match self.event {
            WaitEvent::Child(ChildState::NormalExit { code }) => (code as i32 & 255) << 8,
            WaitEvent::Child(ChildState::SignalExit { signal, core }) => {
                signal.user_id() as i32 | if core { 128 } else { 0 }
            }
            WaitEvent::Child(ChildState::Stop { signal }) => {
                ((signal.user_id() as i32) << 8) | 0x7f
            }
            WaitEvent::Ptrace(TraceTrap { signal, mask }) => {
                ((signal.user_id() as i32) << 8) | 0x7f | mask << 8
            }
            WaitEvent::Child(ChildState::Continue) => 0xffff,
        }
    }
    fn info(&self, task: &Task) -> SigInfo {
        let (code, status) = match self.event {
            WaitEvent::Child(ChildState::NormalExit { code }) => (1, code as i32 & 255),
            WaitEvent::Child(ChildState::SignalExit { signal, core }) => {
                (if core { 3 } else { 2 }, signal.user_id() as i32)
            }
            WaitEvent::Child(ChildState::Stop { signal }) => (5, signal.user_id() as i32),
            WaitEvent::Ptrace(TraceTrap { signal, .. }) => (4, signal.user_id() as i32),
            WaitEvent::Child(ChildState::Continue) => (6, SigId::SIGCONT.user_id() as i32),
        };
        SigInfo {
            signo: SigId::SIGCHLD.user_id() as i32,
            errno: 0,
            code,
            pad: 0,
            pid: self.pid.in_ns(&task.pid_ns()) as i32,
            uid: task.creds.lock_save_irq().user_ns().show_uid(self.uid),
            status,
            pad2: 0,
            utime: 0,
            stime: 0,
            padding: [0; 80],
        }
    }
}
struct NotifierState {
    autoreap: bool,
    children: BTreeMap<Tgid, Event>,
    ptrace: BTreeMap<Tid, Event>,
}
pub struct Notifiers {
    inner: CondVar<NotifierState>,
}
impl Default for Notifiers {
    fn default() -> Self {
        Self::new()
    }
}
impl Notifiers {
    pub fn new() -> Self {
        Self {
            inner: CondVar::new(NotifierState {
                autoreap: false,
                children: BTreeMap::new(),
                ptrace: BTreeMap::new(),
            }),
        }
    }
    pub fn child_update(&self, task: &Task, state: ChildState) {
        let event = Event {
            pid: task.process.pid.clone(),
            lifetime: task.process.lifetime.clone(),
            pgid: task.process.pgid_ref.lock_save_irq().clone(),
            uid: task.creds.lock_save_irq().uid(),
            event: WaitEvent::Child(state),
        };
        self.inner.update(|s| {
            // wait checks this map and the live-child list under inner's
            // lock. Publish exit and remove the live entry in that order's
            // same transaction, avoiding a transient false ECHILD on SMP.
            if event.is_exit()
                && let Some(parent) = task
                    .process
                    .parent
                    .lock_save_irq()
                    .as_ref()
                    .and_then(|p| p.upgrade())
            {
                parent.children.lock_save_irq().remove(&task.process.tgid);
            }
            if s.autoreap {
                s.children.remove(&task.process.tgid);
                event.reap();
                return WakeupType::All;
            }
            s.children.insert(task.process.tgid, event);
            WakeupType::All
        });
    }
    pub fn ptrace_notify(&self, tid: Tid, trap: TraceTrap) {
        let Some(task) = crate::process::find_task_by_tid(tid) else {
            return;
        };
        let event = Event {
            pid: task.pid.clone(),
            lifetime: task.process.lifetime.clone(),
            pgid: task.process.pgid_ref.lock_save_irq().clone(),
            uid: task.creds.lock_save_irq().uid(),
            event: WaitEvent::Ptrace(trap),
        };
        self.inner.update(|s| {
            s.ptrace.insert(tid, event);
            WakeupType::All
        });
    }
    pub fn begin_shutdown(&self) {
        self.inner.update(|s| {
            s.autoreap = true;
            for event in s.children.values() {
                event.reap();
            }
            s.children.clear();
            s.ptrace.clear();
            WakeupType::All
        });
    }
    /// An external setns parent can own children in multiple PID namespaces.
    /// Reparent each zombie to its own nearest reaper, not the parent's init.
    /// The caller holds PID_OPS to serialize with live-child reparenting.
    pub fn reparent(&self, exiting: Tgid) {
        let mut events = None;
        self.inner.update(|s| {
            events = Some(core::mem::take(&mut s.children));
            WakeupType::All
        });
        for (tgid, event) in events.unwrap() {
            let target = crate::process::exit::reaper_for(&event.pid, exiting);
            target.child_notifiers.inner.update(|s| {
                if s.autoreap {
                    event.reap();
                } else {
                    s.children.insert(tgid, event);
                }
                WakeupType::All
            });
            target.queue_signal(SigId::SIGCHLD);
        }
    }
}
fn select_map<K: Ord + Copy>(
    map: &mut BTreeMap<K, Event>,
    task: &Task,
    pid: Selection,
    flags: WaitFlags,
) -> Option<Event> {
    let key = map
        .iter()
        .find(|(_, e)| e.selected(task, pid) && e.eligible(flags))
        .map(|(k, _)| *k)?;
    if flags.contains(WaitFlags::WNOWAIT) {
        map.get(&key).cloned()
    } else {
        let event = map.remove(&key)?;
        event.reap();
        Some(event)
    }
}
fn matching_children(task: &Task, pid: Selection) -> bool {
    task.process
        .children
        .lock_save_irq()
        .values()
        .any(|p| pid.matches(task, &p.pid, &p.pgid_ref.lock_save_irq()))
}
async fn wait(ctx: &ProcessCtx, pid: Selection, flags: WaitFlags) -> Result<Option<Event>> {
    let task = ctx.shared();
    let result = task
        .process
        .child_notifiers
        .inner
        .wait_until(|state| {
            if let Some(event) = select_map(&mut state.ptrace, task, pid, flags)
                .or_else(|| select_map(&mut state.children, task, pid, flags))
            {
                return Some(Ok(Some(event)));
            }
            if !matching_children(task, pid) {
                return Some(Err(KernelError::NoChildProcess));
            }
            if flags.contains(WaitFlags::WNOHANG) {
                Some(Ok(None))
            } else {
                None
            }
        })
        .interruptable()
        .await;
    match result {
        InterruptResult::Interrupted => Err(KernelError::Interrupted),
        InterruptResult::Uninterrupted(r) => r,
    }
}
pub async fn sys_wait4(
    ctx: &ProcessCtx,
    pid: PidT,
    stat_addr: TUA<i32>,
    options: u32,
    rusage: TUA<RUsage>,
) -> Result<usize> {
    let allowed = WaitFlags::WNOHANG
        | WaitFlags::WSTOPPED
        | WaitFlags::WCONTINUED
        | WaitFlags::WNOTHREAD
        | WaitFlags::WCLONE
        | WaitFlags::WALL;
    if options & !allowed.bits() != 0 {
        return Err(KernelError::InvalidValue);
    }
    if !rusage.is_null() {
        return Err(KernelError::NotSupported);
    }
    if pid == i32::MIN {
        return Err(KernelError::NoProcess);
    }
    let pid = match pid {
        -1 => Selection::Any,
        0 => Selection::group(ctx.shared(), 0)?,
        p if p < -1 => Selection::group(ctx.shared(), p.unsigned_abs())?,
        p => Selection::Process(p as u32),
    };
    let flags = WaitFlags::from_bits_retain(options) | WaitFlags::WEXITED;
    let Some(event) = wait(ctx, pid, flags).await? else {
        return Ok(0);
    };
    if !stat_addr.is_null() {
        copy_to_user(stat_addr, event.status()).await?;
    }
    Ok(event.pid.in_ns(&ctx.shared().pid_ns()) as usize)
}
pub async fn sys_waitid(
    ctx: &ProcessCtx,
    idtype: i32,
    id: PidT,
    infop: TUA<SigInfo>,
    options: u32,
    rusage: TUA<RUsage>,
) -> Result<usize> {
    let allowed = WaitFlags::WNOHANG
        | WaitFlags::WSTOPPED
        | WaitFlags::WCONTINUED
        | WaitFlags::WEXITED
        | WaitFlags::WNOWAIT;
    let flags = WaitFlags::from_bits_retain(options);
    if options & !allowed.bits() != 0
        || !flags.intersects(WaitFlags::WSTOPPED | WaitFlags::WCONTINUED | WaitFlags::WEXITED)
    {
        return Err(KernelError::InvalidValue);
    }
    let pid = match idtype {
        0 => Selection::Any,
        1 if id > 0 => Selection::Process(id as u32),
        2 if id >= 0 => Selection::group(ctx.shared(), id as u32)?,
        _ => return Err(KernelError::InvalidValue),
    };
    if !rusage.is_null() {
        return Err(KernelError::NotSupported);
    }
    let event = wait(ctx, pid, flags).await?;
    // Linux accepts a null infop. For WNOHANG, zero the entire ABI object.
    if !infop.is_null() {
        let info = event
            .as_ref()
            .map(|e| e.info(ctx.shared()))
            .unwrap_or(SigInfo {
                signo: 0,
                errno: 0,
                code: 0,
                pad: 0,
                pid: 0,
                uid: 0,
                status: 0,
                pad2: 0,
                utime: 0,
                stime: 0,
                padding: [0; 80],
            });
        copy_to_user(infop, info).await?;
    }
    Ok(0)
}
