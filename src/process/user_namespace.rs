//! User namespaces keep userspace IDs separate from kernel (initial-namespace)
//! IDs. Maps are immutable once installed; credentials and inodes always store
//! kernel IDs, so entering a namespace never changes ownership of an object.
use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use super::creds::Credentials;
use crate::sync::{OnceLock, SpinLock};
use libkernel::{
    error::{KernelError, Result},
    proc::{
        caps::CapabilitiesFlags as Cap,
        ids::{Gid, Uid},
    },
};

pub const OVERFLOW_ID: u32 = 65534;
const MAX_EXTENTS: usize = 340;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MapKind {
    Uid,
    Gid,
}

#[derive(Clone, Copy)]
struct Extent {
    first: u32,
    lower: u32,
    count: u32,
}

struct Maps {
    uid: Vec<Extent>,
    gid: Vec<Extent>,
    allow_setgroups: bool,
}

pub struct UserNamespace {
    pub id: u64,
    pub parent: Option<Arc<Self>>,
    pub owner: Uid,
    level: u32,
    creator_could_setfcap: bool,
    maps: SpinLock<Maps>,
}

impl PartialEq for UserNamespace {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for UserNamespace {}

impl UserNamespace {
    pub fn initial() -> Arc<Self> {
        static INITIAL: OnceLock<Arc<UserNamespace>> = OnceLock::new();
        INITIAL
            .get_or_init(|| {
                Arc::new(Self {
                    id: 1,
                    parent: None,
                    owner: Uid::new_root(),
                    level: 0,
                    creator_could_setfcap: true,
                    maps: SpinLock::new(Maps {
                        uid: vec![Extent {
                            first: 0,
                            lower: 0,
                            count: u32::MAX,
                        }],
                        gid: vec![Extent {
                            first: 0,
                            lower: 0,
                            count: u32::MAX,
                        }],
                        allow_setgroups: true,
                    }),
                })
            })
            .clone()
    }

