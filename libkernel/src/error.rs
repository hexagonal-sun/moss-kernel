use core::convert::Infallible;
use thiserror::Error;

pub mod syscall_error;

#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum ProbeError {
    #[error("No registers present in FDT")]
    NoReg,

    #[error("No register bank size in FDT")]
    NoRegSize,

    #[error("No interrupts in FDT")]
    NoInterrupts,

    #[error("No parent interrupt controller in FDT")]
    NoParentIntterupt,

    #[error("The specified interrupt parent isn't an interrupt controller")]
    NotInterruptController,

    // Driver probing should be tried again after other probes have succeeded.
    #[error("Driver probing deferred for other dependencies")]
    Deferred,
}

#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum MapError {
    #[error("Physical address not page aligned")]
    PhysNotAligned,

    #[error("Physical address not page aligned")]
    VirtNotAligned,

    #[error("Physical and virtual range sizes do not match")]
    SizeMismatch,

    #[error("Failed to walk to the next level page table")]
    WalkFailed,

    #[error("Invalid page table descriptor encountered")]
    InvalidDescriptor,

    #[error("The region to be mapped is smaller than PAGE_SIZE")]
    TooSmall,

    #[error("The VA range is has already been mapped")]
    AlreadyMapped,

    #[error("Page table does not contain an L3 mapping")]
    NotL3Mapped,
}

#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum IoError {
    #[error("The requested I/O operation was out of bounds for the block device")]
    OutOfBounds,

    #[error("Courruption found in the filesystem metadata")]
    MetadataCorruption,
}

#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum FsError {
    #[error("The path or file was not found")]
    NotFound,

    #[error("The path component is not a directory.")]
    NotADirectory,

    #[error("The path component is a directory.")]
    IsADirectory,

    #[error("The file or directory already exists.")]
    AlreadyExists,

    #[error("The directory is not empty.")]
    DirectoryNotEmpty,

    #[error("Invalid input parameters.")]
    InvalidInput,

    #[error("The filesystem is corrupted or has an invalid format.")]
    InvalidFs,

    #[error("Attempted to access data out of bounds.")]
    OutOfBounds,

    #[error("The operation is not permitted.")]
    PermissionDenied,

    #[error("Could not find the specified FS driver")]
    DriverNotFound,

    #[error("Too many open files")]
    TooManyFiles,

    #[error("The device could not be found")]
    NoDevice,

    #[error("Too many symbolic links encountered")]
    Loop,

    #[error("Attempted to rename from cross device")]
    CrossDevice,
}

#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum ExecError {
    #[error("Invalid ELF Format")]
    InvalidElfFormat,

    #[error("Invalid Porgram Header Format")]
    InvalidPHdrFormat,
}

#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum KernelError {
    #[error("Cannot allocate memory")]
    NoMemory,

    #[error("Memory region not found")]
    NoMemRegion,

    #[error("Invalid value")]
    InvalidValue,

    #[error("The current resource is already in use")]
    InUse,

    #[error("Page table mapping failed: {0}")]
    MappingError(#[from] MapError),

    #[error("Provided object is too large")]
    TooLarge,

    #[error("Operation not supported")]
    NotSupported,

    #[error("Device probe failed: {0}")]
    Probe(#[from] ProbeError),

    #[error("I/O operation failed: {0}")]
    Io(#[from] IoError),

    #[error("Filesystem operation failed: {0}")]
    Fs(#[from] FsError),

    #[error("Exec error: {0}")]
    Exec(#[from] ExecError),

    #[error("Not a tty")]
    NotATty,

    #[error("Fault errror during syscall")]
    Fault,

    #[error("Not an open file descriptor")]
    BadFd,

    #[error("Cannot seek on a pipe")]
    SeekPipe,

    #[error("Broken pipe")]
    BrokenPipe,

    #[error("Operation not permitted")]
    NotPermitted,

    #[error("Buffer is full")]
    BufferFull,

    #[error("Operation would block")]
    TryAgain,

    #[error("No such process")]
    NoProcess,

    #[error("Operation timed out")]
    TimedOut,

    #[error("{0}")]
    Other(&'static str),
}

pub type Result<T> = core::result::Result<T, KernelError>;

impl From<Infallible> for KernelError {
    fn from(error: Infallible) -> Self {
        match error {}
    }
}
