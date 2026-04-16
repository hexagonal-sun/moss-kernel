//! AArch64 page table entry (PTE) descriptor types and traits.

use paste::paste;
use tock_registers::interfaces::{ReadWriteable, Readable};
use tock_registers::{register_bitfields, registers::InMemoryRegister};

use crate::memory::PAGE_SHIFT;
use crate::memory::address::{PA, TPA, VA};
use crate::memory::paging::permissions::PtePermissions;
use crate::memory::paging::{PaMapper, PageTableEntry, PgTableArray, TableMapper};
use crate::memory::region::PhysMemoryRegion;

use super::pg_tables::{L1Table, L2Table, L3Table};

#[derive(Clone, Copy)]
struct TableAddr(PA);

impl TableAddr {
    fn as_raw_parts(&self) -> u64 {
        (self.0.value() as u64) & !((1 << PAGE_SHIFT) - 1)
    }

    fn from_raw_parts(v: u64) -> Self {
        Self(PA::from_value(v as usize & !((1 << PAGE_SHIFT) - 1)))
    }
}

/// The memory type attribute applied to a page table mapping.
#[derive(Debug, Clone, Copy)]
pub enum MemoryType {
    /// Device (non-cacheable, non-reorderable) memory.
    Device,
    /// Normal (cacheable) memory.
    Normal,
}

