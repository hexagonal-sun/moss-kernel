//! Synchronisation primitives for `no_std` kernel environments.
//!
//! All primitives are generic over [`CpuOps`](crate::CpuOps) so they can
//! disable/restore interrupts on the local core.

pub mod condvar;
pub mod mpsc;
pub mod mutex;
pub mod once_lock;
pub mod per_cpu;
pub mod rwlock;
pub mod spinlock;
pub mod waker_set;
