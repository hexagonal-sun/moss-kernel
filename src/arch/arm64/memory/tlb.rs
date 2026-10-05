use core::arch::asm;

use libkernel::memory::paging::{TLBInvalidator, TranslationChange};

pub struct AllEl1TlbInvalidator;

impl AllEl1TlbInvalidator {
    pub fn new() -> Self {
        Self
    }
}

impl Drop for AllEl1TlbInvalidator {
    fn drop(&mut self) {
        invalidate_all();
    }
}

impl TLBInvalidator for AllEl1TlbInvalidator {
    fn prepare(&self, _change: &TranslationChange) {
        order_table_stores();
    }

    fn invalidate(&self, _change: &TranslationChange) {
        invalidate_all();
    }

    fn publish(&self, _change: &TranslationChange) {
        publish_tables();
    }
}

pub struct AllEl0TlbInvalidator;

impl AllEl0TlbInvalidator {
    pub fn new() -> Self {
        Self
    }
}

impl Drop for AllEl0TlbInvalidator {
    fn drop(&mut self) {
        invalidate_all();
    }
}

impl TLBInvalidator for AllEl0TlbInvalidator {
    fn prepare(&self, _change: &TranslationChange) {
        order_table_stores();
    }

    fn invalidate(&self, _change: &TranslationChange) {
        invalidate_all();
    }

    fn publish(&self, _change: &TranslationChange) {
        publish_tables();
    }
}

fn invalidate_all() {
    // Conservatively invalidate all levels and ASIDs, including cached blocks
    // and table walks. Range-aware implementations can use TranslationChange.
    unsafe {
        asm!(
            "dsb ishst",
            "tlbi vmalle1is",
            "dsb ish",
            "isb",
            options(nostack, preserves_flags)
        );
    }
}

fn publish_tables() {
    unsafe {
        asm!("dsb ishst", "isb", options(nostack, preserves_flags));
    }
}

fn order_table_stores() {
    unsafe {
        asm!("dsb ishst", options(nostack, preserves_flags));
    }
}
