use crate::{fs::open_file::OpenFile, memory::uaccess::UserCopyable};
use alloc::{sync::Arc, vec::Vec};
use libkernel::error::{FsError, Result};

pub mod dup;
pub mod epoll;
pub mod fcntl;
pub mod select;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fd(pub i32);

unsafe impl UserCopyable for Fd {}

pub const AT_FDCWD: i32 = -100; // Standard value for "current working directory"

impl Fd {
    pub fn as_raw(self) -> i32 {
        self.0
    }

    pub fn is_atcwd(self) -> bool {
        self.0 == AT_FDCWD
    }
}

impl From<u64> for Fd {
    // Convenience implementation for syscalls.
    fn from(value: u64) -> Self {
        Self(value.cast_signed() as _)
    }
}

bitflags::bitflags! {
    #[derive(Clone, Default)]
    pub struct FdFlags: u32 {
        /// The close-on-exec flag.
        const CLOEXEC = 1;
    }
}

#[derive(Clone)]
pub struct FileDescriptorEntry {
    file: Arc<OpenFile>,
    flags: FdFlags,
}

#[derive(Clone)]
pub struct FileDescriptorTable {
    entries: Vec<Option<FileDescriptorEntry>>,
    next_fd_hint: usize,
}

const MAX_FDS: usize = 8192;

impl Default for FileDescriptorTable {
    fn default() -> Self {
        Self::new()
    }
}

impl FileDescriptorTable {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_fd_hint: 0,
        }
    }

    /// Gets the file object associated with a given file descriptor.
    pub fn get(&self, fd: Fd) -> Option<Arc<OpenFile>> {
        self.entries
            .get(fd.0 as usize)
            .and_then(|entry| entry.as_ref())
            .map(|entry| entry.file.clone())
    }

    /// Inserts a new file into the table, returning the new file descriptor.
    pub fn insert(&mut self, file: Arc<OpenFile>) -> Result<Fd> {
        let fd = self.find_free_fd()?;

        let entry = FileDescriptorEntry {
            file,
            flags: FdFlags::default(),
        };

        self.insert_at(fd, entry);

        Ok(fd)
    }

    /// Insert the given entry at the specified index. If there was an entry at
    /// that index `Some(entry)` is returned. Otherwise, `None` is returned.
    fn insert_at(&mut self, fd: Fd, entry: FileDescriptorEntry) -> Option<FileDescriptorEntry> {
        let fd_idx = fd.0 as usize;

        if fd_idx >= self.entries.len() {
            // We need to resize the vector to accommodate the new FD.
            self.entries.resize_with(fd_idx + 1, || None);
        }

        self.entries[fd_idx].replace(entry)
    }

    /// Insert the given entry at or above the specified index, returning the
    /// file descriptor used.
    fn insert_above(&mut self, min_fd: Fd, file: Arc<OpenFile>) -> Result<Fd> {
        let start_idx = min_fd.0 as usize;
        let entry = FileDescriptorEntry {
            file,
            flags: FdFlags::default(),
        };

        for i in start_idx..self.entries.len() {
            if self.entries[i].is_none() {
                let fd = Fd(i as i32);
                self.insert_at(fd, entry);
                return Ok(fd);
            }
        }

        // No free slot found, so we need to expand the table.
        let fd = Fd(self.entries.len() as i32);
        self.entries.push(Some(entry));
        Ok(fd)
    }

    /// Removes a file descriptor from the table, returning the file if it
    /// existed.
    pub fn remove(&mut self, fd: Fd) -> Option<Arc<OpenFile>> {
        let fd_idx = fd.0 as usize;

        if let Some(entry) = self.entries.get_mut(fd_idx)
            && let Some(old_entry) = entry.take()
        {
            // Update the hint to speed up the next search.
            self.next_fd_hint = self.next_fd_hint.min(fd_idx);
            return Some(old_entry.file);
        }

        None
    }

    /// Called during an `execve`; closes all FDs marked with `CLOEXEC` flag.
    pub async fn close_cloexec_entries(&mut self) {
        let fds_to_close = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, fd)| {
                if let Some(fd) = fd
                    && fd.flags.contains(FdFlags::CLOEXEC)
                {
                    Some(i)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        for fd in fds_to_close {
            if let Some(fd) = self.remove(Fd(fd as _))
                && let Some(file) = Arc::into_inner(fd)
            {
                let (ops, ctx) = &mut *file.lock().await;
                let _ = ops.release(ctx).await;
            }
        }
    }

    /// Finds the lowest-numbered available file descriptor.
    fn find_free_fd(&mut self) -> Result<Fd> {
        // Start searching from our hint.
        for i in self.next_fd_hint..self.entries.len() {
            if self.entries[i].is_none() {
                self.next_fd_hint = i + 1;
                return Ok(Fd(i as i32));
            }
        }

        // We didn't find a free slot in the existing capacity
        let next = self.entries.len();

        if next >= MAX_FDS {
            Err(FsError::TooManyFiles.into())
        } else {
            self.next_fd_hint = next + 1;
            Ok(Fd(next as i32))
        }
    }

    /// Number of file descriptors in use.
    pub fn len(&self) -> usize {
        self.entries.iter().filter(|e| e.is_some()).count()
    }
}
