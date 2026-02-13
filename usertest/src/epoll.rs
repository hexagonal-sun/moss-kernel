use crate::register_test;

fn test_epoll() {
    unsafe {
        // create epoll instance
        let epfd = libc::epoll_create1(libc::EPOLL_CLOEXEC);
        assert!(epfd >= 0, "epoll_create1 failed");

        // create a pipe
        let mut fds = [0; 2];
        assert_eq!(libc::pipe(fds.as_mut_ptr()), 0, "pipe failed");

        // add read end of pipe to epoll with EPOLLIN
        let mut ev = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: fds[0] as u64, // store fd in data
        };
        assert_eq!(
            libc::epoll_ctl(epfd, libc::EPOLL_CTL_ADD, fds[0], &mut ev as *mut _),
            0,
            "epoll_ctl ADD failed"
        );

        // write to pipe to trigger EPOLLIN
        let msg = b"x";
        let written = libc::write(fds[1], msg.as_ptr() as *const _, msg.len());
        assert_eq!(written, msg.len() as isize, "write failed");

        // wait for the event
        let mut out: libc::epoll_event = std::mem::zeroed();
        let n = libc::epoll_wait(epfd, &mut out as *mut _, 1, 100);
        assert_eq!(n, 1, "epoll_wait did not return 1");
        assert!((out.events & libc::EPOLLIN as u32) != 0, "EPOLLIN not set");
    }
}

register_test!(test_epoll);
