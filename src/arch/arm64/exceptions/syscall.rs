use crate::{
    arch::{Arch, ArchImpl},
    clock::{
        gettime::sys_clock_gettime,
        settime::sys_clock_settime,
        timeofday::{sys_gettimeofday, sys_settimeofday},
    },
    fs::{
        dir::sys_getdents64,
        pipe::sys_pipe2,
        syscalls::{
            at::{
                access::{sys_faccessat, sys_faccessat2},
                chmod::sys_fchmodat,
                chown::sys_fchownat,
                handle::sys_name_to_handle_at,
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
            close::{sys_close, sys_close_range},
            copy_file_range::sys_copy_file_range,
            getxattr::{sys_fgetxattr, sys_getxattr, sys_lgetxattr},
            ioctl::sys_ioctl,
            iov::{sys_preadv, sys_preadv2, sys_pwritev, sys_pwritev2, sys_readv, sys_writev},
            listxattr::{sys_flistxattr, sys_listxattr, sys_llistxattr},
            mount::sys_mount,
            removexattr::{sys_fremovexattr, sys_lremovexattr, sys_removexattr},
            rw::{sys_pread64, sys_pwrite64, sys_read, sys_write},
            seek::sys_lseek,
            setxattr::{sys_fsetxattr, sys_lsetxattr, sys_setxattr},
            splice::sys_sendfile,
            stat::sys_fstat,
            statfs::{sys_fstatfs, sys_statfs},
            sync::{sys_fdatasync, sys_fsync, sys_sync, sys_syncfs},
            trunc::{sys_ftruncate, sys_truncate},
        },
    },
    kernel::{
        getcpu::sys_getcpu, hostname::sys_sethostname, power::sys_reboot, rand::sys_getrandom,
        sysinfo::sys_sysinfo, uname::sys_uname,
    },
    memory::{
        brk::sys_brk,
        mincore::sys_mincore,
        mmap::{sys_mmap, sys_mprotect, sys_munmap},
        process_vm::sys_process_vm_readv,
    },
    net::syscalls::{
        accept::{sys_accept, sys_accept4},
        bind::sys_bind,
        connect::sys_connect,
        listen::sys_listen,
        recv::sys_recvfrom,
        send::sys_sendto,
        shutdown::sys_shutdown,
        socket::sys_socket,
    },
    process::{
        caps::{sys_capget, sys_capset},
        clone::sys_clone,
        creds::{
            sys_getegid, sys_geteuid, sys_getgid, sys_getresgid, sys_getresuid, sys_getsid,
            sys_gettid, sys_getuid, sys_setfsgid, sys_setfsuid, sys_setgid, sys_setregid,
            sys_setresgid, sys_setresuid, sys_setreuid, sys_setsid, sys_setuid,
        },
        epoll::{sys_epoll_create1, sys_epoll_ctl, sys_epoll_pwait},
        exec::sys_execve,
        exit::{sys_exit, sys_exit_group},
        fd_table::{
            dup::{sys_dup, sys_dup3},
            fcntl::sys_fcntl,
            select::{sys_ppoll, sys_pselect6},
        },
        pidfd::sys_pidfd_open,
        prctl::sys_prctl,
        ptrace::{TracePoint, ptrace_stop, sys_ptrace},
        sleep::{sys_clock_nanosleep, sys_nanosleep},
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
            wait::{sys_wait4, sys_waitid},
        },
        threading::{futex::sys_futex, sys_set_robust_list, sys_set_tid_address},
    },
    sched::{
        self,
        sched_task::state::TaskState,
        syscalls::{sys_sched_getaffinity, sys_sched_setaffinity, sys_sched_yield},
    },
};
use alloc::boxed::Box;
use libkernel::{
    error::{KernelError, syscall_error::kern_err_to_syscall},
    memory::address::{TUA, UA, VA},
};

use crate::sched::syscall_ctx::ProcessCtx;

