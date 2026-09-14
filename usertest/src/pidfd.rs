//! Linux pidfs statfs contract, exercised inside the AArch64 MOSS guest.
use crate::register_test;
use std::{
    ffi::CString,
    mem::{size_of, zeroed},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr,
};

const PID_FS_MAGIC: u64 = 0x5049_4446;
const PIDFD_THREAD: i32 = libc::O_EXCL;
const ST_VALID: i64 = 0x20;

// asm-generic/statfs.h, not the host's layout or libc's private fsid/spare fields.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct StatFs {
    kind: i64,
    bsize: i64,
    blocks: u64,
    bfree: u64,
    bavail: u64,
    files: u64,
    ffree: u64,
    fsid: [i32; 2],
    namelen: i64,
    frsize: i64,
    flags: i64,
    spare: [i64; 4],
}

fn own_fd(fd: i32) -> OwnedFd {
    assert!(
        fd >= 0,
        "fd creation failed: {}",
        std::io::Error::last_os_error()
    );
    unsafe { OwnedFd::from_raw_fd(fd) }
}

fn pidfd(pid: i32, flags: i32) -> OwnedFd {
    own_fd(unsafe { libc::syscall(libc::SYS_pidfd_open, pid, flags) } as i32)
}

fn fstatfs(fd: i32) -> StatFs {
    let mut stat = StatFs::default();
    assert_eq!(
        unsafe { libc::syscall(libc::SYS_fstatfs, fd, &mut stat) },
        0
    );
    stat
}

fn statfs(path: &str) -> StatFs {
    let path = CString::new(path).unwrap();
    let mut stat = StatFs::default();
    assert_eq!(
        unsafe { libc::syscall(libc::SYS_statfs, path.as_ptr(), &mut stat) },
        0,
        "statfs({path:?}): {}",
        std::io::Error::last_os_error()
    );
    stat
}

fn check(stat: StatFs) {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    assert!(page_size > 0);
    assert_eq!(stat.kind as u64, PID_FS_MAGIC);
    assert_eq!(stat.bsize, page_size as i64);
    assert_eq!(stat.frsize, page_size as i64);
    assert_eq!((stat.blocks, stat.bfree, stat.bavail), (0, 0, 0));
    assert_eq!((stat.files, stat.ffree), (0, 0));
    assert_ne!(stat.fsid, [0; 2]); // An instance ID, not a PID or the magic.
    assert_eq!(stat.namelen, 255);
    assert_eq!(stat.flags, ST_VALID);
    assert_eq!(stat.spare, [0; 4]);
}

fn expect_error(ret: libc::c_long, errno: i32) {
    assert_eq!(ret, -1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(errno));
}

fn wait_child(pid: i32) {
    let mut status = 0;
    loop {
        let ret = unsafe { libc::waitpid(pid, &mut status, 0) };
        if ret == pid {
            break;
        }
        expect_error(ret as _, libc::EINTR);
    }
    assert!(libc::WIFEXITED(status), "child wait status {status}");
    assert_eq!(libc::WEXITSTATUS(status), 0);
}

fn test_pidfd_fstatfs() {
    let fd = pidfd(unsafe { libc::getpid() }, 0);
    let raw = fstatfs(fd.as_raw_fd());
    check(raw);
    unsafe {
        let mut stat: libc::statfs = zeroed();
        assert_eq!(libc::fstatfs(fd.as_raw_fd(), &mut stat), 0);
        assert_eq!(size_of::<libc::statfs>(), size_of::<StatFs>());
        assert_eq!(
            ptr::read_unaligned((&stat as *const libc::statfs).cast::<StatFs>()),
            raw
        );
        // GNU's large-file API is the same native 64-bit syscall on AArch64.
        #[cfg(target_env = "gnu")]
        {
            let mut large: libc::statfs64 = zeroed();
            assert_eq!(libc::fstatfs64(fd.as_raw_fd(), &mut large), 0);
            assert_eq!(size_of::<libc::statfs64>(), size_of::<StatFs>());
            assert_eq!(
                ptr::read_unaligned((&large as *const libc::statfs64).cast::<StatFs>()),
                raw
            );
        }
    }
}
register_test!(test_pidfd_fstatfs);

