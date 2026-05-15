use crate::fs::fops::FileOps;
use crate::fs::open_file::{FileCtx, OpenFile};
use crate::memory::uaccess::{copy_from_user_slice, copy_to_user_slice};
use crate::process::fd_table::FdFlags;
use crate::sched::syscall_ctx::ProcessCtx;
use crate::sync::Mutex;
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use core::ffi::c_char;
use libkernel::fs::{OpenFlags, SeekFrom};
use libkernel::memory::address::{TUA, UA};

pub struct MemFd {
    data: Mutex<Vec<u8>>,
}

impl MemFd {
    fn new() -> Self {
        Self {
            data: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl FileOps for MemFd {
    async fn readat(
        &mut self,
        buf: UA,
        count: usize,
        offset: u64,
    ) -> libkernel::error::Result<usize> {
        if count == 0 {
            return Ok(0);
        }
        let data = self.data.lock().await;
        let offset = offset as usize;
        if offset >= data.len() {
            return Ok(0);
        }
        let available = data.len() - offset;
        let read_len = available.min(count);
        copy_to_user_slice(&data[offset..offset + read_len], buf).await?;
        Ok(read_len)
    }

    async fn writeat(
        &mut self,
        buf: UA,
        count: usize,
        offset: u64,
    ) -> libkernel::error::Result<usize> {
        if count == 0 {
            return Ok(0);
        }
        let mut data = self.data.lock().await;
        let offset = offset as usize;
        let end = offset + count;
        if end > data.len() {
            data.resize(end, 0);
        }
        copy_from_user_slice(buf, &mut data[offset..end]).await?;
        Ok(count)
    }

    async fn truncate(&mut self, _ctx: &FileCtx, new_size: usize) -> libkernel::error::Result<()> {
        let mut data = self.data.lock().await;
        data.resize(new_size, 0);
        Ok(())
    }

    async fn seek(&mut self, ctx: &mut FileCtx, pos: SeekFrom) -> libkernel::error::Result<u64> {
        fn saturating_add_signed(u: u64, i: i64) -> u64 {
            if i >= 0 {
                u.saturating_add(i as u64)
            } else {
                u.saturating_sub((-i) as u64)
            }
        }

        let size = self.data.lock().await.len() as u64;
        ctx.pos = match pos {
            SeekFrom::Start(x) => x,
            SeekFrom::End(x) => saturating_add_signed(size, x),
            SeekFrom::Current(x) => saturating_add_signed(ctx.pos, x),
        };

        Ok(ctx.pos)
    }
}

pub async fn sys_memfd_create(
    ctx: &ProcessCtx,
    _name: TUA<c_char>,
    _flags: u32,
) -> libkernel::error::Result<usize> {
    let memfd = MemFd::new();
    let open_file = Arc::new(OpenFile::new(Box::new(memfd), OpenFlags::empty()));
    Ok(ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .insert_with_flags(open_file, FdFlags::empty())?
        .as_raw() as usize)
}
