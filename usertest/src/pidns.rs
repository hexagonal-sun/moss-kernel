//! Namespace ABI regressions: run inside MOSS, not on the build host.
use crate::register_test;
use std::{
    ffi::CString,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr,
};
fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}
#[track_caller]
fn ok(n: i32) {
    assert_eq!(n, 0, "{}", std::io::Error::last_os_error());
}
#[track_caller]
fn error(n: libc::c_long, e: i32) {
    assert_eq!(n, -1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(e));
}
fn pid() -> i32 {
    unsafe { libc::syscall(libc::SYS_getpid) as i32 }
}
fn tid() -> i32 {
    unsafe { libc::syscall(libc::SYS_gettid) as i32 }
}
fn open(s: &str) -> OwnedFd {
    let fd = unsafe { libc::open(c(s).as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    assert!(fd >= 0, "{s}: {}", std::io::Error::last_os_error());
    unsafe { OwnedFd::from_raw_fd(fd) }
}
struct Pipe {
    r: OwnedFd,
    w: OwnedFd,
}
impl Pipe {
    fn new() -> Self {
        let mut f = [-1; 2];
        ok(unsafe { libc::pipe(f.as_mut_ptr()) });
        unsafe {
            Self {
                r: OwnedFd::from_raw_fd(f[0]),
                w: OwnedFd::from_raw_fd(f[1]),
            }
        }
    }
    fn send(&self, n: i32) {
        assert_eq!(
            unsafe { libc::write(self.w.as_raw_fd(), (&n as *const i32).cast(), 4) },
            4
        );
    }
    fn recv(&self) -> i32 {
        let mut n = 0i32;
        loop {
            let r = unsafe { libc::read(self.r.as_raw_fd(), (&mut n as *mut i32).cast(), 4) };
            if r == 4 {
                return n;
            }
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EINTR)
            );
        }
    }
}
struct Child {
    pid: i32,
    waited: bool,
}
impl Child {
    fn wait(&mut self) -> i32 {
        let mut s = 0;
        loop {
            let n = unsafe { libc::waitpid(self.pid, &mut s, 0) };
            if n == self.pid {
                self.waited = true;
                return s;
            }
            error(n as _, libc::EINTR);
        }
    }
    fn join(mut self) {
        assert_eq!(self.wait(), 0, "child {}", self.pid);
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        if !self.waited {
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
            }
            self.wait();
        }
    }
}
fn spawn(flags: i32, f: impl FnOnce()) -> Child {
    let n = unsafe { libc::syscall(libc::SYS_clone, flags | libc::SIGCHLD, 0, 0, 0, 0) };
    assert!(n >= 0, "clone: {}", std::io::Error::last_os_error());
    if n == 0 {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            libc::_exit(if r.is_ok() { 0 } else { 1 });
        }
    }
    Child {
        pid: n as i32,
        waited: false,
    }
}
fn proc_mount(path: &str) {
    ok(unsafe { libc::unshare(libc::CLONE_NEWNS) });
    ok(unsafe {
        libc::mount(
            ptr::null(),
            c"/".as_ptr(),
            ptr::null(),
            libc::MS_PRIVATE | libc::MS_REC,
            ptr::null(),
        )
    });
    std::fs::create_dir(path).unwrap();
    ok(unsafe {
        libc::mount(
            c"proc".as_ptr(),
            c(path).as_ptr(),
            c"proc".as_ptr(),
            0,
            ptr::null(),
        )
    });
}
fn ns(path: &str) -> std::path::PathBuf {
    std::fs::read_link(path).unwrap()
}
fn field(path: &str, key: &str) -> Vec<u32> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .find_map(|s| {
            s.strip_prefix(key)
                .map(|s| s.split_whitespace().map(|n| n.parse().unwrap()).collect())
        })
        .unwrap()
}
fn test_pidns_clone_proc_threads_and_visibility() {
    let outer = pid();
    let path = format!("/tmp/pidns-view-{outer}");
    let old = ns("/proc/self/ns/pid");
    spawn(libc::CLONE_NEWPID, || {
        assert_eq!(pid(), 1);
        assert_eq!(tid(), 1);
        assert_eq!(unsafe { libc::getppid() }, 0);
        assert_eq!(unsafe { libc::getpgid(0) }, 1);
        assert_eq!(unsafe { libc::getsid(0) }, 1);
        assert_ne!(ns("/proc/self/ns/pid"), old);
        // An inherited proc mount intentionally still describes the outer view.
        assert_ne!(field("/proc/self/status", "Pid:")[0], 1);
        let ids = field("/proc/self/status", "NSpid:");
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[1], 1);
        proc_mount(&path);
        assert_eq!(ns(&format!("{path}/self")).to_str(), Some("1"));
        assert_eq!(field(&format!("{path}/1/status"), "NSpid:"), [1]);
        assert_eq!(field(&format!("{path}/1/status"), "PPid:"), [0]);
        let entries: Vec<_> = std::fs::read_dir(&path)
            .unwrap()
            .filter_map(|e| e.unwrap().file_name().to_str().unwrap().parse::<u32>().ok())
            .collect();
        assert_eq!(entries, [1]);
        error(unsafe { libc::kill(outer, 0) } as _, libc::ESRCH);
        error(
            unsafe { libc::syscall(libc::SYS_pidfd_open, outer, 0) },
            libc::ESRCH,
        );
        let tpath = path.clone();
        let t = std::thread::spawn(move || {
            assert_eq!(pid(), 1);
            assert!(tid() > 1);
            assert_eq!(
                field(&format!("{tpath}/thread-self/status"), "Pid:"),
                [tid() as u32]
            );
            assert_eq!(field(&format!("{tpath}/thread-self/status"), "Tgid:"), [1]);
        });
        // Do not override the runtime's clear-TID in an actual pthread.
        t.join().unwrap();
        spawn(0, || {
            assert!(pid() > 1);
            assert_eq!(pid(), tid());
            assert_eq!(unsafe { libc::getppid() }, 1);
            let mut word = 0;
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_set_tid_address, &mut word) },
                tid() as libc::c_long
            );
        })
        .join();
        ok(unsafe { libc::umount(c(&path).as_ptr()) });
    })
    .join();
    std::fs::remove_dir(path).unwrap();
}
register_test!(test_pidns_clone_proc_threads_and_visibility);

