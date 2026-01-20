pub mod syscall;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ExceptionState {
    pub regs: [u64; 16], // generic register storage placeholder
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
}

impl ExceptionState {
    pub fn syscall_nr(&self) -> u64 {
        // On x86_64, syscall number is in rax
        self.regs[0]
    }

    pub fn arg(&self, i: usize) -> u64 {
        match i {
            0 => self.regs[1], // rdi
            1 => self.regs[2], // rsi
            2 => self.regs[3], // rdx
            3 => self.regs[4], // r10
            4 => self.regs[5], // r8
            5 => self.regs[6], // r9
            _ => 0,
        }
    }
}
