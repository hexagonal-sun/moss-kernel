use crate::register_test;
use std::{
    ffi::CString,
    fs, io,
    os::fd::RawFd,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const IN_NONBLOCK: i32 = libc::O_NONBLOCK;
const IN_CREATE: u32 = 0x0000_0100;
const IN_DELETE: u32 = 0x0000_0200;
const IN_IGNORED: u32 = 0x0000_8000;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct InotifyEvent {
    wd: i32,
    mask: u32,
    cookie: u32,
    len: u32,
}

fn read_event(fd: RawFd) -> Option<(InotifyEvent, String)> {
    let mut buf = [0u8; 256];

    for _ in 0..50 {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            assert!(n as usize >= core::mem::size_of::<InotifyEvent>());

            let header = unsafe { *(buf.as_ptr().cast::<InotifyEvent>()) };
            let name = if header.len == 0 {
                String::new()
            } else {
                let name_bytes = &buf[core::mem::size_of::<InotifyEvent>()
                    ..core::mem::size_of::<InotifyEvent>() + header.len as usize];
                let nul = name_bytes
                    .iter()
                    .position(|b| *b == 0)
                    .unwrap_or(name_bytes.len());
                String::from_utf8_lossy(&name_bytes[..nul]).into_owned()
            };

            return Some((header, name));
        }

        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EAGAIN) {
            thread::sleep(Duration::from_millis(10));
            continue;
        }

        panic!("read failed: {err}");
    }

    None
}

fn test_inotify_create_and_rm_watch() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = format!("/tmp/inotify_test_{unique}");
    let file_name = "created.txt";
    let file_path = format!("{dir}/{file_name}");

    fs::create_dir(&dir).expect("create_dir failed");

    unsafe {
        let fd = libc::syscall(libc::SYS_inotify_init1, IN_NONBLOCK) as i32;
        assert!(
            fd >= 0,
            "inotify_init1 failed: {}",
            io::Error::last_os_error()
        );

        let c_dir = CString::new(dir.clone()).unwrap();
        let wd = libc::syscall(
            libc::SYS_inotify_add_watch,
            fd,
            c_dir.as_ptr(),
            (IN_CREATE | IN_DELETE) as libc::c_uint,
        ) as i32;
        assert!(
            wd >= 0,
            "inotify_add_watch failed: {}",
            io::Error::last_os_error()
        );

        fs::write(&file_path, b"hello").expect("write failed");

        let (event, name) = read_event(fd).expect("timed out waiting for inotify event");
        assert_eq!(event.wd, wd);
        assert_ne!(event.mask & IN_CREATE, 0, "missing IN_CREATE: {event:?}");
        assert_eq!(name, file_name);

        let ret = libc::syscall(libc::SYS_inotify_rm_watch, fd, wd);
        assert_eq!(
            ret,
            0,
            "inotify_rm_watch failed: {}",
            io::Error::last_os_error()
        );

        let (event, name) = read_event(fd).expect("timed out waiting for IN_IGNORED");
        assert_eq!(event.wd, wd);
        assert_ne!(event.mask & IN_IGNORED, 0, "missing IN_IGNORED: {event:?}");
        assert!(name.is_empty());

        libc::close(fd);
    }

    fs::remove_file(&file_path).expect("remove_file failed");
    fs::remove_dir(&dir).expect("remove_dir failed");
}

register_test!(test_inotify_create_and_rm_watch);
