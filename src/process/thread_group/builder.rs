use core::sync::atomic::AtomicU32;

use alloc::{collections::btree_map::BTreeMap, sync::Arc};

use crate::sync::SpinLock;

use super::{
    Pgid, ProcessState, Sid, TG_LIST, Tgid, ThreadGroup,
    rsrc_lim::ResourceLimits,
    signal::{SigSet, SignalState},
    wait::ChildNotifiers,
};

/// A builder for creating ThreadGroup instances.
pub struct ThreadGroupBuilder {
    tgid: Tgid,
    parent: Option<Arc<ThreadGroup>>,
    umask: Option<u32>,
    sigstate: Option<Arc<SpinLock<SignalState>>>,
    rsrc_lim: Option<Arc<SpinLock<ResourceLimits>>>,
}

impl ThreadGroupBuilder {
    /// Creates a new ThreadGroupBuilder with the mandatory thread group ID.
    pub fn new(tgid: Tgid) -> Self {
        ThreadGroupBuilder {
            tgid,
            parent: None,
            umask: None,
            sigstate: None,
            rsrc_lim: None,
        }
    }

    /// Sets the parent of the thread group.
    pub fn with_parent(mut self, parent: Arc<ThreadGroup>) -> Self {
        self.parent = Some(parent);
        self
    }

    /// Sets the signal state of the thread group.
    pub fn with_sigstate(mut self, sigstate: Arc<SpinLock<SignalState>>) -> Self {
        self.sigstate = Some(sigstate);
        self
    }

    pub fn with_rsrc_lim(mut self, rsrc_lim: Arc<SpinLock<ResourceLimits>>) -> Self {
        self.rsrc_lim = Some(rsrc_lim);
        self
    }

    /// Builds the ThreadGroup.
    ///
    /// If a sigstate has not been provided, a default one will be created.
    pub fn build(self) -> Arc<ThreadGroup> {
        let ret = Arc::new(ThreadGroup {
            tgid: self.tgid,
            pgid: SpinLock::new(Pgid(self.tgid.value())),
            sid: SpinLock::new(Sid(self.tgid.value())),
            parent: SpinLock::new(self.parent.as_ref().map(Arc::downgrade)),
            umask: SpinLock::new(self.umask.unwrap_or(0)),
            children: SpinLock::new(BTreeMap::new()),
            signals: self
                .sigstate
                .unwrap_or_else(|| Arc::new(SpinLock::new(SignalState::new_default()))),
            rsrc_lim: self
                .rsrc_lim
                .unwrap_or_else(|| Arc::new(SpinLock::new(ResourceLimits::default()))),
            pending_signals: SpinLock::new(SigSet::empty()),
            child_notifiers: ChildNotifiers::new(),
            // Don't start from '0'. Since clone expects the parent to return
            // the tid and the child to return '0', if we started from '0' we
            // couldn't then differentiate between a child and a parent.
            next_tid: AtomicU32::new(1),
            state: SpinLock::new(ProcessState::Running),
            tasks: SpinLock::new(BTreeMap::new()),
        });

        TG_LIST
            .lock_save_irq()
            .insert(self.tgid, Arc::downgrade(&ret));

        ret
    }
}
