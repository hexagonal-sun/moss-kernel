//! A proc mount-list fd pins the target namespace and root captured at open.
use crate::{
    fs::{VfsPath, fops::FileOps, mount::MountNamespace, open_file::FileCtx},
    memory::uaccess::copy_to_user_slice,
    process::{Tid, find_task_by_tid},
};
use alloc::{boxed::Box, format, string::String, sync::Arc};
use async_trait::async_trait;
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{
        Inode, InodeId, SeekFrom,
        attr::{FileAttr, FilePermissions},
    },
    memory::address::UA,
};

pub struct MountsInode {
    pub tid: Tid,
    pub id: InodeId,
    pub info: bool,
}
#[async_trait]
impl Inode for MountsInode {
    fn id(&self) -> InodeId {
        self.id
    }
    async fn getattr(&self) -> Result<FileAttr> {
        Ok(FileAttr {
            id: self.id,
            permissions: FilePermissions::from_bits_retain(0o444),
            ..Default::default()
        })
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
pub fn open(inode: &dyn Inode) -> Result<Option<Box<dyn FileOps>>> {
    let Some(node) = inode.as_any().downcast_ref::<MountsInode>() else {
        return Ok(None);
    };
    let task = find_task_by_tid(node.tid).ok_or(FsError::NotFound)?;
    let ns = task.mount_ns();
    let root = task.fs().root.lock_save_irq().clone();
    Ok(Some(Box::new(MountsFile {
        ns,
        root,
        info: node.info,
    })))
}
struct MountsFile {
    ns: Arc<MountNamespace>,
    root: VfsPath,
    info: bool,
}
fn escape(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            ' ' => out.push_str("\\040"),
            '\t' => out.push_str("\\011"),
            '\n' => out.push_str("\\012"),
            '\\' => out.push_str("\\134"),
            c => out.push(c),
        }
    }
    out
}
impl MountsFile {
    fn contents(&self) -> String {
        let mut text = String::new();
        for mount in self.ns.mounts() {
            let source_root = if mount.id == self.root.mount_id() {
                self.root.dentry.clone()
            } else {
                mount.root.clone()
            };
            let target = if mount.id == self.root.mount_id() {
                "/".into()
            } else if let Some(path) = mount.path().relative_to(&self.root) {
                path
            } else {
                continue;
            };
            if self.info {
                let (shared, master, unbindable) = mount.propagation_ids();
                let mut propagation = String::new();
                if let Some(id) = shared {
                    propagation.push_str(&format!(" shared:{id}"));
                }
                if let Some(id) = master {
                    propagation.push_str(&format!(" master:{id}"));
                }
                if unbindable {
                    propagation.push_str(" unbindable");
                }
                let parent = mount.covered().map_or(mount.id, |p| p.mount_id());
                let mut fs_root = mount.root.clone();
                while let Some(parent) = fs_root.parent() {
                    fs_root = parent;
                }
                let source = VfsPath::new(None, source_root)
                    .relative_to(&VfsPath::new(None, fs_root))
                    .unwrap_or_else(|| "/".into());
                let dev = mount.fs.id();
                let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & 0xfffff000);
                let minor = (dev & 0xff) | ((dev >> 12) & 0xffffff00);
                text.push_str(&format!(
                    "{} {} {}:{} {} {} {}{} - {} none {}\n",
                    mount.id,
                    parent,
                    major,
                    minor,
                    escape(source.as_str()),
                    escape(target.as_str()),
                    mount_options(mount.attrs.flags()),
                    propagation,
                    mount.fs_name,
                    if mount.attrs.superblock_readonly() {
                        "ro"
                    } else {
                        "rw"
                    }
                ));
            } else {
                text.push_str(&format!(
                    "none {} {} {} 0 0\n",
                    escape(target.as_str()),
                    mount.fs_name,
                    mount_options(mount.attrs.effective_flags())
                ));
            }
        }
        text
    }
}
fn mount_options(flags: u64) -> String {
    let mut text = String::from(if flags & 1 != 0 { "ro" } else { "rw" });
    for (bit, name) in [(2, ",nosuid"), (4, ",nodev"), (8, ",noexec")] {
        if flags & bit != 0 {
            text.push_str(name);
        }
    }
    text
}
#[async_trait]
impl FileOps for MountsFile {
    async fn readat(&mut self, buf: UA, count: usize, offset: u64) -> Result<usize> {
        let text = self.contents();
        let start = (offset as usize).min(text.len());
        let end = start.saturating_add(count).min(text.len());
        copy_to_user_slice(&text.as_bytes()[start..end], buf).await?;
        Ok(end - start)
    }
    async fn writeat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::InvalidValue)
    }
    async fn seek(&mut self, ctx: &mut FileCtx, pos: SeekFrom) -> Result<u64> {
        ctx.pos = match pos {
            SeekFrom::Start(n) if n <= i64::MAX as u64 => n,
            SeekFrom::Current(delta) => ctx
                .pos
                .checked_add_signed(delta)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or(KernelError::InvalidValue)?,
            _ => return Err(KernelError::InvalidValue),
        };
        Ok(ctx.pos)
    }
}
