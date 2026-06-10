# futex2 in moss

## What futex2 is

A futex ("fast userspace mutex") is the kernel primitive userspace locking is
built on. A lock is just a 32-bit word in user memory; the fast path (taking an
uncontended lock) never enters the kernel. Only on contention does a thread ask
the kernel to put it to sleep until another thread wakes it.

futex2 is the modern generation of this interface, added to Linux in stages
from 5.16 onward. Instead of one multiplexed `futex` syscall with an `op`
argument, it provides separate syscalls:

- **futex_wait** — sleep on one futex word, as long as it still holds an
  expected value. Returns once woken.
- **futex_wake** — wake up to a given number of threads sleeping on a word.
- **futex_waitv** — sleep on *several* futex words at once (up to 128) and
  return the index of whichever one got woken. This is the headline feature;
  it was driven by Wine/Proton, which needs Windows' "wait for any of these
  objects" semantics to run games efficiently.
- **futex_requeue** — wake some waiters of one word and silently move the rest
  onto a different word, without waking them. Used by condition-variable
  implementations to avoid thundering herds: one broadcast moves everyone onto
  the mutex's wait queue instead of waking them all to fight over the lock.

All waits take a wake *mask*: a waiter is only woken by a wake whose mask
shares at least one bit with its own. Timeouts are *absolute* deadlines against
a chosen clock (monotonic or realtime), not relative durations.

moss implements all four on aarch64 (syscall numbers 449, 454, 455, 456), with
Linux-compatible argument validation and error codes.

## How futex2 differs from legacy futex

- Separate syscalls instead of one `op`-multiplexed entry point.
- Sized futexes: each call carries a flag stating the width of the futex word.
  Like Linux today, moss accepts only the 32-bit size.
- Wake masks are first-class on every wait/wake, not a special bitset op
  bolted on (legacy `FUTEX_WAIT_BITSET`/`FUTEX_WAKE_BITSET`).
- Timeouts are always absolute, with an explicit clock choice per call.
- Multi-wait (`futex_waitv`) exists only in futex2.
- futex2's requeue performs no value comparison, unlike legacy
  `FUTEX_CMP_REQUEUE`; it trusts userspace and simply moves waiters.
- Stricter validation: unaligned futex addresses, unknown flags, reserved
  fields and oversized values are rejected outright.

The legacy `futex` syscall remains fully supported; nothing changed in its
userspace-visible behaviour.

## How moss implements it

Both generations share one wait/wake core; the syscalls differ only in how
they decode arguments.

Every futex word is identified by a key — for process-private futexes the
process id plus the virtual address, for shared ones the physical frame plus
offset, so different mappings of the same memory still meet on the same futex.
A global table maps each key to its wait queue.

Each sleeping thread owns a small shared record (its "waiter cell") holding
its wake mask, whether it has been woken, and *which queue currently holds
it*. That last part is what makes requeueing sound: requeue moves entries
between queues while their owners sleep, and a waiter that later needs to
cancel (timeout, signal) consults its own cell to find where it currently
lives rather than assuming it is still where it went to sleep. Because lock
ordering forbids going from a cell to its queue directly, cancellation
snapshots the location, locks the queue, then re-checks the location and
retries if a concurrent requeue moved it in the meantime.

Waiting — single or multi — is one routine: for each requested futex it locks
the queue, re-reads the user word, and only enqueues if the value still
matches, so a wake cannot slip between the check and the sleep. If any value
mismatches, everything already enqueued is unwound; if one of those waiters
was woken during the unwind, that wake is honoured rather than lost. Legacy
wait and futex2's wait are this routine with a list of one; waitv passes the
whole list and reports the index that fired. Timeouts race the wait against a
timer, with a final "did a wake land anyway" check before reporting a timeout.

Waking finds the queue, removes up to the requested number of waiters whose
masks overlap, marks their cells woken, and only then — after dropping the
queue lock — actually wakes the threads. Requeueing locks both queues (in a
fixed order, so two concurrent requeues cannot deadlock), wakes from the
first, and transplants the remainder onto the second, updating each moved
cell's location.

## Remaining issues

- **Realtime deadlines do not track clock changes.** An absolute
  `CLOCK_REALTIME` deadline is converted into a relative sleep when the
  syscall starts. If the wall clock is stepped while a thread waits, the
  deadline does not move with it, where Linux would honour the new clock.
  Fixing this needs realtime-aware timers in the timer subsystem. The legacy
  bitset-wait path has always had the same simplification.
- **A wake can be lost when a signal interrupts a waiter.** If a wake and a
  signal arrive at almost the same moment, the wake may be consumed (the waker
  is told it woke someone) while the waiter returns "interrupted" instead of
  "woken". Linux closes this window with syscall-restart machinery, which moss
  does not yet have. Inherited from the legacy implementation. The equivalent
  wake-versus-timeout race *is* handled.
- **The futex table never shrinks.** A queue entry is created for every futex
  address ever waited on and is kept after the queue drains. Long-running
  systems touching many distinct addresses leak table entries slowly.
  Pre-existing behaviour; pruning empty queues on the last waiter's exit is
  the obvious fix.
- **Host unit tests skip the sync primitives by default.** The unit-test
  recipe builds the kernel library without optional features, so the tests
  for the synchronisation primitives (including the waker-set operations this
  work added) only run when the full feature set is enabled explicitly.
