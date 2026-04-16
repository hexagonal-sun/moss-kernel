//! x86_64 page table entry (PTE) descriptor types and traits.

use paste::paste;
use tock_registers::interfaces::{ReadWriteable, Readable};
use tock_registers::{register_bitfields, registers::InMemoryRegister};

use crate::memory::address::{PA, TPA, VA};
use crate::memory::paging::{PaMapper, PgTableArray, TableMapper};
use crate::memory::paging::{PageTableEntry, permissions::PtePermissions};
use crate::memory::region::PhysMemoryRegion;

use super::pg_tables::{PDPTable, PDTable, PTable};

// bits [51:12]
const ADDR_MASK: u64 = 0xFFFFFFFFFF000;

/// The memory type attribute applied to a page table mapping.
#[derive(Debug, Clone, Copy)]
pub enum MemoryType {
    /// Uncacheable memory.
    UC,
    /// Write through (cached) memory,
    WT,
    /// Write back (cached) memory.
    WB,
}

macro_rules! define_descriptor {
    (
        $(#[$outer:meta])*
        $name:ident,
        shift: $shift:literal,
        bit7: [ $($bit7:tt)* ]
    ) => {
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        $(#[$outer])*
        pub struct $name(u64);

        impl PageTableEntry for $name {
            type RawDescriptor = u64;
            const INVALID: Self::RawDescriptor = 0;
            const MAP_SHIFT: usize = $shift;
            fn is_valid(self) -> bool { (self.0 & 0b1) != 0 }
            fn as_raw(self) -> u64 { self.0 }
            fn from_raw(v: u64) -> Self { Self(v) }

        }

        paste! {
            #[allow(non_snake_case)]
            mod [<$name Fields>] {
                use super::*;
                register_bitfields![u64,
                    pub BlockPageFields [
                        P OFFSET(0) NUMBITS(1) [ Present = 1, NotPresent = 0 ],
                        RW OFFSET(1) NUMBITS(1) [ ReadWrite = 1, ReadOnly = 0 ],
                        US OFFSET(2) NUMBITS(1) [ UserAccessAllowed = 1, SupervisorAccessOnly = 0 ],
                        PWT OFFSET(3) NUMBITS(1) [ WriteThrough = 1, WriteBack = 0 ],
                        PCD OFFSET(4) NUMBITS(1) [ NotCacheable = 1, IsCacheable = 0 ],
                        A OFFSET(5) NUMBITS(1) [ Accessed = 1, NotAccessed = 0 ],
                        D OFFSET(6) NUMBITS(1) [ Dirty = 1, Clean = 0 ],
                        $($bit7)*,
                        G OFFSET(8) NUMBITS(1) [ IsGlobal = 1, Local = 0 ],
                        NX OFFSET(63) NUMBITS(1) [ NoExecution = 1, ExecutionAllowed = 0 ],
                    ]
                ];
            }

            impl $name {
                /// Returns the interpreted permissions of this descriptor.
                pub fn permissions(self) -> PtePermissions {
                    let reg = InMemoryRegister::new(self.0);
                    let write = reg.is_set([<$name Fields>]::BlockPageFields::RW);
                    let user = reg.is_set([<$name Fields>]::BlockPageFields::US);
                    let nx = reg.is_set([<$name Fields>]::BlockPageFields::NX);

                    let execute = !nx;

                    PtePermissions::from_raw_bits(
                        true, // Always true if valid
                        write,
                        execute,
                        user,
                        false,
                    )
                }

                /// Returns a new descriptor with the given permissions applied.
                pub fn set_permissions(self, perms: PtePermissions) -> Self {
                    let reg = InMemoryRegister::new(self.0);
                    use [<$name Fields>]::BlockPageFields;

                    if perms.is_user() {
                        reg.modify(BlockPageFields::US::UserAccessAllowed);
                    } else {
                        reg.modify(BlockPageFields::US::SupervisorAccessOnly);
                    }

                    if perms.is_write() {
                        reg.modify(BlockPageFields::RW::ReadWrite);
                    } else {
                        reg.modify(BlockPageFields::RW::ReadOnly)
                    }

                    if perms.is_execute() {
                        reg.modify(BlockPageFields::NX::ExecutionAllowed);
                    } else {
                        reg.modify(BlockPageFields::NX::NoExecution);
                    }

                    Self(reg.get())
                }

                fn address(self) -> PA {
                    PA::from_value((self.0 & ADDR_MASK) as usize)
                }
            }
        }
    }
}

define_descriptor!(
    /// A page-map level 4 entry descriptor. Can only be an invalid or table
    /// descriptor.
    PML4E, shift: 39,
    bit7: [PS OFFSET(7) NUMBITS(1) [MapTable = 0]]
);

define_descriptor!(
    /// A page directory pointer entry. Can be a block, table, or invalid descriptor.
    PDPE, shift: 30,
    bit7: [PS OFFSET(7) NUMBITS(1) [MapPage = 1, MapTable = 0]]
);

define_descriptor!(
    /// A page directory entry. Can be a block, table, or invalid descriptor.
    PDE, shift: 21,
    bit7: [PS OFFSET(7) NUMBITS(1) [MapPage = 1, MapTable = 0]]
);

define_descriptor!(
    /// A page table entry. Can be a page or invalid descriptor.
    PTE, shift: 12,
    bit7: [PAT OFFSET(7) NUMBITS(1) [Enabled = 1, Disabled = 0]]
);

macro_rules! impl_pa_mapper {
    ($($name:ident),+ ; marker: $marker:expr) => {
        $(
            paste! {
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

                    let reg = InMemoryRegister::new(
                        (page_address.value() as u64 & ADDR_MASK) | $marker
                    );

                    use [<$name Fields>]::BlockPageFields;

                    match memory_type {
                        MemoryType::UC => {
                            reg.modify(BlockPageFields::PCD::NotCacheable);
                        }
                        MemoryType::WT => {
                            reg.modify(BlockPageFields::PCD::IsCacheable + BlockPageFields::PWT::WriteThrough);
                        }
                        MemoryType::WB => {
                            reg.modify(BlockPageFields::PCD::IsCacheable + BlockPageFields::PWT::WriteBack);
                        }
                    }

                    Self(reg.get()).set_permissions(perms)
                }

                fn mapped_address(self) -> Option<PA> {
                    if (self.0 & $marker) != $marker {
                        return None;
                    }

                    Some(self.address())
                }
            }
            }
        )+
    };
}

