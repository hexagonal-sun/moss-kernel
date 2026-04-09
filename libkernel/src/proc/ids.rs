//! User and group identity types.
//!
//! [`Uid`] and [`Gid`] are thin wrappers around `u32` that prevent accidental
//! mixing of user IDs and group IDs at the type level.

/// A user identity (UID).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Uid(u32);

impl Uid {
    /// Creates a new `Uid` with the given numeric value.
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Returns `true` if this is the root (UID 0) user.
    pub fn is_root(self) -> bool {
        self.0 == 0
    }

    /// Returns the root UID (0).
    pub fn new_root() -> Self {
        Self(0)
    }
}

impl From<u64> for Uid {
    /// Convenience implementation for syscalls.
    fn from(value: u64) -> Self {
        Self(value as _)
    }
}

impl From<Uid> for u32 {
    fn from(value: Uid) -> Self {
        value.0
    }
}

/// A group identity (GID).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gid(u32);

impl Gid {
    /// Creates a new `Gid` with the given numeric value.
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Returns the root group GID (0).
    pub fn new_root_group() -> Self {
        Self(0)
    }
}

impl From<u64> for Gid {
    /// Convenience implementation for syscalls.
    fn from(value: u64) -> Self {
        Self(value as _)
    }
}

impl From<Gid> for u32 {
    fn from(value: Gid) -> Self {
        value.0
    }
}
