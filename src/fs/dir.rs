use alloc::boxed::Box;
use alloc::ffi::CString;
use async_trait::async_trait;
use core::alloc::Layout;
use core::any::Any;
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{DirStream, Dirent, FileType, Inode},
    memory::address::UA,
};
use ringbuf::Arc;

use crate::{
    memory::uaccess::copy_to_user_slice, process::fd_table::Fd, sched::current::current_task_shared,
};

use super::{fops::FileOps, open_file::FileCtx};

/// A stateful, peekable iterator for reading directory entries.
///
/// This struct holds a lock on the `OpenFileInner` for its entire lifetime,
/// ensuring that directory reading is a serialized.
pub struct OpenFileDirIter<'a> {
    file_state: &'a mut FileCtx,
    stream: Box<dyn DirStream>,
    // Cache the next entry, for peekability.
    next: Option<Dirent>,
}

impl<'a> OpenFileDirIter<'a> {
    /// Peeks at the next directory entry without consuming it or advancing the
    /// state.
    pub fn peek(&self) -> Option<&Dirent> {
        self.next.as_ref()
    }

    pub async fn next(&mut self) -> Result<Option<Dirent>> {
        let ret = self.next.take();

        self.next = self.stream.next_entry().await?;

        if let Some(ret) = ret.as_ref() {
            self.file_state.pos = ret.offset;
        }

        Ok(ret)
    }
}

/// A struct that represents an open file to a directory inode, but implement no
/// stateful file operations.
pub struct DirFile {
    inode: Arc<dyn Inode>,
}

impl DirFile {
    pub fn new(inode: Arc<dyn Inode>) -> Self {
        Self { inode }
    }
}

#[async_trait]
impl FileOps for DirFile {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    async fn read(&mut self, _ctx: &mut FileCtx, _buf: UA, _count: usize) -> Result<usize> {
        Err(FsError::IsADirectory.into())
    }

    async fn readat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(FsError::IsADirectory.into())
    }

    async fn write(&mut self, _ctx: &mut FileCtx, _buf: UA, _count: usize) -> Result<usize> {
        Err(FsError::IsADirectory.into())
    }

    async fn writeat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(FsError::IsADirectory.into())
    }

    async fn readdir<'a>(&'a mut self, ctx: &'a mut FileCtx) -> Result<OpenFileDirIter<'a>> {
        if self.inode.getattr().await?.file_type != FileType::Directory {
            return Err(FsError::NotADirectory.into());
        }

        let mut stream = self.inode.readdir(ctx.pos).await?;
        let next = stream.next_entry().await?;

        Ok(OpenFileDirIter {
            file_state: ctx,
            stream,
            next,
        })
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug)]
enum DirentFileType {
    _Unknown = 0,
    Fifo = 1,
    Char = 2,
    Dir = 4,
    Block = 6,
    Reg = 8,
    Link = 10,
    Socket = 12,
    _Wht = 14,
}

impl From<FileType> for DirentFileType {
    fn from(value: FileType) -> Self {
        match value {
            FileType::File => Self::Reg,
            FileType::Directory => Self::Dir,
            FileType::Symlink => Self::Link,
            FileType::BlockDevice(_) => Self::Block,
            FileType::CharDevice(_) => Self::Char,
            FileType::Fifo => Self::Fifo,
            FileType::Socket => Self::Socket,
        }
    }
}

/// The header of a `linux_dirent64` struct.
///
/// This must match the C layout precisely. `#[repr(packed)]` is essential to
/// remove Rust's default padding and make `size_of` report the correct unpadded
/// header size (19 bytes).
#[repr(C, packed)]
struct Dirent64Hdr {
    _ino: u64,
    _off: u64,
    _reclen: u16,
    _kind: DirentFileType,
}

pub async fn sys_getdents64(fd: Fd, mut ubuf: UA, size: u32) -> Result<usize> {
    let task = current_task_shared();
    let file = task
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, ctx) = &mut *file.lock().await;

    let mut entries_iter = ops.readdir(ctx).await?;

    let mut bytes_written = 0;

    // Iterate through the directory's entries, skipping those we've already
    // read.
    while let Some(de) = entries_iter.peek() {
        let c_str_name = CString::new(de.name.clone()).map_err(|_| KernelError::InvalidValue)?;
        let name_buf = c_str_name.as_bytes_with_nul();
        let name_len = name_buf.len();

        let header_len = core::mem::size_of::<Dirent64Hdr>();
        let unpadded_len = header_len + name_len;

        // Userspace expects dirents to always be 8-byte aligned.
        let padded_reclen = Layout::from_size_align(unpadded_len, 8)
            .unwrap()
            .pad_to_align()
            .size();

        // If the full, padded entry doesn't fit, stop here for this syscall.
        if padded_reclen > (size as usize).saturating_sub(bytes_written) {
            break;
        }

        // The dirent fits,  consume this peeked dirent.
        let consumed_de = entries_iter.next().await?.unwrap();
        let current_pos = consumed_de.offset;

        let mut kernel_entry_buf = [0u8; 512];
        let buf_ptr = kernel_entry_buf.as_mut_ptr();

        unsafe {
            // Write ino (u64) at offset 0
            (buf_ptr as *mut u64).write_unaligned(consumed_de.id.inode_id());

            // Write off (u64) at offset 8.
            (buf_ptr.add(8) as *mut u64).write_unaligned(current_pos as u64);

            // Write reclen (u16) at offset 16
            (buf_ptr.add(16) as *mut u16).write_unaligned(padded_reclen as u16);

            // Write kind (u8) at offset 18
            let de_filetype: DirentFileType = consumed_de.file_type.into();
            buf_ptr.add(18).write(de_filetype as u8);
        }

        kernel_entry_buf[header_len..unpadded_len].copy_from_slice(name_buf);

        let entry_slice = &kernel_entry_buf[..padded_reclen];
        copy_to_user_slice(entry_slice, ubuf).await?;

        ubuf = ubuf.add_bytes(padded_reclen);
        bytes_written += padded_reclen;
    }

    // If we didn't write any bytes but there were unprocessed directory
    // entries, that's an error.
    if bytes_written == 0 && entries_iter.peek().is_some() {
        Err(KernelError::InvalidValue)
    } else {
        Ok(bytes_written)
    }
}
