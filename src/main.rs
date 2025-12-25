#![no_std]
#![no_main]
#![feature(used_with_arg)]
#![feature(likely_unlikely)]

use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use arch::{Arch, ArchImpl};
use core::panic::PanicInfo;
use drivers::{fdt_prober::get_fdt, fs::register_fs_drivers};
use fs::VFS;
use getargs::{Opt, Options};
use libkernel::{
    CpuOps, VirtualMemory,
    fs::{
        BlockDevice, OpenFlags, attr::FilePermissions, blk::ramdisk::RamdiskBlkDev, path::Path,
        pathbuf::PathBuf,
    },
    memory::{
        address::{PA, VA},
        region::PhysMemoryRegion,
    },
};
use log::{error, warn};
use process::ctx::UserCtx;
use sched::{current_task, sched_init, spawn_kernel_work, uspc_ret::dispatch_userspace_task};

extern crate alloc;

mod arch;
mod clock;
mod console;
mod drivers;
mod fs;
mod interrupts;
mod kernel;
mod memory;
mod process;
mod sched;
mod sync;

#[panic_handler]
fn on_panic(info: &PanicInfo) -> ! {
    ArchImpl::disable_interrupts();

    let panic_msg = info.message();

    if let Some(location) = info.location() {
        error!(
            "Kernel panicked at {}:{}:{}: {}",
            location.file(),
            location.line(),
            location.column(),
            panic_msg
        );
    } else {
        error!("Kernel panicked at unknown location: {panic_msg}");
    }

    ArchImpl::power_off();
}

async fn launch_init(opts: KOptions) {
    let init = opts
        .init
        .unwrap_or_else(|| panic!("No init specified in kernel command line"));

    let dt = get_fdt();

    let initrd_block_dev: Option<Box<dyn BlockDevice>> = if let Some(chosen) =
        dt.find_nodes("/chosen").next()
        && let Some(start_addr) = chosen
            .find_property("linux,initrd-start")
            .map(|prop| prop.u64())
        && let Some(end_addr) = chosen
            .find_property("linux,initrd-end")
            .map(|prop| prop.u64())
    {
        let region = PhysMemoryRegion::from_start_end_address(
            PA::from_value(start_addr as _),
            PA::from_value(end_addr as _),
        );

        Some(Box::new(
            RamdiskBlkDev::new(
                region,
                VA::from_value(0xffff_9800_0000_0000),
                &mut *ArchImpl::kern_address_space().lock_save_irq(),
            )
            .unwrap(),
        ))
    } else {
        None
    };

    let root_fs = opts
        .root_fs
        .unwrap_or_else(|| panic!("No root FS driver specified in kernel command line"));

    VFS.mount_root(&root_fs, initrd_block_dev)
        .await
        .unwrap_or_else(|e| panic!("Failed to mount root FS: {}", e));

    // Process all automounts.
    for (path, fs) in opts.automounts.iter() {
        let mount_point = VFS
            .resolve_path_absolute(path, VFS.root_inode())
            .await
            .unwrap_or_else(|e| panic!("Could not find automount path: {}. {e}", path.as_str()));

        VFS.mount(mount_point, fs, None)
            .await
            .unwrap_or_else(|e| panic!("Automount failed: {e}"));
    }

    let inode = VFS
        .resolve_path_absolute(&init, VFS.root_inode())
        .await
        .expect("Unable to find init");

    let task = current_task();

    // Ensure that the exec() call applies to init.
    assert!(task.process.tgid.is_init());

    // Now that the root fs has been mounted, set the real root inode as the
    // cwd and root.
    *task.cwd.lock_save_irq() = (VFS.root_inode(), PathBuf::new());
    *task.root.lock_save_irq() = (VFS.root_inode(), PathBuf::new());

    let console = VFS
        .open(
            Path::new("/dev/console"),
            OpenFlags::O_RDWR,
            VFS.root_inode(),
            FilePermissions::empty(),
            task.clone(),
        )
        .await
        .expect("Could not open console for init process");

    {
        let mut fd_table = task.fd_table.lock_save_irq();

        // stdin, stdout, stderr
        fd_table
            .insert(console.clone())
            .expect("Could not clone FD");
        fd_table
            .insert(console.clone())
            .expect("Could not clone FD");
        fd_table
            .insert(console.clone())
            .expect("Could not clone FD");
    }

    process::exec::kernel_exec(inode, vec![init.as_str().to_string()], vec![])
        .await
        .expect("Could not launch init process");
}

struct KOptions {
    init: Option<PathBuf>,
    root_fs: Option<String>,
    automounts: Vec<(PathBuf, String)>,
}

fn parse_args(args: &str) -> KOptions {
    let mut kopts = KOptions {
        init: None,
        root_fs: None,
        automounts: Vec::new(),
    };

    let mut opts = Options::new(args.split(" "));

    loop {
        match opts.next_opt() {
            Ok(Some(arg)) => match arg {
                Opt::Long("init") => kopts.init = Some(PathBuf::from(opts.value().unwrap())),
                Opt::Long("rootfs") => kopts.root_fs = Some(opts.value().unwrap().to_string()),
                Opt::Long("automount") => {
                    let string = opts.value().unwrap();
                    let mut split = string.split(",");
                    let path = split.next().unwrap();
                    let fs = split.next().unwrap();

                    kopts.automounts.push((PathBuf::from(path), fs.to_string()));
                }
                Opt::Long(x) => warn!("Unknown option {}", x),
                Opt::Short(x) => warn!("Unknown option {}", x),
            },
            Ok(None) => return kopts,
            Err(e) => error!("Could not parse option: {}, ignoring.", e),
        }
    }
}

pub fn kmain(args: String, ctx_frame: *mut UserCtx) {
    sched_init();

    register_fs_drivers();

    let kopts = parse_args(&args);

    spawn_kernel_work(launch_init(kopts));

    dispatch_userspace_task(ctx_frame)
}
