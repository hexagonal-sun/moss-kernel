use crate::error::{KernelError, Result};

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct CapabilitiesFlags: u64 {
        const CAP_CHOWN = 1 << 0;
        const CAP_DAC_OVERRIDE = 1 << 1;
        const CAP_DAC_READ_SEARCH = 1 << 2;
        const CAP_FOWNER = 1 << 3;
        const CAP_FSETID = 1 << 4;
        const CAP_KILL = 1 << 5;
        const CAP_SETGID = 1 << 6;
        const CAP_SETUID = 1 << 7;
        const CAP_SETPCAP = 1 << 8;
        const CAP_LINUX_IMMUTABLE = 1 << 9;
        const CAP_NET_BIND_SERVICE = 1 << 10;
        const CAP_NET_BROADCAST = 1 << 11;
        const CAP_NET_ADMIN = 1 << 12;
        const CAP_NET_RAW = 1 << 13;
        const CAP_IPC_LOCK = 1 << 14;
        const CAP_IPC_OWNER = 1 << 15;
        const CAP_SYS_MODULE = 1 << 16;
        const CAP_SYS_RAWIO = 1 << 17;
        const CAP_SYS_CHROOT = 1 << 18;
        const CAP_SYS_PTRACE = 1 << 19;
        const CAP_SYS_PACCT = 1 << 20;
        const CAP_SYS_ADMIN = 1 << 21;
        const CAP_SYS_BOOT = 1 << 22;
        const CAP_SYS_NICE = 1 << 23;
        const CAP_SYS_RESOURCE = 1 << 24;
        const CAP_SYS_TIME = 1 << 25;
        const CAP_SYS_TTY_CONFIG = 1 << 26;
        const CAP_MKNOD = 1 << 27;
        const CAP_LEASE = 1 << 28;
        const CAP_AUDIT_WRITE = 1 << 29;
        const CAP_AUDIT_CONTROL = 1 << 30;
        const CAP_SETFCAP = 1 << 31;
        const CAP_MAC_OVERRIDE = 1 << 32;
        const CAP_MAC_ADMIN = 1 << 33;
        const CAP_SYSLOG = 1 << 34;
        const CAP_WAKE_ALARM = 1 << 35;
        const CAP_BLOCK_SUSPEND = 1 << 36;
        const CAP_AUDIT_READ = 1 << 37;
        const CAP_PERFMON = 1 << 38;
        const CAP_BPF = 1 << 39;
        const CAP_CHECKPOINT_RESTORE = 1 << 40;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    effective: CapabilitiesFlags,
    permitted: CapabilitiesFlags,
    inheritable: CapabilitiesFlags,
    ambient: CapabilitiesFlags,
    bounding: CapabilitiesFlags,
}

impl Capabilities {
    pub fn new(
        effective: CapabilitiesFlags,
        permitted: CapabilitiesFlags,
        inheritable: CapabilitiesFlags,
        ambient: CapabilitiesFlags,
        bounding: CapabilitiesFlags,
    ) -> Self {
        Self {
            effective,
            permitted,
            inheritable,
            ambient,
            bounding,
        }
    }

    pub fn new_root() -> Self {
        Self {
            effective: CapabilitiesFlags::all(),
            permitted: CapabilitiesFlags::all(),
            inheritable: CapabilitiesFlags::all(),
            ambient: CapabilitiesFlags::all(),
            bounding: CapabilitiesFlags::all(),
        }
    }

    pub fn new_empty() -> Self {
        Self {
            effective: CapabilitiesFlags::empty(),
            permitted: CapabilitiesFlags::empty(),
            inheritable: CapabilitiesFlags::empty(),
            ambient: CapabilitiesFlags::empty(),
            bounding: CapabilitiesFlags::empty(),
        }
    }

    /// Convenience method for creating capabilities with a single capability
    pub fn new_cap(cap: CapabilitiesFlags) -> Self {
        Self {
            effective: cap,
            permitted: cap,
            inheritable: cap,
            ambient: cap,
            bounding: cap,
        }
    }

    /// Set the publicly mutable fields on capabilities
    pub fn set_public(
        &mut self,
        caller_caps: Capabilities,
        effective: CapabilitiesFlags,
        permitted: CapabilitiesFlags,
        inheritable: CapabilitiesFlags,
    ) -> Result<()> {
        // permitted should be a subset of self.permitted, and effective should be a subset of permitted
        // inheritable should be a subset of self.bounding, or caller's effective should contain CAP_SETPCAP
        if !self.permitted.contains(permitted)
            || !permitted.contains(effective)
            || (!self.bounding.contains(inheritable)
                && !caller_caps
                    .effective
                    .contains(CapabilitiesFlags::CAP_SETPCAP))
        {
            return Err(KernelError::NotPermitted);
        }
        self.effective = effective;
        self.permitted = permitted;
        self.inheritable = inheritable;
        Ok(())
    }

    pub fn effective(&self) -> CapabilitiesFlags {
        self.effective
    }

    pub fn permitted(&self) -> CapabilitiesFlags {
        self.permitted
    }

    pub fn inheritable(&self) -> CapabilitiesFlags {
        self.inheritable
    }

    pub fn ambient(&self) -> CapabilitiesFlags {
        self.ambient
    }

    pub fn bounding(&self) -> CapabilitiesFlags {
        self.bounding
    }

    /// Checks if a capability is effective, as in if it can be used.
    pub fn is_capable(&self, cap: CapabilitiesFlags) -> bool {
        self.effective.contains(cap)
    }

    /// Shortcut for returning EPERM if a capability is not effective.
    pub fn check_capable(&self, cap: CapabilitiesFlags) -> Result<()> {
        if !self.effective.contains(cap) {
            Err(KernelError::NotPermitted)
        } else {
            Ok(())
        }
    }
}
