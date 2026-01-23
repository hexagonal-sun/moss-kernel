use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
};
use libkernel::{
    KernAddressSpace, VirtualMemory,
    error::{KernelError, Result},
    memory::{address::PA, region::PhysMemoryRegion},
};
use log::info;
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_structs,
    registers::{ReadOnly, ReadWrite, WriteOnly},
};

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

register_structs! {
    /// GIC Distributor registers.
    #[allow(non_snake_case)]
    GicDistributorRegs {
        /// Distributor Control Register.
        (0x0000 => CTLR: ReadWrite<u32>),
        /// Interrupt Controller Type Register.
        (0x0004 => TYPER: ReadOnly<u32>),
        /// Distributor Implementer Identification Register.
        (0x0008 => IIDR: ReadOnly<u32>),
        (0x000c => _reserved_0),
        /// Interrupt Group Registers.
        (0x0080 => IGROUPR: [ReadWrite<u32>; 0x20]),
        /// Interrupt Set-Enable Registers.
        (0x0100 => ISENABLER: [ReadWrite<u32>; 0x20]),
        /// Interrupt Clear-Enable Registers.
        (0x0180 => ICENABLER: [ReadWrite<u32>; 0x20]),
        /// Interrupt Set-Pending Registers.
        (0x0200 => ISPENDR: [ReadWrite<u32>; 0x20]),
        /// Interrupt Clear-Pending Registers.
        (0x0280 => ICPENDR: [ReadWrite<u32>; 0x20]),
        /// Interrupt Set-Active Registers.
        (0x0300 => ISACTIVER: [ReadWrite<u32>; 0x20]),
        /// Interrupt Clear-Active Registers.
        (0x0380 => ICACTIVER: [ReadWrite<u32>; 0x20]),
        /// Interrupt Priority Registers.
        (0x0400 => IPRIORITYR: [ReadWrite<u32>; 0x100]),
        /// Interrupt Processor Targets Registers.
        (0x0800 => ITARGETSR: [ReadWrite<u32>; 0x100]),
        /// Interrupt Configuration Registers.
        (0x0c00 => ICFGR: [ReadWrite<u32>; 0x40]),
        (0x0d00 => _reserved_1),
        /// Software Generated Interrupt Register.
        (0x0f00 => SGIR: WriteOnly<u32>),
        (0x0f04 => @END),
    }
}

register_structs! {
    /// GIC CPU Interface registers.
    #[allow(non_snake_case)]
    GicCpuInterfaceRegs {
        /// CPU Interface Control Register.
        (0x0000 => CTLR: ReadWrite<u32>),
        /// Interrupt Priority Mask Register.
        (0x0004 => PMR: ReadWrite<u32>),
        /// Binary Point Register.
        (0x0008 => BPR: ReadWrite<u32>),
        /// Interrupt Acknowledge Register.
        (0x000c => IAR: ReadOnly<u32>),
        /// End of Interrupt Register.
        (0x0010 => EOIR: WriteOnly<u32>),
        /// Running Priority Register.
        (0x0014 => RPR: ReadOnly<u32>),
        /// Highest Priority Pending Interrupt Register.
        (0x0018 => HPPIR: ReadOnly<u32>),
        (0x001c => _reserved_1),
        /// CPU Interface Identification Register.
        (0x00fc => IIDR: ReadOnly<u32>),
        (0x0100 => _reserved_2),
        /// Deactivate Interrupt Register.
        (0x1000 => DIR: WriteOnly<u32>),
        (0x1004 => @END),
    }
}

struct ArmGicV2InterruptContext {
    raw_id: usize,
    desc: InterruptDescriptor,
    gic: Arc<SpinLock<ArmGicV2>>,
}

impl InterruptContext for ArmGicV2InterruptContext {
    fn descriptor(&self) -> InterruptDescriptor {
        self.desc
    }
}

impl Drop for ArmGicV2InterruptContext {
    fn drop(&mut self) {
        let gic = self.gic.lock_save_irq();
        gic.cpu.EOIR.set(self.raw_id as u32);
    }
}

struct ArmGicV2 {
    dist: &'static mut GicDistributorRegs,
    cpu: &'static mut GicCpuInterfaceRegs,
    this: Weak<SpinLock<Self>>,
}

impl ArmGicV2 {
    fn new(
        dist: &'static mut GicDistributorRegs,
        cpu: &'static mut GicCpuInterfaceRegs,
        this: Weak<SpinLock<Self>>,
    ) -> Self {
        let mut gic = Self { dist, cpu, this };

        gic.init();

        gic
    }

