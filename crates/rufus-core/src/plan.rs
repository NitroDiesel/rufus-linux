//! Immutable operation plans. Desktop validation is UX only; the helper revalidates.

use std::path::PathBuf;

use crate::device::DeviceFingerprint;
use crate::progress::JobId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PartitionScheme {
    Mbr,
    Gpt,
    SuperFloppy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BootMode {
    Bios,
    Uefi,
    Dual,
    NonBootable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FileSystem {
    Fat,
    Fat32,
    ExFat,
    Ntfs,
    Udf,
    Ext2,
    Ext3,
    Ext4,
    Refs,
}

impl FileSystem {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fat => "FAT",
            Self::Fat32 => "FAT32",
            Self::ExFat => "exFAT",
            Self::Ntfs => "NTFS",
            Self::Udf => "UDF",
            Self::Ext2 => "ext2",
            Self::Ext3 => "ext3",
            Self::Ext4 => "ext4",
            Self::Refs => "ReFS",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "fat" | "fat12" | "fat16" => Some(Self::Fat),
            "fat32" | "vfat" => Some(Self::Fat32),
            "exfat" => Some(Self::ExFat),
            "ntfs" => Some(Self::Ntfs),
            "udf" => Some(Self::Udf),
            "ext2" => Some(Self::Ext2),
            "ext3" => Some(Self::Ext3),
            "ext4" => Some(Self::Ext4),
            "refs" => Some(Self::Refs),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum WriteMode {
    /// Sector-for-sector image write (DD / ISOHybrid disk-image path).
    DdImage,
    /// File-copy ISO extraction with bootloader installation.
    IsoFileCopy,
    /// Format only (no image source).
    FormatOnly,
    /// FreeDOS system files on a FAT volume.
    FreeDos,
    /// Windows To Go apply path.
    WindowsToGo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ImageSourceKind {
    None,
    Iso,
    IsoHybrid,
    Raw,
    CompressedRaw,
    Vhd,
    Vhdx,
    Wim,
    Esd,
    Ffu,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ImageSource {
    pub path: PathBuf,
    pub kind: ImageSourceKind,
    pub size_bytes: u64,
    pub decompressed_size_bytes: Option<u64>,
    pub sha256: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PartitionPlan {
    pub scheme: PartitionScheme,
    pub boot_mode: BootMode,
    pub filesystem: FileSystem,
    pub label: String,
    pub cluster_size: Option<u32>,
    /// Extra Linux persistence partition size in bytes (0 = none).
    pub persistence_bytes: u64,
    pub quick_format: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum VerificationLevel {
    None,
    SizeOnly,
    FullReadback,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PlanStep {
    Authorize,
    InhibitAutomount,
    UnmountTarget,
    DisableSwap,
    BadBlocksCheck,
    WipeLeadingSectors {
        bytes: u64,
    },
    CreatePartitionTable {
        scheme: PartitionScheme,
    },
    CreatePartitions,
    Format {
        filesystem: FileSystem,
        label: String,
    },
    WriteImage {
        mode: WriteMode,
    },
    ExtractIsoFiles,
    InstallBootloader {
        name: String,
    },
    CreatePersistence {
        size_bytes: u64,
    },
    ApplyWindowsCustomization,
    Sync,
    Verify {
        level: VerificationLevel,
    },
    RereadPartitionTable,
    Eject,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OperationPlan {
    pub job_id: JobId,
    pub target_node: PathBuf,
    pub target_fingerprint: DeviceFingerprint,
    pub source: Option<ImageSource>,
    pub partition: PartitionPlan,
    pub write_mode: WriteMode,
    pub verification: VerificationLevel,
    pub steps: Vec<PlanStep>,
    /// User-visible action name used in confirmations.
    pub action_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanError {
    InvalidDevice(&'static str),
    IncompatibleOptions(String),
    UnavailableCapability(String),
    SourceMissing,
    SourceTooLarge,
    LabelInvalid(String),
    ArithmeticOverflow,
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDevice(msg) => write!(f, "invalid device: {msg}"),
            Self::IncompatibleOptions(msg) => write!(f, "incompatible options: {msg}"),
            Self::UnavailableCapability(msg) => write!(f, "unavailable: {msg}"),
            Self::SourceMissing => write!(f, "no image source selected"),
            Self::SourceTooLarge => write!(f, "image is larger than the target device"),
            Self::LabelInvalid(msg) => write!(f, "invalid volume label: {msg}"),
            Self::ArithmeticOverflow => write!(f, "size arithmetic overflow"),
        }
    }
}

impl std::error::Error for PlanError {}

impl OperationPlan {
    pub fn validate(&self) -> Result<(), PlanError> {
        self.target_fingerprint
            .validate()
            .map_err(PlanError::InvalidDevice)?;
        if self.steps.is_empty() {
            return Err(PlanError::IncompatibleOptions(
                "operation plan has no steps".into(),
            ));
        }
        if self.partition.label.chars().count() > 32 {
            return Err(PlanError::LabelInvalid(
                "label must be 32 characters or fewer".into(),
            ));
        }
        if self.partition.filesystem == FileSystem::Refs {
            return Err(PlanError::UnavailableCapability(
                "ReFS creation is not available on Linux".into(),
            ));
        }
        if matches!(
            self.write_mode,
            WriteMode::DdImage | WriteMode::IsoFileCopy | WriteMode::WindowsToGo
        ) && self.source.is_none()
        {
            return Err(PlanError::SourceMissing);
        }
        if let Some(source) = &self.source {
            let needed = source.decompressed_size_bytes.unwrap_or(source.size_bytes);
            if needed > self.target_fingerprint.size_bytes {
                return Err(PlanError::SourceTooLarge);
            }
        }
        if self.partition.persistence_bytes > 0 {
            let remaining = self
                .target_fingerprint
                .size_bytes
                .checked_sub(
                    self.source
                        .as_ref()
                        .map(|s| s.decompressed_size_bytes.unwrap_or(s.size_bytes))
                        .unwrap_or(0),
                )
                .ok_or(PlanError::ArithmeticOverflow)?;
            if self.partition.persistence_bytes > remaining {
                return Err(PlanError::IncompatibleOptions(
                    "persistence size exceeds free capacity".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Build a default step sequence from high-level options.
pub fn build_steps(
    write_mode: WriteMode,
    partition: &PartitionPlan,
    verification: VerificationLevel,
    bad_blocks: bool,
) -> Vec<PlanStep> {
    let mut steps = vec![
        PlanStep::Authorize,
        PlanStep::InhibitAutomount,
        PlanStep::UnmountTarget,
        PlanStep::DisableSwap,
    ];
    if bad_blocks {
        steps.push(PlanStep::BadBlocksCheck);
    }
    steps.push(PlanStep::WipeLeadingSectors {
        bytes: 8 * 1024 * 1024,
    });
    steps.push(PlanStep::CreatePartitionTable {
        scheme: partition.scheme,
    });
    steps.push(PlanStep::CreatePartitions);
    steps.push(PlanStep::Format {
        filesystem: partition.filesystem,
        label: partition.label.clone(),
    });
    match write_mode {
        WriteMode::DdImage => steps.push(PlanStep::WriteImage { mode: write_mode }),
        WriteMode::IsoFileCopy => {
            steps.push(PlanStep::ExtractIsoFiles);
            steps.push(PlanStep::InstallBootloader {
                name: "auto".into(),
            });
        }
        WriteMode::FormatOnly => {}
        WriteMode::FreeDos => {
            steps.push(PlanStep::InstallBootloader {
                name: "freedos".into(),
            });
        }
        WriteMode::WindowsToGo => {
            steps.push(PlanStep::WriteImage { mode: write_mode });
            steps.push(PlanStep::ApplyWindowsCustomization);
        }
    }
    if partition.persistence_bytes > 0 {
        steps.push(PlanStep::CreatePersistence {
            size_bytes: partition.persistence_bytes,
        });
    }
    steps.push(PlanStep::Sync);
    if verification != VerificationLevel::None {
        steps.push(PlanStep::Verify {
            level: verification,
        });
    }
    steps.push(PlanStep::RereadPartitionTable);
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{DeviceFingerprint, DeviceNumber};
    use crate::progress::JobId;
    use std::path::PathBuf;

    fn sample_plan() -> OperationPlan {
        OperationPlan {
            job_id: JobId::new(1),
            target_node: PathBuf::from("/dev/sdb"),
            target_fingerprint: DeviceFingerprint {
                number: DeviceNumber::new(8, 16),
                canonical_sysfs_path: PathBuf::from("/sys/devices/example"),
                size_bytes: 32 * 1024 * 1024 * 1024,
                logical_block_size: 512,
                serial: Some("ABC".into()),
                wwn: None,
            },
            source: Some(ImageSource {
                path: PathBuf::from("/tmp/test.iso"),
                kind: ImageSourceKind::Iso,
                size_bytes: 700 * 1024 * 1024,
                decompressed_size_bytes: None,
                sha256: None,
            }),
            partition: PartitionPlan {
                scheme: PartitionScheme::Gpt,
                boot_mode: BootMode::Uefi,
                filesystem: FileSystem::Fat32,
                label: "RUFUS".into(),
                cluster_size: None,
                persistence_bytes: 0,
                quick_format: true,
            },
            write_mode: WriteMode::IsoFileCopy,
            verification: VerificationLevel::FullReadback,
            steps: build_steps(
                WriteMode::IsoFileCopy,
                &PartitionPlan {
                    scheme: PartitionScheme::Gpt,
                    boot_mode: BootMode::Uefi,
                    filesystem: FileSystem::Fat32,
                    label: "RUFUS".into(),
                    cluster_size: None,
                    persistence_bytes: 0,
                    quick_format: true,
                },
                VerificationLevel::FullReadback,
                false,
            ),
            action_name: "Write image".into(),
        }
    }

    #[test]
    fn valid_plan_passes() {
        assert!(sample_plan().validate().is_ok());
    }

    #[test]
    fn refs_is_rejected() {
        let mut plan = sample_plan();
        plan.partition.filesystem = FileSystem::Refs;
        assert!(matches!(
            plan.validate(),
            Err(PlanError::UnavailableCapability(_))
        ));
    }

    #[test]
    fn oversized_source_is_rejected() {
        let mut plan = sample_plan();
        if let Some(source) = plan.source.as_mut() {
            source.size_bytes = plan.target_fingerprint.size_bytes + 1;
        }
        assert_eq!(plan.validate(), Err(PlanError::SourceTooLarge));
    }
}
