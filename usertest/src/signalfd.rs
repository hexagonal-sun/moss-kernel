use crate::register_test;
use std::collections::BTreeSet;

const KERNEL_SIGSET_SIZE: usize = std::mem::size_of::<u64>();

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct SignalfdSiginfo {
    ssi_signo: u32,
    ssi_errno: i32,
    ssi_code: i32,
    ssi_pid: u32,
    ssi_uid: u32,
    ssi_fd: i32,
    ssi_tid: u32,
    ssi_band: u32,
    ssi_overrun: u32,
    ssi_trapno: u32,
    ssi_status: i32,
    ssi_int: i32,
    ssi_ptr: u64,
    ssi_utime: u64,
    ssi_stime: u64,
    ssi_addr: u64,
    pad: [u8; 48],
}

fn sigset(signals: &[libc::c_int]) -> libc::sigset_t {
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        assert_eq!(libc::sigemptyset(&mut mask), 0);
        for &signal in signals {
            assert_eq!(libc::sigaddset(&mut mask, signal), 0);
        }
        mask
    }
}

unsafe fn block_signals(mask: &libc::sigset_t) -> libc::sigset_t {
    let mut old_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::sigprocmask(libc::SIG_BLOCK, mask, &mut old_mask) };
    assert_eq!(
        rc,
        0,
        "sigprocmask failed: {}",
        std::io::Error::last_os_error()
    );
    old_mask
}

unsafe fn restore_sigmask(old_mask: &libc::sigset_t) {
    let rc = unsafe { libc::sigprocmask(libc::SIG_SETMASK, old_mask, std::ptr::null_mut()) };
    assert_eq!(
        rc,
        0,
        "sigprocmask restore failed: {}",
        std::io::Error::last_os_error()
    );
}

unsafe fn signalfd4(fd: libc::c_int, mask: &libc::sigset_t, flags: libc::c_int) -> libc::c_int {
    unsafe {
        libc::syscall(
            libc::SYS_signalfd4,
            fd,
            mask as *const libc::sigset_t,
            KERNEL_SIGSET_SIZE,
            flags,
        ) as libc::c_int
    }
}

unsafe fn read_one(fd: libc::c_int) -> SignalfdSiginfo {
    let mut info: SignalfdSiginfo = unsafe { std::mem::zeroed() };
    let bytes = unsafe {
        libc::read(
            fd,
            &mut info as *mut _ as *mut libc::c_void,
            std::mem::size_of::<SignalfdSiginfo>(),
        )
    };

    assert_eq!(
        bytes,
        std::mem::size_of::<SignalfdSiginfo>() as isize,
        "read failed: {}",
        std::io::Error::last_os_error()
    );
    info
}

unsafe fn expect_errno(ret: libc::c_int, errno: libc::c_int) {
    assert_eq!(ret, -1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(errno));
}

fn test_signalfd_basic_read() {
    assert_eq!(std::mem::size_of::<SignalfdSiginfo>(), 128);

    unsafe {
        let mask = sigset(&[libc::SIGUSR1]);
        let old_mask = block_signals(&mask);

        let fd = signalfd4(-1, &mask, 0);
        assert!(
            fd >= 0,
            "signalfd4 failed: {}",
            std::io::Error::last_os_error()
        );

        assert_eq!(libc::kill(libc::getpid(), libc::SIGUSR1), 0);

        let info = read_one(fd);
        assert_eq!(info.ssi_signo, libc::SIGUSR1 as u32);

        libc::close(fd);
        restore_sigmask(&old_mask);
    }
}

register_test!(test_signalfd_basic_read);

fn test_signalfd_nonblock_and_cloexec() {
    unsafe {
        let mask = sigset(&[libc::SIGUSR1]);
        let old_mask = block_signals(&mask);

        let fd = signalfd4(-1, &mask, libc::O_NONBLOCK | libc::O_CLOEXEC);
        assert!(
            fd >= 0,
            "signalfd4 failed: {}",
            std::io::Error::last_os_error()
        );

        let status_flags = libc::fcntl(fd, libc::F_GETFL);
        assert!(
            status_flags >= 0,
            "F_GETFL failed: {}",
            std::io::Error::last_os_error()
        );
        assert_ne!(status_flags & libc::O_NONBLOCK, 0);

        let fd_flags = libc::fcntl(fd, libc::F_GETFD);
        assert!(
            fd_flags >= 0,
            "F_GETFD failed: {}",
            std::io::Error::last_os_error()
        );
        assert_ne!(fd_flags & libc::FD_CLOEXEC, 0);

        let mut info: SignalfdSiginfo = std::mem::zeroed();
        let ret = libc::read(
            fd,
            &mut info as *mut _ as *mut libc::c_void,
            std::mem::size_of::<SignalfdSiginfo>(),
        ) as libc::c_int;
        expect_errno(ret, libc::EAGAIN);

        libc::close(fd);
        restore_sigmask(&old_mask);
    }
}

register_test!(test_signalfd_nonblock_and_cloexec);

