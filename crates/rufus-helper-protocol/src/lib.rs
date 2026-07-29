//! Narrow, versioned protocol between the unprivileged desktop and root helper.
//!
//! The helper must never accept shell fragments, relative paths, or environment-
//! controlled tool names. Messages are length-prefixed JSON on a Unix socket or pipe.

use std::path::PathBuf;

use rufus_core::device::{DeviceFingerprint, DeviceNumber};
use rufus_core::plan::{
    BootMode, FileSystem, ImageSourceKind, PartitionScheme, VerificationLevel, WriteMode,
};
use rufus_core::progress::{Cancellability, JobId, ProgressStage, ProgressUnit};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Bump on any incompatible wire change.
pub const PROTOCOL_VERSION: u32 = 1;

/// Default abstract/socket path under the root-owned runtime directory.
pub const DEFAULT_SOCKET_NAME: &str = "rufus-linux-helper.sock";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TargetIdentity {
    pub node: PathBuf,
    pub fingerprint: DeviceFingerprint,
    pub display_name: String,
    pub model: String,
    pub serial: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSpec {
    pub path: PathBuf,
    pub kind: ImageSourceKind,
    pub size_bytes: u64,
    pub decompressed_size_bytes: Option<u64>,
    pub expected_sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormatSpec {
    pub scheme: PartitionScheme,
    pub boot_mode: BootMode,
    pub filesystem: FileSystem,
    pub label: String,
    pub cluster_size: Option<u32>,
    pub persistence_bytes: u64,
    pub quick_format: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum HelperOperation {
    /// Write an image (DD or ISO file-copy) after partitioning/formatting.
    WriteMedia {
        write_mode: WriteMode,
        source: SourceSpec,
        format: FormatSpec,
        verification: VerificationLevel,
        bad_blocks: bool,
        install_bootloader: Option<String>,
    },
    /// Format only.
    FormatMedia {
        format: FormatSpec,
        bad_blocks: bool,
    },
    /// Capture the device to a raw image file (user-owned path via fd in future).
    CaptureImage {
        output: PathBuf,
        kind: ImageSourceKind,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperRequest {
    pub protocol_version: u32,
    pub job_id: JobId,
    pub target: TargetIdentity,
    pub operation: HelperOperation,
    pub action_name: String,
}

impl HelperRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ProtocolError::VersionMismatch {
                expected: PROTOCOL_VERSION,
                got: self.protocol_version,
            });
        }
        self.target
            .fingerprint
            .validate()
            .map_err(|msg| ProtocolError::InvalidRequest(msg.into()))?;
        if !self.target.node.is_absolute() {
            return Err(ProtocolError::InvalidRequest(
                "target node must be absolute".into(),
            ));
        }
        match &self.operation {
            HelperOperation::WriteMedia { source, .. } => {
                if !source.path.is_absolute() {
                    return Err(ProtocolError::InvalidRequest(
                        "source path must be absolute".into(),
                    ));
                }
            }
            HelperOperation::CaptureImage { output, .. } => {
                if !output.is_absolute() {
                    return Err(ProtocolError::InvalidRequest(
                        "output path must be absolute".into(),
                    ));
                }
            }
            HelperOperation::FormatMedia { .. } => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum HelperEvent {
    Accepted {
        job_id: JobId,
    },
    Progress {
        job_id: JobId,
        stage: ProgressStage,
        unit: ProgressUnit,
        completed: u64,
        total: Option<u64>,
        bytes_per_second: Option<u64>,
        detail: Option<String>,
        cancellability: Cancellability,
    },
    Log {
        job_id: JobId,
        line: String,
    },
    Finished {
        job_id: JobId,
        result: HelperResult,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum HelperResult {
    Success,
    Cancelled { message: String },
    Failed { code: String, message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ClientMessage {
    Cancel { job_id: JobId },
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("protocol version mismatch: expected {expected}, got {got}")]
    VersionMismatch { expected: u32, got: u32 },
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("encode/decode error: {0}")]
    Codec(String),
    #[error("io error: {0}")]
    Io(String),
}

/// Encode a message as a single newline-delimited JSON object.
pub fn encode_line<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
    let mut bytes = serde_json::to_vec(value).map_err(|e| ProtocolError::Codec(e.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Decode a single newline-terminated JSON object.
pub fn decode_line<T: for<'de> Deserialize<'de>>(line: &[u8]) -> Result<T, ProtocolError> {
    let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
    let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
    serde_json::from_slice(trimmed).map_err(|e| ProtocolError::Codec(e.to_string()))
}

// Serde support for core types used on the wire.
// These mirror definitions so the protocol crate can serialize without
// requiring every consumer to enable core's serde feature for local types.

mod serde_impls {
    use super::*;
    use serde::{Deserialize, Serialize};

    // Re-export through wrapper if needed later.
    #[allow(dead_code)]
    #[derive(Serialize, Deserialize)]
    struct DeviceNumberWire {
        major: u32,
        minor: u32,
    }

    impl From<DeviceNumber> for DeviceNumberWire {
        fn from(value: DeviceNumber) -> Self {
            Self {
                major: value.major,
                minor: value.minor,
            }
        }
    }
}

// Provide serde for rufus-core types via remote impls when feature is on.
// The core crate enables serde feature; we add derives there via cfg.

#[cfg(test)]
mod tests {
    use super::*;
    use rufus_core::device::DeviceNumber;
    use rufus_core::plan::{PartitionScheme, VerificationLevel, WriteMode};
    use std::path::PathBuf;

    fn sample_request() -> HelperRequest {
        HelperRequest {
            protocol_version: PROTOCOL_VERSION,
            job_id: JobId::new(42),
            target: TargetIdentity {
                node: PathBuf::from("/dev/sdb"),
                fingerprint: DeviceFingerprint {
                    number: DeviceNumber::new(8, 16),
                    canonical_sysfs_path: PathBuf::from("/sys/devices/pci0000:00/usb"),
                    size_bytes: 32_000_000_000,
                    logical_block_size: 512,
                    serial: Some("SN123".into()),
                    wwn: None,
                },
                display_name: "SanDisk Ultra".into(),
                model: "Ultra".into(),
                serial: "SN123".into(),
            },
            operation: HelperOperation::WriteMedia {
                write_mode: WriteMode::DdImage,
                source: SourceSpec {
                    path: PathBuf::from("/home/user/image.iso"),
                    kind: ImageSourceKind::Iso,
                    size_bytes: 700_000_000,
                    decompressed_size_bytes: None,
                    expected_sha256: None,
                },
                format: FormatSpec {
                    scheme: PartitionScheme::Gpt,
                    boot_mode: BootMode::Uefi,
                    filesystem: FileSystem::Fat32,
                    label: "RUFUS".into(),
                    cluster_size: None,
                    persistence_bytes: 0,
                    quick_format: true,
                },
                verification: VerificationLevel::FullReadback,
                bad_blocks: false,
                install_bootloader: None,
            },
            action_name: "Write image".into(),
        }
    }

    #[test]
    fn roundtrip_request() {
        let req = sample_request();
        let bytes = encode_line(&req).expect("encode sample request");
        let decoded: HelperRequest = decode_line(&bytes).expect("decode sample request");
        assert_eq!(req, decoded);
        assert!(decoded.validate().is_ok());
    }

    #[test]
    fn rejects_wrong_version() {
        let mut req = sample_request();
        req.protocol_version = 0;
        assert!(matches!(
            req.validate(),
            Err(ProtocolError::VersionMismatch { .. })
        ));
    }
}
