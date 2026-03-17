use crate::sched::syscall_ctx::ProcessCtx;
use libkernel::UserAddressSpace;
use libkernel::error::{KernelError, Result};
use libkernel::memory::address::{TUA, VA};

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub enum FutexKey {
    Private { pid: u32, addr: usize },
    Shared { frame: usize, offset: usize },
}

impl FutexKey {
    pub fn new_private(ctx: &ProcessCtx, uaddr: TUA<u32>) -> Self {
        let pid = ctx.shared().process.tgid.value();

        Self::Private {
            pid,
            addr: uaddr.value(),
        }
    }

    pub fn new_shared(ctx: &ProcessCtx, uaddr: TUA<u32>) -> Result<Self> {
        let pg_info = ctx
            .shared()
            .vm
            .lock_save_irq()
            .mm_mut()
            .address_space_mut()
            .translate(VA::from_value(uaddr.value()))
            .ok_or(KernelError::Fault)?;

        Ok(Self::Shared {
            frame: pg_info.pfn.value(),
            offset: uaddr.page_offset(),
        })
    }
}
