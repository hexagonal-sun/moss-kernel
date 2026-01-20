use crate::process::Task;
use alloc::sync::Arc;
use libkernel::UserAddressSpace;

pub mod idle;

pub fn context_switch(new: Arc<Task>) {
    new.vm
        .lock_save_irq()
        .mm_mut()
        .address_space_mut()
        .activate();
}
