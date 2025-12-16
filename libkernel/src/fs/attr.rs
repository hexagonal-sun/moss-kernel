use crate::{
    error::{KernelError, Result},
    proc::{
        caps::{Capabilities, CapabilitiesFlags},
        ids::{Gid, Uid},
    },
};

use super::{FileType, InodeId};
use bitflags::bitflags;
use core::time::Duration;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct AccessMode: i32 {
        /// Execution is permitted
        const X_OK = 1;
        /// Writing is permitted
        const W_OK = 2;
        /// Reading is permitted
        const R_OK = 4;
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct FilePermissions: u16 {
        const S_IXOTH = 0x0001;
        const S_IWOTH = 0x0002;
        const S_IROTH = 0x0004;

        const S_IXGRP = 0x0008;
        const S_IWGRP = 0x0010;
        const S_IRGRP = 0x0020;

        const S_IXUSR = 0x0040;
        const S_IWUSR = 0x0080;
        const S_IRUSR = 0x0100;

        const S_ISVTX = 0x0200;

        const S_ISGID = 0x0400;
        const S_ISUID = 0x0800;

        // Mutually-exclusive file types:
        const S_IFIFO = 0x1000;
        const S_IFCHR = 0x2000;
        const S_IFDIR = 0x4000;
        const S_IFBLK = 0x6000;
        const S_IFREG = 0x8000;
        const S_IFLNK = 0xA000;
        const S_IFSOCK = 0xC000;
    }
}

/// Represents file metadata, similar to `stat`.
#[derive(Debug, Clone)]
pub struct FileAttr {
    pub id: InodeId,
    pub size: u64,
    pub block_size: u32,
    pub blocks: u64,
    pub atime: Duration, // Access time (e.g., seconds since epoch)
    pub btime: Duration, // Creation time
    pub mtime: Duration, // Modification time
    pub ctime: Duration, // Change time
    pub file_type: FileType,
    pub mode: FilePermissions,
    pub nlinks: u32,
    pub uid: Uid,
    pub gid: Gid,
}

impl Default for FileAttr {
    fn default() -> Self {
        Self {
            id: InodeId::dummy(),
            size: 0,
            block_size: 0,
            blocks: 0,
            atime: Duration::new(0, 0),
            btime: Duration::new(0, 0),
            mtime: Duration::new(0, 0),
            ctime: Duration::new(0, 0),
            file_type: FileType::File,
            mode: FilePermissions::empty(),
            nlinks: 1,
            uid: Uid::new_root(),
            gid: Gid::new_root_group(),
        }
    }
}

