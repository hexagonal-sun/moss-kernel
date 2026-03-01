use crate::register_test;
use std::{
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

static SIGNAL_CAUGHT: AtomicBool = AtomicBool::new(false);

extern "C" fn signal_handler(_: libc::c_int) {
    SIGNAL_CAUGHT.store(true, Ordering::Relaxed);
}

fn register_handler(signum: libc::c_int, restart: bool) {
    unsafe {
        SIGNAL_CAUGHT.store(false, Ordering::Relaxed);
        let mut action: libc::sigaction = std::mem::zeroed();

        action.sa_sigaction = signal_handler as *const () as usize;

        action.sa_flags = if restart { libc::SA_RESTART } else { 0 };

        libc::sigemptyset(&mut action.sa_mask);

        if libc::sigaction(signum, &action, std::ptr::null_mut()) != 0 {
            panic!("Failed to register signal handler");
        }
    }
}

fn test_interruptible_nanosleep() {
    register_handler(libc::SIGALRM, false);

    unsafe {
        let pid = libc::getpid();

        if libc::fork() == 0 {
            // in child.
            let req = libc::timespec {
                tv_sec: 1,
                tv_nsec: 0,
            };

            libc::nanosleep(&req, ptr::null_mut());
            libc::kill(pid, libc::SIGALRM);
            libc::exit(0);
        };

        // Sleep for 5 seconds (much longer than the alarm)
        let req = libc::timespec {
            tv_sec: 5,
            tv_nsec: 0,
        };
        let mut rem = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };

        let ret = libc::nanosleep(&req, &mut rem);
        let err = std::io::Error::last_os_error();

        // Nanosleep should return -1
        assert_eq!(ret, -1);

        // Errno should be EINTR
        assert_eq!(err.raw_os_error(), Some(libc::EINTR));

        // The signal handler should have run
        assert!(SIGNAL_CAUGHT.load(Ordering::Relaxed));

        // The remaining time should be updated (approx 4 seconds left)
        assert!(rem.tv_sec >= 3 && rem.tv_sec <= 5);
    }
}

register_test!(test_interruptible_nanosleep);

fn test_interruptible_read_pipe() {
    register_handler(libc::SIGALRM, false);

    unsafe {
        let mut fds: [libc::c_int; 2] = [0; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            panic!("pipe failed");
        }

        let pid = libc::getpid();

        if libc::fork() == 0 {
            // in child.
            let req = libc::timespec {
                tv_sec: 1,
                tv_nsec: 0,
            };

            libc::nanosleep(&req, ptr::null_mut());
            libc::kill(pid, libc::SIGALRM);
            libc::exit(0);
        };

        let mut buf = [0u8; 10];
        // Try to read from empty pipe (blocking)
        let ret = libc::read(fds[0], buf.as_mut_ptr() as *mut libc::c_void, 10);
        let err = std::io::Error::last_os_error();

        libc::close(fds[0]);
        libc::close(fds[1]);

        assert_eq!(ret, -1);
        assert_eq!(err.raw_os_error(), Some(libc::EINTR));
        assert!(SIGNAL_CAUGHT.load(Ordering::SeqCst));
    }
}

register_test!(test_interruptible_read_pipe);

fn test_interruptible_waitpid() {
    register_handler(libc::SIGALRM, false);

    unsafe {
        let ppid = libc::getpid();
        let cpid = libc::fork();

        if cpid == 0 {
            // in child.
            let req = libc::timespec {
                tv_sec: 1,
                tv_nsec: 0,
            };

            libc::nanosleep(&req, ptr::null_mut());
            libc::kill(ppid, libc::SIGALRM);
            let req = libc::timespec {
                tv_sec: 10,
                tv_nsec: 0,
            };
            libc::nanosleep(&req, ptr::null_mut());
            libc::exit(0);
        };

        // parent.
        let mut status = 0;
        let ret = libc::waitpid(cpid, &mut status, 0); // Blocking wait
        let err = std::io::Error::last_os_error();

        if ret == -1 && err.raw_os_error() == Some(libc::EINTR) {
            // Now we must actually kill/wait the child to clean up zombies
            libc::kill(cpid, libc::SIGKILL);
            libc::waitpid(cpid, &mut status, 0);
        } else {
            panic!(
                "waitpid returned {:?} (errno: {:?}) instead of -1/EINTR",
                ret,
                err.raw_os_error()
            );
        }
    }
}

register_test!(test_interruptible_waitpid);

fn segfault_child(inner: impl FnOnce()) {
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            panic!("fork failed");
        } else if pid == 0 {
            // Child process
            // Reset rust's SIGSEGV stack overflow signal handler to default
            libc::signal(libc::SIGSEGV, libc::SIG_DFL);
            inner()
        } else {
            // Parent process
            let mut status = 0;
            let rusage = std::ptr::null_mut();
            libc::wait4(pid, &mut status, 0, rusage);

            assert!(libc::WIFSIGNALED(status));
            assert_eq!(libc::WTERMSIG(status), libc::SIGSEGV);
        }
    }
}

fn test_segfault_read() {
    segfault_child(|| {
        let addr: *const u8 = std::hint::black_box(std::ptr::null());
        let _ = unsafe { std::ptr::read(addr) };
    });
    segfault_child(|| {
        // Ensure reading from kernel stack fails
        let addr = 0xffff_ba00_0000_0000 as *const u8;
        let _ = unsafe { std::ptr::read(addr) };
    });
}

register_test!(test_segfault_read);

fn test_segfault_write() {
    segfault_child(|| {
        let addr: *mut u8 = std::hint::black_box(std::ptr::null_mut());
        unsafe { std::ptr::write(addr, 42) };
    });
    segfault_child(|| {
        let addr = 0xffff_ba00_0000_0000 as *mut u8;
        unsafe { std::ptr::write(addr, 42) };
    });
}

register_test!(test_segfault_write);

fn rust_stack_overflow() {
    segfault_child(|| {
        fn recurse(n: usize) -> usize {
            let m = std::hint::black_box(n) + 1;
            recurse(m)
        }
        recurse(0);
    });
}

register_test!(rust_stack_overflow);
