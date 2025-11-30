use crate::error::FsError;

use super::KernelError;

pub const EPERM: isize = -1;
pub const ENOENT: isize = -2;
pub const ESRCH: isize = -3;
pub const EINTR: isize = -4;
pub const EIO: isize = -5;
pub const ENXIO: isize = -6;
pub const E2BIG: isize = -7;
pub const ENOEXEC: isize = -8;
pub const EBADF: isize = -9;
pub const ECHILD: isize = -10;
pub const EAGAIN: isize = -11;
pub const ENOMEM: isize = -12;
pub const EACCES: isize = -13;
pub const EFAULT: isize = -14;
pub const ENOTBLK: isize = -15;
pub const EBUSY: isize = -16;
pub const EEXIST: isize = -17;
pub const EXDEV: isize = -18;
pub const ENODEV: isize = -19;
pub const ENOTDIR: isize = -20;
pub const EISDIR: isize = -21;
pub const EINVAL: isize = -22;
pub const ENFILE: isize = -23;
pub const EMFILE: isize = -24;
pub const ENOTTY: isize = -25;
pub const ETXTBSY: isize = -26;
pub const EFBIG: isize = -27;
pub const ENOSPC: isize = -28;
pub const ESPIPE: isize = -29;
pub const EROFS: isize = -30;
pub const EMLINK: isize = -31;
pub const EPIPE: isize = -32;
pub const EDOM: isize = -33;
pub const ERANGE: isize = -34;
pub const EWOULDBLOCK: isize = -EAGAIN;
pub const ENOSYS: isize = -38;

pub fn kern_err_to_syscall(err: KernelError) -> isize {
    match err {
        KernelError::BadFd => EBADF,
        KernelError::InvalidValue => EINVAL,
        KernelError::Fault => EFAULT,
        KernelError::BrokenPipe => EPIPE,
        KernelError::Fs(FsError::NotFound) => ENOENT,
        KernelError::NotATty => ENOTTY,
        KernelError::SeekPipe => ESPIPE,
        KernelError::NotSupported => ENOSYS,
        KernelError::NoMemory => ENOMEM,
        _ => todo!(),
    }
}