fn test_pidfd_statfs_abi_bounds() {
    assert_eq!(size_of::<StatFs>(), 120);
    let fd = pidfd(unsafe { libc::getpid() }, 0);
    // Unaligned output, nonzero initial contents, and canaries on both ends.
    let mut bytes = [0xa5u8; 122];
    let out = unsafe { bytes.as_mut_ptr().add(1).cast::<StatFs>() };
    assert_eq!(
        unsafe { libc::syscall(libc::SYS_fstatfs, fd.as_raw_fd(), out) },
        0
    );
    check(unsafe { ptr::read_unaligned(out) });
    assert_eq!(bytes[0], 0xa5);
    assert_eq!(bytes[121], 0xa5);
}
register_test!(test_pidfd_statfs_abi_bounds);

fn test_pidfd_statfs_instance_and_flags() {
    let pid = unsafe { libc::getpid() };
    let baseline = fstatfs(pidfd(pid, 0).as_raw_fd());
    check(baseline);
    for flags in [
        0,
        libc::O_NONBLOCK,
        PIDFD_THREAD,
        PIDFD_THREAD | libc::O_NONBLOCK,
    ] {
        let fd = pidfd(pid, flags);
        assert_eq!(
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) },
            libc::FD_CLOEXEC
        );
        let dup = own_fd(unsafe { libc::dup(fd.as_raw_fd()) });
        assert_eq!(fstatfs(fd.as_raw_fd()), baseline);
        drop(fd);
        assert_eq!(fstatfs(dup.as_raw_fd()), baseline);
        assert_eq!(unsafe { libc::fcntl(dup.as_raw_fd(), libc::F_GETFD) }, 0);
        assert_eq!(
            unsafe { libc::fcntl(dup.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
            0
        );
        assert_eq!(fstatfs(dup.as_raw_fd()), baseline);
        assert_eq!(
            unsafe { libc::fcntl(dup.as_raw_fd(), libc::F_GETFL) } & PIDFD_THREAD,
            flags & PIDFD_THREAD
        );
    }
    for path in ["/", "/tmp", "/proc"] {
        let other = statfs(path);
        assert_ne!(other.kind as u64, PID_FS_MAGIC);
        assert_ne!(other.fsid, baseline.fsid);
    }
}
register_test!(test_pidfd_statfs_instance_and_flags);

fn test_pidfd_statfs_paths() {
    let fd = pidfd(unsafe { libc::getpid() }, 0);
    let baseline = fstatfs(fd.as_raw_fd());
    let path = format!("/proc/self/fd/{}", fd.as_raw_fd());
    assert_eq!(statfs(&path), baseline);
    assert_eq!(
        statfs(&format!(
            "/proc/{}/fd/{}",
            unsafe { libc::getpid() },
            fd.as_raw_fd()
        )),
        baseline
    );
    assert_eq!(
        std::fs::read_link(&path).unwrap().as_os_str(),
        "anon_inode:[pidfd]"
    );
    let mut stat = StatFs::default();
    for suffix in ["/", "/.", "/..", "/child"] {
        let bad = CString::new(format!("{path}{suffix}")).unwrap();
        expect_error(
            unsafe { libc::syscall(libc::SYS_statfs, bad.as_ptr(), &mut stat) },
            libc::ENOTDIR,
        );
    }
    let link = format!("/tmp/pidfd-statfs-{}", unsafe { libc::getpid() });
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert_eq!(statfs(&link), baseline);
    std::fs::remove_file(&link).unwrap();
    let cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir("/proc/self/fd").unwrap();
    assert_eq!(statfs(&fd.as_raw_fd().to_string()), baseline);
    std::env::set_current_dir(cwd).unwrap();
    assert_ne!(
        statfs(&format!("/proc/self/fdinfo/{}", fd.as_raw_fd())).kind as u64,
        PID_FS_MAGIC
    );
    drop(fd);
    let path = CString::new(path).unwrap();
    expect_error(
        unsafe { libc::syscall(libc::SYS_statfs, path.as_ptr(), &mut stat) },
        libc::ENOENT,
    );
}
register_test!(test_pidfd_statfs_paths);

