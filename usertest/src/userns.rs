//! These are guest syscall tests, never host-kernel substitutes for MOSS.
use crate::register_test;
use std::{
    ffi::CString,
    io::{Read, Write},
    mem::zeroed,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr,
};

fn path(s: &str) -> CString {
    CString::new(s).unwrap()
}
#[track_caller]
fn error(ret: libc::c_long, errno: i32) {
    assert_eq!(ret, -1, "expected errno {errno}");
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(errno));
}
fn open(s: &str, flags: i32) -> OwnedFd {
    let fd = unsafe { libc::open(path(s).as_ptr(), flags, 0o600) };
    assert!(fd >= 0, "open {s}: {}", std::io::Error::last_os_error());
    unsafe { OwnedFd::from_raw_fd(fd) }
}
fn write(s: &str, data: &str) {
    // The same open flags as shell redirection, not a special map-file writer.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(s)
        .unwrap();
    assert_eq!(file.write(data.as_bytes()).unwrap(), data.len());
}
fn bad_write(s: &str, data: &str, errno: i32) {
    let fd = open(s, libc::O_WRONLY);
    error(
        unsafe { libc::write(fd.as_raw_fd(), data.as_ptr().cast(), data.len()) } as _,
        errno,
    );
}
fn numbers(s: &str) -> Vec<u32> {
    std::fs::read_to_string(s)
        .unwrap()
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect()
}
fn ns_id() -> std::path::PathBuf {
    std::fs::read_link("/proc/self/ns/user").unwrap()
}
fn unshare() {
    assert_eq!(unsafe { libc::unshare(libc::CLONE_NEWUSER) }, 0);
}
fn map_self(uid: u32, gid: u32) {
    write("/proc/self/uid_map", &format!("0 {uid} 1\n"));
    write("/proc/self/setgroups", "deny\n");
    write("/proc/self/gid_map", &format!("0 {gid} 1\n"));
}
fn drop_ids() {
    unsafe {
        assert_eq!(libc::setgroups(0, ptr::null()), 0);
        assert_eq!(libc::setgid(1000), 0);
        assert_eq!(libc::setuid(1000), 0);
        // Linux proc ownership changes when dumpability is disabled by setuid.
        assert_eq!(libc::prctl(libc::PR_SET_DUMPABLE, 1, 0, 0, 0), 0);
    }
}
struct Child {
    pid: i32,
    waited: bool,
}
impl Child {
    fn join(mut self) {
        let status = self.wait();
        assert_eq!(status, 0, "userns child {} failed", self.pid);
    }
    fn wait(&mut self) -> i32 {
        let mut status = 0;
        loop {
            let ret = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if ret == self.pid {
                break;
            }
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EINTR)
            );
        }
        self.waited = true;
        status
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
    let pid = unsafe { libc::syscall(libc::SYS_clone, flags | libc::SIGCHLD, 0, 0, 0, 0) };
    assert!(pid >= 0, "clone: {}", std::io::Error::last_os_error());
    if pid == 0 {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            libc::_exit(if result.is_ok() { 0 } else { 1 });
        }
    }
    Child {
        pid: pid as i32,
        waited: false,
    }
}
struct Pipe {
    read: OwnedFd,
    write: OwnedFd,
}
impl Pipe {
    fn new() -> Self {
        let mut fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe {
            Self {
                read: OwnedFd::from_raw_fd(fds[0]),
                write: OwnedFd::from_raw_fd(fds[1]),
            }
        }
    }
    fn send(&self) {
        assert_eq!(
            unsafe { libc::write(self.write.as_raw_fd(), b"!".as_ptr().cast(), 1) },
            1
        );
    }
    fn recv(&self) {
        let mut byte = 0u8;
        loop {
            let ret =
                unsafe { libc::read(self.read.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) };
            if ret == 1 {
                break;
            }
            error(ret as _, libc::EINTR);
        }
    }
}

#[repr(C)]
struct Header {
    version: u32,
    pid: i32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}
fn caps() -> [CapData; 2] {
    let mut data = [CapData::default(); 2];
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_capget,
                &Header {
                    version: 0x20080522,
                    pid: 0,
                },
                data.as_mut_ptr(),
            )
        },
        0
    );
    data
}

