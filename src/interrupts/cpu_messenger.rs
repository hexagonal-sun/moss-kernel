//! A module for sending messages between CPUs, utilising IPIs.

use core::task::Waker;

use super::{
    ClaimedInterrupt, InterruptConfig, InterruptDescriptor, InterruptHandler, get_interrupt_root,
};
use crate::kernel::cpu_id::CpuId;
use crate::process::owned::OwnedTask;
use crate::{
    arch::ArchImpl,
    drivers::Driver,
    kernel::kpipe::KBuf,
    sched,
    sync::{OnceLock, SpinLock},
};
use alloc::boxed::Box;
use alloc::{sync::Arc, vec::Vec};
use libkernel::{
    CpuOps,
    error::{KernelError, Result},
};
use log::warn;

pub enum Message {
    PutTask(Box<OwnedTask>),
    WakeupTask(Waker),
}

struct CpuMessenger {
    mailboxes: SpinLock<Vec<KBuf<Message>>>,
    _irq: ClaimedInterrupt,
}

impl Driver for CpuMessenger {
    fn name(&self) -> &'static str {
        "CPU Messenger"
    }
}

impl InterruptHandler for CpuMessenger {
    fn handle_irq(&self, _desc: InterruptDescriptor) {
        while let Some(message) = CPU_MESSENGER
            .get()
            .unwrap()
            .mailboxes
            .lock_save_irq()
            .get(ArchImpl::id())
            .unwrap()
            .try_pop()
        {
            match message {
                Message::PutTask(task) => sched::insert_task(task),
                Message::WakeupTask(waker) => waker.wake(),
            }
        }
    }
}

const MESSENGER_IRQ_DESC: InterruptDescriptor = InterruptDescriptor::Ipi(0);

pub fn cpu_messenger_init(num_cpus: usize) {
    let cpu_messenger = get_interrupt_root()
        .expect("Interrupt root should be available")
        .claim_interrupt(
            InterruptConfig {
                descriptor: MESSENGER_IRQ_DESC,
                trigger: super::TriggerMode::EdgeRising,
            },
            |irq| {
                let mut mailboxes = Vec::new();

                for _ in 0..num_cpus {
                    mailboxes.push(KBuf::new().expect("Could not allocate CPU mailbox"));
                }

                CpuMessenger {
                    mailboxes: SpinLock::new(mailboxes),
                    _irq: irq,
                }
            },
        )
        .expect("Could not claim messenger IRQ");

    if CPU_MESSENGER.set(cpu_messenger).is_err() {
        warn!("Attempted to initialise cpu messenger multiple times");
    }
}

pub fn message_cpu(cpu_id: CpuId, message: Message) -> Result<()> {
    let messenger = CPU_MESSENGER.get().ok_or(KernelError::InvalidValue)?;
    let irq = get_interrupt_root().ok_or(KernelError::InvalidValue)?;

    messenger
        .mailboxes
        .lock_save_irq()
        .get(cpu_id.value())
        .ok_or(KernelError::InvalidValue)?
        .try_push(message)
        .map_err(|_| KernelError::NoMemory)?;

    irq.raise_ipi(cpu_id.value());

    Ok(())
}

static CPU_MESSENGER: OnceLock<Arc<CpuMessenger>> = OnceLock::new();
