use core::future::poll_fn;
use core::task::{Poll, Waker};

use crate::arch::{Arch, ArchImpl};
use crate::fs::syscalls::iov::IoVec;
use crate::memory::uaccess::{copy_from_user, copy_to_user};
use crate::process::TASK_LIST;
use crate::process::thread_group::signal::SigId;
use crate::sched::current::{current_task, current_task_shared};
use alloc::sync::Arc;
use bitflags::Flags;
use libkernel::error::{KernelError, Result};
use libkernel::memory::address::UA;
use log::warn;

type GpRegs = <ArchImpl as Arch>::PTraceGpRegs;

use super::TaskState;
use super::thread_group::ThreadGroup;
use super::thread_group::wait::ChildState;

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq)]
    pub struct TracePoint: u32 {
        const SyscallEntry = 0x01;
        const SyscallExit  = 0x02;
        /// A new process has begin tracing after being `exec()`d.
        const Exec         = 0x08;
        const Clone        = 0x10;
        const Exit         = 0x20;
        const Fork         = 0x40;
    }
}

#[derive(Clone)]
enum PTraceState {
    /// The traced program should run until `break_points`.
    Running,
    /// The program hit a trace point `TracePoint`,
    TracePointHit {
        reg_set: GpRegs,
        hit_point: TracePoint,
    },
    /// A signal was sent to the traced task.
    SignalTrap { reg_set: GpRegs, signal: SigId },
}

#[derive(Clone)]
pub struct PTrace {
    break_points: TracePoint,
    state: Option<PTraceState>,
    waker: Option<Waker>,
}

impl PTrace {
    pub fn new() -> Self {
        Self {
            state: None,
            break_points: TracePoint::empty(),
            waker: None,
        }
    }

    pub fn is_being_traced(&self) -> bool {
        self.state.is_some()
    }

    /// Tells ptrace that the task has hit one of the trace points in the
    /// kernel. If tracing is in progress *and* the trace point is active within
    /// `break_points`, `true` is returned and the kernel should yield to allow
    /// the tracer to be informed. Otherwise, `false` is returned.
    pub fn hit_trace_point(
        &mut self,
        point: TracePoint,
        regs: &<ArchImpl as Arch>::UserContext,
    ) -> bool {
        let should_stop = match self.state {
            Some(PTraceState::Running) => self.break_points.contains(point),
            _ => false,
        };

        if should_stop {
            self.state = Some(PTraceState::TracePointHit {
                reg_set: regs.into(),
                hit_point: point,
            })
        }

        should_stop
    }

    /// Calculate what extra bits to set (mask) in the status flag of the tracer
    /// upon return of `wait()`.
    fn calc_trace_point_mask(&self) -> i32 {
        match self.state {
            None => 0,
            Some(PTraceState::Running) => 0,
            // No masking for real signal delivery.
            Some(PTraceState::SignalTrap { .. }) => 0,
            Some(PTraceState::TracePointHit { hit_point, .. }) => match hit_point {
                TracePoint::SyscallEntry | TracePoint::SyscallExit => {
                    if self.sysgood {
                        0x80
                    } else {
                        0
                    }
                }
                TracePoint::Exec => (PTRACE_EVENT_EXEC as i32) << 8,
                TracePoint::Clone => (PTRACE_EVENT_CLONE as i32) << 8,
                TracePoint::Exit => (PTRACE_EVENT_EXIT as i32) << 8,
                TracePoint::Fork => (PTRACE_EVENT_FORK as i32) << 8,
                _ => unreachable!(),
            },
        }
    }

    /// Notify parents of a trap event.
    pub fn notify_parent_of_trap(&self, process: Arc<ThreadGroup>) {
        let Some(trap_signal) = (match self.state {
            // For non-signal trace events, we use SIGTRAP.
            Some(PTraceState::TracePointHit { hit_point, .. }) => match hit_point {
                TracePoint::Exec => Some(SigId::SIGSTOP),
                _ => Some(SigId::SIGTRAP),
            },
            Some(PTraceState::SignalTrap { signal, .. }) => Some(signal),
            _ => None,
        }) else {
            warn!("notification of parent failed when in non-traced state");
            return;
        };

        // Notify the parent that we have stopped (SIGCHLD).
        if let Some(parent) = process
            .parent
            .lock_save_irq()
            .as_ref()
            .and_then(|p| p.upgrade())
        {
            parent.child_notifiers.child_update(
                process.tgid,
                ChildState::TraceTrap {
                    signal: trap_signal,
                    mask: self.calc_trace_point_mask(),
                },
            );

            parent
                .pending_signals
                .lock_save_irq()
                .set_signal(SigId::SIGCHLD);
        }
    }

