use crate::register_test;
use std::os::unix::io::AsRawFd;

fn test_epoll_create() {
    unsafe {
        let fd = libc::epoll_create1(0);
        assert!(
            fd >= 0,
            "epoll_create1 failed: {}",
            std::io::Error::last_os_error()
        );
        libc::close(fd);
    }
}
register_test!(test_epoll_create);

fn test_epoll_add_wait() {
    unsafe {
        let epfd = libc::epoll_create1(0);
        assert!(epfd >= 0);

        let mut fds = [0; 2];
        let res = libc::pipe(fds.as_mut_ptr());
        assert_eq!(res, 0);

        let mut ev = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: 42,
        };

        let res = libc::epoll_ctl(epfd, libc::EPOLL_CTL_ADD, fds[0], &mut ev);
        assert_eq!(res, 0);

        // write to pipe
        let data = [1u8; 1];
        libc::write(fds[1], data.as_ptr() as *const libc::c_void, 1);

        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 1];
        let res = libc::epoll_wait(epfd, events.as_mut_ptr(), 1, 100);
        assert_eq!(res, 1);
        assert_eq!(events[0].u64, 42);

        libc::close(fds[0]);
        libc::close(fds[1]);
        libc::close(epfd);
    }
}
register_test!(test_epoll_add_wait);
