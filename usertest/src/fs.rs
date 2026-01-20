use crate::register_test;
use std::ffi::{CStr, CString};
use std::fs;
use std::mem::MaybeUninit;

fn test_opendir() {
    let path = CString::new("/").unwrap();
    unsafe {
        let dir = libc::opendir(path.as_ptr());
        if dir.is_null() {
            panic!("opendir failed");
        }
        libc::closedir(dir);
    }
}

register_test!(test_opendir, "Testing opendir syscall");

fn test_readdir() {
    let path = CString::new("/").unwrap();
    unsafe {
        let dir = libc::opendir(path.as_ptr());
        if dir.is_null() {
            panic!("opendir failed");
        }
        let mut count = 0;
        loop {
            let entry = libc::readdir(dir);
            if entry.is_null() {
                break;
            }
            count += 1;
        }
        libc::closedir(dir);
        if count == 0 {
            panic!("readdir returned no entries");
        }
    }
}

register_test!(test_readdir, "Testing readdir syscall");

fn test_chdir() {
    let path = CString::new("/dev").unwrap();
    let mut buffer = [1u8; 16];
    unsafe {
        if libc::chdir(path.as_ptr()) != 0 {
            panic!("chdir failed");
        }
        if libc::getcwd(
            buffer.as_mut_ptr() as *mut libc::c_char,
            buffer.len() as libc::size_t,
        )
        .is_null()
        {
            panic!("getcwd failed");
        }
        if CStr::from_ptr(buffer.as_ptr() as *const libc::c_char).to_string_lossy() != "/dev" {
            panic!("chdir failed");
        }
    }
}

register_test!(test_chdir, "Testing chdir syscall");

fn test_fchdir() {
    let path = CString::new("/dev").unwrap();
    let mut buffer = [1u8; 16];
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_RDONLY);
        if fd == -1 {
            panic!("open failed");
        }
        if libc::fchdir(fd) != 0 {
            panic!("fchdir failed");
        }
        if libc::getcwd(
            buffer.as_mut_ptr() as *mut libc::c_char,
            buffer.len() as libc::size_t,
        )
        .is_null()
        {
            panic!("getcwd failed");
        }
        if CStr::from_ptr(buffer.as_ptr() as *const libc::c_char).to_string_lossy() != "/dev" {
            panic!("fchdir failed");
        }
        libc::close(fd);
    }
}

register_test!(test_fchdir, "Testing fchdir syscall");

fn test_chroot() {
    let file = "/bin/busybox";
    let c_file = CString::new(file).unwrap();
    let path = CString::new("/dev").unwrap();
    unsafe {
        if libc::chroot(path.as_ptr()) != 0 {
            panic!("chroot failed");
        } else {
            let fd = libc::open(c_file.as_ptr(), libc::O_RDONLY);
            if fd != -1 {
                panic!("chroot failed");
            }
        }
    }
}

register_test!(test_chroot, "Testing chroot syscall");

fn test_chmod() {
    let dir_path = "/tmp/chmod_test";
    let c_dir_path = CString::new(dir_path).unwrap();
    let mut buffer = MaybeUninit::uninit();

    fs::create_dir(dir_path).expect("Failed to create directory");

    let mode = libc::S_IRUSR | libc::S_IWUSR | libc::S_IXUSR;
    unsafe {
        if libc::chmod(c_dir_path.as_ptr(), mode) != 0 {
            panic!("chmod failed");
        }
        if libc::stat(c_dir_path.as_ptr(), buffer.as_mut_ptr()) != 0 {
            panic!("stat failed");
        }
        if buffer.assume_init().st_mode & 0o777 != mode {
            panic!("fchmod failed");
        }
    }
    fs::remove_dir(dir_path).expect("Failed to delete directory");
}

register_test!(test_chmod, "Testing chmod syscall");