fn test_pidfd_statfs_lifetime_and_fork() {
    unsafe {
        libc::alarm(20);
    }
    let own = pidfd(unsafe { libc::getpid() }, 0);
    let baseline = fstatfs(own.as_raw_fd());
    // A pipe keeps the target alive until its pidfds have been opened.
    let mut pipe = [-1; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let read = own_fd(pipe[0]);
    let write = own_fd(pipe[1]);
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        unsafe {
            libc::alarm(20);
        }
        let same = fstatfs(own.as_raw_fd()) == baseline;
        let mut byte = 0u8;
        let ret = unsafe { libc::read(read.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) };
        unsafe { libc::_exit(if same && ret == 1 { 0 } else { 1 }) };
    }
    let target = pidfd(child, 0);
    let nonblock = pidfd(child, libc::O_NONBLOCK);
    assert_eq!(fstatfs(target.as_raw_fd()), baseline);
    assert_eq!(fstatfs(nonblock.as_raw_fd()), baseline);
    assert_eq!(
        unsafe { libc::write(write.as_raw_fd(), b"x".as_ptr().cast(), 1) },
        1
    );
    wait_child(child);
    // A reaped task must not turn a valid pidfd into ESRCH or change its FS.
    assert_eq!(fstatfs(target.as_raw_fd()), baseline);
    assert_eq!(fstatfs(nonblock.as_raw_fd()), baseline);
    assert_eq!(
        statfs(&format!("/proc/self/fd/{}", target.as_raw_fd())),
        baseline
    );
}
register_test!(test_pidfd_statfs_lifetime_and_fork);

fn test_pidfd_statfs_other_process_fd_table() {
    unsafe {
        libc::alarm(20);
    }
    // The child has fd 200 but the parent does not. /proc/<child>/fd lookup
    // must consult the child's table, not the caller's table.
    let self_fd = pidfd(unsafe { libc::getpid() }, 0);
    let baseline = fstatfs(self_fd.as_raw_fd());
    let mut ready = [-1; 2];
    let mut done = [-1; 2];
    assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0);
    assert_eq!(unsafe { libc::pipe(done.as_mut_ptr()) }, 0);
    let ready_r = own_fd(ready[0]);
    let ready_w = own_fd(ready[1]);
    let done_r = own_fd(done[0]);
    let done_w = own_fd(done[1]);
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        unsafe {
            libc::alarm(20);
        }
        assert_eq!(unsafe { libc::dup3(self_fd.as_raw_fd(), 200, 0) }, 200);
        assert_eq!(
            unsafe { libc::write(ready_w.as_raw_fd(), b"x".as_ptr().cast(), 1) },
            1
        );
        let mut byte = 0u8;
        assert_eq!(
            unsafe { libc::read(done_r.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) },
            1
        );
        unsafe { libc::_exit(0) };
    }
    let mut byte = 0u8;
    assert_eq!(
        unsafe { libc::read(ready_r.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) },
        1
    );
    let mut out = StatFs::default();
    expect_error(
        unsafe { libc::syscall(libc::SYS_fstatfs, 200, &mut out) },
        libc::EBADF,
    );
    assert_eq!(statfs(&format!("/proc/{child}/fd/200")), baseline);
    assert_eq!(
        unsafe { libc::write(done_w.as_raw_fd(), b"x".as_ptr().cast(), 1) },
        1
    );
    wait_child(child);
}
register_test!(test_pidfd_statfs_other_process_fd_table);

fn test_pidfd_statfs_errors() {
    let fd = pidfd(unsafe { libc::getpid() }, 0);
    let mut out = StatFs::default();
    for bad_fd in [-1, i32::MAX] {
        expect_error(
            unsafe { libc::syscall(libc::SYS_fstatfs, bad_fd, &mut out) },
            libc::EBADF,
        );
        expect_error(
            unsafe { libc::syscall(libc::SYS_fstatfs, bad_fd, ptr::null_mut::<StatFs>()) },
            libc::EBADF,
        );
        assert_eq!(out, StatFs::default());
    }
    expect_error(
        unsafe { libc::syscall(libc::SYS_fstatfs, fd.as_raw_fd(), ptr::null_mut::<StatFs>()) },
        libc::EFAULT,
    );
    let path = CString::new(format!("/proc/self/fd/{}", fd.as_raw_fd())).unwrap();
    expect_error(
        unsafe { libc::syscall(libc::SYS_statfs, path.as_ptr(), ptr::null_mut::<StatFs>()) },
        libc::EFAULT,
    );
    expect_error(
        unsafe { libc::syscall(libc::SYS_statfs, ptr::null::<u8>(), &mut out) },
        libc::EFAULT,
    );
    expect_error(
        unsafe { libc::syscall(libc::SYS_statfs, c"".as_ptr(), &mut out) },
        libc::ENOENT,
    );
    let closed = fd.as_raw_fd();
    drop(fd);
    expect_error(
        unsafe { libc::syscall(libc::SYS_fstatfs, closed, &mut out) },
        libc::EBADF,
    );
    // Bad targets must not manufacture pidfs inodes or panic in the kernel.
    for flags in [0, PIDFD_THREAD] {
        expect_error(
            unsafe { libc::syscall(libc::SYS_pidfd_open, i32::MAX, flags) },
            libc::ESRCH,
        );
    }
    for pid in [-1, 0] {
        expect_error(
            unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) },
            libc::EINVAL,
        );
    }
}
register_test!(test_pidfd_statfs_errors);

