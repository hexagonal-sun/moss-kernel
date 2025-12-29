use crate::{
    arch::ArchImpl,
    drivers::{
        Driver, DriverManager, fdt_prober,
        init::PlatformBus,
        probe::{DeviceDescriptor, DeviceMatchType},
    },
    interrupts::{
        InterruptConfig, InterruptContext, InterruptController, InterruptDescriptor,
        InterruptManager, TriggerMode, set_interrupt_root,
    },
    kernel_driver,
    sync::SpinLock,
};
use aarch64_cpu::registers::MPIDR_EL1;
use alloc::{boxed::Box, sync::Arc};
use core::arch::asm;
use libkernel::{
    KernAddressSpace, VirtualMemory,
    error::{KernelError, Result},
    memory::{
        address::{PA, VA},
        region::PhysMemoryRegion,
    },
};
use log::{info, warn};
use tock_registers::{
    interfaces::*,
    register_structs,
    registers::{ReadOnly, ReadWrite},
};

use super::arm_gic_v2::GicInterruptID;

#[inline(always)]
fn get_icc_iar1_el1() -> u64 {
    let iar: u64;
    unsafe { asm!("mrs {}, ICC_IAR1_EL1", out(reg) iar, options(nostack, nomem)) };
    iar
}

#[inline(always)]
fn set_icc_pmr_el1(priority: u8) {
    unsafe { asm!("msr ICC_PMR_EL1, {}", in(reg) priority as u64, options(nostack, nomem)) };
}

#[inline(always)]
fn set_icc_igrpen1_el1(enable: u64) {
    unsafe { asm!("msr ICC_IGRpen1_EL1, {}", in(reg) enable, options(nostack, nomem)) };
}

#[inline(always)]
fn set_icc_sgi1r_el1(value: u64) {
    unsafe { asm!("msr ICC_SGI1R_EL1, {}", in(reg) value, options(nostack, nomem)) };
}

register_structs! {
    /// GICv3 Distributor registers.
    #[allow(non_snake_case)]
    GicDistributorRegs {
        /// Distributor Control Register.
        (0x0000 => CTLR: ReadWrite<u32>),
        /// Interrupt Controller Type Register.
        (0x0004 => TYPER: ReadOnly<u32>),
        /// Distributor Implementer Identification Register.
        (0x0008 => IIDR: ReadOnly<u32>),
        (0x000c => _reserved_0),
        /// Interrupt Group Registers (for SPIs).
        (0x0080 => IGROUPR: [ReadWrite<u32>; 0x20]),
        /// Interrupt Set-Enable Registers (for SPIs).
        (0x0100 => ISENABLER: [ReadWrite<u32>; 0x20]),
        /// Interrupt Clear-Enable Registers (for SPIs).
        (0x0180 => ICENABLER: [ReadWrite<u32>; 0x20]),
        /// Interrupt Set-Pending Registers (for SPIs).
        (0x0200 => ISPENDR: [ReadWrite<u32>; 0x20]),
        /// Interrupt Clear-Pending Registers (for SPIs).
        (0x0280 => ICPENDR: [ReadWrite<u32>; 0x20]),
        /// Interrupt Set-Active Registers (for SPIs).
        (0x0300 => ISACTIVER: [ReadWrite<u32>; 0x20]),
        /// Interrupt Clear-Active Registers (for SPIs).
        (0x0380 => ICACTIVER: [ReadWrite<u32>; 0x20]),
        /// Interrupt Priority Registers (for SPIs).
        (0x0400 => IPRIORITYR: [ReadWrite<u32>; 0x100]),
        (0x0800 => _reserved_1),
        /// Interrupt Configuration Registers (for SPIs).
        (0x0C00 => ICFGR: [ReadWrite<u32>; 0x40]),
        (0x0d00 => _reserved_2),
        /// Each register is 64-bits and corresponds to one interrupt ID.
        (0x6100 => IROUTER: [ReadWrite<u64>; 988]),
        (0x7fe0 => @END),
    }
}

