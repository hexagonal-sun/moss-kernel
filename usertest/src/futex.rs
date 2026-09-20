use crate::register_test;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use std::thread;
use std::time::Duration;

fn test_futex() {
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
}

register_test!(test_futex);

fn test_futex_timeout_clocks() {
    // No threads are needed: each wait expires with an unchanged futex word.
    // A relative WAIT must use monotonic time; WAIT_BITSET uses an absolute
    // monotonic deadline unless FUTEX_CLOCK_REALTIME is explicitly selected.
    for (operation, clock, absolute) in [
        (libc::FUTEX_WAIT, libc::CLOCK_MONOTONIC, false),
        (libc::FUTEX_WAIT_BITSET, libc::CLOCK_MONOTONIC, true),
        (
            libc::FUTEX_WAIT_BITSET | libc::FUTEX_CLOCK_REALTIME,
            libc::CLOCK_REALTIME,
            true,
        ),
    ] {
        for private in [0, libc::FUTEX_PRIVATE_FLAG] {
            let word = 0u32;
            let mut deadline = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            if absolute {
                assert_eq!(unsafe { libc::clock_gettime(clock, &mut deadline) }, 0);
            }
            deadline.tv_nsec += 100_000_000;
            if deadline.tv_nsec >= 1_000_000_000 {
                deadline.tv_nsec -= 1_000_000_000;
                deadline.tv_sec += 1;
            }
            let start = std::time::Instant::now();
            let result = unsafe {
                libc::syscall(
                    libc::SYS_futex,
                    &word,
                    operation | private,
                    0u32,
                    &deadline,
                    std::ptr::null::<u32>(),
                    u32::MAX,
                )
            };
            let error = std::io::Error::last_os_error();
            assert_eq!(result, -1);
            assert_eq!(error.raw_os_error(), Some(libc::ETIMEDOUT));
            assert!(
                start.elapsed() >= Duration::from_millis(80),
                "timeout expired early"
            );
        }
    }
}

register_test!(test_futex_timeout_clocks);

fn test_futex_unsupported_realtime_ops() {
    for operation in [libc::FUTEX_WAIT, libc::FUTEX_WAKE, libc::FUTEX_WAKE_BITSET] {
        for private in [0, libc::FUTEX_PRIVATE_FLAG] {
            let word = 0u32;
            let result = unsafe {
                libc::syscall(
                    libc::SYS_futex,
                    &word,
                    operation | private | libc::FUTEX_CLOCK_REALTIME,
                    0u32,
                    std::ptr::null::<libc::timespec>(),
                    std::ptr::null::<u32>(),
                    u32::MAX,
                )
            };
            assert_eq!(result, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ENOSYS)
            );
        }
    }
}

register_test!(test_futex_unsupported_realtime_ops);

fn test_pthread_cond_timedwait_clocks() {
    // Exercise libc's actual timed-wait path, not just the raw futex ABI.
    // In glibc, rejecting WAIT_BITSET|CLOCK_REALTIME can abort the process.
    for clock in [libc::CLOCK_REALTIME, libc::CLOCK_MONOTONIC] {
        unsafe {
            let mut mutex: libc::pthread_mutex_t = std::mem::zeroed();
            let mut cond: libc::pthread_cond_t = std::mem::zeroed();
            let mut attr: libc::pthread_condattr_t = std::mem::zeroed();
            assert_eq!(libc::pthread_mutex_init(&mut mutex, std::ptr::null()), 0);
            assert_eq!(libc::pthread_condattr_init(&mut attr), 0);
            assert_eq!(libc::pthread_condattr_setclock(&mut attr, clock), 0);
            assert_eq!(libc::pthread_cond_init(&mut cond, &attr), 0);
            assert_eq!(libc::pthread_condattr_destroy(&mut attr), 0);
            assert_eq!(libc::pthread_mutex_lock(&mut mutex), 0);

            let mut deadline: libc::timespec = std::mem::zeroed();
            assert_eq!(libc::clock_gettime(clock, &mut deadline), 0);
            deadline.tv_nsec += 100_000_000;
            if deadline.tv_nsec >= 1_000_000_000 {
                deadline.tv_nsec -= 1_000_000_000;
                deadline.tv_sec += 1;
            }
            let start = std::time::Instant::now();
            loop {
                let result = libc::pthread_cond_timedwait(&mut cond, &mut mutex, &deadline);
                if result == 0 {
                    // POSIX permits spurious wakeups; retain the same deadline.
                    continue;
                }
                assert_eq!(result, libc::ETIMEDOUT, "clock {clock}");
                break;
            }
            assert!(start.elapsed() >= Duration::from_millis(80));
            assert_eq!(libc::pthread_mutex_trylock(&mut mutex), libc::EBUSY);
            assert_eq!(libc::pthread_mutex_unlock(&mut mutex), 0);
            assert_eq!(libc::pthread_cond_destroy(&mut cond), 0);
            assert_eq!(libc::pthread_mutex_destroy(&mut mutex), 0);
        }
    }
}