    fn init(&mut self) {
        // 1. Disable Distributor
        self.dist.CTLR.set(0);

        // 2. Disable CPU Interface
        self.cpu.CTLR.set(0);

        // Read the number of interrupt lines (TYPER: bits [4:0])
        // Each bit represents 32 interrupts, so total interrupts = (TYPER[4:0] + 1) * 32
        let it_lines = (self.dist.TYPER.get() & 0x1F) + 1;
        let num_interrupts = (it_lines * 32) as usize;

        // 3. Configure all interrupts
        // Set all interrupts to Group 0 (secure)
        for i in 0..(num_interrupts / 32) {
            self.dist.IGROUPR[i].set(0); // Group 0 = 0 (secure interrupts)
        }

        // Disable all interrupts
        for i in 0..(num_interrupts / 32) {
            self.dist.ICENABLER[i].set(0xFFFF_FFFF);
        }

        // Clear all pending interrupts
        for i in 0..(num_interrupts / 32) {
            self.dist.ICPENDR[i].set(0xFFFF_FFFF);
        }

        // Set priorities - default all to 0xA0 (medium priority)
        for i in 0..(num_interrupts / 4) {
            // Each IPRIORITYR register covers 4 interrupts, 8 bits each
            self.dist.IPRIORITYR[i].set(0xA0A0_A0A0);
        }

        // Configure interrupts as level triggered (0)
        for i in 0..(num_interrupts / 16) {
            // Each ICFGR register configures 16 interrupts, 2 bits each.
            // 0 = level triggered, 1 = edge triggered.
            self.dist.ICFGR[i].set(0);
        }

        // 4. Enable Distributor
        self.dist.CTLR.set(1);

        // 5. Configure CPU Interface
        // Priority mask register: allow all priorities through (0xFF)
        self.cpu.PMR.set(0xFF);

        // Binary point register: no priority grouping (0)
        self.cpu.BPR.set(0);

        // 6. Enable CPU Interface
        self.cpu.CTLR.set(1);
    }
}

#[derive(Clone, Copy)]
pub(super) struct GicInterruptID(pub usize);

impl TryFrom<InterruptDescriptor> for GicInterruptID {
    type Error = libkernel::error::KernelError;

    fn try_from(value: InterruptDescriptor) -> core::result::Result<Self, Self::Error> {
        match value {
            InterruptDescriptor::Spi(x) if x <= 988 => Ok(Self(x + 32)), // SPI range starts at 32
            InterruptDescriptor::Ppi(x) if x <= 15 => Ok(Self(x + 16)),  // PPI range: 16–31
            InterruptDescriptor::Ipi(x) if x <= 15 => Ok(Self(x)),       // IPI range: 0–15
            _ => Err(KernelError::InvalidValue),
        }
    }
}

unsafe impl Sync for ArmGicV2 {}
unsafe impl Send for ArmGicV2 {}

impl InterruptController for ArmGicV2 {
    fn enable_interrupt(&mut self, cfg: InterruptConfig) {
        if let Ok(GicInterruptID(id)) = GicInterruptID::try_from(cfg.descriptor) {
            let prio_reg_idx = id / 4;
            let prio_byte_shift = (id % 4) * 8;

            let current_prio_reg = self.dist.IPRIORITYR[prio_reg_idx].get();

            let new_prio_reg = (current_prio_reg & !(0xFF << prio_byte_shift)) // Clear the target byte
                         | ((1_u32) << prio_byte_shift); // Set the new value

            self.dist.IPRIORITYR[prio_reg_idx].set(new_prio_reg);

            let target_reg_idx = id / 4;
            let target_byte_shift = (id % 4) * 8;

            let current_target_reg = self.dist.ITARGETSR[target_reg_idx].get();

            let new_target_reg = (current_target_reg & !(0xFF << target_byte_shift)) // Clear the target byte
                           | ((1u32) << target_byte_shift); // Set the new core mask

            self.dist.ITARGETSR[target_reg_idx].set(new_target_reg);

            let icfgr_reg_idx = id / 16;
            let bit_shift = (id % 16) * 2;
            let trigger_bit = 1 << (bit_shift + 1);

            let current_icfgr_reg = self.dist.ICFGR[icfgr_reg_idx].get();

            let new_icfgr_reg = match cfg.trigger {
                TriggerMode::EdgeRising | TriggerMode::EdgeFalling => {
                    current_icfgr_reg | trigger_bit
                }
                TriggerMode::LevelHigh | TriggerMode::LevelLow => current_icfgr_reg & !trigger_bit,
            };

            self.dist.ICFGR[icfgr_reg_idx].set(new_icfgr_reg);

            let isenabler_reg_idx = id / 32;
            let isenabler_bit = id % 32;
            self.dist.ISENABLER[isenabler_reg_idx].set(1 << isenabler_bit);
        }
    }

