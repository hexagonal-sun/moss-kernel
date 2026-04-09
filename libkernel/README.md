# libkernel

Architecture-independent kernel building blocks for operating systems.

`libkernel` provides the core abstractions that a kernel needs to manage memory,
processes, filesystems, and synchronisation, agnostic of the an underlying CPU
architecture. It is designed to run in a `no_std` environment and uses feature
gates to keep the dependency footprint minimal.

## Feature gates

| Feature   | Enables                                               | Implies          |
|-----------|-------------------------------------------------------|------------------|
| `sync`    | Synchronisation primitives (spinlock, mutex, rwlock…) | —                |
| `alloc`   | Memory allocators (buddy, slab) and collection types  | `sync`           |
| `paging`  | Page tables, address-space management, PTE helpers    | `alloc`          |
| `proc`    | Process identity types (UID/GID, capabilities)        | —                |
| `fs`      | VFS traits, path manipulation, block I/O              | `proc`, `sync`   |
| `proc_vm` | Process virtual-memory management (mmap, brk, CoW)    | `paging`, `fs`   |
| `kbuf`    | Async-aware circular kernel buffers                   | `sync`           |
| `all`     | Everything above                                      | all of the above |

## Quick start

Add `libkernel` to your `Cargo.toml` with only the features you need:

```toml
[dependencies]
libkernel = { version = "0.1", features = ["sync", "proc"] }
```

## The `CpuOps` trait

Most synchronisation and memory primitives are generic over a
[`CpuOps`](https://docs.rs/libkernel/latest/libkernel/trait.CpuOps.html)
implementation. This trait abstracts the handful of arch-specific operations
(core ID, interrupt masking, halt) that the portable code depends on.

## License

Licensed under the MIT license. See [LICENSE](../LICENSE) for details.
