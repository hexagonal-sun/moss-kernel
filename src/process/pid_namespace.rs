//! Hierarchical PID identities. Scheduler IDs remain global; userspace IDs
//! are resolved only in the caller's active namespace. Identities, not task
//! pointers, pin zombie, pidfd, process-group and session numbers.
use super::{Task, Tid, thread_group::ThreadGroup, user_namespace::UserNamespace};
use crate::sync::{OnceLock, SpinLock};
use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use libkernel::error::{KernelError, Result};

pub static PID_OPS: SpinLock<()> = SpinLock::new(());
struct State {
    next: u32,
    numbers: BTreeMap<u32, Tid>,
    reverse: BTreeMap<Tid, u32>,
    reaper: Option<Weak<ThreadGroup>>,
    dead: bool,
}
pub struct PidNamespace {
    pub id: u64,
    pub parent: Option<Arc<Self>>,
    pub owner: Arc<UserNamespace>,
    pub level: usize,
    state: SpinLock<State>,
}
impl PidNamespace {
    fn new(parent: Option<Arc<Self>>, owner: Arc<UserNamespace>) -> Arc<Self> {
        Arc::new(Self {
            id: super::namespace::next_namespace_id(),
            level: parent.as_ref().map_or(0, |p| p.level + 1),
            parent,
            owner,
            state: SpinLock::new(State {
                next: 1,
                numbers: BTreeMap::new(),
                reverse: BTreeMap::new(),
                reaper: None,
                dead: false,
            }),
        })
    }
    pub fn initial() -> Arc<Self> {
        static INITIAL: OnceLock<Arc<PidNamespace>> = OnceLock::new();
        INITIAL
            .get_or_init(|| Self::new(None, UserNamespace::initial()))
            .clone()
    }
    pub fn create(parent: Arc<Self>, owner: Arc<UserNamespace>) -> Result<Arc<Self>> {
        if parent.level >= 32 {
            return Err(KernelError::NoSpace);
        }
        let mut user = owner.as_ref();
        while user != parent.owner.as_ref() {
            user = user.parent.as_deref().ok_or(KernelError::InvalidValue)?;
        }
        Ok(Self::new(Some(parent), owner))
    }
    pub fn within(&self, ancestor: &Self) -> bool {
        let mut ns = self;
        loop {
            if ns.id == ancestor.id {
                return true;
            }
            let Some(parent) = ns.parent.as_deref() else {
                return false;
            };
            ns = parent;
        }
    }
    pub fn resolve(&self, number: u32) -> Option<Tid> {
        self.state.lock_save_irq().numbers.get(&number).copied()
    }
    pub fn visible(&self, global: Tid) -> u32 {
        self.state
            .lock_save_irq()
            .reverse
            .get(&global)
            .copied()
            .unwrap_or(0)
    }
    pub fn reaper(&self) -> Option<Arc<ThreadGroup>> {
        self.state
            .lock_save_irq()
            .reaper
            .as_ref()
            .and_then(Weak::upgrade)
    }
    pub fn initialized(&self) -> bool {
        self.state.lock_save_irq().reaper.is_some()
    }
    /// Called with PID_OPS during publication, after every fallible clone step.
    pub fn publish(&self, process: &Arc<ThreadGroup>) {
        if process.pid.local() == 1 {
            self.state.lock_save_irq().reaper = Some(Arc::downgrade(process));
        }
    }
    pub fn live(&self) -> bool {
        !self.state.lock_save_irq().dead && self.parent.as_ref().is_none_or(|p| p.live())
    }
    pub fn disable(&self) {
        let _op = PID_OPS.lock_save_irq();
        self.state.lock_save_irq().dead = true;
    }
}

