//! TLB invalidation helpers.

/// Trait for invalidating TLB entries after page table modifications.
pub trait TLBInvalidator {}

/// A no-op TLB invalidator used when invalidation is unnecessary.
pub struct NullTlbInvalidator {}

impl TLBInvalidator for NullTlbInvalidator {}
