use crate::{fs::open_file::OpenFile, memory::uaccess::UserCopyable};
use alloc::{sync::Arc, vec::Vec};
use libkernel::error::{FsError, Result};

pub mod dup;
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
    // Conveience implemtnation for syscalls.
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

    // Insert the given etnry at the specified index. If there was an entry at
    // that index `Some(entry)` is returned. Otherwise, `None` is returned.
    fn insert_at(&mut self, fd: Fd, entry: FileDescriptorEntry) -> Option<FileDescriptorEntry> {
        let fd_idx = fd.0 as usize;

        if fd_idx >= self.entries.len() {
            // We need to resize the vector to accommodate the new FD.
            self.entries.resize_with(fd_idx + 1, || None);
        }

        self.entries[fd_idx].replace(entry)
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

    /// Creates a new `FileDescriptorTable` for a child process during `execve`.
    /// It duplicates all file descriptors that do not have the `CLOEXEC` flag
    /// set.
    pub fn clone_for_exec(&self) -> Self {
        let new_entries = self
            .entries
            .iter()
            .map(|entry| {
                entry.as_ref().and_then(|e| {
                    if !e.flags.contains(FdFlags::CLOEXEC) {
                        Some(e.clone())
                    } else {
                        None
                    }
                })
            })
            .collect();

        Self {
            entries: new_entries,
            next_fd_hint: 0, // Recalculate hint on first use in new process.
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