fn test_userns_creation_and_atomic_errors() {
    let parent_ns = ns_id();
    assert_eq!(numbers("/proc/self/uid_map"), [0, 0, u32::MAX]);
    for flags in [
        libc::CLONE_NEWUSER | libc::CLONE_FS,
        libc::CLONE_NEWUSER | libc::CLONE_THREAD | libc::CLONE_VM | libc::CLONE_SIGHAND,
    ] {
        error(
            unsafe { libc::syscall(libc::SYS_clone, flags | libc::SIGCHLD, 0, 0, 0, 0) },
            libc::EINVAL,
        );
    }
    error(
        unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) } as _,
        libc::EINVAL,
    );
    assert_eq!(ns_id(), parent_ns);
    spawn(libc::CLONE_NEWUSER, || {
        assert_ne!(ns_id(), parent_ns);
        assert_eq!(unsafe { libc::getuid() }, 65534);
        assert_eq!(unsafe { libc::getgid() }, 65534);
        assert!(numbers("/proc/self/uid_map").is_empty());
        assert_eq!(caps()[0].effective, u32::MAX);
        assert_eq!(caps()[0].inheritable, 0);
        error(unsafe { libc::setuid(0) } as _, libc::EINVAL);
        error(unsafe { libc::setgid(0) } as _, libc::EINVAL);
        error(unsafe { libc::setgroups(0, ptr::null()) } as _, libc::EPERM);
        error(
            unsafe { libc::unshare(libc::CLONE_NEWUSER) } as _,
            libc::EPERM,
        );
    })
    .join();
    assert_eq!(ns_id(), parent_ns);
    spawn(0, || {
        assert_eq!(unsafe { libc::chroot(c"/tmp".as_ptr()) }, 0);
        error(
            unsafe { libc::unshare(libc::CLONE_NEWUSER) } as _,
            libc::EPERM,
        );
    })
    .join();
}
register_test!(test_userns_creation_and_atomic_errors);

fn test_userns_control_truncate_preserves_state() {
    spawn(libc::CLONE_NEWUSER, || {
        map_self(0, 0);
        for name in ["uid_map", "gid_map", "setgroups"] {
            let name = format!("/proc/self/{name}");
            let before = std::fs::read_to_string(&name).unwrap();
            let fd = open(&name, libc::O_WRONLY | libc::O_TRUNC);
            assert_eq!(unsafe { libc::ftruncate(fd.as_raw_fd(), 123) }, 0);
            assert_eq!(unsafe { libc::truncate(path(&name).as_ptr(), 0) }, 0);
            assert_eq!(std::fs::read_to_string(&name).unwrap(), before);
        }
        bad_write("/proc/self/uid_map", "0 0 1\n", libc::EPERM);
        bad_write("/proc/self/gid_map", "0 0 1\n", libc::EPERM);
        bad_write("/proc/self/setgroups", "allow\n", libc::EPERM);
    })
    .join();
}
register_test!(test_userns_control_truncate_preserves_state);

