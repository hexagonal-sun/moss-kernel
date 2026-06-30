//! Tests for the futex2 syscall family (futex_wait, futex_wake, futex_waitv,
//! futex_requeue). libc has no wrappers for these, so we issue raw syscalls.

use crate::register_test;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

const SYS_FUTEX_WAITV: libc::c_long = 449;
const SYS_FUTEX_WAKE: libc::c_long = 454;
const SYS_FUTEX_WAIT: libc::c_long = 455;
const SYS_FUTEX_REQUEUE: libc::c_long = 456;

const FUTEX2_SIZE_U32: u32 = 0x02;
const FUTEX2_PRIVATE: u32 = 0x80;
const MATCH_ANY: u64 = 0xffff_ffff;

/// Userspace mirror of the kernel's `struct futex_waitv`.
#[repr(C)]
#[derive(Clone, Copy)]
struct FutexWaitv {
    val: u64,
    uaddr: u64,
    flags: u32,
    reserved: u32,
}

impl FutexWaitv {
    fn new(addr: *const u32, val: u32, flags: u32) -> Self {
        Self {
            val: val as u64,
            uaddr: addr as u64,
            flags,
            reserved: 0,
        }
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap()
}

unsafe fn futex2_wait(
    addr: *const u32,
    val: u64,
    mask: u64,
    flags: u32,
    timeout: *const libc::timespec,
    clockid: i32,
) -> i64 {
    unsafe { libc::syscall(SYS_FUTEX_WAIT, addr, val, mask, flags, timeout, clockid) }
}

unsafe fn futex2_wake(addr: *const u32, mask: u64, nr: i32, flags: u32) -> i64 {
    unsafe { libc::syscall(SYS_FUTEX_WAKE, addr, mask, nr, flags) }
}

unsafe fn futex2_waitv(
    waiters: *const FutexWaitv,
    nr: u32,
    flags: u32,
    timeout: *const libc::timespec,
    clockid: i32,
) -> i64 {
    unsafe { libc::syscall(SYS_FUTEX_WAITV, waiters, nr, flags, timeout, clockid) }
}

unsafe fn futex2_requeue(
    waiters: *const FutexWaitv,
    flags: u32,
    nr_wake: i32,
    nr_requeue: i32,
) -> i64 {
    unsafe { libc::syscall(SYS_FUTEX_REQUEUE, waiters, flags, nr_wake, nr_requeue) }
}

/// Absolute deadline `offset_ms` in the future on `clockid`.
fn abs_deadline(clockid: i32, offset_ms: i64) -> libc::timespec {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe { libc::clock_gettime(clockid, &mut ts) };

    let nsec = ts.tv_nsec + (offset_ms % 1000) * 1_000_000;
    ts.tv_sec += offset_ms / 1000 + nsec / 1_000_000_000;
    ts.tv_nsec = nsec % 1_000_000_000;
    ts
}

fn test_futex2_basic() {
    let futex_word: u32 = 0;
    let addr = &futex_word as *const u32;

    unsafe {
        // Wake with no waiters wakes nothing.
        let ret = futex2_wake(addr, MATCH_ANY, 1, FUTEX2_SIZE_U32);
        if ret != 0 {
            panic!("futex_wake with no waiters returned {ret}");
        }

        // Wait with a mismatched expected value fails immediately with EAGAIN.
        let ret = futex2_wait(
            addr,
            1, // actual value is 0
            MATCH_ANY,
            FUTEX2_SIZE_U32,
            std::ptr::null(),
            libc::CLOCK_MONOTONIC,
        );
        if ret != -1 || errno() != libc::EAGAIN {
            panic!("futex_wait value mismatch: ret {ret}, errno {}", errno());
        }
    }
}

register_test!(test_futex2_basic);

fn test_futex2_wait_wake() {
    let futex_word = Arc::new(AtomicU32::new(0));
    let futex_clone = futex_word.clone();

    let t = thread::spawn(move || {
        let addr = futex_clone.as_ptr() as *const u32;
        let ret = unsafe {
            futex2_wait(
                addr,
                0,
                MATCH_ANY,
                FUTEX2_SIZE_U32,
                std::ptr::null(),
                libc::CLOCK_MONOTONIC,
            )
        };
        if ret != 0 {
            panic!("futex_wait returned {ret}, errno {}", errno());
        }
    });

    thread::sleep(Duration::from_millis(100));

    let addr = futex_word.as_ptr() as *const u32;
    unsafe {
        let ret = futex2_wake(addr, MATCH_ANY, 1, FUTEX2_SIZE_U32);
        if ret != 1 {
            panic!("expected to wake 1 waiter, woke {ret}");
        }
    }

    t.join().expect("waiter thread panicked");
}

register_test!(test_futex2_wait_wake);

fn test_futex2_mask() {
    let futex_word = Arc::new(AtomicU32::new(0));
    let futex_clone = futex_word.clone();

    let t = thread::spawn(move || {
        let addr = futex_clone.as_ptr() as *const u32;
        let ret = unsafe {
            futex2_wait(
                addr,
                0,
                0x1, // waiter mask
                FUTEX2_SIZE_U32,
                std::ptr::null(),
                libc::CLOCK_MONOTONIC,
            )
        };
        if ret != 0 {
            panic!("masked futex_wait returned {ret}, errno {}", errno());
        }
    });

    thread::sleep(Duration::from_millis(100));

    let addr = futex_word.as_ptr() as *const u32;
    unsafe {
        // Non-overlapping mask must not wake the waiter.
        let ret = futex2_wake(addr, 0x2, 1, FUTEX2_SIZE_U32);
        if ret != 0 {
            panic!("woke {ret} waiters despite non-overlapping mask");
        }

        // Overlapping mask wakes it.
        let ret = futex2_wake(addr, 0x1, 1, FUTEX2_SIZE_U32);
        if ret != 1 {
            panic!("expected to wake 1 waiter with matching mask, woke {ret}");
        }
    }

    t.join().expect("waiter thread panicked");
}

register_test!(test_futex2_mask);

fn test_futex2_waitv() {
    // Three futex words; the waiter sleeps on all of them and must report
    // the index of the one that got woken.
    let words = Arc::new([AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)]);
    let words_clone = words.clone();

