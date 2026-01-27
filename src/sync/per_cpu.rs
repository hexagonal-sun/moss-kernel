/// A declarative macro to define a static `PerCpu` variable and register it
/// for automatic initialization.
#[macro_export]
macro_rules! per_cpu {
    ($vis:vis static $name:ident: $type:ty = $initializer:expr;) => {
        $vis static $name: libkernel::sync::per_cpu::PerCpu<$type, $crate::arch::ArchImpl> =
            libkernel::sync::per_cpu::PerCpu::new($initializer);

        paste::paste! {
        #[unsafe(no_mangle)]
        #[unsafe(link_section = ".percpu")]
        #[used(linker)]
        static [<$name _PERCPU_INITIALIZER>]: &'static (
                     dyn libkernel::sync::per_cpu::PerCpuInitializer + Sync
                 ) = &$name;
        }
    };
}

/// Wraps with a [`RefCell`] for convenience
#[macro_export]
macro_rules! local_per_cpu {
    ($vis:vis static $name:ident: $type:ty = $initializer:expr;) => {
        $vis static $name: libkernel::sync::per_cpu::PerCpu<
            core::cell::RefCell<$type>,
            $crate::arch::ArchImpl,
        > = libkernel::sync::per_cpu::PerCpu::new(|| {core::cell::RefCell::new($initializer())});

        paste::paste! {
        #[unsafe(no_mangle)]
        #[unsafe(link_section = ".percpu")]
        #[used(linker)]
        static [<$name _PERCPU_INITIALIZER>]: &'static (
                     dyn libkernel::sync::per_cpu::PerCpuInitializer + Sync
                 ) = &$name;
        }
    };
}