// Block mappings (PDPE, PDE): P + PS must both be set.
impl_pa_mapper!(PDPE, PDE; marker: (1 << 7) | 1);
// Page mappings (PTE): only P; bit 7 is PAT, not PS.
impl_pa_mapper!(PTE; marker: 1);

macro_rules! impl_table_mapper {
    ($($name:ident, next_level: $next_level:ident),+ $(,)?) => {
        $(
            paste!{
            impl TableMapper for $name {
                type NextLevel = $next_level;

                fn next_table_address(self) -> Option<TPA<PgTableArray<Self::NextLevel>>> {
                    use [<$name Fields>]::BlockPageFields;

                    let reg = InMemoryRegister::new(self.0);

                    if reg.matches_all(BlockPageFields::P::Present + BlockPageFields::PS::MapTable) {
                        Some(self.address().cast())
                    } else {
                        None
                    }
                }

                fn new_next_table(pa: TPA<PgTableArray<Self::NextLevel>>) -> Self {
                    use [<$name Fields>]::BlockPageFields;

                    let reg = InMemoryRegister::new(pa.value() as u64 & ADDR_MASK);

                    // Set maximally permissive permissions on table entries so
                    // that the effective permissions are controlled solely by
                    // the leaf (page/block) descriptors. On x86_64 the CPU
                    // ANDs R/W, U/S (and ORs NX) across every level of the
                    // walk, so a restrictive bit here would cap the entire
                    // subtree.
                    reg.modify(BlockPageFields::P::Present
                        + BlockPageFields::PS::MapTable
                        + BlockPageFields::RW::ReadWrite
                        + BlockPageFields::US::UserAccessAllowed);

                    Self(reg.get())
                }
            }
        }
        )+
    };
}

