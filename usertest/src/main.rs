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
    // TODO: make tests their own process later
    run_test(test_sync);
    run_test(test_opendir);
    run_test(test_readdir);
    run_test(test_chdir);
    run_test(test_fork);
    let end = std::time::Instant::now();
    println!("All tests passed in {} ms", (end - start).as_millis());
}

