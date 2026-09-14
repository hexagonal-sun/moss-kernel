//! Private mount topology. Filesystem identity is separate from mount identity.
use super::location::{Dentry, VfsPath};
use crate::{
    process::user_namespace::UserNamespace,
    sync::{OnceLock, SpinLock},
};
use alloc::{
    collections::BTreeMap,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::Filesystem,
};

pub struct Mount {
    pub id: u64,
    pub fs: Arc<dyn Filesystem>,
    pub root: Arc<Dentry>,
    pub fs_name: String,
    pub locked: bool,
    pub path_refs: AtomicUsize,
    namespace: Weak<MountNamespace>,
    // Strong parent references pin the route out of an attached mount. There
    // are no owning child links in Mount, so this cannot form a reference cycle.
    at: SpinLock<Option<(Arc<Mount>, Arc<Dentry>)>>,
}
impl Mount {
    fn new(
        ns: &Arc<MountNamespace>,
        fs: Arc<dyn Filesystem>,
        root: Arc<Dentry>,
        name: String,
        locked: bool,
        at: Option<(Arc<Mount>, Arc<Dentry>)>,
    ) -> Arc<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        if let Some((_, d)) = &at {
            d.mounted.fetch_add(1, Ordering::AcqRel);
        }
        Arc::new(Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            fs,
            root,
            fs_name: name,
            locked,
            path_refs: AtomicUsize::new(0),
            namespace: Arc::downgrade(ns),
            at: SpinLock::new(at),
        })
    }
    pub fn path(self: &Arc<Self>) -> VfsPath {
        VfsPath::new(Some(self.clone()), self.root.clone())
    }
    pub fn covered(&self) -> Option<VfsPath> {
        self.at
            .lock_save_irq()
            .as_ref()
            .map(|(m, d)| VfsPath::new(Some(m.clone()), d.clone()))
    }
    fn detach(&self) -> Option<(Arc<Mount>, Arc<Dentry>)> {
        let at = self.at.lock_save_irq().take();
        if let Some((_, d)) = &at {
            d.mounted.fetch_sub(1, Ordering::AcqRel);
        }
        at
    }
    pub fn namespace(&self) -> Option<Arc<MountNamespace>> {
        self.namespace.upgrade()
    }
}

