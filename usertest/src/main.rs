use colored::Colorize;
use std::{
    io::{Write, stdout},
    sync::{Arc, Barrier, Mutex},
    thread,
};

mod fs;
mod futex;
mod signals;

pub struct Test {
    pub test_text: &'static str,
    pub test_fn: fn(),
}

inventory::collect!(Test);

#[macro_export]
macro_rules! register_test {
    ($name:ident) => {
        // Add to inventory
        inventory::submit! {
            $crate::Test {
                test_text: concat!(module_path!(), "::", stringify!($name)),
                test_fn: $name,
            }
        }
    };
    ($name:ident, $text:expr) => {
        // Add to inventory
        inventory::submit! {
            $crate::Test {
                test_text: $text,
                test_fn: $name,
            }
        }
    };
}

fn test_sync() {
    unsafe {
        libc::sync();
    }
}

register_test!(test_sync);

fn test_clock_sleep() {
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    const SLEEP_LEN: Duration = Duration::from_millis(100);

    let now = Instant::now();
    sleep(SLEEP_LEN);
    assert!(now.elapsed() >= SLEEP_LEN);
}

register_test!(test_clock_sleep);

fn test_fork() {
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
}

register_test!(test_fork);

fn test_rust_thread() {
    let handle = thread::spawn(|| 24);

    assert_eq!(handle.join().unwrap(), 24);
}

register_test!(test_rust_thread);

fn test_rust_mutex() {
    const THREADS: usize = 32;
    const ITERS: usize = 1_000;

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
}

register_test!(test_rust_mutex);

fn test_parking_lot_mutex_timeout() {
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
}

register_test!(test_parking_lot_mutex_timeout);

fn test_thread_with_name() {
    let handle = thread::Builder::new()
        .name("test_thread".to_string())
        .spawn(|| {
            let current_thread = thread::current();
            assert_eq!(current_thread.name(), Some("test_thread"));
        })
        .unwrap();
    handle.join().unwrap();
}

register_test!(test_thread_with_name);

fn test_mincore() {
    use std::ptr;

    unsafe {
        let page_size = libc::sysconf(libc::_SC_PAGESIZE) as usize;
        assert!(page_size > 0);

        // Map exactly one page, read-write, anonymous private
        let addr = libc::mmap(
            ptr::null_mut(),
            page_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        if addr == libc::MAP_FAILED {
            panic!("mmap failed: {}", std::io::Error::last_os_error());
        }

        // Touch the page to fault it in
        ptr::write(addr as *mut u8, 42);

        let mut vec_byte: u8 = 0;
        let ret = libc::mincore(addr as *mut _, page_size, &mut vec_byte as *mut u8);
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            panic!("mincore failed: {}", err);
        }
        // LSB set indicates resident
        assert!(
            vec_byte & 0x1 == 0x1,
            "Expected page to be resident, vec={:02x}",
            vec_byte
        );

        // Cleanup
        let rc = libc::munmap(addr, page_size);
        assert_eq!(rc, 0, "munmap failed: {}", std::io::Error::last_os_error());
    }
}

register_test!(test_mincore);

fn segfault_child(inner: impl FnOnce()) {
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            panic!("fork failed");
        } else if pid == 0 {
            // Child process
            inner()
        } else {
            // Parent process
            let mut status = 0;
            let rusage = std::ptr::null_mut();
            libc::wait4(pid, &mut status, 0, rusage);

            assert!(libc::WIFSIGNALED(status));
            assert_eq!(libc::WTERMSIG(status), libc::SIGSEGV);
        }
    }
}

fn test_segfault_read() {
    segfault_child(|| {
        let addr: *const u8 = std::hint::black_box(std::ptr::null());
        let _ = unsafe { std::ptr::read(addr) };
    });
    segfault_child(|| {
        // Ensure reading from kernel stack fails
        let addr = 0xffff_ba00_0000_0000 as *const u8;
        let _ = unsafe { std::ptr::read(addr) };
    });
}

register_test!(test_segfault_read);

fn test_segfault_write() {
    segfault_child(|| {
        let addr: *mut u8 = std::hint::black_box(std::ptr::null_mut());
        unsafe { std::ptr::write(addr, 42) };
    });
    segfault_child(|| {
        let addr = 0xffff_ba00_0000_0000 as *mut u8;
        unsafe { std::ptr::write(addr, 42) };
    });
}

register_test!(test_segfault_write);

fn run_test(test_fn: fn()) -> Result<(), i32> {
    // Fork a new process to run the test
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            panic!("fork failed");
        } else if pid == 0 {
            // Child process
            let result = std::panic::catch_unwind(|| {
                test_fn();
            });
            let exit_code = if let Err(e) = result {
                // Get the panic info
                eprintln!("Test panicked: {:?}", e);
                let error = std::io::Error::last_os_error();
                eprintln!("Last OS error: {}", error);
                1
            } else {
                0
            };
            libc::_exit(exit_code);
        } else {
            // Parent process
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);
            if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
                Err(status)
            } else {
                Ok(())
            }
        }
    }
}

fn main() {
    println!("Running userspace tests ...");
    let start = std::time::Instant::now();
    let mut failures = 0;
    for test in inventory::iter::<Test> {
        print!("{} ...", test.test_text);
        let _ = stdout().flush();
        match run_test(test.test_fn) {
            Ok(()) => println!("{}", " OK".green()),
            Err(code) => {
                println!(" {}", "FAILED".red());
                eprintln!("Test '{}' failed with exit code {}", test.test_text, code);
                failures += 1;
            }
        }
    }
    let end = std::time::Instant::now();
    if failures > 0 {
        eprintln!(
            "{failures} tests failed in {} ms",
            (end - start).as_millis()
        );
        std::process::exit(1);
    }
    println!("All tests passed in {} ms", (end - start).as_millis());
}
