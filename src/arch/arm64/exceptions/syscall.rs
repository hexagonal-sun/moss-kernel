use crate::{
    arch::{Arch, ArchImpl},
    clock::{gettime::sys_clock_gettime, timeofday::sys_gettimeofday},
    fs::{
        dir::sys_getdents64,
        pipe::sys_pipe2,
        syscalls::{
            at::{
                access::{sys_faccessat, sys_faccessat2},
                chmod::sys_fchmodat,
                chown::sys_fchownat,
                link::sys_linkat,
                mkdir::sys_mkdirat,
                open::sys_openat,
                readlink::sys_readlinkat,
                rename::{sys_renameat, sys_renameat2},
                stat::sys_newfstatat,
                statx::sys_statx,
                symlink::sys_symlinkat,
                unlink::sys_unlinkat,
                utime::sys_utimensat,
            },
            chdir::{sys_chdir, sys_chroot, sys_fchdir, sys_getcwd},
            chmod::sys_fchmod,
            chown::sys_fchown,
            close::sys_close,
            ioctl::sys_ioctl,
            iov::{sys_preadv, sys_preadv2, sys_pwritev, sys_pwritev2, sys_readv, sys_writev},
            rw::{sys_pread64, sys_pwrite64, sys_read, sys_write},
            seek::sys_lseek,
            splice::sys_sendfile,
            stat::sys_fstat,
            sync::{sys_fdatasync, sys_fsync, sys_sync, sys_syncfs},
            trunc::{sys_ftruncate, sys_truncate},
        },
    },
    kernel::{power::sys_reboot, rand::sys_getrandom, sysinfo::sys_sysinfo, uname::sys_uname},
    memory::{
        brk::sys_brk,
        mmap::{sys_mmap, sys_mprotect, sys_munmap},
    },
    process::{
        caps::{sys_capget, sys_capset},
        clone::sys_clone,
        creds::{
            sys_getegid, sys_geteuid, sys_getgid, sys_getresgid, sys_getresuid, sys_gettid,
            sys_getuid,
        },
        exec::sys_execve,
        exit::{sys_exit, sys_exit_group},
        fd_table::{
            dup::{sys_dup, sys_dup3},
            fcntl::sys_fcntl,
            select::{sys_ppoll, sys_pselect6},
        },
        sleep::sys_nanosleep,
        thread_group::{
            Pgid,
            pid::{sys_getpgid, sys_getpid, sys_getppid, sys_setpgid},
            rsrc_lim::sys_prlimit64,
            signal::{
                kill::{sys_kill, sys_tkill},
                sigaction::sys_rt_sigaction,
                sigaltstack::sys_sigaltstack,
                sigprocmask::sys_rt_sigprocmask,
            },
            umask::sys_umask,
            wait::sys_wait4,
        },
        threading::{futex::sys_futex, sys_set_robust_list, sys_set_tid_address},
    },
    sched::{current_task, sys_sched_yield},
};
use alloc::boxed::Box;
use libkernel::{
    error::{KernelError, syscall_error::kern_err_to_syscall},
    memory::address::{TUA, UA, VA},
};

