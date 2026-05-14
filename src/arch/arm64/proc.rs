use crate::process::Task;
use alloc::sync::Arc;

pub mod idle;
pub mod signal;
pub mod vdso;

pub fn context_switch(new: Arc<Task>) {
    new.vm.activate();
}
