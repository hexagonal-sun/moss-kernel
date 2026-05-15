//! Aarch64 stack trace implementation

use crate::StackTrace;
use core::arch::asm;

/// Aarch64 stack trace implementation
pub struct StackTraceImpl {
    /// Frame pointer
    pub fp: usize,
    /// PC ptr
    pub pc_ptr: *const usize,
}

impl StackTrace for StackTraceImpl {
    #[inline(always)]
    unsafe fn start() -> Option<Self> {
        unsafe {
            let fp: usize;
            asm!("mov {}, fp", out(reg) fp);
            let pc_ptr = fp.checked_add(size_of::<usize>())?;
            Some(Self {
                fp,
                pc_ptr: pc_ptr as *const usize,
            })
        }
    }

    unsafe fn next(self) -> Option<Self> {
        unsafe {
            let fp = *(self.fp as *const usize);
            let pc_ptr = fp.checked_add(size_of::<usize>())?;
            Some(Self {
                fp,
                pc_ptr: pc_ptr as *const usize,
            })
        }
    }

    fn fp(&self) -> usize {
        self.fp
    }

    fn pc_ptr(&self) -> *const usize {
        self.pc_ptr
    }
}
