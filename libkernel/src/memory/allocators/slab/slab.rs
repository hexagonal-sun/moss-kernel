use super::SLAB_SIZE_BYTES;
use crate::{
    CpuOps,
    memory::{
        address::{AddressTranslator, VA},
        allocators::{phys::PageAllocation, slab::SLAB_MAX_OBJ_SHIFT},
        region::VirtMemoryRegion,
    },
};

#[derive(Debug, Clone)]
pub struct Slab {
    obj_shift: usize,
    num_free: usize,
    next_free: Option<u16>,
    base: VA,
}

#[derive(PartialEq, Eq, Debug)]
pub enum SlabState {
    Free,
    Partial,
    Full,
}

impl Slab {
    pub fn new<T: AddressTranslator<()>, CPU: CpuOps>(
        alloc: &PageAllocation<'_, CPU>,
        obj_shift: usize,
    ) -> Self {
        assert_eq!(alloc.region().size(), SLAB_SIZE_BYTES);

        // We need *at least* a u16 for free list tracking.
        assert!(obj_shift >= 1);

        // We don't go bigger than 4 pages.
        assert!(obj_shift <= SLAB_MAX_OBJ_SHIFT as usize);

        let num_objs = SLAB_SIZE_BYTES >> obj_shift;

        // Write free list at object slots.
        let va = alloc.region().start_address().to_va::<T>();

        let base = va.cast::<u16>().as_ptr_mut();

        for i in 0..num_objs {
            unsafe {
                base.byte_add(i * (1 << obj_shift))
                    .write(if i == num_objs - 1 {
                        // Sential value for no next list.
                        u16::MAX
                    } else {
                        (i + 1) as u16
                    })
            }
        }

        Self {
            obj_shift,
            num_free: num_objs,
            next_free: Some(0),
            base: va,
        }
    }

    fn calc_obj_idx(&mut self, idx: u16) -> VA {
        self.base.add_bytes(idx as usize * (1 << self.obj_shift))
    }

    pub fn alloc_object(&mut self) -> Option<*mut u8> {
        if self.num_free == 0 {
            return None;
        }

        let va = self.calc_obj_idx(self.next_free.unwrap());

        let next_free = unsafe { va.cast::<u16>().as_ptr().read() };

        self.next_free = if next_free == u16::MAX {
            None
        } else {
            Some(next_free)
        };

        self.num_free -= 1;

        Some(va.cast::<u8>().as_ptr_mut())
    }

    pub fn put_object(&mut self, ptr: *mut u8) {
        let va = VA::from_ptr_mut(ptr.cast());
        // Eneusre ptr is within our slab.
        assert!(VirtMemoryRegion::new(self.base, SLAB_SIZE_BYTES).contains_address(va));

        let idx = (va.value() - self.base.value()) >> self.obj_shift;

        unsafe { ptr.cast::<u16>().write(self.next_free.unwrap_or(u16::MAX)) };

        self.num_free += 1;
        self.next_free = Some(idx as u16);
    }

    fn capacity(&self) -> usize {
        SLAB_SIZE_BYTES >> self.obj_shift
    }

    pub fn state(&self) -> SlabState {
        if self.num_free == 0 {
            SlabState::Full
        } else if self.num_free == self.capacity() {
            SlabState::Free
        } else {
            SlabState::Partial
        }
    }

    pub fn obj_shift(&self) -> usize {
        self.obj_shift
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::{
            address::IdentityTranslator,
            allocators::{phys::tests::TestFixture, slab::SLAB_FRAME_ALLOC_ORDER},
        },
        test::MockCpuOps,
    };
    use core::ptr;

    /// Helper to create a standard test fixture for Slab testing
    fn create_slab_fixture() -> TestFixture {
        // Create a region large enough for a few slabs
        TestFixture::new(&[(0, 1 << 25)], &[])
    }

