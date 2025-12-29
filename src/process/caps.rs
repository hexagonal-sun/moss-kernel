use crate::{
    current_task,
    memory::uaccess::{
        UserCopyable, copy_from_user, copy_obj_array_from_user, copy_objs_to_user, copy_to_user,
    },
    process::TASK_LIST,
};
use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
    proc::caps::{Capabilities, CapabilitiesFlags},
};

const LINUX_CAPABILITY_VERSION_1: u32 = 0x19980330;
const LINUX_CAPABILITY_VERSION_3: u32 = 0x20080522;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct CapUserHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct CapUserData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

impl CapUserData {
    fn from_caps(caps: Capabilities) -> [Self; 2] {
        [
            Self {
                effective: caps.effective().bits() as u32,
                permitted: caps.permitted().bits() as u32,
                inheritable: caps.inheritable().bits() as u32,
            },
            Self {
                effective: (caps.effective().bits() >> 32) as u32,
                permitted: (caps.permitted().bits() >> 32) as u32,
                inheritable: (caps.inheritable().bits() >> 32) as u32,
            },
        ]
    }
}

unsafe impl UserCopyable for CapUserHeader {}
unsafe impl UserCopyable for CapUserData {}

pub async fn sys_capget(hdrp: TUA<CapUserHeader>, datap: TUA<CapUserData>) -> Result<usize> {
    let mut header = copy_from_user(hdrp).await?;

    let task = if header.pid == 0 {
        current_task()
    } else {
        TASK_LIST
            .lock_save_irq()
            .iter()
            .find(|task| task.0.tgid.value() == header.pid as _)
            .and_then(|task| task.1.upgrade())
            .ok_or(KernelError::NoProcess)?
    };
    match header.version {
        LINUX_CAPABILITY_VERSION_1 => {
            let caps = task.creds.lock_save_irq().caps();
            let caps = CapUserData::from_caps(caps);
            copy_to_user(datap, caps[0]).await?;
        }
        LINUX_CAPABILITY_VERSION_3 => {
            let caps = task.creds.lock_save_irq().caps();
            let caps = CapUserData::from_caps(caps);
            copy_objs_to_user(&caps, datap).await?;
        }
        _ => {
            header.version = LINUX_CAPABILITY_VERSION_3;
            copy_to_user(hdrp, header).await?;
            return Err(KernelError::InvalidValue);
        }
    }
    Ok(0)
}

pub async fn sys_capset(hdrp: TUA<CapUserHeader>, datap: TUA<CapUserData>) -> Result<usize> {
    let mut header = copy_from_user(hdrp).await?;

    let caller_caps = current_task().creds.lock_save_irq().caps();
    let task = if header.pid == 0 {
        current_task()
    } else {
        caller_caps.check_capable(CapabilitiesFlags::CAP_SETPCAP)?;
        TASK_LIST
            .lock_save_irq()
            .iter()
            .find(|task| task.0.tgid.value() == header.pid as _)
            .and_then(|task| task.1.upgrade())
            .ok_or(KernelError::NoProcess)?
    };

    let (effective, permitted, inheritable) = match header.version {
        LINUX_CAPABILITY_VERSION_1 => {
            let datap = copy_from_user(datap).await?;
            let effective = CapabilitiesFlags::from_bits_retain(datap.effective as _);
            let permitted = CapabilitiesFlags::from_bits_retain(datap.permitted as _);
            let inheritable = CapabilitiesFlags::from_bits_retain(datap.inheritable as _);

            (effective, permitted, inheritable)
        }
        LINUX_CAPABILITY_VERSION_3 => {
            let datap: [CapUserData; 2] = copy_obj_array_from_user(datap, 2)
                .await?
                .try_into()
                .map_err(|_| KernelError::InvalidValue)?;
            let effective = CapabilitiesFlags::from_bits_retain(
                ((datap[1].effective as u64) << 32) | datap[0].effective as u64,
            );
            let permitted = CapabilitiesFlags::from_bits_retain(
                ((datap[1].permitted as u64) << 32) | datap[0].permitted as u64,
            );
            let inheritable = CapabilitiesFlags::from_bits_retain(
                ((datap[1].inheritable as u64) << 32) | datap[0].inheritable as u64,
            );

            (effective, permitted, inheritable)
        }
        _ => {
            header.version = LINUX_CAPABILITY_VERSION_3;
            copy_to_user(hdrp, header).await?;
            return Err(KernelError::InvalidValue);
        }
    };

    let mut creds = task.creds.lock_save_irq();
    creds
        .caps
        .set_public(caller_caps, effective, permitted, inheritable)?;

    Ok(0)
}
