use crate::fs::open_file::FileCtx;
use crate::kernel::kpipe::KPipe;
use crate::socket::sops::{RecvFlags, SendFlags};
use crate::socket::{SockAddr, SocketOps};
use crate::sync::OnceLock;
use crate::sync::SpinLock;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use core::future::poll_fn;
use core::task::Poll;
use core::task::Waker;
use libkernel::error::{KernelError, Result};
use libkernel::memory::address::UA;

/// Registry mapping Unix socket path bytes to endpoint inbox and listening state
struct Endpoint {
    inbox: Arc<KPipe>,
    listening: bool,
    backlog_max: usize,
    pending: Vec<UnixSocket>,
    /// Wakers for tasks waiting in accept
    waiters: Vec<Waker>,
}

/// Registry mapping Unix socket path bytes to endpoint inbox
static UNIX_ENDPOINTS: OnceLock<SpinLock<BTreeMap<Vec<u8>, Endpoint>>> = OnceLock::new();

fn endpoints() -> &'static SpinLock<BTreeMap<Vec<u8>, Endpoint>> {
    UNIX_ENDPOINTS.get_or_init(|| SpinLock::new(BTreeMap::new()))
}

enum SocketType {
    Stream,
    Datagram,
    SeqPacket,
}

pub struct UnixSocket {
    socket_type: SocketType,
    /// Recv inbox
    inbox: Arc<KPipe>,
    /// The peer endpoint's inbox
    peer_inbox: SpinLock<Option<Arc<KPipe>>>,
    local_addr: SpinLock<Option<crate::socket::SockAddrUn>>,
    connected: SpinLock<bool>,
    listening: SpinLock<bool>,
    backlog: SpinLock<usize>,
    // Shutdown state
    rd_shutdown: SpinLock<bool>,
    wr_shutdown: SpinLock<bool>,
}

impl UnixSocket {
    fn new(socket_type: SocketType) -> Self {
        UnixSocket {
            socket_type,
            inbox: Arc::new(KPipe::new().expect("KPipe::new for UnixSocket")),
            peer_inbox: SpinLock::new(None),
            local_addr: SpinLock::new(None),
            connected: SpinLock::new(false),
            listening: SpinLock::new(false),
            backlog: SpinLock::new(0),
            rd_shutdown: SpinLock::new(false),
            wr_shutdown: SpinLock::new(false),
        }
    }

    pub fn new_stream() -> Self {
        Self::new(SocketType::Stream)
    }
    pub fn new_datagram() -> Self {
        Self::new(SocketType::Datagram)
    }
    pub fn new_seqpacket() -> Self {
        Self::new(SocketType::SeqPacket)
    }

    fn path_bytes(saun: &crate::socket::SockAddrUn) -> Option<Vec<u8>> {
        // Unix path is a sun_path-like fixed-size buffer which may be null-terminated
        let mut end = saun.path.len();
        for (i, b) in saun.path.iter().enumerate() {
            if *b == 0 {
                end = i;
                break;
            }
        }
        if end == 0 {
            None
        } else {
            Some(saun.path[..end].to_vec())
        }
    }
}

#[async_trait]
impl SocketOps for UnixSocket {
    async fn bind(&self, addr: SockAddr) -> Result<()> {
        match addr {
            SockAddr::Un(saun) => {
                let Some(path) = UnixSocket::path_bytes(&saun) else {
                    return Err(KernelError::InvalidValue);
                };
                // Register endpoint; if already exists, return error
                let mut reg = endpoints().lock_save_irq();
                if reg.contains_key(&path) {
                    return Err(KernelError::InvalidValue);
                }
                reg.insert(
                    path,
                    Endpoint {
                        inbox: self.inbox.clone(),
                        listening: false,
                        backlog_max: 0,
                        pending: Vec::new(),
                        waiters: Vec::new(),
                    },
                );
                *self.local_addr.lock_save_irq() = Some(saun);
                Ok(())
            }
            _ => Err(KernelError::InvalidValue),
        }
    }

    async fn connect(&self, addr: SockAddr) -> Result<()> {
        match addr {
            SockAddr::Un(saun) => {
                let Some(path) = UnixSocket::path_bytes(&saun) else {
                    return Err(KernelError::InvalidValue);
                };
                let mut reg = endpoints().lock_save_irq();
                let Some(ep) = reg.get_mut(&path) else {
                    return Err(KernelError::InvalidValue);
                };
                if ep.listening {
                    if ep.pending.len() >= ep.backlog_max {
                        return Err(KernelError::TryAgain);
                    }
                    let server_sock = UnixSocket::new(SocketType::Stream);
                    *server_sock.peer_inbox.lock_save_irq() = Some(self.inbox.clone());
                    *server_sock.connected.lock_save_irq() = true;
                    // Client links to listener inbox to write into server
                    *self.peer_inbox.lock_save_irq() = Some(server_sock.inbox.clone());
                    *self.connected.lock_save_irq() = true;
                    ep.pending.push(server_sock);
                    // Wake one waiter if present
                    if let Some(w) = ep.waiters.pop() {
                        w.wake();
                    }
                    Ok(())
                } else {
                    // Non-listening endpoint: treat as datagram or pre-bound stream endpoint
                    *self.peer_inbox.lock_save_irq() = Some(ep.inbox.clone());
                    *self.connected.lock_save_irq() = true;
                    Ok(())
                }
            }
            _ => Err(KernelError::InvalidValue),
        }
    }