    #[test]
    fn slab_init_layout() {
        let fixture = create_slab_fixture();
        dbg!(fixture.allocator.free_pages());
        let alloc = fixture
            .allocator
            .alloc_frames(SLAB_FRAME_ALLOC_ORDER as _)
            .unwrap();

        // Create a slab with object size 32 (2^5) (512 objects)
        let obj_shift = 5;
        let slab = Slab::new::<IdentityTranslator, MockCpuOps>(&alloc, obj_shift);

        assert_eq!(slab.state(), SlabState::Free);
        assert_eq!(slab.num_free, 512);
        assert_eq!(slab.next_free, Some(0));

        // Verify the linked list construction.
        let base_ptr = alloc.region().start_address().value() as *const u16;

        unsafe {
            // Index 0 should point to 1
            assert_eq!(*base_ptr, 1);
            // Index 1 (at byte offset 32) should point to 2
            // 32 bytes = 16 * u16
            assert_eq!(*base_ptr.byte_add(32), 2);
            // The last index (127) should point to MAX (sentinel)
            assert_eq!(*base_ptr.byte_add(511 * 32), u16::MAX);
        }
    }

    #[test]
    fn slab_alloc_free_basic() {
        let fixture = create_slab_fixture().leak_allocator();
        let alloc = fixture.alloc_frames(SLAB_FRAME_ALLOC_ORDER as _).unwrap();
        let mut slab = Slab::new::<IdentityTranslator, MockCpuOps>(&alloc, 6); // 64 byte objects

        // Allocate first object (Index 0)
        let ptr1 = slab.alloc_object().unwrap();
        assert_eq!(slab.num_free, (SLAB_SIZE_BYTES >> 6) - 1);
        assert_eq!(slab.state(), SlabState::Partial);

        // Allocate second object (Index 1)
        let ptr2 = slab.alloc_object().unwrap();
        assert_eq!(slab.num_free, (SLAB_SIZE_BYTES >> 6) - 2);

        // Verify pointers are distinct and spaced correctly
        assert_eq!(unsafe { ptr1.byte_add(64) }, ptr2);

        // Free the first object (Index 0).
        slab.put_object(ptr1);
        assert_eq!(slab.num_free, (SLAB_SIZE_BYTES >> 6) - 1);
        assert_eq!(slab.next_free, Some(0));
        assert_eq!(unsafe { *(ptr1 as *const u16) }, 2);

        // Re-allocate. Should get Index 0 back.
        let ptr3 = slab.alloc_object().unwrap();
        assert_eq!(ptr1, ptr3);
        assert_eq!(slab.next_free, Some(2));

        // Clean up remainder
        slab.put_object(ptr2);
        slab.put_object(ptr3);
        assert_eq!(slab.state(), SlabState::Free);
    }

    #[test]
    fn slab_exhaustion() {
        let fixture = create_slab_fixture().leak_allocator();
        let alloc = fixture.alloc_frames(SLAB_FRAME_ALLOC_ORDER as _).unwrap();

        // Large objects: 4096 bytes (1 page) (order 12).
        let mut slab = Slab::new::<IdentityTranslator, MockCpuOps>(&alloc, 12);
        let capacity = 4;

        let mut ptrs = Vec::new();

        // Fill it up
        for _ in 0..capacity {
            assert_ne!(slab.state(), SlabState::Full);
            if let Some(ptr) = slab.alloc_object() {
                ptrs.push(ptr);
            }
        }

        assert_eq!(ptrs.len(), capacity);
        assert_eq!(slab.state(), SlabState::Full);
        assert_eq!(slab.next_free, None);

        // Try to alloc one more
        assert!(slab.alloc_object().is_none());

        // Free one
        let last = ptrs.pop().unwrap();
        slab.put_object(last);
        assert_eq!(slab.state(), SlabState::Partial);
        assert!(slab.next_free.is_some());

        // Allocate it back
        let new_ptr = slab.alloc_object().unwrap();
        assert_eq!(new_ptr, last);
        assert_eq!(slab.state(), SlabState::Full);
    }

