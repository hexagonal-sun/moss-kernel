use super::meta::TtyMetadata;
use super::meta::VSUSP;
use super::{TtyInputHandler, meta::*};
use crate::console::Console;
use crate::kernel::kpipe::KPipe;
use crate::kernel::rand::entropy_pool;
use crate::process::thread_group::Pgid;
use crate::process::thread_group::signal::SigId;
use crate::process::thread_group::signal::kill::send_signal_to_pg;
use crate::sched::current_work;
use crate::sync::{CondVar, SpinLock};
use alloc::{sync::Arc, vec::Vec};
use libkernel::error::Result;
use libkernel::sync::condvar::WakeupType;

pub struct TtyInputCooker {
    cooked_buf: KPipe,
    eof_pending: CondVar<bool>,
    line_buf: Vec<u8>,
    console: Arc<dyn Console>,
    meta: Arc<SpinLock<TtyMetadata>>,
}

impl TtyInputCooker {
    pub fn new(console: Arc<dyn Console>, meta: Arc<SpinLock<TtyMetadata>>) -> Result<Self> {
        Ok(Self {
            line_buf: Vec::with_capacity(1024),
            eof_pending: CondVar::new(false),
            cooked_buf: KPipe::new()?,
            console,
            meta,
        })
    }

    pub fn cooked_buf_pipe(&self) -> KPipe {
        self.cooked_buf.clone()
    }

    pub fn eof_pending(&self) -> &CondVar<bool> {
        &self.eof_pending
    }
}

impl TtyInputHandler for SpinLock<TtyInputCooker> {
    fn push_byte(&self, byte: u8) {
        let mut this = self.lock_save_irq();
        let termios = this.meta.lock_save_irq().termios;

        let TtyInputCooker {
            line_buf,
            cooked_buf,
            console,
            ..
        } = &mut *this;

        // Seed the entropy pool.
        //
        // SAFETY: A console interrupt isn't periodic.
        entropy_pool().add_temporal_entropy();

        // Handle signal-generating control characters
        if termios.c_lflag.contains(TermiosLocalFlags::ISIG) {
            let intr_char = termios.c_cc[VINTR];
            let quit_char = termios.c_cc[VQUIT];
            let susp_char = termios.c_cc[VSUSP];

            let sig = if byte == intr_char {
                Some(SigId::SIGINT)
            } else if byte == quit_char {
                Some(SigId::SIGQUIT)
            } else if byte == susp_char {
                Some(SigId::SIGTSTP)
            } else {
                None
            };

            if let Some(sig) = sig {
                let echo_enabled = termios.c_lflag.contains(TermiosLocalFlags::ECHO);
                let console_clone = console.clone();
                let pgid: Pgid = {
                    let meta = this.meta.lock_save_irq();
                    meta.fg_pg
                        .unwrap_or_else(|| *current_work().process.pgid.lock_save_irq())
                };

                drop(this);

                send_signal_to_pg(pgid, sig);

                if echo_enabled
                    && let Some(display) = match sig {
                        SigId::SIGINT => Some(b"^C\r\n".as_slice()),
                        SigId::SIGQUIT => Some(b"^\\\r\n".as_slice()),
                        SigId::SIGTSTP => Some(b"^Z\r\n".as_slice()),
                        _ => None,
                    }
                {
                    console_clone.write_buf(display);
                }

                return;
            }
        }

        // Check if we are in canonical mode
        if !termios.c_lflag.contains(TermiosLocalFlags::ICANON) {
            // In raw mode, we just pass the byte through.
            if cooked_buf.try_push(byte).is_ok()
                && termios.c_lflag.contains(TermiosLocalFlags::ECHO)
            {
                // Echo if requested
                console.write_buf(&[byte]);
            }
        } else {
            let erase_char = termios.c_cc[VERASE];
            let kill_char = termios.c_cc[VKILL];
            let eof_char = termios.c_cc[VEOF];

            match byte {
                // Line terminator
                b'\n' | b'\r' => {
                    // Echo newline if enabled
                    if termios.c_lflag.contains(TermiosLocalFlags::ECHO) {
                        console.write_buf(b"\r\n");
                    }

                    line_buf.push(b'\n');

                    cooked_buf.try_push_slice(line_buf);

                    line_buf.clear();
                }

                // Character erase: Backspace or DEL key
                b if b == erase_char => {
                    if !line_buf.is_empty() {
                        line_buf.pop();
                        // Echo erase sequence if enabled (backspace, space, backspace)
                        if termios.c_lflag.contains(TermiosLocalFlags::ECHOE) {
                            console.write_buf(b"\x08 \x08");
                        }
                    }
                }

                // Line kill
                b if b == kill_char => {
                    // Echo kill sequence if enabled
                    while !line_buf.is_empty() {
                        line_buf.pop();
                        if termios.c_lflag.contains(TermiosLocalFlags::ECHOE) {
                            console.write_buf(b"\x08 \x08");
                        }
                    }
                }

                // End of file
                b if b == eof_char => {
                    if line_buf.is_empty() {
                        this.eof_pending.update(|s| {
                            *s = true;
                            WakeupType::One
                        });
                    } else {
                        cooked_buf.try_push_slice(line_buf);
                        line_buf.clear();
                    }
                }

                // Regular printable character
                _ => {
                    if line_buf.len() < line_buf.capacity() {
                        line_buf.push(byte);
                        // Echo the character if enabled
                        if termios.c_lflag.contains(TermiosLocalFlags::ECHO) {
                            console.write_buf(&[byte]);
                        }
                    }
                }
            }
        }
    }
}