impl FileAttr {
    /// Checks if a given set of credentials has the requested access permissions for this file.
    ///
    /// # Arguments
    /// * `uid` - The user-ID that will be checked against this file's uid field.
    /// * `gid` - The group-ID that will be checked against this file's uid field.
    /// * `caps` - The capabilities of the user.
    /// * `requested_mode` - A bitmask of `AccessMode` flags (`R_OK`, `W_OK`, `X_OK`) to check.
    pub fn check_access(
        &self,
        uid: Uid,
        gid: Gid,
        caps: Capabilities,
        requested_mode: AccessMode,
    ) -> Result<()> {
        // For filesystem related tasks, the CAP_DAC_OVERRIDE bypasses all permission checks.
        if caps.is_capable(CapabilitiesFlags::CAP_DAC_OVERRIDE) {
            return Ok(());
        }

        // root (UID 0) bypasses most permission checks. For execute, at
        // least one execute bit must be set.
        if uid.is_root() {
            if requested_mode.contains(AccessMode::X_OK) {
                // Root still needs at least one execute bit to be set for X_OK
                if self.mode.intersects(
                    FilePermissions::S_IXUSR | FilePermissions::S_IXGRP | FilePermissions::S_IXOTH,
                ) {
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        }

        // Determine which set of permission bits to use (owner, group, or other)
        let perms_to_check = if self.uid == uid {
            // User is the owner
            self.mode
        } else if self.gid == gid {
            // User is in the file's group. Shift group bits to align with owner bits for easier checking.
            FilePermissions::from_bits_truncate(self.mode.bits() << 3)
        } else {
            // Others. Shift other bits to align with owner bits.
            FilePermissions::from_bits_truncate(self.mode.bits() << 6)
        };

        if requested_mode.contains(AccessMode::R_OK)
            && !perms_to_check.contains(FilePermissions::S_IRUSR)
            && !caps.is_capable(CapabilitiesFlags::CAP_DAC_READ_SEARCH)
        {
            return Err(KernelError::NotPermitted);
        }
        if requested_mode.contains(AccessMode::W_OK)
            && !perms_to_check.contains(FilePermissions::S_IWUSR)
        {
            return Err(KernelError::NotPermitted);
        }
        if requested_mode.contains(AccessMode::X_OK)
            && !perms_to_check.contains(FilePermissions::S_IXUSR)
            && (self.file_type != FileType::Directory // CAP_DAC_READ_SEARCH allows directory search as well
                || !caps.is_capable(CapabilitiesFlags::CAP_DAC_READ_SEARCH))
        {
            return Err(KernelError::NotPermitted);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::KernelError;

    const ROOT_UID: Uid = Uid::new(0);
    const ROOT_GID: Gid = Gid::new(0);
    const OWNER_UID: Uid = Uid::new(1000);
    const OWNER_GID: Gid = Gid::new(1000);
    const GROUP_MEMBER_UID: Uid = Uid::new(1001);
    const FILE_GROUP_GID: Gid = Gid::new(2000);
    const OTHER_UID: Uid = Uid::new(1002);
    const OTHER_GID: Gid = Gid::new(3000);

    fn setup_file(mode: FilePermissions) -> FileAttr {
        FileAttr {
            uid: OWNER_UID,
            gid: FILE_GROUP_GID,
            mode,
            ..Default::default()
        }
    }

    #[test]
    fn root_can_read_without_perms() {
        let file = setup_file(FilePermissions::empty());
        assert!(
            file.check_access(
                ROOT_UID,
                ROOT_GID,
                Capabilities::new_empty(),
                AccessMode::R_OK
            )
            .is_ok()
        );
    }

    #[test]
    fn root_can_write_without_perms() {
        let file = setup_file(FilePermissions::empty());
        assert!(
            file.check_access(
                ROOT_UID,
                ROOT_GID,
                Capabilities::new_empty(),
                AccessMode::W_OK
            )
            .is_ok()
        );
    }

    #[test]
    fn root_cannot_execute_if_no_exec_bits_are_set() {
        let file = setup_file(FilePermissions::S_IRUSR | FilePermissions::S_IWUSR);
        let result = file.check_access(
            ROOT_UID,
            ROOT_GID,
            Capabilities::new_empty(),
            AccessMode::X_OK,
        );
        assert!(matches!(result, Err(KernelError::NotPermitted)));
    }

    #[test]
    fn root_can_execute_if_owner_exec_bit_is_set() {
        let file = setup_file(FilePermissions::S_IXUSR);
        assert!(
            file.check_access(
                ROOT_UID,
                ROOT_GID,
                Capabilities::new_empty(),
                AccessMode::X_OK
            )
            .is_ok()
        );
    }

    #[test]
    fn root_can_execute_if_group_exec_bit_is_set() {
        let file = setup_file(FilePermissions::S_IXGRP);
        assert!(
            file.check_access(
                ROOT_UID,
                ROOT_GID,
                Capabilities::new_empty(),
                AccessMode::X_OK
            )
            .is_ok()
        );
    }

    #[test]
    fn root_can_execute_if_other_exec_bit_is_set() {
        let file = setup_file(FilePermissions::S_IXOTH);
        assert!(
            file.check_access(
                ROOT_UID,
                ROOT_GID,
                Capabilities::new_empty(),
                AccessMode::X_OK
            )
            .is_ok()
        );
    }

    #[test]
    fn owner_can_read_when_permitted() {
        let file = setup_file(FilePermissions::S_IRUSR);
        assert!(
            file.check_access(
                OWNER_UID,
                OWNER_GID,
                Capabilities::new_empty(),
                AccessMode::R_OK
            )
            .is_ok()
        );
    }

    #[test]
    fn owner_cannot_read_when_denied() {
        let file = setup_file(FilePermissions::S_IWUSR | FilePermissions::S_IXUSR);
        let result = file.check_access(
            OWNER_UID,
            OWNER_GID,
            Capabilities::new_empty(),
            AccessMode::R_OK,
        );
        assert!(matches!(result, Err(KernelError::NotPermitted)));
    }

    #[test]
    fn owner_can_write_when_permitted() {
        let file = setup_file(FilePermissions::S_IWUSR);
        assert!(
            file.check_access(
                OWNER_UID,
                OWNER_GID,
                Capabilities::new_empty(),
                AccessMode::W_OK
            )
            .is_ok()
        );
    }

    #[test]
    fn owner_cannot_write_when_denied() {
        let file = setup_file(FilePermissions::S_IRUSR);
        let result = file.check_access(
            OWNER_UID,
            OWNER_GID,
            Capabilities::new_empty(),
            AccessMode::W_OK,
        );
        assert!(matches!(result, Err(KernelError::NotPermitted)));
    }

    #[test]
    fn owner_can_read_write_execute_when_permitted() {
        let file = setup_file(
            FilePermissions::S_IRUSR | FilePermissions::S_IWUSR | FilePermissions::S_IXUSR,
        );
        let mode = AccessMode::R_OK | AccessMode::W_OK | AccessMode::X_OK;
        assert!(
            file.check_access(OWNER_UID, OWNER_GID, Capabilities::new_empty(), mode)
                .is_ok()
        );
    }

    #[test]
    fn owner_access_denied_if_one_of_many_perms_is_missing() {
        let file = setup_file(FilePermissions::S_IRUSR | FilePermissions::S_IXUSR);
        let mode = AccessMode::R_OK | AccessMode::W_OK | AccessMode::X_OK; // Requesting Write is denied
        let result = file.check_access(OWNER_UID, OWNER_GID, Capabilities::new_empty(), mode);
        assert!(matches!(result, Err(KernelError::NotPermitted)));
    }

    #[test]
    fn group_member_can_read_when_permitted() {
        let file = setup_file(FilePermissions::S_IRGRP);
        assert!(
            file.check_access(
                GROUP_MEMBER_UID,
                FILE_GROUP_GID,
                Capabilities::new_empty(),
                AccessMode::R_OK
            )
            .is_ok()
        );
    }

    #[test]
    fn group_member_cannot_write_when_owner_can() {
        let file = setup_file(FilePermissions::S_IWUSR | FilePermissions::S_IRGRP);
        let result = file.check_access(
            GROUP_MEMBER_UID,
            FILE_GROUP_GID,
            Capabilities::new_empty(),
            AccessMode::W_OK,
        );
        assert!(matches!(result, Err(KernelError::NotPermitted)));
    }

    #[test]
    fn group_member_cannot_read_when_denied() {
        let file = setup_file(FilePermissions::S_IWGRP);
        let result = file.check_access(
            GROUP_MEMBER_UID,
            FILE_GROUP_GID,
            Capabilities::new_empty(),
            AccessMode::R_OK,
        );
        assert!(matches!(result, Err(KernelError::NotPermitted)));
    }

    #[test]
    fn other_can_execute_when_permitted() {
        let file = setup_file(FilePermissions::S_IXOTH);
        assert!(
            file.check_access(
                OTHER_UID,
                OTHER_GID,
                Capabilities::new_empty(),
                AccessMode::X_OK
            )
            .is_ok()
        );
    }

    #[test]
    fn other_cannot_read_when_only_owner_and_group_can() {
        let file = setup_file(FilePermissions::S_IRUSR | FilePermissions::S_IRGRP);
        let result = file.check_access(
            OTHER_UID,
            OTHER_GID,
            Capabilities::new_empty(),
            AccessMode::R_OK,
        );
        assert!(matches!(result, Err(KernelError::NotPermitted)));
    }

    #[test]
    fn other_cannot_write_when_denied() {
        let file = setup_file(FilePermissions::S_IROTH);
        let result = file.check_access(
            OTHER_UID,
            OTHER_GID,
            Capabilities::new_empty(),
            AccessMode::W_OK,
        );
        assert!(matches!(result, Err(KernelError::NotPermitted)));
    }

    #[test]
    fn no_requested_mode_is_always_ok() {
        // Checking for nothing should always succeed if the file exists.
        let file = setup_file(FilePermissions::empty());
        assert!(
            file.check_access(
                OTHER_UID,
                OTHER_GID,
                Capabilities::new_empty(),
                AccessMode::empty()
            )
            .is_ok()
        );
    }

    #[test]
    fn user_in_different_group_is_treated_as_other() {
        let file = setup_file(FilePermissions::S_IROTH); // Only other can read
        // This user is not the owner and not in the file's group.
        assert!(
            file.check_access(
                GROUP_MEMBER_UID,
                OTHER_GID,
                Capabilities::new_empty(),
                AccessMode::R_OK
            )
            .is_ok()
        );
    }

    #[test]
    fn cap_dac_override_can_read_write_exec_without_perms() {
        let file = setup_file(FilePermissions::empty());
        let mode = AccessMode::R_OK | AccessMode::W_OK | AccessMode::X_OK;
        assert!(
            file.check_access(
                ROOT_UID,
                ROOT_GID,
                Capabilities::new_cap(CapabilitiesFlags::CAP_DAC_OVERRIDE),
                mode,
            )
            .is_ok()
        );
    }

    #[test]
    fn cap_dac_read_search_can_read_without_perms() {
        let file = setup_file(FilePermissions::empty());
        assert!(
            file.check_access(
                OTHER_UID,
                OTHER_GID,
                Capabilities::new_cap(CapabilitiesFlags::CAP_DAC_READ_SEARCH),
                AccessMode::R_OK,
            )
            .is_ok()
        );
    }

    #[test]
    fn cap_dac_read_search_cannot_write_without_perms() {
        let file = setup_file(FilePermissions::empty());
        let result = file.check_access(
            OTHER_UID,
            OTHER_GID,
            Capabilities::new_cap(CapabilitiesFlags::CAP_DAC_READ_SEARCH),
            AccessMode::W_OK,
        );
        assert!(matches!(result, Err(KernelError::NotPermitted)));
    }

    #[test]
    fn cap_dac_read_search_cannot_exec_without_perms() {
        let file = setup_file(FilePermissions::empty());
        let result = file.check_access(
            OTHER_UID,
            OTHER_GID,
            Capabilities::new_cap(CapabilitiesFlags::CAP_DAC_READ_SEARCH),
            AccessMode::X_OK,
        );
        assert!(matches!(result, Err(KernelError::NotPermitted)));
    }
}
