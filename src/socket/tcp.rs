use crate::arch::ArchImpl;
use crate::fs::fops::FileOps;
use crate::fs::open_file::FileCtx;
use crate::socket::sops::SocketOps;
use crate::socket::{ShutdownHow, SockAddr, process_packets, sockets};
use crate::sync::SpinLock;
use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use async_trait::async_trait;
use core::sync::atomic::{AtomicUsize, Ordering};
use libkernel::error::KernelError;
use libkernel::memory::address::UA;
use libkernel::sync::spinlock::SpinLockIrqGuard;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp::SocketBuffer;
use smoltcp::wire::IpEndpoint;

const BACKLOG_MAX: usize = 8;
#[expect(dead_code)]
static INUSE_ENDPOINTS: SpinLock<BTreeSet<u16>> = SpinLock::new(BTreeSet::new());
#[expect(dead_code)]
static PASSIVE_OPENS_TOTAL: AtomicUsize = AtomicUsize::new(0);
#[expect(dead_code)]
static WRITTEN_BYTES_TOTAL: AtomicUsize = AtomicUsize::new(0);
#[expect(dead_code)]
static READ_BYTES_TOTAL: AtomicUsize = AtomicUsize::new(0);

pub struct TcpSocket {
    handle: SocketHandle,
    local_endpoint: SpinLock<Option<IpEndpoint>>,
    backlogs: SpinLock<Vec<Arc<TcpSocket>>>,
    num_backlogs: AtomicUsize,
}

impl TcpSocket {
    pub fn new() -> Self {
        let rx_buffer = SocketBuffer::new(vec![0; 4096]);
        let tx_buffer = SocketBuffer::new(vec![0; 4096]);
        let inner = smoltcp::socket::tcp::Socket::new(rx_buffer, tx_buffer);
        let handle = sockets().lock_save_irq().add(inner);
        TcpSocket {
            handle,
            local_endpoint: SpinLock::new(None),
            backlogs: SpinLock::new(Vec::new()),
            num_backlogs: AtomicUsize::new(0),
        }
    }

    fn refill_backlog_sockets(
        &self,
        backlogs: &mut SpinLockIrqGuard<Vec<Arc<TcpSocket>>, ArchImpl>,
    ) -> Result<(), KernelError> {
        let local_endpoint = match *self.local_endpoint.lock_save_irq() {
            Some(local_endpoint) => local_endpoint,
            None => return Err(KernelError::InvalidValue),
        };

        for _ in 0..(self.num_backlogs.load(Ordering::SeqCst) - backlogs.len()) {
            let socket = TcpSocket::new();
            sockets()
                .lock_save_irq()
                .get_mut::<smoltcp::socket::tcp::Socket>(socket.handle)
                .listen(local_endpoint)
                .unwrap();
            backlogs.push(Arc::new(socket));
        }

        Ok(())
    }
}

#[async_trait]
impl SocketOps for TcpSocket {
    async fn bind(&self, addr: SockAddr) -> libkernel::error::Result<()> {
        *self.local_endpoint.lock_save_irq() = Some(addr.try_into()?);
        Ok(())
    }

    async fn listen(&self, backlog: i32) -> Result<(), KernelError> {
        let mut backlogs = self.backlogs.lock_save_irq();

        let new_num_backlogs = (backlog as usize).min(BACKLOG_MAX);
        backlogs.truncate(new_num_backlogs);
        self.num_backlogs.store(new_num_backlogs, Ordering::SeqCst);

        self.refill_backlog_sockets(&mut backlogs)
    }

    async fn read(
        &mut self,
        _ctx: &mut FileCtx,
        _buf: UA,
        _count: usize,
    ) -> libkernel::error::Result<usize> {
        todo!()
    }

    async fn write(
        &mut self,
        _ctx: &mut FileCtx,
        _buf: UA,
        _count: usize,
    ) -> libkernel::error::Result<usize> {
        todo!()
    }

    async fn shutdown(&self, _how: ShutdownHow) -> libkernel::error::Result<()> {
        sockets()
            .lock_save_irq()
            .get_mut::<smoltcp::socket::tcp::Socket>(self.handle)
            .close();

        process_packets();
        Ok(())
    }

    fn as_file(self: Box<Self>) -> Box<dyn FileOps> {
        self
    }
}