fn test_fchmod() {
    let dir_path = "/tmp/fchmod_test";
    let c_dir_path = CString::new(dir_path).unwrap();
    let mut buffer = MaybeUninit::uninit();

    fs::create_dir(dir_path).expect("Failed to create directory");

    let mode = libc::S_IRUSR | libc::S_IWUSR | libc::S_IXUSR;
    unsafe {
        let fd = libc::open(c_dir_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY);
        if fd == -1 {
            panic!("open failed");
        }
        if libc::fchmod(fd, mode) != 0 {
            panic!("fchmod failed");
        }
        if libc::fstat(fd, buffer.as_mut_ptr()) != 0 {
            panic!("stat failed");
        }
        if buffer.assume_init().st_mode & 0o777 != mode {
            panic!("fchmod failed");
        }
        libc::close(fd);
    }
    fs::remove_dir(dir_path).expect("Failed to delete directory");
}

register_test!(test_fchmod, "Testing fchmod syscall");

fn test_chown() {
    let dir_path = "/tmp/chown_test";
    let c_dir_path = CString::new(dir_path).unwrap();
    let mut buffer = MaybeUninit::uninit();

    fs::create_dir(dir_path).expect("Failed to create directory");

    unsafe {
        if libc::chown(c_dir_path.as_ptr(), 1, 1) != 0 {
            panic!("chown failed");
        }
        if libc::stat(c_dir_path.as_ptr(), buffer.as_mut_ptr()) != 0 {
            panic!("stat failed");
        }
        let stat = buffer.assume_init();
        if stat.st_uid != 1 || stat.st_gid != 1 {
            panic!("chown failed");
        }
    }
    fs::remove_dir(dir_path).expect("Failed to delete directory");
}

register_test!(test_chown, "Testing chown syscall");

fn test_fchown() {
    let dir_path = "/tmp/fchown_test";
    let c_dir_path = CString::new(dir_path).unwrap();
    let mut buffer = MaybeUninit::uninit();

    fs::create_dir(dir_path).expect("Failed to create directory");

    unsafe {
        let fd = libc::open(c_dir_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY);
        if fd == -1 {
            panic!("open failed");
        }
        if libc::fchown(fd, 1, 1) != 0 {
            panic!("fchown failed");
        }
        if libc::fstat(fd, buffer.as_mut_ptr()) != 0 {
            panic!("stat failed");
        }
        let stat = buffer.assume_init();
        if stat.st_uid != 1 || stat.st_gid != 1 {
            panic!("fchown failed");
        }
        libc::close(fd);
    }
    fs::remove_dir(dir_path).expect("Failed to delete directory");
}

register_test!(test_fchown, "Testing fchown syscall");

fn test_read() {
    let file = "/dev/zero";
    let c_file = CString::new(file).unwrap();
    let mut buffer = [1u8; 16];
    unsafe {
        let fd = libc::open(c_file.as_ptr(), libc::O_RDONLY);
        if fd < 0 {
            panic!("open failed");
        }
        let ret = libc::read(fd, buffer.as_mut_ptr() as *mut libc::c_void, buffer.len());
        if ret < 0 || ret as usize != buffer.len() {
            panic!("read failed");
        }
        libc::close(fd);
        assert!(buffer.iter().take(ret as usize).all(|&b| b == 0));
    }
}

register_test!(test_read, "Testing read syscall");

fn test_write() {
    let file = "/dev/null";
    let c_file = CString::new(file).unwrap();
    let data = b"Hello, world!";
    unsafe {
        let fd = libc::open(c_file.as_ptr(), libc::O_WRONLY);
        if fd < 0 {
            panic!("open failed");
        }
        let ret = libc::write(fd, data.as_ptr() as *const libc::c_void, data.len());
        if ret < 0 || ret as usize != data.len() {
            panic!("write failed");
        }
        libc::close(fd);
    }
}

register_test!(test_write, "Testing write syscall");

