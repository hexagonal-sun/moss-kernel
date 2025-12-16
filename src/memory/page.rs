use super::{PAGE_ALLOC, PageOffsetTranslator};
use crate::arch::ArchImpl;
use libkernel::memory::page_alloc::PageAllocGetter;

pub struct PgAllocGetter {}

impl PageAllocGetter<ArchImpl> for PgAllocGetter {
    fn global_page_alloc() -> &'static libkernel::sync::once_lock::OnceLock<
        libkernel::memory::page_alloc::FrameAllocator<ArchImpl>,
        ArchImpl,
    > {
        &PAGE_ALLOC
    }
}

pub type ClaimedPage = libkernel::memory::page::ClaimedPage<ArchImpl, PgAllocGetter, PageOffsetTranslator>;
