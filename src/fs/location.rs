//! A pathname is a mount and a dentry, not just an inode. Bind mounts and
//! namespace copies can expose the same inode through different locations.
use super::mount::Mount;
use crate::sync::SpinLock;
use alloc::{
    boxed::Box,
    collections::BTreeMap,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use async_trait::async_trait;
use core::{
    any::Any,
    ops::Deref,
    sync::atomic::{AtomicU64, Ordering},
};
use libkernel::{
    error::Result,
    fs::{Inode, InodeId, attr::FileAttr, pathbuf::PathBuf},
};

pub struct Dentry {
    pub id: u64,
    pub mounted: core::sync::atomic::AtomicUsize,
    pub inode: Arc<dyn Inode>,
    location: SpinLock<Option<(Arc<Dentry>, String)>>,
    children: SpinLock<BTreeMap<String, Weak<Dentry>>>,
    deleted: core::sync::atomic::AtomicBool,
}

impl Dentry {
    fn new(inode: Arc<dyn Inode>, location: Option<(Arc<Self>, String)>) -> Arc<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Arc::new(Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            inode,
            mounted: core::sync::atomic::AtomicUsize::new(0),
            location: SpinLock::new(location),
            children: SpinLock::new(BTreeMap::new()),
            deleted: core::sync::atomic::AtomicBool::new(false),
        })
    }
    pub fn root(inode: Arc<dyn Inode>) -> Arc<Self> {
        Self::new(inode, None)
    }

    pub async fn lookup(self: &Arc<Self>, name: &str) -> Result<Arc<Self>> {
        // Revalidate against the filesystem, including dynamic proc entries.
        let inode = self.inode.lookup(name).await?;
        Ok(self.child(name, inode))
    }
    pub fn child(self: &Arc<Self>, name: &str, inode: Arc<dyn Inode>) -> Arc<Self> {
        let mut children = self.children.lock_save_irq();
        if let Some(child) = children.get(name).and_then(Weak::upgrade)
            && child.inode.id() == inode.id()
            && !child.deleted.load(Ordering::Acquire)
        {
            return child;
        }
        if children.len() >= 64 {
            children.retain(|_, d| d.strong_count() != 0);
        }
        let child = Self::new(inode, Some((self.clone(), name.into())));
        children.insert(name.into(), Arc::downgrade(&child));
        child
    }
    pub fn parent(&self) -> Option<Arc<Self>> {
        self.location
            .lock_save_irq()
            .as_ref()
            .map(|(p, _)| p.clone())
    }
    pub fn is_deleted(&self) -> bool {
        self.deleted.load(Ordering::Acquire)
    }
    pub fn is_below(self: &Arc<Self>, root: &Arc<Self>) -> bool {
        let mut d = self.clone();
        loop {
            if d.id == root.id {
                return true;
            }
            let Some(parent) = d.parent() else {
                return false;
            };
            d = parent;
        }
    }
    pub fn unlink(parent: &Arc<Self>, name: &str) {
        if let Some(child) = parent
            .children
            .lock_save_irq()
            .remove(name)
            .and_then(|d| d.upgrade())
        {
            child.deleted.store(true, Ordering::Release);
        }
    }
    pub fn moved(self: &Arc<Self>, parent: &Arc<Self>, name: &str) {
        let old = self.location.lock_save_irq().clone();
        if let Some((old_parent, old_name)) = old {
            old_parent.children.lock_save_irq().remove(&old_name);
        }
        *self.location.lock_save_irq() = Some((parent.clone(), name.into()));
        parent
            .children
            .lock_save_irq()
            .insert(name.into(), Arc::downgrade(self));
    }
}

