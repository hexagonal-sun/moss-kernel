//! DAC and path-handle tests run in the AArch64 guest, on ext4 and tmpfs.
use crate::register_test;
use std::{
    ffi::CString,
    mem::zeroed,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::PermissionsExt,
    },
    ptr,
};

fn path(s: &str) -> CString {
    CString::new(s).unwrap()
}
#[track_caller]
fn error(ret: libc::c_long, errno: i32) {
    assert_eq!(ret, -1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(errno));
}
fn open(s: &str, flags: i32) -> OwnedFd {
    let fd = unsafe { libc::open(path(s).as_ptr(), flags, 0o666) };
    assert!(fd >= 0, "open {s}: {}", std::io::Error::last_os_error());
    unsafe { OwnedFd::from_raw_fd(fd) }
}
fn chmod(s: &str, mode: u32) {
    std::fs::set_permissions(s, std::fs::Permissions::from_mode(mode)).unwrap();
}
fn child(f: impl FnOnce()) {
    unsafe {
        let pid = libc::fork();
        assert!(pid >= 0);
        if pid == 0 {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            libc::_exit(if result.is_ok() { 0 } else { 1 });
        }
        let mut status = 0;
        loop {
            let ret = libc::waitpid(pid, &mut status, 0);
            if ret == pid {
                break;
            }
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EINTR)
            );
        }
        assert_eq!(status, 0, "permission child failed");
    }
}
fn drop_ids(uid: u32, groups: &[u32]) {
    unsafe {
        assert_eq!(libc::setgroups(groups.len(), groups.as_ptr()), 0);
        assert_eq!(libc::setgid(uid), 0);
        assert_eq!(libc::setuid(uid), 0);
    }
}
fn on_fs(name: &str, f: impl Fn(&str)) {
    for base in ["/", "/tmp"] {
        let dir = format!("{base}/permissions-{name}-{}", unsafe { libc::getpid() });
        std::fs::create_dir(&dir).unwrap();
        chmod(&dir, 0o755);
        f(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

fn test_permissions_opath_and_fsuid() {
    on_fs("fsuid", |dir| {
        let file = format!("{dir}/file");
        std::fs::write(&file, b"secret").unwrap();
        chmod(&file, 0);
        let blocked = format!("{dir}/blocked");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(format!("{blocked}/file"), b"secret").unwrap();
        chmod(&blocked, 0);
        child(|| unsafe {
            assert_eq!(libc::setfsuid(1000), 0);
            assert_eq!(libc::getuid(), 0);
            assert_eq!(libc::geteuid(), 0);
            assert_eq!(libc::setfsuid(u32::MAX), 1000);
            let handle = open(&file, libc::O_PATH);
            let mut st: libc::stat = zeroed();
            assert_eq!(libc::fstat(handle.as_raw_fd(), &mut st), 0);
            error(
                libc::open(path(&file).as_ptr(), libc::O_RDONLY) as _,
                libc::EACCES,
            );
            error(libc::truncate(path(&file).as_ptr(), 0) as _, libc::EACCES);
            let directory = open(&blocked, libc::O_PATH | libc::O_DIRECTORY);
            error(libc::fchdir(directory.as_raw_fd()) as _, libc::EACCES);
            error(
                libc::openat(directory.as_raw_fd(), c"file".as_ptr(), libc::O_PATH) as _,
                libc::EACCES,
            );
            // access() uses real IDs and permitted capabilities, not fsuid.
            assert_eq!(libc::access(path(&file).as_ptr(), libc::R_OK), 0);
            assert_eq!(libc::setfsuid(0), 1000);
            drop(open(&file, libc::O_RDONLY));
        });
        child(|| unsafe {
            drop_ids(1000, &[]);
            assert_eq!(libc::setfsuid(0), 1000); // failure returns previous fsuid
            assert_eq!(libc::setfsuid(u32::MAX), 1000);
            error(libc::setuid(0) as _, libc::EPERM);
            error(
                libc::open(path(&file).as_ptr(), libc::O_RDONLY) as _,
                libc::EACCES,
            );
            drop(open(&file, libc::O_PATH));
        });
    });
}
register_test!(test_permissions_opath_and_fsuid);

fn test_permissions_groups_owner_and_umask() {
    on_fs("groups", |dir| {
        let shared = format!("{dir}/shared");
        std::fs::create_dir(&shared).unwrap();
        unsafe {
            assert_eq!(libc::chown(path(&shared).as_ptr(), 0, 2000), 0);
        }
        chmod(&shared, 0o2770);
        child(|| unsafe {
            drop_ids(1000, &[2000]);
            assert_eq!(libc::getgroups(0, ptr::null_mut()), 1);
            let mut groups = [0; 2];
            assert_eq!(libc::getgroups(2, groups.as_mut_ptr()), 1);
            assert_eq!(groups[0], 2000);
            error(libc::setgroups(0, ptr::null()) as _, libc::EPERM);
            libc::umask(0o027);
            child(|| {
                let file = open(&format!("{shared}/file"), libc::O_CREAT | libc::O_RDWR);
                let mut st: libc::stat = zeroed();
                assert_eq!(libc::fstat(file.as_raw_fd(), &mut st), 0);
                assert_eq!(
                    (st.st_uid, st.st_gid, st.st_mode & 0o7777),
                    (1000, 2000, 0o640)
                );
                assert_eq!(libc::fchown(file.as_raw_fd(), 1000, 2000), 0);
                error(libc::fchown(file.as_raw_fd(), 1001, 2000) as _, libc::EPERM);
                assert_eq!(libc::fchmod(file.as_raw_fd(), 0o600), 0);
                assert_eq!(
                    libc::mkdir(path(&format!("{shared}/dir")).as_ptr(), 0o777),
                    0
                );
                assert_eq!(
                    libc::stat(path(&format!("{shared}/dir")).as_ptr(), &mut st),
                    0
                );
                assert_eq!(
                    (st.st_uid, st.st_gid, st.st_mode & 0o7777),
                    (1000, 2000, 0o2750)
                );
            });
        });
        child(|| {
            drop_ids(1001, &[]);
            error(
                unsafe { libc::open(path(&shared).as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) }
                    as _,
                libc::EACCES,
            );
        });
    });
}
register_test!(test_permissions_groups_owner_and_umask);

fn test_permissions_mutation_and_sticky() {
    on_fs("sticky", |dir| {
        let shared = format!("{dir}/shared");
        std::fs::create_dir(&shared).unwrap();
        chmod(&shared, 0o1777);
        let victim = format!("{shared}/victim");
        std::fs::write(&victim, b"owned by root").unwrap();
        child(|| unsafe {
            drop_ids(1000, &[]);
            error(
                libc::open(
                    path(&format!("{dir}/denied")).as_ptr(),
                    libc::O_CREAT | libc::O_WRONLY,
                    0o600,
                ) as _,
                libc::EACCES,
            );
            error(
                libc::mkdir(path(&format!("{dir}/denied-dir")).as_ptr(), 0o700) as _,
                libc::EACCES,
            );
            error(libc::unlink(path(&victim).as_ptr()) as _, libc::EPERM);
            error(
                libc::rename(
                    path(&victim).as_ptr(),
                    path(&format!("{shared}/stolen")).as_ptr(),
                ) as _,
                libc::EPERM,
            );
            let mine = format!("{shared}/mine");
            drop(open(&mine, libc::O_CREAT | libc::O_RDWR));
            assert_eq!(libc::unlink(path(&mine).as_ptr()), 0);
            error(libc::chmod(path(&victim).as_ptr(), 0o777) as _, libc::EPERM);
            error(
                libc::mount(
                    ptr::null(),
                    path(&shared).as_ptr(),
                    c"tmpfs".as_ptr(),
                    0,
                    ptr::null(),
                ) as _,
                libc::EPERM,
            );
        });
        chmod(&shared, 0o777);
        child(|| {
            drop_ids(1000, &[]);
            assert_eq!(unsafe { libc::unlink(path(&victim).as_ptr()) }, 0);
        });
    });
}
register_test!(test_permissions_mutation_and_sticky);

fn test_permissions_fd_access_modes() {
    on_fs("fdmode", |dir| {
        let file = format!("{dir}/file");
        std::fs::write(&file, b"data").unwrap();
        let read = open(&file, libc::O_RDONLY);
        let write = open(&file, libc::O_WRONLY);
        unsafe {
            error(
                libc::write(read.as_raw_fd(), ptr::null(), 0) as _,
                libc::EBADF,
            );
            error(
                libc::pwrite(read.as_raw_fd(), ptr::null(), 0, 0) as _,
                libc::EBADF,
            );
            error(libc::ftruncate(read.as_raw_fd(), 0) as _, libc::EINVAL);
            error(
                libc::read(write.as_raw_fd(), ptr::null_mut(), 0) as _,
                libc::EBADF,
            );
            error(
                libc::pread(write.as_raw_fd(), ptr::null_mut(), 0, 0) as _,
                libc::EBADF,
            );
            assert_eq!(
                libc::mmap(
                    ptr::null_mut(),
                    4096,
                    libc::PROT_READ,
                    libc::MAP_PRIVATE,
                    write.as_raw_fd(),
                    0
                ),
                libc::MAP_FAILED
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EACCES)
            );
        }
    });
}
register_test!(test_permissions_fd_access_modes);

fn test_permissions_chroot_resolution() {
    on_fs("chroot", |dir| {
        let jail = format!("{dir}/jail");
        std::fs::create_dir(&jail).unwrap();
        std::fs::write(format!("{jail}/visible"), b"inside").unwrap();
        std::os::unix::fs::symlink("/visible", format!("{jail}/link")).unwrap();
        child(|| unsafe {
            assert_eq!(libc::chroot(path(&jail).as_ptr()), 0);
            assert_eq!(libc::chdir(c"/".as_ptr()), 0);
            assert_eq!(std::fs::read("/link").unwrap(), b"inside");
            assert_eq!(std::fs::read("/../../visible").unwrap(), b"inside");
            error(
                libc::open(c"/bin/sh".as_ptr(), libc::O_PATH) as _,
                libc::ENOENT,
            );
        });
    });
}
register_test!(test_permissions_chroot_resolution);

fn test_permissions_proc_fd_credentials() {
    let parent = unsafe { libc::getpid() };
    let file = open("/bin/sh", libc::O_PATH);
    let target = format!("/proc/{parent}/fd/{}", file.as_raw_fd());
    // Hold a nofollow handle first: readlinkat must still recheck credentials.
    let link = open(&target, libc::O_PATH | libc::O_NOFOLLOW);
    child(|| unsafe {
        drop_ids(1000, &[]);
        let own = format!("/proc/self/fd/{}", file.as_raw_fd());
        drop(open(&own, libc::O_PATH)); // same thread group exception, dumpable=0
        error(
            libc::open(path(&target).as_ptr(), libc::O_PATH) as _,
            libc::EACCES,
        );
        let mut buf = [0u8; 256];
        error(
            libc::readlinkat(
                link.as_raw_fd(),
                c"".as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            ) as _,
            libc::EACCES,
        );
    });
}
register_test!(test_permissions_proc_fd_credentials);

fn test_permissions_namespace_requests_do_not_fake_success() {
    // Reject unimplemented mount options before any path copying or mount
    // mutation. POSIXACL (1 << 16) must not alias MS_SILENT (1 << 15).
    for flags in [
        libc::MS_MOVE,
        libc::MS_SYNCHRONOUS,
        libc::MS_DIRSYNC,
        libc::MS_REC,
        1 << 16,
    ] {
        error(
            unsafe { libc::mount(ptr::null(), ptr::null(), ptr::null(), flags, ptr::null()) } as _,
            libc::ENOSYS,
        );
    }
    for flag in [
        libc::CLONE_NEWNET,
        libc::CLONE_NEWUTS,
        libc::CLONE_NEWIPC,
        libc::CLONE_NEWCGROUP,
    ] {
        let result = unsafe { libc::syscall(libc::SYS_clone, flag | libc::SIGCHLD, 0, 0, 0, 0) };
        if result == 0 {
            unsafe {
                libc::_exit(99);
            }
        }
        if result > 0 {
            let mut status = 0;
            unsafe {
                libc::waitpid(result as _, &mut status, 0);
            }
            panic!("unsupported namespace created an unisolated child");
        }
        error(result, libc::EINVAL);
    }
}
register_test!(test_permissions_namespace_requests_do_not_fake_success);

fn test_permissions_real_effective_access_and_exec() {
    on_fs("access", |dir| {
        let file = format!("{dir}/file");
        std::fs::write(&file, b"not an executable").unwrap();
        chmod(&file, 0o600);
        child(|| unsafe {
            // Keep effective root but change the real UID. access() must not
            // borrow effective-root DAC capabilities for either path or file.
            assert_eq!(libc::setresuid(1000, u32::MAX, u32::MAX), 0);
            error(
                libc::access(path(&file).as_ptr(), libc::R_OK) as _,
                libc::EACCES,
            );
            assert_eq!(
                libc::syscall(
                    libc::SYS_faccessat2,
                    libc::AT_FDCWD,
                    path(&file).as_ptr(),
                    libc::R_OK,
                    libc::AT_EACCESS
                ),
                0
            );
            drop(open(&file, libc::O_RDONLY));
            let mut real = 0;
            let mut effective = 0;
            let mut saved = 0;
            assert_eq!(libc::getresuid(&mut real, &mut effective, &mut saved), 0);
            assert_eq!((real, effective, saved), (1000, 0, 0));
            error(
                libc::syscall(
                    libc::SYS_faccessat2,
                    libc::AT_FDCWD,
                    path(&file).as_ptr(),
                    8,
                    0,
                ),
                libc::EINVAL,
            );
            let argv = [c"file".as_ptr(), ptr::null()];
            let envp = [ptr::null::<libc::c_char>()];
            error(
                libc::execve(path(&file).as_ptr(), argv.as_ptr(), envp.as_ptr()) as _,
                libc::EACCES,
            );
        });
    });
}
register_test!(test_permissions_real_effective_access_and_exec);

#[repr(C)]
struct CapHeader {
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

fn test_permissions_capabilities_and_proc_dumpable() {
    let parent = unsafe { libc::getpid() };
    let file = open("/bin/sh", libc::O_PATH);
    let target = format!("/proc/{parent}/fd/{}", file.as_raw_fd());
    child(|| unsafe {
        let header = CapHeader {
            version: 0x20080522,
            pid: 0,
        };
        let mut caps = [CapData::default(); 2];
        assert_eq!(
            libc::syscall(libc::SYS_capget, &header, caps.as_mut_ptr()),
            0
        );
        let all = caps;
        for cap in &mut caps {
            cap.effective = 0;
        }
        assert_eq!(libc::syscall(libc::SYS_capset, &header, caps.as_ptr()), 0);
        // Same UID and DAC allow this path; the commoncap subset check must
        // still reject a target with permitted capabilities we lack effectively.
        error(
            libc::open(path(&target).as_ptr(), libc::O_PATH) as _,
            libc::EACCES,
        );
        assert_eq!(libc::syscall(libc::SYS_capset, &header, all.as_ptr()), 0);
        drop(open(&target, libc::O_PATH));
    });
    unsafe {
        assert_eq!(libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0), 0);
    }
    child(|| unsafe {
        let header = CapHeader {
            version: 0x20080522,
            pid: 0,
        };
        let mut caps = [CapData::default(); 2];
        assert_eq!(
            libc::syscall(libc::SYS_capget, &header, caps.as_mut_ptr()),
            0
        );
        caps[0].effective &= !(1 << 19); // CAP_SYS_PTRACE only
        assert_eq!(libc::syscall(libc::SYS_capset, &header, caps.as_ptr()), 0);
        error(
            libc::open(path(&target).as_ptr(), libc::O_PATH) as _,
            libc::EACCES,
        );
        assert_eq!(libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0), 0);
        error(
            libc::prctl(libc::PR_SET_DUMPABLE, 2, 0, 0, 0) as _,
            libc::EINVAL,
        );
        let foreign = CapHeader {
            version: 0x20080522,
            pid: parent,
        };
        error(
            libc::syscall(libc::SYS_capset, &foreign, caps.as_ptr()),
            libc::EPERM,
        );
        error(
            libc::prctl(libc::PR_CAPBSET_READ, 64, 0, 0, 0) as _,
            libc::EINVAL,
        );
    });
}
register_test!(test_permissions_capabilities_and_proc_dumpable);

fn test_permissions_opath_empty_path_and_mount_parent() {
    on_fs("empty", |dir| {
        let file = format!("{dir}/file");
        std::fs::write(&file, b"data").unwrap();
        let handle = open(&file, libc::O_PATH);
        unsafe {
            // AArch64 fchmodat2: fchmod must reject O_PATH, but EMPTY_PATH
            // operates on its inode and performs the ordinary ownership check.
            assert_eq!(
                libc::syscall(
                    452,
                    handle.as_raw_fd(),
                    c"".as_ptr(),
                    0o640,
                    libc::AT_EMPTY_PATH
                ),
                0
            );
            let mut st: libc::stat = zeroed();
            assert_eq!(libc::fstat(handle.as_raw_fd(), &mut st), 0);
            assert_eq!(st.st_mode & 0o7777, 0o640);
            let linked = format!("{dir}/linked");
            assert_eq!(
                libc::linkat(
                    handle.as_raw_fd(),
                    c"".as_ptr(),
                    libc::AT_FDCWD,
                    path(&linked).as_ptr(),
                    libc::AT_EMPTY_PATH
                ),
                0
            );
            assert_eq!(libc::stat(path(&linked).as_ptr(), &mut st), 0);
            assert_eq!(st.st_nlink, 2);
        }
    });
    unsafe {
        let root = open("/", libc::O_PATH);
        let up = open("/tmp/..", libc::O_PATH);
        let mut a: libc::stat = zeroed();
        let mut b: libc::stat = zeroed();
        assert_eq!(libc::fstat(root.as_raw_fd(), &mut a), 0);
        assert_eq!(libc::fstat(up.as_raw_fd(), &mut b), 0);
        assert_eq!((a.st_dev, a.st_ino), (b.st_dev, b.st_ino));
    }
}
register_test!(test_permissions_opath_empty_path_and_mount_parent);
