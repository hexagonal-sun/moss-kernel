use crate::{
    arch::{
        ArchImpl,
        arm64::{
            boot::{
                arch_init_secondary,
                memory::{KERNEL_STACK_PG_ORDER, allocate_kstack_region},
            },
            psci::{PSCIEntry, PSCIMethod, boot_secondary_psci},
        },
    },
    drivers::{fdt_prober::get_fdt, timer::now},
    kfunc_pa, ksym_pa,
    memory::PAGE_ALLOC,
    sync::OnceLock,
};
use core::{
    arch::naked_asm,
    hint::spin_loop,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use libkernel::{
    CpuOps, KernAddressSpace, VirtualMemory,
    error::{KernelError, Result},
    memory::{
        address::{PA, VA},
        permissions::PtePermissions,
    },
};
use log::{info, warn};

unsafe extern "C" {
    static __boot_stack: u8;
    static exception_return: u8;
}

#[derive(Debug)]
#[repr(C)]
struct SecondaryBootInfo {
    boot_stack_addr: PA,
    kstack_addr: VA,
    kmem_ttbr: PA,
    idmap_ttbr: PA,
    start_fn: VA,
    exception_ret: VA,
}

#[unsafe(naked)]
#[unsafe(no_mangle)]
extern "C" fn do_secondary_start(boot_info: *const SecondaryBootInfo) {
    naked_asm!(
        "ldr x1, [x0]", // Setup boot stack.
        "mov sp, x1",
        "mov x19, x0", // Save boot_info .
        "mov x0, sp",  // Arg0: EL1 stack pointer.
        "bl transition_to_el1",
        "ldr x0, [x19, #0x18]", // Arg0: idmap_ttbr.
        "ldr x1, [x19, #0x10]", // Arg1: kmem_ttbr.
        "bl enable_mmu",
        "ldr x1, [x19, #0x20]",     // Load `start_fn`
        "ldr x2, [x19, #0x8]",      // Load kstack addr
        "mov sp, x2",               // Set final stack addr
        "sub sp, sp, #(0x10 * 18)", // Allocate a context switch frame
        "mov x0, sp",               // ctx switch frame ptr
        "ldr x2, [x19, #0x28]",     // return to exception_ret.
        "mov lr, x2",
        "br  x1", // branch to Rust entry point
        "b    ."  // Just in case!
    )
}

enum EntryMethod {
    Psci(PSCIEntry),
}

fn find_enable_method(node: &fdt_parser::Node<'static>) -> Result<EntryMethod> {
    let method = node
        .find_property("enable-method")
        .ok_or(KernelError::Other("enable-method property missing"))?
        .str();

    if method == "psci" {
        let fdt = get_fdt();
        let psci_node = fdt
            .get_node_by_name("psci")
            .ok_or(KernelError::Other("psci node missing"))?;

        let method = match psci_node.find_property("method").map(|x| x.str()) {
            Some("hvc") => PSCIMethod::Hvc,
            Some("smc") => PSCIMethod::Smc,
            _ => return Err(KernelError::Other("Unknown method in psci node")),
        };

        let cpu_on_id = psci_node.find_property("cpu_on").map(|x| x.u32());

        Ok(EntryMethod::Psci(PSCIEntry { method, cpu_on_id }))
    } else {
        Err(KernelError::Other("Unknown enable method"))
    }
}

fn prepare_for_secondary_entry() -> Result<(PA, PA)> {
    static mut SECONDARY_BOOT_CTX: MaybeUninit<SecondaryBootInfo> = MaybeUninit::uninit();

    let entry_fn = kfunc_pa!(do_secondary_start as *const () as usize);
    let boot_stack = ksym_pa!(__boot_stack);
    let ctx = ksym_pa!(SECONDARY_BOOT_CTX);

    let kstack_vaddr = allocate_kstack_region();
    let kstack_paddr = PAGE_ALLOC
        .get()
        .unwrap()
        .alloc_frames(KERNEL_STACK_PG_ORDER as _)?
        .leak();

    ArchImpl::kern_address_space().lock_save_irq().map_normal(
        kstack_paddr,
        kstack_vaddr,
        PtePermissions::rw(false),
    )?;

    unsafe {
        (&raw mut SECONDARY_BOOT_CTX as *mut SecondaryBootInfo).write(SecondaryBootInfo {
            boot_stack_addr: boot_stack,
            kstack_addr: kstack_vaddr.end_address(),
            kmem_ttbr: ArchImpl::kern_address_space().lock_save_irq().table_pa(),
            idmap_ttbr: *IDMAP_ADDR
                .get()
                .ok_or(KernelError::Other("Idmap not set"))?,
            start_fn: VA::from_value(arch_init_secondary as *const () as usize),
            exception_ret: VA::from_value(&exception_return as *const _ as usize),
        })
    };

    Ok((entry_fn, ctx))
}

fn do_boot_secondary(cpu_node: fdt_parser::Node<'static>) -> Result<()> {
    let id = cpu_node
        .reg()
        .and_then(|mut x| x.next().map(|x| x.address))
        .ok_or(KernelError::Other("reg property missing on CPU node"))?;

    // Skip boot core.
    if id == 0 {
        return Ok(());
    }

    let mode = find_enable_method(&cpu_node)?;

    let (entry_fn, ctx) = prepare_for_secondary_entry()?;

    SECONDARY_BOOT_FLAG.store(false, Ordering::Relaxed);

    match mode {
        EntryMethod::Psci(pscientry) => boot_secondary_psci(pscientry, id as _, entry_fn, ctx),
    }

    let timeout = now().map(|x| x + Duration::from_millis(100));

    while !SECONDARY_BOOT_FLAG.load(Ordering::Acquire) {
        spin_loop();
        if let Some(timeout) = timeout
            && let Some(now) = now()
            && now >= timeout
        {
            return Err(KernelError::Other("timeout waiting for core entry"));
        }
    }

    Ok(())
}

fn cpu_node_iter() -> impl Iterator<Item = fdt_parser::Node<'static>> {
    let fdt = get_fdt();

    fdt.all_nodes().filter(|node| {
        node.find_property("device_type")
            .map(|prop| prop.str() == "cpu")
            .unwrap_or(false)
    })
}

pub fn boot_secondaries() {
    for cpu_node in cpu_node_iter() {
        if let Err(e) = do_boot_secondary(cpu_node) {
            log::warn!("Failed to boot secondary: {}", e);
        }
    }
}

pub fn cpu_count() -> usize {
    cpu_node_iter().count()
}

pub fn save_idmap(addr: PA) {
    if IDMAP_ADDR.set(addr).is_err() {
        warn!("Attempted to set ID map multiple times");
    }
}

pub fn secondary_booted() {
    let id = ArchImpl::id();

    info!("CPU {} online.", id);

    SECONDARY_BOOT_FLAG.store(true, Ordering::Release);
}

static IDMAP_ADDR: OnceLock<PA> = OnceLock::new();
static SECONDARY_BOOT_FLAG: AtomicBool = AtomicBool::new(false);
