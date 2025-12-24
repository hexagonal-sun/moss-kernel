use crate::{
    CpuOps,
    error::{FsError, KernelError, Result},
    fs::{
        DirStream, Dirent, FileType, Filesystem, Inode, InodeId,
        attr::{FileAttr, FilePermissions},
    },
    memory::{
        PAGE_SIZE,
        address::{AddressTranslator, VA},
        page::ClaimedPage,
        page_alloc::PageAllocGetter,
    },
    sync::spinlock::SpinLockIrq,
};
use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use async_trait::async_trait;
use core::{
    cmp::min,
    marker::PhantomData,
    mem::size_of,
    sync::atomic::{AtomicU64, Ordering},
};

const BLOCK_SZ: usize = PAGE_SIZE;

// Calculate max size based on how many pointers fit in one page (the indirect
// block)
const MAX_SZ: usize = BLOCK_SZ * (PAGE_SIZE / size_of::<*mut u8>());

struct TmpFsRegInner<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    indirect_block: ClaimedPage<C, G, T>,
    size: usize,
    allocated_blocks: usize,
}

impl<C, G, T> TmpFsRegInner<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    fn block_slot_ptr(&mut self, block_idx: usize) -> *mut *mut u8 {
        debug_assert!(block_idx < PAGE_SIZE / size_of::<*mut u8>());
        unsafe {
            self.indirect_block
                .as_ptr_mut()
                .cast::<*mut u8>()
                .add(block_idx)
        }
    }

    fn block_ptr_mut(&mut self, block_idx: usize) -> *mut u8 {
        unsafe { *self.block_slot_ptr(block_idx) }
    }

    fn try_alloc_block(&mut self, block_idx: usize) -> Result<*mut u8> {
        // Ensure no discontinuity. If we write to block 5, blocks 0-4 must exist.
        // We iterate up to and including the target block_idx.
        for i in self.allocated_blocks..=block_idx {
            let new_page = ClaimedPage::<C, G, T>::alloc_zeroed()?;

            unsafe {
                *self.block_slot_ptr(i) = new_page.as_ptr_mut();
            }

            new_page.leak();
            self.allocated_blocks += 1;
        }

        Ok(self.block_ptr_mut(block_idx))
    }
}

impl<C, G, T> Drop for TmpFsRegInner<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    fn drop(&mut self) {
        for i in 0..self.allocated_blocks {
            let ptr = unsafe { *self.block_slot_ptr(i) };
            if !ptr.is_null() {
                // SAFETY: This pointer was obtained from ClaimedPage::leak() in
                // the block allocation code.
                unsafe {
                    ClaimedPage::<C, G, T>::from_pfn(
                        VA::from_ptr_mut(ptr.cast()).to_pa::<T>().to_pfn(),
                    )
                };

                // Drop happens here, releasing memory.
            }
        }
    }
}

struct TmpFsReg<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    id: InodeId,
    attr: SpinLockIrq<FileAttr, C>,
    inner: SpinLockIrq<TmpFsRegInner<C, G, T>, C>,
}

impl<C, G, T> TmpFsReg<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    fn new(id: InodeId) -> Result<Self> {
        Ok(Self {
            id,
            attr: SpinLockIrq::new(FileAttr {
                file_type: FileType::File,
                size: 0,
                nlinks: 1,
                ..Default::default()
            }),
            inner: SpinLockIrq::new(TmpFsRegInner {
                indirect_block: ClaimedPage::<C, G, T>::alloc_zeroed()?,
                size: 0,
                allocated_blocks: 0,
            }),
        })
    }

    fn offset_to_block_locus(offset: usize) -> (usize, usize) {
        (offset / BLOCK_SZ, offset % BLOCK_SZ)
    }
}