fn test_link() {
    let path = "/tmp/link_test";
    let link = "/tmp/link_test_link";
    let c_path = CString::new(path).unwrap();
    let c_link = CString::new(link).unwrap();
    let mut stat_targetbuf = MaybeUninit::uninit();
    let mut stat_linkbuf = MaybeUninit::uninit();

    unsafe {
        let fd = libc::open(c_path.as_ptr(), libc::O_CREAT, 0o777);
        if fd < 0 {
            panic!("open failed");
        }
        libc::close(fd);

        let ret = libc::link(c_path.as_ptr(), c_link.as_ptr());
        if ret < 0 {
            panic!("link failed");
        }
        let ret = libc::stat(c_link.as_ptr(), stat_linkbuf.as_mut_ptr());
        if ret < 0 {
            panic!("stat failed");
        }
        let ret = libc::stat(c_path.as_ptr(), stat_targetbuf.as_mut_ptr());
        if ret < 0 {
            panic!("stat failed");
        }
        if stat_linkbuf.assume_init().st_ino != stat_targetbuf.assume_init().st_ino {
            panic!("link failed");
        }
    }
    fs::remove_file(path).expect("Failed to delete file");
    fs::remove_file(link).expect("Failed to delete link");
}

register_test!(test_link, "Testing link syscall");

fn test_symlink() {
    use std::fs::{self, File};
    use std::io::{Read, Write};

    let path = "/tmp/symlink_test";
    let link = "/tmp/symlink_test_link";
    let c_path = CString::new(path).unwrap();
    let c_link = CString::new(link).unwrap();
    let mut buffer = [1u8; 17];

    let mut file = File::create_new(path).expect("Failed to create file");
    file.write_all(b"Hello, world!")
        .expect("Failed to write to file");

    unsafe {
        let ret = libc::symlink(c_path.as_ptr(), c_link.as_ptr());
        if ret < 0 {
            panic!("symlink failed");
        }

        let mut file = File::open(link).expect("Failed to open file");
        let mut string = String::new();
        file.read_to_string(&mut string)
            .expect("Failed to read from file");
        if string != "Hello, world!" {
            panic!("symlink failed");
        }
        let ret = libc::readlink(c_link.as_ptr(), buffer.as_mut_ptr() as *mut libc::c_char, buffer.len());
        if ret < 0 {
            panic!("readlink failed");
        }
        if buffer != *b"/tmp/symlink_test" {
            panic!("readlink failed");
        }
    }
    fs::remove_file(path).expect("Failed to delete file");
    fs::remove_file(link).expect("Failed to delete link");
}

register_test!(test_symlink, "Testing symlink syscall");

fn test_rename() {
    use std::fs::{self, File};
    use std::io::{Read, Write};

    let old_path = "/tmp/rename_test";
    let new_path = "/tmp/rename_test_new";
    let c_old_path = CString::new(old_path).unwrap();
    let c_new_path = CString::new(new_path).unwrap();

    let mut file = File::create_new(old_path).expect("Failed to create file");
    file.write_all(b"Hello, world!")
        .expect("Failed to write to file");

    unsafe {
        let ret = libc::rename(c_old_path.as_ptr(), c_new_path.as_ptr());
        if ret < 0 {
            panic!("rename failed");
        }
        let fd = libc::open(c_old_path.as_ptr(), libc::O_RDONLY);
        if fd != -1 {
            panic!("open failed");
        }
    }
    let mut file = File::open(new_path).expect("Failed to open file");
    let mut string = String::new();
    file.read_to_string(&mut string)
        .expect("Failed to read from file");
    if string != "Hello, world!" {
        panic!("rename failed");
    }

    fs::remove_file(new_path).expect("Failed to delete file");
}

register_test!(test_rename, "Testing rename syscall");

