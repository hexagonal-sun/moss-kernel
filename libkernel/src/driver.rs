//! Device descriptor types.

/// A major/minor pair identifying a character or block device.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct CharDevDescriptor {
    /// The major device number (identifies the driver).
    pub major: u64,
    /// The minor device number (identifies the device instance).
    pub minor: u64,
}

impl CharDevDescriptor {
    /// Encodes this device descriptor into a Linux-style `dev_t` value.
    pub const fn dev_t(self) -> u64 {
        (self.minor & 0xff)
            | ((self.major & 0xfff) << 8)
            | ((self.minor & !0xff) << 12)
            | ((self.major & !0xfff) << 32)
    }
}
