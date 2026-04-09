//! Device descriptor types.

/// A major/minor pair identifying a character or block device.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct CharDevDescriptor {
    /// The major device number (identifies the driver).
    pub major: u64,
    /// The minor device number (identifies the device instance).
    pub minor: u64,
}
