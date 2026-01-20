use crate::memory::uaccess::UserCopyable;

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct X86_64PtraceGPRegs {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub orig_rax: u64,
    pub rip: u64,
    pub cs: u64,
    pub eflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

unsafe impl UserCopyable for X86_64PtraceGPRegs {}

impl From<&super::exceptions::ExceptionState> for X86_64PtraceGPRegs {
    fn from(_s: &super::exceptions::ExceptionState) -> Self {
        // Minimal conversion; fields may be populated when exception state is defined.
        X86_64PtraceGPRegs::default()
    }
}