    let flags = FUTEX2_SIZE_U32 | FUTEX2_PRIVATE;

    let t = thread::spawn(move || {
        let waiters: Vec<FutexWaitv> = words_clone
            .iter()
            .map(|w| FutexWaitv::new(w.as_ptr() as *const u32, 0, flags))
            .collect();

        let ret = unsafe {
            futex2_waitv(
                waiters.as_ptr(),
                waiters.len() as u32,
                0,
                std::ptr::null(),
                libc::CLOCK_MONOTONIC,
            )
        };
        if ret != 1 {
            panic!("futex_waitv returned {ret}, expected woken index 1");
        }
    });

    thread::sleep(Duration::from_millis(100));

    unsafe {
        let ret = futex2_wake(words[1].as_ptr() as *const u32, MATCH_ANY, 1, flags);
        if ret != 1 {
            panic!("expected to wake 1 waitv waiter, woke {ret}");
        }
    }

    t.join().expect("waitv thread panicked");
}

register_test!(test_futex2_waitv);

fn test_futex2_invalid_args() {
    let word: u32 = 0;
    let addr = &word as *const u32;
    let valid = [FutexWaitv::new(addr, 0, FUTEX2_SIZE_U32)];

    unsafe {
        // waitv: one entry's expected value mismatches -> EAGAIN.
        let pair = [AtomicU32::new(0), AtomicU32::new(7)];
        let waiters = [
            FutexWaitv::new(pair[0].as_ptr() as *const u32, 0, FUTEX2_SIZE_U32),
            FutexWaitv::new(pair[1].as_ptr() as *const u32, 0, FUTEX2_SIZE_U32),
        ];
        let ret = futex2_waitv(waiters.as_ptr(), 2, 0, std::ptr::null(), 0);
        if ret != -1 || errno() != libc::EAGAIN {
            panic!("waitv mismatch: ret {ret}, errno {}", errno());
        }

        // waitv: nr_futexes == 0 and > 128 are invalid.
        for nr in [0u32, 129] {
            let ret = futex2_waitv(valid.as_ptr(), nr, 0, std::ptr::null(), 0);
            if ret != -1 || errno() != libc::EINVAL {
                panic!("waitv nr={nr}: ret {ret}, errno {}", errno());
            }
        }

        // waitv: non-zero syscall-level flags are invalid.
        let ret = futex2_waitv(valid.as_ptr(), 1, 1, std::ptr::null(), 0);
        if ret != -1 || errno() != libc::EINVAL {
            panic!("waitv flags=1: ret {ret}, errno {}", errno());
        }

        // waitv: reserved field must be zero.
        let mut bad = valid;
        bad[0].reserved = 1;
        let ret = futex2_waitv(bad.as_ptr(), 1, 0, std::ptr::null(), 0);
        if ret != -1 || errno() != libc::EINVAL {
            panic!("waitv reserved!=0: ret {ret}, errno {}", errno());
        }

        // waitv: entry flags must include FUTEX2_SIZE_U32.
        let mut bad = valid;
        bad[0].flags = 0;
        let ret = futex2_waitv(bad.as_ptr(), 1, 0, std::ptr::null(), 0);
        if ret != -1 || errno() != libc::EINVAL {
            panic!("waitv no-size flags: ret {ret}, errno {}", errno());
        }

        // waitv: NULL waiter array faults.
        let ret = futex2_waitv(std::ptr::null(), 1, 0, std::ptr::null(), 0);
        if ret != -1 || errno() != libc::EFAULT {
            panic!("waitv NULL array: ret {ret}, errno {}", errno());
        }

        // wait: bad clockid is only rejected when a timeout is supplied.
        let ts = abs_deadline(libc::CLOCK_MONOTONIC, 50);
        let ret = futex2_wait(addr, 1, MATCH_ANY, FUTEX2_SIZE_U32, &ts, 7);
        if ret != -1 || errno() != libc::EINVAL {
            panic!("wait bad clockid: ret {ret}, errno {}", errno());
        }

        // wait: zero mask can never be woken.
        let ret = futex2_wait(
            addr,
            0,
            0,
            FUTEX2_SIZE_U32,
            std::ptr::null(),
            libc::CLOCK_MONOTONIC,
        );
        if ret != -1 || errno() != libc::EINVAL {
            panic!("wait mask=0: ret {ret}, errno {}", errno());
        }

        // wait: futex word must be 4-byte aligned.
        let unaligned = (addr as usize + 2) as *const u32;
        let ret = futex2_wait(
            unaligned,
            0,
            MATCH_ANY,
            FUTEX2_SIZE_U32,
            std::ptr::null(),
            libc::CLOCK_MONOTONIC,
        );
        if ret != -1 || errno() != libc::EINVAL {
            panic!("wait unaligned: ret {ret}, errno {}", errno());
        }

        // requeue: negative counts are invalid.
        let pair = [valid[0], valid[0]];
        let ret = futex2_requeue(pair.as_ptr(), 0, -1, 0);
        if ret != -1 || errno() != libc::EINVAL {
            panic!("requeue nr_wake=-1: ret {ret}, errno {}", errno());
        }
    }
}

