use crate::register_test;
use std::ffi::{CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt, symlink};
use std::path::Path;
use std::sync::{Arc, Barrier, mpsc};
use std::thread;

// Exercise the guest's ext4 root and tmpfs /tmp; verify the backing filesystem.
fn on_test_filesystem(root: &str, name: &str, test: fn(&Path)) {
    let c_root = CString::new(root).unwrap();
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::statfs(c_root.as_ptr(), &mut stat) }, 0);
    assert_eq!(
        stat.f_type as u64,
        if root == "/" { 0xef53 } else { 0x0102_1994 }
    );
    let dir = Path::new(root).join(name);
    fs::create_dir(&dir).unwrap();
    test(&dir);
    fs::remove_dir_all(dir).unwrap();
}

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

register_test!(test_opendir);

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

register_test!(test_readdir);

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
        if CStr::from_ptr(buffer.as_ptr()).to_string_lossy() != "/dev" {
            panic!("chdir failed");
        }
    }
}

register_test!(test_chdir);

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
        if CStr::from_ptr(buffer.as_ptr()).to_string_lossy() != "/dev" {
            panic!("fchdir failed");
        }
        libc::close(fd);
    }
}

register_test!(test_fchdir);

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

register_test!(test_chroot);

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

register_test!(test_chmod);

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

register_test!(test_fchmod);

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

register_test!(test_chown);

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

register_test!(test_fchown);

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

register_test!(test_read);

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

register_test!(test_write);

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

register_test!(test_link);

fn check_unlink_write_race(dir: &Path) {
    let path = dir.join("data");
    let alias = dir.join("alias");
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    thread::scope(|scope| {
        // Dropping requests on assertion failure releases the writer instead
        // of stranding it at a barrier and hiding the original failure.
        let (request, requests) = mpsc::sync_channel::<usize>(0);
        let (complete, completions) = mpsc::sync_channel(0);
        let file_ref = &file;
        let writer = scope.spawn(move || {
            for round in requests {
                let result = file_ref.set_len(0).and_then(|()| {
                    let bytes = vec![(round + 1) as u8; 8193 + round];
                    file_ref.write_all_at(&bytes, 0)
                });
                if complete.send(result).is_err() {
                    break;
                }
            }
        });
        for round in 0..64 {
            fs::hard_link(&path, &alias).unwrap();
            request.send(round).unwrap();
            fs::remove_file(&alias).unwrap();
            completions.recv().unwrap().unwrap();
            let expected = vec![(round + 1) as u8; 8193 + round];
            assert_eq!(file.metadata().unwrap().nlink(), 1);
            assert_eq!(file.metadata().unwrap().len(), expected.len() as u64);
            assert_eq!(
                fs::read(&path).unwrap(),
                expected,
                "unlink/write round {round}"
            );
        }
        drop(request);
        writer.join().unwrap();
    });
}

fn test_ext4_unlink_write_race() {
    on_test_filesystem("/", "fs-unlink-race", check_unlink_write_race);
}

register_test!(test_ext4_unlink_write_race);

fn test_tmpfs_unlink_write_race() {
    on_test_filesystem("/tmp", "fs-unlink-race", check_unlink_write_race);
}

register_test!(test_tmpfs_unlink_write_race);

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
        let ret = libc::readlink(c_link.as_ptr(), buffer.as_mut_ptr(), buffer.len());
        if ret < 0 {
            panic!("readlink failed");
        }
        if buffer[..ret as usize] != *b"/tmp/symlink_test" {
            panic!("readlink failed");
        }
    }
    fs::remove_file(path).expect("Failed to delete file");
    fs::remove_file(link).expect("Failed to delete link");
}

register_test!(test_symlink);

fn check_symlink_boundaries(dir: &Path) {
    for len in [1, 13, 59, 60, 61, 120] {
        let target = "t".repeat(len);
        let link = dir.join(format!("symlink-{len}"));
        let alias = dir.join(format!("alias-{len}"));
        symlink(&target, &link).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), Path::new(&target));
        fs::hard_link(&link, &alias).unwrap();
        assert_eq!(fs::symlink_metadata(&link).unwrap().nlink(), 2);
        fs::remove_file(&link).unwrap();
        assert_eq!(fs::symlink_metadata(&alias).unwrap().nlink(), 1);
        assert_eq!(fs::read_link(&alias).unwrap(), Path::new(&target));
        fs::remove_file(&alias).unwrap();
    }
    // Reuse freed inode/block numbers after removing the links.
    fs::write(dir.join("after-unlink"), vec![0x5a; 8192]).unwrap();
    assert_eq!(
        fs::read(dir.join("after-unlink")).unwrap(),
        vec![0x5a; 8192]
    );
}