macro_rules! define_descriptor {
    (
        $(#[$outer:meta])*
        $name:ident,
        shift: $shift:literal,
        // Optional: Implement TableMapper if this section is present
        $( table: {
            bits: $table_bits:literal,
            next_level: $next_level:ident,
           },
        )?
        // Optional: Implement PaMapper if this section is present
        $( map: {
                bits: $map_bits:literal,
                oa_len: $oa_len:literal,
            },
        )?
    ) => {
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        $(#[$outer])*
        pub struct $name(u64);

        impl PageTableEntry for $name {
            type RawDescriptor = u64;
            const INVALID: u64 = 0;
            const MAP_SHIFT: usize = $shift;

            fn is_valid(self) -> bool { (self.0 & 0b11) != 0 }
            fn as_raw(self) -> Self::RawDescriptor { self.0 }
            fn from_raw(v: Self::RawDescriptor) -> Self { Self(v) }
        }

        $(
            impl TableMapper for $name {
                type NextLevel = $next_level;

                fn next_table_address(self) -> Option<TPA<PgTableArray<Self::NextLevel>>> {
                    if (self.0 & 0b11) == $table_bits {
                        Some(TableAddr::from_raw_parts(self.0).0.cast())
                    } else {
                        None
                    }
                }

                fn new_next_table(pa: TPA<PgTableArray<Self::NextLevel>>) -> Self {
                    Self(TableAddr(pa.to_untyped()).as_raw_parts() | $table_bits)
                }
            }
        )?

        $(
            paste! {
                #[allow(non_snake_case)]
                mod [<$name Fields>] {
                    use super::*;
                    register_bitfields![u64,
                        pub BlockPageFields [
                            ATTR_INDEX OFFSET(2) NUMBITS(3) [],
                            AP OFFSET(6) NUMBITS(2) [ RW_EL1 = 0b00, RW_EL0 = 0b01, RO_EL1 = 0b10, RO_EL0 = 0b11 ],
                            SH OFFSET(8) NUMBITS(2) [ NonShareable = 0b00, Unpredictable = 0b01, OuterShareable = 0b10, InnerShareable = 0b11 ],
                            AF OFFSET(10) NUMBITS(1) [ Accessed = 1 ],
                            PXN OFFSET(53) NUMBITS(1) [ NotExecutableAtEL1 = 1, ExecutableAtEL1 = 0 ],
                            XN OFFSET(54) NUMBITS(1) [ NotExecutable = 1, Executable = 0 ],
                            // Software defined bit
                            COW OFFSET(55) NUMBITS(1) [ CowShared = 1, NotCowShared = 0 ],
                            OUTPUT_ADDR OFFSET($shift) NUMBITS($oa_len) []
                        ]
                    ];
                }

            impl $name {
                /// Returns the interpreted permissions if this is a block/page
                /// descriptor.
                pub fn permissions(self) -> Option<PtePermissions> {
                    // Check if the descriptor bits match the block/page type
                    if (self.0 & 0b11) != $map_bits {
                        return None;
                    }

                    let reg = InMemoryRegister::new(self.0);
                    let ap_val = reg.read([<$name Fields>]::BlockPageFields::AP);

                    let (write, user) = match ap_val {
                        0b00 => (true, false),  // RW_EL1
                        0b01 => (true, true),   // RW_EL0
                        0b10 => (false, false), // RO_EL1
                        0b11 => (false, true),  // RO_EL0
                        _ => unreachable!(),
                    };

                    let xn = reg.is_set([<$name Fields>]::BlockPageFields::XN);
                    let cow = reg.is_set([<$name Fields>]::BlockPageFields::COW);

                    let execute = !xn;

                    Some(PtePermissions::from_raw_bits(
                        true, // Always true if valid
                        write,
                        execute,
                        user,
                        cow,
                    ))
                }

                /// Returns a new descriptor with the given permissions applied.
                pub fn set_permissions(self, perms: PtePermissions) -> Self {
                    let reg = InMemoryRegister::new(self.0);
                    use [<$name Fields>]::BlockPageFields;


                    let ap = match (perms.is_user(), perms.is_write()) {
                        (false, true) => BlockPageFields::AP::RW_EL1,
                        (true, true) => BlockPageFields::AP::RW_EL0,
                        (false, false) => BlockPageFields::AP::RO_EL1,
                        (true, false) => BlockPageFields::AP::RO_EL0,
                    };

                    reg.modify(ap);

                    if !perms.is_execute() {
                        reg.modify(BlockPageFields::XN::NotExecutable + BlockPageFields::PXN::NotExecutableAtEL1);
                    } else {
                        reg.modify(BlockPageFields::XN::Executable + BlockPageFields::PXN::ExecutableAtEL1);
                    }

                    if perms.is_cow() {
                        reg.modify(BlockPageFields::COW::CowShared)
                    } else {
                        reg.modify(BlockPageFields::COW::NotCowShared)
                    }

                    Self(reg.get())
                }
            }

            impl PaMapper for $name {
                type MemoryType = MemoryType;

                fn could_map(region: PhysMemoryRegion, va: VA) -> bool {
                    let is_aligned = |addr: usize| (addr & ((1 << Self::MAP_SHIFT) - 1)) == 0;
                    is_aligned(region.start_address().value())
                        && is_aligned(va.value())
                        && region.size() >= (1 << Self::MAP_SHIFT)
                }

                fn new_map_pa(page_address: PA, memory_type: MemoryType, perms: PtePermissions) -> Self {
                    let is_aligned = |addr: usize| (addr & ((1 << Self::MAP_SHIFT) - 1)) == 0;
                    if !is_aligned(page_address.value()) {
                        panic!("Cannot map non-aligned physical address");
                    }

                    let reg = InMemoryRegister::new(0);
                    use [<$name Fields>]::BlockPageFields;

                    reg.modify(BlockPageFields::OUTPUT_ADDR.val((page_address.value() >> Self::MAP_SHIFT) as u64)
                        + BlockPageFields::AF::Accessed);

                    match memory_type {
                        MemoryType::Device => {
                            reg.modify(BlockPageFields::SH::NonShareable + BlockPageFields::ATTR_INDEX.val(1));
                        }
                        MemoryType::Normal => {
                            reg.modify(BlockPageFields::SH::InnerShareable + BlockPageFields::ATTR_INDEX.val(0));
                        }
                    }

                    Self(reg.get() | $map_bits).set_permissions(perms)
                }

                fn mapped_address(self) -> Option<PA> {
                    use [<$name Fields>]::BlockPageFields;

                    match self.0 & 0b11 {
                        0b00 => return None,
                        // Swapped out page.
                        0b10 =>  {},
                        $map_bits => {},
                        _ => return None,
                    }

                    let reg = InMemoryRegister::new(self.0);
                    let addr = reg.read(BlockPageFields::OUTPUT_ADDR);
                    Some(PA::from_value((addr << Self::MAP_SHIFT) as usize))
                }
            }
            }
        )?
    };
}

define_descriptor!(
    /// A Level 0 descriptor. Can only be an invalid or table descriptor.
    L0Descriptor,
    shift: 39,
    table: {
        bits: 0b11,
        next_level: L1Table,
    },
);

define_descriptor!(
    /// A Level 1 descriptor. Can be a block, table, or invalid descriptor.
    L1Descriptor,
    shift: 30,
    table: {
        bits: 0b11,
        next_level: L2Table,
    },
    map: {
        bits: 0b01,    // L1 Block descriptor has bits[1:0] = 01
        oa_len: 18,    // Output address length for 48-bit PA
    },
);

define_descriptor!(
    /// A Level 2 descriptor. Can be a block, table, or invalid descriptor.
    L2Descriptor,
    shift: 21,
    table: {
        bits: 0b11,
        next_level: L3Table,
    },
    map: {
        bits: 0b01,    // L2 Block descriptor has bits[1:0] = 01
        oa_len: 27,    // Output address length for 48-bit PA
    },
);

define_descriptor!(
    /// A Level 3 descriptor. Can be a page or invalid descriptor.
    L3Descriptor,
    shift: 12,
    // Note: No 'table' capability at L3.
    map: {
        bits: 0b11,    // L3 Page descriptor has bits[1:0] = 11
        oa_len: 36,    // Output address length for 48-bit PA
    },
);

/// The decoded state of an L3 page descriptor.
pub enum L3DescriptorState {
    /// The entry is not present.
    Invalid,
    /// The entry has been swapped out but retains address information.
    Swapped,
    /// The entry is a valid page mapping.
    Valid,
}

impl L3Descriptor {
    const SWAPPED_BIT: u64 = 1 << 1;
    const STATE_MASK: u64 = 0b11;

    /// Checks if this is a non-present entry (e.g., PROT_NONE or paged to
    /// disk).
    pub fn state(self) -> L3DescriptorState {
        match self.0 & Self::STATE_MASK {
            0b00 => L3DescriptorState::Invalid,
            0b10 => L3DescriptorState::Swapped,
            0b01 => L3DescriptorState::Invalid,
            0b11 => L3DescriptorState::Valid,
            _ => unreachable!(),
        }
    }

    /// Mark an existing PTE as swapped (invalid but containing valid
    /// information).
    pub fn mark_as_swapped(self) -> Self {
        Self(Self::SWAPPED_BIT | (self.0 & !Self::STATE_MASK))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::region::PhysMemoryRegion;
    use crate::memory::{PAGE_SHIFT, PAGE_SIZE};

    const KERNEL_PERMS: bool = false;
    const USER_PERMS: bool = true;

    #[test]
    fn test_invalid_descriptor() {
        let d = L0Descriptor::invalid();
        assert!(!d.is_valid());
        assert_eq!(d.as_raw(), 0);
    }

    #[test]
    fn test_l0_table_descriptor() {
        let pa = PA::from_value(0x1000_0000);
        let d = L0Descriptor::new_next_table(pa.cast());

        assert!(d.is_valid());
        assert_eq!(d.as_raw(), 0x1000_0000 | 0b11);
        assert_eq!(d.next_table_address().map(|x| x.to_untyped()), Some(pa));
    }

    #[test]
    fn test_l1_table_descriptor() {
        let pa = PA::from_value(0x2000_0000);
        let d = L1Descriptor::new_next_table(pa.cast());

        assert!(d.is_valid());
        assert_eq!(d.as_raw(), 0x2000_0000 | 0b11);
        assert_eq!(d.next_table_address().map(|x| x.to_untyped()), Some(pa));
        assert!(d.mapped_address().is_none());
        assert!(d.permissions().is_none());
    }

    #[test]
    fn test_l1_block_creation() {
        let pa = PA::from_value(1 << 30); // 1GiB aligned
        let perms = PtePermissions::rw(KERNEL_PERMS);

        let d = L1Descriptor::new_map_pa(pa, MemoryType::Normal, perms);

        assert!(d.is_valid());
        assert_eq!(d.as_raw() & 0b11, 0b01); // Is a block descriptor
        assert!(d.next_table_address().is_none());

        // Check address part (bits [47:30])
        assert_eq!((d.as_raw() >> 30) & 0x3_FFFF, 1);
        // AF bit should be set
        assert_ne!(d.as_raw() & (1 << 10), 0);
    }

    #[test]
    fn test_l1_block_permissions() {
        let pa = PA::from_value(1 << 30);

        let d_krw =
            L1Descriptor::new_map_pa(pa, MemoryType::Normal, PtePermissions::rw(KERNEL_PERMS));
        let d_kro =
            L1Descriptor::new_map_pa(pa, MemoryType::Normal, PtePermissions::ro(KERNEL_PERMS));
        let d_urw =
            L1Descriptor::new_map_pa(pa, MemoryType::Normal, PtePermissions::rw(USER_PERMS));
        let d_uro =
            L1Descriptor::new_map_pa(pa, MemoryType::Normal, PtePermissions::ro(USER_PERMS));
        let d_krwx =
            L1Descriptor::new_map_pa(pa, MemoryType::Normal, PtePermissions::rwx(KERNEL_PERMS));

        assert_eq!(d_krw.permissions(), Some(PtePermissions::rw(KERNEL_PERMS)));
        assert_eq!(d_kro.permissions(), Some(PtePermissions::ro(KERNEL_PERMS)));
        assert_eq!(d_urw.permissions(), Some(PtePermissions::rw(USER_PERMS)));
        assert_eq!(d_uro.permissions(), Some(PtePermissions::ro(USER_PERMS)));
        assert_eq!(
            d_krwx.permissions(),
            Some(PtePermissions::rwx(KERNEL_PERMS))
        );

        // Verify XN bit is NOT set for executable
        assert_eq!(d_krwx.as_raw() & (1 << 54), 0);
        // Verify XN bit IS set for non-executable
        assert_ne!(d_krw.as_raw() & (1 << 54), 0);
    }

    #[test]
    fn test_l1_could_map() {
        let one_gib = 1 << 30;
        let good_region = PhysMemoryRegion::new(PA::from_value(one_gib), one_gib);
        let good_va = VA::from_value(one_gib * 2);

        assert!(L1Descriptor::could_map(good_region, good_va));

        // Bad region size
        let small_region = PhysMemoryRegion::new(PA::from_value(one_gib), one_gib - 1);
        assert!(!L1Descriptor::could_map(small_region, good_va));

        // Bad region alignment
        let unaligned_region = PhysMemoryRegion::new(PA::from_value(one_gib + 1), one_gib);
        assert!(!L1Descriptor::could_map(unaligned_region, good_va));

        // Bad VA alignment
        let unaligned_va = VA::from_value(one_gib + 1);
        assert!(!L1Descriptor::could_map(good_region, unaligned_va));
    }

    #[test]
    #[should_panic]
    fn test_l1_map_unaligned_pa_panics() {
        let pa = PA::from_value((1 << 30) + 1); // Not 1GiB aligned
        let perms = PtePermissions::rw(KERNEL_PERMS);
        L1Descriptor::new_map_pa(pa, MemoryType::Normal, perms);
    }

    #[test]
    fn test_l1_from_raw_roundtrip() {
        let pa = PA::from_value(1 << 30);
        let d = L1Descriptor::new_map_pa(pa, MemoryType::Normal, PtePermissions::rw(false));
        let raw = d.as_raw();
        let decoded = L1Descriptor::from_raw(raw);
        assert_eq!(decoded.as_raw(), d.as_raw());
        assert_eq!(decoded.mapped_address(), d.mapped_address());
        assert_eq!(decoded.permissions(), d.permissions());
    }

    #[test]
    fn test_l2_block_creation() {
        let pa = PA::from_value(2 << 21); // 2MiB aligned
        let perms = PtePermissions::rw(USER_PERMS);

        let d = L2Descriptor::new_map_pa(pa, MemoryType::Normal, perms);

        assert!(d.is_valid());
        assert_eq!(d.as_raw() & 0b11, 0b01); // L2 block
        assert!(d.next_table_address().is_none());
        assert_eq!(d.mapped_address(), Some(pa));
    }

    #[test]
    fn test_l2_block_permissions() {
        let pa = PA::from_value(4 << 21); // 2MiB aligned

        let d_kro =
            L2Descriptor::new_map_pa(pa, MemoryType::Normal, PtePermissions::ro(KERNEL_PERMS));
        let d_krwx =
            L2Descriptor::new_map_pa(pa, MemoryType::Normal, PtePermissions::rwx(KERNEL_PERMS));

        assert_eq!(d_kro.permissions(), Some(PtePermissions::ro(KERNEL_PERMS)));
        assert_eq!(
            d_krwx.permissions(),
            Some(PtePermissions::rwx(KERNEL_PERMS))
        );

        // XN bit for execute = false should not be set
        assert_eq!(d_krwx.as_raw() & (1 << 54), 0);
        // XN bit for execute = false should be set
        assert_ne!(d_kro.as_raw() & (1 << 54), 0);
    }

    #[test]
    fn test_l2_could_map() {
        let size = 1 << 21;
        let good_region = PhysMemoryRegion::new(PA::from_value(size), size);
        let good_va = VA::from_value(size * 3);

        assert!(L2Descriptor::could_map(good_region, good_va));

        let unaligned_pa = PhysMemoryRegion::new(PA::from_value(size + 1), size);
        let unaligned_va = VA::from_value(size + 1);

        assert!(!L2Descriptor::could_map(unaligned_pa, good_va));
        assert!(!L2Descriptor::could_map(good_region, unaligned_va));
    }

    #[test]
    #[should_panic]
    fn test_l2_map_unaligned_pa_panics() {
        let pa = PA::from_value((1 << 21) + 1);
        L2Descriptor::new_map_pa(pa, MemoryType::Normal, PtePermissions::rw(false));
    }

    #[test]
    fn test_l2_from_raw_roundtrip() {
        let pa = PA::from_value(1 << 21);
        let d = L2Descriptor::new_map_pa(pa, MemoryType::Normal, PtePermissions::rx(true));
        let raw = d.as_raw();
        let decoded = L2Descriptor::from_raw(raw);
        assert_eq!(decoded.as_raw(), d.as_raw());
        assert_eq!(decoded.mapped_address(), d.mapped_address());
        assert_eq!(decoded.permissions(), d.permissions());
    }

    #[test]
    fn test_l3_page_creation() {
        let pa = PA::from_value(PAGE_SIZE * 10); // 4KiB aligned
        let perms = PtePermissions::rx(USER_PERMS);

        let d = L3Descriptor::new_map_pa(pa, MemoryType::Normal, perms);

        assert!(d.is_valid());
        assert_eq!(d.as_raw() & 0b11, 0b11); // Is a page descriptor

        // Check address part (bits [47:12])
        assert_eq!(
            (d.as_raw() >> PAGE_SHIFT),
            (pa.value() >> PAGE_SHIFT) as u64
        );
        // AF bit should be set
        assert_ne!(d.as_raw() & (1 << 10), 0);
    }

    #[test]
    fn test_l3_permissions() {
        let pa = PA::from_value(PAGE_SIZE);

        let d_urx =
            L3Descriptor::new_map_pa(pa, MemoryType::Normal, PtePermissions::rx(USER_PERMS));
        assert_eq!(d_urx.permissions(), Some(PtePermissions::rx(USER_PERMS)));

        // Verify XN bit is NOT set for executable
        assert_eq!(d_urx.as_raw() & (1 << 54), 0);
    }

    #[test]
    fn test_l3_could_map() {
        let good_region = PhysMemoryRegion::new(PA::from_value(PAGE_SIZE), PAGE_SIZE);
        let good_va = VA::from_value(PAGE_SIZE * 2);

        assert!(L3Descriptor::could_map(good_region, good_va));

        // Bad region alignment
        let unaligned_region = PhysMemoryRegion::new(PA::from_value(PAGE_SIZE + 1), PAGE_SIZE);
        assert!(!L3Descriptor::could_map(unaligned_region, good_va));
    }

    #[test]
    fn test_l3_from_raw_roundtrip() {
        let pa = PA::from_value(PAGE_SIZE * 8);
        let d = L3Descriptor::new_map_pa(pa, MemoryType::Device, PtePermissions::rw(true));
        let raw = d.as_raw();
        let decoded = L3Descriptor::from_raw(raw);
        assert_eq!(decoded.as_raw(), d.as_raw());
        assert_eq!(decoded.mapped_address(), d.mapped_address());
        assert_eq!(decoded.permissions(), d.permissions());
    }

    #[test]
    fn test_l2_invalid_descriptor() {
        let d = L2Descriptor::invalid();
        assert!(!d.is_valid());
        assert_eq!(d.as_raw(), 0);
        assert!(d.next_table_address().is_none());
        assert!(d.mapped_address().is_none());
        assert!(d.permissions().is_none());
    }
}