register_test!(test_futex2_invalid_args);

fn test_futex2_timeout() {
    let word: u32 = 0;
    let addr = &word as *const u32;

    for clockid in [libc::CLOCK_MONOTONIC, libc::CLOCK_REALTIME] {
        let ts = abs_deadline(clockid, 50);
        let start = Instant::now();

        let ret = unsafe { futex2_wait(addr, 0, MATCH_ANY, FUTEX2_SIZE_U32, &ts, clockid) };
        if ret != -1 || errno() != libc::ETIMEDOUT {
            panic!(
                "wait clockid={clockid}: ret {ret}, errno {}, expected ETIMEDOUT",
                errno()
            );
        }
        if start.elapsed() < Duration::from_millis(50) {
            panic!("wait on clockid={clockid} timed out early");
        }
    }

    // A deadline that has already passed times out immediately (the zero
    // point of both clocks is in the past).
    let ts: libc::timespec = unsafe { std::mem::zeroed() };
    let ret = unsafe {
        futex2_wait(
            addr,
            0,
            MATCH_ANY,
            FUTEX2_SIZE_U32,
            &ts,
            libc::CLOCK_MONOTONIC,
        )
    };
    if ret != -1 || errno() != libc::ETIMEDOUT {
        panic!("wait past deadline: ret {ret}, errno {}", errno());
    }

    // ...but a value mismatch still takes precedence over the timeout.
    let ret = unsafe {
        futex2_wait(
            addr,
            1,
            MATCH_ANY,
            FUTEX2_SIZE_U32,
            &ts,
            libc::CLOCK_MONOTONIC,
        )
    };
    if ret != -1 || errno() != libc::EAGAIN {
        panic!("wait mismatch+past deadline: ret {ret}, errno {}", errno());
    }
}

register_test!(test_futex2_timeout);