fn test_ext4_symlink_boundaries() {
    on_test_filesystem("/", "fs-symlink", check_symlink_boundaries);
}

register_test!(test_ext4_symlink_boundaries);

fn test_tmpfs_symlink_boundaries() {
    on_test_filesystem("/tmp", "fs-symlink", check_symlink_boundaries);
}

register_test!(test_tmpfs_symlink_boundaries);

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

register_test!(test_rename);

fn check_rename_inode_consistency(dir: &Path) {
    let old = dir.join("old");
    let new = dir.join("new");
    fs::write(&old, b"payload").unwrap();
    let mut opened = File::open(&old).unwrap();
    let ino = opened.metadata().unwrap().ino();
    fs::rename(&old, &new).unwrap();
    assert!(!old.exists());
    assert_eq!(fs::metadata(&new).unwrap().ino(), ino);
    let mut bytes = Vec::new();
    opened.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"payload");
    assert_eq!(opened.metadata().unwrap().nlink(), 1);

    // Renaming two names of the same inode is a no-op, not an unlink.
    fs::hard_link(&new, &old).unwrap();
    fs::rename(&old, &new).unwrap();
    assert_eq!(fs::metadata(&old).unwrap().ino(), ino);
    assert_eq!(opened.metadata().unwrap().nlink(), 2);
    let c_old = CString::new(old.as_os_str().as_encoded_bytes()).unwrap();
    let c_new = CString::new(new.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                c_old.as_ptr(),
                libc::AT_FDCWD,
                c_new.as_ptr(),
                1,
            )
        },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EEXIST)
    );
    fs::remove_file(&old).unwrap();
    assert_eq!(opened.metadata().unwrap().nlink(), 1);

    fs::create_dir(dir.join("other")).unwrap();
    let dst = dir.join("other/dst");
    fs::write(&dst, b"replaced").unwrap();
    fs::hard_link(&dst, &old).unwrap();
    let replaced = File::open(&old).unwrap();
    fs::rename(&new, &dst).unwrap();
    assert!(!new.exists());
    assert_eq!(fs::read(&dst).unwrap(), b"payload");
    assert_eq!(replaced.metadata().unwrap().nlink(), 1);
    assert_eq!(fs::read(&old).unwrap(), b"replaced");

    // Opposing cross-directory moves must not invert directory lock order.
    fs::create_dir(dir.join("left")).unwrap();
    fs::create_dir(dir.join("right")).unwrap();
    thread::scope(|scope| {
        let barrier = Arc::new(Barrier::new(2));
        for worker in 0..2 {
            let barrier = barrier.clone();
            scope.spawn(move || {
                let a = dir.join(format!("left/{worker}"));
                let b = dir.join(format!("right/{worker}"));
                let (a, b) = if worker == 0 { (a, b) } else { (b, a) };
                fs::write(&a, [worker as u8]).unwrap();
                barrier.wait();
                for _ in 0..32 {
                    fs::rename(&a, &b).unwrap();
                    fs::rename(&b, &a).unwrap();
                }
                assert_eq!(fs::read(&a).unwrap(), [worker as u8]);
            });
        }
    });
}

fn test_ext4_rename_inode_consistency() {
    on_test_filesystem("/", "fs-rename", check_rename_inode_consistency);
}

register_test!(test_ext4_rename_inode_consistency);

fn test_tmpfs_rename_inode_consistency() {
    on_test_filesystem("/tmp", "fs-rename", check_rename_inode_consistency);
}

register_test!(test_tmpfs_rename_inode_consistency);

fn test_ext4_directory_rename() {
    on_test_filesystem("/", "fs-directory", |dir| {
        let base_links = fs::metadata(dir).unwrap().nlink();
        let source = dir.join("source");
        let parent = dir.join("parent");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&parent).unwrap();
        assert_eq!(fs::metadata(dir).unwrap().nlink(), base_links + 2);
        fs::write(source.join("file"), b"directory payload").unwrap();
        let opened = File::open(&source).unwrap();
        let ino = opened.metadata().unwrap().ino();
        let renamed = dir.join("renamed");
        fs::rename(&source, &renamed).unwrap();
        assert_eq!(fs::metadata(&renamed).unwrap().ino(), ino);
        let destination = parent.join("child");
        fs::rename(&renamed, &destination).unwrap();
        assert_eq!(
            fs::read(destination.join("file")).unwrap(),
            b"directory payload"
        );
        assert_eq!(fs::metadata(dir).unwrap().nlink(), base_links + 1);
        assert_eq!(fs::metadata(&parent).unwrap().nlink(), 3);
        // Use the pre-rename directory fd: path normalization cannot fake "..".
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::fstatat(opened.as_raw_fd(), c"..".as_ptr(), &mut stat, 0) },
            0
        );
        assert_eq!(stat.st_ino, fs::metadata(&parent).unwrap().ino());
        assert!(fs::rename(&parent, destination.join("invalid")).is_err());
        assert!(fs::remove_dir(&destination).is_err());
        fs::remove_file(destination.join("file")).unwrap();
        fs::remove_dir(&destination).unwrap();
        assert_eq!(fs::metadata(&parent).unwrap().nlink(), 2);
    });
}