struct Topology {
    root: Option<Arc<Mount>>,
    mounts: BTreeMap<u64, Arc<Mount>>,
    edges: BTreeMap<(u64, u64), u64>,
}
pub struct MountNamespace {
    pub id: u64,
    pub owner: Arc<UserNamespace>,
    tree: SpinLock<Topology>,
}
impl MountNamespace {
    fn empty(owner: Arc<UserNamespace>) -> Arc<Self> {
        Arc::new(Self {
            id: crate::process::namespace::next_namespace_id(),
            owner,
            tree: SpinLock::new(Topology {
                root: None,
                mounts: BTreeMap::new(),
                edges: BTreeMap::new(),
            }),
        })
    }
    pub fn initial() -> Arc<Self> {
        static INITIAL: OnceLock<Arc<MountNamespace>> = OnceLock::new();
        INITIAL
            .get_or_init(|| Self::empty(UserNamespace::initial()))
            .clone()
    }
    pub fn install_root(
        self: &Arc<Self>,
        fs: Arc<dyn Filesystem>,
        dentry: Arc<Dentry>,
        name: &str,
    ) {
        let m = Mount::new(self, fs, dentry, name.into(), false, None);
        let mut t = self.tree.lock_save_irq();
        assert!(t.root.is_none());
        t.root = Some(m.clone());
        t.mounts.insert(m.id, m);
    }
    pub fn root(&self) -> VfsPath {
        self.tree.lock_save_irq().root.as_ref().unwrap().path()
    }
    pub fn contains(&self, path: &VfsPath) -> bool {
        self.tree
            .lock_save_irq()
            .mounts
            .contains_key(&path.mount_id())
    }
    pub fn follow(mut path: VfsPath) -> VfsPath {
        loop {
            let Some(ns) = path.mount.as_ref().and_then(|m| m.namespace()) else {
                return path;
            };
            let next = {
                let t = ns.tree.lock_save_irq();
                t.edges
                    .get(&(path.mount_id(), path.dentry.id))
                    .and_then(|id| t.mounts.get(id))
                    .cloned()
            };
            let Some(next) = next else {
                return path;
            };
            path = next.path();
        }
    }
    pub fn attach(
        self: &Arc<Self>,
        target: &VfsPath,
        fs: Arc<dyn Filesystem>,
        root: Arc<Dentry>,
        name: &str,
    ) -> Result<()> {
        if target.dentry.is_deleted() {
            return Err(FsError::NotFound.into());
        }
        let parent = target
            .mount
            .as_ref()
            .ok_or(KernelError::InvalidValue)?
            .clone();
        let mut t = self.tree.lock_save_irq();
        if !t.mounts.contains_key(&parent.id) {
            return Err(KernelError::InvalidValue);
        }
        let key = (parent.id, target.dentry.id);
        if t.edges.contains_key(&key) {
            return Err(FsError::Busy.into());
        }
        if t.mounts.len() >= 100000 {
            return Err(KernelError::NoSpace);
        }
        let m = Mount::new(
            self,
            fs,
            root,
            name.into(),
            false,
            Some((parent, target.dentry.clone())),
        );
        t.edges.insert(key, m.id);
        t.mounts.insert(m.id, m);
        Ok(())
    }
    pub fn duplicate(
        self: &Arc<Self>,
        owner: Arc<UserNamespace>,
    ) -> (Arc<Self>, BTreeMap<u64, Arc<Mount>>) {
        let ns = Self::empty(owner.clone());
        let old = self.tree.lock_save_irq();
        let mut tree = ns.tree.lock_save_irq();
        let mut remap: BTreeMap<u64, Arc<Mount>> = BTreeMap::new();
        // IDs increase in attachment order; parents are copied before children.
        for m in old.mounts.values() {
            let at =
                m.at.lock_save_irq()
                    .as_ref()
                    .map(|(p, d)| (remap[&p.id].clone(), d.clone()));
            let new = Mount::new(
                &ns,
                m.fs.clone(),
                m.root.clone(),
                m.fs_name.clone(),
                m.locked || owner != self.owner,
                at.clone(),
            );
            if old.root.as_ref().is_some_and(|r| r.id == m.id) {
                tree.root = Some(new.clone());
            }
            if let Some((p, d)) = at {
                tree.edges.insert((p.id, d.id), new.id);
            }
            tree.mounts.insert(new.id, new.clone());
            remap.insert(m.id, new);
        }
        drop(tree);
        (ns, remap)
    }
    pub fn has_locked_children(&self, path: &VfsPath) -> bool {
        self.tree.lock_save_irq().mounts.values().any(|m| {
            m.locked
                && m.at
                    .lock_save_irq()
                    .as_ref()
                    .is_some_and(|(p, d)| p.id == path.mount_id() && d.is_below(&path.dentry))
        })
    }
    pub fn is_mountpoint(&self, path: &VfsPath) -> bool {
        let t = self.tree.lock_save_irq();
        t.edges.contains_key(&(path.mount_id(), path.dentry.id)) || path.is_mount_root()
    }
    pub fn unmount(&self, path: &VfsPath, lazy: bool) -> Result<()> {
        let m = path.mount.as_ref().ok_or(KernelError::InvalidValue)?;
        let mut t = self.tree.lock_save_irq();
        if !t.mounts.contains_key(&m.id) || !path.is_mount_root() || m.locked {
            return Err(KernelError::InvalidValue);
        }
        if t.root.as_ref().is_some_and(|r| r.id == m.id) {
            return Err(FsError::Busy.into());
        }
        if t.edges.keys().any(|(p, _)| *p == m.id) {
            // Subtree lazy detach needs an independently owned detached tree.
            // Do not report success while losing its child mounts.
            return Err(if lazy {
                KernelError::NotSupported
            } else {
                FsError::Busy.into()
            });
        }
        if !lazy && m.path_refs.load(Ordering::Acquire) > 1 {
            return Err(FsError::Busy.into());
        }
        let (parent, dentry) = m.detach().ok_or(KernelError::InvalidValue)?;
        t.edges.remove(&(parent.id, dentry.id));
        t.mounts.remove(&m.id);
        Ok(())
    }
    pub fn mounts(&self) -> Vec<Arc<Mount>> {
        self.tree.lock_save_irq().mounts.values().cloned().collect()
    }
}
impl Drop for MountNamespace {
    fn drop(&mut self) {
        // Remaining paths keep individual mounts/filesystems alive, not a
        // namespace that no task or namespace descriptor references any more.
        for m in self.tree.lock_save_irq().mounts.values() {
            m.detach();
        }
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        self.detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use async_trait::async_trait;
    use libkernel::fs::Inode;
    use moss_macros::ktest;

    struct TestFs;
    #[async_trait]
    impl Filesystem for TestFs {
        async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
            Ok(Arc::new(crate::fs::DummyInode {}))
        }
        fn id(&self) -> u64 {
            1000
        }
        fn magic(&self) -> u64 {
            0
        }
    }
    fn fixture() -> (Arc<MountNamespace>, Arc<dyn Filesystem>) {
        let ns = MountNamespace::empty(UserNamespace::initial());
        let fs: Arc<dyn Filesystem> = Arc::new(TestFs);
        ns.install_root(
            fs.clone(),
            Dentry::root(Arc::new(crate::fs::DummyInode {})),
            "test",
        );
        (ns, fs)
    }
    fn child(parent: &VfsPath, name: &str) -> VfsPath {
        parent.child(name, Arc::new(crate::fs::DummyInode {}))
    }

