mod sops;
pub mod syscalls;
mod tcp;
mod unix;

use crate::drivers::timer::now;
use crate::sync::OnceLock;
use crate::sync::SpinLock;
use alloc::vec;
use core::net::Ipv4Addr;
use libkernel::error::KernelError;
use libkernel::memory::address::UA;
use libkernel::sync::waker_set::WakerSet;
use smoltcp::iface::SocketSet;
use smoltcp::wire::{IpAddress, IpEndpoint};
pub use sops::SocketOps;

static SOCKETS: OnceLock<SpinLock<SocketSet>> = OnceLock::new();

fn sockets() -> &'static SpinLock<SocketSet<'static>> {
    SOCKETS.get_or_init(|| SpinLock::new(SocketSet::new(vec![])))
}

// static INTERFACE: OnceLock<SpinLock<EthernetInterface<OurDevice>>> = OnceLock::new();
// static DHCP_CLIENT: OnceLock<SpinLock<Dhcpv4Client>> = OnceLock::new();

static DHCP_ENABLED: OnceLock<bool> = OnceLock::new();

#[expect(dead_code)]
fn dhcp_enabled() -> bool {
    *DHCP_ENABLED.get().unwrap()
}

static SOCKET_WAIT_QUEUE: OnceLock<SpinLock<WakerSet>> = OnceLock::new();

fn socket_wait_queue() -> &'static SpinLock<WakerSet> {
    SOCKET_WAIT_QUEUE.get_or_init(|| SpinLock::new(WakerSet::new()))
}

pub const AF_UNIX: i32 = 1;
pub const AF_INET: i32 = 2;
pub const SOCK_STREAM: i32 = 1;
pub const SOCK_DGRAM: i32 = 2;
pub const SOCK_SEQPACKET: i32 = 5;
pub const IPPROTO_TCP: i32 = 6;
#[expect(dead_code)]
pub const IPPROTO_UDP: i32 = 17;

#[repr(i32)]
pub enum ShutdownHow {
    Read = 0,
    Write = 1,
    ReadWrite = 2,
}

impl TryFrom<i32> for ShutdownHow {
    type Error = KernelError;
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(ShutdownHow::Read),
            1 => Ok(ShutdownHow::Write),
            2 => Ok(ShutdownHow::ReadWrite),
            _ => Err(KernelError::InvalidValue),
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum SockAddr {
    In(SockAddrIn),
    Un(SockAddrUn),
}

#[derive(Copy, Clone, Debug)]
#[repr(C, packed)]
pub struct SockAddrIn {
    family: u16,
    port: [u8; 2],
    addr: [u8; 4],
    zero: [u8; 8],
}

#[derive(Copy, Clone, Debug)]
#[repr(C, packed)]
pub struct SockAddrUn {
    family: u16,
    path: [u8; 108],
}

unsafe impl crate::memory::uaccess::UserCopyable for SockAddrIn {}
unsafe impl crate::memory::uaccess::UserCopyable for SockAddrUn {}

impl TryFrom<SockAddr> for IpEndpoint {
    type Error = KernelError;
    fn try_from(sockaddr: SockAddr) -> Result<IpEndpoint, KernelError> {
        match sockaddr {
            SockAddr::In(SockAddrIn { port, addr, .. }) => Ok(IpEndpoint {
                port: u16::from_be_bytes(port),
                addr: if u32::from_be_bytes(addr) == 0 {
                    IpAddress::Ipv4(Ipv4Addr::UNSPECIFIED)
                } else {
                    IpAddress::Ipv4(Ipv4Addr::from(addr))
                },
            }),
            _ => Err(KernelError::InvalidValue),
        }
    }
}

impl From<IpEndpoint> for SockAddr {
    fn from(endpoint: IpEndpoint) -> SockAddr {
        SockAddr::In(SockAddrIn {
            family: AF_INET as u16,
            port: endpoint.port.to_be_bytes(),
            addr: match endpoint.addr {
                IpAddress::Ipv4(addr) => addr.octets(),
                _ => unimplemented!(),
            },
            zero: [0; 8],
        })
    }
}

pub fn process_packets() {
    // For now, just wake any tasks waiting on socket progress.
    let _ = sockets().lock_save_irq();
    let _ = now();
    socket_wait_queue().lock_save_irq().wake_all();
}

pub fn parse_sockaddr(uaddr: UA, len: usize) -> Result<SockAddr, KernelError> {
    use crate::memory::uaccess::try_copy_from_user;
    use libkernel::memory::address::TUA;

    // Need at least a family field
    if len < size_of::<u16>() {
        return Err(KernelError::InvalidValue);
    }

    let family: u16 = try_copy_from_user(TUA::from_value(uaddr.value()))?;

    match family as i32 {
        AF_INET => {
            if len < size_of::<SockAddrIn>() {
                return Err(KernelError::InvalidValue);
            }
            let sain: SockAddrIn = try_copy_from_user(uaddr.cast())?;
            Ok(SockAddr::In(sain))
        }
        AF_UNIX => {
            if len < size_of::<SockAddrUn>() {
                return Err(KernelError::InvalidValue);
            }
            let saun: SockAddrUn = try_copy_from_user(uaddr.cast())?;
            Ok(SockAddr::Un(saun))
        }
        _ => Err(KernelError::AddressFamilyNotSupported),
    }
}
