//! A page-backed async-aware circular kernel buffer.

use crate::{
    CpuOps,
    sync::{
        spinlock::SpinLockIrq,
        waker_set::{WakerSet, wait_until},
    },
};
use alloc::sync::Arc;
use core::{cmp::min, future, mem::MaybeUninit, task::Poll};
use ringbuf::{
    SharedRb,
    storage::Storage,
    traits::{Consumer, Observer, Producer, SplitRef},
};

struct KBufInner<T, S: Storage<Item = T>> {
    buf: SharedRb<S>,
    read_waiters: WakerSet,
    write_waiters: WakerSet,
}

pub struct KBufCore<T, S: Storage<Item = T>, C: CpuOps> {
    inner: Arc<SpinLockIrq<KBufInner<T, S>, C>>,
}

impl<T, S: Storage<Item = T>, C: CpuOps> Clone for KBufCore<T, S, C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T, S: Storage<Item = T>, C: CpuOps> KBufCore<T, S, C> {
    pub fn new(storage: S) -> Self {
        let rb = unsafe { SharedRb::from_raw_parts(storage, 0, 0) };

        Self {
            inner: Arc::new(SpinLockIrq::new(KBufInner {
                buf: rb,
                read_waiters: WakerSet::new(),
                write_waiters: WakerSet::new(),
            })),
        }
    }

    pub fn read_ready(&self) -> impl Future<Output = ()> + use<T, S, C> {
        let lock = self.inner.clone();

        wait_until(
            lock,
            |inner| &mut inner.read_waiters,
            |inner| {
                if inner.buf.is_empty() { None } else { Some(()) }
            },
        )
    }

    pub async fn write_ready(&self) {
        wait_until(
            self.inner.clone(),
            |inner| &mut inner.write_waiters,
            |inner| if inner.buf.is_full() { None } else { Some(()) },
        )
        .await;
    }

    /// Pushes a value of type `T` into the buffer. If the buffer is full, this
    /// function will wait for a slot.
    pub async fn push(&self, mut obj: T) {
        loop {
            self.write_ready().await;

            match self.try_push(obj) {
                Ok(()) => return,
                Err(o) => obj = o,
            }
        }
    }

    pub fn try_push(&self, obj: T) -> core::result::Result<(), T> {
        let mut inner = self.inner.lock_save_irq();

        let res = inner.buf.try_push(obj);

        if res.is_ok() {
            inner.read_waiters.wake_one();
        }

        res
    }

    pub async fn pop(&self) -> T {
        loop {
            self.read_ready().await;

            if let Some(obj) = self.try_pop() {
                return obj;
            }
        }
    }

    pub fn try_pop(&self) -> Option<T> {
        let mut inner = self.inner.lock_save_irq();

        let res = inner.buf.try_pop();

        if res.is_some() {
            inner.write_waiters.wake_one();
        }

        res
    }
}

impl<T: Copy, S: Storage<Item = T>, C: CpuOps> KBufCore<T, S, C> {
    pub async fn pop_slice(&self, buf: &mut [T]) -> usize {
        wait_until(
            self.inner.clone(),
            |inner| &mut inner.read_waiters,
            |inner| {
                let size = inner.buf.pop_slice(buf);

                if size != 0 {
                    // Wake up any writers that may be waiting on the pipe.
                    inner.write_waiters.wake_one();
                    Some(size)
                } else {
                    // Sleep.
                    None
                }
            },
        )
        .await
    }

    pub fn try_pop_slice(&self, buf: &mut [T]) -> usize {
        let mut guard = self.inner.lock_save_irq();
        let size = guard.buf.pop_slice(buf);
        if size > 0 {
            guard.write_waiters.wake_one();
        }
        size
    }

    pub async fn push_slice(&self, buf: &[T]) -> usize {
        wait_until(
            self.inner.clone(),
            |inner| &mut inner.write_waiters,
            |inner| {
                let bytes_written = inner.buf.push_slice(buf);

                // If we didn't fill the buffer completely, other pending writes may be
                // able to complete.
                if !inner.buf.is_full() {
                    inner.write_waiters.wake_one();
                }

                if bytes_written > 0 {
                    // We wrote some data, wake up any blocking readers.
                    inner.read_waiters.wake_one();
                    Some(bytes_written)
                } else {
                    // Sleep.
                    None
                }
            },
        )
        .await
    }

    pub fn try_push_slice(&self, buf: &[T]) -> usize {
        let mut guard = self.inner.lock_save_irq();
        let size = guard.buf.push_slice(buf);
        if size > 0 {
            guard.read_waiters.wake_one();
        }
        size
    }