    #[ktest]
    fn copied_mounts_keep_identity_and_lock_inherited_tree() {
        assert_eq!(
            libkernel::error::syscall_error::kern_err_to_syscall(FsError::CrossDevice.into()),
            -18
        );
        assert_eq!(
            libkernel::error::syscall_error::kern_err_to_syscall(FsError::DriverNotFound.into()),
            -19
        );
        let (ns, fs) = fixture();
        let root = ns.root();
        let target = child(&root, "target");
        ns.attach(
            &target,
            fs.clone(),
            Dentry::root(Arc::new(crate::fs::DummyInode {})),
            "test",
        )
        .unwrap();
        let mounted = MountNamespace::follow(target.clone());
        let (copy, map) = ns.duplicate(UserNamespace::initial());
        assert!(Arc::ptr_eq(
            &map[&mounted.mount_id()].fs,
            &mounted.mount.as_ref().unwrap().fs
        ));
        assert_ne!(map[&mounted.mount_id()].id, mounted.mount_id());
        assert_eq!(map[&mounted.mount_id()].root.id, mounted.dentry.id);
        assert!(!copy.contains(&mounted));
        let creds = crate::process::creds::Credentials::new_root();
        let owner = UserNamespace::create(&creds).unwrap();
        let (locked, remap) = ns.duplicate(owner);
        let locked_path = remap[&mounted.mount_id()].path();
        assert_eq!(
            locked.unmount(&locked_path, true),
            Err(KernelError::InvalidValue)
        );
        assert!(locked.has_locked_children(&locked.root()));
        drop(locked_path);
        drop(remap);
        drop(locked);
        drop(map);
        drop(copy);
        assert_eq!(target.dentry.mounted.load(Ordering::Acquire), 1);
    }

    #[ktest]
    fn lazy_detach_pins_filesystem_only_until_last_path() {
        let (ns, _) = fixture();
        let root = ns.root();
        let target = child(&root, "target");
        let fs: Arc<dyn Filesystem> = Arc::new(TestFs);
        let weak_fs = Arc::downgrade(&fs);
        ns.attach(
            &target,
            fs,
            Dentry::root(Arc::new(crate::fs::DummyInode {})),
            "test",
        )
        .unwrap();
        let path = MountNamespace::follow(target.clone());
        let retained = path.clone();
        assert_eq!(ns.unmount(&path, false), Err(FsError::Busy.into()));
        ns.unmount(&path, true).unwrap();
        assert!(MountNamespace::follow(target.clone()) == target);
        assert_eq!(target.dentry.mounted.load(Ordering::Acquire), 0);
        assert!(retained.parent(&root) == retained);
        drop(path);
        assert!(weak_fs.upgrade().is_some());
        let pinned = retained.pinned_inode();
        drop(retained);
        assert!(weak_fs.upgrade().is_some());
        drop(pinned);
        assert!(weak_fs.upgrade().is_none());
    }

    #[ktest]
    fn live_dentries_follow_rename_and_reject_deleted_mountpoint() {
        let (ns, fs) = fixture();
        let root = ns.root();
        let dir = child(&root, "old");
        let file = child(&dir, "file");
        assert!(dir == child(&root, "old"));
        dir.dentry.moved(&root.dentry, "new");
        assert_eq!(file.relative_to(&root).unwrap().as_str(), "/new/file");
        Dentry::unlink(&dir.dentry, "file");
        assert!(file.relative_to(&root).is_none());
        assert_eq!(file.display_path(&root).as_str(), "/new/file (deleted)");
        assert_eq!(
            ns.attach(&file, fs, root.dentry.clone(), "test"),
            Err(FsError::NotFound.into())
        );
    }
}
