use crate::{
    drivers::Driver,
    fs::FilesystemDriver,
    process::{
        Tid, find_task_by_tid,
        thread_group::{Tgid, ThreadGroup, signal::SigId},
    },
    sched::current_work,
    sync::{OnceLock, SpinLock},
};
use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    format,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use async_trait::async_trait;
use core::any::Any;
use core::str;
use core::sync::atomic::{AtomicU64, Ordering};
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{
        BlockDevice, CGROUPFS_ID, DirStream, Dirent, FileType, Filesystem, Inode, InodeId,
        SimpleDirStream,
        attr::{FileAttr, FilePermissions},
    },
};
use log::warn;

const CGROUP2_MAGIC: u64 = 0x63677270;
const AVAILABLE_CONTROLLERS: &[&str] = &[];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CgroupFileKind {
    Type,
    Procs,
    Threads,
    Controllers,
    SubtreeControl,
    Events,
    MaxDescendants,
    MaxDepth,
    Stat,
    Freeze,
    Kill,
}

impl CgroupFileKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Type => "cgroup.type",
            Self::Procs => "cgroup.procs",
            Self::Threads => "cgroup.threads",
            Self::Controllers => "cgroup.controllers",
            Self::SubtreeControl => "cgroup.subtree_control",
            Self::Events => "cgroup.events",
            Self::MaxDescendants => "cgroup.max.descendants",
            Self::MaxDepth => "cgroup.max.depth",
            Self::Stat => "cgroup.stat",
            Self::Freeze => "cgroup.freeze",
            Self::Kill => "cgroup.kill",
        }
    }

    const fn all_for_root() -> &'static [Self] {
        &[
            Self::Controllers,
            Self::MaxDepth,
            Self::MaxDescendants,
            Self::Procs,
            Self::Stat,
            Self::SubtreeControl,
            Self::Threads,
        ]
    }

    const fn all_for_non_root() -> &'static [Self] {
        &[
            Self::Controllers,
            Self::Events,
            Self::Freeze,
            Self::Kill,
            Self::MaxDepth,
            Self::MaxDescendants,
            Self::Procs,
            Self::Stat,
            Self::SubtreeControl,
            Self::Threads,
            Self::Type,
        ]
    }

    const fn permissions(self) -> FilePermissions {
        match self {
            Self::Kill => FilePermissions::from_bits_retain(0o200),
            Self::Controllers | Self::Events | Self::Stat => {
                FilePermissions::from_bits_retain(0o444)
            }
            Self::Type
            | Self::Procs
            | Self::Threads
            | Self::SubtreeControl
            | Self::MaxDescendants
            | Self::MaxDepth
            | Self::Freeze => FilePermissions::from_bits_retain(0o644),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum CgroupLimit {
    #[default]
    Max,
    Value(u64),
}

impl CgroupLimit {
    fn parse(value: &str) -> Result<Self> {
        if value == "max" {
            return Ok(Self::Max);
        }

        Ok(Self::Value(
            value
                .parse::<u64>()
                .map_err(|_| KernelError::InvalidValue)?,
        ))
    }

    const fn allows(self, value: u64) -> bool {
        match self {
            Self::Max => true,
            Self::Value(limit) => value <= limit,
        }
    }

    fn as_string(self) -> String {
        match self {
            Self::Max => "max".to_string(),
            Self::Value(value) => value.to_string(),
        }
    }
}

#[derive(Default)]
struct CgroupNodeState {
    frozen: bool,
    max_descendants: CgroupLimit,
    max_depth: CgroupLimit,
    subtree_control: BTreeSet<&'static str>,
}

struct CgroupDirInode {
    id: InodeId,
    name: String,
    self_ref: Weak<CgroupDirInode>,
    parent: SpinLock<Option<Weak<CgroupDirInode>>>,
    children: SpinLock<BTreeMap<String, Arc<CgroupDirInode>>>,
    state: SpinLock<CgroupNodeState>,
}

impl CgroupDirInode {
    fn new_root() -> Arc<Self> {
        Arc::new_cyclic(|weak_self| Self {
            id: InodeId::from_fsid_and_inodeid(CGROUPFS_ID, 1),
            name: String::new(),
            self_ref: weak_self.clone(),
            parent: SpinLock::new(None),
            children: SpinLock::new(BTreeMap::new()),
            state: SpinLock::new(CgroupNodeState::default()),
        })
    }

    fn new_child(id: InodeId, name: String, parent: &Arc<CgroupDirInode>) -> Arc<Self> {
        Arc::new_cyclic(|weak_self| Self {
            id,
            name,
            self_ref: weak_self.clone(),
            parent: SpinLock::new(Some(Arc::downgrade(parent))),
            children: SpinLock::new(BTreeMap::new()),
            state: SpinLock::new(CgroupNodeState::default()),
        })
    }

    fn is_root(&self) -> bool {
        self.parent.lock_save_irq().is_none()
    }

    fn arc(&self) -> Arc<Self> {
        self.self_ref
            .upgrade()
            .expect("cgroup directory inode lost self reference")
    }

    fn path(&self) -> String {
        if self.is_root() {
            return "/".to_string();
        }

        let mut components = Vec::new();
        let mut current = Some(self.arc());
        while let Some(node) = current {
            if !node.name.is_empty() {
                components.push(node.name.clone());
            }
            current = node.parent.lock_save_irq().as_ref().and_then(Weak::upgrade);
        }
        components.reverse();

        let mut path = String::from("/");
        path.push_str(&components.join("/"));
        path
    }

    fn is_same_or_descendant_of(&self, ancestor: &Arc<CgroupDirInode>) -> bool {
        let mut current = Some(self.arc());
        while let Some(node) = current {
            if node.id == ancestor.id {
                return true;
            }
            current = node.parent.lock_save_irq().as_ref().and_then(Weak::upgrade);
        }
        false
    }

    fn control_file_kinds(&self) -> &'static [CgroupFileKind] {
        if self.is_root() {
            CgroupFileKind::all_for_root()
        } else {
            CgroupFileKind::all_for_non_root()
        }
    }

    fn has_control_file(&self, name: &str) -> Option<CgroupFileKind> {
        self.control_file_kinds()
            .iter()
            .copied()
            .find(|kind| kind.name() == name)
    }

    fn file_inode_id(&self, kind: CgroupFileKind) -> InodeId {
        InodeId::from_fsid_and_inodeid(CGROUPFS_ID, (self.id.inode_id() << 8) | (kind as u64 + 1))
    }

    fn dir_attr(&self) -> FileAttr {
        FileAttr {
            id: self.id,
            file_type: FileType::Directory,
            permissions: FilePermissions::from_bits_retain(0o755),
            ..FileAttr::default()
        }
    }
}

#[async_trait]
impl Inode for CgroupDirInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.dir_attr())
    }

    async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
        if let Some(child) = self.children.lock_save_irq().get(name).cloned() {
            return Ok(child);
        }

        if let Some(kind) = self.has_control_file(name) {
            return Ok(Arc::new(CgroupControlInode::new(self.arc(), kind)));
        }

        Err(FsError::NotFound.into())
    }

    async fn create(
        &self,
        name: &str,
        file_type: FileType,
        _permissions: FilePermissions,
        _time: Option<core::time::Duration>,
    ) -> Result<Arc<dyn Inode>> {
        if file_type != FileType::Directory {
            return Err(KernelError::NotSupported);
        }

        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(FsError::InvalidInput.into());
        }

        if self.has_control_file(name).is_some() {
            return Err(FsError::AlreadyExists.into());
        }

        let this = self.arc();
        cgroupfs().check_create_constraints(&this)?;

        let mut children = self.children.lock_save_irq();
        if children.contains_key(name) {
            return Err(FsError::AlreadyExists.into());
        }

        let id = cgroupfs().alloc_inode_id();
        let child = CgroupDirInode::new_child(id, name.to_string(), &this);
        children.insert(name.to_string(), child.clone());

        Ok(child)
    }

    async fn unlink(&self, name: &str) -> Result<()> {
        let mut children = self.children.lock_save_irq();
        let child = children.get(name).cloned().ok_or(FsError::NotFound)?;

        if !child.children.lock_save_irq().is_empty() {
            return Err(FsError::DirectoryNotEmpty.into());
        }

        if cgroupfs().has_live_processes_direct(&child) {
            return Err(FsError::DirectoryNotEmpty.into());
        }

        children.remove(name);
        Ok(())
    }

    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        let mut entries = Vec::new();

        for kind in self.control_file_kinds() {
            entries.push(Dirent::new(
                kind.name().to_string(),
                self.file_inode_id(*kind),
                FileType::File,
                0,
            ));
        }

        for (name, child) in self.children.lock_save_irq().iter() {
            entries.push(Dirent::new(
                name.clone(),
                child.id(),
                FileType::Directory,
                0,
            ));
        }

        entries.sort_by(|left, right| left.name.cmp(&right.name));
        for (idx, entry) in entries.iter_mut().enumerate() {
            entry.offset = (idx + 1) as u64;
        }

        Ok(Box::new(SimpleDirStream::new(entries, start_offset)))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct CgroupControlInode {
    id: InodeId,
    node: Arc<CgroupDirInode>,
    kind: CgroupFileKind,
}

