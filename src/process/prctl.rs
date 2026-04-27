use crate::memory::uaccess::copy_to_user_slice;
use crate::memory::uaccess::cstr::UserCStr;
use crate::process::Comm;
use crate::sched::syscall_ctx::ProcessCtx;
use bitflags::Flags;
use core::ffi::c_char;
use libkernel::error::{KernelError, Result};
use libkernel::memory::address::TUA;
use libkernel::proc::caps::CapabilitiesFlags;

const PR_CAPBSET_READ: i32 = 23;
const PR_CAPBSET_DROP: i32 = 24;
const PR_SET_NAME: i32 = 15;
const PR_GET_NAME: i32 = 16;
const PR_GET_SECUREBITS: i32 = 27;
const PR_SET_MM: i32 = 35;
const PR_GET_NO_NEW_PRIVS: i32 = 39;
const PR_CAP_AMBIENT: i32 = 47;

#[derive(Debug)]
enum SetMMOp {
    StartCode = 1,
    EndCode = 2,
    StartData = 3,
    EndData = 4,
    StartStack = 5,
    StartBrk = 6,
    Brk = 7,
    ArgStart = 8,
    ArgEnd = 9,
    EnvStart = 10,
    EnvEnd = 11,
    AUX = 12,
    ExeFile = 13,
    Map = 14,
    MapSize = 15,
}

impl TryFrom<u64> for SetMMOp {
    type Error = KernelError;

    fn try_from(value: u64) -> Result<Self> {
        match value {
            1 => Ok(SetMMOp::StartCode),
            2 => Ok(SetMMOp::EndCode),
            3 => Ok(SetMMOp::StartData),
            4 => Ok(SetMMOp::EndData),
            5 => Ok(SetMMOp::StartStack),
            6 => Ok(SetMMOp::StartBrk),
            7 => Ok(SetMMOp::Brk),
            8 => Ok(SetMMOp::ArgStart),
            9 => Ok(SetMMOp::ArgEnd),
            10 => Ok(SetMMOp::EnvStart),
            11 => Ok(SetMMOp::EnvEnd),
            12 => Ok(SetMMOp::AUX),
            13 => Ok(SetMMOp::ExeFile),
            14 => Ok(SetMMOp::Map),
            15 => Ok(SetMMOp::MapSize),
            _ => Err(KernelError::InvalidValue),
        }
    }
}

#[derive(Debug)]
enum AmbientCapOp {
    IsSet = 1,
    Raise = 2,
    Lower = 3,
    ClearAll = 4,
}

impl TryFrom<u64> for AmbientCapOp {
    type Error = KernelError;

    fn try_from(value: u64) -> Result<Self> {
        match value {
            1 => Ok(AmbientCapOp::IsSet),
            2 => Ok(AmbientCapOp::Raise),
            3 => Ok(AmbientCapOp::Lower),
            4 => Ok(AmbientCapOp::ClearAll),
            _ => Err(KernelError::InvalidValue),
        }
    }
}

fn pr_read_capbset(ctx: &ProcessCtx, what: usize) -> Result<usize> {
    let what = CapabilitiesFlags::from_bits(1u64 << what).ok_or(KernelError::InvalidValue)?;
    let task = ctx.shared();
    let creds = task.creds.lock_save_irq();
    Ok(creds.caps.bounding().contains(what) as _)
}

async fn pr_drop_capbset(ctx: &ProcessCtx, what: usize) -> Result<usize> {
    let what = CapabilitiesFlags::from_bits(1u64 << what).ok_or(KernelError::InvalidValue)?;
    let task = ctx.shared();
    let mut creds = task.creds.lock_save_irq();
    creds.caps.bounding_mut().remove(what);
    Ok(0)
}

async fn pr_get_name(ctx: &ProcessCtx, str: TUA<c_char>) -> Result<usize> {
    let task = ctx.shared();
    let comm = task.comm.lock_save_irq().0;
    copy_to_user_slice(&comm, str.to_untyped()).await?;
    Ok(0)
}

async fn pr_set_name(ctx: &ProcessCtx, str: TUA<c_char>) -> Result<usize> {
    let task = ctx.shared();
    let mut buf: [u8; 64] = [0; 64];
    let name = UserCStr::from_ptr(str).copy_from_user(&mut buf).await?;
    *task.comm.lock_save_irq() = Comm::new(name);
    Ok(0)
}

async fn pr_set_mm(ctx: &ProcessCtx, op: u64) -> Result<usize> {
    let op = SetMMOp::try_from(op)?;
    let task = ctx.shared();
    match op {
        op => todo!("unsupported set_mm op: {op:?}")
    }
}

async fn pr_cap_ambient(ctx: &ProcessCtx, op: u64, arg1: u64) -> Result<usize> {
    let op = AmbientCapOp::try_from(op)?;
    let task = ctx.shared();
    match op {
        AmbientCapOp::ClearAll => {
            let mut creds = task.creds.lock_save_irq();
            creds.caps.ambient_mut().clear();
            Ok(0)
        }
        AmbientCapOp::IsSet => {
            let what =
                CapabilitiesFlags::from_bits(1u64 << arg1).ok_or(KernelError::InvalidValue)?;
            let creds = task.creds.lock_save_irq();
            let is_set = creds.caps.ambient().contains(what);
            Ok(is_set as _)
        }
        AmbientCapOp::Lower => {
            let what =
                CapabilitiesFlags::from_bits(1u64 << arg1).ok_or(KernelError::InvalidValue)?;
            let mut creds = task.creds.lock_save_irq();
            creds.caps.ambient_mut().remove(what);
            Ok(0)
        }
        AmbientCapOp::Raise => {
            let what =
                CapabilitiesFlags::from_bits(1u64 << arg1).ok_or(KernelError::InvalidValue)?;
            let mut creds = task.creds.lock_save_irq();
            if !creds.caps.inheritable().contains(what) {
                return Err(KernelError::NotPermitted);
            }
            if !creds.caps.bounding().contains(what) {
                return Err(KernelError::NotPermitted);
            }
            creds.caps.ambient_mut().insert(what);
            Ok(0)
        }
    }
}

pub async fn sys_prctl(ctx: &ProcessCtx, op: i32, arg1: u64, arg2: u64) -> Result<usize> {
    match op {
        PR_SET_NAME => pr_set_name(ctx, TUA::from_value(arg1 as usize)).await,
        PR_GET_NAME => pr_get_name(ctx, TUA::from_value(arg1 as usize)).await,
        PR_CAPBSET_READ => pr_read_capbset(ctx, arg1 as usize),
        PR_CAPBSET_DROP => pr_drop_capbset(ctx, arg1 as usize).await,
        PR_GET_SECUREBITS => Ok(0),
        PR_SET_MM => pr_set_mm(ctx, arg1).await,
        PR_GET_NO_NEW_PRIVS => Ok(0),
        PR_CAP_AMBIENT => pr_cap_ambient(ctx, arg1, arg2).await,
        _ => todo!("prctl op: {}", op),
    }
}
