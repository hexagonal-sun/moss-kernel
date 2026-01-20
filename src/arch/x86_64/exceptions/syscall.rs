use crate::{arch::{Arch, ArchImpl}, sched::current::current_task};
use alloc::boxed::Box;
use super::ExceptionState;
use libkernel::error::{KernelError, syscall_error::kern_err_to_syscall};
use libkernel::memory::address::{TUA, UA, VA};

use crate::clock::{timeofday::sys_gettimeofday, gettime::sys_clock_gettime};
use crate::kernel::{rand::sys_getrandom, uname::sys_uname};
use crate::process::thread_group::pid::sys_getpid;
use crate::process::exit::{sys_exit, sys_exit_group};
use crate::fs::syscalls::rw::{sys_read, sys_write};
use crate::fs::syscalls::at::open::sys_openat;
use crate::fs::syscalls::close::sys_close;
use crate::fs::syscalls::seek::sys_lseek;
use crate::process::fd_table::{Fd, AT_FDCWD};
use crate::process::clone::sys_clone;
use crate::process::exec::sys_execve;
use crate::fs::syscalls::stat::sys_fstat;
use crate::fs::syscalls::at::statx::sys_statx;
use crate::fs::syscalls::ioctl::sys_ioctl;
use crate::process::fd_table::select::{sys_ppoll, sys_pselect6};

use crate::memory::mmap::{sys_mmap, sys_munmap, sys_mprotect};

use crate::process::ptrace::{TracePoint, ptrace_stop};
use crate::process::thread_group::wait::sys_wait4;
use crate::process::sleep::{sys_nanosleep, sys_clock_nanosleep};
use crate::process::thread_group::pid::{sys_getppid, sys_getpgid, sys_setpgid};
use crate::process::thread_group::rsrc_lim::sys_prlimit64;
use crate::process::thread_group::signal::kill::{sys_kill, sys_tkill};
use crate::process::thread_group::signal::sigaction::sys_rt_sigaction;
use crate::process::thread_group::signal::sigaltstack::sys_sigaltstack;
use crate::process::thread_group::signal::sigprocmask::sys_rt_sigprocmask;
use crate::process::thread_group::umask::sys_umask;
use crate::process::threading::{sys_set_tid_address, sys_set_robust_list, futex::sys_futex};
use crate::process::prctl::sys_prctl;
use crate::process::ptrace::sys_ptrace;
use crate::sched::sys_sched_yield;
use crate::process::creds::sys_gettid;

// Include the assembly entry point
use core::arch::global_asm;
global_asm!(include_str!("syscall_entry.inc.s"));

#[unsafe(no_mangle)]
pub extern "C" fn handle_syscall_wrapper(state: &mut ExceptionState) {
    // Save the userspace context into the current task.
    current_task().ctx.save_user_ctx(state);

    // Spawn the async syscall handler as kernel work.
    crate::spawn_kernel_work(handle_syscall());

    // Enter the userspace return dispatcher.
    // This will poll the syscall work, and it will eventually
    // restore the (potentially modified) context back into `state`
    // and return to the assembly entry point.
    crate::sched::uspc_ret::dispatch_userspace_task(state);
}

