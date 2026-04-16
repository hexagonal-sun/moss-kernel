//! Test harness for paging unit-tests.

use crate::{
    error::KernelError,
    memory::address::{IdentityTranslator, TPA, TVA},
};

use super::{
    PageAllocator, PageTableMapper, PgTable, PgTableArray, TLBInvalidator, walk::WalkContext,
};

/// A mock TLB invalidator that does nothing for unit testing.
pub struct MockTLBInvalidator;
impl TLBInvalidator for MockTLBInvalidator {}

/// Mock page allocator that allocates on the host heap and uses a counter
/// to simulate memory limits.
pub struct MockPageAllocator {
    pub pages_allocated: usize,
    pub max_pages: usize,
}

impl MockPageAllocator {
    fn new(max_pages: usize) -> Self {
        Self {
            pages_allocated: 0,
            max_pages,
        }
    }
}

impl PageAllocator for MockPageAllocator {
    fn allocate_page_table<T: PgTable>(&mut self) -> crate::error::Result<TPA<PgTableArray<T>>> {
        if self.pages_allocated >= self.max_pages {
            Err(KernelError::NoMemory)
        } else {
            self.pages_allocated += 1;
            // Allocate a page-aligned table on the host heap.
            let layout = std::alloc::Layout::new::<PgTableArray<T>>();

            let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
            if ptr.is_null() {
                panic!("Host failed to allocate memory for test");
            }

            // Return the raw pointer value as our "physical address".
            Ok(TPA::from_value(ptr as usize))
        }
    }
}

/// A mock mapper for host-based testing. It assumes that the "physical
/// address" (TPA) is just a raw pointer from the host's virtual address
/// space, which is true for tests using heap allocation. It performs a
/// direct cast.
pub struct PassthroughMapper;

impl PageTableMapper for PassthroughMapper {
    unsafe fn with_page_table<T: PgTable, R>(
        &mut self,
        pa: TPA<PgTableArray<T>>,
        f: impl FnOnce(TVA<PgTableArray<T>>) -> R,
    ) -> crate::error::Result<R> {
        // The "physical address" in our test is the raw pointer from the heap.
        // Just cast it back and use it.
        Ok(f(pa.to_va::<IdentityTranslator>()))
    }
}

pub struct TestHarness<R: PgTable> {
    pub allocator: MockPageAllocator,
    pub mapper: PassthroughMapper,
    pub invalidator: MockTLBInvalidator,
    pub root_table: TPA<PgTableArray<R>>,
}

impl<R: PgTable> TestHarness<R> {
    pub fn new(max_pages: usize) -> Self {
        let mut allocator = MockPageAllocator::new(max_pages);
        let root_table = allocator.allocate_page_table::<R>().unwrap();

        Self {
            allocator,
            mapper: PassthroughMapper,
            invalidator: MockTLBInvalidator,
            root_table,
        }
    }

    pub fn create_walk_ctx(&mut self) -> WalkContext<'_, PassthroughMapper> {
        WalkContext {
            mapper: &mut self.mapper,
            invalidator: &self.invalidator,
        }
    }
}
