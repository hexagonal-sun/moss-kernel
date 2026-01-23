//! A page-backed async-aware circular kernel buffer.

use crate::{
    arch::ArchImpl,
    memory::{
        page::ClaimedPage,
        uaccess::{copy_from_user_slice, copy_to_user_slice},
    },
};
use core::{cmp::min, marker::PhantomData, ops::Deref};
use libkernel::{
    error::Result,
    memory::{PAGE_SIZE, address::UA, kbuf::KBufCore},
};
use ringbuf::storage::Storage;

pub struct PageBackedStorage<T>(ClaimedPage, PhantomData<T>);

const USER_COPY_CHUNK_SIZE: usize = 0x100;

unsafe impl<T> Storage for PageBackedStorage<T> {
    type Item = T;

    fn len(&self) -> usize {
        PAGE_SIZE / core::mem::size_of::<T>()
    }

    fn as_mut_ptr(&self) -> *mut core::mem::MaybeUninit<Self::Item> {
        self.0.as_ptr_mut() as *mut _
    }
}

#[derive(Clone)]
pub struct KBuf<T> {
    inner: KBufCore<T, PageBackedStorage<T>, ArchImpl>,
}

impl<T> KBuf<T> {
    pub fn new() -> Result<Self> {
        let pg = ClaimedPage::alloc_zeroed()?;

        Ok(Self {
            inner: KBufCore::new(PageBackedStorage(pg, PhantomData)),
        })
    }
}

// Implement Deref to forward all &self methods automatically
impl<T> Deref for KBuf<T> {
    type Target = KBufCore<T, PageBackedStorage<T>, ArchImpl>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub type KPipe = KBuf<u8>;

impl KPipe {
    /// Copies `count` bytes to the KPipe from a user-space buffer.
    ///
    /// This function will fill the kbuf as much as possible and return the
    /// number of bytes written. If the buffer is full when called, this
    /// function will block until space becomes available.
    pub async fn copy_from_user(&self, src: UA, count: usize) -> Result<usize> {
        let mut temp_buf = [0u8; USER_COPY_CHUNK_SIZE];
        let chunk_buf = &mut temp_buf[..min(count, USER_COPY_CHUNK_SIZE)];

        copy_from_user_slice(src, chunk_buf).await?;

        Ok(self.inner.push_slice(chunk_buf).await)
    }

    /// Copies `count` bytes from the KPipe to a user-space buffer.
    ///
    /// This function will drain as much of the buffer as possible and return
    /// the number of bytes written. If the buffer is empty when called, this
    /// function will block until data becomes available.
    pub async fn copy_to_user(&self, dst: UA, count: usize) -> Result<usize> {
        let mut temp_buf = [0u8; USER_COPY_CHUNK_SIZE];
        let chunk_buf = &mut temp_buf[..min(count, USER_COPY_CHUNK_SIZE)];

        let bytes_read = self.inner.pop_slice(chunk_buf).await;

        copy_to_user_slice(chunk_buf, dst).await?;

        Ok(bytes_read)
    }

    /// Moves up to `count` bytes from `source` KBuf into `self`.
    ///
    /// It performs a direct memory copy between the kernel buffers without an
    /// intermediate stack buffer. It also handles async waiting and deadlock
    /// avoidance.
    pub async fn splice_from(&self, source: &KPipe, count: usize) -> usize {
        self.inner.splice_from(&source.inner, count).await
    }
}