impl CgroupControlInode {
    fn new(node: Arc<CgroupDirInode>, kind: CgroupFileKind) -> Self {
        Self {
            id: node.file_inode_id(kind),
            node,
            kind,
        }
    }

    fn attr(&self) -> FileAttr {
        FileAttr {
            id: self.id,
            file_type: FileType::File,
            permissions: self.kind.permissions(),
            ..FileAttr::default()
        }
    }

    fn read_all(&self) -> Result<Vec<u8>> {
        let fs = cgroupfs();
        let data = match self.kind {
            CgroupFileKind::Type => {
                if self.node.is_root() {
                    return Err(FsError::NotFound.into());
                }
                b"domain\n".to_vec()
            }
            CgroupFileKind::Procs => {
                let mut out = String::new();
                for tgid in fs.direct_tgids(&self.node) {
                    out.push_str(&format!("{}\n", tgid.value()));
                }
                out.into_bytes()
            }
            CgroupFileKind::Threads => {
                let mut out = String::new();
                for tid in fs.direct_tids(&self.node) {
                    out.push_str(&format!("{}\n", tid.value()));
                }
                out.into_bytes()
            }
            CgroupFileKind::Controllers => {
                let mut out = AVAILABLE_CONTROLLERS.join(" ");
                out.push('\n');
                out.into_bytes()
            }
            CgroupFileKind::SubtreeControl => {
                let state = self.node.state.lock_save_irq();
                let mut out = state
                    .subtree_control
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" ");
                out.push('\n');
                out.into_bytes()
            }
            CgroupFileKind::Events => {
                if self.node.is_root() {
                    return Err(FsError::NotFound.into());
                }
                format!(
                    "populated {}\nfrozen {}\n",
                    u8::from(fs.has_live_processes_recursive(&self.node)),
                    u8::from(fs.is_effectively_frozen(&self.node)),
                )
                .into_bytes()
            }
            CgroupFileKind::MaxDescendants => {
                let state = self.node.state.lock_save_irq();
                format!("{}\n", state.max_descendants.as_string()).into_bytes()
            }
            CgroupFileKind::MaxDepth => {
                let state = self.node.state.lock_save_irq();
                format!("{}\n", state.max_depth.as_string()).into_bytes()
            }
            CgroupFileKind::Stat => format!(
                "nr_descendants {}\nnr_dying_descendants 0\n",
                fs.num_descendants(&self.node),
            )
            .into_bytes(),
            CgroupFileKind::Freeze => {
                if self.node.is_root() {
                    return Err(FsError::NotFound.into());
                }
                format!("{}\n", u8::from(fs.is_effectively_frozen(&self.node))).into_bytes()
            }
            CgroupFileKind::Kill => return Err(KernelError::NotSupported),
        };