fn test_userns_unprivileged_mapping_and_permissions() {
    let parent = unsafe { libc::getpid() };
    for base in ["", "/tmp"] {
        let dir = format!("{base}/userns-dac-{parent}");
        std::fs::create_dir(&dir).unwrap();
        let root_file = format!("{dir}/root");
        let owned_file = format!("{dir}/owned");
        std::fs::write(&root_file, b"root only").unwrap();
        std::fs::write(&owned_file, b"owned").unwrap();
        unsafe {
            assert_eq!(libc::chmod(path(&dir).as_ptr(), 0o777), 0);
            assert_eq!(libc::chmod(path(&root_file).as_ptr(), 0o600), 0);
            assert_eq!(libc::chown(path(&owned_file).as_ptr(), 1000, 1000), 0);
            assert_eq!(libc::chmod(path(&owned_file).as_ptr(), 0), 0);
        }
        spawn(0, || unsafe {
            drop_ids();
            unshare();
            bad_write("/proc/self/uid_map", "0 0 1\n", libc::EPERM);
            bad_write("/proc/self/gid_map", "0 1000 1\n", libc::EPERM);
            map_self(1000, 1000);
            assert_eq!(libc::getuid(), 0);
            assert_eq!(libc::getgid(), 0);
            assert_eq!(libc::setfsuid(u32::MAX), 0);
            assert_eq!(libc::setfsgid(u32::MAX), 0);
            error(libc::setresuid(0, 1, u32::MAX) as _, libc::EINVAL);
            assert_eq!(libc::geteuid(), 0);
            error(libc::setgroups(0, ptr::null()) as _, libc::EPERM);
            bad_write("/proc/self/setgroups", "allow\n", libc::EPERM);
            bad_write("/proc/self/uid_map", "0 1000 1\n", libc::EPERM);
            assert_eq!(std::fs::read(&owned_file).unwrap(), b"owned"); // mapped DAC override
            error(
                libc::open(path(&root_file).as_ptr(), libc::O_RDONLY) as _,
                libc::EACCES,
            );
            error(
                libc::chmod(path(&root_file).as_ptr(), 0o777) as _,
                libc::EPERM,
            );
            error(
                libc::chown(path(&owned_file).as_ptr(), 1, 0) as _,
                libc::EINVAL,
            );
            let handle = open(&root_file, libc::O_PATH);
            let mut st: libc::stat = zeroed();
            assert_eq!(libc::fstat(handle.as_raw_fd(), &mut st), 0);
            assert_eq!((st.st_uid, st.st_gid), (65534, 65534));
            assert_eq!(libc::stat(path(&owned_file).as_ptr(), &mut st), 0);
            assert_eq!((st.st_uid, st.st_gid), (0, 0));
            // AArch64 statx UAPI: 256 bytes, UID/GID at offsets 20/24.
            // The project's musl libc crate does not expose struct statx.
            let mut stx = [0u64; 32];
            assert_eq!(
                libc::syscall(
                    libc::SYS_statx,
                    libc::AT_FDCWD,
                    path(&owned_file).as_ptr(),
                    0,
                    0x7ff,
                    &mut stx
                ),
                0
            );
            assert_eq!(((stx[2] >> 32) as u32, stx[3] as u32), (0, 0));
            std::fs::write(format!("{dir}/new"), b"new").unwrap();
            error(
                libc::sethostname(c"forbidden".as_ptr(), 9) as _,
                libc::EPERM,
            );
            error(
                libc::mount(
                    ptr::null(),
                    c"/tmp".as_ptr(),
                    c"tmpfs".as_ptr(),
                    0,
                    ptr::null(),
                ) as _,
                libc::EPERM,
            );
            error(
                libc::syscall(
                    libc::SYS_reboot,
                    0xfee1deadu32,
                    672274793u32,
                    0x89abcdefu32,
                    0,
                ),
                libc::EPERM,
            );
            let time = libc::timespec {
                tv_sec: 1000,
                tv_nsec: 0,
            };
            error(
                libc::clock_settime(libc::CLOCK_REALTIME, &time) as _,
                libc::EPERM,
            );
            error(libc::kill(parent, 0) as _, libc::EPERM);
            error(
                libc::syscall(libc::SYS_tgkill, parent, parent, 0),
                libc::EPERM,
            );
            let rlim = libc::rlimit {
                rlim_cur: 1,
                rlim_max: 1,
            };
            error(
                libc::syscall(libc::SYS_prlimit64, parent, libc::RLIMIT_NOFILE, &rlim, 0),
                libc::EPERM,
            );
            error(
                libc::syscall(libc::SYS_ptrace, libc::PTRACE_GETREGSET, parent, 1, 0),
                libc::EPERM,
            );
            let mut byte = 0u8;
            let iov = libc::iovec {
                iov_base: (&mut byte as *mut u8).cast(),
                iov_len: 1,
            };
            error(
                libc::syscall(libc::SYS_process_vm_readv, parent, &iov, 1, &iov, 1, 0),
                libc::EPERM,
            );
            error(
                libc::open(
                    path(&format!("/proc/{parent}/ns/user")).as_ptr(),
                    libc::O_RDONLY,
                ) as _,
                libc::EACCES,
            );
            let status = std::fs::read_to_string("/proc/self/status").unwrap();
            assert!(status.contains("Uid:\t0\t0\t0\t0\n"));
        })
        .join();
        let mut st: libc::stat = unsafe { zeroed() };
        assert_eq!(
            unsafe { libc::stat(path(&format!("{dir}/new")).as_ptr(), &mut st) },
            0
        );
        assert_eq!((st.st_uid, st.st_gid), (1000, 1000));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
register_test!(test_userns_unprivileged_mapping_and_permissions);

fn test_userns_nested_mapping_and_setgroups() {
    spawn(0, || {
        drop_ids();
        unshare();
        map_self(1000, 1000);
        let outer = ns_id();
        unshare();
        assert_ne!(ns_id(), outer);
        assert_eq!(
            std::fs::read_to_string("/proc/self/setgroups").unwrap(),
            "deny\n"
        );
        bad_write("/proc/self/setgroups", "allow\n", libc::EPERM);
        map_self(0, 0);
        assert_eq!(numbers("/proc/self/uid_map"), [0, 0, 1]);
        assert_eq!(unsafe { libc::getuid() }, 0);
        assert_eq!(unsafe { libc::setresuid(0, 0, 0) }, 0);
        let mut ids = [u32::MAX; 3];
        assert_eq!(
            unsafe { libc::getresuid(&mut ids[0], &mut ids[1], &mut ids[2]) },
            0
        );
        assert_eq!(ids, [0; 3]);
    })
    .join();
}
register_test!(test_userns_nested_mapping_and_setgroups);

fn test_userns_map_validation_and_writer_race() {
    let gate = Pipe::new();
    let child = spawn(libc::CLONE_NEWUSER, || gate.recv());
    let uid_map = format!("/proc/{}/uid_map", child.pid);
    for bad in [
        "",
        "0 0 0\n",
        "0 0 4294967296\n",
        "4294967295 0 1\n",
        "0 4294967295 1\n",
        "0 0 2\n1 4 1\n",
        "0 0 2\n4 1 1\n",
        "0 0 1 extra\n",
        "-1 0 1\n",
        "0 0 1\n\n",
    ] {
        bad_write(&uid_map, bad, libc::EINVAL);
    }
    let fd = open(&uid_map, libc::O_WRONLY);
    error(
        unsafe { libc::pwrite(fd.as_raw_fd(), b"0 0 1\n".as_ptr().cast(), 6, 1) } as _,
        libc::EINVAL,
    );
    let long = vec![b' '; 4096];
    error(
        unsafe { libc::write(fd.as_raw_fd(), long.as_ptr().cast(), long.len()) } as _,
        libc::EINVAL,
    );
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let name = uid_map.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let fd = open(&name, libc::O_WRONLY);
                barrier.wait();
                let ret = unsafe { libc::write(fd.as_raw_fd(), b"0 0 2\n".as_ptr().cast(), 6) };
                if ret == -1 {
                    error(ret as _, libc::EPERM);
                    false
                } else {
                    assert_eq!(ret, 6);
                    true
                }
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|t| usize::from(t.join().unwrap()))
            .sum::<usize>(),
        1
    );
    assert_eq!(numbers(&uid_map), [0, 0, 2]);
    gate.send();
    child.join();
}
register_test!(test_userns_map_validation_and_writer_race);