#[async_trait]
impl<C, G, T> Inode for TmpFsReg<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    fn id(&self) -> InodeId {
        self.id
    }

    async fn read_at(&self, mut offset: u64, buf: &mut [u8]) -> Result<usize> {
        let mut inner = self.inner.lock_save_irq();

        if offset as usize >= inner.size {
            return Ok(0);
        }

        let mut bytes_to_read = min(inner.size - offset as usize, buf.len());
        let mut buf_ptr = buf.as_mut_ptr();
        let mut total_read = 0;

        while bytes_to_read > 0 {
            let (blk_idx, blk_offset) = Self::offset_to_block_locus(offset as _);

            // Check if this block is actually allocated (sparse file protection)
            // though try_alloc_block enforces continuity, so this check is sanity.
            if blk_idx >= inner.allocated_blocks {
                break;
            }

            let bytes_in_block = BLOCK_SZ - blk_offset;
            let chunk_len = min(bytes_to_read, bytes_in_block);

            unsafe {
                let src = inner.block_ptr_mut(blk_idx).add(blk_offset);
                src.copy_to_nonoverlapping(buf_ptr, chunk_len);
                buf_ptr = buf_ptr.add(chunk_len);
            };

            offset += chunk_len as u64;
            total_read += chunk_len;
            bytes_to_read -= chunk_len;
        }

        Ok(total_read)
    }

    async fn write_at(&self, mut offset: u64, buf: &[u8]) -> Result<usize> {
        if offset as usize >= MAX_SZ {
            return Err(FsError::OutOfBounds.into());
        }

        let mut inner = self.inner.lock_save_irq();

        // Calculate how much we can write without exceeding MAX_SZ
        let available_space = MAX_SZ.saturating_sub(offset as usize);
        let mut bytes_to_write = min(buf.len(), available_space);

        if bytes_to_write == 0 {
            return Ok(0);
        }

        let mut buf_ptr = buf.as_ptr();
        let mut total_written = 0;

        while bytes_to_write > 0 {
            let (blk_idx, blk_offset) = Self::offset_to_block_locus(offset as _);

            // Ensure the block exists
            let block_ptr = inner.try_alloc_block(blk_idx)?;

            let bytes_in_block = BLOCK_SZ - blk_offset;
            let chunk_len = min(bytes_to_write, bytes_in_block);

            unsafe {
                let dst = block_ptr.add(blk_offset);
                dst.copy_from_nonoverlapping(buf_ptr, chunk_len);
                buf_ptr = buf_ptr.add(chunk_len);
            }

            offset += chunk_len as u64;
            total_written += chunk_len;
            bytes_to_write -= chunk_len;
        }

        // Update file size if we extended it
        if offset as usize > inner.size {
            inner.size = offset as usize;
        }
        self.attr.lock_save_irq().size = inner.size as _;

        Ok(total_written)
    }

    async fn truncate(&self, size: u64) -> Result<()> {
        let mut inner = self.inner.lock_save_irq();
        let new_size = size as usize;

        if new_size > MAX_SZ {
            return Err(FsError::OutOfBounds.into());
        }

        // Handle Expansion
        if new_size > inner.size {
            // We just update the size. The holes are implicitly zeroed by
            // read_at logic, and write_at will fill them with zeroed pages when
            // touched.
            inner.size = new_size;
            self.attr.lock_save_irq().size = size;
            return Ok(());
        }

        // 2. Handle Shrinking
        if new_size < inner.size {
            // Calculate number of blocks required for the new size.
            let new_blk_count = new_size.div_ceil(BLOCK_SZ);

            // Free the excess blocks from the end
            while inner.allocated_blocks > new_blk_count {
                let release_idx = inner.allocated_blocks - 1;

                unsafe {
                    let ptr_slot = inner.block_slot_ptr(release_idx);
                    let ptr = *ptr_slot;

                    if !ptr.is_null() {
                        // Reconstruct the ClaimedPage to drop it (returning frame to allocator)
                        let page = ClaimedPage::<C, G, T>::from_pfn(
                            VA::from_ptr_mut(ptr.cast()).to_pa::<T>().to_pfn(),
                        );

                        drop(page);

                        // Null the slot to prevent double-free in Drop
                        *ptr_slot = core::ptr::null_mut();
                    }
                }

                inner.allocated_blocks -= 1;
            }

            // Zero out trailing data in the last retained page. This is POSIX
            // behavior: bytes past the new EOF must appear as zero if we extend
            // the file later.
            if new_blk_count > 0 {
                let last_blk_idx = new_blk_count - 1;
                let offset_in_block = new_size % BLOCK_SZ;

                if offset_in_block > 0 {
                    let ptr = inner.block_ptr_mut(last_blk_idx);
                    unsafe {
                        let tail_ptr = ptr.add(offset_in_block);
                        let tail_len = BLOCK_SZ - offset_in_block;
                        tail_ptr.write_bytes(0, tail_len);
                    }
                }
            }

            inner.size = new_size;
        }
        self.attr.lock_save_irq().size = inner.size as _;

        Ok(())
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attr.lock_save_irq().clone())
    }

    async fn setattr(&self, attr: FileAttr) -> Result<()> {
        self.inner.lock_save_irq().size = attr.size as _;
        *self.attr.lock_save_irq() = attr;
        Ok(())
    }
}

struct TmpFsDirEnt {
    name: String,
    id: InodeId,
    kind: FileType,
    inode: Arc<dyn Inode>,
}