    /// Moves up to `count` objs from `source` KBuf into `self`.
    ///
    /// It performs a direct memory copy between the kernel buffers without an
    /// intermediate stack buffer. It also handles async waiting and deadlock
    /// avoidance.
    pub async fn splice_from(&self, source: &KBufCore<T, S, C>, count: usize) -> usize {
        if count == 0 {
            return 0;
        }

        // Splicing from a buffer to itself is a no-op that would instantly
        // deadlock.
        if Arc::ptr_eq(&self.inner, &source.inner) {
            return 0;
        }

        future::poll_fn(|cx| -> Poll<usize> {
            // Lock two KBufs with the lower memory address first to prevent
            // AB-BA deadlocks.
            let self_ptr = Arc::as_ptr(&self.inner);
            let source_ptr = Arc::as_ptr(&source.inner);

            let (mut self_guard, mut source_guard) = if self_ptr < source_ptr {
                (self.inner.lock_save_irq(), source.inner.lock_save_irq())
            } else {
                let source_g = source.inner.lock_save_irq();
                let self_g = self.inner.lock_save_irq();
                (self_g, source_g)
            };

            let (_, source_consumer) = source_guard.buf.split_ref();
            let (mut self_producer, _) = self_guard.buf.split_ref();

            // Determine the maximum number of bytes we can move in one go.
            let bytes_to_move = min(
                count,
                min(source_consumer.occupied_len(), self_producer.vacant_len()),
            );

            if bytes_to_move > 0 {
                // We can move data. Get the memory slices for direct copy.
                let (src_head, src_tail) = source_consumer.occupied_slices();
                let (dst_head, dst_tail) = self_producer.vacant_slices_mut();

                // Perform the copy, which may involve multiple steps if the
                // source or destination wraps around the end of the ring
                // buffer.
                let copied =
                    Self::copy_slices((src_head, src_tail), (dst_head, dst_tail), bytes_to_move);

                // Advance the read/write heads in the ring buffers.
                unsafe {
                    source_consumer.advance_read_index(copied);
                    self_producer.advance_write_index(copied);
                }

                drop(source_consumer);
                drop(self_producer);

                // Wake up anyone waiting for the opposite condition. A reader
                // might be waiting for data in `self`.
                self_guard.read_waiters.wake_one();

                // A writer might be waiting for space in `source`.
                source_guard.write_waiters.wake_one();

                Poll::Ready(copied)
            } else {
                // We can't move data. We need to wait. If source is empty, we
                // must wait for a writer on the source.
                if source_consumer.is_empty() {
                    drop(source_consumer);
                    source_guard.read_waiters.register(cx.waker());
                }

                // If destination is full, we must wait for a reader on the
                // destination.
                if self_producer.is_full() {
                    drop(self_producer);
                    self_guard.write_waiters.register(cx.waker());
                }

                Poll::Pending
            }
        })
        .await
    }

