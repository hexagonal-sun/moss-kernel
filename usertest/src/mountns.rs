//! Run on the MOSS guest: these tests deliberately exercise its syscall ABI.
use crate::register_test;
use std::{
    ffi::CString,
    io::{Read, Seek, SeekFrom},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::MetadataExt,
    },
    ptr,
};

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}
#[track_caller]
fn ok(ret: i32) {
    assert_eq!(ret, 0, "{}", std::io::Error::last_os_error());
}
#[track_caller]
fn error(ret: libc::c_long, errno: i32) {
    assert_eq!(ret, -1, "expected errno {errno}");
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(errno));
}
fn open(path: &str, flags: i32) -> OwnedFd {
    let fd = unsafe { libc::open(c(path).as_ptr(), flags, 0o600) };
    assert!(fd >= 0, "open {path}: {}", std::io::Error::last_os_error());
    unsafe { OwnedFd::from_raw_fd(fd) }
}
fn ns() -> std::path::PathBuf {
    std::fs::read_link("/proc/self/ns/mnt").unwrap()
}
fn unshare() {
    ok(unsafe { libc::unshare(libc::CLONE_NEWNS) });
}
fn mount(target: &str, fs: &str) {
    ok(unsafe {
        libc::mount(
            c"none".as_ptr(),
            c(target).as_ptr(),
            c(fs).as_ptr(),
            0,
            ptr::null(),
        )
    });
}
fn bind(source: &str, target: &str) {
    ok(unsafe {
        libc::mount(
            c(source).as_ptr(),
            c(target).as_ptr(),
            ptr::null(),
            libc::MS_BIND,
            ptr::null(),
        )
    });
}
fn umount(target: &str, flags: i32) {
    ok(unsafe { libc::umount2(c(target).as_ptr(), flags) });
}
fn statx(path: &str) -> [u64; 32] {
    let mut guarded = [u64::MAX; 34];
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_statx,
                libc::AT_FDCWD,
                c(path).as_ptr(),
                0,
                0x17ff,
                guarded.as_mut_ptr().add(1),
            )
        },
        0
    );
    assert_eq!(guarded[0], u64::MAX);
    assert_eq!(guarded[33], u64::MAX);
    let raw: [u64; 32] = guarded[1..33].try_into().unwrap();
    let dev = std::fs::metadata(path).unwrap().dev();
    assert_eq!(raw[17] as u32, libc::major(dev) as u32);
    assert_eq!((raw[17] >> 32) as u32, libc::minor(dev) as u32);
    raw
}
fn mount_id(path: &str) -> u64 {
    let raw = statx(path);
    assert_ne!(raw[0] & 0x1000, 0);
    raw[18]
}
struct Fixture(String);
impl Fixture {
    fn new(name: &str) -> Self {
        let dir = format!("/tmp/mountns-{name}-{}", unsafe {
            libc::syscall(libc::SYS_getpid)
        });
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn sub(&self, name: &str) -> String {
        let p = format!("{}/{name}", self.0);
        std::fs::create_dir(&p).unwrap();
        p
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Child {
    pid: i32,
    waited: bool,
}
impl Child {
    fn wait(&mut self) -> i32 {
        let mut status = 0;
        loop {
            let ret = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if ret == self.pid {
                break;
            }
            error(ret as _, libc::EINTR);
        }
        self.waited = true;
        status
    }
    fn join(mut self) {
        assert_eq!(self.wait(), 0, "mountns child {} failed", self.pid);
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
        ok(unsafe { libc::pipe(fds.as_mut_ptr()) });
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
            let mut pfd = libc::pollfd {
                fd: self.read.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut pfd, 1, 10000) };
            if ready == -1 {
                error(ready as _, libc::EINTR);
                continue;
            }
            assert_eq!(ready, 1, "child readiness timed out");
            let ret =
                unsafe { libc::read(self.read.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) };
            if ret == 1 {
                break;
            }
            error(ret as _, libc::EINTR);
        }
    }
}

fn test_mountns_private_clone_and_unshare() {
    let fixture = Fixture::new("private");
    std::fs::write(format!("{}/base", fixture.0), b"underlying").unwrap();
    let old = open(&fixture.0, libc::O_PATH | libc::O_DIRECTORY);
    let original = ns();
    for flags in [libc::CLONE_NEWNS, 0] {
        spawn(flags, || {
            if flags == 0 {
                unshare();
            }
            assert_ne!(ns(), original);
            // An executable inherited from the old namespace still has a name.
            assert!(
                std::env::current_exe()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("usertest")
            );
            mount(&fixture.0, "tmpfs");
            assert!(!std::path::Path::new(&format!("{}/base", fixture.0)).exists());
            std::fs::write(format!("{}/private", fixture.0), b"private").unwrap();
            let old_base = format!("/proc/self/fd/{}/base", old.as_raw_fd());
            assert_eq!(std::fs::read(old_base).unwrap(), b"underlying");
            let fd = unsafe { libc::openat(old.as_raw_fd(), c"base".as_ptr(), libc::O_RDONLY) };
            assert!(fd >= 0);
            unsafe {
                libc::close(fd);
            }
            assert!(
                std::process::Command::new("/bin/true")
                    .status()
                    .unwrap()
                    .success()
            );
        })
        .join();
        assert_eq!(ns(), original);
        assert_eq!(
            std::fs::read(format!("{}/base", fixture.0)).unwrap(),
            b"underlying"
        );
        assert!(!std::path::Path::new(&format!("{}/private", fixture.0)).exists());
    }
}
register_test!(test_mountns_private_clone_and_unshare);

fn test_mountns_overmount_keeps_cwd_and_dirfd_view() {
    let fixture = Fixture::new("overmount");
    std::fs::write(format!("{}/base", fixture.0), b"underlying").unwrap();
    spawn(libc::CLONE_NEWNS, || {
        std::env::set_current_dir(&fixture.0).unwrap();
        let fd = open(".", libc::O_PATH | libc::O_DIRECTORY);
        mount(&fixture.0, "tmpfs");
        assert_eq!(std::fs::read("base").unwrap(), b"underlying");
        assert!(!std::path::Path::new(&format!("{}/base", fixture.0)).exists());
        assert_eq!(
            std::fs::read(format!("/proc/self/fd/{}/base", fd.as_raw_fd())).unwrap(),
            b"underlying"
        );
        std::fs::write("created", b"cwd").unwrap();
        std::fs::rename("created", "renamed").unwrap();
        std::fs::hard_link("renamed", "hard").unwrap();
        std::fs::create_dir("dir").unwrap();
        std::os::unix::fs::symlink("renamed", "link").unwrap();
        assert_eq!(std::fs::read("link").unwrap(), b"cwd");
        assert!(!std::path::Path::new(&format!("{}/renamed", fixture.0)).exists());
        std::fs::remove_file("hard").unwrap();
        std::fs::remove_dir("dir").unwrap();
        // mount/umount deliberately operate on the top mount, unlike openat.
        let first = mount_id(&fixture.0);
        mount(".", "tmpfs");
        assert_ne!(mount_id(&fixture.0), first);
        umount(".", 0);
        assert_eq!(mount_id(&fixture.0), first);
        umount(&fixture.0, 0);
        assert_eq!(
            std::fs::read(format!("{}/renamed", fixture.0)).unwrap(),
            b"cwd"
        );
    })
    .join();
}
register_test!(test_mountns_overmount_keeps_cwd_and_dirfd_view);

fn test_mountns_fork_shares_mounts() {
    // Isolate the test itself, then prove an ordinary fork shares this view.
    let fixture = Fixture::new("shared");
    spawn(libc::CLONE_NEWNS, || {
        let original = ns();
        spawn(0, || {
            assert_eq!(ns(), original);
            mount(&fixture.0, "tmpfs");
            std::fs::write(format!("{}/child", fixture.0), b"shared").unwrap();
        })
        .join();
        assert_eq!(
            std::fs::read(format!("{}/child", fixture.0)).unwrap(),
            b"shared"
        );
        umount(&fixture.0, 0);
        assert!(!std::path::Path::new(&format!("{}/child", fixture.0)).exists());
    })
    .join();
}
register_test!(test_mountns_fork_shares_mounts);

fn test_mountns_bind_identity_and_dotdot() {
    let fixture = Fixture::new("bind");
    let source = fixture.sub("source");
    let alias = fixture.sub("alias");
    let nested = fixture.sub("nested");
    spawn(libc::CLONE_NEWNS, || {
        mount(&source, "tmpfs");
        std::fs::create_dir(format!("{source}/sub")).unwrap();
        std::fs::write(format!("{source}/sub/file"), b"same inode").unwrap();
        bind(&source, &alias);
        bind(&format!("{source}/sub"), &nested);
        let a = std::fs::metadata(format!("{source}/sub/file")).unwrap();
        let b = std::fs::metadata(format!("{nested}/file")).unwrap();
        assert_eq!((a.dev(), a.ino()), (b.dev(), b.ino()));
        assert_ne!(mount_id(&source), mount_id(&alias));
        assert_ne!(mount_id(&nested), mount_id(&alias));
        assert_ne!(statx(&nested)[1] & 0x2000, 0);
        assert_eq!(statx(&format!("{source}/sub"))[1] & 0x2000, 0);
        std::fs::write(format!("{nested}/file"), b"changed").unwrap();
        assert_eq!(
            std::fs::read(format!("{alias}/sub/file")).unwrap(),
            b"changed"
        );
        std::env::set_current_dir(&nested).unwrap();
        assert_eq!(std::env::current_dir().unwrap().to_str().unwrap(), nested);
        std::env::set_current_dir("..").unwrap();
        assert_eq!(
            std::env::current_dir().unwrap().to_str().unwrap(),
            fixture.0
        );
        // File bind mounts retain file type and filesystem identity too.
        let target = format!("{}/file-target", fixture.0);
        std::fs::write(&target, b"old").unwrap();
        bind(&format!("{source}/sub/file"), &target);
        assert_eq!(std::fs::read(&target).unwrap(), b"changed");
        assert_ne!(mount_id(&target), mount_id(&source));
        umount(&target, 0);
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
    })
    .join();
}
register_test!(test_mountns_bind_identity_and_dotdot);

fn test_mountns_same_superblock_multiple_mounts() {
    let fixture = Fixture::new("proc-alias");
    let a = fixture.sub("a");
    let b = fixture.sub("b");
    spawn(libc::CLONE_NEWNS, || {
        mount(&a, "proc");
        mount(&b, "proc");
        let ma = std::fs::metadata(&a).unwrap();
        let mb = std::fs::metadata(&b).unwrap();
        assert_eq!((ma.dev(), ma.ino()), (mb.dev(), mb.ino()));
        assert_ne!(mount_id(&a), mount_id(&b));
        for target in [&a, &b] {
            assert_eq!(
                std::fs::read_link(format!("{target}/self"))
                    .unwrap()
                    .to_str()
                    .unwrap(),
                unsafe { libc::syscall(libc::SYS_getpid) }.to_string()
            );
            std::env::set_current_dir(target).unwrap();
            std::env::set_current_dir("..").unwrap();
            assert_eq!(
                std::env::current_dir().unwrap().to_str().unwrap(),
                fixture.0
            );
        }
        umount(&a, 0);
        assert!(std::path::Path::new(&format!("{b}/self/status")).exists());
    })
    .join();
}
register_test!(test_mountns_same_superblock_multiple_mounts);

fn test_mountns_stack_busy_lazy_and_mmap() {
    let fixture = Fixture::new("lifetime");
    std::fs::write(format!("{}/base", fixture.0), b"base").unwrap();
    spawn(libc::CLONE_NEWNS, || {
        mount(&fixture.0, "tmpfs");
        let name = format!("{}/kept", fixture.0);
        std::fs::write(&name, vec![42u8; 4096]).unwrap();
        let file = open(&name, libc::O_RDONLY);
        let directory = open(&fixture.0, libc::O_PATH | libc::O_DIRECTORY);
        let mapping = unsafe {
            libc::mmap(
                ptr::null_mut(),
                4096,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        mount(&fixture.0, "tmpfs");
        assert!(!std::path::Path::new(&name).exists());
        umount(&fixture.0, 0);
        assert!(std::path::Path::new(&name).exists());
        error(
            unsafe { libc::umount2(c(&fixture.0).as_ptr(), 0) } as _,
            libc::EBUSY,
        );
        umount(&fixture.0, libc::MNT_DETACH);
        assert_eq!(
            std::fs::read(format!("{}/base", fixture.0)).unwrap(),
            b"base"
        );
        let mut buf = [0u8; 1];
        assert_eq!(
            unsafe { libc::read(file.as_raw_fd(), buf.as_mut_ptr().cast(), 1) },
            1
        );
        assert_eq!(buf[0], 42);
        let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
        ok(unsafe { libc::fstatfs(file.as_raw_fd(), &mut stats) });
        assert_eq!(stats.f_type as u64, 0x01021994);
        assert_eq!(
            std::fs::read(format!("/proc/self/fd/{}/kept", directory.as_raw_fd()))
                .unwrap()
                .len(),
            4096
        );
        ok(unsafe { libc::fchdir(directory.as_raw_fd()) });
        assert_eq!(
            std::env::current_dir().unwrap_err().raw_os_error(),
            Some(libc::ENOENT)
        );
        std::env::set_current_dir("..").unwrap();
        assert_eq!(std::fs::read("kept").unwrap().len(), 4096);
        std::env::set_current_dir("/").unwrap();
        drop(directory);
        drop(file);
        // First access only after every fd and cwd reference has been dropped.
        assert_eq!(unsafe { mapping.cast::<u8>().read_volatile() }, 42);
        ok(unsafe { libc::munmap(mapping, 4096) });
    })
    .join();
}
register_test!(test_mountns_stack_busy_lazy_and_mmap);

fn test_mountns_live_paths_rename_and_chroot() {
    let fixture = Fixture::new("paths");
    let original = fixture.sub("original");
    std::fs::create_dir(format!("{original}/sub")).unwrap();
    std::os::unix::fs::symlink("original/sub", format!("{}/link", fixture.0)).unwrap();
    spawn(libc::CLONE_NEWNS, || {
        let fd = open(&format!("{original}/sub"), libc::O_PATH | libc::O_DIRECTORY);
        std::env::set_current_dir(format!("{}/link", fixture.0)).unwrap();
        assert_eq!(
            std::env::current_dir().unwrap().to_str().unwrap(),
            format!("{original}/sub")
        );
        let moved = format!("{}/moved", fixture.0);
        std::fs::rename(&original, &moved).unwrap();
        let expected = format!("{moved}/sub");
        let mut tiny = [0u8; 1];
        error(
            unsafe { libc::syscall(libc::SYS_getcwd, tiny.as_mut_ptr(), 1) },
            libc::ERANGE,
        );
        assert_eq!(std::env::current_dir().unwrap().to_str().unwrap(), expected);
        assert_eq!(
            std::fs::read_link("/proc/self/cwd")
                .unwrap()
                .to_str()
                .unwrap(),
            expected
        );
        assert_eq!(
            std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))
                .unwrap()
                .to_str()
                .unwrap(),
            expected
        );
        std::env::set_current_dir("/").unwrap();
        ok(unsafe { libc::fchdir(fd.as_raw_fd()) });
        ok(unsafe { libc::chroot(c(&fixture.0).as_ptr()) });
        assert_eq!(
            std::env::current_dir().unwrap().to_str().unwrap(),
            "/moved/sub"
        );
        std::env::set_current_dir("../../../../..").unwrap();
        assert_eq!(std::env::current_dir().unwrap().to_str().unwrap(), "/");
    })
    .join();
}
register_test!(test_mountns_live_paths_rename_and_chroot);

fn test_mountns_setns_lifetime_and_owner() {
    let fixture = Fixture::new("setns");
    spawn(libc::CLONE_NEWNS, || {
        let original = open("/proc/self/ns/mnt", libc::O_RDONLY);
        let original_id = ns();
        assert_eq!(
            std::fs::read_link(format!("/proc/self/fd/{}", original.as_raw_fd())).unwrap(),
            original_id
        );
        let ready = Pipe::new();
        let release = Pipe::new();
        let child = spawn(libc::CLONE_NEWNS, || {
            mount(&fixture.0, "tmpfs");
            std::fs::write(format!("{}/child", fixture.0), b"retained").unwrap();
            ready.send();
            release.recv();
        });
        ready.recv();
        let target = open(&format!("/proc/{}/ns/mnt", child.pid), libc::O_RDONLY);
        let mut mounts = std::fs::File::open(format!("/proc/{}/mountinfo", child.pid)).unwrap();
        release.send();
        child.join();
        assert_eq!(
            unsafe { libc::ioctl(target.as_raw_fd(), 0xb703) },
            libc::CLONE_NEWNS
        );
        error(
            unsafe { libc::ioctl(target.as_raw_fd(), 0xb702) } as _,
            libc::EINVAL,
        );
        error(
            unsafe { libc::ioctl(target.as_raw_fd(), 0xb704, ptr::null_mut::<u32>()) } as _,
            libc::EINVAL,
        );
        let owner = unsafe { libc::ioctl(target.as_raw_fd(), 0xb701) };
        assert!(owner >= 0);
        let owner = unsafe { OwnedFd::from_raw_fd(owner) };
        assert_eq!(
            unsafe { libc::ioctl(owner.as_raw_fd(), 0xb703) },
            libc::CLONE_NEWUSER
        );
        assert_ne!(
            unsafe { libc::fcntl(owner.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        error(
            unsafe { libc::setns(target.as_raw_fd(), libc::CLONE_NEWUSER) } as _,
            libc::EINVAL,
        );
        let opath = open("/proc/self/ns/mnt", libc::O_PATH);
        error(
            unsafe { libc::setns(opath.as_raw_fd(), libc::CLONE_NEWNS) } as _,
            libc::EBADF,
        );
        std::env::set_current_dir("/tmp").unwrap();
        ok(unsafe { libc::setns(target.as_raw_fd(), 0) });
        assert_ne!(ns(), original_id);
        assert_eq!(std::env::current_dir().unwrap().to_str().unwrap(), "/");
        assert_eq!(
            std::fs::read(format!("{}/child", fixture.0)).unwrap(),
            b"retained"
        );
        let mut text = String::new();
        mounts.read_to_string(&mut text).unwrap();
        assert!(text.contains(&fixture.0));
        ok(unsafe { libc::setns(original.as_raw_fd(), libc::CLONE_NEWNS) });
        assert_eq!(ns(), original_id);
        assert!(!std::path::Path::new(&format!("{}/child", fixture.0)).exists());
    })
    .join();
}
register_test!(test_mountns_setns_lifetime_and_owner);

fn test_mountns_userns_authority_and_locked_mounts() {
    let fixture = Fixture::new("userns");
    ok(unsafe { libc::chown(c(&fixture.0).as_ptr(), 1000, 1000) });
    spawn(0, || {
        let initial = open("/proc/self/ns/mnt", libc::O_RDONLY);
        unsafe {
            ok(libc::setgroups(0, ptr::null()));
            ok(libc::setgid(1000));
            ok(libc::setuid(1000));
            ok(libc::prctl(libc::PR_SET_DUMPABLE, 1, 0, 0, 0));
        }
        error(
            unsafe { libc::unshare(libc::CLONE_NEWNS) } as _,
            libc::EPERM,
        );
        ok(unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) });
        std::fs::write("/proc/self/uid_map", b"0 1000 1\n").unwrap();
        std::fs::write("/proc/self/setgroups", b"deny\n").unwrap();
        std::fs::write("/proc/self/gid_map", b"0 1000 1\n").unwrap();
        mount(&fixture.0, "tmpfs");
        assert_eq!(std::fs::metadata(&fixture.0).unwrap().uid(), 0);
        std::fs::write(format!("{}/owned", fixture.0), b"owned").unwrap();
        error(
            unsafe { libc::umount2(c"/tmp".as_ptr(), libc::MNT_DETACH) } as _,
            libc::EINVAL,
        );
        error(
            unsafe {
                libc::mount(
                    c"/".as_ptr(),
                    c(&fixture.0).as_ptr(),
                    ptr::null(),
                    libc::MS_BIND,
                    ptr::null(),
                )
            } as _,
            libc::EINVAL,
        );
        error(
            unsafe {
                libc::mount(
                    c"none".as_ptr(),
                    c(&fixture.0).as_ptr(),
                    c"proc".as_ptr(),
                    0,
                    ptr::null(),
                )
            } as _,
            libc::EPERM,
        );
        error(
            unsafe { libc::setns(initial.as_raw_fd(), libc::CLONE_NEWNS) } as _,
            libc::EPERM,
        );
        umount(&fixture.0, 0);
    })
    .join();
    assert!(!std::path::Path::new(&format!("{}/owned", fixture.0)).exists());
}
register_test!(test_mountns_userns_authority_and_locked_mounts);

fn test_mountns_rejects_unsupported_atomically() {
    let fixture = Fixture::new("reject");
    let old = open(&fixture.0, libc::O_PATH | libc::O_DIRECTORY);
    spawn(0, || {
        let initial = ns();
        error(
            unsafe { libc::unshare(libc::CLONE_NEWNS | libc::CLONE_NEWNET) } as _,
            libc::EINVAL,
        );
        assert_eq!(ns(), initial);
        error(
            unsafe {
                libc::syscall(
                    libc::SYS_clone,
                    libc::CLONE_NEWNS | libc::CLONE_FS | libc::SIGCHLD,
                    0,
                    0,
                    0,
                    0,
                )
            },
            libc::EINVAL,
        );
        unshare();
        let id = mount_id(&fixture.0);
        error(
            unsafe {
                libc::mount(
                    c"none".as_ptr(),
                    c(&fixture.0).as_ptr(),
                    c"missing-filesystem".as_ptr(),
                    0,
                    ptr::null(),
                )
            } as _,
            libc::ENODEV,
        );
        for flags in [
            libc::MS_MOVE,
            libc::MS_SYNCHRONOUS,
            libc::MS_DIRSYNC,
            libc::MS_REMOUNT | libc::MS_REC,
            libc::MS_BIND | libc::MS_MOVE,
        ] {
            error(
                unsafe {
                    libc::mount(
                        c(&fixture.0).as_ptr(),
                        c(&fixture.0).as_ptr(),
                        c"tmpfs".as_ptr(),
                        flags,
                        ptr::null(),
                    )
                } as _,
                libc::ENOSYS,
            );
            assert_eq!(mount_id(&fixture.0), id);
        }
        error(
            unsafe {
                libc::mount(
                    c"none".as_ptr(),
                    c(&format!("/proc/self/fd/{}", old.as_raw_fd())).as_ptr(),
                    c"tmpfs".as_ptr(),
                    0,
                    ptr::null(),
                )
            } as _,
            libc::EINVAL,
        );
        mount(&fixture.0, "tmpfs");
        let sub = format!("{}/sub", fixture.0);
        std::fs::create_dir(&sub).unwrap();
        mount(&sub, "tmpfs");
        let outer = mount_id(&fixture.0);
        let inner = mount_id(&sub);
        error(
            unsafe { libc::umount2(c(&fixture.0).as_ptr(), 0) } as _,
            libc::EBUSY,
        );
        error(
            unsafe { libc::umount2(c(&fixture.0).as_ptr(), libc::MNT_DETACH) } as _,
            libc::ENOSYS,
        );
        assert_eq!(mount_id(&fixture.0), outer);
        assert_eq!(mount_id(&sub), inner);
        umount(&sub, 0);
        umount(&fixture.0, 0);
    })
    .join();
}
register_test!(test_mountns_rejects_unsupported_atomically);

fn test_mountns_shared_fs_detachment() {
    let fixture = Fixture::new("fs-context");
    spawn(libc::CLONE_NEWNS, || {
        std::env::set_current_dir(&fixture.0).unwrap();
        let fd = open("/proc/self/ns/mnt", libc::O_RDONLY);
        let original = ns();
        let ready = Pipe::new();
        let release = Pipe::new();
        let child = spawn(libc::CLONE_FS, || {
            error(
                unsafe { libc::setns(fd.as_raw_fd(), libc::CLONE_NEWNS) } as _,
                libc::EINVAL,
            );
            unshare();
            std::env::set_current_dir("/").unwrap();
            unsafe {
                libc::umask(0o077);
            }
            ready.send();
            release.recv();
        });
        ready.recv();
        assert_eq!(ns(), original);
        assert_eq!(
            std::env::current_dir().unwrap().to_str().unwrap(),
            fixture.0
        );
        release.send();
        child.join();
    })
    .join();
}
register_test!(test_mountns_shared_fs_detachment);

fn test_mountns_mountinfo_and_cross_mount_operations() {
    let fixture = Fixture::new("mountinfo");
    let source = fixture.sub("source");
    let alias = fixture.sub("with space");
    spawn(0, || {
        let mut old = std::fs::File::open("/proc/self/mountinfo").unwrap();
        unshare();
        mount(&source, "tmpfs");
        bind(&source, &alias);
        let id = mount_id(&alias);
        let text = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
        let line = text
            .lines()
            .find(|line| line.starts_with(&format!("{id} ")))
            .unwrap();
        assert!(line.contains(&alias.replace(' ', "\\040")));
        assert!(line.contains(" - tmpfs "));
        assert_eq!(
            std::fs::read_to_string("/proc/mounts").unwrap(),
            std::fs::read_to_string("/proc/self/mounts").unwrap()
        );
        let mut before = String::new();
        old.read_to_string(&mut before).unwrap();
        assert!(!before.contains("\\040"));
        old.seek(SeekFrom::Start(0)).unwrap();
        let mut again = String::new();
        old.read_to_string(&mut again).unwrap();
        assert_eq!(before, again);
        let from = format!("{source}/file");
        let to = format!("{alias}/to");
        std::fs::write(&from, b"data").unwrap();
        error(
            unsafe { libc::link(c(&from).as_ptr(), c(&to).as_ptr()) } as _,
            libc::EXDEV,
        );
        error(
            unsafe { libc::rename(c(&from).as_ptr(), c(&to).as_ptr()) } as _,
            libc::EXDEV,
        );
        let hard = format!("{source}/hard");
        std::fs::hard_link(&from, &hard).unwrap();
        let fd = open(&from, libc::O_PATH);
        std::fs::rename(&from, &hard).unwrap();
        let info =
            std::fs::read_to_string(format!("/proc/self/fdinfo/{}", fd.as_raw_fd())).unwrap();
        assert!(info.contains(&format!("mnt_id: {}\n", mount_id(&source))));
        assert!(std::path::Path::new(&from).exists());
        assert_eq!(
            std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))
                .unwrap()
                .to_str()
                .unwrap(),
            from
        );
        std::fs::rename(&from, format!("{source}/renamed")).unwrap();
        assert_eq!(std::fs::read(format!("{alias}/renamed")).unwrap(), b"data");
        error(unsafe { libc::rmdir(c(&alias).as_ptr()) } as _, libc::EBUSY);
    })
    .join();
}
register_test!(test_mountns_mountinfo_and_cross_mount_operations);

