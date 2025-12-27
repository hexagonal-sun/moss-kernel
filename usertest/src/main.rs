use std::{
    ffi::{CStr, CString},
    fs,
    mem::MaybeUninit,
    sync::{Arc, Barrier, Mutex},
    thread,
};

fn test_sync() {
    print!("Testing sync syscall ...");
    unsafe {
        libc::sync();
    }
    println!(" OK");
}

fn test_clock_sleep() {
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    const SLEEP_LEN: Duration = Duration::from_millis(100);

    print!("Testing clock and sleep syscalls ...");
    let now = Instant::now();
    sleep(SLEEP_LEN);
    assert!(now.elapsed() >= SLEEP_LEN);
    println!(" OK");
}

fn test_opendir() {
    print!("Testing opendir syscall ...");
    let path = CString::new("/").unwrap();
    unsafe {
        let dir = libc::opendir(path.as_ptr());
        if dir.is_null() {
            panic!("opendir failed");
        }
        libc::closedir(dir);
    }
    println!(" OK");
}

fn test_readdir() {
    print!("Testing readdir syscall ...");
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
    println!(" OK");
}

fn test_chdir() {
    print!("Testing chdir syscall ...");
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
    println!(" OK");
}

fn test_fchdir() {
    print!("Testing fchdir syscall ...");
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
    println!(" OK");
}

fn test_chroot() {
    print!("Testing chroot syscall ...");
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
    println!(" OK");
}

fn test_chmod() {
    print!("Testing chmod syscall ..."); // this actually tests fchmodat
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
    println!(" OK");
}

fn test_fchmod() {
    print!("Testing fchmod syscall ...");
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
    println!(" OK");
}

fn test_chown() {
    print!("Testing chown syscall ..."); // this actually tests fchownat
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
    println!(" OK");
}

fn test_fchown() {
    print!("Testing fchown syscall ...");
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
    println!(" OK");
}

fn test_fork() {
    print!("Testing fork syscall ...");
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            panic!("fork failed");
        } else if pid == 0 {
            // Child process
            libc::_exit(0);
        } else {
            // Parent process
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);
        }
    }
    println!(" OK");
}

fn test_read() {
    print!("Testing read syscall ...");
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
    println!(" OK");
}

fn test_write() {
    print!("Testing write syscall ...");
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
    println!(" OK");
}

fn test_link() {
    print!("Testing link syscall ..."); // actually tests linkat
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
    println!(" OK");
}

fn test_symlink() {
    use std::fs::{self, File};
    use std::io::{Read, Write};

    print!("Testing symlink syscall ..."); // actually tests symlinkat
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
        if buffer != *b"/tmp/symlink_test" {
            panic!("readlink failed");
        }
    }
    fs::remove_file(path).expect("Failed to delete file");
    fs::remove_file(link).expect("Failed to delete link");
    println!(" OK");
}

fn test_rename() {
    use std::fs::{self, File};
    use std::io::{Read, Write};

    print!("Testing rename syscall ...");
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
    println!(" OK");
}

fn test_futex() {
    print!("Testing futex syscall ...");
    let mut futex_word: libc::c_uint = 0;
    let addr = &mut futex_word as *mut libc::c_uint;
    unsafe {
        // FUTEX_WAKE should succeed (no waiters, returns 0)
        let ret = libc::syscall(
            libc::SYS_futex,
            addr,
            libc::FUTEX_WAKE,
            1,
            std::ptr::null::<libc::c_void>(),
            std::ptr::null::<libc::c_void>(),
            0,
        );
        if ret < 0 {
            panic!("futex wake failed");
        }

        // FUTEX_WAIT with an *unexpected* value (1) should fail immediately and
        // return -1 with errno = EAGAIN.  We just check the return value here
        // to avoid blocking the test.
        let ret2 = libc::syscall(
            libc::SYS_futex,
            addr,
            libc::FUTEX_WAIT,
            1u32, // expected value differs from actual (0)
            std::ptr::null::<libc::c_void>(),
            std::ptr::null::<libc::c_void>(),
            0,
        );
        if ret2 != -1 {
            panic!("futex wait did not error out as expected");
        }
    }
    println!(" OK");
}