struct TmpFsDirInode<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    entries: SpinLockIrq<Vec<TmpFsDirEnt>, C>,
    attrs: SpinLockIrq<FileAttr, C>,
    id: u64,
    fs: Weak<TmpFs<C, G, T>>,
    this: Weak<Self>,
}

struct TmpFsDirReader<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    inode: Arc<TmpFsDirInode<C, G, T>>,
    offset: usize,
}

#[async_trait]
impl<C, G, T> DirStream for TmpFsDirReader<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    async fn next_entry(&mut self) -> Result<Option<Dirent>> {
        let guard = self.inode.entries.lock_save_irq();
        if let Some(entry) = guard.get(self.offset) {
            self.offset += 1;

            let dent = Some(Dirent {
                id: entry.id,
                name: entry.name.clone(),
                file_type: entry.kind,
                offset: self.offset as _,
            });

            Ok(dent)
        } else {
            Ok(None)
        }
    }
}

#[async_trait]
impl<C, G, T> Inode for TmpFsDirInode<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    fn id(&self) -> crate::fs::InodeId {
        InodeId::from_fsid_and_inodeid(self.fs.upgrade().unwrap().id(), self.id)
    }

    async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
        self.entries
            .lock_save_irq()
            .iter()
            .find(|x| x.name == name)
            .map(|x| x.inode.clone())
            .ok_or(FsError::NotFound.into())
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attrs.lock_save_irq().clone())
    }

    async fn setattr(&self, attr: FileAttr) -> Result<()> {
        *self.attrs.lock_save_irq() = attr;
        Ok(())
    }

    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        Ok(Box::new(TmpFsDirReader {
            inode: self.this.upgrade().unwrap(),
            offset: start_offset as _,
        }))
    }

    async fn create(
        &self,
        name: &str,
        file_type: FileType,
        mode: FilePermissions,
    ) -> Result<Arc<dyn Inode>> {
        let mut entries = self.entries.lock_save_irq();

        if entries.iter().any(|e| e.name == name) {
            return Err(FsError::AlreadyExists.into());
        }

        let fs = self.fs.upgrade().ok_or(FsError::InvalidFs)?;
        let new_id = fs.alloc_inode_id();
        let inode_id = InodeId::from_fsid_and_inodeid(fs.id(), new_id);

        let inode: Arc<dyn Inode> = match file_type {
            FileType::File => Arc::new(TmpFsReg::<C, G, T>::new(inode_id)?),
            FileType::Directory => TmpFsDirInode::<C, G, T>::new(new_id, self.fs.clone(), mode),
            _ => return Err(KernelError::NotSupported),
        };

        entries.push(TmpFsDirEnt {
            name: name.to_string(),
            id: inode_id,
            kind: file_type,
            inode: inode.clone(),
        });

        Ok(inode)
    }

    async fn unlink(&self, name: &str) -> Result<()> {
        let mut entries = self.entries.lock_save_irq();
        let index = entries.iter().position(|e| e.name == name);

        if let Some(idx) = index {
            entries.remove(idx);
            Ok(())
        } else {
            Err(FsError::NotFound.into())
        }
    }

    async fn link(&self, name: &str, inode: Arc<dyn Inode>) -> Result<()> {
        let mut attr = inode.getattr().await?;
        let kind = attr.file_type;
        attr.nlinks += 1;
        inode.setattr(attr).await?;

        let mut entries = self.entries.lock_save_irq();

        if entries.iter().any(|e| e.name == name) {
            return Err(FsError::AlreadyExists.into());
        }

        entries.push(TmpFsDirEnt {
            name: name.to_string(),
            id: inode.id(),
            kind,
            inode,
        });

        Ok(())
    }
}

impl<C, G, T> TmpFsDirInode<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    pub fn new(id: u64, fs: Weak<TmpFs<C, G, T>>, mode: FilePermissions) -> Arc<Self> {
        Arc::new_cyclic(|weak_this| Self {
            entries: SpinLockIrq::new(Vec::new()),
            attrs: SpinLockIrq::new(FileAttr {
                size: 0,
                file_type: FileType::Directory,
                block_size: BLOCK_SZ as _,
                mode,
                ..Default::default()
            }),
            id,
            fs,
            this: weak_this.clone(),
        })
    }
}

pub struct TmpFs<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    id: u64,
    next_inode_id: AtomicU64,
    root: Arc<TmpFsDirInode<C, G, T>>,
    pg_allocator: PhantomData<G>,
    _phantom: PhantomData<T>,
}