    pub fn create(creator: &Credentials) -> Result<Arc<Self>> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(2);
        let parent = creator.user_ns();
        if parent.level >= 32 {
            return Err(KernelError::NoSpace);
        }
        if parent.from_uid(creator.euid()).is_none() || parent.from_gid(creator.egid()).is_none() {
            return Err(KernelError::NotPermitted);
        }
        let allow_setgroups = parent.maps.lock_save_irq().allow_setgroups;
        Ok(Arc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            parent: Some(parent.clone()),
            owner: creator.euid(),
            level: parent.level + 1,
            creator_could_setfcap: creator.caps().is_capable(Cap::CAP_SETFCAP),
            maps: SpinLock::new(Maps {
                uid: Vec::new(),
                gid: Vec::new(),
                allow_setgroups,
            }),
        }))
    }

    fn map_range(&self, kind: MapKind, id: u32, count: u32, down: bool) -> Option<u32> {
        if count == 0 {
            return None;
        }
        id.checked_add(count)?;
        let maps = self.maps.lock_save_irq();
        let map = match kind {
            MapKind::Uid => &maps.uid,
            MapKind::Gid => &maps.gid,
        };
        map.iter().find_map(|e| {
            let (from, to) = if down {
                (e.first, e.lower)
            } else {
                (e.lower, e.first)
            };
            let off = id.checked_sub(from)?;
            if off.checked_add(count)? <= e.count {
                Some(to + off)
            } else {
                None
            }
        })
    }

    pub fn make_uid(&self, id: u32) -> Result<Uid> {
        self.map_range(MapKind::Uid, id, 1, true)
            .map(Uid::new)
            .ok_or(KernelError::InvalidValue)
    }
    pub fn make_gid(&self, id: u32) -> Result<Gid> {
        self.map_range(MapKind::Gid, id, 1, true)
            .map(Gid::new)
            .ok_or(KernelError::InvalidValue)
    }
    pub fn from_uid(&self, id: Uid) -> Option<u32> {
        self.map_range(MapKind::Uid, id.into(), 1, false)
    }
    pub fn from_gid(&self, id: Gid) -> Option<u32> {
        self.map_range(MapKind::Gid, id.into(), 1, false)
    }
    pub fn show_uid(&self, id: Uid) -> u32 {
        self.from_uid(id).unwrap_or(OVERFLOW_ID)
    }
    pub fn show_gid(&self, id: Gid) -> u32 {
        self.from_gid(id).unwrap_or(OVERFLOW_ID)
    }

    pub fn may_setgroups(&self) -> bool {
        let maps = self.maps.lock_save_irq();
        maps.allow_setgroups && !maps.gid.is_empty()
    }

    pub fn read_setgroups(&self) -> &'static str {
        if self.maps.lock_save_irq().allow_setgroups {
            "allow\n"
        } else {
            "deny\n"
        }
    }

    pub fn write_setgroups(
        &self,
        data: &[u8],
        offset: u64,
        opener: &Credentials,
        caller: &Credentials,
    ) -> Result<usize> {
        opener.check_capable_in(self, Cap::CAP_SYS_ADMIN)?;
        caller.check_capable_in(self, Cap::CAP_SYS_ADMIN)?;
        if offset != 0 || data.len() >= 8 {
            return Err(KernelError::InvalidValue);
        }
        let value = core::str::from_utf8(data)
            .map_err(|_| KernelError::InvalidValue)?
            .trim_end();
        let allow = match value {
            "allow" => true,
            "deny" => false,
            _ => return Err(KernelError::InvalidValue),
        };
        let mut maps = self.maps.lock_save_irq();
        if (allow && !maps.allow_setgroups) || (!allow && !maps.gid.is_empty()) {
            return Err(KernelError::NotPermitted);
        }
        maps.allow_setgroups = allow;
        Ok(data.len())
    }

    pub fn read_map(&self, kind: MapKind, viewer: &UserNamespace) -> String {
        // Reading one's own map reports IDs in the parent; other readers see
        // their own namespace. Clone before taking another namespace's lock.
        let extents = {
            let maps = self.maps.lock_save_irq();
            match kind {
                MapKind::Uid => maps.uid.clone(),
                MapKind::Gid => maps.gid.clone(),
            }
        };
        let viewer = if self == viewer {
            self.parent.as_deref().unwrap_or(viewer)
        } else {
            viewer
        };
        let mut result = String::new();
        for e in extents {
            let lower = viewer
                .map_range(kind, e.lower, e.count, false)
                .unwrap_or(u32::MAX);
            result.push_str(&format!("{:10} {:10} {:10}\n", e.first, lower, e.count));
        }
        result
    }

    pub fn write_map(
        &self,
        kind: MapKind,
        data: &[u8],
        offset: u64,
        opener: &Credentials,
        caller: &Credentials,
    ) -> Result<usize> {
        let parent = self.parent.as_ref().ok_or(KernelError::NotPermitted)?;
        let cap = match kind {
            MapKind::Uid => Cap::CAP_SETUID,
            MapKind::Gid => Cap::CAP_SETGID,
        };
        caller.check_capable_in(self, cap)?;
        opener.check_capable_in(self, cap)?;
        if opener.user_ns().as_ref() != self && opener.user_ns() != *parent {
            return Err(KernelError::NotPermitted);
        }
        if offset != 0 || data.len() >= 4096 {
            return Err(KernelError::InvalidValue);
        }
        let input = core::str::from_utf8(data).map_err(|_| KernelError::InvalidValue)?;
        let mut extents: Vec<Extent> = Vec::new();
        for line in input.lines() {
            let mut words = line.split_ascii_whitespace();
            let mut number = || {
                words
                    .next()
                    .and_then(|s| {
                        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                            None
                        } else {
                            s.parse::<u32>().ok()
                        }
                    })
                    .ok_or(KernelError::InvalidValue)
            };
            let e = Extent {
                first: number()?,
                lower: number()?,
                count: number()?,
            };
            if words.next().is_some()
                || e.count == 0
                || e.first.checked_add(e.count).is_none()
                || e.lower.checked_add(e.count).is_none()
                || extents.len() == MAX_EXTENTS
            {
                return Err(KernelError::InvalidValue);
            }
            for old in &extents {
                if (e.first < old.first + old.count && old.first < e.first + e.count)
                    || (e.lower < old.lower + old.count && old.lower < e.lower + e.count)
                {
                    return Err(KernelError::InvalidValue);
                }
            }
            extents.push(e);
        }
        if extents.is_empty() {
            return Err(KernelError::InvalidValue);
        }

        // Validate privileges and flatten parent ranges before locking this
        // namespace: no child->parent lock nesting and no partial installation.
        if kind == MapKind::Uid && extents.iter().any(|e| e.lower == 0) {
            let can_setfcap = if opener.user_ns().as_ref() == self {
                self.creator_could_setfcap
            } else {
                opener.capable_in(parent, Cap::CAP_SETFCAP)
            };
            if !can_setfcap {
                return Err(KernelError::NotPermitted);
            }
        }
        let privileged = opener.capable_in(parent, cap) && caller.capable_in(parent, cap);
        let own_id = match kind {
            MapKind::Uid => u32::from(opener.euid()),
            MapKind::Gid => u32::from(opener.egid()),
        };
        for e in &mut extents {
            e.lower = parent
                .map_range(kind, e.lower, e.count, true)
                .ok_or(KernelError::NotPermitted)?;
        }
        let mut maps = self.maps.lock_save_irq();
        let already_written = match kind {
            MapKind::Uid => !maps.uid.is_empty(),
            MapKind::Gid => !maps.gid.is_empty(),
        };
        if already_written {
            return Err(KernelError::NotPermitted);
        }
        if !privileged
            && (extents.len() != 1
                || extents[0].count != 1
                || extents[0].lower != own_id
                || opener.euid() != self.owner
                || (kind == MapKind::Gid && maps.allow_setgroups))
        {
            return Err(KernelError::NotPermitted);
        }
        match kind {
            MapKind::Uid => maps.uid = extents,
            MapKind::Gid => maps.gid = extents,
        };
        Ok(data.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libkernel::proc::caps::Capabilities;
    use moss_macros::ktest;

    #[ktest]
    fn capability_authority_is_directional() {
        let root = Credentials::new_root();
        let ns = UserNamespace::create(&root).unwrap();
        let mut child = root.clone();
        child.enter_user_ns(ns.clone());
        assert!(root.capable_in(&ns, Cap::CAP_SYS_ADMIN));
        assert!(!child.capable_in(&root.user_ns(), Cap::CAP_SYS_ADMIN));
        let sibling = UserNamespace::create(&root).unwrap();
        assert!(!child.capable_in(&sibling, Cap::CAP_SYS_ADMIN));
        let mut owner = root.clone();
        owner.caps = Capabilities::new_empty();
        assert!(owner.capable_in(&ns, Cap::CAP_SYS_ADMIN));
        assert!(!owner.capable_in(&owner.user_ns(), Cap::CAP_SYS_ADMIN));
    }

    #[ktest]
    fn maps_exclude_sentinel_and_install_atomically() {
        let root = Credentials::new_root();
        let ns = UserNamespace::create(&root).unwrap();
        assert_eq!(ns.show_uid(Uid::new_root()), OVERFLOW_ID);
        assert!(
            ns.write_map(MapKind::Uid, b"0 0 1\n0 2 1\n", 0, &root, &root)
                .is_err()
        );
        assert!(ns.read_map(MapKind::Uid, &root.user_ns()).is_empty());
        ns.write_map(MapKind::Uid, b"0 4294967293 2\n", 0, &root, &root)
            .unwrap();
        assert_eq!(u32::from(ns.make_uid(1).unwrap()), u32::MAX - 1);
        assert!(ns.make_uid(2).is_err());
        assert!(ns.make_uid(u32::MAX).is_err());
        assert_eq!(
            ns.write_map(MapKind::Uid, b"0 0 1\n", 0, &root, &root),
            Err(KernelError::NotPermitted)
        );
    }

    #[ktest]
    fn parent_root_mapping_requires_setfcap() {
        let mut root = Credentials::new_root();
        let flags = Cap::all() & !Cap::CAP_SETFCAP;
        root.caps = Capabilities::new(flags, flags, Cap::empty(), Cap::empty(), Cap::all());
        let ns = UserNamespace::create(&root).unwrap();
        let mut child = root.clone();
        child.enter_user_ns(ns.clone());
        assert_eq!(
            ns.write_map(MapKind::Uid, b"0 0 1\n", 0, &root, &root),
            Err(KernelError::NotPermitted)
        );
        assert_eq!(
            ns.write_map(MapKind::Uid, b"0 0 1\n", 0, &child, &child),
            Err(KernelError::NotPermitted)
        );
        ns.write_map(MapKind::Uid, b"0 1000 1\n", 0, &root, &root)
            .unwrap();
    }
}
