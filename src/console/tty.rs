use crate::{
    fs::{fops::FileOps, open_file::FileCtx},
    kernel::kpipe::KPipe,
    memory::uaccess::{copy_from_user, copy_from_user_slice, copy_to_user},
    process::thread_group::{
        Pgid,
        signal::{InterruptResult, Interruptable},
    },
    sched::current::current_task,
    sync::SpinLock,
};
use alloc::{boxed::Box, sync::Arc};
use async_trait::async_trait;
use cooker::TtyInputCooker;
use core::any::Any;
use core::{cmp::min, pin::Pin};
use futures::{
    future::{Either, select},
    pin_mut,
};
use libkernel::{
    error::{KernelError, Result},
    fs::SeekFrom,
    memory::address::{TUA, UA},
};
use meta::{
    TCGETS, TCGETS2, TCSETS, TCSETS2, TCSETSW, TCSETSW2, TIOCGPGRP, TIOCGWINSZ, TIOCSPGRP,
    TIOCSWINSZ, Termios, Termios2, TermiosOutputFlags, TtyMetadata,
};

use super::Console;

mod cooker;
mod meta;

/// A trait for an object that can receive input bytes from a console driver.
pub trait TtyInputHandler: Send + Sync {
    /// Pushes a single byte of input into the TTY layer. This is typically
    /// called from a driver's interrupt handler.
    fn push_byte(&self, byte: u8);
}

pub struct Tty {
    console: Arc<dyn Console>,
    meta: Arc<SpinLock<TtyMetadata>>,
    input_cooker: Arc<SpinLock<TtyInputCooker>>,
}

impl Tty {
    pub fn new(console: Arc<dyn Console>) -> Result<Self> {
        let meta = Arc::new(SpinLock::new(TtyMetadata::default()));

        let this = Self {
            console: console.clone(),
            meta: meta.clone(),
            input_cooker: Arc::new(SpinLock::new(TtyInputCooker::new(console.clone(), meta)?)),
        };

        let tty_as_handler: Arc<dyn TtyInputHandler> = this.input_cooker.clone();

        console.register_input_handler(Arc::downgrade(&tty_as_handler));

        Ok(this)
    }

    fn process_and_write_chunk(&mut self, chunk: &[u8]) {
        let termios_flags = self.meta.lock_save_irq().termios.c_oflag;

        if !termios_flags.contains(TermiosOutputFlags::OPOST) {
            // Raw mode: write the bytes directly with no translation.
            self.console.write_buf(chunk);
            return;
        }

        let translate_newline = termios_flags.contains(TermiosOutputFlags::ONLCR);

        for &byte in chunk.iter() {
            if translate_newline && byte == b'\n' {
                self.console.write_char('\r');
                self.console.write_char('\n');
            } else {
                self.console.write_char(byte.into());
            }
        }
    }
}

#[async_trait]
impl FileOps for Tty {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    async fn read(&mut self, _ctx: &mut FileCtx, usr_buf: UA, count: usize) -> Result<usize> {
        self.readat(usr_buf, count, 0).await
    }

    async fn readat(&mut self, usr_buf: UA, count: usize, _offset: u64) -> Result<usize> {
        let (cooked_pipe, eof_fut) = {
            let cooker = self.input_cooker.lock_save_irq();

            (
                cooker.cooked_buf_pipe(),
                cooker.eof_pending().wait_until(|s| {
                    if *s {
                        *s = false;
                        Some(())
                    } else {
                        None
                    }
                }),
            )
        };

        let copy_fut = cooked_pipe.copy_to_user(usr_buf, count);

        pin_mut!(copy_fut);

        match select(copy_fut, eof_fut).interruptable().await {
            InterruptResult::Interrupted => Err(KernelError::Interrupted),
            InterruptResult::Uninterrupted(Either::Left((result, _))) => result,
            InterruptResult::Uninterrupted(Either::Right(_)) => Ok(0),
        }
    }

    fn poll_read_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + 'static + Send>> {
        let (read_ready_fut, eof_fut) = {
            let cooker = self.input_cooker.lock_save_irq();

            (
                cooker.cooked_buf_pipe().read_ready(),
                cooker
                    .eof_pending()
                    .wait_until(|s| if *s { Some(()) } else { None }),
            )
        };

