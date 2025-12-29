//! A module for sending messages between CPUs, utilising IPIs.

use super::{
    ClaimedInterrupt, InterruptConfig, InterruptDescriptor, InterruptHandler, get_interrupt_root,
};
use crate::process::Task;
use crate::{
    arch::ArchImpl,
    drivers::Driver,
    kernel::kpipe::KBuf,
    sched,
    sync::{OnceLock, SpinLock},
};
use alloc::{sync::Arc, vec::Vec};
use libkernel::{
    CpuOps,
    error::{KernelError, Result},
};
use log::{info, warn};

#[derive(Clone)]
pub enum Message {
    PutTask(Arc<Task>),
    Ping(u32),
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
        let message = CPU_MESSENGER
            .get()
            .unwrap()
            .mailboxes
            .lock_save_irq()
            .get(ArchImpl::id())
            .unwrap()
            .try_pop();

        match message {
            Some(Message::PutTask(task)) => sched::insert_task(task),
            Some(Message::Ping(cpu_id)) => {
                info!("CPU {} recieved ping from CPU {}", ArchImpl::id(), cpu_id)
            }
            None => warn!("Spurious CPU IPI"),
        }
    }
}

const MESSENGER_IRQ_DESC: InterruptDescriptor = InterruptDescriptor::Ipi(0);

pub fn cpu_messenger_init(num_cpus: usize) {
    let cpu_messenger = get_interrupt_root()
        .expect("Interrupt root should be avilable")
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

pub fn message_cpu(cpu_id: usize, message: Message) -> Result<()> {
    let messenger = CPU_MESSENGER.get().ok_or(KernelError::InvalidValue)?;
    let irq = get_interrupt_root().ok_or(KernelError::InvalidValue)?;

    messenger
        .mailboxes
        .lock_save_irq()
        .get(cpu_id)
        .ok_or(KernelError::InvalidValue)?
        .try_push(message)
        .map_err(|_| KernelError::NoMemory)?;

    irq.raise_ipi(cpu_id);

    Ok(())
}

static CPU_MESSENGER: OnceLock<Arc<CpuMessenger>> = OnceLock::new();