fn test_futex2_requeue() {
    const NR_THREADS: usize = 4;

    let f1 = Arc::new(AtomicU32::new(0));
    let f2 = Arc::new(AtomicU32::new(0));
    let woken = Arc::new(AtomicU32::new(0));

    let threads: Vec<_> = (0..NR_THREADS)
        .map(|_| {
            let f1 = f1.clone();
            let woken = woken.clone();
            thread::spawn(move || {
                let ret = unsafe {
                    futex2_wait(
                        f1.as_ptr() as *const u32,
                        0,
                        MATCH_ANY,
                        FUTEX2_SIZE_U32,
                        std::ptr::null(),
                        libc::CLOCK_MONOTONIC,
                    )
                };
                if ret != 0 {
                    panic!("requeued futex_wait returned {ret}, errno {}", errno());
                }
                woken.fetch_add(1, Ordering::SeqCst);
            })
        })
        .collect();

    // Let all threads park on f1.
    thread::sleep(Duration::from_millis(150));

    let pair = [
        FutexWaitv::new(f1.as_ptr() as *const u32, 0, FUTEX2_SIZE_U32),
        FutexWaitv::new(f2.as_ptr() as *const u32, 0, FUTEX2_SIZE_U32),
    ];

    unsafe {
        // Wake one waiter, move the rest to f2.
        let ret = futex2_requeue(pair.as_ptr(), 0, 1, NR_THREADS as i32 - 1);
        if ret != 1 {
            panic!("futex_requeue woke {ret}, expected 1");
        }
    }

    thread::sleep(Duration::from_millis(100));

    let woken_now = woken.load(Ordering::SeqCst);
    if woken_now != 1 {
        panic!("{woken_now} threads ran after requeue, expected 1");
    }

    unsafe {
        // The other three must now be waiting on f2, not f1.
        let ret = futex2_wake(f1.as_ptr() as *const u32, MATCH_ANY, 64, FUTEX2_SIZE_U32);
        if ret != 0 {
            panic!("woke {ret} waiters still on f1 after requeue");
        }

        let ret = futex2_wake(f2.as_ptr() as *const u32, MATCH_ANY, 64, FUTEX2_SIZE_U32);
        if ret != NR_THREADS as i64 - 1 {
            panic!("woke {ret} waiters on f2, expected {}", NR_THREADS - 1);
        }
    }

    for t in threads {
        t.join().expect("waiter thread panicked");
    }

    if woken.load(Ordering::SeqCst) != NR_THREADS as u32 {
        panic!("not all requeued threads completed");
    }
}

register_test!(test_futex2_requeue);

fn test_futex2_wake_n_of_m() {
    // Six waiters park on one futex; waking 2 must wake exactly 2 (the two
    // oldest, FIFO), leaving 4. A final wake-all drains the rest.
    const NR_THREADS: usize = 6;

    let f = Arc::new(AtomicU32::new(0));
    let woken = Arc::new(AtomicU32::new(0));

    let threads: Vec<_> = (0..NR_THREADS)
        .map(|_| {
            let f = f.clone();
            let woken = woken.clone();
            thread::spawn(move || {
                let ret = unsafe {
                    futex2_wait(
                        f.as_ptr() as *const u32,
                        0,
                        MATCH_ANY,
                        FUTEX2_SIZE_U32,
                        std::ptr::null(),
                        libc::CLOCK_MONOTONIC,
                    )
                };
                if ret != 0 {
                    panic!("futex_wait returned {ret}, errno {}", errno());
                }
                woken.fetch_add(1, Ordering::SeqCst);
            })
        })
        .collect();

    thread::sleep(Duration::from_millis(150));

    unsafe {
        let ret = futex2_wake(f.as_ptr() as *const u32, MATCH_ANY, 2, FUTEX2_SIZE_U32);
        if ret != 2 {
            panic!("wake nr=2 woke {ret}, expected 2");
        }
    }

    thread::sleep(Duration::from_millis(100));
    let woken_now = woken.load(Ordering::SeqCst);
    if woken_now != 2 {
        panic!("{woken_now} threads ran after wake nr=2, expected 2");
    }

    unsafe {
        let ret = futex2_wake(f.as_ptr() as *const u32, MATCH_ANY, 64, FUTEX2_SIZE_U32);
        if ret != NR_THREADS as i64 - 2 {
            panic!("wake-all woke {ret}, expected {}", NR_THREADS - 2);
        }
    }

    for t in threads {
        t.join().expect("waiter thread panicked");
    }
}

register_test!(test_futex2_wake_n_of_m);

fn test_futex2_wake_nr_zero() {
    // Waking 0 waiters is a no-op that returns 0, never EFAULT, even when the
    // address has no queue. Guards against computing the key (and faulting on
    // a shared translation) before the nr<=0 short-circuit.
    let word: u32 = 0;
    let addr = &word as *const u32;

    unsafe {
        let ret = futex2_wake(addr, MATCH_ANY, 0, FUTEX2_SIZE_U32);
        if ret != 0 {
            panic!("wake nr=0 returned {ret}, errno {}", errno());
        }

        // Negative count is likewise a 0-wake no-op (not EINVAL).
        let ret = futex2_wake(addr, MATCH_ANY, -1, FUTEX2_SIZE_U32);
        if ret != 0 {
            panic!("wake nr=-1 returned {ret}, errno {}", errno());
        }
    }
}