fn test_pidns_unshare_and_setns_children_only() {
    let original = pid();
    let active = open("/proc/self/ns/pid");
    let active_link = ns("/proc/self/ns/pid");
    ok(unsafe { libc::unshare(libc::CLONE_NEWPID) });
    assert_eq!(pid(), original);
    assert_eq!(ns("/proc/self/ns/pid"), active_link);
    error(
        unsafe { libc::open(c"/proc/self/ns/pid_for_children".as_ptr(), libc::O_RDONLY) } as _,
        libc::ENOENT,
    );
    error(
        unsafe { libc::unshare(libc::CLONE_NEWPID) } as _,
        libc::EINVAL,
    );
    let ready = Pipe::new();
    let release = Pipe::new();
    let child = spawn(0, || {
        assert_eq!(pid(), 1);
        ready.send(1);
        release.recv();
    });
    assert_eq!(ready.recv(), 1);
    let childns = open("/proc/self/ns/pid_for_children");
    assert_ne!(ns("/proc/self/ns/pid_for_children"), active_link);
    assert_eq!(
        unsafe { libc::ioctl(childns.as_raw_fd(), 0xb703) },
        libc::CLONE_NEWPID
    );
    let parent = unsafe { libc::ioctl(childns.as_raw_fd(), 0xb702) };
    assert!(parent >= 0);
    unsafe {
        libc::close(parent);
    }
    spawn(0, || {
        assert_eq!(pid(), 2);
        assert_eq!(unsafe { libc::getppid() }, 0);
    })
    .join();
    ok(unsafe { libc::setns(active.as_raw_fd(), libc::CLONE_NEWPID) });
    spawn(0, || {
        assert_ne!(pid(), 1);
    })
    .join();
    ok(unsafe { libc::setns(childns.as_raw_fd(), 0) });
    spawn(0, || {
        assert_eq!(pid(), 3);
    })
    .join();
    ok(unsafe { libc::setns(active.as_raw_fd(), 0) });
    release.send(1);
    child.join();
    // Namespace handles survive init, but the PID allocator must stay dead.
    ok(unsafe { libc::setns(childns.as_raw_fd(), 0) });
    error(
        unsafe { libc::syscall(libc::SYS_clone, libc::SIGCHLD, 0, 0, 0, 0) },
        libc::ENOMEM,
    );
    ok(unsafe { libc::setns(active.as_raw_fd(), 0) });
}
register_test!(test_pidns_unshare_and_setns_children_only);

