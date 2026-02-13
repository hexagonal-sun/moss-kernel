use core::fmt;

use super::proc_vm::vmarea::VMAPermissions;

/// Represents the memory permissions for a virtual memory mapping.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct PtePermissions {
    read: bool,
    write: bool,
    execute: bool,
    user: bool,
    cow: bool,
}

impl From<VMAPermissions> for PtePermissions {
    fn from(value: VMAPermissions) -> Self {
        Self {
            read: value.read,
            write: value.write,
            execute: value.execute,
            user: true, // VMAs only represent user address spaces.
            cow: false, // a VMA will only be COW when it's cloned.
        }
    }
}

impl PtePermissions {
    /// Creates a new `PtePermissions` from its raw boolean components.
    ///
    /// This constructor is intended exclusively for use by the
    /// architecture-specific MMU implementation when decoding a raw page table
    /// entry. It is marked `pub(crate)` to prevent its use outside the
    /// libkernel crate, preserving the safety invariants for the rest of the
    /// kernel.
    #[inline]
    pub(crate) const fn from_raw_bits(
        read: bool,
        write: bool,
        execute: bool,
        user: bool,
        cow: bool,
    ) -> Self {
        debug_assert!(
            !(write && cow),
            "PTE permissions cannot be simultaneously writable and CoW"
        );

        Self {
            read,
            write,
            execute,
            user,
            cow,
        }
    }

    /// Creates a new read-only permission set.
    pub const fn ro(user: bool) -> Self {
        Self {
            read: true,
            write: false,
            execute: false,
            user,
            cow: false,
        }
    }

    /// Creates a new read-write permission set.
    pub const fn rw(user: bool) -> Self {
        Self {
            read: true,
            write: true,
            execute: false,
            user,
            cow: false,
        }
    }

    /// Creates a new read-execute permission set.
    pub const fn rx(user: bool) -> Self {
        Self {
            read: true,
            write: false,
            execute: true,
            user,
            cow: false,
        }
    }

    /// Creates a new read-write-execute permission set.
    pub const fn rwx(user: bool) -> Self {
        Self {
            read: true,
            write: true,
            execute: true,
            user,
            cow: false,
        }
    }

    /// Returns `true` if the mapping is readable.
    pub const fn is_read(&self) -> bool {
        self.read
    }

    /// Returns `true` if the mapping is writable. This will be `false` for a
    /// CoW mapping.
    pub const fn is_write(&self) -> bool {
        self.write
    }

    /// Returns `true` if the mapping is executable.
    pub const fn is_execute(&self) -> bool {
        self.execute
    }

    /// Returns `true` if the mapping is accessible from user space.
    pub const fn is_user(&self) -> bool {
        self.user
    }

    /// Returns `true` if the mapping is a Copy-on-Write mapping.
    pub const fn is_cow(&self) -> bool {
        self.cow
    }

    /// Converts a writable permission set into its Copy-on-Write equivalent.
    ///
    /// This method enforces the invariant that a mapping cannot be both
    /// writable and CoW by explicitly setting `write` to `false`.
    ///
    /// # Example
    /// ```
    /// use libkernel::memory::permissions::PtePermissions;
    ///
    /// let perms = PtePermissions::rw(true);
    /// let cow_perms = perms.into_cow();
    /// assert!(!cow_perms.is_write());
    /// assert!(cow_perms.is_cow());
    /// assert!(cow_perms.is_read());
    /// ```
    ///
    /// # Panics
    ///
    /// Panics in debug builds if the permissions are not originally writable,
    /// as it is a logical error to make a non-writable page Copy-on-Write.
    pub fn into_cow(self) -> Self {
        debug_assert!(self.write, "Cannot make a non-writable mapping CoW");
        Self {
            write: false,
            cow: true,
            ..self
        }
    }

    /// Converts a Copy-on-Write permission set back into a writable one.
    ///
    /// This is used by the page fault handler after a page has been copied or
    /// exclusively claimed. It makes the page writable by the hardware and
    /// removes the kernel's `CoW` marker.
    ///
    /// # Example
    /// ```
    /// use libkernel::memory::permissions::PtePermissions;
    ///
    /// let cow_perms = PtePermissions::rw(true).into_cow();
    /// let writable_perms = cow_perms.from_cow();
    /// assert!(writable_perms.is_write());
    /// assert!(!writable_perms.is_cow());
    /// assert!(writable_perms.is_read());
    /// ```
    ///
    /// # Panics
    ///
    /// Panics in debug builds if the permissions are not CoW, as this indicates a
    /// logic error in the fault handler.
    pub fn from_cow(self) -> Self {
        debug_assert!(self.cow, "Cannot make a non-CoW mapping writable again");
        Self {
            write: true, // The invariant is enforced here.
            cow: false,
            ..self
        }
    }
}

impl fmt::Display for PtePermissions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let r = if self.read { 'r' } else { '-' };
        let x = if self.execute { 'x' } else { '-' };
        let user = if self.user { 'u' } else { 'k' };

        // Display 'w' for writable, or 'c' for CoW. The invariant guarantees
        // that `self.write` and `self.cow` cannot both be true.
        let w_or_c = if self.write {
            'w'
        } else if self.cow {
            'c'
        } else {
            '-'
        };

        write!(f, "{r}{w_or_c}{x} {user}")
    }
}

impl fmt::Debug for PtePermissions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemPermissions")
            .field("read", &self.read)
            .field("write", &self.write)
            .field("execute", &self.execute)
            .field("user", &self.user)
            .field("cow", &self.cow)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constructors() {
        let p = PtePermissions::rw(true);
        assert!(p.is_read());
        assert!(p.is_write());
        assert!(!p.is_execute());
        assert!(p.is_user());
        assert!(!p.is_cow());

        let p = PtePermissions::rx(false);
        assert!(p.is_read());
        assert!(!p.is_write());
        assert!(p.is_execute());
        assert!(!p.is_user());
        assert!(!p.is_cow());
    }

    #[test]
    fn test_cow_transition() {
        let p_rw = PtePermissions::rw(true);
        let p_cow = p_rw.into_cow();

        // Check CoW state
        assert!(p_cow.is_read());
        assert!(!p_cow.is_write());
        assert!(!p_cow.is_execute());
        assert!(p_cow.is_user());
        assert!(p_cow.is_cow());

        // Transition back
        let p_final = p_cow.from_cow();
        assert_eq!(p_rw, p_final);
    }

    #[test]
    #[should_panic]
    fn test_into_cow_panic() {
        // Cannot make a read-only page CoW
        let p_ro = PtePermissions::ro(true);
        let _ = p_ro.into_cow();
    }

    #[test]
    #[should_panic]
    fn test_from_cow_panic() {
        // Cannot convert a non-CoW page from CoW
        let p_rw = PtePermissions::rw(true);
        let _ = p_rw.from_cow();
    }

    #[test]
    fn test_display_format() {
        assert_eq!(format!("{}", PtePermissions::rw(true)), "rw- u");
        assert_eq!(format!("{}", PtePermissions::rwx(false)), "rwx k");
        assert_eq!(format!("{}", PtePermissions::ro(true)), "r-- u");
        assert_eq!(format!("{}", PtePermissions::rx(false)), "r-x k");

        let cow_perms = PtePermissions::rw(true).into_cow();
        assert_eq!(format!("{}", cow_perms), "rc- u");

        let cow_exec_perms = PtePermissions::rwx(false).into_cow();
        assert_eq!(format!("{}", cow_exec_perms), "rcx k");
    }
}