    pub fn set_waker(&mut self, waker: Waker) {
        // Ensure we never override an already existing waker.
        debug_assert!(self.waker.is_none());

        self.waker = Some(waker);
    }

    /// Notify ptrace that a signal has been delivered for the task.
    ///
    /// This function returns `true` if the task should be put to sleep and wait
    /// for the tracer, `false` if the signal should be delivered as per-ususal.
    pub fn trace_signal(&mut self, signal: SigId, regs: &<ArchImpl as Arch>::UserContext) -> bool {
        // Never handle a SIGKILL.
        if signal == SigId::SIGKILL {
            return false;
        }

        let should_stop = matches!(self.state, Some(PTraceState::Running));

        if should_stop {
            self.state = Some(PTraceState::SignalTrap {
                reg_set: regs.into(),
                signal,
            })
        }

        should_stop
    }

    /// Returns the current GP regset when the program has been halted.
    pub fn regset(&self) -> Option<GpRegs> {
        match self.state.as_ref()? {
            PTraceState::Running => None,
            PTraceState::TracePointHit { reg_set, .. } => Some(*reg_set),
            PTraceState::SignalTrap { reg_set, .. } => Some(*reg_set),
        }
    }
}

#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PtraceOperation {
    TraceMe = 0,
    PeekText = 1,
    PeekData = 2,
    // PeekUser = 3,
    // PokeText = 4,
    // PokeData = 5,
    // PokeUser = 6,
    Cont = 7,
    // Kill = 8,
    // SingleStep = 9,
    // GetRegs = 12,
    // SetRegs = 13,
    // GetFpRegs = 14,
    // SetFpRegs = 15,
    // Attach = 16,
    // Detach = 17,
    Syscall = 24,
    GetRegSet = 0x4204,
}

impl TryFrom<i32> for PtraceOperation {
    type Error = KernelError;

    fn try_from(value: i32) -> Result<Self> {
        match value {
            0 => Ok(PtraceOperation::TraceMe),
            1 => Ok(PtraceOperation::PeekText),
            2 => Ok(PtraceOperation::PeekData),
            0x4204 => Ok(PtraceOperation::GetRegSet),
            // TODO: Should be EIO
            _ => Err(KernelError::InvalidValue),
        }
    }
}

pub async fn ptrace_stop(point: TracePoint) {
    let task_sh = current_task_shared();
    {
        let mut ptrace = task_sh.ptrace.lock_save_irq();

        if ptrace.hit_trace_point(point, current_task().ctx.user()) {
            ptrace.notify_parent_of_trap(task_sh.process.clone());
        } else {
            return;
        }
    }

    poll_fn(|cx| {
        let mut ptrace = task_sh.ptrace.lock_save_irq();

        if matches!(ptrace.state, Some(PTraceState::Running)) {
            Poll::Ready(())
        } else {
            ptrace.set_waker(cx.waker().clone());
            Poll::Pending
        }
    })
    .await;
}

pub async fn sys_ptrace(op: i32, pid: u64, addr: UA, data: UA) -> Result<usize> {
    let op = PtraceOperation::try_from(op)?;

    if op == PtraceOperation::TraceMe {
        let current_task = current_task_shared();
        let mut ptrace = current_task.ptrace.lock_save_irq();

        ptrace.state = Some(PTraceState::Running);

        // Set default breakpoint for TraceMe.
        ptrace.break_points = TracePoint::Exec;

        return Ok(0);
    }

    let target_task = {
        TASK_LIST
            .lock_save_irq()
            .iter()
            .find(|(desc, _)| desc.tid.value() == pid as u32)
            .and_then(|(_, task)| task.upgrade())
            .ok_or(KernelError::NoProcess)?
    };

    // TODO: Check CAP_SYS_PTRACE & security
    match op {
        PtraceOperation::TraceMe => {
            unreachable!();
        }
        PtraceOperation::GetRegSet => {
            let regs = target_task.ptrace.lock_save_irq().regset();

            if addr.value() != 1 {
                // TODO: Suppoer other reg sets, vector, VFP, etc...
                return Err(KernelError::InvalidValue);
            }

            let user_iov = data.cast::<IoVec>();

            let mut iov = copy_from_user(user_iov).await?;

            if iov.iov_len < size_of::<GpRegs>() {
                return Err(KernelError::InvalidValue);
            }

            if let Some(regs) = regs {
                copy_to_user(iov.iov_base.cast::<GpRegs>(), regs).await?;
                iov.iov_len = size_of::<GpRegs>();
                copy_to_user(user_iov, iov).await?;

                Ok(0)
            } else {
                Err(KernelError::NoProcess)
            }
        }
        // TODO: Wrong error
        _ => Err(KernelError::InvalidValue),
    }
}