fn test_signalfd_reads_multiple_pending_signals() {
    unsafe {
        let mask = sigset(&[libc::SIGUSR1, libc::SIGUSR2]);
        let old_mask = block_signals(&mask);

        let fd = signalfd4(-1, &mask, 0);
        assert!(
            fd >= 0,
            "signalfd4 failed: {}",
            std::io::Error::last_os_error()
        );

        assert_eq!(libc::kill(libc::getpid(), libc::SIGUSR2), 0);
        assert_eq!(libc::kill(libc::getpid(), libc::SIGUSR1), 0);

        let mut infos: [SignalfdSiginfo; 2] = std::mem::zeroed();
        let bytes = libc::read(
            fd,
            infos.as_mut_ptr() as *mut libc::c_void,
            std::mem::size_of_val(&infos),
        );

        assert_eq!(
            bytes,
            std::mem::size_of_val(&infos) as isize,
            "read failed: {}",
            std::io::Error::last_os_error()
        );

        let signals = infos
            .iter()
            .map(|info| info.ssi_signo as libc::c_int)
            .collect::<BTreeSet<_>>();

        assert_eq!(signals, BTreeSet::from([libc::SIGUSR1, libc::SIGUSR2]));

        libc::close(fd);
        restore_sigmask(&old_mask);
    }
}

register_test!(test_signalfd_reads_multiple_pending_signals);

fn test_signalfd_updates_mask_and_rejects_invalid_inputs() {
    unsafe {
        let mask = sigset(&[libc::SIGUSR1, libc::SIGUSR2]);
        let old_mask = block_signals(&mask);

        let initial_mask = sigset(&[libc::SIGUSR1]);
        let updated_mask = sigset(&[libc::SIGUSR2]);

        let fd = signalfd4(-1, &initial_mask, libc::O_NONBLOCK);
        assert!(
            fd >= 0,
            "signalfd4 failed: {}",
            std::io::Error::last_os_error()
        );

        assert_eq!(signalfd4(fd, &updated_mask, 0), fd);

        assert_eq!(libc::kill(libc::getpid(), libc::SIGUSR1), 0);
        let mut info: SignalfdSiginfo = std::mem::zeroed();
        let ret = libc::read(
            fd,
            &mut info as *mut _ as *mut libc::c_void,
            std::mem::size_of::<SignalfdSiginfo>(),
        ) as libc::c_int;
        expect_errno(ret, libc::EAGAIN);

        assert_eq!(libc::kill(libc::getpid(), libc::SIGUSR2), 0);
        let info = read_one(fd);
        assert_eq!(info.ssi_signo, libc::SIGUSR2 as u32);

        assert_eq!(signalfd4(fd, &mask, 0), fd);
        let info = read_one(fd);
        assert_eq!(info.ssi_signo, libc::SIGUSR1 as u32);

        let invalid_flags_fd = signalfd4(-1, &initial_mask, 0x4);
        expect_errno(invalid_flags_fd, libc::EINVAL);

        let invalid_fd = signalfd4(123456, &initial_mask, 0);
        expect_errno(invalid_fd, libc::EBADF);

        let mut pipe_fds = [0; 2];
        assert_eq!(libc::pipe(pipe_fds.as_mut_ptr()), 0);
        let not_signalfd = signalfd4(pipe_fds[0], &initial_mask, 0);
        expect_errno(not_signalfd, libc::EINVAL);
        libc::close(pipe_fds[0]);
        libc::close(pipe_fds[1]);

        libc::close(fd);
        restore_sigmask(&old_mask);
    }
}

register_test!(test_signalfd_updates_mask_and_rejects_invalid_inputs);

fn test_signalfd_epoll_readiness() {
    unsafe {
        let mask = sigset(&[libc::SIGUSR1]);
        let old_mask = block_signals(&mask);

        let fd = signalfd4(-1, &mask, libc::O_NONBLOCK);
        assert!(
            fd >= 0,
            "signalfd4 failed: {}",
            std::io::Error::last_os_error()
        );

        let epfd = libc::epoll_create1(0);
        assert!(
            epfd >= 0,
            "epoll_create1 failed: {}",
            std::io::Error::last_os_error()
        );

        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: 0x51A1_FD,
        };
        assert_eq!(
            libc::epoll_ctl(epfd, libc::EPOLL_CTL_ADD, fd, &mut event),
            0
        );

        assert_eq!(libc::kill(libc::getpid(), libc::SIGUSR1), 0);

        let mut ready = [libc::epoll_event { events: 0, u64: 0 }; 1];
        let n = libc::epoll_wait(epfd, ready.as_mut_ptr(), ready.len() as i32, 100);
        assert_eq!(
            n,
            1,
            "epoll_wait failed: {}",
            std::io::Error::last_os_error()
        );
        assert_eq!(ready[0].u64, 0x51A1_FD);
        assert_ne!(ready[0].events & libc::EPOLLIN as u32, 0);

        let info = read_one(fd);
        assert_eq!(info.ssi_signo, libc::SIGUSR1 as u32);

        libc::close(epfd);
        libc::close(fd);
        restore_sigmask(&old_mask);
    }
}

register_test!(test_signalfd_epoll_readiness);