pub struct VfsPath {
    pub mount: Option<Arc<Mount>>,
    pub dentry: Arc<Dentry>,
}
impl Clone for VfsPath {
    fn clone(&self) -> Self {
        Self::new(self.mount.clone(), self.dentry.clone())
    }
}
impl Drop for VfsPath {
    fn drop(&mut self) {
        if let Some(m) = &self.mount {
            m.path_refs.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
impl PartialEq for VfsPath {
    fn eq(&self, rhs: &Self) -> bool {
        self.mount_id() == rhs.mount_id() && self.dentry.id == rhs.dentry.id
    }
}
impl Eq for VfsPath {}
impl Deref for VfsPath {
    type Target = Arc<dyn Inode>;
    fn deref(&self) -> &Self::Target {
        &self.dentry.inode
    }
}
impl VfsPath {
    pub fn mount_flags(&self) -> u64 {
        self.mount.as_ref().map_or(0, |m| m.attrs.effective_flags())
    }
    pub fn begin_write(&self) -> Result<Option<super::mount::attributes::WriteLease>> {
        self.mount
            .as_ref()
            .map(super::mount::attributes::WriteLease::acquire)
            .transpose()
    }
    pub fn check_exec_mount(&self) -> Result<()> {
        if self.mount_flags() & super::mount::attributes::NOEXEC != 0 {
            return Err(libkernel::error::FsError::PermissionDenied.into());
        }
        Ok(())
    }
    pub async fn setattr(&self, attr: FileAttr) -> Result<()> {
        let _lease = self.begin_write()?;
        self.dentry.inode.setattr(attr).await
    }
    pub async fn truncate(&self, size: u64) -> Result<()> {
        let _lease = self.begin_write()?;
        self.dentry.inode.truncate(size).await
    }
    pub async fn create(
        &self,
        name: &str,
        kind: libkernel::fs::FileType,
        mode: libkernel::fs::attr::FilePermissions,
        time: Option<core::time::Duration>,
    ) -> Result<Arc<dyn Inode>> {
        let _lease = self.begin_write()?;
        self.dentry.inode.create(name, kind, mode, time).await
    }
    pub async fn unlink(&self, name: &str) -> Result<()> {
        let _lease = self.begin_write()?;
        self.dentry.inode.unlink(name).await
    }
    pub async fn link(&self, name: &str, inode: Arc<dyn Inode>) -> Result<()> {
        let _lease = self.begin_write()?;
        self.dentry.inode.link(name, inode).await
    }
    pub async fn symlink(&self, name: &str, target: &libkernel::fs::path::Path) -> Result<()> {
        let _lease = self.begin_write()?;
        self.dentry.inode.symlink(name, target).await
    }
    pub async fn rename_from(
        &self,
        parent: Arc<dyn Inode>,
        old: &str,
        new: &str,
        no_replace: bool,
    ) -> Result<()> {
        let _lease = self.begin_write()?;
        self.dentry
            .inode
            .rename_from(parent, old, new, no_replace)
            .await
    }
    pub async fn exchange(&self, first: &str, parent: Arc<dyn Inode>, second: &str) -> Result<()> {
        let _lease = self.begin_write()?;
        self.dentry.inode.exchange(first, parent, second).await
    }
    pub async fn setxattr(
        &self,
        name: &str,
        value: &[u8],
        create: bool,
        replace: bool,
    ) -> Result<()> {
        let _lease = self.begin_write()?;
        self.dentry
            .inode
            .setxattr(name, value, create, replace)
            .await
    }
    pub async fn removexattr(&self, name: &str) -> Result<()> {
        let _lease = self.begin_write()?;
        self.dentry.inode.removexattr(name).await
    }
    pub fn new(mount: Option<Arc<Mount>>, dentry: Arc<Dentry>) -> Self {
        if let Some(m) = &mount {
            m.path_refs.fetch_add(1, Ordering::AcqRel);
        }
        Self { mount, dentry }
    }
    pub fn anonymous(inode: Arc<dyn Inode>) -> Self {
        Self::new(None, Dentry::root(inode))
    }
    pub fn inode(&self) -> Arc<dyn Inode> {
        self.dentry.inode.clone()
    }
    pub fn mount_id(&self) -> u64 {
        self.mount.as_ref().map_or(0, |m| m.id)
    }
    pub fn is_mount_root(&self) -> bool {
        self.mount
            .as_ref()
            .is_some_and(|m| m.root.id == self.dentry.id)
    }
    pub async fn lookup(&self, name: &str) -> Result<Self> {
        Ok(Self::new(
            self.mount.clone(),
            self.dentry.lookup(name).await?,
        ))
    }
    pub fn child(&self, name: &str, inode: Arc<dyn Inode>) -> Self {
        Self::new(self.mount.clone(), self.dentry.child(name, inode))
    }
    pub fn parent(&self, root: &Self) -> Self {
        let mut here = self.clone();
        loop {
            if here == *root {
                return here;
            }
            if here.is_mount_root() {
                let covered = here.mount.as_ref().unwrap().covered();
                if let Some(covered) = covered {
                    here = covered;
                    continue;
                }
                return here;
            }
            return here
                .dentry
                .parent()
                .map_or_else(|| here.clone(), |p| Self::new(here.mount.clone(), p));
        }
    }
    /// Render from live dentries, so rename, bind mounts and chroot are visible.
    /// None means this path cannot be reached from the supplied process root.
    pub fn relative_to(&self, root: &Self) -> Option<PathBuf> {
        let mut here = self.clone();
        let mut names: Vec<String> = Vec::new();
        loop {
            if here == *root {
                break;
            }
            if here.dentry.deleted.load(Ordering::Acquire) {
                return None;
            }
            if here.is_mount_root() {
                here = here.mount.as_ref()?.covered()?;
                continue;
            }
            let (parent, name) = here.dentry.location.lock_save_irq().clone()?;
            names.push(name);
            here = Self::new(here.mount.clone(), parent);
        }
        names.reverse();
        Some(alloc::format!("/{}", names.join("/")).into())
    }
    /// proc readlink can describe a retained path in another namespace or a
    /// detached mount. Unlike getcwd, it must not reinterpret it in our view.
    pub fn display_path(&self, root: &Self) -> PathBuf {
        let mut here = self.clone();
        let mut names: Vec<String> = Vec::new();
        let mut deleted = false;
        loop {
            deleted |= here.dentry.deleted.load(Ordering::Acquire);
            if here == *root {
                break;
            }
            if here.is_mount_root() {
                if let Some(covered) = here.mount.as_ref().and_then(|m| m.covered()) {
                    here = covered;
                    continue;
                }
                break;
            }
            let Some((parent, name)) = here.dentry.location.lock_save_irq().clone() else {
                break;
            };
            names.push(name);
            here = Self::new(here.mount.clone(), parent);
        }
        names.reverse();
        alloc::format!(
            "/{}{}",
            names.join("/"),
            if deleted { " (deleted)" } else { "" }
        )
        .into()
    }
    /// VMAs must pin the mount too: filesystem inodes can hold only a Weak fs.
    pub fn pinned_inode(&self) -> Arc<dyn Inode> {
        Arc::new(PinnedInode(
            self.clone(),
            self.mount_flags() & super::mount::attributes::NOEXEC == 0,
        ))
    }
}

struct PinnedInode(VfsPath, bool);
pub fn check_mapping_exec(inode: &dyn Inode) -> Result<()> {
    if let Some(pinned) = inode.as_any().downcast_ref::<PinnedInode>()
        && !pinned.1
    {
        return Err(libkernel::error::FsError::PermissionDenied.into());
    }
    Ok(())
}
#[async_trait]
impl Inode for PinnedInode {
    fn id(&self) -> InodeId {
        self.0.id()
    }
    async fn getattr(&self) -> Result<FileAttr> {
        self.0.getattr().await
    }
    async fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize> {
        self.0.read_at(off, buf).await
    }
    async fn write_at(&self, off: u64, buf: &[u8]) -> Result<usize> {
        self.0.write_at(off, buf).await
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