    #[test]
    fn slab_free_out_of_order() {
        let fixture = create_slab_fixture().leak_allocator();
        let alloc = fixture.alloc_frames(SLAB_FRAME_ALLOC_ORDER as _).unwrap();

        // 2048 byte objects (8 objects total)
        let mut slab = Slab::new::<IdentityTranslator, MockCpuOps>(&alloc, 11);

        let mut ptrs = Vec::new();
        for _ in 0..8 {
            ptrs.push(slab.alloc_object().unwrap());
        }

        assert_eq!(slab.state(), SlabState::Full);

        // Free evens
        slab.put_object(ptrs[0]);
        slab.put_object(ptrs[2]);
        slab.put_object(ptrs[4]);
        slab.put_object(ptrs[6]);

        assert_eq!(slab.num_free, 4);

        // Free odds
        slab.put_object(ptrs[1]);
        slab.put_object(ptrs[3]);
        slab.put_object(ptrs[5]);
        slab.put_object(ptrs[7]);

        assert_eq!(slab.state(), SlabState::Free);
        assert_eq!(slab.num_free, 8);

        let p = slab.alloc_object().unwrap();
        assert_eq!(p, ptrs[7]);
    }

    #[test]
    fn slab_boundary_integrity() {
        let fixture = create_slab_fixture().leak_allocator();
        let alloc = fixture.alloc_frames(SLAB_FRAME_ALLOC_ORDER as _).unwrap();

        // 128 byte objects -> 128 objects
        let mut slab = Slab::new::<IdentityTranslator, MockCpuOps>(&alloc, 7);

        // Allocate all objects and write a specific pattern to them
        let mut ptrs = Vec::new();
        for i in 0..128 {
            let ptr = slab.alloc_object().unwrap();
            unsafe {
                // Fill the object with a pattern
                ptr::write_bytes(ptr, (i as u8) + 1, 128);
            }
            ptrs.push((i + 1, ptr));
        }

        // Verify patterns remain intact (no overlapping writes during
        // allocation tracking)
        for (i, ptr) in ptrs.iter() {
            unsafe {
                let slice = core::slice::from_raw_parts(*ptr, 128);
                for byte in slice {
                    assert_eq!(*byte, (*i as u8));
                }
            }
        }

        // Free every odd index from the list and verify integrity of even data.
        ptrs.retain(|(i, ptr)| {
            if i % 2 == 0 {
                slab.put_object(*ptr);
                return false;
            } else {
                return true;
            }
        });

        for (i, ptr) in ptrs.iter() {
            unsafe {
                let slice = core::slice::from_raw_parts(*ptr, 128);
                for byte in slice {
                    assert_eq!(*byte, (*i as u8));
                }
            }
        }
    }

    #[test]
    fn slab_put_pointer_calculation() {
        let fixture = create_slab_fixture().leak_allocator();
        let alloc = fixture.alloc_frames(SLAB_FRAME_ALLOC_ORDER as _).unwrap();
        let base_addr = alloc.region().start_address().value();

        // 256 byte objects
        let mut slab = Slab::new::<IdentityTranslator, MockCpuOps>(&alloc, 8);

        // Manually create a pointer that corresponds to Index 3
        // Base + 3 * 256
        let ptr_val = base_addr + (3 * 256);
        let ptr = ptr_val as *mut u8;

        // Force the internal state to look like we allocated everything
        while slab.alloc_object().is_some() {}
        assert_eq!(slab.state(), SlabState::Full);

        slab.put_object(ptr);

        assert_eq!(slab.num_free, 1);
        assert_eq!(slab.next_free, Some(3));

        // Re-allocating should return exactly that pointer
        let new_ptr = slab.alloc_object().unwrap();
        assert_eq!(new_ptr, ptr);
    }
}