pub async fn handle_syscall(mut ctx: ProcessCtx) {
    ctx.task_mut().update_accounting(None);
    ctx.task_mut().in_syscall = true;
    ptrace_stop(&ctx, TracePoint::SyscallEntry).await;

    let (nr, arg1, arg2, arg3, arg4, arg5, arg6) = {
        let state = ctx.task().ctx.user();

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
        0x14 => sys_epoll_create1(&ctx, arg1 as _).await,
        0x15 => {
            sys_epoll_ctl(
                &ctx,
                arg1.into(),
                arg2 as _,
                arg3.into(),
                TUA::from_value(arg4 as _),
            )
            .await
        }
        0x16 => {
            sys_epoll_pwait(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
                TUA::from_value(arg5 as _),
                arg6 as _,
            )
            .await
        }
        0x5 => {
            sys_setxattr(
                &ctx,
                TUA::from_value(arg1 as _),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
                arg5 as _,
            )
            .await
        }
        0x6 => {
            sys_lsetxattr(
                &ctx,
                TUA::from_value(arg1 as _),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
                arg5 as _,
            )
            .await
        }
        0x7 => {
            sys_fsetxattr(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
                arg5 as _,
            )
            .await
        }
        0x8 => {
            sys_getxattr(
                &ctx,
                TUA::from_value(arg1 as _),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x9 => {
            sys_lgetxattr(
                &ctx,
                TUA::from_value(arg1 as _),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0xa => {
            sys_fgetxattr(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0xb => {
            sys_listxattr(
                &ctx,
                TUA::from_value(arg1 as _),
                TUA::from_value(arg2 as _),
                arg3 as _,
            )
            .await
        }
        0xc => {
            sys_llistxattr(
                &ctx,
                TUA::from_value(arg1 as _),
                TUA::from_value(arg2 as _),
                arg3 as _,
            )
            .await
        }
        0xd => sys_flistxattr(&ctx, arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0xe => sys_removexattr(&ctx, TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0xf => sys_lremovexattr(&ctx, TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0x10 => sys_fremovexattr(&ctx, arg1.into(), TUA::from_value(arg2 as _)).await,
        0x11 => sys_getcwd(&ctx, TUA::from_value(arg1 as _), arg2 as _).await,
        0x17 => sys_dup(&ctx, arg1.into()),
        0x18 => sys_dup3(&ctx, arg1.into(), arg2.into(), arg3 as _),
        0x19 => sys_fcntl(&ctx, arg1.into(), arg2 as _, arg3 as _).await,
        0x1d => sys_ioctl(&ctx, arg1.into(), arg2 as _, arg3 as _).await,
        0x20 => Ok(0), // sys_flock is a noop
        0x21 => Err(KernelError::NotSupported),
        0x22 => sys_mkdirat(&ctx, arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x23 => sys_unlinkat(&ctx, arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x24 => {
            sys_symlinkat(
                &ctx,
                TUA::from_value(arg1 as _),
                arg2.into(),
                TUA::from_value(arg3 as _),
            )
            .await
        }
        0x25 => {
            sys_linkat(
                &ctx,
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
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3.into(),
                TUA::from_value(arg4 as _),
            )
            .await
        }
        0x28 => {
            sys_mount(
                &ctx,
                TUA::from_value(arg1 as _),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
                TUA::from_value(arg5 as _),
            )
            .await
        }
        0x2b => sys_statfs(&ctx, TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0x2c => sys_fstatfs(&ctx, arg1.into(), TUA::from_value(arg2 as _)).await,
        0x2d => sys_truncate(&ctx, TUA::from_value(arg1 as _), arg2 as _).await,
        0x2e => sys_ftruncate(&ctx, arg1.into(), arg2 as _).await,
        0x30 => sys_faccessat(&ctx, arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x31 => sys_chdir(&ctx, TUA::from_value(arg1 as _)).await,
        0x32 => sys_fchdir(&ctx, arg1.into()).await,
        0x33 => sys_chroot(&ctx, TUA::from_value(arg1 as _)).await,
        0x34 => sys_fchmod(&ctx, arg1.into(), arg2 as _).await,
        0x35 => {
            sys_fchmodat(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        0x36 => {
            sys_fchownat(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
                arg5 as _,
            )
            .await
        }
        0x37 => sys_fchown(&ctx, arg1.into(), arg2 as _, arg3 as _).await,
        0x38 => {
            sys_openat(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        0x39 => sys_close(&ctx, arg1.into()).await,
        0x3b => sys_pipe2(&ctx, TUA::from_value(arg1 as _), arg2 as _).await,
        0x3d => sys_getdents64(&ctx, arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x3e => sys_lseek(&ctx, arg1.into(), arg2 as _, arg3 as _).await,
        0x3f => sys_read(&ctx, arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x40 => sys_write(&ctx, arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x41 => sys_readv(&ctx, arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x42 => sys_writev(&ctx, arg1.into(), TUA::from_value(arg2 as _), arg3 as _).await,
        0x43 => {
            sys_pread64(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        0x44 => {
            sys_pwrite64(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        0x45 => {
            sys_preadv(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        0x46 => {
            sys_pwritev(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        0x47 => {
            sys_sendfile(
                &ctx,
                arg1.into(),
                arg2.into(),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x48 => {
            sys_pselect6(
                &ctx,
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
                &ctx,
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
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x4f => {
            sys_newfstatat(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x50 => sys_fstat(&ctx, arg1.into(), TUA::from_value(arg2 as _)).await,
        0x51 => sys_sync(&ctx).await,
        0x52 => sys_fsync(&ctx, arg1.into()).await,
        0x53 => sys_fdatasync(&ctx, arg1.into()).await,
        0x58 => {
            sys_utimensat(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x5a => sys_capget(&ctx, TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0x5b => sys_capset(&ctx, TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0x5d => {
            let _ = sys_exit(&mut ctx, arg1 as _).await;

            debug_assert!(
                sched::current_work()
                    .state
                    .load(core::sync::atomic::Ordering::Acquire)
                    == TaskState::Finished
            );

            // Don't process result on exit.
            return;
        }
        0x5e => {
            let _ = sys_exit_group(&ctx, arg1 as _).await;

            debug_assert!(
                sched::current_work()
                    .state
                    .load(core::sync::atomic::Ordering::Acquire)
                    == TaskState::Finished
            );

            // Don't process result on exit.
            return;
        }
        0x5f => {
            sys_waitid(
                &ctx,
                arg1 as _,
                arg2 as _,
                TUA::from_value(arg3 as _),
                arg4 as _,
                TUA::from_value(arg5 as _),
            )
            .await
        }
        0x60 => sys_set_tid_address(&mut ctx, TUA::from_value(arg1 as _)),
        0x62 => {
            sys_futex(
                &ctx,
                TUA::from_value(arg1 as _),
                arg2 as _,
                arg3 as _,
                TUA::from_value(arg4 as _),
                TUA::from_value(arg5 as _),
                arg6 as _,
            )
            .await
        }
        0x63 => sys_set_robust_list(&mut ctx, TUA::from_value(arg1 as _), arg2 as _).await,
        0x65 => sys_nanosleep(TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0x70 => sys_clock_settime(arg1 as _, TUA::from_value(arg2 as _)).await,
        0x71 => sys_clock_gettime(&ctx, arg1 as _, TUA::from_value(arg2 as _)).await,
        0x73 => {
            sys_clock_nanosleep(
                arg1 as _,
                arg2 as _,
                TUA::from_value(arg3 as _),
                TUA::from_value(arg4 as _),
            )
            .await
        }
        0x75 => {
            sys_ptrace(
                &ctx,
                arg1 as _,
                arg2 as _,
                TUA::from_value(arg3 as _),
                TUA::from_value(arg4 as _),
            )
            .await
        }
        0x7a => sys_sched_setaffinity(&ctx, arg1 as _, arg2 as _, TUA::from_value(arg3 as _)).await,
        0x7b => sys_sched_getaffinity(&ctx, arg1 as _, arg2 as _, TUA::from_value(arg3 as _)).await,
        0x7c => sys_sched_yield(),
        0x81 => sys_kill(&ctx, arg1 as _, arg2.into()),
        0x82 => sys_tkill(&ctx, arg1 as _, arg2.into()),
        0x84 => sys_sigaltstack(&ctx, TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0x86 => {
            sys_rt_sigaction(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x87 => {
            sys_rt_sigprocmask(
                &mut ctx,
                arg1 as _,
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x8b => {
            // Special case for sys_rt_sigreturn
            //
            // SAFETY: Signal work will only be polled once this kernel work has
            // returned. Therefore there will be no concurrent accesses of the
            // ctx.
            let ctx2 = unsafe { ctx.clone() };
            ctx.task_mut()
                .ctx
                .put_signal_work(Box::pin(ArchImpl::do_signal_return(ctx2)));

            return;
        }
        0x8e => sys_reboot(&ctx, arg1 as _, arg2 as _, arg3 as _, arg4 as _).await,
        0x8f => sys_setregid(&ctx, arg1 as _, arg2 as _),
        0x90 => sys_setgid(&ctx, arg1 as _),
        0x91 => sys_setreuid(&ctx, arg1 as _, arg2 as _),
        0x92 => sys_setuid(&ctx, arg1 as _),
        0x93 => sys_setresuid(&ctx, arg1 as _, arg2 as _, arg3 as _),
        0x94 => {
            sys_getresuid(
                &ctx,
                TUA::from_value(arg1 as _),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
            )
            .await
        }
        0x95 => sys_setresgid(&ctx, arg1 as _, arg2 as _, arg3 as _),
        0x96 => {
            sys_getresgid(
                &ctx,
                TUA::from_value(arg1 as _),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
            )
            .await
        }
        0x97 => sys_setfsuid(&ctx, arg1 as _).map_err(|e| match e {}),
        0x98 => sys_setfsgid(&ctx, arg1 as _).map_err(|e| match e {}),
        0x9a => sys_setpgid(&ctx, arg1 as _, Pgid(arg2 as _)),
        0x9b => sys_getpgid(&ctx, arg1 as _),
        0x9c => sys_getsid(&ctx).await,
        0x9d => sys_setsid(&ctx).await,
        0xa0 => sys_uname(TUA::from_value(arg1 as _)).await,
        0xa1 => sys_sethostname(&ctx, TUA::from_value(arg1 as _), arg2 as _).await,
        0xa3 => Err(KernelError::InvalidValue),
        0xa6 => sys_umask(&ctx, arg1 as _).map_err(|e| match e {}),
        0xa7 => sys_prctl(&ctx, arg1 as _, arg2, arg3).await,
        0xa8 => sys_getcpu(TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0xa9 => sys_gettimeofday(TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0xaa => sys_settimeofday(TUA::from_value(arg1 as _), TUA::from_value(arg2 as _)).await,
        0xac => sys_getpid(&ctx).map_err(|e| match e {}),
        0xad => sys_getppid(&ctx).map_err(|e| match e {}),
        0xae => sys_getuid(&ctx).map_err(|e| match e {}),
        0xaf => sys_geteuid(&ctx).map_err(|e| match e {}),
        0xb0 => sys_getgid(&ctx).map_err(|e| match e {}),
        0xb1 => sys_getegid(&ctx).map_err(|e| match e {}),
        0xb2 => sys_gettid(&ctx).map_err(|e| match e {}),
        0xb3 => sys_sysinfo(TUA::from_value(arg1 as _)).await,
        0xc6 => sys_socket(&ctx, arg1 as _, arg2 as _, arg3 as _).await,
        0xc8 => sys_bind(&ctx, arg1.into(), UA::from_value(arg2 as _), arg3 as _).await,
        0xc9 => sys_listen(&ctx, arg1.into(), arg2 as _).await,
        0xca => {
            sys_accept(
                &ctx,
                arg1.into(),
                UA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
            )
            .await
        }
        0xcb => sys_connect(&ctx, arg1.into(), UA::from_value(arg2 as _), arg3 as _).await,
        0xce => {
            sys_sendto(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
                UA::from_value(arg5 as _),
                arg6 as _,
            )
            .await
        }
        0xcf => {
            sys_recvfrom(
                &ctx,
                arg1.into(),
                UA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
                UA::from_value(arg5 as _),
                TUA::from_value(arg6 as _),
            )
            .await
        }
        0xd2 => sys_shutdown(&ctx, arg1.into(), arg2 as _).await,
        0xd6 => sys_brk(&ctx, VA::from_value(arg1 as _))
            .await
            .map_err(|e| match e {}),
        0xd7 => sys_munmap(&ctx, VA::from_value(arg1 as usize), arg2 as _).await,
        0xdc => {
            sys_clone(
                &ctx,
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
                &mut ctx,
                TUA::from_value(arg1 as _),
                TUA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
            )
            .await
        }
        0xde => sys_mmap(&ctx, arg1, arg2, arg3, arg4, arg5.into(), arg6).await,
        0xdf => Ok(0), // fadvise64_64 is a no-op
        0xe2 => sys_mprotect(&ctx, VA::from_value(arg1 as _), arg2 as _, arg3 as _),
        0xe8 => sys_mincore(&ctx, arg1, arg2 as _, TUA::from_value(arg3 as _)).await,
        0xe9 => Ok(0), // sys_madvise is a no-op
        0xf2 => {
            sys_accept4(
                &ctx,
                arg1.into(),
                UA::from_value(arg2 as _),
                TUA::from_value(arg3 as _),
                arg4 as _,
            )
            .await
        }
        0x104 => {
            sys_wait4(
                &ctx,
                arg1.cast_signed() as _,
                TUA::from_value(arg2 as _),
                arg3 as _,
                TUA::from_value(arg4 as _),
            )
            .await
        }
        0x105 => {
            sys_prlimit64(
                &ctx,
                arg1 as _,
                arg2 as _,
                TUA::from_value(arg3 as _),
                TUA::from_value(arg4 as _),
            )
            .await
        }
        0x108 => sys_name_to_handle_at(),
        0x109 => Err(KernelError::NotSupported),
        0x10b => sys_syncfs(&ctx, arg1.into()).await,
        0x10e => {
            sys_process_vm_readv(
                arg1 as _,
                TUA::from_value(arg2 as _),
                arg3 as _,
                TUA::from_value(arg4 as _),
                arg5 as _,
                arg6 as _,
            )
            .await
        }
        0x114 => {
            sys_renameat2(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3.into(),
                TUA::from_value(arg4 as _),
                arg5 as _,
            )
            .await
        }
        0x116 => sys_getrandom(TUA::from_value(arg1 as _), arg2 as _, arg3 as _).await,
        0x11d => {
            sys_copy_file_range(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3.into(),
                TUA::from_value(arg4 as _),
                arg5 as _,
                arg6 as _,
            )
            .await
        }
        0x11e => {
            sys_preadv2(
                &ctx,
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
                &ctx,
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
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
                TUA::from_value(arg5 as _),
            )
            .await
        }
        0x125 => Err(KernelError::NotSupported),
        0x1ae => Err(KernelError::NotSupported),
        0x1b2 => sys_pidfd_open(&ctx, arg1 as _, arg2 as _).await,
        0x1b4 => sys_close_range(&ctx, arg1.into(), arg2.into(), arg3 as _).await,
        0x1b7 => {
            sys_faccessat2(
                &ctx,
                arg1.into(),
                TUA::from_value(arg2 as _),
                arg3 as _,
                arg4 as _,
            )
            .await
        }
        0x1b8 => Ok(0), // process_madvise is a no-op
        _ => panic!(
            "Unhandled syscall 0x{nr:x}, PC: 0x{:x}",
            ctx.task().ctx.user().elr_el1
        ),
    };

    let ret_val = match res {
        Ok(v) => v as isize,
        Err(e) => kern_err_to_syscall(e),
    };

    ctx.task_mut().ctx.user_mut().x[0] = ret_val.cast_unsigned() as u64;
    ptrace_stop(&ctx, TracePoint::SyscallExit).await;
    ctx.task_mut().update_accounting(None);
    ctx.task_mut().in_syscall = false;
}
