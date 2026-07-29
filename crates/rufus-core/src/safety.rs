//! Safety policy for target eligibility and destructive confirmations.

use crate::device::{BlockDevice, DeviceRisk};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SafetyError {
    ContainsRoot,
    ContainsBoot,
    ContainsSwap,
    MountedChildren,
    ActiveRaidMember,
    ActiveVolumeMember,
    ReadOnly,
    HasDependents,
    InternalDiskHidden,
    IdentityUnstable,
    ZeroSize,
    SourceOnTarget,
    Custom(String),
}

impl std::fmt::Display for SafetyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ContainsRoot => write!(f, "device contains the root filesystem"),
            Self::ContainsBoot => write!(f, "device contains a boot partition"),
            Self::ContainsSwap => write!(f, "device has active swap"),
            Self::MountedChildren => write!(
                f,
                "device has mounted partitions that could not be released"
            ),
            Self::ActiveRaidMember => write!(f, "device is an active RAID member"),
            Self::ActiveVolumeMember => {
                write!(f, "device is an active LVM or device-mapper member")
            }
            Self::ReadOnly => write!(f, "device is read-only"),
            Self::HasDependents => write!(f, "device has active dependents"),
            Self::InternalDiskHidden => {
                write!(f, "internal disks are hidden unless expert mode is enabled")
            }
            Self::IdentityUnstable => write!(f, "device identity is unstable or incomplete"),
            Self::ZeroSize => write!(f, "device reports zero size"),
            Self::SourceOnTarget => write!(f, "selected image is stored on the target device"),
            Self::Custom(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for SafetyError {}

impl From<DeviceRisk> for SafetyError {
    fn from(risk: DeviceRisk) -> Self {
        match risk {
            DeviceRisk::ContainsRoot => Self::ContainsRoot,
            DeviceRisk::ContainsBoot => Self::ContainsBoot,
            DeviceRisk::ContainsSwap => Self::ContainsSwap,
            DeviceRisk::MountedChildren => Self::MountedChildren,
            DeviceRisk::ActiveRaidMember => Self::ActiveRaidMember,
            DeviceRisk::ActiveVolumeMember => Self::ActiveVolumeMember,
            DeviceRisk::ReadOnly => Self::ReadOnly,
            DeviceRisk::HasDependents => Self::HasDependents,
            DeviceRisk::Internal => Self::InternalDiskHidden,
            DeviceRisk::IdentityUnstable => Self::IdentityUnstable,
        }
    }
}

/// Snapshot of safety-relevant flags for a single operation attempt.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SafetySnapshot {
    pub show_fixed_disks: bool,
    pub show_usb_hard_disks: bool,
    pub allow_zero_wipe: bool,
    pub allow_fast_zero: bool,
    pub allow_bad_blocks: bool,
    pub source_on_target: bool,
}

/// Policy that evaluates whether a device may be offered or written.
#[derive(Clone, Debug, Default)]
pub struct SafetyPolicy {
    pub show_fixed_disks: bool,
    pub show_usb_hard_disks: bool,
}

