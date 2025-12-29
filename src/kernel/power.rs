use crate::{ArchImpl, arch::Arch, sched::current_task};
use libkernel::{
    error::{KernelError, Result},
    proc::caps::CapabilitiesFlags,
};

pub async fn sys_reboot(magic: u32, magic2: u32, op: u32, _arg: usize) -> Result<usize> {
    current_task()
        .creds
        .lock_save_irq()
        .caps()
        .check_capable(CapabilitiesFlags::CAP_SYS_BOOT)?;

    const LINUX_REBOOT_MAGIC1: u32 = 0xfee1_dead;
    const LINUX_REBOOT_MAGIC2: u32 = 672274793;
    const LINUX_REBOOT_MAGIC2A: u32 = 852072454;
    const LINUX_REBOOT_MAGIC2B: u32 = 369367448;
    const LINUX_REBOOT_MAGIC2C: u32 = 537993216;
    // const LINUX_REBOOT_CMD_CAD_OFF: u32 = 0x0000_0000;
    // const LINUX_REBOOT_CMD_CAD_ON: u32 = 0x89ab_cdef;
    // const LINUX_REBOOT_CMD_HALT: u32 = 0xcdef_0123;
    // const LINUX_REBOOT_CMD_KEXEC: u32 = 0x4558_4543;
    const LINUX_REBOOT_CMD_POWER_OFF: u32 = 0x4321_fedc;
    const LINUX_REBOOT_CMD_RESTART: u32 = 0x0123_4567;
    // const LINUX_REBOOT_CMD_RESTART2: u32 = 0xa1b2_c3d4;
    // const LINUX_REBOOT_CMD_SW_SUSPEND: u32 = 0xd000_fce1;
    if magic != LINUX_REBOOT_MAGIC1
        || (magic2 != LINUX_REBOOT_MAGIC2
            && magic2 != LINUX_REBOOT_MAGIC2A
            && magic2 != LINUX_REBOOT_MAGIC2B
            && magic2 != LINUX_REBOOT_MAGIC2C)
    {
        return Err(KernelError::InvalidValue);
    }
    match op {
        LINUX_REBOOT_CMD_POWER_OFF => {
            // User is supposed to sync first.
            ArchImpl::power_off()
        }
        LINUX_REBOOT_CMD_RESTART => ArchImpl::restart(),
        // TODO: Implement other reboot operations.
        _ => Err(KernelError::InvalidValue),
    }
}