fn mount_flags(source: &str, target: &str, flags: libc::c_ulong) {
    ok(unsafe {
        libc::mount(
            c(source).as_ptr(),
            c(target).as_ptr(),
            ptr::null(),
            flags,
            ptr::null(),
        )
    });
}
fn propagation(target: &str, flags: libc::c_ulong) {
    mount_flags("none", target, flags);
}
fn mount_line(target: &str) -> String {
    let id = mount_id(target);
    std::fs::read_to_string("/proc/self/mountinfo")
        .unwrap()
        .lines()
        .find(|s| s.starts_with(&format!("{id} ")))
        .unwrap()
        .into()
}
fn marker(target: &str, name: &str) {
    std::fs::write(format!("{target}/{name}"), b"mounted").unwrap();
}
fn visible(target: &str, name: &str) -> bool {
    std::path::Path::new(&format!("{target}/{name}")).exists()
}

fn test_mountns_shared_peers_and_namespace_copy() {
    let fixture = Fixture::new("shared-peers");
    let a = fixture.sub("a");
    let b = fixture.sub("b");
    spawn(libc::CLONE_NEWNS, || {
        mount(&a, "tmpfs");
        std::fs::create_dir(format!("{a}/sub")).unwrap();
        propagation(&a, libc::MS_SHARED);
        bind(&a, &b);
        let peer = mount_line(&a)
            .split_whitespace()
            .find(|s| s.starts_with("shared:"))
            .unwrap()
            .to_string();
        assert!(mount_line(&b).contains(&peer));
        spawn(libc::CLONE_NEWNS, || {
            mount(&format!("{a}/sub"), "tmpfs");
            marker(&format!("{a}/sub"), "child");
        })
        .join();
        assert!(visible(&format!("{a}/sub"), "child"));
        assert!(visible(&format!("{b}/sub"), "child"));
        let busy = open(&format!("{b}/sub/child"), libc::O_RDONLY);
        error(
            unsafe { libc::umount2(c(&format!("{a}/sub")).as_ptr(), 0) } as _,
            libc::EBUSY,
        );
        drop(busy);
        umount(&format!("{a}/sub"), 0);
        assert!(!visible(&format!("{b}/sub"), "child"));
        spawn(libc::CLONE_NEWNS, || {
            propagation(&a, libc::MS_PRIVATE | libc::MS_REC);
            mount(&format!("{a}/sub"), "tmpfs");
            marker(&format!("{a}/sub"), "private");
        })
        .join();
        assert!(!visible(&format!("{a}/sub"), "private"));
        assert!(!visible(&format!("{b}/sub"), "private"));
    })
    .join();
}
register_test!(test_mountns_shared_peers_and_namespace_copy);