fn test_truncate() {
    print!("Testing truncate syscall ...");
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
    println!(" OK");
}

fn test_ftruncate() {
    print!("Testing ftruncate syscall ...");
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
    println!(" OK");
}

fn test_utimens() {
    print!("Testing utimens syscall ...");
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
    println!(" OK");
}

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

    print!("Testing statx syscall ...");
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
    println!(" OK");
}

fn test_rust_file() {
    print!("Testing rust file operations ...");
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
    println!(" OK");
}

fn test_rust_dir() {
    print!("Testing rust directory operations ...");
    use std::fs;
    use std::path::Path;

    let dir_path = "/tmp/rust_dir_test";
    fs::create_dir(dir_path).expect("Failed to create directory");
    assert!(Path::new(dir_path).exists());
    fs::remove_dir(dir_path).expect("Failed to delete directory");
    println!(" OK");
}

fn test_rust_thread() {
    print!("Testing rust threads ...");

    let handle = thread::spawn(|| 24);

    assert_eq!(handle.join().unwrap(), 24);
    println!(" OK");
}

fn test_rust_mutex() {
    const THREADS: usize = 32;
    const ITERS: usize = 1_000;

    print!("Testing rust mutex ...");

    let mtx = Arc::new(Mutex::new(0usize));
    let barrier = Arc::new(Barrier::new(THREADS));

    let mut handles = Vec::with_capacity(THREADS);

    for _ in 0..THREADS {
        let mtx = Arc::clone(&mtx);
        let barrier = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            barrier.wait();

            for _ in 0..ITERS {
                let mut guard = mtx.lock().unwrap();
                *guard += 1;
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    let final_val = *mtx.lock().unwrap();

    assert_eq!(final_val, THREADS * ITERS);

    println!(" OK");
}

fn test_parking_lot_mutex_timeout() {
    print!("Testing parking_lot mutex with timeout ...");
    use parking_lot::Mutex;
    use std::time::Duration;
    let mtx = Arc::new(Mutex::new(()));
    let mtx_clone = Arc::clone(&mtx);
    let guard = mtx.lock();
    // Now try to acquire the lock with a timeout in another thread
    let handle = thread::spawn(move || {
        let timeout = Duration::from_millis(100);
        let result = mtx_clone.try_lock_for(timeout);
        assert!(result.is_none(), "Expected to not acquire the lock");
    });
    handle.join().unwrap();
    drop(guard);
    println!(" OK");
}

fn run_test(test_fn: fn()) {
    // Fork a new process to run the test
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            panic!("fork failed");
        } else if pid == 0 {
            // Child process
            test_fn();
            libc::_exit(0);
        } else {
            // Parent process
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);
            if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
                panic!(
                    "Test failed in child process: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}

fn main() {
    println!("Running userspace tests ...");
    let start = std::time::Instant::now();
    run_test(test_sync);
    run_test(test_clock_sleep);
    run_test(test_opendir);
    run_test(test_readdir);
    run_test(test_chdir);
    run_test(test_fchdir);
    run_test(test_chroot);
    run_test(test_chmod);
    run_test(test_fchmod);
    run_test(test_chown);
    run_test(test_fchown);
    run_test(test_fork);
    run_test(test_read);
    run_test(test_write);
    run_test(test_link);
    run_test(test_symlink);
    run_test(test_rename);
    run_test(test_futex);
    run_test(test_truncate);
    run_test(test_ftruncate);
    run_test(test_utimens);
    run_test(test_statx);
    run_test(test_rust_file);
    run_test(test_rust_dir);
    run_test(test_rust_thread);
    run_test(test_rust_mutex);
    run_test(test_parking_lot_mutex_timeout);
    let end = std::time::Instant::now();
    println!("All tests passed in {} ms", (end - start).as_millis());
}