        Box::pin(async move {
            select(read_ready_fut, eof_fut).interruptable().await;

            Ok(())
        })
    }

    async fn write(&mut self, _ctx: &mut FileCtx, ptr: UA, count: usize) -> Result<usize> {
        self.writeat(ptr, count, 0).await
    }

    async fn writeat(&mut self, mut ptr: UA, count: usize, _offset: u64) -> Result<usize> {
        const CHUNK_SZ: usize = 128;

        let mut remaining = count;
        let mut total_written = 0;

        let mut user_chunk_buf = [0_u8; CHUNK_SZ];

        while remaining > 0 {
            let chunk_size = min(remaining, CHUNK_SZ);
            let raw_slice = &mut user_chunk_buf[..chunk_size];

            copy_from_user_slice(ptr, raw_slice).await?;

            self.process_and_write_chunk(raw_slice);

            ptr = ptr.add_bytes(chunk_size);
            total_written += chunk_size;
            remaining -= chunk_size;
        }

        Ok(total_written)
    }

    fn poll_write_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + 'static + Send>> {
        // A TTY is always ready to be written to.
        Box::pin(async { Ok(()) })
    }

    async fn ioctl(&mut self, _ctx: &mut FileCtx, request: usize, argp: usize) -> Result<usize> {
        match request {
            TIOCGWINSZ => {
                let sz = self.meta.lock_save_irq().winsz;

                copy_to_user(TUA::from_value(argp), sz).await?;

                return Ok(0);
            }
            TIOCSWINSZ => {
                let obj = copy_from_user(TUA::from_value(argp)).await?;

                self.meta.lock_save_irq().winsz = obj;

                return Ok(0);
            }
            TIOCGPGRP => {
                let fg_pg = self
                    .meta
                    .lock_save_irq()
                    .fg_pg
                    .unwrap_or_else(|| *current_task().process.pgid.lock_save_irq());

                copy_to_user(TUA::from_value(argp), fg_pg).await?;

                return Ok(0);
            }
            TIOCSPGRP => {
                let pgid: Pgid = copy_from_user(TUA::from_value(argp)).await?;

                self.meta.lock_save_irq().fg_pg = Some(pgid);

                return Ok(0);
            }
            TCGETS => {
                let termios: Termios = self.meta.lock_save_irq().termios.into();

                copy_to_user(TUA::from_value(argp), termios).await?;

                return Ok(0);
            }
            TCGETS2 => {
                let termios = self.meta.lock_save_irq().termios;

                copy_to_user(TUA::from_value(argp), termios).await?;

                return Ok(0);
            }
            TCSETS | TCSETSW => {
                let new_termios: Termios = copy_from_user(TUA::from_value(argp)).await?;

                self.meta
                    .lock_save_irq()
                    .termios
                    .update_from_termios(&new_termios);

                return Ok(0);
            }
            TCSETS2 | TCSETSW2 => {
                let new_termios: Termios2 = copy_from_user(TUA::from_value(argp)).await?;

                self.meta.lock_save_irq().termios = new_termios;

                return Ok(0);
            }
            _ => Err(KernelError::NotATty),
        }
    }

    async fn flush(&self, _ctx: &FileCtx) -> Result<()> {
        Ok(())
    }

    async fn splice_from(
        &mut self,
        _ctx: &mut FileCtx,
        kbuf: &KPipe,
        mut count: usize,
    ) -> Result<usize> {
        let mut buf = [0; 32];
        let mut total_bytes_read = 0;

        while count != 0 {
            let bytes_read = kbuf.pop_slice(&mut buf).await;

            if bytes_read == 0 {
                return Ok(total_bytes_read);
            }

            self.process_and_write_chunk(&buf[..bytes_read]);

            total_bytes_read += bytes_read;
            count -= bytes_read;
        }

        Ok(total_bytes_read)
    }

    async fn seek(&mut self, _ctx: &mut FileCtx, _pos: SeekFrom) -> Result<u64> {
        Err(KernelError::SeekPipe)
    }
}