register_structs! {
    /// GICv3 Redistributor registers (Control portion).
    /// Each core has a Redistributor.
    #[allow(non_snake_case)]
    GicRedistributorRegs {
        /// Redistributor Control Register.
        (0x0000 => CTLR: ReadWrite<u32>),
        /// Implementer Identification Register.
        (0x0004 => IIDR: ReadOnly<u32>),
        /// Redistributor Type Register.
        (0x0008 => TYPER: ReadOnly<u64>),
        /// Error Reporting Status Register.
        (0x0010 => STATUSR: ReadWrite<u32>),
        /// Redistributor Wake Register.
        (0x0014 => WAKER: ReadWrite<u32>),
        (0x0018 => @END),
    }
}

register_structs! {
    /// GICv3 Redistributor SGI & PPI registers.
    /// This block is at an offset from the main Redistributor control page.
    #[allow(non_snake_case)]
    GicrSgiPpiRegs {
        (0x0000 => _reserved_0),
        /// Interrupt Group Register 0 (for SGIs 0-15 and PPIs 16-31).
        (0x0080 => IGROUPR0: ReadWrite<u32>),
        (0x0084 => _reserved_1),
        /// Interrupt Set-Enable Register 0.
        (0x0100 => ISENABLER0: ReadWrite<u32>),
        (0x0104 => _reserved_2),
        /// Interrupt Clear-Enable Register 0.
        (0x0180 => ICENABLER0: ReadWrite<u32>),
        (0x0184 => _reserved_3),
        /// Interrupt Set-Pending Register 0.
        (0x0200 => ISPENDR0: ReadWrite<u32>),
        (0x0204 => _reserved_4),
        /// Interrupt Clear-Pending Register 0.
        (0x0280 => ICPENDR0: ReadWrite<u32>),
        (0x0284 => _reserved_5),
        /// Interrupt Set-Active Register 0.
        (0x0300 => ISACTIVER0: ReadWrite<u32>),
        (0x0304 => _reserved_6),
        /// Interrupt Clear-Active Register 0.
        (0x0380 => ICACTIVER0: ReadWrite<u32>),
        (0x0384 => _reserved_7),
        /// Interrupt Priority Registers (8 registers for 32 interrupts).
        (0x0400 => IPRIORITYR: [ReadWrite<u32>; 8]),
        (0x0420 => _reserved_8),
        /// Interrupt Configuration Register (for PPIs). SGIs are fixed.
        (0x0C00 => ICFGR1: ReadWrite<u32>),
        (0x0C04 => @END),
    }
}

#[derive(Debug)]
struct ArmGicV3InterruptContext {
    raw_id: u64,
    desc: InterruptDescriptor,
}

impl InterruptContext for ArmGicV3InterruptContext {
    fn descriptor(&self) -> InterruptDescriptor {
        self.desc
    }
}

impl Drop for ArmGicV3InterruptContext {
    fn drop(&mut self) {
        unsafe {
            asm!("msr ICC_EOIR1_EL1, {}", in(reg) self.raw_id);
        }
    }
}

pub struct ArmGicV3 {
    dist: &'static mut GicDistributorRegs,
    rdist_base: VA,
    rdist_stride: usize,
}

impl ArmGicV3 {
    /// Creates a new GICv3 driver instance.
    ///
    /// This function performs the one-time distributor initialization. Per-core
    /// initialization must be done separately via `init_core`.
    pub fn new(dist_mem: VA, rdist_mem: VA, rdist_stride: usize) -> Result<Self> {
        let dist = unsafe { &mut *(dist_mem.value() as *mut GicDistributorRegs) };

        // Disable the distributor before configuration
        dist.CTLR.set(0);

        // Wait for writes to complete.
        while dist.CTLR.get() & (1 << 31) != 0 {} // RWP bit

        // Configure SPIs
        let it_lines = (dist.TYPER.get() & 0x1F) + 1;
        let num_spis = (it_lines * 32).saturating_sub(32) as usize;

        // Set all SPIs to Group 1 (non-secure)
        for i in 1..num_spis.div_ceil(32) {
            dist.IGROUPR[i].set(0xFFFF_FFFF);
        }

        // Disable, clear pending, and set default priority for all SPIs
        for i in 1..num_spis.div_ceil(32) {
            dist.ICENABLER[i].set(0xFFFF_FFFF);
            dist.ICPENDR[i].set(0xFFFF_FFFF);
        }

        // Set all priorites to 0 (highest).
        for i in 8..((num_spis + 31) / 4) {
            dist.IPRIORITYR[i].set(0);
        }

        // EnableGrp1ns: Enable group 1Ns interrupts
        dist.CTLR.set(1 << 1);

        // Wait for writes to complete.
        while dist.CTLR.get() & (1 << 31) != 0 {}

        Ok(Self {
            dist,
            rdist_base: rdist_mem,
            rdist_stride,
        })
    }

