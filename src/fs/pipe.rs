use crate::{
    clock::realtime::date,
    kernel::kpipe::KPipe,
    memory::uaccess::copy_to_user,
    process::{
        fd_table::Fd,
        thread_group::signal::{InterruptResult, Interruptable, SigId},
    },
    sched::current::current_task,
    sync::CondVar,
};
use alloc::{boxed::Box, sync::Arc};
use async_trait::async_trait;
use core::{
    future,
    pin::pin,
    sync::atomic::{AtomicU64, Ordering},
    task::Poll,
    time::Duration,
};
use libkernel::{
    error::{KernelError, Result},
    fs::{
        FileType, Inode, InodeId, OpenFlags, SeekFrom,
        attr::{FileAttr, FilePermissions},
        pathbuf::PathBuf,
    },
    memory::{
        PAGE_SIZE,
        address::{TUA, UA},
    },
    proc::ids::{Gid, Uid},
    sync::condvar::WakeupType,
};
//
use super::{
    fops::FileOps,
    open_file::{FileCtx, OpenFile},
};

struct PipeInode {
    id: InodeId,
    time: Duration,
    uid: Uid,
    gid: Gid,
}

#[async_trait]
impl Inode for PipeInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(FileAttr {
            id: self.id,
            size: 0,
            block_size: PAGE_SIZE as _,
            blocks: 0,
            atime: self.time,
            btime: self.time,
            mtime: self.time,
            ctime: self.time,
            file_type: FileType::Fifo,
            mode: FilePermissions::from_bits_retain(0o0600),
            nlinks: 1,
            uid: self.uid,
            gid: self.gid,
        })
    }
}

#[derive(Clone)]
struct PipeInner {
    buf: KPipe,
    other_side_gone: CondVar<bool>,
}

impl PipeInner {}

struct PipeReader {
    inner: PipeInner,
}

impl PipeReader {
    async fn do_read(&self, read_fut: impl Future<Output = Result<usize>>) -> Result<usize> {
        let mut read_fut = pin!(read_fut);
        let mut gone_fut =
            pin!(
                self.inner
                    .other_side_gone
                    .wait_until(|gone| if *gone { Some(()) } else { None })
            );

        match future::poll_fn(move |cx| {
            // Check the consumption future first, before we check whether the
            // other side of the pipe has gone. This ensures we drain the buffer
            // first.
            if let Poll::Ready(r) = read_fut.as_mut().poll(cx) {
                Poll::Ready(r)
            } else if gone_fut.as_mut().poll(cx).is_ready() {
                Poll::Ready(Ok(0))
            } else {
                Poll::Pending
            }
        })
        .interruptable()
        .await
        {
            InterruptResult::Interrupted => Err(KernelError::Interrupted),
            InterruptResult::Uninterrupted(r) => r,
        }
    }
}

#[async_trait]
impl FileOps for PipeReader {
    async fn read(&mut self, _ctx: &mut FileCtx, u_buf: UA, count: usize) -> Result<usize> {
        self.readat(u_buf, count, 0).await
    }

    async fn readat(&mut self, u_buf: UA, count: usize, _offset: u64) -> Result<usize> {
        if count == 0 {
            return Ok(0);
        }

        self.do_read(self.inner.buf.copy_to_user(u_buf, count))
            .await
    }

    async fn write(&mut self, _ctx: &mut FileCtx, _buf: UA, _count: usize) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    async fn writeat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    async fn seek(&mut self, _ctx: &mut FileCtx, _pos: SeekFrom) -> Result<u64> {
        Err(KernelError::SeekPipe)
    }

    async fn splice_into(
        &mut self,
        _ctx: &mut FileCtx,
        kbuf: &KPipe,
        count: usize,
    ) -> Result<usize> {
        self.do_read(async { Ok(kbuf.splice_from(&self.inner.buf, count).await) })
            .await
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        // notify any writers that the read end of the pipe has gone.
        self.inner.other_side_gone.update(|gone| {
            *gone = true;
            WakeupType::All
        });
    }
}