impl_table_mapper!(
    PML4E, next_level: PDPTable,
    PDPE, next_level: PDTable,
    PDE, next_level: PTable,
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::region::PhysMemoryRegion;
    use crate::memory::{PAGE_SHIFT, PAGE_SIZE};

    const KERNEL_PERMS: bool = false;
    const USER_PERMS: bool = true;

    #[test]
    fn test_invalid_descriptor() {
        let d = PML4E::invalid();
        assert!(!d.is_valid());
        assert_eq!(d.as_raw(), 0);
    }

    #[test]
    fn test_pml4e_table_descriptor() {
        let pa = PA::from_value(0x1000_0000);
        let d = PML4E::new_next_table(pa.cast());

        assert!(d.is_valid());
        // P=1, RW=1, US=1 → low bits = 0b111
        assert_eq!(d.as_raw(), 0x1000_0000 | 0b111);
        assert_eq!(d.next_table_address().map(|x| x.to_untyped()), Some(pa));
    }

    #[test]
    fn test_pdpe_table_descriptor() {
        let pa = PA::from_value(0x2000_0000);
        let d = PDPE::new_next_table(pa.cast());

        assert!(d.is_valid());
        // P=1, RW=1, US=1 → low bits = 0b111
        assert_eq!(d.as_raw(), 0x2000_0000 | 0b111);
        assert_eq!(d.next_table_address().map(|x| x.to_untyped()), Some(pa));
        assert!(d.mapped_address().is_none());
    }

    #[test]
    fn test_pdpe_block_creation() {
        let pa = PA::from_value(1 << 30); // 1GiB aligned
        let perms = PtePermissions::rw(KERNEL_PERMS);

        let d = PDPE::new_map_pa(pa, MemoryType::WB, perms);

        assert!(d.is_valid());
        assert_eq!(d.as_raw() & (1 << 7 | 1), 1 << 7 | 1); // PS and present
        assert!(d.next_table_address().is_none());

        // Check address part (bits [47:30])
        assert_eq!(d.mapped_address(), Some(pa));
        assert_eq!((d.as_raw() >> 30) & 0x3_FFFF, 1);
    }

    #[test]
    fn test_pdpe_block_permissions() {
        let pa = PA::from_value(1 << 30);

        let d_krw = PDPE::new_map_pa(pa, MemoryType::WB, PtePermissions::rw(KERNEL_PERMS));
        let d_kro = PDPE::new_map_pa(pa, MemoryType::WB, PtePermissions::ro(KERNEL_PERMS));
        let d_urw = PDPE::new_map_pa(pa, MemoryType::WB, PtePermissions::rw(USER_PERMS));
        let d_uro = PDPE::new_map_pa(pa, MemoryType::WB, PtePermissions::ro(USER_PERMS));
        let d_krwx = PDPE::new_map_pa(pa, MemoryType::WB, PtePermissions::rwx(KERNEL_PERMS));

        assert_eq!(d_krw.permissions(), PtePermissions::rw(KERNEL_PERMS));
        assert_eq!(d_kro.permissions(), PtePermissions::ro(KERNEL_PERMS));
        assert_eq!(d_urw.permissions(), PtePermissions::rw(USER_PERMS));
        assert_eq!(d_uro.permissions(), PtePermissions::ro(USER_PERMS));
        assert_eq!(d_krwx.permissions(), PtePermissions::rwx(KERNEL_PERMS));

        // Verify NX bit is NOT set for executable
        assert_eq!(d_krwx.as_raw() & (1 << 63), 0);

        // Verify NX bit IS set for non-executable
        assert_eq!(d_krw.as_raw() & (1 << 63), 1 << 63);
    }

    #[test]
    fn test_pdpe_could_map() {
        let one_gib = 1 << 30;
        let good_region = PhysMemoryRegion::new(PA::from_value(one_gib), one_gib);
        let good_va = VA::from_value(one_gib * 2);

        assert!(PDPE::could_map(good_region, good_va));

        // Bad region size
        let small_region = PhysMemoryRegion::new(PA::from_value(one_gib), one_gib - 1);
        assert!(!PDPE::could_map(small_region, good_va));

        // Bad region alignment
        let unaligned_region = PhysMemoryRegion::new(PA::from_value(one_gib + 1), one_gib);
        assert!(!PDPE::could_map(unaligned_region, good_va));

        // Bad VA alignment
        let unaligned_va = VA::from_value(one_gib + 1);
        assert!(!PDPE::could_map(good_region, unaligned_va));
    }

    #[test]
    #[should_panic]
    fn test_pdpe_map_unaligned_pa_panics() {
        let pa = PA::from_value((1 << 30) + 1); // Not 1GiB aligned
        let perms = PtePermissions::rw(KERNEL_PERMS);
        PDPE::new_map_pa(pa, MemoryType::WB, perms);
    }

    #[test]
    fn test_pdpe_from_raw_roundtrip() {
        let pa = PA::from_value(1 << 30);
        let d = PDPE::new_map_pa(pa, MemoryType::WB, PtePermissions::rw(false));
        let raw = d.as_raw();
        let decoded = PDPE::from_raw(raw);
        assert_eq!(decoded.as_raw(), d.as_raw());
        assert_eq!(decoded.mapped_address(), d.mapped_address());
        assert_eq!(decoded.permissions(), d.permissions());
    }

    #[test]
    fn test_pde_block_creation() {
        let pa = PA::from_value(2 << 21); // 2MiB aligned
        let perms = PtePermissions::rw(USER_PERMS);

        let d = PDE::new_map_pa(pa, MemoryType::WB, perms);

        assert!(d.is_valid());
        assert_eq!(d.as_raw() & (1 << 7 | 1), 1 << 7 | 1); // PS and present
        assert!(d.next_table_address().is_none());
        assert_eq!(d.mapped_address(), Some(pa));
    }

    #[test]
    fn test_pde_block_permissions() {
        let pa = PA::from_value(4 << 21); // 2MiB aligned

        let d_kro = PDE::new_map_pa(pa, MemoryType::WB, PtePermissions::ro(KERNEL_PERMS));
        let d_krwx = PDE::new_map_pa(pa, MemoryType::WB, PtePermissions::rwx(KERNEL_PERMS));

        assert_eq!(d_kro.permissions(), PtePermissions::ro(KERNEL_PERMS));
        assert_eq!(d_krwx.permissions(), PtePermissions::rwx(KERNEL_PERMS));

        // NX bit for executable should not be set
        assert_eq!(d_krwx.as_raw() & (1 << 63), 0);
        // NX bit for non-executable should be set
        assert_ne!(d_kro.as_raw() & (1 << 63), 0);
    }

    #[test]
    fn test_pde_could_map() {
        let size = 1 << 21;
        let good_region = PhysMemoryRegion::new(PA::from_value(size), size);
        let good_va = VA::from_value(size * 3);

        assert!(PDE::could_map(good_region, good_va));

        let unaligned_pa = PhysMemoryRegion::new(PA::from_value(size + 1), size);
        let unaligned_va = VA::from_value(size + 1);

        assert!(!PDE::could_map(unaligned_pa, good_va));
        assert!(!PDE::could_map(good_region, unaligned_va));
    }

    #[test]
    #[should_panic]
    fn test_pde_map_unaligned_pa_panics() {
        let pa = PA::from_value((1 << 21) + 1);
        PDE::new_map_pa(pa, MemoryType::WB, PtePermissions::rw(false));
    }

    #[test]
    fn test_pde_from_raw_roundtrip() {
        let pa = PA::from_value(1 << 21);
        let d = PDE::new_map_pa(pa, MemoryType::WB, PtePermissions::rx(true));
        let raw = d.as_raw();
        let decoded = PDE::from_raw(raw);
        assert_eq!(decoded.as_raw(), d.as_raw());
        assert_eq!(decoded.mapped_address(), d.mapped_address());
        assert_eq!(decoded.permissions(), d.permissions());
    }

    #[test]
    fn test_pte_page_creation() {
        let pa = PA::from_value(PAGE_SIZE * 10); // 4KiB aligned
        let perms = PtePermissions::rx(USER_PERMS);

        let d = PTE::new_map_pa(pa, MemoryType::WB, perms);

        assert!(d.is_valid());
        assert_eq!(d.as_raw() & 1, 1); // P set
        assert_eq!(d.as_raw() & (1 << 7), 0); // PAT (bit 7) clear

        // Check address part (bits [51:12])
        assert_eq!(
            (d.as_raw() >> PAGE_SHIFT) & ((1 << 40) - 1),
            (pa.value() >> PAGE_SHIFT) as u64
        );
    }

    #[test]
    fn test_pte_permissions() {
        let pa = PA::from_value(PAGE_SIZE);

        let d_urx = PTE::new_map_pa(pa, MemoryType::WB, PtePermissions::rx(USER_PERMS));
        assert_eq!(d_urx.permissions(), PtePermissions::rx(USER_PERMS));

        // Verify NX bit is NOT set for executable
        assert_eq!(d_urx.as_raw() & (1 << 63), 0);
    }

    #[test]
    fn test_pte_could_map() {
        let good_region = PhysMemoryRegion::new(PA::from_value(PAGE_SIZE), PAGE_SIZE);
        let good_va = VA::from_value(PAGE_SIZE * 2);

        assert!(PTE::could_map(good_region, good_va));

        // Bad region alignment
        let unaligned_region = PhysMemoryRegion::new(PA::from_value(PAGE_SIZE + 1), PAGE_SIZE);
        assert!(!PTE::could_map(unaligned_region, good_va));
    }

    #[test]
    fn test_pte_from_raw_roundtrip() {
        let pa = PA::from_value(PAGE_SIZE * 8);
        let d = PTE::new_map_pa(pa, MemoryType::UC, PtePermissions::rw(true));
        let raw = d.as_raw();
        let decoded = PTE::from_raw(raw);
        assert_eq!(decoded.as_raw(), d.as_raw());
        assert_eq!(decoded.mapped_address(), d.mapped_address());
        assert_eq!(decoded.permissions(), d.permissions());
    }

    #[test]
    fn test_pde_invalid_descriptor() {
        let d = PDE::invalid();
        assert!(!d.is_valid());
        assert_eq!(d.as_raw(), 0);
        assert!(d.next_table_address().is_none());
        assert!(d.mapped_address().is_none());
    }

    #[test]
    fn test_table_entries_are_maximally_permissive() {
        let pa = PA::from_value(0x1000_0000);

        for d in [
            PML4E::new_next_table(pa.cast()).as_raw(),
            PDPE::new_next_table(pa.cast()).as_raw(),
            PDE::new_next_table(pa.cast()).as_raw(),
        ] {
            // R/W must be set so the subtree can contain writable leaves.
            assert_ne!(d & (1 << 1), 0, "R/W bit must be set on table entry");

            // U/S must be set so the subtree can contain user-accessible leaves.
            assert_ne!(d & (1 << 2), 0, "U/S bit must be set on table entry");

            // NX must be clear so the subtree can contain executable leaves.
            assert_eq!(d & (1 << 63), 0, "NX bit must be clear on table entry");
        }
    }

    #[test]
    fn test_pte_pat_bit_is_clear() {
        // On 4KiB PTEs, bit 7 is PAT (not PS). With the default PAT MSR,
        // PAT=0 + PCD=0 + PWT=0 selects WB (index 0). Setting PAT=1 would
        // select index 4 (typically UC), which is not what we want.
        let pa = PA::from_value(PAGE_SIZE);
        for mt in [MemoryType::WB, MemoryType::WT, MemoryType::UC] {
            let d = PTE::new_map_pa(pa, mt, PtePermissions::rw(false));
            assert_eq!(
                d.as_raw() & (1 << 7),
                0,
                "PAT bit (7) must be clear on PTE for {:?}",
                mt
            );
        }
    }

    #[test]
    fn test_pdpe_pde_ps_bit_is_set() {
        // Block (large-page) mappings on PDPE and PDE must set PS (bit 7).
        let pdpe = PDPE::new_map_pa(
            PA::from_value(1 << 30),
            MemoryType::WB,
            PtePermissions::rw(false),
        );
        assert_ne!(pdpe.as_raw() & (1 << 7), 0, "PS must be set on PDPE block");

        let pde = PDE::new_map_pa(
            PA::from_value(1 << 21),
            MemoryType::WB,
            PtePermissions::rw(false),
        );
        assert_ne!(pde.as_raw() & (1 << 7), 0, "PS must be set on PDE block");
    }
}