impl<C, G, T> TmpFs<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    pub fn new(fs_id: u64) -> Arc<Self> {
        Arc::new_cyclic(|weak_fs| {
            let root =
                TmpFsDirInode::new(1, weak_fs.clone(), FilePermissions::from_bits_retain(0o766));

            Self {
                id: fs_id,
                next_inode_id: AtomicU64::new(2),
                root,
                pg_allocator: PhantomData,
                _phantom: PhantomData,
            }
        })
    }

    pub fn alloc_inode_id(&self) -> u64 {
        self.next_inode_id.fetch_add(1, Ordering::Relaxed)
    }
}

#[async_trait]
impl<C, G, T> Filesystem for TmpFs<C, G, T>
where
    C: CpuOps,
    G: PageAllocGetter<C>,
    T: AddressTranslator<()>,
{
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        Ok(self.root.clone())
    }

    fn id(&self) -> u64 {
        self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{FileType, InodeId, attr::FilePermissions};
    use crate::memory::{
        PAGE_SIZE,
        address::IdentityTranslator,
        page_alloc::{self, FrameAllocator, PageAllocGetter},
    };
    use crate::sync::once_lock::OnceLock;
    use crate::test::MockCpuOps;
    use alloc::vec;
    use std::sync::Arc;

    static PG_ALLOC: OnceLock<FrameAllocator<MockCpuOps>, MockCpuOps> = OnceLock::new();

    struct TmpFsPgAllocGetter {}

    impl PageAllocGetter<MockCpuOps> for TmpFsPgAllocGetter {
        fn global_page_alloc() -> &'static OnceLock<FrameAllocator<MockCpuOps>, MockCpuOps> {
            &PG_ALLOC
        }
    }

    /// Initializes the global allocator for the test suite.
    fn init_allocator() {
        PG_ALLOC.get_or_init(|| {
            // Allocate 32MB for the test heap to ensure we don't run out during large file tests
            page_alloc::tests::TestFixture::new(&[(0, 32 * 1024 * 1024)], &[]).leak_allocator()
        });
    }

    /// Creates a fresh Filesystem and a detached regular file for isolated file testing.
    fn setup_env() -> (
        Arc<TmpFs<MockCpuOps, TmpFsPgAllocGetter, IdentityTranslator>>,
        TmpFsReg<MockCpuOps, TmpFsPgAllocGetter, IdentityTranslator>,
    ) {
        init_allocator();
        let fs = TmpFs::new(0);
        let reg = TmpFsReg::new(InodeId::from_fsid_and_inodeid(0, 1024)).unwrap();
        (fs, reg)
    }

    /// Creates just the Filesystem to test directory hierarchies.
    fn setup_fs() -> Arc<TmpFs<MockCpuOps, TmpFsPgAllocGetter, IdentityTranslator>> {
        init_allocator();
        TmpFs::new(1)
    }

    #[tokio::test]
    async fn test_simple_read_write() {
        let (_, reg) = setup_env();
        let data = b"Hello, Kernel!";
        let mut buf = vec![0u8; 100];

        let written = reg.write_at(0, data).await.expect("Write failed");
        assert_eq!(written, data.len());

        let read = reg.read_at(0, &mut buf).await.expect("Read failed");
        assert_eq!(read, data.len());
        assert_eq!(&buf[..read], data);

        let attr = reg.getattr().await.expect("Getattr failed");
        assert_eq!(attr.size, data.len() as u64);
    }

    #[tokio::test]
    async fn test_write_across_page_boundary() {
        let (_, reg) = setup_env();

        let data_len = 5000;
        let data: Vec<u8> = (0..data_len).map(|i| (i % 255) as u8).collect();

        let written = reg.write_at(0, &data).await.expect("Write failed");
        assert_eq!(written, data_len);

        let mut buf = vec![0u8; data_len];
        let read = reg.read_at(0, &mut buf).await.expect("Read failed");
        assert_eq!(read, data_len);
        assert_eq!(buf, data);
    }

    #[tokio::test]
    async fn test_sparse_write_and_read() {
        let (_, reg) = setup_env();

        // Write at offset 5000 (Allocates Block 1, leaves Block 0 unallocated/sparse)
        let data = b"Sparse";
        reg.write_at(5000, data).await.expect("Write failed");

        let mut buf = vec![0u8; 10];
        let read = reg.read_at(0, &mut buf).await.expect("Read hole failed");
        assert_eq!(read, 10);
        assert_eq!(buf, vec![0u8; 10], "Sparse region should be zeroed");

        let read = reg.read_at(5000, &mut buf).await.expect("Read data failed");
        assert_eq!(read, data.len());
        assert_eq!(&buf[..read], data);

        let attr = reg.getattr().await.unwrap();
        assert_eq!(attr.size, 5000 + data.len() as u64);
    }

    #[tokio::test]
    async fn test_write_append() {
        let (_, reg) = setup_env();

        reg.write_at(0, b"Hello").await.unwrap();
        reg.write_at(5, b" World").await.unwrap();

        let mut buf = vec![0u8; 20];
        let read = reg.read_at(0, &mut buf).await.unwrap();
        assert_eq!(&buf[..read], b"Hello World");
    }

    #[tokio::test]
    async fn test_write_out_of_bounds() {
        let (_, reg) = setup_env();

        let max_sz = (PAGE_SIZE * (PAGE_SIZE / 8)) as u64;

        let res = reg.write_at(max_sz, b"X").await;

        assert!(res.is_err(), "Should fail to write beyond MAX_SZ");
    }

    #[tokio::test]
    async fn test_truncate() {
        let (_, reg) = setup_env();

        reg.write_at(0, b"1234567890").await.unwrap();

        reg.truncate(5).await.expect("Truncate down failed");
        let attr = reg.getattr().await.unwrap();
        assert_eq!(attr.size, 5);

        reg.truncate(10).await.expect("Truncate up failed");
        let attr = reg.getattr().await.unwrap();
        assert_eq!(attr.size, 10);

        let mut buf = vec![0u8; 10];
        let read = reg.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 10);
        // "12345" + 5 zeros
        assert_eq!(&buf[..5], b"12345");
        assert_eq!(&buf[5..], &[0, 0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn test_dir_create_and_lookup() {
        let fs = setup_fs();
        let root = fs.root_inode().await.unwrap();

        // Create a file
        let file_inode = root
            .create(
                "test_file.txt",
                FileType::File,
                FilePermissions::from_bits_retain(0),
            )
            .await
            .expect("Create failed");

        // Lookup
        let found = root.lookup("test_file.txt").await.expect("Lookup failed");
        assert_eq!(found.id(), file_inode.id());

        // Lookup non-existent
        let err = root.lookup("ghost.txt").await;
        assert!(err.is_err()); // Should be NotFound
    }

    #[tokio::test]
    async fn test_dir_create_duplicate() {
        let fs = setup_fs();
        let root = fs.root_inode().await.unwrap();

        root.create("dup", FileType::File, FilePermissions::from_bits_retain(0))
            .await
            .unwrap();

        let res = root
            .create("dup", FileType::File, FilePermissions::empty())
            .await;
        assert!(res.is_err(), "Should not allow duplicate file creation");
    }

    #[tokio::test]
    async fn test_dir_subdirectories() {
        let fs = setup_fs();
        let root = fs.root_inode().await.unwrap();

        // Create /subdir
        let subdir = root
            .create("subdir", FileType::Directory, FilePermissions::empty())
            .await
            .unwrap();

        // Create /subdir/inner
        let inner = subdir
            .create("inner", FileType::File, FilePermissions::empty())
            .await
            .unwrap();

        // Verify hierarchy
        let found_subdir = root.lookup("subdir").await.unwrap();
        let found_inner = found_subdir.lookup("inner").await.unwrap();
        assert_eq!(found_inner.id(), inner.id());
    }

    #[tokio::test]
    async fn test_readdir() {
        let fs = setup_fs();
        let root = fs.root_inode().await.unwrap();

        // Create files in "random" order
        root.create("c.txt", FileType::File, FilePermissions::empty())
            .await
            .unwrap();
        root.create("a.txt", FileType::File, FilePermissions::empty())
            .await
            .unwrap();
        root.create("b.dir", FileType::Directory, FilePermissions::empty())
            .await
            .unwrap();

        let mut dir_stream = root.readdir(0).await.expect("Readdir failed");

        let mut names = Vec::new();
        while let Some(dent) = dir_stream.next_entry().await.unwrap() {
            names.push(dent.name);
        }

        // We don't guarantee order in the current implementation (it's a Vec push),
        // but let's verify existence.
        assert!(names.contains(&"a.txt".to_string()));
        assert!(names.contains(&"b.dir".to_string()));
        assert!(names.contains(&"c.txt".to_string()));
        assert_eq!(names.len(), 3);
    }

    #[tokio::test]
    async fn test_inode_id_uniqueness() {
        let fs = setup_fs();
        let root = fs.root_inode().await.unwrap();

        let f1 = root
            .create("f1", FileType::File, FilePermissions::empty())
            .await
            .unwrap();
        let f2 = root
            .create("f2", FileType::File, FilePermissions::empty())
            .await
            .unwrap();

        assert_ne!(f1.id(), f2.id());
        assert_ne!(f1.id(), root.id());
    }
}