fn test_pidfd_statfs_faulting_buffers() {
    let fd = pidfd(unsafe { libc::getpid() }, 0);
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    unsafe {
        let mapping = libc::mmap(
            ptr::null_mut(),
            page * 2,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        assert_ne!(mapping, libc::MAP_FAILED);
        let bytes = mapping.cast::<u8>();
        ptr::write_bytes(bytes, 0xa5, page * 2);
        assert_eq!(libc::mprotect(mapping, page, libc::PROT_READ), 0);
        expect_error(
            libc::syscall(libc::SYS_fstatfs, fd.as_raw_fd(), mapping),
            libc::EFAULT,
        );
        assert!(
            std::slice::from_raw_parts(bytes, page)
                .iter()
                .all(|x| *x == 0xa5)
        );
        assert_eq!(
            libc::mprotect(mapping, page, libc::PROT_READ | libc::PROT_WRITE),
            0
        );
        assert_eq!(libc::munmap(bytes.add(page).cast(), page), 0);
        let exact = bytes.add(page - size_of::<StatFs>()).cast::<StatFs>();
        assert_eq!(libc::syscall(libc::SYS_fstatfs, fd.as_raw_fd(), exact), 0);
        check(ptr::read_unaligned(exact));
        expect_error(
            libc::syscall(libc::SYS_fstatfs, fd.as_raw_fd(), bytes.add(page - 1)),
            libc::EFAULT,
        );
        assert_eq!(libc::munmap(mapping, page), 0);
    }
    check(fstatfs(fd.as_raw_fd()));
}
register_test!(test_pidfd_statfs_faulting_buffers);

fn test_pidfd_statfs_threads() {
    unsafe {
        libc::alarm(20);
    }
    let baseline = fstatfs(pidfd(unsafe { libc::getpid() }, 0).as_raw_fd());
    let (tx, rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
        tx.send(tid).unwrap();
        done_rx.recv().unwrap();
    });
    let tid = rx.recv().unwrap();
    expect_error(
        unsafe { libc::syscall(libc::SYS_pidfd_open, tid, 0) },
        libc::ESRCH,
    );
    let fd = pidfd(tid, PIDFD_THREAD);
    assert_eq!(fstatfs(fd.as_raw_fd()), baseline);
    done_tx.send(()).unwrap();
    thread.join().unwrap();
    assert_eq!(fstatfs(fd.as_raw_fd()), baseline);
    // Concurrent opens and statfs calls must all observe one pidfs instance.
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..32 {
                    let other = pidfd(unsafe { libc::getpid() }, 0);
                    assert_eq!(fstatfs(other.as_raw_fd()), baseline);
                    assert_eq!(fstatfs(fd.as_raw_fd()), baseline);
                }
            });
        }
    });
}
register_test!(test_pidfd_statfs_threads);

fn test_pidfd_statvfs_libc() {
    let fd = pidfd(unsafe { libc::getpid() }, 0);
    let raw = fstatfs(fd.as_raw_fd());
    let path = CString::new(format!("/proc/self/fd/{}", fd.as_raw_fd())).unwrap();
    unsafe {
        let mut stat: libc::statvfs = zeroed();
        assert_eq!(libc::fstatvfs(fd.as_raw_fd(), &mut stat), 0);
        let mut by_path: libc::statvfs = zeroed();
        assert_eq!(libc::statvfs(path.as_ptr(), &mut by_path), 0);
        for v in [stat, by_path] {
            assert_eq!(v.f_bsize as i64, raw.bsize);
            assert_eq!(v.f_frsize as i64, raw.frsize);
            assert_eq!((v.f_blocks, v.f_bfree, v.f_bavail), (0, 0, 0));
            assert_eq!((v.f_files, v.f_ffree, v.f_favail), (0, 0, 0));
            assert_eq!(v.f_namemax, 255);
            assert_eq!(v.f_fsid, stat.f_fsid);
            // GNU strips ST_VALID; musl preserves it. The raw statfs ABI
            // is identical and checked separately, independent of libc.
            #[cfg(target_env = "gnu")]
            assert_eq!(v.f_flag, 0);
            #[cfg(target_env = "musl")]
            assert_eq!(v.f_flag, ST_VALID as libc::c_ulong);
        }
    }
}
register_test!(test_pidfd_statvfs_libc);
