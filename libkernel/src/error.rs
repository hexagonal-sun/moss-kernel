//! Unified kernel error types.
//!
//! This module defines the error hierarchy used throughout the kernel.
//! [`KernelError`] is the top-level enum and wraps domain-specific errors such
//! as [`MapError`], [`FsError`], [`IoError`], and others. A [`Result<T>`] type
//! alias is provided for convenience.
//!
//! The [`syscall_error`] submodule maps [`KernelError`] variants to their POSIX
//! `errno` equivalents for returning values to user space.

use core::convert::Infallible;
use thiserror::Error;

pub mod syscall_error;

/// Errors that can occur during device probing.
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum ProbeError {
    /// No registers present in the Flattened Device Tree.
    #[error("No registers present in FDT")]
    NoReg,

    /// No register bank size found in the Flattened Device Tree.
    #[error("No register bank size in FDT")]
    NoRegSize,

    /// No interrupts declared in the Flattened Device Tree.
    #[error("No interrupts in FDT")]
    NoInterrupts,

    /// No parent interrupt controller found in the Flattened Device Tree.
    #[error("No parent interrupt controller in FDT")]
    NoParentInterrupt,

    /// The specified interrupt parent is not an interrupt controller.
    #[error("The specified interrupt parent isn't an interrupt controller")]
    NotInterruptController,

    /// Driver probing should be tried again after other probes have succeeded.
    #[error("Driver probing deferred for other dependencies")]
    Deferred,

    /// Device inspected but not a match for this driver; skip silently.
    #[error("Device not matched by driver")]
    NoMatch,
}

/// Errors that can occur during page table mapping.
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum MapError {
    /// Physical address is not page aligned.
    #[error("Physical address not page aligned")]
    PhysNotAligned,

    /// Virtual address is not page aligned.
    #[error("Virtual address not page aligned")]
    VirtNotAligned,

    /// Physical and virtual range sizes do not match.
    #[error("Physical and virtual range sizes do not match")]
    SizeMismatch,

    /// Failed to walk to the next level page table.
    #[error("Failed to walk to the next level page table")]
    WalkFailed,

    /// Invalid page table descriptor encountered.
    #[error("Invalid page table descriptor encountered")]
    InvalidDescriptor,

    /// The region to be mapped is smaller than `PAGE_SIZE`.
    #[error("The region to be mapped is smaller than PAGE_SIZE")]
    TooSmall,

    /// The virtual address range has already been mapped.
    #[error("The VA range is has already been mapped")]
    AlreadyMapped,

    /// Page table does not contain an L3 mapping.
    #[error("Page table does not contain an L3 mapping")]
    NotL3Mapped,
}

/// Errors from block-level I/O operations.
#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum IoError {
    /// The requested I/O operation was out of bounds for the block device.
    #[error("The requested I/O operation was out of bounds for the block device")]
    OutOfBounds,

    /// Corruption found in the filesystem metadata.
    #[error("Corruption found in the filesystem metadata")]
    MetadataCorruption,
}

/// Errors from filesystem operations.
#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum FsError {
    /// The path or file was not found.
    #[error("The path or file was not found")]
    NotFound,

    /// The path component is not a directory.
    #[error("The path component is not a directory.")]
    NotADirectory,

    /// The path component is a directory.
    #[error("The path component is a directory.")]
    IsADirectory,

    /// The file or directory already exists.
    #[error("The file or directory already exists.")]
    AlreadyExists,

    /// The directory is not empty.
    #[error("The directory is not empty.")]
    DirectoryNotEmpty,

    /// Invalid input parameters.
    #[error("Invalid input parameters.")]
    InvalidInput,

    /// The filesystem is corrupted or has an invalid format.
    #[error("The filesystem is corrupted or has an invalid format.")]
    InvalidFs,

    /// Attempted to access data out of bounds.
    #[error("Attempted to access data out of bounds.")]
    OutOfBounds,

    /// The operation is not permitted.
    #[error("The operation is not permitted.")]
    PermissionDenied,

    /// Could not find the specified filesystem driver.
    #[error("Could not find the specified FS driver")]
    DriverNotFound,

    /// Too many open files.
    #[error("Too many open files")]
    TooManyFiles,

    /// The device could not be found.
    #[error("The device could not be found")]
    NoDevice,

    /// Too many symbolic links encountered.
    #[error("Too many symbolic links encountered")]
    Loop,

    /// Attempted to rename across devices.
    #[error("Attempted to rename from cross device")]
    CrossDevice,
}