        Ok(data)
    }

    fn parse_single_value<'a>(&self, buf: &'a [u8]) -> Result<&'a str> {
        if buf.len() > 4096 {
            return Err(KernelError::TooLarge);
        }

        str::from_utf8(buf)
            .map(str::trim)
            .map_err(|_| KernelError::InvalidValue)
    }
}

#[async_trait]
impl Inode for CgroupControlInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let data = self.read_all()?;
        let start = offset as usize;
        if start >= data.len() {
            return Ok(0);
        }

        let end = usize::min(start + buf.len(), data.len());
        let slice = &data[start..end];
        buf[..slice.len()].copy_from_slice(slice);
        Ok(slice.len())
    }

    async fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize> {
        if offset != 0 {
            return Err(KernelError::InvalidValue);
        }

        let value = self.parse_single_value(buf)?;

        match self.kind {
            CgroupFileKind::Type => {
                if self.node.is_root() {
                    return Err(FsError::NotFound.into());
                }
                match value {
                    "" | "domain" => {}
                    "threaded" => return Err(KernelError::NotSupported),
                    _ => return Err(KernelError::InvalidValue),
                }
            }
            CgroupFileKind::Procs => {
                let pid = if value == "0" {
                    current_work().process.tgid.value()
                } else {
                    value
                        .parse::<u32>()
                        .map_err(|_| KernelError::InvalidValue)?
                };
                let tgid = Tgid(pid);
                ThreadGroup::get(tgid).ok_or(FsError::NotFound)?;
                cgroupfs().move_thread_group(tgid, self.node.clone())?;
            }
            CgroupFileKind::Threads => {
                let tid = if value == "0" {
                    current_work().tid.value()
                } else {
                    value
                        .parse::<u32>()
                        .map_err(|_| KernelError::InvalidValue)?
                };
                let task = find_task_by_tid(Tid(tid)).ok_or(FsError::NotFound)?;
                cgroupfs().move_thread_group(task.process.tgid, self.node.clone())?;
            }
            CgroupFileKind::SubtreeControl => {
                let mut requested = self.node.state.lock_save_irq().subtree_control.clone();
                if !value.is_empty() {
                    for token in value.split_ascii_whitespace() {
                        if token.len() < 2 {
                            return Err(KernelError::InvalidValue);
                        }
                        let (op, controller_name) = token.split_at(1);
                        let controller = AVAILABLE_CONTROLLERS
                            .iter()
                            .copied()
                            .find(|controller| *controller == controller_name)
                            .ok_or(KernelError::InvalidValue)?;
                        match op {
                            "+" => {
                                requested.insert(controller);
                            }
                            "-" => {
                                requested.remove(controller);
                            }
                            _ => return Err(KernelError::InvalidValue),
                        }
                    }
                }

                self.node.state.lock_save_irq().subtree_control = requested;
            }
            CgroupFileKind::MaxDescendants => {
                self.node.state.lock_save_irq().max_descendants = CgroupLimit::parse(value)?;
            }
            CgroupFileKind::MaxDepth => {
                self.node.state.lock_save_irq().max_depth = CgroupLimit::parse(value)?;
            }
            CgroupFileKind::Freeze => {
                if self.node.is_root() {
                    return Err(FsError::NotFound.into());
                }
                self.node.state.lock_save_irq().frozen = match value {
                    "0" => false,
                    "1" => true,
                    _ => return Err(KernelError::InvalidValue),
                };
            }
            CgroupFileKind::Kill => {
                if self.node.is_root() || value != "1" {
                    return Err(KernelError::InvalidValue);
                }
                let victims = cgroupfs().subtree_tgids(&self.node);
                for tgid in victims {
                    if let Some(tg) = ThreadGroup::get(tgid) {
                        tg.deliver_signal(SigId::SIGKILL);
                    }
                }
            }
            CgroupFileKind::Controllers | CgroupFileKind::Events | CgroupFileKind::Stat => {
                return Err(KernelError::NotSupported);
            }
        }

        Ok(buf.len())
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attr())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct CgroupFs {
    root: Arc<CgroupDirInode>,
    next_inode_id: AtomicU64,
    memberships: SpinLock<BTreeMap<Tgid, Weak<CgroupDirInode>>>,
}