fn test_pidns_nested_and_setns_cannot_escape() {
    let ancestor = open("/proc/self/ns/pid");
    spawn(libc::CLONE_NEWPID, || {
        let own = open("/proc/self/ns/pid");
        error(
            unsafe { libc::setns(ancestor.as_raw_fd(), 0) } as _,
            libc::EINVAL,
        );
        error(
            unsafe { libc::ioctl(own.as_raw_fd(), 0xb702) } as _,
            libc::EPERM,
        );
        spawn(libc::CLONE_NEWPID, || {
            assert_eq!(pid(), 1);
            assert_eq!(unsafe { libc::getppid() }, 0);
            let v = field("/proc/self/status", "NSpid:");
            assert_eq!(v.len(), 3);
            assert_eq!(v[2], 1);
            error(
                unsafe { libc::setns(own.as_raw_fd(), 0) } as _,
                libc::EINVAL,
            );
        })
        .join();
        assert_eq!(pid(), 1);
    })
    .join();
}
register_test!(test_pidns_nested_and_setns_cannot_escape);

fn test_pidns_init_signals_orphans_and_teardown() {
    let report = Pipe::new();
    let grand_ready = Pipe::new();
    let go = Pipe::new();
    let child = spawn(libc::CLONE_NEWPID, || {
        ok(unsafe { libc::kill(1, libc::SIGTERM) }); // default disposition cannot kill namespace init
        std::thread::sleep(std::time::Duration::from_millis(10));
        let grand = spawn(0, || {
            let orphan = spawn(0, || {
                grand_ready.send(field("/proc/self/status", "Pid:")[0] as i32);
                go.recv();
                assert_eq!(unsafe { libc::getppid() }, 1);
                report.send(1);
                loop {
                    unsafe {
                        libc::pause();
                    }
                }
            });
            std::mem::forget(orphan);
        });
        grand.join();
        go.send(1);
        // Wait for the orphan to report reparenting before exiting PID 1.
        assert_eq!(report.recv(), 1);
    });
    let orphan_global = grand_ready.recv();
    child.join();
    error(unsafe { libc::kill(orphan_global, 0) } as _, libc::ESRCH);
}
register_test!(test_pidns_init_signals_orphans_and_teardown);

fn test_pidns_waitid_abi_and_group_zombie() {
    spawn(libc::CLONE_NEWPID, || {
        let ready = Pipe::new();
        let go = Pipe::new();
        let mut child = spawn(0, || {
            ok(unsafe { libc::setpgid(0, 0) });
            ready.send(pid());
            go.recv();
            unsafe {
                libc::_exit(37);
            }
        });
        assert_eq!(ready.recv(), child.pid);
        let mut buf = [0xfeed_dead_dead_beefu64; 18];
        ok(unsafe {
            libc::syscall(
                libc::SYS_waitid,
                libc::P_PID,
                child.pid,
                buf.as_mut_ptr().add(1),
                libc::WEXITED | libc::WNOHANG,
                0,
            )
        } as i32);
        assert_eq!(&buf[1..17], &[0; 16]);
        assert_eq!(buf[0], 0xfeed_dead_dead_beef);
        assert_eq!(buf[17], buf[0]);
        // P_PGID=1 must not collapse into waitpid(-1)'s "any child" selector.
        error(
            unsafe {
                libc::syscall(
                    libc::SYS_waitid,
                    libc::P_PGID,
                    1,
                    buf.as_mut_ptr().add(1),
                    libc::WEXITED | libc::WNOHANG,
                    0,
                )
            },
            libc::ECHILD,
        );
        go.send(1);
        ok(unsafe {
            libc::syscall(
                libc::SYS_waitid,
                libc::P_PID,
                child.pid,
                buf.as_mut_ptr().add(1),
                libc::WEXITED | libc::WNOWAIT,
                0,
            )
        } as i32);
        let raw = unsafe { std::slice::from_raw_parts(buf.as_ptr().add(1).cast::<i32>(), 32) };
        assert_eq!(
            (raw[0], raw[1], raw[2], raw[4], raw[6]),
            (libc::SIGCHLD, 0, libc::CLD_EXITED, child.pid, 37)
        );
        assert_eq!(buf[0], 0xfeed_dead_dead_beef);
        assert_eq!(buf[17], buf[0]);
        // The task may already be freed: PGID selection still uses the zombie's identity.
        let mut s = 0;
        assert_eq!(unsafe { libc::waitpid(-child.pid, &mut s, 0) }, child.pid);
        assert_eq!(s, 37 << 8);
        child.waited = true;
        error(
            unsafe { libc::waitpid(9999, &mut s, libc::WNOHANG) } as _,
            libc::ECHILD,
        );
    })
    .join();
}
register_test!(test_pidns_waitid_abi_and_group_zombie);

