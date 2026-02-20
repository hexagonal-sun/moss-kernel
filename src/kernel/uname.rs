use crate::kernel::hostname::hostname;
use crate::{
    arch::{Arch, ArchImpl},
    memory::uaccess::{copy_to_user, UserCopyable},
};
use alloc::ffi::CString;
use core::ffi::c_char;
use core::ffi::CStr;
use core::str::FromStr;
use libkernel::{error::Result, memory::address::TUA};

const SYSNAME: &CStr = c"Moss";

/// Systemd uses the release field to determine compatibility.
/// It's also necessary for libc programs; otherwise they exit with an error Kernel too old.
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

    let version = CString::from_str(env!("MOSS_VERSION")).unwrap();
    copy_str_to_c_char_arr(&mut uts.version, version.as_c_str().to_bytes_with_nul());

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
    use crate::kernel::uname::{build_utsname, SYSNAME};
    use crate::ktest;
    use core::ffi::CStr;

    ktest! {
        fn sysname_correct() {
            let uts = build_utsname();
            let sysname_cstr = unsafe { CStr::from_ptr(uts.sysname.as_ptr()) };
            assert_eq!(sysname_cstr, SYSNAME);
        }
    }

    fn validate_datetime(datetime: &str) {
        let mut parts = datetime.splitn(6, ' ');
        let day_of_week = parts.next().expect("Day of week"); // "Tue"
        let month = parts.next().expect("Month"); // "Feb"
        let day = parts.next().expect("Day"); // "20"
        let time = parts.next().expect("Time"); // "12:34:56"
        let timezone = parts.next().expect("TimeZone"); // "UTC"
        let year = parts.next().expect("Year"); // "2024"

        assert_eq!(timezone, "UTC");
        assert!(year.parse::<u16>().is_ok());
        assert!(year.starts_with("20"));
        assert!(time.split(':').all(|s| s.len() == 2));
        assert_eq!(month.len(), 3);
        assert_eq!(day_of_week.len(), 3);
        assert!(day.parse::<u8>().is_ok());
    }

    fn validate_version(version: &str, smp: bool) {
        let mut parts = if smp {
            version.splitn(4, ' ')
        } else {
            version.splitn(3, ' ')
        };

        let build_num = parts.next().unwrap(); // "#1"
        let sysname = parts.next().unwrap(); // "Moss"
        let smp = parts.next().unwrap(); // "SMP"
        let datetime = parts.next().unwrap(); // "Tue Feb 20 12:34:56 UTC 2024"

        assert!(
            build_num.starts_with('#'),
            "Build number should start with '#'"
        );
        assert_eq!(sysname, "Moss");
        assert_eq!(smp, "SMP");

        validate_datetime(datetime)
    }

    ktest! {
        // Test that the version string is of the format "#1 Moss SMP Tue Feb 20 12:34:56 UTC 2024"
        fn version_format_smp() {
            let uts = build_utsname();
            let version_cstr = unsafe { CStr::from_ptr(uts.version.as_ptr()) };
            let version = version_cstr.to_str().unwrap();

            #[cfg(feature = "smp")]
            validate_version(version, true);
            #[cfg(not(feature = "smp"))]
            validate_version(version, false);
        }
    }
}
