//! Path-only descriptors, tested against the guest ABI and guest filesystems.
use crate::register_test;
use std::{
    ffi::CString,
    mem::zeroed,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr,
};

fn fd(raw: i32) -> OwnedFd {
    assert!(raw >= 0, "open: {}", std::io::Error::last_os_error());
    unsafe { OwnedFd::from_raw_fd(raw) }
}

fn open(path: &str, flags: i32) -> OwnedFd {
    let path = CString::new(path).unwrap();
    fd(unsafe { libc::open(path.as_ptr(), flags, 0o600) })
}

#[track_caller]
fn error(result: libc::c_long, expected: i32) {
    assert_eq!(result, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(expected)
    );
}

fn on_fs(name: &str, test: impl Fn(&str)) {
    for base in ["/", "/tmp"] {
        let dir = format!("{base}/opath-{name}-{}", unsafe { libc::getpid() });
        std::fs::create_dir(&dir).unwrap();
        test(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

fn test_opath_flags_and_metadata() {
    assert_eq!(libc::O_DIRECTORY, 0o40000); // Linux AArch64, not the host ABI.
    assert_eq!(libc::O_NOFOLLOW, 0o100000);
    on_fs("metadata", |dir| {
        let path = format!("{dir}/file");
        std::fs::write(&path, b"unchanged").unwrap();
        let file = open(
            &path,
            libc::O_PATH
                | libc::O_CLOEXEC
                | libc::O_RDWR
                | libc::O_TRUNC
                | libc::O_CREAT
                | libc::O_EXCL
                | libc::O_NONBLOCK,
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
        unsafe {
            assert_eq!(libc::fcntl(file.as_raw_fd(), libc::F_GETFL), libc::O_PATH);
            assert_eq!(
                libc::fcntl(file.as_raw_fd(), libc::F_GETFD),
                libc::FD_CLOEXEC
            );
            let mut st: libc::stat = zeroed();
            assert_eq!(libc::fstat(file.as_raw_fd(), &mut st), 0);
            assert_eq!(st.st_size, 9);
            assert_eq!(st.st_mode & libc::S_IFMT, libc::S_IFREG);
            assert_eq!(
                libc::fstatat(file.as_raw_fd(), c"".as_ptr(), &mut st, libc::AT_EMPTY_PATH),
                0
            );
            let mut fs: libc::statfs = zeroed();
            assert_eq!(libc::fstatfs(file.as_raw_fd(), &mut fs), 0);
            let expected = if dir.starts_with("/tmp/") {
                0x0102_1994
            } else {
                0xef53
            };
            assert_eq!(fs.f_type as u64, expected);
            let duplicate = fd(libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 200));
            assert_eq!(duplicate.as_raw_fd(), 200);
            assert_eq!(
                libc::fcntl(duplicate.as_raw_fd(), libc::F_GETFD),
                libc::FD_CLOEXEC
            );
            assert_eq!(libc::fstat(duplicate.as_raw_fd(), &mut st), 0);
            let ordinary_dup = fd(libc::dup(file.as_raw_fd()));
            assert_eq!(libc::fcntl(ordinary_dup.as_raw_fd(), libc::F_GETFD), 0);
            assert!(
                std::fs::read_dir("/proc/self/fd")
                    .unwrap()
                    .any(|e| e.unwrap().file_name() == "200")
            );
            error(
                libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, -1) as _,
                libc::EINVAL,
            );
            error(libc::dup3(file.as_raw_fd(), -1, 0) as _, libc::EBADF);
        }
        let missing = CString::new(format!("{dir}/missing")).unwrap();
        error(
            unsafe { libc::open(missing.as_ptr(), libc::O_PATH | libc::O_CREAT, 0o600) } as _,
            libc::ENOENT,
        );
        assert!(!std::path::Path::new(missing.to_str().unwrap()).exists());
    });
}
register_test!(test_opath_flags_and_metadata);

fn test_opath_rejects_io() {
    on_fs("io", |dir| {
        let path = format!("{dir}/file");
        std::fs::write(&path, b"data").unwrap();
        let file = open(&path, libc::O_PATH);
        let raw = file.as_raw_fd();
        unsafe {
            error(libc::read(raw, ptr::null_mut(), 0) as _, libc::EBADF);
            error(libc::write(raw, ptr::null(), 0) as _, libc::EBADF);
            error(libc::pread(raw, ptr::null_mut(), 0, 0) as _, libc::EBADF);
            error(libc::pwrite(raw, ptr::null(), 0, 0) as _, libc::EBADF);
            error(libc::readv(raw, ptr::null(), 0) as _, libc::EBADF);
            error(libc::writev(raw, ptr::null(), 0) as _, libc::EBADF);
            error(libc::lseek(raw, 0, libc::SEEK_SET) as _, libc::EBADF);
            error(libc::ftruncate(raw, 0) as _, libc::EBADF);
            error(libc::fsync(raw) as _, libc::EBADF);
            error(libc::fdatasync(raw) as _, libc::EBADF);
            error(libc::syncfs(raw) as _, libc::EBADF);
            error(libc::syscall(libc::SYS_fchmod, raw, 0o777), libc::EBADF);
            error(libc::syscall(libc::SYS_fchown, raw, 0, 0), libc::EBADF);
            // musl deliberately retries EBADF through /proc/self/fd; GNU
            // returns the raw fd syscall's error. Check both contracts rather
            // than mistaking libc's pathname fallback for kernel fd access.
            #[cfg(target_env = "musl")]
            {
                assert_eq!(libc::fchmod(raw, 0o666), 0);
                assert_eq!(libc::fchown(raw, 0, 0), 0);
            }
            #[cfg(target_env = "gnu")]
            {
                error(libc::fchmod(raw, 0o777) as _, libc::EBADF);
                error(libc::fchown(raw, 0, 0) as _, libc::EBADF);
            }
            error(
                libc::ioctl(raw, libc::FIONREAD, ptr::null_mut::<i32>()) as _,
                libc::EBADF,
            );
            error(
                libc::fcntl(raw, libc::F_SETFL, libc::O_NONBLOCK) as _,
                libc::EBADF,
            );
            error(
                libc::syscall(libc::SYS_getdents64, raw, ptr::null_mut::<u8>(), 128),
                libc::EBADF,
            );
            let mapping = libc::mmap(
                ptr::null_mut(),
                4096,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                raw,
                0,
            );
            assert_eq!(mapping, libc::MAP_FAILED);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            let mut poll = [
                libc::pollfd {
                    fd: raw,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: -1,
                    events: libc::POLLIN,
                    revents: -1,
                },
            ];
            assert_eq!(libc::poll(poll.as_mut_ptr(), 2, 0), 1);
            assert_eq!(poll[0].revents, libc::POLLNVAL);
            assert_eq!(poll[1].revents, 0);
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"data");
    });
}
register_test!(test_opath_rejects_io);

fn test_opath_symlink_and_dirfd() {
    on_fs("links", |dir| {
        std::fs::write(format!("{dir}/file"), b"data").unwrap();
        std::os::unix::fs::symlink("file", format!("{dir}/link")).unwrap();
        std::os::unix::fs::symlink("missing", format!("{dir}/dangling")).unwrap();
        let directory = open(dir, libc::O_PATH | libc::O_DIRECTORY);
        let link = fd(unsafe {
            libc::openat(
                directory.as_raw_fd(),
                c"link".as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW,
            )
        });
        unsafe {
            let mut st: libc::stat = zeroed();
            assert_eq!(libc::fstat(link.as_raw_fd(), &mut st), 0);
            assert_eq!(st.st_mode & libc::S_IFMT, libc::S_IFLNK);
            let mut target = [0u8; 32];
            assert_eq!(
                libc::readlinkat(
                    link.as_raw_fd(),
                    c"".as_ptr(),
                    target.as_mut_ptr().cast(),
                    target.len()
                ),
                4
            );
            assert_eq!(&target[..4], b"file");
            error(
                libc::openat(directory.as_raw_fd(), c"link".as_ptr(), libc::O_NOFOLLOW) as _,
                libc::ELOOP,
            );
            error(
                libc::openat(
                    directory.as_raw_fd(),
                    c"file".as_ptr(),
                    libc::O_PATH | libc::O_DIRECTORY,
                ) as _,
                libc::ENOTDIR,
            );
            error(
                libc::openat(directory.as_raw_fd(), c"dangling".as_ptr(), libc::O_PATH) as _,
                libc::ENOENT,
            );
            let dangling = fd(libc::openat(
                directory.as_raw_fd(),
                c"dangling".as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW,
            ));
            assert_eq!(libc::fstat(dangling.as_raw_fd(), &mut st), 0);
            error(libc::fchdir(link.as_raw_fd()) as _, libc::ENOTDIR);
            let saved = open("/", libc::O_PATH | libc::O_DIRECTORY);
            assert_eq!(libc::fchdir(directory.as_raw_fd()), 0);
            assert_eq!(std::fs::read("file").unwrap(), b"data");
            assert_eq!(libc::fchdir(saved.as_raw_fd()), 0);
        }
    });
}
register_test!(test_opath_symlink_and_dirfd);

fn test_opath_pidfd_magic_link() {
    unsafe {
        let pid = fd(libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) as i32);
        let path = format!("/proc/self/fd/{}", pid.as_raw_fd());
        let followed = open(&path, libc::O_PATH);
        let link = open(&path, libc::O_PATH | libc::O_NOFOLLOW);
        let mut st: libc::statfs = zeroed();
        assert_eq!(libc::fstatfs(followed.as_raw_fd(), &mut st), 0);
        assert_eq!(st.f_type as u64, 0x5049_4446);
        assert_eq!(libc::fstatfs(link.as_raw_fd(), &mut st), 0);
        assert_eq!(st.f_type as u64, 0x9fa0);
        drop(pid);
        assert_eq!(libc::fstatfs(followed.as_raw_fd(), &mut st), 0);
        assert_eq!(st.f_type as u64, 0x5049_4446);
    }
}
register_test!(test_opath_pidfd_magic_link);
