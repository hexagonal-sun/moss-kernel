//! Handling of timers and other asynchronous callbacks
//!
//! We represent timer data as an u64:
//! u16 namespace, u16 unused, u32 id
//!
//! This is used by the callback to identify the relevant timer. It is also used to remove timers

/// Not a timer, generic callback
/// Catch all for all invalid namespaces.
const NAMESPACE_NONE: u16 = 0;
const NAMESPACE_ITIMER: u16 = 1;
const NAMESPACE_TIMERFD: u16 = 2;
const NAMESPACE_TIMER: u16 = 3;

#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(u16)]
pub enum TimerNamespace {
    None = NAMESPACE_NONE,
    ITimer = NAMESPACE_ITIMER,
    TimerFd = NAMESPACE_TIMERFD,
    Timer = NAMESPACE_TIMER,
}

impl From<u16> for TimerNamespace {
    fn from(value: u16) -> Self {
        match value {
            NAMESPACE_NONE => Self::None,
            NAMESPACE_ITIMER => Self::ITimer,
            NAMESPACE_TIMERFD => Self::TimerFd,
            NAMESPACE_TIMER => Self::Timer,
            _ => Self::None, // Default to None for invalid namespaces
        }
    }
}

pub fn make_timer_id(namespace: TimerNamespace, id: u32) -> u64 {
    (namespace as u64) << 48 | (id as u64)
}

pub fn parse_timer_id(timer_id: u64) -> (TimerNamespace, u32) {
    let namespace = TimerNamespace::from((timer_id >> 48) as u16);
    let id = (timer_id & 0xFFFF_FFFF) as u32;
    (namespace, id)
}

#[cfg(test)]
mod tests {
    use crate::clock::timer::{TimerNamespace, make_timer_id, parse_timer_id};
    use moss_macros::ktest;

    #[ktest]
    fn test_timer_id_encoding() {
        let namespace = TimerNamespace::ITimer;
        let id = 42;
        let timer_id = make_timer_id(namespace, id);
        let (parsed_namespace, parsed_id) = parse_timer_id(timer_id);
        assert_eq!(namespace, parsed_namespace);
        assert_eq!(id, parsed_id);
    }
}
