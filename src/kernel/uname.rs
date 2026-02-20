use crate::kernel::hostname::hostname;
use crate::{
    arch::{Arch, ArchImpl},
    memory::uaccess::{UserCopyable, copy_to_user},
};
use alloc::ffi::CString;
use core::str::FromStr;
use core::{ffi::c_char, mem};
use libkernel::{error::Result, memory::address::TUA};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OldUtsname {
    sysname: [c_char; 65],
    nodename: [c_char; 65],
    release: [c_char; 65],
    version: [c_char; 65],
    machine: [c_char; 65],
}

unsafe impl UserCopyable for OldUtsname {}

fn copy_str_to_c_char_arr(dest: &mut [c_char], src: &[u8]) {
    let len = core::cmp::min(dest.len(), src.len());

    // This is safe because c_char is i8, which has the same size and alignment
    // as u8. We are just changing the "signedness" of the byte while copying.
    unsafe {
        let dest_ptr = dest.as_mut_ptr();
        let dest_slice = core::slice::from_raw_parts_mut(dest_ptr, dest.len());
        dest_slice[..len].copy_from_slice(&src[..len]);
    }
    // The rest of `dest` will remain zeroed from the initial `mem::zeroed`.
}

pub async fn sys_uname(uts_ptr: TUA<OldUtsname>) -> Result<usize> {
    let mut uts = unsafe { mem::zeroed::<OldUtsname>() };

    let sysname = c"Moss".to_bytes_with_nul();
    copy_str_to_c_char_arr(&mut uts.sysname, sysname);

    let nodename = CString::from_str(&hostname().lock_save_irq()).unwrap();
    copy_str_to_c_char_arr(&mut uts.nodename, nodename.as_c_str().to_bytes_with_nul());

    let release = c"5.7.1".to_bytes_with_nul();
    copy_str_to_c_char_arr(&mut uts.release, release);

    #[cfg(feature = "smp")]
    let version = c"#1 Moss SMP Tue Feb 20 12:34:56 UTC 2024".to_bytes_with_nul();
    #[cfg(not(feature = "smp"))]
    let version = c"#1 Moss Tue Feb 20 12:34:56 UTC 2024".to_bytes_with_nul();
    copy_str_to_c_char_arr(&mut uts.version, version);

    let machine = CString::new(ArchImpl::name()).unwrap();
    let machine = machine.to_bytes_with_nul();
    copy_str_to_c_char_arr(&mut uts.machine, machine);

    copy_to_user(uts_ptr, uts).await?;

    Ok(0)
}