fn test_mountns_slave_and_shared_slave_direction() {
    let fixture = Fixture::new("slave");
    let a = fixture.sub("a");
    let b = fixture.sub("b");
    let cpath = fixture.sub("c");
    let d = fixture.sub("d");
    spawn(libc::CLONE_NEWNS, || {
        mount(&a, "tmpfs");
        for name in ["up", "down"] {
            std::fs::create_dir(format!("{a}/{name}")).unwrap();
        }
        propagation(&a, libc::MS_SHARED);
        bind(&a, &b);
        propagation(&b, libc::MS_SLAVE);
        assert!(mount_line(&b).contains(" master:"));
        assert!(!mount_line(&b).contains(" shared:"));
        propagation(&b, libc::MS_SHARED);
        bind(&b, &cpath);
        bind(&b, &d);
        propagation(&d, libc::MS_SLAVE);
        assert!(mount_line(&b).contains(" shared:"));
        assert!(mount_line(&b).contains(" master:"));
        mount(&format!("{a}/up"), "tmpfs");
        marker(&format!("{a}/up"), "up");
        for target in [&a, &b, &cpath, &d] {
            assert!(visible(&format!("{target}/up"), "up"));
        }
        mount(&format!("{b}/down"), "tmpfs");
        marker(&format!("{b}/down"), "down");
        assert!(!visible(&format!("{a}/down"), "down"));
        for target in [&b, &cpath, &d] {
            assert!(visible(&format!("{target}/down"), "down"));
        }
        umount(&format!("{b}/down"), 0);
        for target in [&b, &cpath, &d] {
            assert!(!visible(&format!("{target}/down"), "down"));
        }
        umount(&format!("{a}/up"), 0);
        for target in [&a, &b, &cpath, &d] {
            assert!(!visible(&format!("{target}/up"), "up"));
        }
    })
    .join();
}
register_test!(test_mountns_slave_and_shared_slave_direction);

