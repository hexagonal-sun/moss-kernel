use crate::fs::BlockDevice;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

struct CacheEntry {
    block_number: u64,
    data: Vec<u8>,
    dirty: bool,
}

/// LRU block cache
pub(crate) struct BlockCache {
    /// The cache entries, ordered from most recently used (front) to least recently used (back)
    /// This never exceeds `capacity` in length.
    cache: VecDeque<CacheEntry>,
    /// Maximum number of blocks the cache can hold
    capacity: usize,
    /// Size of each block in bytes
    block_size: usize,
    pub dev: Box<dyn BlockDevice>,
}

impl BlockCache {
    /// Creates a new BlockCache with the given capacity (in number of blocks)
    /// and block size (in bytes).
    pub fn new(capacity: usize, block_size: usize, dev: Box<dyn BlockDevice>) -> Self {
        Self {
            cache: VecDeque::with_capacity(capacity),
            capacity,
            block_size,
            dev,
        }
    }

    /// Finds the position of a block in the cache
    fn find_position(&self, block_number: u64) -> Option<usize> {
        for (i, entry) in self.cache.iter().enumerate() {
            if entry.block_number == block_number {
                return Some(i);
            }
        }
        None
    }

    /// Retrieves a block from the cache, loading it from the device if necessary.
    pub async fn get_or_load(&mut self, block_number: u64) -> crate::error::Result<&[u8]> {
        let block_size = self.block_size;

        if let Some(pos) = self.find_position(block_number) {
            let entry = self.cache.remove(pos).unwrap();
            self.cache.push_front(entry);
            return Ok(&self.cache.front().unwrap().data);
        }

        // Not in cache, read from device
        let mut data = vec![0; block_size];
        self.dev.read(block_number, &mut data).await?;

        self.insert(block_number, data);

        // Return a reference to the newly inserted block at the front
        Ok(&self
            .cache
            .front()
            .expect("cache should have an entry after insert")
            .data)
    }

    /// Retrieves a mutable block from the cache, loading it from the device if necessary.
    /// Marks the block as dirty.
    pub async fn get_or_load_mut(
        &mut self,
        block_number: u64,
    ) -> crate::error::Result<&mut Vec<u8>> {
        let block_size = self.block_size;

        if let Some(pos) = self.find_position(block_number) {
            let entry = self.cache.remove(pos).unwrap();
            self.cache.push_front(entry);
            // Mark as dirty since we are returning a mutable reference
            if let Some(entry) = self.cache.front_mut() {
                entry.dirty = true;
            } else {
                panic!("cache should have an entry after re-insert");
            }
            return Ok(&mut self.cache.front_mut().unwrap().data);
        }

        // Not in cache, read from device
        let mut data = vec![0; block_size];
        self.dev.read(block_number, &mut data).await?;

        self.insert(block_number, data);

        // Mark as dirty since we are returning a mutable reference
        if let Some(entry) = self.cache.front_mut() {
            entry.dirty = true;
        } else {
            panic!("cache should have an entry after insert");
        }

        // Return a mutable reference to the newly inserted block at the front
        Ok(&mut self
            .cache
            .front_mut()
            .expect("cache should have an entry after insert")
            .data)
    }

    /// Inserts a block into the cache.
    pub fn insert(&mut self, block_number: u64, data: Vec<u8>) {
        if self.cache.len() == self.capacity {
            self.cache.pop_back();
        }
        self.cache.push_front(CacheEntry {
            block_number,
            data,
            dirty: false,
        });
    }

    #[expect(dead_code)]
    pub fn write_back(&mut self, block_number: u64, data: Vec<u8>) {
        if let Some(entry) = self
            .cache
            .iter_mut()
            .find(|entry| entry.block_number == block_number)
        {
            entry.data = data;
            entry.dirty = true;
        } else {
            self.insert(block_number, data);
            if let Some(entry) = self
                .cache
                .iter_mut()
                .find(|entry| entry.block_number == block_number)
            {
                entry.dirty = true;
            }
        }
    }

    /// Writes all dirty blocks back to the device.
    pub async fn write_dirty(&mut self) -> crate::error::Result<()> {
        for entry in self.cache.iter_mut() {
            if entry.dirty {
                self.dev.write(entry.block_number, &entry.data).await?;
                entry.dirty = false;
            }
        }
        Ok(())
    }
}
