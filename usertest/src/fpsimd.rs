use crate::register_test;
use std::{
    arch::{asm, global_asm},
    sync::{
        Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

#[repr(C, align(16))]
#[derive(Clone, Copy, Default)]
struct FpState {
    regs: [[u64; 2]; 32],
    fpcr: u32,
    fpsr: u32,
}

impl FpState {
    fn sentinel(worker: u32) -> Self {
        let mut state = Self {
            fpcr: (worker & 3) << 22,
            fpsr: worker,
            ..Default::default()
        };
        for (reg, lanes) in state.regs.iter_mut().enumerate() {
            *lanes = [
                (u64::from(worker) << 48) | reg as u64,
                0xfedc_ba98_7654_0000 | (u64::from(worker) << 8) | reg as u64,
            ];
        }
        state
    }

    fn assert_preserved(&self, observed: &Self) {
        for reg in 0..32 {
            assert_eq!(observed.regs[reg], self.regs[reg], "Q{reg} changed");
        }
        assert_eq!(observed.fpcr, self.fpcr, "FPCR changed");
        assert_eq!(observed.fpsr, self.fpsr, "FPSR changed");
    }
}

// No Rust/libc call may occur between loading the sentinels and observing
// them: the calling convention permits callees to clobber SIMD registers.
// Preserve the ABI's callee-saved D8-D15 and floating-point control state.
global_asm!(
    r#"
    .text
    .p2align 2
    .global fpsimd_syscall_probe
    .type fpsimd_syscall_probe, %function
fpsimd_syscall_probe:
    sub sp, sp, #80
    stp d8, d9, [sp, #0]
    stp d10, d11, [sp, #16]
    stp d12, d13, [sp, #32]
    stp d14, d15, [sp, #48]
    mrs x5, fpcr
    mrs x6, fpsr
    stp x5, x6, [sp, #64]
    mov x9, x0
    mov x10, x1
    mov x8, x2
    mov x0, x3
    mov x1, x4
    ldr w2, [x9, #512]
    ldr w3, [x9, #516]
    msr fpcr, x2
    msr fpsr, x3
    ldp q0, q1, [x9, #0]
    ldp q2, q3, [x9, #32]
    ldp q4, q5, [x9, #64]
    ldp q6, q7, [x9, #96]
    ldp q8, q9, [x9, #128]
    ldp q10, q11, [x9, #160]
    ldp q12, q13, [x9, #192]
    ldp q14, q15, [x9, #224]
    ldp q16, q17, [x9, #256]
    ldp q18, q19, [x9, #288]
    ldp q20, q21, [x9, #320]
    ldp q22, q23, [x9, #352]
    ldp q24, q25, [x9, #384]
    ldp q26, q27, [x9, #416]
    ldp q28, q29, [x9, #448]
    ldp q30, q31, [x9, #480]
    mov x2, xzr
    mov x3, xzr
    mov x4, xzr
    svc #0
    stp q0, q1, [x10, #0]
    stp q2, q3, [x10, #32]
    stp q4, q5, [x10, #64]
    stp q6, q7, [x10, #96]
    stp q8, q9, [x10, #128]
    stp q10, q11, [x10, #160]
    stp q12, q13, [x10, #192]
    stp q14, q15, [x10, #224]
    stp q16, q17, [x10, #256]
    stp q18, q19, [x10, #288]
    stp q20, q21, [x10, #320]
    stp q22, q23, [x10, #352]
    stp q24, q25, [x10, #384]
    stp q26, q27, [x10, #416]
    stp q28, q29, [x10, #448]
    stp q30, q31, [x10, #480]
    mrs x2, fpcr
    mrs x3, fpsr
    str w2, [x10, #512]
    str w3, [x10, #516]
    ldp x2, x3, [sp, #64]
    msr fpcr, x2
    msr fpsr, x3
    ldp d8, d9, [sp, #0]
    ldp d10, d11, [sp, #16]
    ldp d12, d13, [sp, #32]
    ldp d14, d15, [sp, #48]
    add sp, sp, #80
    ret
    .size fpsimd_syscall_probe, .-fpsimd_syscall_probe
"#
);

unsafe extern "C" {
    fn fpsimd_syscall_probe(
        input: *const FpState,
        output: *mut FpState,
        syscall: libc::c_long,
        arg0: libc::c_long,
        arg1: libc::c_long,
    ) -> libc::c_long;
}

fn test_fpsimd_context() {
    // More runnable workers than CPUs in the project's four-CPU SMP setup.
    let barrier = Barrier::new(8);
    thread::scope(|scope| {
        for worker in 1..=8 {
            let barrier = &barrier;
            scope.spawn(move || {
                let expected = FpState::sentinel(worker);
                barrier.wait();
                for _ in 0..32 {
                    let mut observed = FpState::default();
                    assert_eq!(
                        unsafe {
                            fpsimd_syscall_probe(
                                &expected,
                                &mut observed,
                                libc::SYS_sched_yield,
                                0,
                                0,
                            )
                        },
                        0
                    );
                    expected.assert_preserved(&observed);
                }
            });
        }
    });
}

register_test!(test_fpsimd_context);

static SIGNAL_SEEN: AtomicBool = AtomicBool::new(false);
static SIGNAL_SP: AtomicUsize = AtomicUsize::new(0);

extern "C" fn clobber_fpsimd(_: libc::c_int) {
    let stack_pointer: usize;
    unsafe {
        asm!(
            "mov {stack_pointer}, sp",
            "movi v0.16b, #0",
            "movi v31.16b, #0",
            "msr fpcr, xzr",
            "msr fpsr, xzr",
            stack_pointer = out(reg) stack_pointer,
            out("v0") _,
            out("v31") _,
            options(nostack),
        );
    }
    SIGNAL_SP.store(stack_pointer, Ordering::Relaxed);
    SIGNAL_SEEN.store(true, Ordering::Relaxed);
}

fn test_fpsimd_signal_return() {
    unsafe {
        // An unaligned stack top also checks that the kernel aligns SP,
        // instead of relying on the caller to provide an aligned buffer.
        let mut stack_mem = vec![0u8; 64 * 1024 + 1];
        let stack = libc::stack_t {
            ss_sp: stack_mem.as_mut_ptr().add(1).cast(),
            ss_flags: 0,
            ss_size: 64 * 1024,
        };
        let mut old_stack: libc::stack_t = std::mem::zeroed();
        assert_eq!(libc::sigaltstack(&stack, &mut old_stack), 0);
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = clobber_fpsimd as *const () as usize;
        assert_eq!(libc::sigemptyset(&mut action.sa_mask), 0);
        let expected = FpState::sentinel(1);
        for flags in [0, libc::SA_ONSTACK] {
            action.sa_flags = flags;
            assert_eq!(
                libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()),
                0
            );
            let mut observed = FpState::default();
            SIGNAL_SEEN.store(false, Ordering::Relaxed);
            assert_eq!(
                fpsimd_syscall_probe(
                    &expected,
                    &mut observed,
                    libc::SYS_kill,
                    libc::getpid().into(),
                    libc::SIGUSR1.into(),
                ),
                0
            );
            assert!(SIGNAL_SEEN.load(Ordering::Relaxed));
            let sp = SIGNAL_SP.load(Ordering::Relaxed);
            assert_eq!(sp % 16, 0, "signal handler SP is not aligned");
            if flags == libc::SA_ONSTACK {
                let start = stack.ss_sp as usize;
                assert!((start..start + stack.ss_size).contains(&sp));
            }
            expected.assert_preserved(&observed);
        }
        let disabled = libc::stack_t {
            ss_sp: std::ptr::null_mut(),
            ss_flags: libc::SS_DISABLE,
            ss_size: 0,
        };
        assert_eq!(libc::sigaltstack(&disabled, std::ptr::null_mut()), 0);
        let mut queried: libc::stack_t = std::mem::zeroed();
        assert_eq!(libc::sigaltstack(std::ptr::null(), &mut queried), 0);
        assert_eq!(queried.ss_flags, libc::SS_DISABLE);
        assert_eq!(libc::sigaltstack(&old_stack, std::ptr::null_mut()), 0);
    }
}

register_test!(test_fpsimd_signal_return);

fn test_fpsimd_clone() {
    // The test runner forks before entering each test, so no other threads
    // exist here. Use raw clone(SIGCHLD, NULL, NULL, NULL, 0), i.e. fork,
    // without a libc call clobbering the sentinels before the syscall.
    unsafe {
        let expected = FpState::sentinel(3);
        let mut observed = FpState::default();
        let pid = fpsimd_syscall_probe(
            &expected,
            &mut observed,
            libc::SYS_clone,
            libc::SIGCHLD.into(),
            0,
        );
        assert!(pid >= 0, "clone returned {pid}");
        if pid == 0 {
            expected.assert_preserved(&observed);
            libc::_exit(0);
        }
        let mut status = 0;
        loop {
            let waited = libc::waitpid(pid as libc::pid_t, &mut status, 0);
            if waited == pid as libc::pid_t {
                break;
            }
            assert_eq!(waited, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EINTR)
            );
        }
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
        expected.assert_preserved(&observed);
    }
}

register_test!(test_fpsimd_clone);