fn test_mountns_recursive_bind_prunes_unbindable() {
    let fixture = Fixture::new("rbind");
    let a = fixture.sub("a");
    let plain = fixture.sub("plain");
    let recursive = fixture.sub("recursive");
    spawn(libc::CLONE_NEWNS, || {
        mount(&a, "tmpfs");
        for name in ["kept", "pruned"] {
            std::fs::create_dir(format!("{a}/{name}")).unwrap();
        }
        mount(&format!("{a}/kept"), "tmpfs");
        marker(&format!("{a}/kept"), "inside");
        mount(&format!("{a}/pruned"), "tmpfs");
        marker(&format!("{a}/pruned"), "secret");
        propagation(&format!("{a}/pruned"), libc::MS_UNBINDABLE);
        bind(&a, &plain);
        mount_flags(&a, &recursive, libc::MS_BIND | libc::MS_REC);
        assert!(!visible(&format!("{plain}/kept"), "inside"));
        assert!(visible(&format!("{recursive}/kept"), "inside"));
        assert!(!visible(&format!("{recursive}/pruned"), "secret"));
        assert_ne!(
            mount_id(&format!("{a}/kept")),
            mount_id(&format!("{recursive}/kept"))
        );
        error(
            unsafe {
                libc::mount(
                    c(&format!("{a}/pruned")).as_ptr(),
                    c(&plain).as_ptr(),
                    ptr::null(),
                    libc::MS_BIND,
                    ptr::null(),
                )
            } as _,
            libc::EINVAL,
        );
        umount(&format!("{recursive}/kept"), 0);
        umount(&recursive, 0);
    })
    .join();
}
register_test!(test_mountns_recursive_bind_prunes_unbindable);

