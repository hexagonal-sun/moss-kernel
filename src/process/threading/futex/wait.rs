use alloc::{boxed::Box, sync::Arc};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
    sync::waker_set::WakerSet,
};

use crate::{
    memory::uaccess::{copy_from_user, try_copy_from_user},
    sync::SpinLock,
};

enum WaitState {
    Init,
    HandlingFault(Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>),
    Waiting { token: u64 },
}

pub struct FutexWait {
    uaddr: TUA<u32>,
    val: u32,
    queue: Arc<SpinLock<WakerSet<u32>>>,
    bitmask: u32,
    state: WaitState,
}

impl FutexWait {
    pub fn new(
        uaddr: TUA<u32>,
        val: u32,
        bitmask: u32,
        queue: Arc<SpinLock<WakerSet<u32>>>,
    ) -> Self {
        Self {
            uaddr,
            val,
            bitmask,
            queue,
            state: WaitState::Init,
        }
    }
}

impl Drop for FutexWait {
    fn drop(&mut self) {
        if let WaitState::Waiting { token } = &self.state {
            self.queue.lock_save_irq().remove(*token);
        }
    }
}

impl Future for FutexWait {
    type Output = Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safe because we are inside Pin and not moving out of Self
        let this = unsafe { self.as_mut().get_unchecked_mut() };

        loop {
            match &mut this.state {
                WaitState::Init => {
                    let mut wait_queue = this.queue.lock_save_irq();

                    match try_copy_from_user(this.uaddr) {
                        Ok(val) => {
                            if val != this.val {
                                return Poll::Ready(Err(KernelError::TryAgain));
                            }

                            let token = wait_queue.register_with_data(cx.waker(), this.bitmask);

                            this.state = WaitState::Waiting { token };

                            return Poll::Pending;
                        }
                        Err(_) => {
                            drop(wait_queue);

                            let uaddr = this.uaddr;
                            let fault_handler = Box::pin(async move {
                                copy_from_user(uaddr).await?;
                                Ok(())
                            });

                            this.state = WaitState::HandlingFault(fault_handler);
                            continue;
                        }
                    }
                }

                WaitState::Waiting { token } => {
                    let wait_queue = this.queue.lock_save_irq();

                    if !wait_queue.contains_token(*token) {
                        return Poll::Ready(Ok(()));
                    } else {
                        return Poll::Pending;
                    }
                }

                WaitState::HandlingFault(fut) => match fut.as_mut().poll(cx) {
                    Poll::Ready(Ok(_)) => {
                        this.state = WaitState::Init;
                    }
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => return Poll::Pending,
                },
            }
        }
    }
}