/// Errors that occur when loading or parsing an executable.
#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum ExecError {
    /// Invalid ELF format.
    #[error("Invalid ELF Format")]
    InvalidElfFormat,

    /// Invalid script format.
    #[error("Invalid Script Format")]
    InvalidScriptFormat,

    /// Invalid program header format.
    #[error("Invalid Program Header Format")]
    InvalidPHdrFormat,
}

/// Top-level kernel error type wrapping all domain-specific errors.
#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum KernelError {
    /// Cannot allocate memory.
    #[error("Cannot allocate memory")]
    NoMemory,

    /// Memory region not found.
    #[error("Memory region not found")]
    NoMemRegion,

    /// Invalid value.
    #[error("Invalid value")]
    InvalidValue,

    /// The current resource is already in use.
    #[error("The current resource is already in use")]
    InUse,

    /// Page table mapping failed.
    #[error("Page table mapping failed: {0}")]
    MappingError(#[from] MapError),

    /// Provided object is too large.
    #[error("Provided object is too large")]
    TooLarge,

    /// Operation not supported.
    #[error("Operation not supported")]
    NotSupported,

    /// Address family not supported.
    #[error("Address family not supported")]
    AddressFamilyNotSupported,

    /// Device probe failed.
    #[error("Device probe failed: {0}")]
    Probe(#[from] ProbeError),

    /// I/O operation failed.
    #[error("I/O operation failed: {0}")]
    Io(#[from] IoError),

    /// Filesystem operation failed.
    #[error("Filesystem operation failed: {0}")]
    Fs(#[from] FsError),

    /// Exec error during executable loading.
    #[error("Exec error: {0}")]
    Exec(#[from] ExecError),

    /// Not a tty.
    #[error("Not a tty")]
    NotATty,

    /// Fault error during syscall.
    #[error("Fault error during syscall")]
    Fault,

    /// Not an open file descriptor.
    #[error("Not an open file descriptor")]
    BadFd,

    /// Cannot seek on a pipe.
    #[error("Cannot seek on a pipe")]
    SeekPipe,

    /// Broken pipe.
    #[error("Broken pipe")]
    BrokenPipe,

    /// Operation not permitted.
    #[error("Operation not permitted")]
    NotPermitted,

    /// Buffer is full.
    #[error("Buffer is full")]
    BufferFull,

    /// Operation would block.
    #[error("Operation would block")]
    TryAgain,

    /// No such process.
    #[error("No such process")]
    NoProcess,

    /// No child process.
    #[error("No child process")]
    NoChildProcess,

    /// Operation timed out.
    #[error("Operation timed out")]
    TimedOut,

    /// Value out of range.
    #[error("Value out of range")]
    RangeError,

    /// Operation not supported on transport endpoint.
    #[error("Operation not supported on transport endpoint")]
    OpNotSupported,

    /// Interrupted system call.
    #[error("Interrupted system call")]
    Interrupted,

    /// Name too long.
    #[error("Name too long")]
    NameTooLong,

    /// Not a socket.
    #[error("Not a socket")]
    NotASocket,

    /// Other error with a static description.
    #[error("{0}")]
    Other(&'static str),
}

/// Convenience alias for a [`core::result::Result`] with [`KernelError`].
pub type Result<T> = core::result::Result<T, KernelError>;

impl From<Infallible> for KernelError {
    fn from(error: Infallible) -> Self {
        match error {}
    }
}
