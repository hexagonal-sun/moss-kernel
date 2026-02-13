//! An asynchronous, multi-producer, single-consumer (MPSC) channel.
//!
//! This module provides a queue for sending values between asynchronous tasks

//! within the kernel.
use super::condvar::{CondVar, WakeupType};
use crate::CpuOps;
use alloc::collections::VecDeque;

struct MpscState<T: Send> {
    data: VecDeque<T>,
    senders: usize,
    recv_gone: bool,
}

/// The receiving half of the MPSC channel.
///
/// There can only be one `Reciever` for a given channel.
///
/// If the `Reciever` is dropped, the channel is closed. Any subsequent messages
/// sent by a `Sender` will be dropped.
pub struct Reciever<T: Send, C: CpuOps> {
    inner: CondVar<MpscState<T>, C>,
}

enum RxResult<T> {
    Data(T),
    SenderGone,
}

impl<T: Send, C: CpuOps> Reciever<T, C> {
    /// Asynchronously waits for a message from the channel.
    ///
    /// This function returns a `Future` that resolves to:
    /// - `Some(T)`: If a message was successfully received from the channel.
    /// - `None`: If all `Sender` instances have been dropped, indicating that
    ///   no more messages will ever be sent. The channel is now closed.
    pub async fn recv(&self) -> Option<T> {
        let result = self
            .inner
            .wait_until(|state| {
                if let Some(data) = state.data.pop_front() {
                    Some(RxResult::Data(data))
                } else if state.senders == 0 {
                    Some(RxResult::SenderGone)
                } else {
                    None
                }
            })
            .await;

        match result {
            RxResult::Data(d) => Some(d),
            RxResult::SenderGone => None,
        }
    }
}

impl<T: Send, C: CpuOps> Drop for Reciever<T, C> {
    fn drop(&mut self) {
        self.inner.update(|state| {
            // Since there can only be once reciever and we are now dropping
            // it, drain the queue, and set a flag such that any more sends
            // result in the value being dropped.
            core::mem::take(&mut state.data);
            state.recv_gone = true;

            WakeupType::None
        });
    }
}

/// The sending half of the MPSC channel.
///
/// `Sender` handles can be cloned to allow multiple producers to send messages
/// to the single `Reciever`.
///
/// When the last `Sender` is dropped, the channel is closed. This will cause
/// the `Reciever::recv` future to resolve to `None`.
pub struct Sender<T: Send, C: CpuOps> {
    inner: CondVar<MpscState<T>, C>,
}

impl<T: Send, C: CpuOps> Sender<T, C> {
    /// Sends a message into the channel.
    ///
    /// This method enqueues the given object `obj` for the `Reciever` to
    /// consume. After enqueuing the message, it notifies one waiting `Reciever`
    /// task, if one exists.
    ///
    /// This operation is non-blocking from an async perspective, though it will
    /// acquire a spinlock.
    pub fn send(&self, obj: T) {
        self.inner.update(|state| {
            if state.recv_gone {
                // Receiver has been dropped, so drop the message.
                return WakeupType::None;
            }

            state.data.push_back(obj);

            WakeupType::One
        });
    }
}

impl<T: Send, C: CpuOps> Clone for Sender<T, C> {
    fn clone(&self) -> Self {
        self.inner.update(|state| {
            state.senders += 1;

            WakeupType::None
        });

        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Send, C: CpuOps> Drop for Sender<T, C> {
    fn drop(&mut self) {
        self.inner.update(|state| {
            state.senders -= 1;

            if state.senders == 0 {
                // Wake the receiver to let it know the channel is now closed. We
                // use wake_all as a safeguard, though only one task should be
                // waiting.
                WakeupType::All
            } else {
                WakeupType::None
            }
        });
    }
}

/// Creates a new asynchronous, multi-producer, single-consumer channel.
///
/// Returns a tuple containing the `Sender` and `Reciever` halves. The `Sender`
/// can be cloned to create multiple producers, while the `Reciever` is the
/// single consumer.
pub fn channel<T: Send, C: CpuOps>() -> (Sender<T, C>, Reciever<T, C>) {
    let state = MpscState {
        data: VecDeque::new(),
        senders: 1,
        recv_gone: false,
    };

    let waitq = CondVar::new(state);

    let tx = Sender {
        inner: waitq.clone(),
    };

    let rx = Reciever { inner: waitq };

    (tx, rx)
}
