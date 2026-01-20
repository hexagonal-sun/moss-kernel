use std::{
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
            crate::Test {
                test_text: stringify!($name),
                test_fn: $name,
            }
        }
    };
    ($name:ident, $text:expr) => {
        // Add to inventory
        inventory::submit! {
            crate::Test {
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

register_test!(test_sync, "Testing sync syscall");

fn test_clock_sleep() {
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    const SLEEP_LEN: Duration = Duration::from_millis(100);

    let now = Instant::now();
    sleep(SLEEP_LEN);
    assert!(now.elapsed() >= SLEEP_LEN);
}

register_test!(test_clock_sleep, "Testing clock sleep");

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

register_test!(test_fork, "Testing fork syscall");

fn test_rust_thread() {
    let handle = thread::spawn(|| 24);

    assert_eq!(handle.join().unwrap(), 24);
}

register_test!(test_rust_thread, "Testing rust threads");

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

register_test!(test_rust_mutex, "Testing rust mutex");

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

register_test!(
    test_parking_lot_mutex_timeout,
    "Testing parking_lot mutex with timeout"
);

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

register_test!(test_thread_with_name, "Testing thread with name");

#[cfg(target_arch = "x86_64")]
fn test_raw_syscall() {
    let ret: i64;
    unsafe {
       core::arch::asm!(
           "syscall",
           in("rax") 39, // getpid
           lateout("rax") ret,
           out("rcx") _,
           out("r11") _,
           options(nostack, preserves_flags)
       );
    }
    assert!(ret > 0);
}

#[cfg(target_arch = "x86_64")]
register_test!(test_raw_syscall, "Testing raw syscall instruction");

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
    for test in inventory::iter::<Test> {
        print!("{} ...", test.test_text);
        run_test(test.test_fn);
        println!(" OK");
    }
    let end = std::time::Instant::now();
    println!("All tests passed in {} ms", (end - start).as_millis());
}