    /// Initializes the GIC for the calling CPU core.
    pub fn init_core(&mut self, core_id: usize) -> Result<()> {
        let mpidr = MPIDR_EL1.get();

        // Find this core's Redistributor
        let rdist = self.get_rdist_for_cpu(mpidr)?;
        let sgi_ppi =
            unsafe { &mut *((rdist as *mut _ as *mut u8).add(64 * 1024) as *mut GicrSgiPpiRegs) };

        // Wake up the Redistributor Clear ProcessorSleep bit, then wait for
        // ChildrenAsleep to be clear.
        rdist.WAKER.set(rdist.WAKER.get() & !(1 << 1));
        while rdist.WAKER.get() & (1 << 2) != 0 {}

        info!(
            "GICv3: Redistributor for core {} (MPIDR=0x{:x}) is awake.",
            core_id, mpidr
        );

        // 2. Configure PPIs and SGIs for this core SGIs (0-15) are Group 0,
        // PPIs (16-31) are Group 1
        sgi_ppi.IGROUPR0.set(0xFFFF_FFFF); // PPIs are Grp1, SGIs are Grp0
        sgi_ppi.ICENABLER0.set(0xFFFF_FFFF); // Disable all
        sgi_ppi.ICPENDR0.set(0xFFFF_FFFF); // Clear all pending

        // Set default priorities
        for i in 0..8 {
            sgi_ppi.IPRIORITYR[i].set(0xA0A0_A0A0);
        }

        // 3. Enable the CPU's system register interface to the GIC
        unsafe {
            // Set SRE (System Register Enable) bit
            asm!("msr ICC_SRE_EL1, {val}", val = in(reg) 1u64);
            // Synchronize
            asm!("isb");
        }

        // 4. Set priority mask register to allow all priorities
        set_icc_pmr_el1(0xff);

        // 5. Enable interrupt group 1 at the CPU interface
        set_icc_igrpen1_el1(1);

        info!("GICv3: CPU interface for core {} enabled.", core_id);
        Ok(())
    }

    /// Gets a mutable reference to the redistributor for a given CPU affinity.
    fn get_rdist_for_cpu(&self, mpidr: u64) -> Result<&'static mut GicRedistributorRegs> {
        // This is a simplified search. A full implementation should parse the
        // GICR's TYPER to find the correct redistributor for a given MPIDR. For
        // systems where cores and redistributors are mapped linearly (like QEMU
        // virt) this should be fine, but we do an assert just to be ensure.
        let core_id = (mpidr & 0xFF) + ((mpidr >> 8) & 0xFF) * 4; // Simple linear core ID
        let rdist_offset = core_id as usize * self.rdist_stride;
        let rdist_addr = self.rdist_base.value() + rdist_offset;

        let rdist = unsafe { &mut *(rdist_addr as *mut GicRedistributorRegs) };

        assert_eq!(rdist.TYPER.get() >> 32, core_id);

        Ok(rdist)
    }

    /// Gets a mutable reference to the SGI/PPI block for a given redistributor.
    fn get_sgi_ppi_for_rdist(&self, rdist: &GicRedistributorRegs) -> &'static mut GicrSgiPpiRegs {
        let rdist_addr = rdist as *const _ as usize;
        let sgi_ppi_addr = rdist_addr + (64 * 1024); // SGI base is at a 64K offset
        unsafe { &mut *(sgi_ppi_addr as *mut GicrSgiPpiRegs) }
    }
}

unsafe impl Sync for ArmGicV3 {}
unsafe impl Send for ArmGicV3 {}

