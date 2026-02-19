use crate::arch::{Arch, ArchImpl};
use crate::console::write_fmt;
use crate::drivers::timer::uptime;
use alloc::format;
use core::fmt::Display;

const TEXT_GREEN: &str = "\x1b[32m";
const TEXT_RED: &str = "\x1b[31m";
const TEXT_RESET: &str = "\x1b[0m";

pub enum TestResult {
    Ok,
    Failed,
    Skipped,
}

impl Display for TestResult {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TestResult::Ok => write!(f, "{TEXT_GREEN}ok{TEXT_RESET}"),
            TestResult::Failed => write!(f, "{TEXT_GREEN}failed{TEXT_RESET}"),
            TestResult::Skipped => write!(f, "skipped"),
        }
    }
}

pub struct Test {
    pub name: &'static str,
    pub test_fn: fn() -> TestResult,
}

#[cfg(test)]
pub fn test_runner(tests: &[&Test]) {
    write_fmt(format_args!("\nrunning {} tests\n", tests.len())).unwrap();
    let mut passed = 0;
    let mut failed = 0;
    let mut ignored = 0;
    let start = uptime();
    for test in tests {
        let result = (test.test_fn)();
        match result {
            TestResult::Ok => passed += 1,
            TestResult::Failed => failed += 1,
            TestResult::Skipped => ignored += 1,
        }
        write_fmt(format_args!("test {} ... {}\n", test.name, result)).unwrap();
    }
    let duration = uptime() - start;
    write_fmt(format_args!(
        "\ntest result: {}. {passed} passed; {failed} failed; {ignored} ignored; finished in {}.{}s\n",
        if failed == 0 {
            format!("{TEXT_GREEN}ok{TEXT_RESET}")
        } else {
            format!("{TEXT_RED}FAILED{TEXT_RESET}")
        },
        duration.as_secs(),
        duration.subsec_millis() / 10
    ))
    .unwrap();
    ArchImpl::power_off();
}

pub fn panic_noop(_: *mut u8, _: *mut u8) {}

#[macro_export]
macro_rules! ktest {
    ($name:ident, fn $fn_name:ident() $body:block) => {
        #[cfg(test)]
        fn $fn_name(_: *mut u8) {
            $body
        }

        paste::paste! {
            #[cfg(test)]
            #[test_case]
            static [<__TEST_ $name>]: crate::testing::Test = crate::testing::Test {
                name: concat!(module_path!(), "::", stringify!($name)),
                test_fn: || {
                    let result = unsafe {
                        core::intrinsics::catch_unwind(
                            $fn_name as fn(*mut u8),
                            core::ptr::null_mut(),
                            crate::testing::panic_noop,
                        )
                    };
                    match result {
                        0 => crate::testing::TestResult::Ok,
                        1 => crate::testing::TestResult::Failed,
                        _ => unreachable!("catch_unwind should only return 0 or 1"),
                    }
                },
            };
        }
    };
    (fn $name:ident() $body:block) => {
        crate::ktest!($name, fn $name() $body);
    };
    (async fn $name:ident() $body:block) => {
        async fn $name() {
            $body
        }

        paste::paste! {
            crate::ktest! {
                $name,
                fn [<__sync_ $name>]() {
                    let mut fut = alloc::boxed::Box::pin($name());
                    let desc = crate::process::TaskDescriptor::from_tgid_tid(crate::process::thread_group::Tgid(0), crate::process::Tid(0));
                    let waker = crate::sched::waker::create_waker(desc);
                    let mut ctx = core::task::Context::from_waker(&waker);
                    loop {
                        match fut.as_mut().poll(&mut ctx) {
                            core::task::Poll::Ready(()) => break,
                            _ => {},
                        }
                    }
                }
            }
        }

    }
}
