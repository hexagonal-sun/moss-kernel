use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use core::num::NonZeroU32;

/// Shares live inodes, without retaining every inode ever visited.
pub(super) struct InodeCache<T> {
    entries: BTreeMap<NonZeroU32, Weak<T>>,
}

impl<T> InodeCache<T> {
    pub(super) fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    pub(super) fn get_or_insert_with(
        &mut self,
        id: NonZeroU32,
        create: impl FnOnce() -> T,
    ) -> Arc<T> {
        if let Some(existing) = self.entries.get(&id).and_then(Weak::upgrade) {
            return existing;
        }
        // Only scan on a miss. Cache hits (including the root inode) stay cheap.
        // A traversal with no retained handles cannot accumulate dead entries.
        self.entries.retain(|_, inode| inode.strong_count() != 0);
        let inode = Arc::new(create());
        self.entries.insert(id, Arc::downgrade(&inode));
        inode
    }

    pub(super) fn get(&self, id: NonZeroU32) -> Option<Arc<T>> {
        self.entries.get(&id).and_then(Weak::upgrade)
    }

    pub(super) fn remove(&mut self, id: NonZeroU32) {
        self.entries.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_live_inodes_and_reaps_closed_inodes() {
        let mut cache = InodeCache::new();
        let root_id = NonZeroU32::new(2).unwrap();
        let root = cache.get_or_insert_with(root_id, || 42);
        assert!(Arc::ptr_eq(&root, &cache.get_or_insert_with(root_id, || 0)));
        for id in 3..4096 {
            let inode = cache.get_or_insert_with(NonZeroU32::new(id).unwrap(), || id);
            assert!(cache.entries.len() <= 2);
            drop(inode);
        }
        assert_eq!(*root, 42);
    }

    #[test]
    fn inode_number_reuse_does_not_share_unlinked_state() {
        let mut cache = InodeCache::new();
        let id = NonZeroU32::new(10).unwrap();
        let old = cache.get_or_insert_with(id, || 1);
        cache.remove(id);
        let new = cache.get_or_insert_with(id, || 2);
        assert!(!Arc::ptr_eq(&old, &new));
        assert_eq!((*old, *new), (1, 2));
    }
}
