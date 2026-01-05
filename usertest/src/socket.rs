use libc::{AF_INET, AF_UNIX, SOCK_DGRAM, SOCK_STREAM};
use libc::{accept, bind, connect, listen, shutdown, socket};

pub fn test_tcp_socket_creation() {
    print!("Testing TCP socket creation ... ");
    unsafe {
        let sockfd = socket(AF_INET, SOCK_STREAM, 0);
        if sockfd < 0 {
            panic!("Failed to create TCP socket");
        }
    }
    println!("OK");
}

pub fn test_unix_socket_creation() {
    print!("Testing UNIX stream socket creation ... ");
    unsafe {
        let sockfd = socket(AF_UNIX, SOCK_STREAM, 0);
        if sockfd < 0 {
            panic!("Failed to create UNIX stream socket");
        }
    }
    println!("OK");

    print!("Testing UNIX datagram socket creation ... ");
    unsafe {
        let sockfd = socket(AF_UNIX, SOCK_DGRAM, 0);
        if sockfd < 0 {
            panic!("Failed to create UNIX datagram socket");
        }
    }
    println!("OK");
}

pub fn test_unix_socket_basic_functions() {
    print!("Testing UNIX socket functions ... ");
    let sockfd = unsafe { socket(AF_UNIX, SOCK_STREAM, 0) };
    if sockfd < 0 {
        panic!("Failed to create UNIX stream socket for function tests");
    }
    let path = "/tmp/test_socket";
    let sockaddr = libc::sockaddr_un {
        sun_family: AF_UNIX as u16,
        sun_path: {
            let mut path_array = [0u8; 108];
            for (i, &b) in path.as_bytes().iter().enumerate() {
                path_array[i] = b;
            }
            path_array
        },
    };
    let bind_result = unsafe {
        bind(
            sockfd,
            &sockaddr as *const libc::sockaddr_un as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as u32,
        )
    };
    if bind_result < 0 {
        panic!("Failed to bind UNIX socket");
    }
    let listen_result = unsafe { listen(sockfd, 5) };
    if listen_result < 0 {
        panic!("Failed to listen on UNIX socket");
    }
    let shutdown_result = unsafe { shutdown(sockfd, 2) };
    if shutdown_result < 0 {
        panic!("Failed to shutdown UNIX socket");
    }
    println!("OK");
}

pub fn test_unix_socket_fork_msg_passing() {
    use std::ptr;

    print!("Testing UNIX socket fork message passing ... ");

    // Create server socket, bind and listen before fork
    let server_fd = unsafe { socket(AF_UNIX, SOCK_STREAM, 0) };
    if server_fd < 0 {
        panic!("Failed to create server UNIX socket");
    }

    let path = "/tmp/uds_fork_test";
    let sockaddr = libc::sockaddr_un {
        sun_family: AF_UNIX as u16,
        sun_path: {
            let mut path_array = [0u8; 108];
            for (i, &b) in path.as_bytes().iter().enumerate() {
                path_array[i] = b;
            }
            path_array
        },
    };

    let ret = unsafe {
        bind(
            server_fd,
            &sockaddr as *const libc::sockaddr_un as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as u32,
        )
    };
    if ret < 0 {
        panic!("Server bind failed");
    }
    let ret = unsafe { listen(server_fd, 1) };
    if ret < 0 {
        panic!("Server listen failed");
    }

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        panic!("fork failed");
    }

    if pid == 0 {
        // Child: client
        let client_fd = unsafe { socket(AF_UNIX, SOCK_STREAM, 0) };
        if client_fd < 0 {
            panic!("Client socket creation failed");
        }
        let ret = unsafe {
            connect(
                client_fd,
                &sockaddr as *const libc::sockaddr_un as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_un>() as u32,
            )
        };
        if ret < 0 {
            panic!("Client connect failed");
        }

        // Send request
        let req = b"hello";
        let wr = unsafe { libc::write(client_fd, req.as_ptr() as *const _, req.len()) };
        if wr != req.len() as isize {
            panic!("Client write failed");
        }

        // Receive response
        let mut resp = [0u8; 5];
        let rd = unsafe { libc::read(client_fd, resp.as_mut_ptr() as *mut _, resp.len()) };
        if rd != resp.len() as isize || &resp != b"world" {
            panic!("Client read failed");
        }

        unsafe { libc::close(client_fd) };
        unsafe { libc::_exit(0) };
    } else {
        // Parent: server
        let conn_fd = unsafe { accept(server_fd, ptr::null_mut(), ptr::null_mut()) };
        if conn_fd < 0 {
            panic!("Server accept failed");
        }

        // Receive request
        let mut buf = [0u8; 5];
        let rd = unsafe { libc::read(conn_fd, buf.as_mut_ptr() as *mut _, buf.len()) };
        if rd != buf.len() as isize || &buf != b"hello" {
            panic!("Server read failed");
        }

        // Send response
        let resp = b"world";
        let wr = unsafe { libc::write(conn_fd, resp.as_ptr() as *const _, resp.len()) };
        if wr != resp.len() as isize {
            panic!("Server write failed");
        }

        // Wait for child
        let mut status = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };
        if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
            panic!("Client process did not exit cleanly");
        }

        unsafe { libc::close(conn_fd) };
        unsafe { libc::close(server_fd) };
        println!("OK");
    }
}
