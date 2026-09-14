//! Mount identity, private/shared/slave topology and atomic propagation.
pub mod attributes;
use super::location::{Dentry, VfsPath};
use crate::{
    process::user_namespace::UserNamespace,
    sync::{OnceLock, SpinLock},
};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use attributes::{MountAttributes, SuperblockState};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::Filesystem,
};

/// All topology writes (including cross-namespace propagation and remount)
/// share this short, non-async transaction lock. No filesystem I/O under it.
pub(super) static MOUNT_OPS: SpinLock<()> = SpinLock::new(());
static MOUNTS: SpinLock<Vec<Weak<Mount>>> = SpinLock::new(Vec::new());
fn mounts_live() -> Vec<Arc<Mount>> {
    let mut mounts = MOUNTS.lock_save_irq();
    mounts.retain(|m| m.strong_count() != 0);
    mounts
        .iter()
        .filter_map(Weak::upgrade)
        .filter(|m| m.attached.load(Ordering::Acquire))
        .collect()
}
fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Master links only point to already existing groups: the propagation graph
/// is acyclic. Retaining a vanished group preserves its upstream relation.
pub struct PeerGroup {
    pub id: u64,
    master: Option<Arc<PeerGroup>>,
}
impl PeerGroup {
    fn new(master: Option<Arc<Self>>) -> Arc<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Arc::new(Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            master,
        })
    }
    fn descends_from(&self, id: u64) -> bool {
        let mut group = Some(self);
        while let Some(g) = group {
            if g.id == id {
                return true;
            }
            group = g.master.as_deref();
        }
        false
    }
}
#[derive(Clone, Default)]
pub struct Propagation {
    pub peer: Option<Arc<PeerGroup>>,
    master: Option<Arc<PeerGroup>>,
    pub unbindable: bool,
}
impl Propagation {
    fn master(&self) -> Option<Arc<PeerGroup>> {
        self.peer
            .as_ref()
            .and_then(|p| p.master.clone())
            .or_else(|| self.master.clone())
    }
    fn receives(&self, group: u64) -> bool {
        self.peer.as_ref().is_some_and(|p| p.id == group)
            || self.master().is_some_and(|m| m.descends_from(group))
    }
    fn less_privileged(&self) -> Self {
        Self {
            peer: None,
            master: self.peer.clone().or_else(|| self.master()),
            unbindable: self.unbindable,
        }
    }
    fn shared(&mut self) {
        if self.peer.is_none() {
            self.peer = Some(PeerGroup::new(self.master.take()));
        }
        self.unbindable = false;
    }
}

