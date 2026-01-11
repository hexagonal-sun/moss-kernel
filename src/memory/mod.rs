use crate::{
    arch::ArchImpl,
    sync::{OnceLock, SpinLock},
};
use libkernel::memory::{
    page_alloc::FrameAllocator,
    region::PhysMemoryRegion,
    smalloc::{RegionList, Smalloc},
};

pub mod brk;
pub mod fault;
pub mod mmap;
pub mod page;
pub mod process_vm;
pub mod uaccess;

pub type PageOffsetTranslator = libkernel::memory::pg_offset::PageOffsetTranslator<ArchImpl>;

// Initial memory allocator. Used for initial memory setup.
const STATIC_REGION_COUNT: usize = 128;

static INIT_MEM_REGIONS: [PhysMemoryRegion; STATIC_REGION_COUNT] =
    [PhysMemoryRegion::empty(); STATIC_REGION_COUNT];
static INIT_RES_REGIONS: [PhysMemoryRegion; STATIC_REGION_COUNT] =
    [PhysMemoryRegion::empty(); STATIC_REGION_COUNT];

pub static INITAL_ALLOCATOR: SpinLock<Option<Smalloc<PageOffsetTranslator>>> =
    SpinLock::new(Some(Smalloc::new(
        RegionList::new(STATIC_REGION_COUNT, INIT_MEM_REGIONS.as_ptr().cast_mut()),
        RegionList::new(STATIC_REGION_COUNT, INIT_RES_REGIONS.as_ptr().cast_mut()),
    )));

// Main page allocator, setup by consuming smalloc.
pub static PAGE_ALLOC: OnceLock<FrameAllocator<ArchImpl>> = OnceLock::new();