    fn disable_interrupt(&mut self, i: InterruptDescriptor) {
        if let Ok(GicInterruptID(id)) = GicInterruptID::try_from(i) {
            let reg = id / 32;
            let bit = id % 32;
            self.dist.ICENABLER[reg].set(1 << bit);
        }
    }

    fn raise_ipi(&mut self, _target_cpu_id: usize) {
        todo!()
    }

    fn enable_core(&mut self, _cpu_id: usize) {
        todo!()
    }

    fn read_active_interrupt(&mut self) -> Option<Box<dyn InterruptContext>> {
        let int_id = self.cpu.IAR.get() as usize;

        let descriptor = match int_id {
            0..=15 => InterruptDescriptor::Ipi(int_id),
            16..=31 => InterruptDescriptor::Ppi(int_id - 16),
            32..=1020 => InterruptDescriptor::Spi(int_id - 32),
            _ => return None, // ID 1020-1023 are special (spurious, etc.)
        };

        let gic = self.this.upgrade()?;

        let context = ArmGicV2InterruptContext {
            raw_id: int_id,
            desc: descriptor,
            gic,
        };

        Some(Box::new(context))
    }

    fn parse_fdt_interrupt_regs(
        &self,
        iter: &mut dyn Iterator<Item = u32>,
    ) -> libkernel::error::Result<InterruptConfig> {
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
            0x2 => {
                if matches!(descriptor, InterruptDescriptor::Spi(_)) {
                    return Err(KernelError::InvalidValue);
                }
                EdgeFalling
            }
            0x4 => LevelHigh,
            0x8 => {
                if matches!(descriptor, InterruptDescriptor::Spi(_)) {
                    return Err(KernelError::InvalidValue);
                }
                LevelLow
            }
            _ => return Err(KernelError::InvalidValue),
        };

        Ok(InterruptConfig {
            descriptor,
            trigger,
        })
    }
}

pub fn gic_v2_probe(_dm: &mut DriverManager, d: DeviceDescriptor) -> Result<Arc<dyn Driver>> {
    match d {
        DeviceDescriptor::Fdt(fdt_node, _) => {
            use libkernel::error::ProbeError::*;

            let mut regs = fdt_node.reg().ok_or(NoReg)?;
            let distributor_region = regs.next().ok_or(NoReg)?;
            let cpu_region = regs.next().ok_or(NoReg)?;

            let (distributor_mem, cpu_mem) = {
                let addr_spc = <ArchImpl as VirtualMemory>::kern_address_space();
                let mut kern_addr_spc = addr_spc.lock_save_irq();

                let distributor_mem = kern_addr_spc.map_mmio(PhysMemoryRegion::new(
                    PA::from_value(distributor_region.address as usize),
                    distributor_region.size.ok_or(NoRegSize)?,
                ))?;

                let cpu_mem = kern_addr_spc.map_mmio(PhysMemoryRegion::new(
                    PA::from_value(cpu_region.address as usize),
                    cpu_region.size.ok_or(NoRegSize)?,
                ))?;

                (distributor_mem, cpu_mem)
            };

            info!(
                "ARM Gic V2 initialising: distributor_regs: {distributor_mem:?} cpu_regs: {cpu_mem:?}",
            );

            let dev = Arc::new_cyclic(|this| {
                SpinLock::new(ArmGicV2::new(
                    unsafe { &mut *(distributor_mem.value() as *mut GicDistributorRegs) },
                    unsafe { &mut *(cpu_mem.value() as *mut GicCpuInterfaceRegs) },
                    this.clone(),
                ))
            });

            let manager = InterruptManager::new(fdt_node.name, dev);

            if fdt_prober::is_intc_root(&fdt_node) {
                set_interrupt_root(manager.clone());
            }

            Ok(manager)
        }
    }
}

pub fn arm_gicv2_init(bus: &mut PlatformBus, _dm: &mut DriverManager) -> Result<()> {
    bus.register_platform_driver(
        DeviceMatchType::FdtCompatible("arm,gic-400"),
        Box::new(gic_v2_probe),
    );

    bus.register_platform_driver(
        DeviceMatchType::FdtCompatible("arm,cortex-a15-gic"),
        Box::new(gic_v2_probe),
    );

    Ok(())
}

kernel_driver!(arm_gicv2_init);
