# futex2 review findings

Review of branch `futex2` vs `master`. High-effort multi-agent pass (5 finders, verified).

Ranked most-severe first. Confirmed = traced in code; Plausible = real & reachable, lower confidence or known/benign.

## Correctness — confirmed

### 1. Lost wake on signal interrupt *(top bug)*
`src/process/threading/futex/wait.rs:64`

On signal, the wait future is dropped and `WaitGuard::drop` calls `cell.unregister()` but discards its bool. If `futex_wake` already consumed the wake (counted it via `mark_woken`), the waiter returns `EINTR` and the wake is lost — userspace condvar/mutex can hang. The timeout branch recovers this race via `finish().ok_or(TimedOut)`; the interrupt path has no equivalent. Affects both futex1 and futex2 (shared core).

**Fix:** on interrupt, inspect `unregister()` return — if a wake was consumed, return success (mirror timeout `finish()` recovery) or re-wake another waiter.

*Acknowledged in etc/futex2.md but still live.*

### 2. requeue-to-self silently drops nr_requeue
`src/process/threading/futex/mod.rs:84`

`requeue_key` shortcuts `key1==key2` → `wake_key(nr_wake, key1, u64::MAX)`, ignoring `nr_requeue`. Linux returns `EINVAL` for requeue onto the same address. Shared futexes: two VAs mapping the same frame collapse to one key and hit this unexpectedly.

### 3. Zero bitset not rejected on legacy path
`src/process/threading/futex/mod.rs:204`

`FUTEX_WAIT_BITSET` / `FUTEX_WAKE_BITSET` forward `val3` as the mask with no zero check. `mask==0` → `cell.mask & mask` always 0 → waiter permanently unwakeable. Linux returns `EINVAL`. The futex2 paths already guard `mask==0`; the legacy path doesn't. Inconsistent.

### 4. wake computes key before nr<=0 check
`src/process/threading/futex/futex2.rs:166`

`make_key` (translates the address, can fault for a shared futex) runs before the `nr<=0` early return. `futex_wake(nr=0)` on an unmapped shared address returns `EFAULT` instead of `Ok(0)`. Move the `nr` check above `make_key`.

## Correctness — plausible

### 5. Duplicate uaddr in waitv inflates wake count
`src/process/threading/futex/futex2.rs:200`

Same `uaddr` at two `futex_waitv` indices → two cells on one queue for one task. `futex_wake(nr>=2)` matches both, returns `woke=2` though only one logical waiter exists. No panic, returned index valid, but count is wrong. No dedup guard. (requeue has the analogous issue via `take_first`.)

### 6. abs_timeout realtime semantics
`src/process/threading/futex/futex2.rs:438`

Absolute `CLOCK_REALTIME` deadline converted to a relative sleep once at syscall entry. If `set_date()` was never called, `date()` falls back to `uptime()` → deadline saturates to an effectively infinite timeout. Also a concurrent `clock_settime` doesn't retarget an in-progress wait. Documented limitation in etc/futex2.md, but user-visible.

## Quality

### 7. do_futex_wait duplicates futex2 wait core
`src/process/threading/futex/mod.rs:136`

Both `do_futex_wait` and `sys_futex_wait` build a single `ParsedWaiter`, call `futex_wait_multi(slice::from_ref(&waiter), timeout)`, and `.map(|_| 0)`. Extract one shared single-waiter helper — also lets the #1 fix land in one place instead of two.

### 8. Box::pin(sleep) heap-allocs the hot path
`src/process/threading/futex/wait.rs:147`

The timeout branch does `Box::pin(sleep(dur))` + a fresh `select_biased` on every timed wait — a heap alloc/free on every contended timed lock acquire. Pin on the stack with `core::pin::pin!` / `pin_mut!`.

### 9. Sentinel FutexKey in requeue
`src/process/threading/futex/futex2.rs:244`

Seeds the keys array with a dummy `FutexKey::Private { pid: 0, addr: 0 }` and overwrites it in the loop. If a future refactor leaves an entry unwritten, the bogus key leaks into `requeue_key`. Collect the two keys directly (map entries into the array) so no sentinel exists.

### 10. u64 mask is unused width
`src/process/threading/futex/mod.rs:49`

Wake mask widened to `u64` everywhere (`WaiterCell.mask`, `ParsedWaiter.mask`, `wake_key`, `requeue_key`) but no path carries >32 bits — `check_flags` rejects masks > `u32::MAX` and every caller casts a u32. Unused width + casts at every call site. Should be `u32` (matches the only supported `FUTEX2_SIZE_U32`).

---

## Linux reference

futex2 = the `futex_waitv` syscall. Real kernel source:

- `kernel/futex/` — https://github.com/torvalds/linux/tree/master/kernel/futex
  - `core.c` — hash buckets, queue, wait/wake core
  - `waitwake.c` — `futex_wait`, `futex_wake`, **`futex_wait_multiple`** (the multi-wait core; our `futex_wait_multi` analog)
  - `syscalls.c` — `sys_futex`, **`sys_futex_waitv`**, `futex_parse_waitv`
  - `requeue.c`, `pi.c` — requeue + priority inheritance (out of scope here)
  - `futex.h` — `futex_q`, `futex_hash_bucket`
- UAPI: `include/uapi/linux/futex.h` — flags, `struct futex_waitv`
- Docs: https://docs.kernel.org/userspace-api/futex2.html
- man: `man 2 futex_waitv`, `man 2 futex`
