# Syscalls (x86_64)

| Number | Name      | Signature / Notes                                                                       | Kernel symbol / handler            | Implemented |
|--------|-----------|-----------------------------------------------------------------------------------------|------------------------------------|-------------|
| 0 (0x0)    | read      | (unsigned int fd, char *buf, size_t count)                                             | `sys_read`                          | true        |
| 1 (0x1)    | write     | (unsigned int fd, const char *buf, size_t count)                                       | `sys_write`                         | true        |
| 2 (0x2)    | open      | (const char *pathname, int flags, mode_t mode) -> mapped to `openat(AT_FDCWD, ...)`    | `sys_openat`                        | true        |
| 3 (0x3)    | close     | (unsigned int fd)                                                                       | `sys_close`                         | true        |
| 5 (0x5)    | fstat     | (unsigned int fd, struct stat *statbuf)                                                | `sys_fstat`                         | true        |
| 7 (0x7)    | poll      | (struct pollfd *ufds, unsigned int nfds, int timeout) -> mapped to `ppoll`              | `sys_ppoll`                         | true        |
| 8 (0x8)    | lseek     | (unsigned int fd, off_t offset, unsigned int whence)                                   | `sys_lseek`                         | true        |
| 9 (0x9)    | mmap      | (void *addr, size_t len, int prot, int flags, int fd, off_t offset)                    | `sys_mmap`                          | true        |
| 10 (0xa)   | mprotect  | (void *addr, size_t len, int prot)                                                      | `sys_mprotect`                      | true        |
| 11 (0xb)   | munmap    | (void *addr, size_t len)                                                                | `sys_munmap`                        | true        |
| 16 (0x10)  | ioctl     | (unsigned int fd, unsigned int cmd, unsigned long arg)                                 | `sys_ioctl`                         | true        |
| 23 (0x17)  | select    | (int nfds, fd_set *readfds, fd_set *writefds, fd_set *exceptfds, struct timeval *tv)    | `sys_pselect6` (mapped)             | true        |
| 39 (0x27)  | getpid    | ()                                                                                      | `sys_getpid`                        | true        |
| 56 (0x38)  | clone     | (flags, newsp, parent_tidptr, child_tidptr, tls)                                        | `sys_clone`                         | true        |
| 57 (0x39)  | fork      | () -> mapped to `clone(0, ...)`                                                          | `sys_clone` (fork emulation)        | true        |
| 59 (0x3b)  | execve    | (const char *filename, const char *const *argv, const char *const *envp)               | `sys_execve`                        | true        |
| 63 (0x3f)  | uname     | (struct utsname *buf)                                                                   | `sys_uname`                         | true        |
| 96 (0x60)  | gettimeofday | (struct timeval *tv, struct timezone *tz)                                             | `sys_gettimeofday`                  | true        |
| 232 (0xe8) | reboot    | (int magic1, int magic2, unsigned int cmd, void *arg)                                   | `sys_reboot`                        | (partial)   |
| 318 (0x13e)| getrandom | (char *buf, size_t len, unsigned int flags)                                             | `sys_getrandom`                     | true        |
| 231 (0xe7) | exit_group| (int exit_code)                                                                         | `sys_exit_group`                    | true        |
| 60 (0x3c)  | exit      | (int exit_code)                                                                         | `sys_exit`                          | true        |
| 332 (0x14c)| statx     | (int dfd, const char *pathname, unsigned flags, unsigned int mask, struct statx *buf)   | `sys_statx`                         | true        |


Notes:
- This file tracks the progress of syscall handler coverage on x86_64.
- Implemented entries call into existing `sys_*` handlers in `src/`.
- `poll`/`select` are mapped into the existing `ppoll`/`pselect6` implementations with a minimal translation.
- Many syscalls are still unimplemented; they will return ENOSYS/NotSupported.
