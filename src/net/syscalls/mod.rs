use crate::process::fd_table::Fd;
use crate::sched::current_task;
use libkernel::error::KernelError;
use libkernel::memory::address::UA;

pub mod recv;
pub mod send;

pub enum AddressFamily {
    Unix = 1,
    Inet = 2,
    Inet6 = 10,
}

impl TryFrom<i32> for AddressFamily {
    type Error = KernelError;

    fn try_from(value: i32) -> libkernel::error::Result<Self> {
        match value {
            1 => Ok(AddressFamily::Unix),
            2 => Ok(AddressFamily::Inet),
            10 => Ok(AddressFamily::Inet6),
            _ => Err(KernelError::InvalidValue),
        }
    }
}

pub enum SocketType {
    Datagram = 1,
    Stream = 2,
    Raw = 3,
    RDM = 4,
    SeqPacket = 5,
    DCCP = 6,
    Packet = 10,
}

impl TryFrom<i32> for SocketType {
    type Error = KernelError;

    fn try_from(value: i32) -> libkernel::error::Result<Self> {
        match value {
            1 => Ok(SocketType::Datagram),
            2 => Ok(SocketType::Stream),
            3 => Ok(SocketType::Raw),
            4 => Ok(SocketType::RDM),
            5 => Ok(SocketType::SeqPacket),
            6 => Ok(SocketType::DCCP),
            10 => Ok(SocketType::Packet),
            _ => Err(KernelError::InvalidValue),
        }
    }
}

pub async fn sys_socket(
    domain: i32,
    type_: i32,
    _protocol: i32,
) -> libkernel::error::Result<usize> {
    let _family = AddressFamily::try_from(domain)?;
    // TODO: mask out flags from type_
    let _socket_type = SocketType::try_from(type_)?;
    Err(KernelError::NotSupported)
}

pub async fn sys_bind(fd: Fd, addr: UA, addr_len: u32) -> libkernel::error::Result<usize> {
    Err(KernelError::NotSupported)
}

pub async fn sys_listen(fd: Fd, backlog: i32) -> libkernel::error::Result<usize> {
    let task = current_task();
    let socket = task
        .fd_table
        .lock_save_irq()
        .get_socket(fd)
        .ok_or(KernelError::BadFd)?;
    let (ops, state) = &mut *socket.lock().await;
    ops.listen(state, backlog).await?;
    Ok(0)
}

pub async fn sys_accept(fd: Fd, addr: UA, addr_len: u32) -> libkernel::error::Result<usize> {
    let _ = (addr, addr_len);
    Err(KernelError::NotSupported)
}

pub async fn sys_connect(fd: Fd, addr: UA, addr_len: u32) -> libkernel::error::Result<usize> {
    Err(KernelError::NotSupported)
}

pub enum ShutdownHow {
    Read = 0,
    Write = 1,
    Both = 2,
}

impl TryFrom<i32> for ShutdownHow {
    type Error = KernelError;

    fn try_from(value: i32) -> libkernel::error::Result<Self> {
        match value {
            0 => Ok(ShutdownHow::Read),
            1 => Ok(ShutdownHow::Write),
            2 => Ok(ShutdownHow::Both),
            _ => Err(KernelError::InvalidValue),
        }
    }
}

pub async fn sys_shutdown(fd: Fd, how: i32) -> libkernel::error::Result<usize> {
    let _how = ShutdownHow::try_from(how)?;
    Err(KernelError::NotSupported)
}