impl InterruptController for ArmGicV3 {
    fn enable_interrupt(&mut self, cfg: InterruptConfig) {
        let Ok(GicInterruptID(id)) = GicInterruptID::try_from(cfg.descriptor) else {
            return;
        };

        match cfg.descriptor {
            // SPIs are handled by the Distributor
            InterruptDescriptor::Spi(_) => {
                // Configure trigger mode (2 bits per interrupt)
                let icfgr_idx = id / 16;
                let bit_shift = (id % 16) * 2;
                let current_icfgr = self.dist.ICFGR[icfgr_idx].get();
                let new_icfgr = match cfg.trigger {
                    TriggerMode::EdgeRising | TriggerMode::EdgeFalling => {
                        current_icfgr | (0b10 << bit_shift)
                    }
                    TriggerMode::LevelHigh | TriggerMode::LevelLow => {
                        current_icfgr & !(0b11 << bit_shift)
                    }
                };
                self.dist.ICFGR[icfgr_idx].set(new_icfgr);

                let target_affinity = 0;
                self.dist.IROUTER[id].set(target_affinity);

                // Enable the interrupt
                let enabler_idx = id / 32;
                self.dist.ISENABLER[enabler_idx].set(1 << (id % 32));
            }
            // PPIs and SGIs are handled by the per-core Redistributor
            InterruptDescriptor::Ppi(_) | InterruptDescriptor::Ipi(_) => {
                let mpidr = MPIDR_EL1.get();

                if let Ok(rdist) = self.get_rdist_for_cpu(mpidr) {
                    let sgi_ppi = self.get_sgi_ppi_for_rdist(rdist);

                    // Configure trigger for PPIs (SGIs are fixed edge-triggered)
                    if let InterruptDescriptor::Ppi(_) = cfg.descriptor {
                        let bit_shift = (id % 16) * 2; // PPIs start at bit 0 of ICFGR1
                        let current_icfgr = sgi_ppi.ICFGR1.get();
                        let new_icfgr = match cfg.trigger {
                            TriggerMode::EdgeRising | TriggerMode::EdgeFalling => {
                                current_icfgr | (0b10 << bit_shift)
                            }
                            TriggerMode::LevelHigh | TriggerMode::LevelLow => {
                                current_icfgr & !(0b10 << bit_shift)
                            }
                        };
                        sgi_ppi.ICFGR1.set(new_icfgr);
                    }

                    // Enable the interrupt
                    sgi_ppi.ISENABLER0.set(1 << (id % 32));
                }
            }
        }
    }

    fn disable_interrupt(&mut self, i: InterruptDescriptor) {
        let Ok(GicInterruptID(id)) = GicInterruptID::try_from(i) else {
            return;
        };

        match i {
            InterruptDescriptor::Spi(_) => {
                let disabler_idx = id / 32;
                self.dist.ICENABLER[disabler_idx].set(1 << (id % 32));
            }
            InterruptDescriptor::Ppi(_) | InterruptDescriptor::Ipi(_) => {
                let mpidr = MPIDR_EL1.get();

                if let Ok(rdist) = self.get_rdist_for_cpu(mpidr) {
                    let sgi_ppi = self.get_sgi_ppi_for_rdist(rdist);
                    sgi_ppi.ICENABLER0.set(1 << (id % 32));
                }
            }
        }
    }

    fn read_active_interrupt(&mut self) -> Option<Box<dyn InterruptContext>> {
        // In GICv3, we read a system register to get the active interrupt.
        let int_id = get_icc_iar1_el1() as usize;

        let descriptor = match int_id {
            0..=15 => InterruptDescriptor::Ipi(int_id),
            16..=31 => InterruptDescriptor::Ppi(int_id - 16),
            32..=1019 => InterruptDescriptor::Spi(int_id - 32),
            // 1020-1023 are special interrupt IDs, e.g., for spurious interrupts.
            _ => return None,
        };

        let context = ArmGicV3InterruptContext {
            raw_id: int_id as u64,
            desc: descriptor,
        };

        Some(Box::new(context))
    }

    fn raise_ipi(&mut self, target_cpu_id: usize) {
        set_icc_sgi1r_el1(1 << (target_cpu_id as u64 & 0xffff));
    }