fn test_mountns_less_privileged_shared_copy_is_slave() {
    let fixture = Fixture::new("locked-shared");
    let a = fixture.sub("a");
    spawn(libc::CLONE_NEWNS, || {
        mount(&a, "tmpfs");
        for name in ["parent", "child"] {
            std::fs::create_dir(format!("{a}/{name}")).unwrap();
        }
        propagation(&a, libc::MS_SHARED);
        let ready = Pipe::new();
        let release = Pipe::new();
        let child = spawn(libc::CLONE_NEWUSER | libc::CLONE_NEWNS, || {
            assert!(mount_line(&a).contains(" master:"));
            assert!(!mount_line(&a).contains(" shared:"));
            mount(&format!("{a}/child"), "tmpfs");
            marker(&format!("{a}/child"), "child");
            ready.send();
            release.recv();
            assert!(visible(&format!("{a}/parent"), "parent"));
            error(
                unsafe { libc::umount2(c(&a).as_ptr(), libc::MNT_DETACH) } as _,
                libc::EINVAL,
            );
            error(
                unsafe {
                    libc::mount(
                        ptr::null(),
                        c(&a).as_ptr(),
                        ptr::null(),
                        libc::MS_REMOUNT | libc::MS_RDONLY,
                        ptr::null(),
                    )
                } as _,
                libc::EPERM,
            );
        });
        ready.recv();
        assert!(!visible(&format!("{a}/child"), "child"));
        mount(&format!("{a}/parent"), "tmpfs");
        marker(&format!("{a}/parent"), "parent");
        release.send();
        child.join();
    })
    .join();
}
register_test!(test_mountns_less_privileged_shared_copy_is_slave);