pub async fn handle_syscall() {
    ptrace_stop(TracePoint::SyscallEntry).await;

    let (nr, args) = {
        let task = current_task();
        let ctx = &task.ctx;
        let state = ctx.user();
        (
            state.syscall_nr(),
            [
                state.arg(0),
                state.arg(1),
                state.arg(2),
                state.arg(3),
                state.arg(4),
                state.arg(5),
            ],
        )
    };

    log::info!("x86_64 syscall nr={} args={:?}", nr, args);

    let res = match nr as u64 {
        0 => {
            // read(fd, buf, count)
            sys_read(args[0].into(), TUA::from_value(args[1] as _), args[2] as _).await
        }
        1 => {
            // write(fd, buf, count)
            sys_write(args[0].into(), TUA::from_value(args[1] as _), args[2] as _).await
        }
        2 => {
            // open (legacy) - map to openat with AT_FDCWD
            sys_openat(Fd(AT_FDCWD), TUA::from_value(args[0] as _), args[1] as _, args[2] as _).await
        }
        3 => {
            // close(fd)
            sys_close(args[0].into()).await
        }
        5 => {
            // fstat(fd, statbuf)
            sys_fstat(args[0].into(), TUA::from_value(args[1] as _)).await
        }
        7 => {
            // poll -> map to ppoll with no timeout/sigmask
            sys_ppoll(TUA::from_value(args[0] as _), args[1] as _, TUA::from_value(0), TUA::from_value(0), 0).await
        }
        8 => {
            // lseek(fd, offset, whence)
            sys_lseek(args[0].into(), args[1] as _, args[2] as _).await
        }
        9 => {
            // mmap(addr, len, prot, flags, fd, offset)
            sys_mmap(args[0], args[1], args[2], args[3], args[4].into(), args[5]).await
        }
        10 => {
            // mprotect(start, len, prot)
            sys_mprotect(VA::from_value(args[0] as _), args[1] as _, args[2]).map(|_| 0).map_err(KernelError::from)
        }
        11 => {
            // munmap(addr, len)
            sys_munmap(VA::from_value(args[0] as _), args[1] as _).await
        }
        16 => {
            // ioctl(fd, request, arg)
            sys_ioctl(args[0].into(), args[1] as _, args[2] as _).await
        }
        24 => sys_sched_yield().map(|_| 0).map_err(KernelError::from),
        35 => sys_nanosleep(UA::from_value(args[0] as _).cast(), UA::from_value(args[1] as _).cast()).await,
        230 => sys_clock_nanosleep(args[0] as _, UA::from_value(args[1] as _).cast(), UA::from_value(args[2] as _).cast()).await,
        39 => sys_getpid().map_err(|e| match e {}),
        110 => sys_getppid().map_err(|e| match e {}),
        121 => sys_getpgid(args[0] as _),
        109 => sys_setpgid(args[0] as _, crate::process::thread_group::Pgid(args[1] as u32)),
        56 => {
            // clone(flags, newsp, parent_tidptr, child_tidptr, tls)
            sys_clone(
                args[0] as _,
                UA::from_value(args[1] as _),
                UA::from_value(args[2] as _).cast(),
                UA::from_value(args[3] as _).cast(),
                args[4] as _,
            )
            .await
        }
        57 => {
            // fork -> clone with flags=0 (simple emulation)
            sys_clone(0, UA::from_value(0), UA::from_value(0).cast(), UA::from_value(0).cast(), 0).await
        }
        61 => sys_wait4(args[0] as _, UA::from_value(args[1] as _).cast(), args[2] as _, UA::from_value(args[3] as _).cast()).await,
        62 => sys_kill(args[0] as _, crate::process::thread_group::signal::uaccess::UserSigId::from(args[1])),
        200 => sys_tkill(args[0] as _, crate::process::thread_group::signal::uaccess::UserSigId::from(args[1])),
        13 => sys_rt_sigaction(crate::process::thread_group::signal::uaccess::UserSigId::from(args[0]), UA::from_value(args[1] as _).cast(), UA::from_value(args[2] as _).cast(), args[3] as _).await,
        131 => sys_sigaltstack(UA::from_value(args[0] as _).cast(), UA::from_value(args[1] as _).cast()).await,
        14 => sys_rt_sigprocmask(args[0] as _, UA::from_value(args[1] as _).cast(), UA::from_value(args[2] as _).cast(), args[3] as _).await,
        95 => sys_umask(args[0] as _).map_err(|e| match e {}),
        218 => sys_set_tid_address(UA::from_value(args[0] as _).cast()),
        273 => sys_set_robust_list(UA::from_value(args[0] as _).cast(), args[1] as _).await,
        202 => {
            let uaddr = UA::from_value(args[0] as usize).cast();
            let op = args[1] as i32;
            let val = args[2] as u32;
            let timeout = UA::from_value(args[3] as usize).cast();
            let uaddr2 = UA::from_value(args[4] as usize).cast();
            let val3 = args[5] as u32;
            sys_futex(uaddr, op, val, timeout, uaddr2, val3).await
        }
        157 => sys_prctl(args[0] as i32, args[1] as _, args[2] as _).await,
        101 => sys_ptrace(args[0] as i32, args[1] as u64, UA::from_value(args[2] as usize), UA::from_value(args[3] as usize)).await,
        302 => sys_prlimit64(args[0] as _, args[1] as _, UA::from_value(args[2] as _).cast(), UA::from_value(args[3] as _).cast()).await,
        59 => {
            // execve(filename, argv, envp)
            sys_execve(
                TUA::from_value(args[0] as _),
                TUA::from_value(args[1] as _),
                TUA::from_value(args[2] as _),
            )
            .await
        }
        63 => sys_uname(TUA::from_value(args[0] as _)).await,
        96 => sys_gettimeofday(TUA::from_value(args[0] as _), TUA::from_value(args[1] as _)).await,
        257 => {
            // openat(dirfd, pathname, flags, mode)
            sys_openat(args[0].into(), TUA::from_value(args[1] as _), args[2] as _, args[3] as _).await
        }
        318 => sys_getrandom(TUA::from_value(args[0] as _), args[1] as _, args[2] as _).await,
        332 => {
            // statx(dfd, pathname, flags, mask, buffer)
            sys_statx(
                args[0].into(),
                TUA::from_value(args[1] as _),
                args[2] as _,
                args[3] as _,
                TUA::from_value(args[4] as _),
            )
            .await
        }
        186 => sys_gettid().map_err(|e| match e {}),
        60 => {
            let _ = sys_exit(args[0] as _).await;
            return;
        }
        231 => {
            let _ = sys_exit_group(args[0] as _).await;
            return;
        }
        _ => Err(KernelError::NotSupported),
    };

    let ret_val = match res {
        Ok(v) => v as isize,
        Err(e) => kern_err_to_syscall(e),
    };

    current_task().ctx.user_mut().regs[0] = ret_val.cast_unsigned() as u64;
    ptrace_stop(TracePoint::SyscallExit).await;
}
