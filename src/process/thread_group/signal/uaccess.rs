use core::mem::transmute;

use libkernel::error::KernelError;

use super::SigId;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UserSigId(u32);

impl UserSigId {
    pub fn optional(self) -> Result<Option<SigId>, KernelError> {
        if self.0 == 0 {
            Ok(None)
        } else {
            self.try_into().map(Some)
        }
    }
}

impl TryFrom<UserSigId> for SigId {
    type Error = KernelError;

    fn try_from(value: UserSigId) -> core::result::Result<Self, Self::Error> {
        if value.0 < 1 || value.0 > 31 {
            Err(KernelError::InvalidValue)
        } else {
            // SAFETY: The above bounds check ensure that the value is within
            // range.
            Ok(unsafe { transmute::<u32, SigId>(value.0 - 1) })
        }
    }
}

impl From<u64> for UserSigId {
    fn from(value: u64) -> Self {
        Self(value as _)
    }
}
