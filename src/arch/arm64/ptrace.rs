use crate::memory::uaccess::UserCopyable;

use super::exceptions::ExceptionState;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Arm64PtraceGPRegs {
    pub x: [u64; 31], // x0-x30
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
}

unsafe impl UserCopyable for Arm64PtraceGPRegs {}

impl From<&ExceptionState> for Arm64PtraceGPRegs {
    fn from(value: &ExceptionState) -> Self {
        Self {
            x: value.x,
            sp: value.sp_el0,
            pc: value.elr_el1,
            pstate: value.spsr_el1,
        }
    }
}
