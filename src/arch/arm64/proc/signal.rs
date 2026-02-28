use super::vdso::VDSO_BASE;
use crate::{
    arch::arm64::exceptions::ExceptionState,
    memory::uaccess::{UserCopyable, copy_from_user, copy_to_user},
    process::thread_group::signal::{
        SigId, ksigaction::UserspaceSigAction, sigaction::SigActionFlags,
    },
    sched::current::current_task,
};
use libkernel::{
    error::Result,
    memory::{
        PAGE_SIZE,
        address::{TUA, UA},
    },
};

#[repr(C)]
#[derive(Clone, Copy)]
struct RtSigFrame {
    uctx: ExceptionState,
    alt_stack_prev_addr: UA,
}

// SAFETY: The signal frame that's copied to user-space only contains
// information regarding this task's context and is made up of PoDs.
unsafe impl UserCopyable for RtSigFrame {}

pub async fn do_signal(id: SigId, sa: UserspaceSigAction) -> Result<ExceptionState> {
    let task = current_task();
    let mut signal = task.process.signals.lock_save_irq();

    let saved_state = *task.ctx.user();
    let mut new_state = saved_state;
    let mut frame = RtSigFrame {
        uctx: saved_state,
        alt_stack_prev_addr: UA::null(),
    };

    // Use the provided restorer trampoline, or the one provided by the VDSO if
    // not.
    let restorer = sa
        .restorer
        .map(|x| x.value())
        .unwrap_or_else(|| VDSO_BASE.value());

    let addr: TUA<RtSigFrame> = if sa.flags.contains(SigActionFlags::SA_ONSTACK)
        && let Some(alt_stack) = signal.alt_stack.as_mut()
        && let Some(alloc) = alt_stack.alloc_alt_stack::<RtSigFrame>()
    {
        frame.alt_stack_prev_addr = alloc.old_ptr;
        alloc.data_ptr.cast()
    } else {
        TUA::from_value(new_state.sp_el0 as _)
            .sub_objs(1)
            .align(PAGE_SIZE)
    };

    drop(signal);
    drop(task);

    copy_to_user(addr, frame).await?;

    new_state.sp_el0 = addr.value() as _;
    new_state.elr_el1 = sa.action.value() as _;
    new_state.x[30] = restorer as _;
    new_state.x[0] = id.user_id();

    Ok(new_state)
}

pub async fn do_signal_return() -> Result<ExceptionState> {
    let task = current_task();

    let sig_frame_addr: TUA<RtSigFrame> = TUA::from_value(task.ctx.user().sp_el0 as _);

    let sig_frame = copy_from_user(sig_frame_addr).await?;

    if !sig_frame.alt_stack_prev_addr.is_null() {
        task.process
            .signals
            .lock_save_irq()
            .alt_stack
            .as_mut()
            .expect("Alt stack disappeared during use")
            .restore_alt_stack(sig_frame.alt_stack_prev_addr);
    }

    Ok(sig_frame.uctx)
}
