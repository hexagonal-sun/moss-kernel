use crate::arch::ArchImpl;

pub mod per_cpu;

pub type SpinLock<T> = libkernel::sync::spinlock::SpinLockIrq<T, ArchImpl>;
pub type Mutex<T> = libkernel::sync::mutex::Mutex<T, ArchImpl>;
pub type AsyncMutexGuard<'a, T> = libkernel::sync::mutex::AsyncMutexGuard<'a, T, ArchImpl>;
#[expect(dead_code)]
pub type Rwlock<T> = libkernel::sync::rwlock::Rwlock<T, ArchImpl>;
#[expect(dead_code)]
pub type AsyncRwlockReadGuard<'a, T> =
    libkernel::sync::rwlock::AsyncRwlockReadGuard<'a, T, ArchImpl>;
#[expect(dead_code)]
pub type AsyncRwlockWriteGuard<'a, T> =
    libkernel::sync::rwlock::AsyncRwlockWriteGuard<'a, T, ArchImpl>;
pub type OnceLock<T> = libkernel::sync::once_lock::OnceLock<T, ArchImpl>;
pub type CondVar<T> = libkernel::sync::condvar::CondVar<T, ArchImpl>;
// pub type Reciever<T> = libkernel::sync::mpsc::Reciever<T, ArchImpl>;
// pub type Sender<T> = libkernel::sync::mpsc::Sender<T, ArchImpl>;

// pub fn channel<T: Send>() -> (Sender<T>, Reciever<T>) {
//     libkernel::sync::mpsc::channel()
// }