struct PipeWriter {
    inner: PipeInner,
}

impl PipeWriter {
    async fn do_write(&self, write_fut: impl Future<Output = Result<usize>>) -> Result<usize> {
        let mut write_fut = pin!(write_fut);
        let mut gone_fut =
            pin!(
                self.inner
                    .other_side_gone
                    .wait_until(|gone| if *gone { Some(()) } else { None })
            );

        future::poll_fn(move |cx| {
            // Check the gone future first, before we write data into the
            // buffer. There's no point writing data if there's no consumer!
            if gone_fut.as_mut().poll(cx).is_ready() {
                // Other side of the pipe has been closed.
                current_task().raise_task_signal(SigId::SIGPIPE);
                Poll::Ready(Err(KernelError::BrokenPipe))
            } else if let Poll::Ready(x) = write_fut.as_mut().poll(cx) {
                Poll::Ready(x)
            } else {
                Poll::Pending
            }
        })
        .await
    }
}

#[async_trait]
impl FileOps for PipeWriter {
    async fn read(&mut self, _ctx: &mut FileCtx, _buf: UA, _count: usize) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    async fn readat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    async fn write(&mut self, _ctx: &mut FileCtx, u_buf: UA, count: usize) -> Result<usize> {
        self.writeat(u_buf, count, 0).await
    }

    async fn writeat(&mut self, u_buf: UA, count: usize, _offset: u64) -> Result<usize> {
        if count == 0 {
            return Ok(0);
        }

        self.do_write(self.inner.buf.copy_from_user(u_buf, count))
            .await
    }

    async fn seek(&mut self, _ctx: &mut FileCtx, _pos: SeekFrom) -> Result<u64> {
        Err(KernelError::SeekPipe)
    }

    async fn splice_from(
        &mut self,
        _ctx: &mut FileCtx,
        kbuf: &KPipe,
        count: usize,
    ) -> Result<usize> {
        self.do_write(async { Ok(self.inner.buf.splice_from(kbuf, count).await) })
            .await
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        // notify any readers that the write end of the pipe has gone.
        self.inner.other_side_gone.update(|gone| {
            *gone = true;
            WakeupType::All
        });
    }
}

pub async fn sys_pipe2(fds: TUA<[Fd; 2]>, flags: u32) -> Result<usize> {
    let flags = OpenFlags::from_bits_retain(flags);

    let kbuf = KPipe::new()?;
    let condvar = CondVar::new(false);

    let inner = PipeInner {
        buf: kbuf,
        other_side_gone: condvar,
    };

    let reader = PipeReader {
        inner: inner.clone(),
    };

    let writer = PipeWriter { inner };

    let (read_fd, write_fd) = {
        static INODE_ID: AtomicU64 = AtomicU64::new(0);

        let task = current_task();
        let mut fds = task.fd_table.lock_save_irq();

        let inode = {
            let creds = task.creds.lock_save_irq();
            Arc::new(PipeInode {
                id: InodeId::from_fsid_and_inodeid(0xf, INODE_ID.fetch_add(1, Ordering::Relaxed)),
                time: date(),
                uid: creds.uid(),
                gid: creds.gid(),
            })
        };

        let mut read_file = OpenFile::new(Box::new(reader), flags);
        let mut write_file = OpenFile::new(Box::new(writer), flags);

        read_file.update(inode.clone(), PathBuf::new());
        write_file.update(inode, PathBuf::new());

        let read_fd = fds.insert(Arc::new(read_file))?;
        let write_fd = fds.insert(Arc::new(write_file))?;

        (read_fd, write_fd)
    };

    // TODO: What if the copy fails here, we've leaked the above file
    // descriptors.
    copy_to_user(fds, [read_fd as _, write_fd as _]).await?;

    Ok(0)
}