    fn enable_core(&mut self, cpu_id: usize) {
        if let Err(e) = self.init_core(cpu_id) {
            warn!("Failed to enable interrupts for core {}: {}", cpu_id, e);
            return;
        }

        for i in 0..16 {
            self.enable_interrupt(InterruptConfig {
                descriptor: InterruptDescriptor::Ipi(i),
                trigger: TriggerMode::EdgeRising,
            });
        }

        // Also enable the timer
        self.enable_interrupt(InterruptConfig {
            descriptor: InterruptDescriptor::Ppi(14),
            trigger: TriggerMode::EdgeRising,
        });
    }

    fn parse_fdt_interrupt_regs(
        &self,
        iter: &mut dyn Iterator<Item = u32>,
    ) -> Result<InterruptConfig> {
        // This part is defined by the device tree binding, which is largely
        // compatible between GIC versions.
        use TriggerMode::*;

        let int_type = iter.next().ok_or(KernelError::InvalidValue)?;
        let int_number = iter.next().ok_or(KernelError::InvalidValue)?;
        let flags = iter.next().ok_or(KernelError::InvalidValue)?;

        let descriptor = match int_type {
            0 => InterruptDescriptor::Spi(int_number as usize),
            1 => InterruptDescriptor::Ppi(int_number as usize),
            _ => return Err(KernelError::InvalidValue),
        };

        let trigger = match flags & 0xF {
            0x1 => EdgeRising,
            0x4 => LevelHigh,
            // GICv3 simplified trigger types for SPIs. Level-low and falling-edge are less common.
            _ => {
                warn!("Unsupported GIC interrupt trigger flag: {}", flags);
                LevelHigh // Default to a safe value
            }
        };

        Ok(InterruptConfig {
            descriptor,
            trigger,
        })
    }
}

pub fn gic_v3_probe(_dm: &mut DriverManager, d: DeviceDescriptor) -> Result<Arc<dyn Driver>> {
    match d {
        DeviceDescriptor::Fdt(fdt_node, _) => {
            use libkernel::error::ProbeError::*;

            let mut regs = fdt_node.reg().ok_or(NoReg)?;
            let dist_region = regs.next().ok_or(NoReg)?;
            let rdist_region = regs.next().ok_or(NoReg)?;

            let rdist_stride = fdt_node
                .find_property("redistributor-stride")
                .map(|p| p.u32())
                .unwrap_or(0) as usize
                + 0x10000 * 2; // Two 64kb frames: RD and SGI

            let (dist_mem, rdist_mem) = {
                let addr_spc = <ArchImpl as VirtualMemory>::kern_address_space();
                let mut kern_addr_spc = addr_spc.lock_save_irq();

                let dist_mem = kern_addr_spc.map_mmio(PhysMemoryRegion::new(
                    PA::from_value(dist_region.address as usize),
                    dist_region.size.ok_or(NoRegSize)?,
                ))?;

                // Map the *entire* redistributor region. The driver will calculate offsets.
                let rdist_mem = kern_addr_spc.map_mmio(PhysMemoryRegion::new(
                    PA::from_value(rdist_region.address as usize),
                    rdist_region.size.ok_or(NoRegSize)?,
                ))?;

                (dist_mem, rdist_mem)
            };

            info!(
                "ARM GICv3 found: dist @ {:?}, rdist @ {:?} (stride=0x{:x})",
                dist_region, rdist_region, rdist_stride
            );

            let mut gic = ArmGicV3::new(dist_mem, rdist_mem, rdist_stride)?;

            if let Err(e) = gic.init_core(0) {
                panic!("Failed to initialize GICv3 for boot core: {:?}", e);
            }

            let gic = Arc::new(SpinLock::new(gic));

            let manager = InterruptManager::new(fdt_node.name, gic);

            if fdt_prober::is_intc_root(&fdt_node) {
                set_interrupt_root(manager.clone());
            }

            Ok(manager)
        }
    }
}
pub fn arm_gicv3_init(bus: &mut PlatformBus, _dm: &mut DriverManager) -> Result<()> {
    bus.register_platform_driver(
        DeviceMatchType::FdtCompatible("arm,gic-v3"),
        Box::new(gic_v3_probe),
    );

    Ok(())
}

kernel_driver!(arm_gicv3_init);