register_test!(test_ext4_directory_rename);

fn test_truncate() {
    use std::fs::{self, File};
    use std::io::{Read, Seek, Write};

    let path = "/tmp/truncate_test.txt";
    let mut file = File::create_new(path).expect("Failed to create file");
    file.write_all(b"Hello, world!")
        .expect("Failed to write to file");
    unsafe {
        let result = libc::truncate(CString::new(path).unwrap().as_ptr(), 5);
        assert_eq!(
            result,
            0,
            "truncate failed: {}",
            std::io::Error::last_os_error()
        );
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

register_test!(test_truncate);

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

register_test!(test_ftruncate);

fn check_truncate_boundaries(dir: &Path) {
    let path = dir.join("file");
    let link = dir.join("link");
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    fs::hard_link(&path, &link).unwrap();
    let other = File::open(&link).unwrap();
    let data: Vec<u8> = (0..16385).map(|n| (n % 251 + 1) as u8).collect();
    // Include EOF in a retained block, exact block boundaries, and zero.
    for size in [5, 4095, 4096, 4097, 0] {
        file.set_len(0).unwrap();
        file.rewind().unwrap();
        file.write_all(&data).unwrap();
        let c_path = CString::new(link.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(
            unsafe { libc::truncate(c_path.as_ptr(), size as libc::off_t) },
            0
        );
        for metadata in [
            file.metadata().unwrap(),
            other.metadata().unwrap(),
            fs::metadata(&path).unwrap(),
        ] {
            assert_eq!(metadata.len(), size as u64);
            assert_eq!(metadata.nlink(), 2);
        }
        let mut bytes = vec![0xff; data.len()];
        assert_eq!(other.read_at(&mut bytes, 0).unwrap(), size);
        assert_eq!(&bytes[..size], &data[..size]);
        assert_eq!(fs::read(&path).unwrap(), data[..size]);
        file.set_len(data.len() as u64).unwrap();
        let mut expected = data[..size].to_vec();
        expected.resize(data.len(), 0);
        let actual = fs::read(&link).unwrap();
        assert_eq!(
            actual.len(),
            expected.len(),
            "shrink/extend length at {size}"
        );
        assert!(actual == expected, "shrink/extend content at {size}");
    }
}

fn test_ext4_truncate_boundaries() {
    on_test_filesystem("/", "fs-truncate", check_truncate_boundaries);
}

register_test!(test_ext4_truncate_boundaries);

fn test_tmpfs_truncate_boundaries() {
    on_test_filesystem("/tmp", "fs-truncate", check_truncate_boundaries);
}

register_test!(test_tmpfs_truncate_boundaries);

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

register_test!(test_utimens);

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
        pub __spare1: u32,
        pub __spare3: [u64; 9],
    }

    #[repr(C)]
    #[derive(Debug, Default, Clone, Copy)]
    pub struct StatXTimestamp {
        pub tv_sec: i64,
        pub tv_nsec: u32,
        pub __pad1: i32,
    }

    let file = "/tmp/statx_test";
    assert_eq!(std::mem::size_of::<StatX>(), 256);
    assert_eq!(std::mem::offset_of!(StatX, stx_mnt_id), 144);
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

register_test!(test_statx);

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
    fs::hard_link(path, "/tmp/rust_fs_test_link.txt").expect("Failed to create hard link");
    let metadata = fs::metadata(path).expect("Failed to get metadata");
    assert_eq!(metadata.len(), 12);
    fs::rename(
        "/tmp/rust_fs_test_link.txt",
        "/tmp/rust_fs_test_renamed.txt",
    )
    .expect("Failed to rename file");
    fs::remove_file("/tmp/rust_fs_test_renamed.txt").expect("Failed to delete renamed file");
    fs::remove_file(path).expect("Failed to delete file");
}

register_test!(test_rust_file);

fn test_rust_dir() {
    use std::fs;
    use std::path::Path;

    let dir_path = "/tmp/rust_dir_test";
    fs::create_dir(dir_path).expect("Failed to create directory");
    assert!(Path::new(dir_path).exists());
    fs::remove_dir(dir_path).expect("Failed to delete directory");
}

register_test!(test_rust_dir);