register_test!(test_futex2_wake_nr_zero);

fn test_futex2_wake_no_waiters() {
    // Waking an address nobody has ever waited on returns 0.
    let word: u32 = 0;
    let addr = &word as *const u32;

    unsafe {
        let ret = futex2_wake(addr, MATCH_ANY, 64, FUTEX2_SIZE_U32 | FUTEX2_PRIVATE);
        if ret != 0 {
            panic!("wake of un-waited address woke {ret}, expected 0");
        }
    }
}

register_test!(test_futex2_wake_no_waiters);

fn test_futex2_requeue_to_self() {
    // Requeueing a futex onto itself (uaddr1 == uaddr2) is rejected with
    // EINVAL on Linux. moss currently short-circuits this to a plain wake and
    // returns success, silently dropping nr_requeue -- this test encodes the
    // Linux contract and is expected to fail until that is fixed.
    let f = Arc::new(AtomicU32::new(0));
    let woken = Arc::new(AtomicU32::new(0));

    let f1 = f.clone();
    let woken1 = woken.clone();
    let t = thread::spawn(move || {
        let ret = unsafe {
            futex2_wait(
                f1.as_ptr() as *const u32,
                0,
                MATCH_ANY,
                FUTEX2_SIZE_U32,
                std::ptr::null(),
                libc::CLOCK_MONOTONIC,
            )
        };
        if ret != 0 {
            panic!("futex_wait returned {ret}, errno {}", errno());
        }
        woken1.fetch_add(1, Ordering::SeqCst);
    });

    thread::sleep(Duration::from_millis(100));

    let same = f.as_ptr() as *const u32;
    let pair = [
        FutexWaitv::new(same, 0, FUTEX2_SIZE_U32),
        FutexWaitv::new(same, 0, FUTEX2_SIZE_U32),
    ];

    unsafe {
        let ret = futex2_requeue(pair.as_ptr(), 0, 1, 1);
        if ret != -1 || errno() != libc::EINVAL {
            panic!("requeue-to-self: ret {ret}, errno {}, expected EINVAL", errno());
        }
    }

    // The waiter is still parked (requeue was rejected); wake it so the
    // thread can exit cleanly.
    unsafe {
        futex2_wake(same, MATCH_ANY, 1, FUTEX2_SIZE_U32);
    }
    t.join().expect("waiter thread panicked");
}

register_test!(test_futex2_requeue_to_self);

fn test_futex2_requeue_timeout_race() {
    // Requeue waiters that are about to time out: their timeout-path
    // unregistration must find them on the destination queue.
    const NR_THREADS: usize = 4;

    let f1 = Arc::new(AtomicU32::new(0));
    let f2 = Arc::new(AtomicU32::new(0));

    let threads: Vec<_> = (0..NR_THREADS)
        .map(|_| {
            let f1 = f1.clone();
            thread::spawn(move || {
                let ts = abs_deadline(libc::CLOCK_MONOTONIC, 150);
                let ret = unsafe {
                    futex2_wait(
                        f1.as_ptr() as *const u32,
                        0,
                        MATCH_ANY,
                        FUTEX2_SIZE_U32,
                        &ts,
                        libc::CLOCK_MONOTONIC,
                    )
                };
                if ret != -1 || errno() != libc::ETIMEDOUT {
                    panic!("racing wait: ret {ret}, errno {}", errno());
                }
            })
        })
        .collect();

    thread::sleep(Duration::from_millis(50));

    let pair = [
        FutexWaitv::new(f1.as_ptr() as *const u32, 0, FUTEX2_SIZE_U32),
        FutexWaitv::new(f2.as_ptr() as *const u32, 0, FUTEX2_SIZE_U32),
    ];

    unsafe {
        // Move everyone to f2 without waking anybody.
        let ret = futex2_requeue(pair.as_ptr(), 0, 0, NR_THREADS as i32);
        if ret != 0 {
            panic!("requeue woke {ret}, expected 0");
        }
    }

    // All threads now time out while parked on f2.
    for t in threads {
        t.join().expect("waiter thread panicked");
    }

    unsafe {
        // Every waiter unregistered itself from f2 on timeout.
        let ret = futex2_wake(f2.as_ptr() as *const u32, MATCH_ANY, 64, FUTEX2_SIZE_U32);
        if ret != 0 {
            panic!("{ret} stale waiters left on f2");
        }
    }
}

register_test!(test_futex2_requeue_timeout_race);
