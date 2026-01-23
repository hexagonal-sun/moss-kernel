use libkernel::memory::address::TUA;

use super::{SigId, SigSet, sigaction::SigActionFlags};

#[derive(Clone, Copy, Debug)]
pub struct UserspaceSigAction {
    pub action: TUA<extern "C" fn(i32)>,
    pub restorer: Option<TUA<extern "C" fn()>>,
    pub flags: SigActionFlags,
    pub mask: SigSet,
}

#[derive(Clone, Copy, Debug)]
/// How the kernel should respond to a signal.
pub enum KSignalAction {
    Term,
    Core,
    Stop,
    Continue,
    Userspace(SigId, UserspaceSigAction),
}

impl KSignalAction {
    /// Returns the default action for a given signal.
    ///
    /// For signals whose default is to be ignored, `None` is returned.
    pub const fn default_action(signal: SigId) -> Option<Self> {
        match signal {
            SigId::SIGABRT => Some(Self::Core),
            SigId::SIGALRM => Some(Self::Term),
            SigId::SIGBUS => Some(Self::Core),
            SigId::SIGCHLD => None,
            SigId::SIGCONT => Some(Self::Continue),
            SigId::SIGFPE => Some(Self::Core),
            SigId::SIGHUP => Some(Self::Term),
            SigId::SIGILL => Some(Self::Core),
            SigId::SIGINT => Some(Self::Term),
            SigId::SIGIO => Some(Self::Term),
            SigId::SIGKILL => Some(Self::Term),
            SigId::SIGPIPE => Some(Self::Term),
            SigId::SIGPROF => Some(Self::Term),
            SigId::SIGPWR => Some(Self::Term),
            SigId::SIGQUIT => Some(Self::Core),
            SigId::SIGSEGV => Some(Self::Core),
            SigId::SIGSTKFLT => Some(Self::Term),
            SigId::SIGSTOP => Some(Self::Stop),
            SigId::SIGTSTP => Some(Self::Stop),
            SigId::SIGTERM => Some(Self::Term),
            SigId::SIGTRAP => Some(Self::Core),
            SigId::SIGTTIN => Some(Self::Stop),
            SigId::SIGTTOU => Some(Self::Stop),
            SigId::SIGUNUSED => Some(Self::Core),
            SigId::SIGURG => None,
            SigId::SIGUSR1 => Some(Self::Term),
            SigId::SIGUSR2 => Some(Self::Term),
            SigId::SIGVTALRM => Some(Self::Term),
            SigId::SIGXCPU => Some(Self::Core),
            SigId::SIGXFSZ => Some(Self::Core),
            SigId::SIGWINCH => None,
        }
    }
}