impl CgroupFs {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            root: CgroupDirInode::new_root(),
            next_inode_id: AtomicU64::new(2),
            memberships: SpinLock::new(BTreeMap::new()),
        })
    }

    fn alloc_inode_id(&self) -> InodeId {
        InodeId::from_fsid_and_inodeid(
            CGROUPFS_ID,
            self.next_inode_id.fetch_add(1, Ordering::SeqCst),
        )
    }

    fn inherited_node(&self, parent: Option<&Arc<ThreadGroup>>) -> Arc<CgroupDirInode> {
        parent
            .and_then(|tg| self.memberships.lock_save_irq().get(&tg.tgid).cloned())
            .and_then(|node| node.upgrade())
            .unwrap_or_else(|| self.root.clone())
    }

    fn register_thread_group(&self, tg: &Arc<ThreadGroup>, parent: Option<&Arc<ThreadGroup>>) {
        if tg.tgid.is_idle() {
            return;
        }

        let node = self.inherited_node(parent);
        self.memberships
            .lock_save_irq()
            .insert(tg.tgid, Arc::downgrade(&node));
    }

    fn unregister_thread_group(&self, tgid: Tgid) {
        self.memberships.lock_save_irq().remove(&tgid);
    }

    fn move_thread_group(&self, tgid: Tgid, destination: Arc<CgroupDirInode>) -> Result<()> {
        if tgid.is_idle() {
            return Err(KernelError::InvalidValue);
        }

        ThreadGroup::get(tgid).ok_or(FsError::NotFound)?;
        self.memberships
            .lock_save_irq()
            .insert(tgid, Arc::downgrade(&destination));
        Ok(())
    }

    fn direct_tgids(&self, node: &Arc<CgroupDirInode>) -> Vec<Tgid> {
        self.memberships
            .lock_save_irq()
            .iter()
            .filter_map(|(tgid, membership)| {
                let member = membership.upgrade()?;
                if member.id == node.id && ThreadGroup::get(*tgid).is_some() {
                    Some(*tgid)
                } else {
                    None
                }
            })
            .collect()
    }

    fn direct_tids(&self, node: &Arc<CgroupDirInode>) -> Vec<Tid> {
        let mut tids = Vec::new();
        for tgid in self.direct_tgids(node) {
            let Some(tg) = ThreadGroup::get(tgid) else {
                continue;
            };
            tids.extend(
                tg.tasks
                    .lock_save_irq()
                    .iter()
                    .filter_map(|(tid, task)| task.upgrade().map(|_| *tid)),
            );
        }
        tids
    }

    fn subtree_tgids(&self, node: &Arc<CgroupDirInode>) -> Vec<Tgid> {
        self.memberships
            .lock_save_irq()
            .iter()
            .filter_map(|(tgid, membership)| {
                let member = membership.upgrade()?;
                if member.is_same_or_descendant_of(node) && ThreadGroup::get(*tgid).is_some() {
                    Some(*tgid)
                } else {
                    None
                }
            })
            .collect()
    }

    fn has_live_processes_direct(&self, node: &Arc<CgroupDirInode>) -> bool {
        !self.direct_tgids(node).is_empty()
    }

    fn has_live_processes_recursive(&self, node: &Arc<CgroupDirInode>) -> bool {
        !self.subtree_tgids(node).is_empty()
    }

    fn is_effectively_frozen(&self, node: &Arc<CgroupDirInode>) -> bool {
        let mut current = Some(node.clone());
        while let Some(group) = current {
            if group.state.lock_save_irq().frozen {
                return true;
            }
            current = group
                .parent
                .lock_save_irq()
                .as_ref()
                .and_then(Weak::upgrade);
        }
        false
    }

    fn num_descendants(&self, node: &Arc<CgroupDirInode>) -> u64 {
        fn count(node: &Arc<CgroupDirInode>) -> u64 {
            let children = node.children.lock_save_irq();
            children.values().map(|child| 1 + count(child)).sum()
        }

        count(node)
    }

    fn check_create_constraints(&self, parent: &Arc<CgroupDirInode>) -> Result<()> {
        let mut distance = 1;
        let mut current = Some(parent.clone());

        while let Some(node) = current {
            let state = node.state.lock_save_irq();
            if !state.max_depth.allows(distance) {
                return Err(KernelError::InvalidValue);
            }
            if !state
                .max_descendants
                .allows(self.num_descendants(&node) + 1)
            {
                return Err(KernelError::InvalidValue);
            }
            drop(state);

            distance += 1;
            current = node.parent.lock_save_irq().as_ref().and_then(Weak::upgrade);
        }

        Ok(())
    }

    fn path_for_tgid(&self, tgid: Tgid) -> String {
        self.memberships
            .lock_save_irq()
            .get(&tgid)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| self.root.clone())
            .path()
    }
}

