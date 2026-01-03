use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use std::thread;
use std::time::Duration;

pub fn test_futex_bitset() {
    print!("Testing futex bitset ...");

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

    println!(" OK");
}