fn test_userns_namespace_fd_lifetime_and_setns() {
    let gate = Pipe::new();
    let parent_fd = open("/proc/self/ns/user", libc::O_RDONLY);
    let child = spawn(libc::CLONE_NEWUSER, || gate.recv());
    write(&format!("/proc/{}/uid_map", child.pid), "0 0 1\n");
    write(&format!("/proc/{}/gid_map", child.pid), "0 0 1\n");
    let child_fd = open(&format!("/proc/{}/ns/user", child.pid), libc::O_RDONLY);
    let map_fd = open(&format!("/proc/{}/uid_map", child.pid), libc::O_RDONLY);
    let mut st: libc::stat = unsafe { zeroed() };
    assert_eq!(unsafe { libc::fstat(child_fd.as_raw_fd(), &mut st) }, 0);
    gate.send();
    child.join();
    let mut map_file = std::fs::File::from(map_fd);
    let mut text = String::new();
    map_file.read_to_string(&mut text).unwrap();
    assert_eq!(text.split_whitespace().collect::<Vec<_>>(), ["0", "0", "1"]);
    spawn(0, || unsafe {
        let mut stats: libc::statfs = zeroed();
        assert_eq!(libc::fstatfs(child_fd.as_raw_fd(), &mut stats), 0);
        assert_eq!(stats.f_type as u64, 0x6e736673);
        assert_eq!(
            libc::ioctl(child_fd.as_raw_fd(), 0xb703),
            libc::CLONE_NEWUSER
        );
        let owner_fd = libc::ioctl(child_fd.as_raw_fd(), 0xb702);
        assert!(owner_fd >= 0);
        assert_ne!(libc::fcntl(owner_fd, libc::F_GETFD) & libc::FD_CLOEXEC, 0);
        libc::close(owner_fd);
        assert_eq!(libc::setns(child_fd.as_raw_fd(), libc::CLONE_NEWUSER), 0);
        assert_eq!(libc::getuid(), 0);
        assert_eq!(ns_id().to_str().unwrap(), format!("user:[{}]", st.st_ino));
        error(libc::setns(child_fd.as_raw_fd(), 0) as _, libc::EINVAL);
        error(libc::setns(parent_fd.as_raw_fd(), 0) as _, libc::EPERM);
        error(libc::ioctl(child_fd.as_raw_fd(), 0xb702) as _, libc::EPERM);
        let own_path = open("/proc/self/ns/user", libc::O_PATH);
        error(libc::setns(own_path.as_raw_fd(), 0) as _, libc::EBADF);
        error(
            libc::setns(child_fd.as_raw_fd(), libc::CLONE_NEWPID) as _,
            libc::EINVAL,
        );
    })
    .join();
}
register_test!(test_userns_namespace_fd_lifetime_and_setns);