register_test!(test_pthread_cond_timedwait_clocks);

fn test_futex_bitset() {
    // Wait on bit 1, Wake on bit 1
    {
        let futex_word = Arc::new(AtomicU32::new(0));
        let futex_clone = futex_word.clone();

        let t = thread::spawn(move || {
            let addr = futex_clone.as_ptr();
            unsafe {
                // Wait for value 0, with bitmask 0x01
                let ret = libc::syscall(
                    libc::SYS_futex,
                    addr,
                    libc::FUTEX_WAIT_BITSET,
                    0,
                    std::ptr::null::<libc::c_void>(),
                    std::ptr::null::<libc::c_void>(),
                    0x1u32,
                );

                // we were woken up
                if ret != 0 {
                    panic!(
                        "Unexpected return value from futex wait bitset syscall: {}",
                        ret
                    );
                }
            }
        });

        thread::sleep(Duration::from_millis(100));

        // Wake using bitmask 0x01
        let addr = futex_word.as_ptr();
        unsafe {
            let ret = libc::syscall(
                libc::SYS_futex,
                addr,
                libc::FUTEX_WAKE_BITSET,
                1,
                std::ptr::null::<libc::c_void>(),
                std::ptr::null::<libc::c_void>(),
                0x01u32,
            );

            if ret != 1 {
                panic!(
                    "Expected to wake 1 thread with matching bitset, woke {}",
                    ret
                );
            }
        }

        t.join().expect("Thread panicked");
    }

    // Wait on bit 2, Wake on bit 1.
    {
        let futex_word = Arc::new(AtomicU32::new(0));
        let futex_clone = futex_word.clone();

        let woke_up = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let woke_up_clone = woke_up.clone();

        let t = thread::spawn(move || {
            let addr = futex_clone.as_ptr();
            unsafe {
                // Wait for value 0, with bitmask 0x02
                libc::syscall(
                    libc::SYS_futex,
                    addr,
                    libc::FUTEX_WAIT_BITSET,
                    0,
                    std::ptr::null::<libc::c_void>(),
                    std::ptr::null::<libc::c_void>(),
                    0x02u32, // Mask is 0x02
                );

                woke_up_clone.store(true, Ordering::SeqCst);
            }
        });

        thread::sleep(Duration::from_millis(100));

        let addr = futex_word.as_ptr();
        unsafe {
            // Attempt to wake using mask 0x01: should not wake up the thread.
            let ret = libc::syscall(
                libc::SYS_futex,
                addr,
                libc::FUTEX_WAKE_BITSET,
                1,
                std::ptr::null::<libc::c_void>(),
                std::ptr::null::<libc::c_void>(),
                0x01u32, // Mask 0x01
            );

            if ret != 0 {
                panic!("Woke thread despite mismatched bitset masks.");
            }
        }

        // Verify thread is still sleeping
        if woke_up.load(Ordering::SeqCst) {
            panic!("Thread woke up unexpectedly");
        }

        // Wake with matching mask (0x02) so the thread can exit.
        unsafe {
            let ret = libc::syscall(
                libc::SYS_futex,
                addr,
                libc::FUTEX_WAKE_BITSET,
                1,
                std::ptr::null::<libc::c_void>(),
                std::ptr::null::<libc::c_void>(),
                0x02u32,
            );

            if ret != 1 {
                panic!("Failed to clean up waiting thread");
            }
        }

        t.join().expect("Thread panicked");
    }

    {
        let futex_word = Arc::new(AtomicU32::new(0));
        let futex_clone = futex_word.clone();

        let t = thread::spawn(move || {
            let addr = futex_clone.as_ptr();
            unsafe {
                // Wait on an odd bit
                libc::syscall(
                    libc::SYS_futex,
                    addr,
                    libc::FUTEX_WAIT_BITSET,
                    0,
                    std::ptr::null::<libc::c_void>(),
                    std::ptr::null::<libc::c_void>(),
                    0x0000_1000u32,
                );
            }
        });

        thread::sleep(Duration::from_millis(100));

        let addr = futex_word.as_ptr();
        unsafe {
            // Wake with MATCH_ANY
            let ret = libc::syscall(
                libc::SYS_futex,
                addr,
                libc::FUTEX_WAKE_BITSET,
                1,
                std::ptr::null::<libc::c_void>(),
                std::ptr::null::<libc::c_void>(),
                u32::MAX,
            );

            if ret != 1 {
                panic!("MATCH_ANY failed to wake thread");
            }
        }
        t.join().expect("Thread panicked");
    }
}

register_test!(test_futex_bitset);