fn test_mountns_remount_readonly_superblock_and_bind() {
    let fixture = Fixture::new("remount");
    let a = fixture.sub("a");
    let b = fixture.sub("b");
    spawn(libc::CLONE_NEWNS, || {
        mount(&a, "tmpfs");
        marker(&a, "file");
        bind(&a, &b);
        mount_flags(
            "none",
            &b,
            libc::MS_REMOUNT | libc::MS_BIND | libc::MS_RDONLY,
        );
        let file = format!("{b}/file");
        error(
            unsafe { libc::open(c(&file).as_ptr(), libc::O_WRONLY) } as _,
            libc::EROFS,
        );
        error(
            unsafe { libc::chmod(c(&file).as_ptr(), 0o600) } as _,
            libc::EROFS,
        );
        error(unsafe { libc::unlink(c(&file).as_ptr()) } as _, libc::EROFS);
        error(
            unsafe { libc::mkdir(c(&format!("{b}/dir")).as_ptr(), 0o700) } as _,
            libc::EROFS,
        );
        let fd = open(&file, libc::O_RDONLY);
        error(
            unsafe { libc::fchmod(fd.as_raw_fd(), 0o600) } as _,
            libc::EROFS,
        );
        let mut stats = [0u64; 15];
        assert_eq!(
            unsafe { libc::syscall(libc::SYS_fstatfs, fd.as_raw_fd(), stats.as_mut_ptr()) },
            0
        );
        assert_ne!(stats[10] & 1, 0);
        marker(&a, "writable");
        let writer = open(&format!("{a}/file"), libc::O_WRONLY);
        error(
            unsafe {
                libc::mount(
                    ptr::null(),
                    c(&a).as_ptr(),
                    ptr::null(),
                    libc::MS_REMOUNT | libc::MS_RDONLY,
                    ptr::null(),
                )
            } as _,
            libc::EBUSY,
        );
        drop(writer);
        mount_flags("none", &a, libc::MS_REMOUNT | libc::MS_RDONLY);
        mount_flags("none", &b, libc::MS_REMOUNT | libc::MS_BIND);
        error(
            unsafe { libc::open(c(&file).as_ptr(), libc::O_WRONLY) } as _,
            libc::EROFS,
        );
        assert!(mount_line(&b).contains(" none ro"));
        mount_flags("none", &a, libc::MS_REMOUNT);
        marker(&b, "writable-again");
        mount_flags(
            "none",
            &b,
            libc::MS_REMOUNT | libc::MS_BIND | libc::MS_RDONLY,
        );
        let ready = Pipe::new();
        let release = Pipe::new();
        let child = spawn(libc::CLONE_NEWUSER | libc::CLONE_NEWNS, || {
            error(
                unsafe {
                    libc::mount(
                        ptr::null(),
                        c(&b).as_ptr(),
                        ptr::null(),
                        libc::MS_REMOUNT | libc::MS_BIND,
                        ptr::null(),
                    )
                } as _,
                libc::EPERM,
            );
            ready.send();
            release.recv();
        });
        ready.recv();
        release.send();
        child.join();
    })
    .join();
}
register_test!(test_mountns_remount_readonly_superblock_and_bind);

