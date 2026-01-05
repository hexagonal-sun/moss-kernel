use crate::fs::fops::FileOps;
use crate::fs::open_file::OpenFile;
use crate::sched::current::current_task_shared;
use crate::socket::tcp::TcpSocket;
use crate::socket::unix::UnixSocket;
use crate::socket::{AF_INET, AF_UNIX, IPPROTO_TCP, SOCK_DGRAM, SOCK_SEQPACKET, SOCK_STREAM};
use alloc::boxed::Box;
use alloc::sync::Arc;
use libkernel::error::KernelError;
use libkernel::fs::OpenFlags;

pub const CLOSE_ON_EXEC: i32 = 0x80000;
pub const NONBLOCK: i32 = 0x800;

pub async fn sys_socket(domain: i32, type_: i32, protocol: i32) -> libkernel::error::Result<usize> {
    let _close_on_exec = (type_ & CLOSE_ON_EXEC) != 0;
    let _nonblock = (type_ & NONBLOCK) != 0;
    // Mask out flags
    let type_ = type_ & !(CLOSE_ON_EXEC | NONBLOCK);
    let new_socket: Box<dyn FileOps> = match (domain, type_, protocol) {
        (AF_INET, SOCK_STREAM, 0) | (AF_INET, SOCK_STREAM, IPPROTO_TCP) => {
            Box::new(TcpSocket::new())
        }
        (AF_UNIX, SOCK_STREAM, _) => Box::new(UnixSocket::new_stream()),
        (AF_UNIX, SOCK_DGRAM, _) => Box::new(UnixSocket::new_datagram()),
        (AF_UNIX, SOCK_SEQPACKET, _) => Box::new(UnixSocket::new_seqpacket()),
        _ => return Err(KernelError::AddressFamilyNotSupported),
    };
    // TODO: Correct flags
    let open_file = OpenFile::new(new_socket, OpenFlags::empty());
    let fd = current_task_shared()
        .fd_table
        .lock_save_irq()
        .insert(Arc::new(open_file))?;
    Ok(fd.as_raw() as usize)
}