pub struct PidIdentity {
    pub global: Tid,
    numbers: Vec<(Arc<PidNamespace>, u32)>,
}
impl PidIdentity {
    pub fn allocate(global: Tid, ns: Arc<PidNamespace>) -> Result<Arc<Self>> {
        let _op = PID_OPS.lock_save_irq();
        if global.0 >= i32::MAX as u32 {
            return Err(KernelError::TryAgain);
        }
        if !ns.live() {
            return Err(KernelError::NoMemory);
        }
        let mut ancestors = Vec::new();
        let mut current = Some(ns);
        while let Some(ns) = current {
            current = ns.parent.clone();
            ancestors.push(ns);
        }
        for ns in &ancestors {
            let s = ns.state.lock_save_irq();
            if ns.level != 0 && s.next != 1 && s.reaper.is_none() {
                return Err(KernelError::TryAgain);
            }
            if s.next >= i32::MAX as u32 {
                return Err(KernelError::TryAgain);
            }
        }
        let mut numbers = Vec::new();
        for ns in ancestors.into_iter().rev() {
            let number = {
                let mut state = ns.state.lock_save_irq();
                let n = if ns.level == 0 {
                    global.0
                } else {
                    let n = state.next;
                    state.next += 1;
                    n
                };
                if n != 0 {
                    state.numbers.insert(n, global);
                    state.reverse.insert(global, n);
                }
                n
            };
            numbers.push((ns, number));
        }
        Ok(Arc::new(Self { global, numbers }))
    }
    pub fn initial(global: Tid) -> Arc<Self> {
        Self::allocate(global, PidNamespace::initial()).expect("initial PID allocation")
    }
    pub fn namespace(&self) -> Arc<PidNamespace> {
        self.numbers.last().unwrap().0.clone()
    }
    pub fn local(&self) -> u32 {
        self.numbers.last().unwrap().1
    }
    pub fn in_ns(&self, ns: &PidNamespace) -> u32 {
        self.numbers
            .iter()
            .find(|(n, _)| n.id == ns.id)
            .map_or(0, |(_, p)| *p)
    }
    pub fn hierarchy(&self, ns: &PidNamespace) -> Vec<u32> {
        self.numbers
            .iter()
            .skip_while(|(n, _)| n.id != ns.id)
            .map(|(_, p)| *p)
            .collect()
    }
}
impl Drop for PidIdentity {
    fn drop(&mut self) {
        for (ns, n) in &self.numbers {
            if *n == 0 {
                continue;
            }
            let mut state = ns.state.lock_save_irq();
            state.numbers.remove(n);
            state.reverse.remove(&self.global);
            if *n == 1 && !state.dead && state.reaper.is_none() {
                state.next = 1;
            }
        }
    }
}
pub fn find_task(task: &Task, number: u32) -> Option<Arc<crate::sched::sched_task::Work>> {
    super::find_task_by_tid(task.pid_ns().resolve(number)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use moss_macros::ktest;
    #[ktest]
    fn pid_identity_reservation_rolls_back_without_an_init() {
        let ns = PidNamespace::create(PidNamespace::initial(), UserNamespace::initial()).unwrap();
        let global = Tid::next_tid();
        let pid = PidIdentity::allocate(global, ns.clone()).unwrap();
        assert_eq!(pid.local(), 1);
        assert_eq!(pid.in_ns(&PidNamespace::initial()), global.0);
        let sibling =
            PidNamespace::create(PidNamespace::initial(), UserNamespace::initial()).unwrap();
        assert_eq!(pid.in_ns(&sibling), 0);
        assert!(matches!(
            PidIdentity::allocate(Tid::next_tid(), ns.clone()),
            Err(KernelError::TryAgain)
        ));
        drop(pid);
        assert_eq!(ns.resolve(1), None);
        assert_eq!(
            PidIdentity::allocate(Tid::next_tid(), ns.clone())
                .unwrap()
                .local(),
            1
        );
    }
    #[ktest]
    fn dead_pid_namespace_and_descendants_reject_allocations() {
        let ns = PidNamespace::create(PidNamespace::initial(), UserNamespace::initial()).unwrap();
        let descendant = PidNamespace::create(ns.clone(), UserNamespace::initial()).unwrap();
        assert!(descendant.within(&ns));
        assert!(!ns.within(&descendant));
        ns.disable();
        assert!(matches!(
            PidIdentity::allocate(Tid::next_tid(), ns),
            Err(KernelError::NoMemory)
        ));
        assert!(matches!(
            PidIdentity::allocate(Tid::next_tid(), descendant),
            Err(KernelError::NoMemory)
        ));
    }
}