fn test_truncate() {
    use std::fs::{self, File};
    use std::io::{Read, Seek, Write};

    let path = "/tmp/truncate_test.txt";
    let mut file = File::create_new(path).expect("Failed to create file");
    file.write_all(b"Hello, world!")
        .expect("Failed to write to file");
    unsafe {
        libc::truncate(CString::new(path).unwrap().as_ptr(), 5);
    }

    let mut string = String::new();
    file.rewind().expect("Failed to rewind file");
    file.read_to_string(&mut string)
        .expect("Failed to read from file");
    if string != "Hello" {
        println!("{string}");
        panic!("truncate failed");
    }

    fs::remove_file(path).expect("Failed to delete file");
}

register_test!(test_truncate, "Testing truncate syscall");

fn test_ftruncate() {
    let file = "/tmp/ftruncate_test.txt";
    let c_file = CString::new(file).unwrap();
    let data = b"Hello, world!";
    let mut buffer = [1u8; 5];
    unsafe {
        let fd = libc::open(c_file.as_ptr(), libc::O_RDWR | libc::O_CREAT, 0o777);
        let ret = libc::pwrite64(fd, data.as_ptr() as *const libc::c_void, data.len(), 0);
        if ret < 0 || ret as usize != data.len() {
            panic!("write failed");
        }
        libc::ftruncate(fd, 5);
        let ret = libc::pread64(
            fd,
            buffer.as_mut_ptr() as *mut libc::c_void,
            buffer.len(),
            0,
        );
        if ret < 0 || ret as usize != 5 {
            panic!("read failed");
        }
        if &buffer != b"Hello" {
            panic!("ftruncate failed");
        }
        libc::close(fd);
    }
    fs::remove_file(file).expect("Failed to delete file");
}

register_test!(test_ftruncate, "Testing ftruncate syscall");

fn test_utimens() {
    let file = "/tmp/utimens_test";
    let c_file = CString::new(file).unwrap();
    let mut buffer = MaybeUninit::uninit();

    let mut times = [libc::timespec {
        tv_sec: 1766620800,
        tv_nsec: 1,
    }; 2]; // 1 ns after dec 25 2025
    unsafe {
        let fd = libc::open(c_file.as_ptr(), libc::O_CREAT, 0o777);
        if fd < 0 {
            panic!("open failed");
        }
        let ret = libc::utimensat(libc::AT_FDCWD, c_file.as_ptr(), times.as_mut_ptr(), 0);
        if ret < 0 {
            panic!("utimensat failed");
        }
        let ret = libc::stat(c_file.as_ptr(), buffer.as_mut_ptr());
        if ret < 0 {
            panic!("stat failed");
        }
        let stat = buffer.assume_init();
        if stat.st_atime != times[0].tv_sec
            || stat.st_atime_nsec != times[0].tv_nsec
            || stat.st_mtime != times[1].tv_sec
            || stat.st_mtime_nsec != times[1].tv_nsec
        {
            panic!("utimensat failed");
        }

        times = [libc::timespec {
            tv_sec: 1767225600,
            tv_nsec: 5000,
        }; 2]; // 5000 ns after jan 1 2026
        let ret = libc::futimens(fd, times.as_mut_ptr());
        if ret < 0 {
            panic!("futimens failed");
        }
        let ret = libc::stat(c_file.as_ptr(), buffer.as_mut_ptr());
        if ret < 0 {
            panic!("stat failed");
        }
        let stat = buffer.assume_init();
        if stat.st_atime != times[0].tv_sec
            || stat.st_atime_nsec != times[0].tv_nsec
            || stat.st_mtime != times[1].tv_sec
            || stat.st_mtime_nsec != times[1].tv_nsec
        {
            panic!("utimensat failed");
        }
        libc::close(fd);
    }
    fs::remove_file(file).expect("Failed to delete file");
}

register_test!(test_utimens, "Testing utimens syscall");

