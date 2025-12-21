use std::thread;

fn test_sync() {
    print!("Testing sync syscall ...");
    unsafe {
        libc::sync();
    }
    println!(" OK");
}

fn test_opendir() {
    print!("Testing opendir syscall ...");
    let path = std::ffi::CString::new("/").unwrap();
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
    let path = std::ffi::CString::new("/").unwrap();
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
    let path = std::ffi::CString::new("/").unwrap();
    unsafe {
        if libc::chdir(path.as_ptr()) != 0 {
            panic!("chdir failed");
        }
    }
    println!(" OK");
}

fn test_chroot() {
    print!("Testing chroot syscall ...");
    let file = "/bin/busybox";
    let c_file = std::ffi::CString::new(file).unwrap();
    let path = std::ffi::CString::new("/dev").unwrap();
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
    let c_file = std::ffi::CString::new(file).unwrap();
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
    let c_file = std::ffi::CString::new(file).unwrap();
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
                panic!("Test failed in child process");
            }
        }
    }
}

fn main() {
    println!("Running userspace tests ...");
    let start = std::time::Instant::now();
    run_test(test_sync);
    run_test(test_opendir);
    run_test(test_readdir);
    run_test(test_chdir);
    run_test(test_chroot);
    run_test(test_fork);
    run_test(test_read);
    run_test(test_write);
    run_test(test_futex);
    run_test(test_rust_file);
    run_test(test_rust_dir);
    run_test(test_rust_thread);
    let end = std::time::Instant::now();
    println!("All tests passed in {} ms", (end - start).as_millis());
}
