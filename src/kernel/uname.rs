use crate::kernel::hostname::hostname;
use crate::{
    arch::{Arch, ArchImpl},
    memory::uaccess::{UserCopyable, copy_to_user},
};
use alloc::ffi::CString;
use core::{ffi::c_char};
use core::ffi::CStr;
use core::str::FromStr;
use libkernel::{error::Result, memory::address::TUA};

const SYSNAME: &CStr = c"Moss";
const RELEASE: &CStr = c"4.2.3";

///  POSIX specifies the order when using -a (equivalent to -snrvm):
///   1. sysname (-s) - OS name
///   2. nodename (-n) - hostname
///   3. release (-r) - OS release
///   4. version (-v) - OS version
///   5. machine (-m) - hardware type
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OldUtsname {
    sysname: [c_char; 65],
    nodename: [c_char; 65],
    release: [c_char; 65],
    version: [c_char; 65],
    machine: [c_char; 65],
}

impl Default for OldUtsname {
    fn default() -> Self {
        Self {
            sysname: [0; 65],
            nodename: [0; 65],
            release: [0; 65],
            version: [0; 65],
            machine: [0; 65],
        }
    }
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

/// Build an `OldUtsname` struct with the current system information, without involving the
/// kernel. This makes it easier to test.
fn build_utsname() -> OldUtsname {
    let mut uts = OldUtsname::default();

    copy_str_to_c_char_arr(&mut uts.sysname, SYSNAME.to_bytes_with_nul());

    let nodename = CString::from_str(&hostname().lock_save_irq()).unwrap();
    copy_str_to_c_char_arr(&mut uts.nodename, nodename.as_c_str().to_bytes_with_nul());

    copy_str_to_c_char_arr(&mut uts.release, RELEASE.to_bytes_with_nul());

    #[cfg(feature = "smp")]
    let version = c"#1 Moss SMP Tue Feb 20 12:34:56 UTC 2024".to_bytes_with_nul();
    #[cfg(not(feature = "smp"))]
    let version = c"#1 Moss Tue Feb 20 12:34:56 UTC 2024".to_bytes_with_nul();
    copy_str_to_c_char_arr(&mut uts.version, version);

    let machine = CString::new(ArchImpl::name()).unwrap();
    let machine = machine.to_bytes_with_nul();
    copy_str_to_c_char_arr(&mut uts.machine, machine);

    uts
}

/// Implement the uname syscall, returning 0 for success
pub async fn sys_uname(uts_ptr: TUA<OldUtsname>) -> Result<usize> {
    let uts = build_utsname();
    copy_to_user(uts_ptr, uts).await?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use core::ffi::CStr;
    use crate::kernel::uname::{build_utsname, SYSNAME};

    #[test]
    fn version_conforms_to_format() {
        let uts = build_utsname();
        let sysname_cstr = unsafe { CStr::from_ptr(uts.sysname.as_ptr()) };
        assert_eq!(sysname_cstr, SYSNAME);
    }
}