    async fn listen(&self, backlog: i32) -> Result<()> {
        match self.socket_type {
            SocketType::Stream | SocketType::SeqPacket => {}
            SocketType::Datagram => return Err(KernelError::NotSupported),
        }
        if backlog < 0 {
            return Err(KernelError::InvalidValue);
        }
        let Some(saun) = &*self.local_addr.lock_save_irq() else {
            return Err(KernelError::InvalidValue);
        };
        let Some(path) = UnixSocket::path_bytes(saun) else {
            return Err(KernelError::InvalidValue);
        };
        let mut reg = endpoints().lock_save_irq();
        let Some(ep) = reg.get_mut(&path) else {
            return Err(KernelError::InvalidValue);
        };
        ep.listening = true;
        ep.backlog_max = backlog as usize;
        *self.listening.lock_save_irq() = true;
        *self.backlog.lock_save_irq() = backlog as usize;
        Ok(())
    }

    async fn accept(&self) -> Result<Box<dyn SocketOps>> {
        {
            if !*self.listening.lock_save_irq() {
                return Err(KernelError::InvalidValue);
            }
        }
        let path_vec: Vec<u8> = {
            let guard = self.local_addr.lock_save_irq();
            let Some(saun) = &*guard else {
                return Err(KernelError::InvalidValue);
            };
            let Some(pv) = UnixSocket::path_bytes(saun) else {
                return Err(KernelError::InvalidValue);
            };
            pv
        };

        let sock = poll_fn(|cx| {
            let mut reg = endpoints().lock_save_irq();
            let Some(ep) = reg.get_mut(&path_vec) else {
                return Poll::Ready(Err(KernelError::InvalidValue));
            };
            if let Some(sock) = ep.pending.pop() {
                Poll::Ready(Ok(sock))
            } else {
                ep.waiters.push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await?;

        Ok(Box::new(sock))
    }

    async fn recv(
        &mut self,
        _ctx: &mut FileCtx,
        buf: UA,
        count: usize,
        _flags: RecvFlags,
    ) -> Result<usize> {
        if count == 0 {
            return Ok(0);
        }
        if *self.rd_shutdown.lock_save_irq() {
            return Ok(0);
        }
        self.inbox.copy_to_user(buf, count).await
    }

    async fn recvfrom(
        &mut self,
        _ctx: &mut FileCtx,
        _buf: UA,
        _count: usize,
        _flags: RecvFlags,
        _addr: Option<SockAddr>,
    ) -> Result<(usize, Option<SockAddr>)> {
        todo!();
        // let n = self.recv(ctx, buf, count, flags).await?;
        // Ok((n, None))
    }

    async fn send(
        &mut self,
        _ctx: &mut FileCtx,
        buf: UA,
        count: usize,
        _flags: SendFlags,
    ) -> Result<usize> {
        if count == 0 {
            return Ok(0);
        }
        if *self.wr_shutdown.lock_save_irq() {
            return Err(KernelError::BrokenPipe);
        }
        match self.socket_type {
            SocketType::Stream | SocketType::SeqPacket => {
                if !*self.connected.lock_save_irq() {
                    return Err(KernelError::InvalidValue);
                }
            }
            SocketType::Datagram => {}
        }
        let Some(peer) = self.peer_inbox.lock_save_irq().clone() else {
            return Err(KernelError::InvalidValue);
        };
        peer.copy_from_user(buf, count).await
    }

    async fn sendto(
        &mut self,
        _ctx: &mut FileCtx,
        _buf: UA,
        _count: usize,
        _flags: SendFlags,
        _addr: SockAddr,
    ) -> Result<usize> {
        todo!();
        // self.send(ctx, buf, count, flags).await
    }

    async fn shutdown(&self, how: crate::socket::ShutdownHow) -> Result<()> {
        match how {
            crate::socket::ShutdownHow::Read => {
                *self.rd_shutdown.lock_save_irq() = true;
            }
            crate::socket::ShutdownHow::Write => {
                *self.wr_shutdown.lock_save_irq() = true;
            }
            crate::socket::ShutdownHow::ReadWrite => {
                *self.rd_shutdown.lock_save_irq() = true;
                *self.wr_shutdown.lock_save_irq() = true;
            }
        }
        Ok(())
    }

    fn as_file(self: Box<Self>) -> Box<dyn crate::fs::fops::FileOps> {
        self
    }
}

impl Drop for UnixSocket {
    fn drop(&mut self) {
        if let Some(saun) = &*self.local_addr.lock_save_irq()
            && let Some(path) = UnixSocket::path_bytes(saun)
        {
            let mut reg = endpoints().lock_save_irq();
            reg.remove(&path);
        }
    }
}