    /// Helper function to copy data between pairs of buffer slices.
    fn copy_slices(
        (src_head, src_tail): (&[MaybeUninit<T>], &[MaybeUninit<T>]),
        (dst_head, dst_tail): (&mut [MaybeUninit<T>], &mut [MaybeUninit<T>]),
        mut amount: usize,
    ) -> usize {
        let original_amount = amount;

        // Copy from src_head to dst_head
        let n1 = min(amount, min(src_head.len(), dst_head.len()));
        if n1 > 0 {
            dst_head[..n1].copy_from_slice(&src_head[..n1]);
            amount -= n1;
        }

        if amount == 0 {
            return original_amount;
        }

        // Copy from the remainder of src_head to dst_tail
        let src_after_head = &src_head[n1..];
        let n2 = min(amount, min(src_after_head.len(), dst_tail.len()));
        if n2 > 0 {
            dst_tail[..n2].copy_from_slice(&src_after_head[..n2]);
            amount -= n2;
        }

        if amount == 0 {
            return original_amount;
        }

        // Copy from src_tail to the remainder of dst_head
        let dst_after_head = &mut dst_head[n1..];
        let n3 = min(amount, min(src_tail.len(), dst_after_head.len()));
        if n3 > 0 {
            dst_after_head[..n3].copy_from_slice(&src_tail[..n3]);
            amount -= n3;
        }

        original_amount - amount
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{memory::PAGE_SIZE, test::MockCpuOps};
    use ringbuf::storage::Heap;
    use tokio::time::{Duration, timeout};

    // Helper to create a KBufCore backed by a dynamically allocated buffer for testing.
    fn make_kbuf(size: usize) -> KBufCore<u8, Heap<u8>, MockCpuOps> {
        let storage = Heap::new(size);
        KBufCore::new(storage)
    }

    #[tokio::test]
    async fn simple_read_write() {
        let kbuf = make_kbuf(16);
        let in_buf = [1, 2, 3];
        let mut out_buf = [0; 3];

        let written = kbuf.push_slice(&in_buf).await;
        assert_eq!(written, 3);

        // read_ready should complete immediately since there's data.
        kbuf.read_ready().await;

        let read = kbuf.pop_slice(&mut out_buf).await;
        assert_eq!(read, 3);

        assert_eq!(in_buf, out_buf);
    }

    #[tokio::test]
    async fn read_blocks_when_empty() {
        let kbuf = make_kbuf(16);
        let mut out_buf = [0; 3];

        // We expect the read to time out because the buffer is empty and it should block.
        let result = timeout(Duration::from_millis(10), kbuf.pop_slice(&mut out_buf)).await;
        assert!(result.is_err(), "Read should have blocked and timed out");
    }

    #[tokio::test]
    async fn write_blocks_when_full() {
        let kbuf = make_kbuf(16);
        let big_buf = [0; 16];
        let small_buf = [1];

        // Fill the buffer completely.
        let written = kbuf.push_slice(&big_buf).await;
        assert_eq!(written, 16);
        assert!(kbuf.inner.lock_save_irq().buf.is_full());

        // The next write should block.
        let result = timeout(Duration::from_millis(10), kbuf.push_slice(&small_buf)).await;
        assert!(result.is_err(), "Write should have blocked and timed out");
    }

    #[tokio::test]
    async fn write_wakes_reader() {
        let kbuf = make_kbuf(16);
        let kbuf_clone = kbuf.clone();

        // Spawn a task that will block reading from the empty buffer.
        let reader_task = tokio::spawn(async move {
            assert_eq!(kbuf_clone.pop().await, 10);
            assert_eq!(kbuf_clone.pop().await, 20);
            assert_eq!(kbuf_clone.pop().await, 30);
            assert_eq!(kbuf_clone.pop().await, 40);
        });

        // Give the reader a moment to start and block.
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Now, write to the buffer. This should wake up the reader.
        kbuf.push(10).await;

        // Give the reader a moment to block again.
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Now, write to the buffer. This should wake up the reader.
        kbuf.push(20).await;

        assert!(kbuf.try_push(30).is_ok());
        assert!(kbuf.try_push(40).is_ok());

        reader_task.await.unwrap();
    }

    #[tokio::test]
    async fn write_slice_wakes_reader() {
        let kbuf = make_kbuf(16);
        let kbuf_clone = kbuf.clone();
        let in_buf = [10, 20, 30];
        let mut out_buf = [0; 3];

        // Spawn a task that will block reading from the empty buffer.
        let reader_task = tokio::spawn(async move {
            kbuf_clone.pop_slice(&mut out_buf).await;
            out_buf // Return the buffer to check the result
        });

        // Give the reader a moment to start and block.
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Now, write to the buffer. This should wake up the reader.
        kbuf.push_slice(&in_buf).await;

        // The reader task should now complete.
        let result_buf = reader_task.await.unwrap();
        assert_eq!(result_buf, in_buf);
    }

    #[tokio::test]
    async fn read_wakes_writer() {
        let kbuf = make_kbuf(8);
        let kbuf_clone = kbuf.clone();
        let mut buf = [0; 8];

        // Fill the buffer.
        kbuf.push_slice(&[1; 8]).await;
        assert!(kbuf.inner.lock_save_irq().buf.is_full());

        // Spawn a task that will block reading from the empty buffer.
        let reader_task = tokio::spawn(async move {
            kbuf_clone.push(10).await;
            kbuf_clone.push(20).await;
            kbuf_clone.push(30).await;
            kbuf_clone.push(40).await;
        });

        // Give the writer a moment to start and block.
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Now, read from the buffer. This should wake up the writer.
        assert_eq!(kbuf.pop().await, 1);

        // Give the writer a moment to block again.
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Now, write to the buffer. This should wake up the reader.
        assert_eq!(kbuf.pop().await, 1);

        assert!(kbuf.try_pop().is_some());
        assert!(kbuf.try_pop().is_some());

        reader_task.await.unwrap();

        kbuf.pop_slice(&mut buf).await;
        assert_eq!(&buf, &[1, 1, 1, 1, 10, 20, 30, 40]);
    }

    #[tokio::test]
    async fn read_slice_wakes_writer() {
        let kbuf = make_kbuf(8);
        let kbuf_clone = kbuf.clone();
        let mut out_buf = [0; 4];

        // Fill the buffer.
        kbuf.push_slice(&[1; 8]).await;
        assert!(kbuf.inner.lock_save_irq().buf.is_full());

        // Spawn a task that will block trying to write to the full buffer.
        let writer_task = tokio::spawn(async move {
            let written = kbuf_clone.push_slice(&[2; 4]).await;
            assert_eq!(written, 4);
        });

        // Give the writer a moment to start and block.
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Now, read from the buffer. This should make space and wake the writer.
        let read = kbuf.pop_slice(&mut out_buf).await;
        assert_eq!(read, 4);

        // The writer task should now complete.
        writer_task.await.unwrap();

        // The buffer should contain the remaining 4 ones and the 4 twos from the writer.
        kbuf.pop_slice(&mut out_buf).await;
        assert_eq!(out_buf, [1, 1, 1, 1]);
        kbuf.pop_slice(&mut out_buf).await;
        assert_eq!(out_buf, [2, 2, 2, 2]);
    }

    #[tokio::test]
    async fn concurrent_producer_consumer() {
        const ITERATIONS: usize = 5000;
        let kbuf = make_kbuf(PAGE_SIZE);
        let producer_kbuf = kbuf.clone();
        let consumer_kbuf = kbuf.clone();

        let producer = tokio::spawn(async move {
            for i in 0..ITERATIONS {
                let byte = (i % 256) as u8;
                producer_kbuf.push_slice(&[byte]).await;
            }
        });

        let consumer = tokio::spawn(async move {
            let mut received = 0;
            while received < ITERATIONS {
                let mut buf = [0; 1];
                let count = consumer_kbuf.pop_slice(&mut buf).await;
                if count > 0 {
                    let expected_byte = (received % 256) as u8;
                    assert_eq!(buf[0], expected_byte);
                    received += 1;
                }
            }
        });

        let (prod_res, cons_res) = tokio::join!(producer, consumer);
        prod_res.unwrap();
        cons_res.unwrap();
    }

    // --- Splice Tests ---

    #[tokio::test]
    async fn splice_simple_transfer() {
        let src = make_kbuf(PAGE_SIZE);
        let dest = make_kbuf(PAGE_SIZE);
        let data: Vec<u8> = (0..100).collect();
        let mut out_buf = vec![0; 100];

        src.push_slice(&data).await;
        assert_eq!(src.inner.lock_save_irq().buf.occupied_len(), 100);
        assert_eq!(dest.inner.lock_save_irq().buf.occupied_len(), 0);

        let spliced = dest.splice_from(&src, 100).await;
        assert_eq!(spliced, 100);

        assert_eq!(src.inner.lock_save_irq().buf.occupied_len(), 0);
        assert_eq!(dest.inner.lock_save_irq().buf.occupied_len(), 100);

        dest.pop_slice(&mut out_buf).await;
        assert_eq!(out_buf, data);
    }

    #[tokio::test]
    async fn splice_limited_by_count() {
        let src = make_kbuf(PAGE_SIZE);
        let dest = make_kbuf(PAGE_SIZE);
        let data: Vec<u8> = (0..100).collect();

        src.push_slice(&data).await;
        let spliced = dest.splice_from(&src, 50).await;
        assert_eq!(spliced, 50);

        assert_eq!(src.inner.lock_save_irq().buf.occupied_len(), 50);
        assert_eq!(dest.inner.lock_save_irq().buf.occupied_len(), 50);

        let mut out_buf = vec![0; 50];
        dest.pop_slice(&mut out_buf).await;
        assert_eq!(out_buf, &data[0..50]);
    }

    #[tokio::test]
    async fn splice_limited_by_dest_capacity() {
        let src = make_kbuf(200);
        let dest = make_kbuf(100); // Smaller destination
        let data: Vec<u8> = (0..200).collect();

        src.push_slice(&data).await;
        // Splice more than dest has capacity for.
        let spliced = dest.splice_from(&src, 200).await;
        assert_eq!(spliced, 100);

        assert_eq!(src.inner.lock_save_irq().buf.occupied_len(), 100);
        assert_eq!(dest.inner.lock_save_irq().buf.occupied_len(), 100);
        assert!(dest.inner.lock_save_irq().buf.is_full());
    }

    #[tokio::test]
    async fn splice_blocks_on_full_dest_and_wakes() {
        let src = make_kbuf(100);
        let dest = make_kbuf(50);
        let dest_clone = dest.clone();

        src.push_slice(&(0..100).collect::<Vec<u8>>()).await;
        dest.push_slice(&(0..50).collect::<Vec<u8>>()).await;
        assert!(dest.inner.lock_save_irq().buf.is_full());

        // This splice will block because dest is full.
        let splice_task = tokio::spawn(async move {
            // It will only be able to splice 0 bytes initially, then block,
            // then it will splice 25 bytes once space is made.
            let spliced_bytes = dest_clone.splice_from(&src, 100).await;
            assert_eq!(spliced_bytes, 25);
        });

        tokio::time::sleep(Duration::from_millis(5)).await;

        // Make space in the destination buffer.
        let mut read_buf = [0; 25];
        let read_bytes = dest.pop_slice(&mut read_buf).await;
        assert_eq!(read_bytes, 25);

        // The splice task should now unblock and complete.
        timeout(Duration::from_millis(50), splice_task)
            .await
            .expect("Splice task should have completed")
            .unwrap();
    }

    #[tokio::test]
    async fn splice_to_self_is_noop_and_doesnt_deadlock() {
        let kbuf = make_kbuf(100);
        let data = [1, 2, 3, 4, 5];
        kbuf.push_slice(&data).await;

        // This should return immediately with 0 and not deadlock.
        let spliced = kbuf.splice_from(&kbuf, 50).await;
        assert_eq!(spliced, 0);

        // Verify data is untouched.
        let mut out_buf = [0; 5];
        kbuf.pop_slice(&mut out_buf).await;
        assert_eq!(out_buf, data);
    }

    #[tokio::test]
    async fn splice_into_partially_full_buffer() {
        let src = make_kbuf(PAGE_SIZE);
        let dest = make_kbuf(PAGE_SIZE);

        // Setup: `dest` already has 20 bytes of data.
        let old_data = vec![255; 20];
        dest.push_slice(&old_data).await;

        // `src` has 50 bytes of new data to be spliced.
        let splice_data: Vec<u8> = (0..50).collect();
        src.push_slice(&splice_data).await;

        // Action: Splice the 50 bytes from `src` into `dest`.
        // There is enough room, so the full amount should be spliced.
        let spliced = dest.splice_from(&src, 50).await;

        // Assertions:
        assert_eq!(spliced, 50, "Should have spliced the requested 50 bytes");

        // `src` should now be empty.
        assert!(src.inner.lock_save_irq().buf.is_empty());

        // `dest` should contain the old data followed by the new data.
        assert_eq!(
            dest.inner.lock_save_irq().buf.occupied_len(),
            old_data.len() + splice_data.len()
        );

        let mut final_dest_data = vec![0; 70];
        dest.pop_slice(&mut final_dest_data).await;

        // Check that the original data is at the start.
        assert_eq!(&final_dest_data[0..20], &old_data[..]);
        // Check that the spliced data comes after it.
        assert_eq!(&final_dest_data[20..70], &splice_data[..]);
    }

    #[tokio::test]
    async fn splice_into_almost_full_buffer_is_limited() {
        let src = make_kbuf(PAGE_SIZE);
        // Use a smaller destination buffer to make capacity relevant.
        let dest = make_kbuf(100);

        // `dest` has 80 bytes, leaving only 20 bytes of free space.
        let old_data = vec![255; 80];
        dest.push_slice(&old_data).await;

        // `src` has 50 bytes, more than the available space in `dest`.
        let splice_data: Vec<u8> = (0..50).collect();
        src.push_slice(&splice_data).await;

        // Attempt to splice 50 bytes. This should be limited by `dest`'s
        // capacity.
        let spliced = dest.splice_from(&src, 50).await;

        assert_eq!(
            spliced, 20,
            "Splice should be limited to the 20 bytes of available space"
        );

        // `dest` should now be completely full.
        assert!(dest.inner.lock_save_irq().buf.is_full());

        // `src` should have the remaining 30 bytes that couldn't be spliced.
        assert_eq!(src.inner.lock_save_irq().buf.occupied_len(), 30);

        // Verify the contents of `dest`.
        let mut final_dest_data = vec![0; 100];
        dest.pop_slice(&mut final_dest_data).await;
        assert_eq!(&final_dest_data[0..80], &old_data[..]);
        assert_eq!(&final_dest_data[80..100], &splice_data[0..20]); // Only the first 20 bytes

        // Verify the remaining contents of `src`.
        let mut remaining_src_data = vec![0; 30];
        src.pop_slice(&mut remaining_src_data).await;
        assert_eq!(&remaining_src_data[..], &splice_data[20..50]); // The last 30 bytes
    }
}
