use core::convert::Infallible;

use libkernel::memory::address::VA;

use crate::sched::current::current_task;

/// Handles the `brk` system call.
///
/// This function emulates the behavior of the Linux `brk` syscall.
///
/// # Arguments
/// * `addr`: The virtual address for the new program break.
///
/// # Returns
/// A `Result` containing the new program break address as a `usize`. Note that
/// according to the `brk(2)` man page, the syscall itself doesn't "fail" in the
/// traditional sense. It always returns a memory address, hence the Infallible
/// error type.
/// - If `addr` is 0, it returns the current break.
/// - On a successful resize, it returns the new break.
/// - On a failed resize, it returns the current, unchanged break.
pub async fn sys_brk(addr: VA) -> Result<usize, Infallible> {
    let task = current_task();
    let mut vm = task.vm.lock_save_irq();

    // The query case `brk(0)` is special and is handled separately from modifications.
    if addr.is_null() {
        let current_brk_val = vm.current_brk().value();
        return Ok(current_brk_val);
    }

    // For non-null addresses, attempt to resize the break.
    let resize_result = vm.resize_brk(addr);

    match resize_result {
        // Success: The break was resized. The function returns the new address.
        Ok(new_brk) => Ok(new_brk.value()),
        // Failure: The resize was invalid (e.g., collision, shrink below start).
        // The contract is to return the current, unchanged break address.
        Err(_) => {
            let current_brk_val = vm.current_brk().value();
            Ok(current_brk_val)
        }
    }
}
