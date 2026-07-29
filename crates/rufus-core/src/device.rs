use std::fmt;
use std::path::{Path, PathBuf};

/// Kernel block-device number. Unlike `/dev/sdX`, this cannot change within a device lifetime.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceNumber {
    pub major: u32,
    pub minor: u32,
}

impl DeviceNumber {
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }
}

impl fmt::Display for DeviceNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.major, self.minor)
    }
}

/// Stable facts that must be re-read before the first destructive write.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceFingerprint {
    pub number: DeviceNumber,
    pub canonical_sysfs_path: PathBuf,
    pub size_bytes: u64,
    pub logical_block_size: u32,
    pub serial: Option<String>,
    pub wwn: Option<String>,
}

impl DeviceFingerprint {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.canonical_sysfs_path.is_absolute() {
            return Err("sysfs path must be absolute");
        }
        if self.size_bytes == 0 {
            return Err("device size must be non-zero");
        }
        if self.logical_block_size < 512 || !self.logical_block_size.is_power_of_two() {
            return Err("logical block size must be a power of two and at least 512 bytes");
        }
        Ok(())
    }

    /// Strict equality for a preflight/revalidation comparison.
    pub fn matches(&self, observed: &Self) -> bool {
        self == observed
    }

    pub fn has_persistent_id(&self) -> bool {
        self.wwn.as_ref().is_some_and(|value| !value.is_empty())
            || self.serial.as_ref().is_some_and(|value| !value.is_empty())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DeviceClass {
    Removable,
    ExternalFixed,
    Internal,
    Virtual,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Transport {
    Usb,
    Sd,
    Mmc,
    Nvme,
    Sata,
    Scsi,
    Virtio,
    Loop,
    Other,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DeviceRisk {
    ContainsRoot,
    ContainsBoot,
    ContainsSwap,
    MountedChildren,
    ActiveRaidMember,
    ActiveVolumeMember,
    ReadOnly,
    HasDependents,
    Internal,
    IdentityUnstable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BlockDevice {
    pub node: PathBuf,
    pub display_name: String,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub class: DeviceClass,
    pub transport: Transport,
    pub fingerprint: DeviceFingerprint,
    pub risks: Vec<DeviceRisk>,
}

impl BlockDevice {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.node.is_absolute() {
            return Err("device node must be absolute");
        }
        if self.display_name.trim().is_empty() {
            return Err("display name must not be empty");
        }
        self.fingerprint.validate()
    }

    pub fn is_node(&self, path: &Path) -> bool {
        self.node == path
    }

    pub fn has_risk(&self, risk: DeviceRisk) -> bool {
        self.risks.contains(&risk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_requires_sane_block_geometry() {
        let mut fingerprint = fingerprint();
        fingerprint.logical_block_size = 1000;
        assert_eq!(
            fingerprint.validate(),
            Err("logical block size must be a power of two and at least 512 bytes")
        );
    }

    #[test]
    fn revalidation_is_strict() {
        let expected = fingerprint();
        let mut replacement = expected.clone();
        replacement.serial = Some("replacement".into());
        assert!(!expected.matches(&replacement));
    }

    fn fingerprint() -> DeviceFingerprint {
        DeviceFingerprint {
            number: DeviceNumber::new(8, 16),
            canonical_sysfs_path: PathBuf::from("/sys/devices/example"),
            size_bytes: 16 * 1024 * 1024,
            logical_block_size: 512,
            serial: Some("serial".into()),
            wwn: None,
        }
    }
}
