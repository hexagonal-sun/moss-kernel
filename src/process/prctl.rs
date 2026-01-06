use libkernel::error::{KernelError, Result};

const PR_CAPBSET_READ: i32 = 23;
const CAP_MAX: usize = 40;

fn pr_read_capset(what: usize) -> Result<usize> {
    // Validate the argument
    if what > CAP_MAX {
        return Err(KernelError::InvalidValue);
    }

    // Assume we have *all* the capabilities.
    Ok(1)
}

pub fn sys_prctl(op: i32, arg1: usize) -> Result<usize> {
    match op {
        PR_CAPBSET_READ => pr_read_capset(arg1),
        _ => todo!("prctl op: {}", op),
    }
}