fn test_pidns_clone_invalid_and_permission_checks() {
    for flags in [
        libc::CLONE_NEWPID | libc::CLONE_PARENT,
        libc::CLONE_NEWPID | libc::CLONE_THREAD | libc::CLONE_VM | libc::CLONE_SIGHAND,
    ] {
        error(
            unsafe { libc::syscall(libc::SYS_clone, flags | libc::SIGCHLD, 0, 0, 0, 0) },
            libc::EINVAL,
        );
    }
    spawn(0, || {
        ok(unsafe { libc::setuid(1000) });
        error(
            unsafe { libc::unshare(libc::CLONE_NEWPID) } as _,
            libc::EPERM,
        );
        error(
            unsafe {
                libc::syscall(
                    libc::SYS_clone,
                    libc::CLONE_NEWPID | libc::SIGCHLD,
                    0,
                    0,
                    0,
                    0,
                )
            },
            libc::EPERM,
        );
    })
    .join();
    spawn(
        libc::CLONE_NEWUSER | libc::CLONE_NEWPID | libc::CLONE_NEWNS,
        || {
            assert_eq!(pid(), 1);
            let fd = open("/proc/self/ns/pid");
            let owner = unsafe { libc::ioctl(fd.as_raw_fd(), 0xb701) };
            assert!(owner >= 0);
            assert_eq!(unsafe { libc::ioctl(owner, 0xb703) }, libc::CLONE_NEWUSER);
            unsafe {
                libc::close(owner);
            }
        },
    )
    .join();
}
register_test!(test_pidns_clone_invalid_and_permission_checks);

fn test_pidns_clone_tid_stores_and_session_translation() {
    let mut parent_tid = -7i32;
    let mut child_tid = -9i32;
    // AArch64 raw clone order is flags, stack, parent_tid, tls, child_tid.
    let child = unsafe {
        libc::syscall(
            libc::SYS_clone,
            libc::CLONE_NEWPID
                | libc::CLONE_PARENT_SETTID
                | libc::CLONE_CHILD_SETTID
                | libc::SIGCHLD,
            0,
            &mut parent_tid,
            0,
            &mut child_tid,
        )
    };
    assert!(child >= 0);
    if child == 0 {
        let r = std::panic::catch_unwind(|| {
            assert_eq!(pid(), 1);
            assert_eq!(child_tid, 1);
            assert_eq!(parent_tid, -7);
        });
        unsafe {
            libc::_exit(if r.is_ok() { 0 } else { 1 });
        }
    }
    assert_eq!(parent_tid, child as i32);
    assert_eq!(child_tid, -9);
    Child {
        pid: child as i32,
        waited: false,
    }
    .join();
    spawn(libc::CLONE_NEWPID, || {
        let ready = Pipe::new();
        let go = Pipe::new();
        let child = spawn(0, || {
            assert_eq!(unsafe { libc::setsid() }, pid());
            assert_eq!(unsafe { libc::getpgid(0) }, pid());
            error(unsafe { libc::setsid() } as _, libc::EPERM);
            ready.send(pid());
            go.recv();
        });
        assert_eq!(ready.recv(), child.pid);
        assert_eq!(unsafe { libc::getsid(child.pid) }, child.pid);
        error(unsafe { libc::setpgid(child.pid, 0) } as _, libc::EPERM);
        go.send(1);
        child.join();
    })
    .join();
}
register_test!(test_pidns_clone_tid_stores_and_session_translation);

fn test_pidns_init_exit_stops_running_threads() {
    spawn(libc::CLONE_NEWPID, || {
        let ready = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..3 {
            let ready = ready.clone();
            std::mem::forget(std::thread::spawn(move || {
                ready.fetch_add(1, std::sync::atomic::Ordering::Release);
                loop {
                    std::hint::spin_loop();
                }
            }));
        }
        while ready.load(std::sync::atomic::Ordering::Acquire) != 3 {
            std::thread::yield_now();
        }
        // exit_group must synchronize with other CPUs before parent wait returns.
    })
    .join();
}
register_test!(test_pidns_init_exit_stops_running_threads);