pub struct Mount {
    pub id: u64,
    pub fs: Arc<dyn Filesystem>,
    pub root: Arc<Dentry>,
    pub fs_name: String,
    pub locked: bool,
    pub attrs: MountAttributes,
    pub path_refs: AtomicUsize,
    pub propagation: SpinLock<Propagation>,
    /// Replicas of one attachment event, not aliases of the same inode.
    event: u64,
    attached: AtomicBool,
    namespace: Weak<MountNamespace>,
    at: SpinLock<Option<(Arc<Mount>, Arc<Dentry>)>>,
}
struct MountSpec {
    fs: Arc<dyn Filesystem>,
    root: Arc<Dentry>,
    name: String,
    sb: Arc<SuperblockState>,
    flags: u64,
    locked_flags: u64,
    locked: bool,
    propagation: Propagation,
    event: u64,
}
impl Mount {
    fn new(
        ns: &Arc<MountNamespace>,
        spec: MountSpec,
        at: Option<(Arc<Mount>, Arc<Dentry>)>,
    ) -> Arc<Self> {
        if let Some((_, d)) = &at {
            d.mounted.fetch_add(1, Ordering::AcqRel);
        }
        let mount = Arc::new(Self {
            id: next_id(),
            fs: spec.fs,
            root: spec.root,
            fs_name: spec.name,
            locked: spec.locked,
            attrs: MountAttributes::new(spec.sb, spec.flags, spec.locked_flags),
            path_refs: AtomicUsize::new(0),
            propagation: SpinLock::new(spec.propagation),
            event: spec.event,
            attached: AtomicBool::new(true),
            namespace: Arc::downgrade(ns),
            at: SpinLock::new(at),
        });
        MOUNTS.lock_save_irq().push(Arc::downgrade(&mount));
        mount
    }
    fn spec(&self) -> MountSpec {
        MountSpec {
            fs: self.fs.clone(),
            root: self.root.clone(),
            name: self.fs_name.clone(),
            sb: self.attrs.superblock.clone(),
            flags: self.attrs.flags(),
            locked_flags: self.attrs.locked,
            locked: self.locked,
            propagation: self.propagation.lock_save_irq().clone(),
            event: self.event,
        }
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
        self.attached.store(false, Ordering::Release);
        let at = self.at.lock_save_irq().take();
        if let Some((_, d)) = &at {
            d.mounted.fetch_sub(1, Ordering::AcqRel);
        }
        at
    }
    pub fn namespace(&self) -> Option<Arc<MountNamespace>> {
        self.namespace.upgrade()
    }
    pub fn propagation_ids(&self) -> (Option<u64>, Option<u64>, bool) {
        let _guard = MOUNT_OPS.lock_save_irq();
        let p = self.propagation.lock_save_irq().clone();
        let live = mounts_live();
        let mut master = p.master();
        while let Some(g) = &master {
            if live.iter().any(|m| {
                m.propagation
                    .lock_save_irq()
                    .peer
                    .as_ref()
                    .is_some_and(|p| p.id == g.id)
            }) {
                break;
            }
            master = g.master.clone();
        }
        (
            p.peer.as_ref().map(|g| g.id),
            master.map(|g| g.id),
            p.unbindable,
        )
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
struct CopyEntry {
    old: u64,
    parent: Option<(u64, Arc<Dentry>)>,
    spec: MountSpec,
}
struct Destination {
    ns: Arc<MountNamespace>,
    mount: Arc<Mount>,
    dentry: Arc<Dentry>,
    propagation: Propagation,
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
        let _guard = MOUNT_OPS.lock_save_irq();
        let sb = SuperblockState::get(fs.id(), self.owner.clone(), 0);
        let m = Mount::new(
            self,
            MountSpec {
                fs,
                root: dentry,
                name: name.into(),
                sb,
                flags: 0,
                locked_flags: 0,
                locked: false,
                propagation: Propagation::default(),
                event: next_id(),
            },
            None,
        );
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
    fn destinations(self: &Arc<Self>, target: &VfsPath) -> Result<Vec<Destination>> {
        let parent = target.mount.as_ref().ok_or(KernelError::InvalidValue)?;
        if !self.contains(target) {
            return Err(KernelError::InvalidValue);
        }
        if target.dentry.is_deleted() {
            return Err(FsError::NotFound.into());
        }
        let p = parent.propagation.lock_save_irq().clone();
        let mut dest = alloc::vec![Destination {
            ns: self.clone(),
            mount: parent.clone(),
            dentry: target.dentry.clone(),
            propagation: p.clone()
        }];
        if let Some(group) = p.peer {
            for peer in mounts_live() {
                if peer.id == parent.id || !target.dentry.is_below(&peer.root) {
                    continue;
                }
                let prop = peer.propagation.lock_save_irq().clone();
                if !prop.receives(group.id) {
                    continue;
                }
                let Some(ns) = peer.namespace() else {
                    continue;
                };
                let at = Self::follow(VfsPath::new(Some(peer), target.dentry.clone()));
                dest.push(Destination {
                    ns,
                    mount: at.mount.as_ref().unwrap().clone(),
                    dentry: at.dentry.clone(),
                    propagation: prop,
                });
            }
        }
        Ok(dest)
    }
    /// Stage every replica, validate capacity/collisions in all namespaces,
    /// then publish the complete set. No partial propagated attachment.
    fn attach_tree(self: &Arc<Self>, target: &VfsPath, mut entries: Vec<CopyEntry>) -> Result<()> {
        let destinations = self.destinations(target)?;
        let origin_parent = destinations[0].propagation.peer.clone();
        if origin_parent.is_some() {
            for entry in &mut entries {
                entry.spec.propagation.shared();
            }
        }
        let mut groups: Vec<BTreeMap<u64, Arc<PeerGroup>>> = entries
            .iter()
            .map(|e| {
                let mut map = BTreeMap::new();
                if let (Some(parent), Some(child)) = (&origin_parent, &e.spec.propagation.peer) {
                    map.insert(parent.id, child.clone());
                }
                map
            })
            .collect();
        let mut staged: Vec<(Arc<Self>, Arc<Mount>, u64, u64)> = Vec::new();
        let mut counts: BTreeMap<u64, usize> = BTreeMap::new();
        let mut keys = BTreeSet::new();
        for dest in destinations {
            let mut copies: BTreeMap<u64, Arc<Mount>> = BTreeMap::new();
            for (idx, entry) in entries.iter().enumerate() {
                let (parent, dentry) = entry.parent.as_ref().map_or_else(
                    || (dest.mount.clone(), dest.dentry.clone()),
                    |(id, d)| (copies[id].clone(), d.clone()),
                );
                let mut prop = entry.spec.propagation.clone();
                if let Some(origin) = &origin_parent {
                    if dest
                        .propagation
                        .peer
                        .as_ref()
                        .is_none_or(|g| g.id != origin.id)
                    {
                        if let Some(peer) = &dest.propagation.peer {
                            prop = Propagation {
                                peer: Some(replica_group(peer, &mut groups[idx])),
                                master: None,
                                unbindable: false,
                            };
                        } else if let Some(master) = dest.propagation.master() {
                            prop = Propagation {
                                peer: None,
                                master: Some(replica_group(&master, &mut groups[idx])),
                                unbindable: false,
                            };
                        }
                    }
                }
                let less = dest.ns.owner != self.owner;
                let spec = MountSpec {
                    fs: entry.spec.fs.clone(),
                    root: entry.spec.root.clone(),
                    name: entry.spec.name.clone(),
                    sb: entry.spec.sb.clone(),
                    flags: entry.spec.flags,
                    locked_flags: entry.spec.locked_flags | if less { entry.spec.flags } else { 0 },
                    locked: entry.spec.locked || less,
                    propagation: prop,
                    event: entry.spec.event,
                };
                if !keys.insert((dest.ns.id, parent.id, dentry.id)) {
                    return Err(FsError::Busy.into());
                }
                if dest
                    .ns
                    .tree
                    .lock_save_irq()
                    .edges
                    .contains_key(&(parent.id, dentry.id))
                {
                    return Err(FsError::Busy.into());
                }
                *counts.entry(dest.ns.id).or_default() += 1;
                if dest.ns.tree.lock_save_irq().mounts.len() + counts[&dest.ns.id] > 100000 {
                    return Err(KernelError::NoSpace);
                }
                let mount = Mount::new(&dest.ns, spec, Some((parent.clone(), dentry.clone())));
                copies.insert(entry.old, mount.clone());
                staged.push((dest.ns.clone(), mount, parent.id, dentry.id));
            }
        }
        for (ns, m, parent, dentry) in staged {
            let mut t = ns.tree.lock_save_irq();
            t.edges.insert((parent, dentry), m.id);
            t.mounts.insert(m.id, m);
        }
        Ok(())
    }
    pub fn attach(
        self: &Arc<Self>,
        target: &VfsPath,
        fs: Arc<dyn Filesystem>,
        root: Arc<Dentry>,
        name: &str,
    ) -> Result<()> {
        self.attach_flags(target, fs, root, name, 0)
    }
    pub fn attach_flags(
        self: &Arc<Self>,
        target: &VfsPath,
        fs: Arc<dyn Filesystem>,
        root: Arc<Dentry>,
        name: &str,
        flags: u64,
    ) -> Result<()> {
        let _guard = MOUNT_OPS.lock_save_irq();
        let owner = crate::drivers::fs::proc::superblock_owner(fs.id())
            .unwrap_or_else(|| self.owner.clone());
        let sb = SuperblockState::get(fs.id(), owner, flags);
        self.attach_tree(
            target,
            alloc::vec![CopyEntry {
                old: 0,
                parent: None,
                spec: MountSpec {
                    fs,
                    root,
                    name: name.into(),
                    sb,
                    flags,
                    locked_flags: 0,
                    locked: false,
                    propagation: Propagation::default(),
                    event: next_id()
                }
            }],
        )
    }
    pub fn bind(
        self: &Arc<Self>,
        source: &VfsPath,
        target: &VfsPath,
        recursive: bool,
    ) -> Result<()> {
        let _guard = MOUNT_OPS.lock_save_irq();
        if !self.contains(source) || !self.contains(target) {
            return Err(KernelError::InvalidValue);
        }
        let source_mount = source.mount.as_ref().ok_or(KernelError::InvalidValue)?;
        let mut root = source_mount.spec();
        if root.propagation.unbindable || source.dentry.is_deleted() {
            return Err(KernelError::InvalidValue);
        }
        if !recursive && self.has_locked_children(source) {
            return Err(KernelError::InvalidValue);
        }
        root.root = source.dentry.clone();
        root.event = next_id();
        // A fresh bind of a locked mount is removable as a unit, but copied
        // restrictions and locked child mounts cannot be stripped.
        root.locked = false;
        let mut entries = alloc::vec![CopyEntry {
            old: source_mount.id,
            parent: None,
            spec: root
        }];
        if recursive {
            let all = self.mounts();
            let mut included = BTreeSet::from([source_mount.id]);
            for m in all {
                if m.id == source_mount.id {
                    continue;
                }
                let Some((parent, dentry)) = m.at.lock_save_irq().clone() else {
                    continue;
                };
                if !included.contains(&parent.id)
                    || (parent.id == source_mount.id && !dentry.is_below(&source.dentry))
                {
                    continue;
                }
                let spec = m.spec();
                if spec.propagation.unbindable {
                    if m.locked {
                        return Err(KernelError::InvalidValue);
                    }
                    continue;
                }
                included.insert(m.id);
                entries.push(CopyEntry {
                    old: m.id,
                    parent: Some((parent.id, dentry)),
                    spec,
                });
            }
        }
        self.attach_tree(target, entries)
    }
    pub fn duplicate(
        self: &Arc<Self>,
        owner: Arc<UserNamespace>,
    ) -> (Arc<Self>, BTreeMap<u64, Arc<Mount>>) {
        let _guard = MOUNT_OPS.lock_save_irq();
        let ns = Self::empty(owner.clone());
        let old = self.tree.lock_save_irq();
        let mut tree = ns.tree.lock_save_irq();
        let mut remap: BTreeMap<u64, Arc<Mount>> = BTreeMap::new();
        for m in old.mounts.values() {
            let at =
                m.at.lock_save_irq()
                    .as_ref()
                    .map(|(p, d)| (remap[&p.id].clone(), d.clone()));
            let mut spec = m.spec();
            if owner != self.owner {
                spec.locked = true;
                spec.locked_flags |= spec.flags;
                spec.propagation = spec.propagation.less_privileged();
            }
            let new = Mount::new(&ns, spec, at.clone());
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
    fn subtree(&self, root: u64) -> Vec<Arc<Mount>> {
        let all = self.mounts();
        let mut ids = BTreeSet::from([root]);
        let mut result = Vec::new();
        for m in all {
            if m.id == root
                || m.at
                    .lock_save_irq()
                    .as_ref()
                    .is_some_and(|(p, _)| ids.contains(&p.id))
            {
                ids.insert(m.id);
                result.push(m);
            }
        }
        result
    }
    pub fn change_propagation(&self, path: &VfsPath, kind: u64, recursive: bool) -> Result<()> {
        let _guard = MOUNT_OPS.lock_save_irq();
        if !self.contains(path) || !path.is_mount_root() {
            return Err(KernelError::InvalidValue);
        }
        let selected = if recursive {
            self.subtree(path.mount_id())
        } else {
            alloc::vec![path.mount.as_ref().unwrap().clone()]
        };
        let live = mounts_live();
        for m in selected {
            let mut p = m.propagation.lock_save_irq();
            match kind {
                0x100000 => p.shared(),
                0x80000 => {
                    if let Some(group) = p.peer.take() {
                        let peers = live.iter().any(|other| {
                            other.id != m.id
                                && other
                                    .propagation
                                    .lock_save_irq()
                                    .peer
                                    .as_ref()
                                    .is_some_and(|g| g.id == group.id)
                        });
                        p.master = if peers {
                            Some(group)
                        } else {
                            group.master.clone()
                        };
                    }
                }
                0x40000 => *p = Propagation::default(),
                0x20000 => {
                    *p = Propagation {
                        unbindable: true,
                        ..Default::default()
                    }
                }
                _ => return Err(KernelError::InvalidValue),
            }
        }
        Ok(())
    }
    pub fn remount(
        &self,
        path: &VfsPath,
        flags: u64,
        bind: bool,
        creds: &crate::process::creds::Credentials,
    ) -> Result<()> {
        let _guard = MOUNT_OPS.lock_save_irq();
        if !self.contains(path) || !path.is_mount_root() {
            return Err(KernelError::InvalidValue);
        }
        let mount = path.mount.as_ref().unwrap();
        if !bind {
            creds.check_capable_in(
                &mount.attrs.superblock.owner,
                libkernel::proc::caps::CapabilitiesFlags::CAP_SYS_ADMIN,
            )?;
        }
        mount.attrs.remount(flags, bind)
    }
    pub fn unmount(&self, path: &VfsPath, lazy: bool) -> Result<()> {
        let _guard = MOUNT_OPS.lock_save_irq();
        let m = path.mount.as_ref().ok_or(KernelError::InvalidValue)?;
        if !self.contains(path) || !path.is_mount_root() || m.locked {
            return Err(KernelError::InvalidValue);
        }
        let (parent, dentry) = m.at.lock_save_irq().clone().ok_or(FsError::Busy)?;
        let peer = parent.propagation.lock_save_irq().peer.clone();
        let mut removals =
            alloc::vec![(m.namespace().ok_or(KernelError::InvalidValue)?, m.clone())];
        if let Some(peer) = peer {
            for other in mounts_live() {
                if other.id == m.id || other.event != m.event {
                    continue;
                }
                let Some((p, d)) = other.at.lock_save_irq().clone() else {
                    continue;
                };
                if d.id != dentry.id || !p.propagation.lock_save_irq().receives(peer.id) {
                    continue;
                }
                if let Some(ns) = other.namespace() {
                    removals.push((ns, other));
                }
            }
        }
        for (ns, mount) in &removals {
            if ns
                .tree
                .lock_save_irq()
                .edges
                .keys()
                .any(|(p, _)| *p == mount.id)
            {
                return Err(if lazy {
                    KernelError::NotSupported
                } else {
                    FsError::Busy.into()
                });
            }
            let own_ref = usize::from(mount.id == m.id);
            if !lazy && mount.path_refs.load(Ordering::Acquire) > own_ref {
                return Err(FsError::Busy.into());
            }
        }
        for (ns, mount) in removals {
            let (p, d) = mount.detach().ok_or(KernelError::InvalidValue)?;
            let mut t = ns.tree.lock_save_irq();
            t.edges.remove(&(p.id, d.id));
            t.mounts.remove(&mount.id);
        }
        Ok(())
    }
    pub fn mounts(&self) -> Vec<Arc<Mount>> {
        self.tree.lock_save_irq().mounts.values().cloned().collect()
    }
}
/// Map the destination parent's master chain to the corresponding new child's
/// chain, retaining shared+slave relationships rather than flattening slaves.
fn replica_group(
    parent: &Arc<PeerGroup>,
    groups: &mut BTreeMap<u64, Arc<PeerGroup>>,
) -> Arc<PeerGroup> {
    if let Some(group) = groups.get(&parent.id) {
        return group.clone();
    }
    let mut chain = Vec::new();
    let mut current = Some(parent.clone());
    while let Some(g) = current {
        if groups.contains_key(&g.id) {
            break;
        }
        current = g.master.clone();
        chain.push(g);
    }
    for g in chain.into_iter().rev() {
        let master = g.master.as_ref().and_then(|m| groups.get(&m.id)).cloned();
        groups.insert(g.id, PeerGroup::new(master));
    }
    groups[&parent.id].clone()
}
impl Drop for MountNamespace {
    fn drop(&mut self) {
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
    fn remount_write_leases_are_mount_local_or_superblock_wide() {
        let (ns, _) = fixture();
        let root = ns.root();
        let target = child(&root, "alias");
        ns.bind(&root, &target, false).unwrap();
        let alias = MountNamespace::follow(target);
        let creds = crate::process::creds::Credentials::new_root();
        let lease = root.begin_write().unwrap();
        ns.remount(&alias, attributes::RDONLY, true, &creds)
            .unwrap();
        assert_eq!(alias.mount_flags() & attributes::RDONLY, attributes::RDONLY);
        assert_eq!(root.mount_flags() & attributes::RDONLY, 0);
        assert_eq!(
            ns.remount(&root, attributes::RDONLY, false, &creds),
            Err(FsError::Busy.into())
        );
        drop(lease);
        ns.remount(&root, attributes::RDONLY, false, &creds)
            .unwrap();
        assert!(matches!(root.begin_write(), Err(KernelError::ReadOnly)));
        ns.remount(&root, 0, false, &creds).unwrap();
        assert!(root.begin_write().is_ok());
    }

    #[ktest]
    fn shared_mount_copy_receives_events_until_made_private() {
        let (ns, fs) = fixture();
        let root = ns.root();
        ns.change_propagation(&root, 1 << 20, false).unwrap();
        let (copy, _) = ns.duplicate(UserNamespace::initial());
        let target = child(&root, "event");
        ns.attach(
            &target,
            fs.clone(),
            Dentry::root(Arc::new(crate::fs::DummyInode {})),
            "test",
        )
        .unwrap();
        assert_eq!(copy.mounts().len(), 2);
        copy.change_propagation(&copy.root(), 1 << 18, true)
            .unwrap();
        let next = child(&root, "private-event");
        ns.attach(
            &next,
            fs,
            Dentry::root(Arc::new(crate::fs::DummyInode {})),
            "test",
        )
        .unwrap();
        assert_eq!(ns.mounts().len(), 3);
        assert_eq!(copy.mounts().len(), 2);
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