impl SafetyPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the device should appear in the device list at all.
    pub fn is_listed(&self, device: &BlockDevice) -> bool {
        if device.has_risk(DeviceRisk::ContainsRoot) || device.has_risk(DeviceRisk::ContainsBoot) {
            return false;
        }
        match device.class {
            crate::device::DeviceClass::Removable => true,
            crate::device::DeviceClass::ExternalFixed => self.show_usb_hard_disks,
            crate::device::DeviceClass::Internal => self.show_fixed_disks,
            crate::device::DeviceClass::Virtual => false,
            crate::device::DeviceClass::Unknown => self.show_fixed_disks,
        }
    }

    /// Hard reject list before privilege is requested.
    pub fn evaluate_target(
        &self,
        device: &BlockDevice,
        snapshot: &SafetySnapshot,
    ) -> Result<(), SafetyError> {
        device
            .validate()
            .map_err(|msg| SafetyError::Custom(msg.into()))?;

        if device.fingerprint.size_bytes == 0 {
            return Err(SafetyError::ZeroSize);
        }
        if snapshot.source_on_target {
            return Err(SafetyError::SourceOnTarget);
        }

        let blocking = [
            DeviceRisk::ContainsRoot,
            DeviceRisk::ContainsBoot,
            DeviceRisk::ContainsSwap,
            DeviceRisk::ActiveRaidMember,
            DeviceRisk::ActiveVolumeMember,
            DeviceRisk::ReadOnly,
            DeviceRisk::HasDependents,
            DeviceRisk::IdentityUnstable,
        ];
        for risk in blocking {
            if device.has_risk(risk) {
                return Err(risk.into());
            }
        }

        if device.has_risk(DeviceRisk::Internal) && !self.show_fixed_disks {
            return Err(SafetyError::InternalDiskHidden);
        }

        if device.has_risk(DeviceRisk::MountedChildren) {
            // Mounted children are OK to list; the helper must unmount them.
            // They only block if the planner cannot schedule unmount.
        }

        let _ = snapshot;
        Ok(())
    }

    /// Extra confirmations required beyond the primary destructive dialog.
    pub fn extra_confirmations(
        &self,
        device: &BlockDevice,
        snapshot: &SafetySnapshot,
        partition_count: usize,
    ) -> Vec<&'static str> {
        let mut items = Vec::new();
        if matches!(
            device.class,
            crate::device::DeviceClass::Internal | crate::device::DeviceClass::ExternalFixed
        ) {
            items.push("This is not a standard USB flash drive.");
        }
        if partition_count > 1 {
            items.push("Multiple partitions will be permanently removed.");
        }
        if snapshot.allow_bad_blocks {
            items.push("Bad-block testing will overwrite all data and take a long time.");
        }
        if snapshot.allow_zero_wipe || snapshot.allow_fast_zero {
            items.push("The device will be zeroed before formatting.");
        }
        items
    }
}

/// Format a human-readable confirmation that names the exact target.
pub fn confirmation_message(
    action: &str,
    display_name: &str,
    node: &str,
    capacity: &str,
    serial: Option<&str>,
) -> String {
    let serial = serial.unwrap_or("(serial unavailable)");
    format!(
        "{action} will permanently destroy all data on:\n\n\
         {display_name}\n\
         {node} · {capacity}\n\
         Serial: {serial}\n\n\
         This cannot be undone."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{
        BlockDevice, DeviceClass, DeviceFingerprint, DeviceNumber, DeviceRisk, Transport,
    };
    use std::path::PathBuf;

    fn device(class: DeviceClass, risks: Vec<DeviceRisk>) -> BlockDevice {
        BlockDevice {
            node: PathBuf::from("/dev/sdb"),
            display_name: "Test Stick".into(),
            vendor: Some("Test".into()),
            model: Some("Stick".into()),
            class,
            transport: Transport::Usb,
            fingerprint: DeviceFingerprint {
                number: DeviceNumber::new(8, 16),
                canonical_sysfs_path: PathBuf::from("/sys/devices/example"),
                size_bytes: 16 * 1024 * 1024 * 1024,
                logical_block_size: 512,
                serial: Some("SN".into()),
                wwn: None,
            },
            risks,
        }
    }

    #[test]
    fn root_device_is_never_listed() {
        let policy = SafetyPolicy::new();
        let dev = device(DeviceClass::Removable, vec![DeviceRisk::ContainsRoot]);
        assert!(!policy.is_listed(&dev));
    }

    #[test]
    fn internal_hidden_by_default() {
        let policy = SafetyPolicy::new();
        let dev = device(DeviceClass::Internal, vec![DeviceRisk::Internal]);
        assert!(!policy.is_listed(&dev));
        assert!(policy
            .evaluate_target(&dev, &SafetySnapshot::default())
            .is_err());
    }

    #[test]
    fn removable_usb_is_eligible() {
        let policy = SafetyPolicy::new();
        let dev = device(DeviceClass::Removable, vec![]);
        assert!(policy.is_listed(&dev));
        assert!(policy
            .evaluate_target(&dev, &SafetySnapshot::default())
            .is_ok());
    }
}