fn test_userns_map_fd_does_not_lend_parent_privileges() {
    let gate = Pipe::new();
    let target = spawn(libc::CLONE_NEWUSER, || gate.recv());
    let name = format!("/proc/{}/uid_map", target.pid);
    let map = open(&name, libc::O_WRONLY);
    let ns = open(&format!("/proc/{}/ns/user", target.pid), libc::O_RDONLY);
    spawn(0, || unsafe {
        assert_eq!(libc::setns(ns.as_raw_fd(), 0), 0);
        // The opener was privileged in the parent, but the current writer isn't.
        error(
            libc::write(map.as_raw_fd(), b"0 0 2\n".as_ptr().cast(), 6) as _,
            libc::EPERM,
        );
    })
    .join();
    write(&name, "0 0 2\n");
    gate.send();
    target.join();
}
register_test!(test_userns_map_fd_does_not_lend_parent_privileges);

fn test_userns_unshare_fs_and_thread_restrictions() {
    let ready = Pipe::new();
    let gate = Pipe::new();
    let cwd = std::env::current_dir().unwrap();
    let old_umask = unsafe { libc::umask(0o022) };
    let child = spawn(libc::CLONE_FS, || {
        unshare();
        assert_eq!(unsafe { libc::chdir(c"/tmp".as_ptr()) }, 0);
        unsafe {
            libc::umask(0o077);
        }
        ready.send();
        gate.recv();
        assert_eq!(std::env::current_dir().unwrap().to_str().unwrap(), "/tmp");
    });
    ready.recv();
    assert_eq!(std::env::current_dir().unwrap(), cwd);
    assert_eq!(unsafe { libc::umask(old_umask) }, 0o022);
    gate.send();
    child.join();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let b = barrier.clone();
    let thread = std::thread::spawn(move || {
        b.wait();
        b.wait();
    });
    barrier.wait();
    let ns = ns_id();
    error(
        unsafe { libc::unshare(libc::CLONE_NEWUSER) } as _,
        libc::EINVAL,
    );
    assert_eq!(ns_id(), ns);
    barrier.wait();
    thread.join().unwrap();
}
register_test!(test_userns_unshare_fs_and_thread_restrictions);

pub fn exec_probe() -> bool {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("--userns-exec-probe") {
        return false;
    }
    let mapped = args.get(2).map(String::as_str) == Some("mapped");
    assert_eq!(unsafe { libc::getuid() }, if mapped { 0 } else { 65534 });
    assert_eq!(caps()[0].effective, if mapped { u32::MAX } else { 0 });
    assert_eq!(caps()[0].permitted, if mapped { u32::MAX } else { 0 });
    true
}
fn test_userns_exec_recalculates_capabilities() {
    let executable = std::env::current_exe().unwrap();
    for mapped in [false, true] {
        spawn(0, || {
            drop_ids();
            unshare();
            if mapped {
                map_self(1000, 1000);
            }
            let status = std::process::Command::new(&executable)
                .arg("--userns-exec-probe")
                .arg(if mapped { "mapped" } else { "unmapped" })
                .status()
                .unwrap();
            assert!(status.success());
        })
        .join();
    }
}
register_test!(test_userns_exec_recalculates_capabilities);

fn test_userns_clone_tid_stores_use_correct_address_space() {
    let mut child_word = 0x1234_5678u32;
    let mut parent_word = 0u32;
    let flags =
        libc::CLONE_NEWUSER | libc::CLONE_PARENT_SETTID | libc::CLONE_CHILD_SETTID | libc::SIGCHLD;
    let pid = unsafe {
        libc::syscall(
            libc::SYS_clone,
            flags,
            0,
            &mut parent_word,
            0,
            &mut child_word,
        )
    };
    assert!(pid >= 0);
    if pid == 0 {
        let value = unsafe { ptr::read_volatile(&child_word) };
        unsafe {
            // Raw clone bypasses musl's fork wrapper and its cached pthread
            // TID update. Check the actual kernel TID on both libc variants.
            libc::_exit(if value == libc::syscall(libc::SYS_gettid) as u32 {
                0
            } else {
                1
            });
        }
    }
    let child = Child {
        pid: pid as i32,
        waited: false,
    };
    assert_eq!(unsafe { ptr::read_volatile(&parent_word) }, pid as u32);
    assert_eq!(unsafe { ptr::read_volatile(&child_word) }, 0x1234_5678);
    child.join();
}
register_test!(test_userns_clone_tid_stores_use_correct_address_space);