pub async fn handle_syscall() {
    let task = current_task();

    let (nr, arg1, arg2, arg3, arg4, arg5, arg6) = {
        let ctx = task.ctx.lock_save_irq();
        let state = ctx.user();

        (
            state.x[8] as u32,
            state.x[0],
            state.x[1],
            state.x[2],
            state.x[3],
            state.x[4],
            state.x[5],
        )
    };

    let res = match nr {
        0x11 => sys_getcwd(TUA::from_value(arg1 as _), arg2 as _).await,
        0x17 => sys_dup(arg1.into()),
        0x18 => sys_dup3(arg1.into(), arg2.into(), arg3 as _),
        0x19 => sys_fcntl(arg1.into(), arg2 as _, arg3 as _).await,
        0x1d => sys_ioctl(arg1.into(), arg2 as _, arg3 as _).await,
        0x22 => sys_mkdirat(arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x23 => sys_unlinkat(arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x24 => {
            sys_symlinkat(
                TUA::from_value(arg1 as _),
                arg2.into(),
                TUA::from_value(arg3 as _),
            )
            .await
        }
        0x25 => {
            sys_linkat(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3.into(),
                TUA::from_value(arg4 as _),
                arg5 as _,
            )
            .await
        }
        0x26 => {
            sys_renameat(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3.into(),
                TUA::from_value(arg4 as _),
            )
            .await
        }
        0x2d => sys_truncate(TUA::from_value(arg1 as _), arg2 as _).await,
        0x2e => sys_ftruncate(arg1.into(), arg2 as _).await,
        0x30 => sys_faccessat(arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x31 => sys_chdir(TUA::from_value(arg1 as _)).await,
        0x32 => sys_fchdir(arg1.into()).await,
        0x33 => sys_chroot(TUA::from_value(arg1 as _)).await,
        0x34 => sys_fchmod(arg1.into(), arg2 as _).await,
        0x35 => {
            sys_fchmodat(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        0x36 => {
            sys_fchownat(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
                arg5 as _,
            )
            .await
        }
        0x37 => sys_fchown(arg1.into(), arg2 as _, arg3 as _).await,
        0x38 => {
            sys_openat(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        0x39 => sys_close(arg1.into()).await,
        0x3b => sys_pipe2(TUA::from_value(arg1 as _), arg2 as _).await,
        0x3d => sys_getdents64(arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x3e => sys_lseek(arg1.into(), arg2 as _, arg3 as _).await,
        0x3f => sys_read(arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x40 => sys_write(arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x41 => sys_readv(arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x42 => sys_writev(arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x43 => {
            sys_pread64(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        0x44 => {
            sys_pwrite64(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        0x45 => {
            sys_preadv(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        0x46 => {
            sys_pwritev(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        0x47 => {
            sys_sendfile(
                arg1.into(),
                arg2.into(),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x48 => {
            sys_pselect6(
                arg1 as _,
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                TUA::from_value(arg4 as _),
                TUA::from_value(arg5 as _),
                TUA::from_value(arg6 as _),
            )
            .await
        }
        0x49 => {
            sys_ppoll(
                TUA::from_value(arg1 as _),
                arg2 as _,
                TUA::from_value(arg3 as _),
                TUA::from_value(arg4 as _),
                arg5 as _,
            )
            .await
        }
        0x4e => {
            sys_readlinkat(
                arg1.into(),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x4f => {
            sys_newfstatat(
                arg1.into(),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x50 => sys_fstat(arg1.into(), TUA::from_value(arg2 as _)).await,
        0x51 => sys_sync().await,
        0x52 => sys_fsync(arg1.into()).await,
        0x53 => sys_fdatasync(arg1.into()).await,
        0x58 => {
            sys_utimensat(
                arg1.into(),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x5a => sys_capget(TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0x5b => sys_capset(TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0x5d => sys_exit(arg1 as _).await,
        0x5e => sys_exit_group(arg1 as _),
        0x60 => sys_set_tid_address(TUA::from_value(arg1 as _)),
        0x62 => {
            sys_futex(
                TUA::from_value(arg1 as _),
                arg2 as _,
                arg3 as _,
                TUA::from_value(arg4 as _),
                TUA::from_value(arg5 as _),
                arg6 as _,
            )
            .await
        }
        0x63 => sys_set_robust_list(TUA::from_value(arg1 as _), arg2 as _).await,
        0x65 => sys_nanosleep(TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0x71 => sys_clock_gettime(arg1 as _, TUA::from_value(arg2 as _)).await,
        0x7c => sys_sched_yield(),
        0x81 => sys_kill(arg1 as _, arg2.into()),
        0x82 => sys_tkill(arg1 as _, arg2.into()),
        0x84 => sys_sigaltstack(TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0x86 => {
            sys_rt_sigaction(
                arg1.into(),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x87 => {
            sys_rt_sigprocmask(
                arg1 as _,
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x8b => {
            // Special case for sys_rt_sigreturn
            task.ctx
                .lock_save_irq()
                .put_signal_work(Box::pin(ArchImpl::do_signal_return()));

            return;
        }
        0x8e => sys_reboot(arg1 as _, arg2 as _, arg3 as _, arg4 as _).await,
        0x94 => {
            sys_getresuid(
                TUA::from_value(arg1 as _),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
            )
            .await
        }
        0x96 => {
            sys_getresgid(
                TUA::from_value(arg1 as _),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
            )
            .await
        }
        0x9a => sys_setpgid(arg1 as _, Pgid(arg2 as _)),
        0x9b => sys_getpgid(arg1 as _),
        0xa0 => sys_uname(TUA::from_value(arg1 as _)).await,
        0xa3 => Err(KernelError::InvalidValue),
        0xa6 => sys_umask(arg1 as _).map_err(|e| match e {}),
        0xa9 => sys_gettimeofday(TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0xac => sys_getpid().map_err(|e| match e {}),
        0xad => sys_getppid().map_err(|e| match e {}),
        0xae => sys_getuid().map_err(|e| match e {}),
        0xaf => sys_geteuid().map_err(|e| match e {}),
        0xb0 => sys_getgid().map_err(|e| match e {}),
        0xb1 => sys_getegid().map_err(|e| match e {}),
        0xb2 => sys_gettid().map_err(|e| match e {}),
        0xb3 => sys_sysinfo(TUA::from_value(arg1 as _)).await,
        0xc6 => Err(KernelError::NotSupported),
        0xd6 => sys_brk(VA::from_value(arg1 as _))
            .await
            .map_err(|e| match e {}),
        0xd7 => sys_munmap(VA::from_value(arg1 as usize), arg2 as _).await,
        0xdc => {
            sys_clone(
                arg1 as _,
                UA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                TUA::from_value(arg5 as _),
                arg4 as _,
            )
            .await
        }
        0xdd => {
            sys_execve(
                TUA::from_value(arg1 as _),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
            )
            .await
        }
        0xde => sys_mmap(arg1, arg2, arg3, arg4, arg5.into(), arg6).await,
        0xe2 => sys_mprotect(VA::from_value(arg1 as _), arg2 as _, arg3 as _),
        0xe9 => Ok(0), // sys_madvise is a no-op
        0x104 => {
            sys_wait4(
                arg1.cast_signed() as _,
                TUA::from_value(arg2 as _),
                arg3 as _,
                TUA::from_value(arg4 as _),
            )
            .await
        }
        0x105 => {
            sys_prlimit64(
                arg1 as _,
                arg2 as _,
                TUA::from_value(arg3 as _),
                TUA::from_value(arg4 as _),
            )
            .await
        }
        0x10b => sys_syncfs(arg1.into()).await,
        0x114 => {
            sys_renameat2(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3.into(),
                TUA::from_value(arg4 as _),
                arg5 as _,
            )
            .await
        }
        0x116 => sys_getrandom(TUA::from_value(arg1 as _), arg2 as _, arg3 as _).await,
        0x11e => {
            sys_preadv2(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
                arg5 as _,
            )
            .await
        }
        0x11f => {
            sys_pwritev2(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
                arg5 as _,
            )
            .await
        }
        0x123 => {
            sys_statx(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
                TUA::from_value(arg5 as _),
            )
            .await
        }
        0x125 => Err(KernelError::NotSupported),
        0x1b7 => {
            sys_faccessat2(
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        _ => panic!(
            "Unhandled syscall 0x{nr:x}, PC: 0x{:x}",
            current_task().ctx.lock_save_irq().user().elr_el1
        ),
    };

    let ret_val = match res {
        Ok(v) => v as isize,
        Err(e) => kern_err_to_syscall(e),
    };

    task.ctx.lock_save_irq().user_mut().x[0] = ret_val.cast_unsigned() as u64;
}
