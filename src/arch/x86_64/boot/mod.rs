use alloc::vec::Vec;
use log::info;
use crate::sync::OnceLock;

use libkernel::memory::address::VA;

use crate::memory::PageOffsetTranslator;

const KERNEL_STACK_SHIFT: usize = 15; // 32 KiB
const KERNEL_STACK_SZ: usize = 1 << KERNEL_STACK_SHIFT;
const KERNEL_STACK_PG_ORDER: usize = (KERNEL_STACK_SZ / libkernel::memory::PAGE_SIZE).ilog2() as usize;

static KSTACK_VAS: OnceLock<Vec<VA>> = OnceLock::new();

/// Allocate per-CPU kernel stacks and record their virtual top addresses.
/// This should be called once on the primary core after the main page
/// allocator (`PAGE_ALLOC`) is initialized.
pub fn setup_stacks_primary() {
    use crate::memory::PAGE_ALLOC;
    use crate::arch::ArchImpl;
    use crate::arch::Arch;


    let num_cpus = ArchImpl::cpu_count();

    let page_alloc = PAGE_ALLOC.get().expect("PAGE_ALLOC not initialized");

    let mut stacks: Vec<VA> = Vec::new();

    for _ in 0..num_cpus {
        let region = page_alloc
            .alloc_frames(KERNEL_STACK_PG_ORDER as u8)
            .expect("Failed to allocate kernel stack frames")
            .leak();

        let kva = region.start_address().to_va::<PageOffsetTranslator>();
        let top = kva.add_bytes(KERNEL_STACK_SZ);
        stacks.push(top);
    }

    KSTACK_VAS.set(stacks).expect("KSTACK_VAS already initialized");

    info!("Allocated kernel stacks for {} CPUs", num_cpus);
}

/// Initialize per-CPU state for the current CPU (set MSR to point at the
/// kernel stack top for this CPU). This should be called on each CPU early
/// in their boot path.
pub fn init_cpu_percpu() {
    use super::{MSR_KERNEL_GS_BASE, wrmsr};
    use crate::arch::ArchImpl;
    use libkernel::CpuOps;

    let id = ArchImpl::id();
    let stacks = KSTACK_VAS.get().expect("KSTACK_VAS not initialized");
    let top = stacks[id];

    // Write the kernel GS base to point at the per-CPU stack/top or data.
    unsafe { wrmsr(MSR_KERNEL_GS_BASE, top.value() as u64) }
}
