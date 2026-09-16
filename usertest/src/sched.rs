use crate::register_test;
use std::{
    cell::Cell,
    hint::black_box,
    sync::{Arc, Barrier},
    thread,
};

thread_local! {
    static YIELD_MARKER: Cell<usize> = const { Cell::new(0) };
}

fn test_sched_yield_context() {
    // Every registered test runs in a child process. Turn a lost-thread/join
    // hang into a failed test, even if the corrupted thread cannot panic.
    unsafe {
        assert_ne!(libc::signal(libc::SIGALRM, libc::SIG_DFL), libc::SIG_ERR);
        let timer = libc::itimerval {
            it_interval: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            it_value: libc::timeval {
                tv_sec: 60,
                tv_usec: 0,
            },
        };
        assert_eq!(
            libc::setitimer(libc::ITIMER_REAL, &timer, std::ptr::null_mut()),
            0
        );
    }

    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = (1..=2)
        .map(|marker| {
            let barrier = barrier.clone();
            thread::spawn(move || {
                let tid = unsafe { libc::syscall(libc::SYS_gettid) };
                let stack_marker = [marker; 16];
                let stack_address = black_box(stack_marker.as_ptr());
                YIELD_MARKER.set(marker);
                barrier.wait();

                for _ in 0..10_000 {
                    assert_eq!(unsafe { libc::sched_yield() }, 0);
                    assert_eq!(unsafe { libc::syscall(libc::SYS_gettid) }, tid);
                    assert_eq!(YIELD_MARKER.get(), marker);
                    assert_eq!(black_box(stack_marker.as_ptr()), stack_address);
                    assert_eq!(black_box(stack_marker), [marker; 16]);
                }
                marker
            })
        })
        .collect();

    for (index, thread) in threads.into_iter().enumerate() {
        assert_eq!(thread.join().unwrap(), index + 1);
    }

    unsafe {
        let timer: libc::itimerval = std::mem::zeroed();
        assert_eq!(
            libc::setitimer(libc::ITIMER_REAL, &timer, std::ptr::null_mut()),
            0
        );
    }
}

register_test!(test_sched_yield_context);