fn test_mountns_remount_noexec_nodev_and_mapping() {
    let fixture = Fixture::new("mount-flags");
    let binary = fixture.sub("bin");
    let device = format!("{}/null", fixture.0);
    std::fs::write(&device, b"").unwrap();
    spawn(libc::CLONE_NEWNS, || {
        bind("/bin", &binary);
        bind("/dev/null", &device);
        mount_flags(
            "none",
            &binary,
            libc::MS_REMOUNT | libc::MS_BIND | libc::MS_NOEXEC | libc::MS_NOSUID,
        );
        let file = format!("{binary}/busybox");
        let fd = open(&file, libc::O_RDONLY);
        let _path = open(&file, libc::O_PATH);
        error(
            unsafe { libc::access(c(&file).as_ptr(), libc::X_OK) } as _,
            libc::EACCES,
        );
        assert_eq!(
            std::process::Command::new(&file)
                .status()
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EACCES)
        );
        let exec = unsafe {
            libc::mmap(
                ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_EXEC,
                libc::MAP_PRIVATE,
                fd.as_raw_fd(),
                0,
            )
        };
        assert_eq!(exec, libc::MAP_FAILED);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
        let map = unsafe {
            libc::mmap(
                ptr::null_mut(),
                4096,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                fd.as_raw_fd(),
                0,
            )
        };
        assert_ne!(map, libc::MAP_FAILED);
        error(
            unsafe { libc::mprotect(map, 4096, libc::PROT_READ | libc::PROT_EXEC) } as _,
            libc::EACCES,
        );
        mount_flags("none", &binary, libc::MS_REMOUNT | libc::MS_BIND);
        // VM_MAYEXEC was fixed when this mapping was created.
        error(
            unsafe { libc::mprotect(map, 4096, libc::PROT_READ | libc::PROT_EXEC) } as _,
            libc::EACCES,
        );
        ok(unsafe { libc::munmap(map, 4096) });
        assert!(
            std::process::Command::new(&file)
                .arg("true")
                .status()
                .unwrap()
                .success()
        );
        mount_flags(
            "none",
            &device,
            libc::MS_REMOUNT | libc::MS_BIND | libc::MS_NODEV,
        );
        error(
            unsafe { libc::open(c(&device).as_ptr(), libc::O_RDONLY) } as _,
            libc::EACCES,
        );
        let _path = open(&device, libc::O_PATH);
        let _original = open("/dev/null", libc::O_RDONLY);
    })
    .join();
}
register_test!(test_mountns_remount_noexec_nodev_and_mapping);