#[async_trait]
impl Filesystem for CgroupFs {
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        Ok(self.root.clone())
    }

    fn id(&self) -> u64 {
        CGROUPFS_ID
    }

    fn magic(&self) -> u64 {
        CGROUP2_MAGIC
    }
}

static CGROUPFS_INSTANCE: OnceLock<Arc<CgroupFs>> = OnceLock::new();

/// Initializes and/or returns the global singleton [`CgroupFs`] instance.
pub fn cgroupfs() -> Arc<CgroupFs> {
    CGROUPFS_INSTANCE
        .get_or_init(|| {
            log::info!("cgroupfs initialized");
            CgroupFs::new()
        })
        .clone()
}

pub fn register_thread_group(tg: &Arc<ThreadGroup>, parent: Option<&Arc<ThreadGroup>>) {
    cgroupfs().register_thread_group(tg, parent);
}

pub fn unregister_thread_group(tgid: Tgid) {
    cgroupfs().unregister_thread_group(tgid);
}

pub fn cgroup_path_for_thread_group(tgid: Tgid) -> String {
    cgroupfs().path_for_tgid(tgid)
}

pub struct CgroupFsDriver;

impl CgroupFsDriver {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Driver for CgroupFsDriver {
    fn name(&self) -> &'static str {
        "cgroupfs"
    }

    fn as_filesystem_driver(self: Arc<Self>) -> Option<Arc<dyn FilesystemDriver>> {
        Some(self)
    }
}

#[async_trait]
impl FilesystemDriver for CgroupFsDriver {
    async fn construct(
        &self,
        _fs_id: u64,
        device: Option<Box<dyn BlockDevice>>,
    ) -> Result<Arc<dyn Filesystem>> {
        if device.is_some() {
            warn!("cgroupfs should not be constructed with a block device");
            return Err(KernelError::InvalidValue);
        }
        Ok(cgroupfs())
    }
}