fn test_statx() {
    #[repr(C)]
    #[derive(Debug, Default, Clone, Copy)]
    pub struct StatX {
        pub stx_mask: u32,
        pub stx_blksize: u32,
        pub stx_attributes: u64,
        pub stx_nlink: u32,
        pub stx_uid: u32,
        pub stx_gid: u32,
        pub stx_mode: u16,
        pub __pad1: u16,
        pub stx_ino: u64,
        pub stx_size: u64,
        pub stx_blocks: u64,
        pub stx_attributes_mask: u64,
        pub stx_atime: StatXTimestamp,
        pub stx_btime: StatXTimestamp,
        pub stx_ctime: StatXTimestamp,
        pub stx_mtime: StatXTimestamp,
        pub stx_rdev_major: u32,
        pub stx_rdev_minor: u32,
        pub stx_dev_major: u32,
        pub stx_dev_minor: u32,
        pub stx_mnt_id: u64,
        pub stx_dio_mem_align: u32,
        pub stx_dio_offset_align: u32,
        pub stx_subvol: u64,
        pub stx_atomic_write_unit_min: u32,
        pub stx_atomic_write_unit_max: u32,
        pub stx_atomic_write_segments_max: u32,
        pub stx_dio_read_offset_align: u32,
        pub stx_atomic_write_unit_max_opt: u32,
        pub __unused1: u64,
        pub __unused2: u64,
        pub __unused3: u64,
        pub __unused4: u64,
        pub __unused5: u64,
        pub __unused6: u64,
    }

    #[repr(C)]
    #[derive(Debug, Default, Clone, Copy)]
    pub struct StatXTimestamp {
        pub tv_sec: i64,
        pub tv_nsec: u32,
        pub __pad1: i32,
    }

    let file = "/tmp/statx_test";
    let c_file = CString::new(file).unwrap();
    let data = b"Hello, world!";
    let mut buffer = MaybeUninit::uninit();
    unsafe {
        let fd = libc::open(c_file.as_ptr(), libc::O_WRONLY | libc::O_CREAT, 0o644);
        if fd < 0 {
            panic!("open failed");
        }
        let ret = libc::write(fd, data.as_ptr() as *const libc::c_void, data.len());
        if ret < 0 {
            panic!("write failed");
        }
        libc::close(fd);
        let ret = libc::syscall(
            libc::SYS_statx,
            libc::AT_FDCWD,
            c_file.as_ptr(),
            0,
            0x000007ff as libc::c_uint,
            buffer.as_mut_ptr(),
        );
        if ret < 0 {
            panic!("statx failed");
        }
        let statx: StatX = buffer.assume_init();
        assert_eq!(statx.stx_mask, 0x000007ff);
        assert_eq!(statx.stx_nlink, 1);
        assert_eq!(statx.stx_mode as u32, libc::S_IFREG | 0o644);
        assert_eq!(statx.stx_uid, libc::getuid());
        assert_eq!(statx.stx_gid, libc::getgid());
        assert_eq!(statx.stx_size, data.len() as u64);
    }
    fs::remove_file(file).expect("Failed to delete file");
}

register_test!(test_statx, "Testing statx syscall");

fn test_rust_file() {
    use std::fs::{self, File};
    use std::io::{Read, Write};

    let path = "/tmp/rust_fs_test.txt";
    {
        let mut file = File::create(path).expect("Failed to create file");
        file.write_all(b"Hello, Rust!")
            .expect("Failed to write to file");
    }
    {
        let mut file = File::open(path).expect("Failed to open file");
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .expect("Failed to read from file");
        assert_eq!(contents, "Hello, Rust!");
    }
    fs::remove_file(path).expect("Failed to delete file");
}

register_test!(test_rust_file, "Testing rust file operations");

fn test_rust_dir() {
    use std::fs;
    use std::path::Path;

    let dir_path = "/tmp/rust_dir_test";
    fs::create_dir(dir_path).expect("Failed to create directory");
    assert!(Path::new(dir_path).exists());
    fs::remove_dir(dir_path).expect("Failed to delete directory");
}

register_test!(test_rust_dir, "Testing rust directory operations");
