use crate::net::sops::SocketOps;
use crate::sync::{AsyncMutexGuard, Mutex};
use alloc::boxed::Box;

pub struct SocketCtx {}

impl SocketCtx {
    pub fn new() -> Self {
        Self {}
    }
}

pub struct OpenSocket {
    state: Mutex<(Box<dyn SocketOps>, SocketCtx)>,
}

impl OpenSocket {
    pub fn new(ops: Box<dyn SocketOps>) -> Self {
        Self {
            state: Mutex::new((ops, SocketCtx::new())),
        }
    }

    pub async fn lock(&self) -> AsyncMutexGuard<'_, (Box<dyn SocketOps>, SocketCtx)> {
        self.state.lock().await
    }
}
