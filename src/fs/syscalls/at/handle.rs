use libkernel::error::{KernelError, Result};

pub fn sys_name_to_handle_at() -> Result<usize> {
    Err(KernelError::OpNotSupported)
}
