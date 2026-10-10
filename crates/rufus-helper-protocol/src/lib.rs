//! Narrow, versioned protocol between the unprivileged desktop and root helper.
//!
//! The helper must never accept shell fragments, relative paths, or environment-
//! controlled tool names. Messages are bounded newline-delimited JSON on a pipe.

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

/// Hard ceiling for one request or event. Protocol messages are normally only
/// a few kilobytes; the bound prevents an untrusted peer from growing memory
/// without limit before JSON validation.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

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

/// Longest local account name accepted, as upstream's `MAX_USERNAME_LENGTH`.
pub const MAX_USERNAME_CHARS: usize = 128;
/// Longest regional value; real ones are a few dozen characters.
pub const MAX_REGIONAL_CHARS: usize = 96;

/// The Windows User Experience dialog's choices. The helper turns them into
/// an answer file, so only typed values cross the boundary, never XML.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WindowsCustomization {
    /// Remove the 4 GB+ RAM, Secure Boot, and TPM 2.0 requirements.
    pub bypass_requirements: bool,
    /// Remove the requirement for an online Microsoft account.
    pub no_online_account: bool,
    /// Create a local account with this name and an empty password.
    pub local_account: Option<String>,
    /// Copy this computer's keyboard, locale, and time zone.
    pub regional: Option<RegionalSettings>,
    /// Answer "no" to the data collection questions.
    pub no_data_collection: bool,
    /// Disable BitLocker automatic device encryption.
    pub disable_bitlocker: bool,
    /// Don't force Copilot, OneDrive, Outlook, Fast Startup, and so on.
    pub quality_of_life: bool,
    /// Copy SkuSiPolicy.p7b to the ESP on first logon (KB5042562).
    pub apply_skusipolicy: bool,
    /// Erase the first disk and install this image index without asking.
    pub silent_install_index: Option<u32>,
    /// Restrict Windows to S Mode.
    pub force_s_mode: bool,
}

/// Answer-file regional values, already in Windows' notation.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegionalSettings {
    /// `InputLocale`, such as `0409:00000409` or `en-US`.
    pub input_locale: String,
    pub system_locale: String,
    pub user_locale: String,
    pub ui_language: String,
    /// A Windows time zone name such as `W. Europe Standard Time`.
    pub time_zone: Option<String>,
}

impl WindowsCustomization {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if let Some(name) = &self.local_account {
            if name.trim().is_empty()
                || name.chars().count() > MAX_USERNAME_CHARS
                || name.chars().any(char::is_control)
            {
                return Err(ProtocolError::InvalidRequest(
                    "invalid local account name".into(),
                ));
            }
        }
        if let Some(regional) = &self.regional {
            let locale = |value: &str| {
                !value.is_empty()
                    && value.len() <= MAX_REGIONAL_CHARS
                    && value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | ':' | ';' | '_'))
            };
            let zone = |value: &str| {
                !value.is_empty()
                    && value.len() <= MAX_REGIONAL_CHARS
                    && value.chars().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '+' | '-' | '(' | ')')
                    })
            };
            if ![
                &regional.input_locale,
                &regional.system_locale,
                &regional.user_locale,
                &regional.ui_language,
            ]
            .iter()
            .all(|value| locale(value))
                || regional
                    .time_zone
                    .as_deref()
                    .is_some_and(|value| !zone(value))
            {
                return Err(ProtocolError::InvalidRequest(
                    "invalid regional settings".into(),
                ));
            }
        }
        if self.silent_install_index == Some(0) {
            return Err(ProtocolError::InvalidRequest(
                "Windows image indexes start at 1".into(),
            ));
        }
        Ok(())
    }
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
        /// Windows User Experience choices for Windows installer media.
        #[serde(default)]
        windows_customization: Option<Box<WindowsCustomization>>,
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
            HelperOperation::WriteMedia {
                source,
                windows_customization,
                ..
            } => {
                if !source.path.is_absolute() {
                    return Err(ProtocolError::InvalidRequest(
                        "source path must be absolute".into(),
                    ));
                }
                if let Some(customization) = windows_customization {
                    customization.validate()?;
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
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(ProtocolError::Codec(
            "message exceeds protocol size limit".into(),
        ));
    }
    Ok(bytes)
}

/// Decode a single newline-terminated JSON object.
pub fn decode_line<T: for<'de> Deserialize<'de>>(line: &[u8]) -> Result<T, ProtocolError> {
    if line.len() > MAX_MESSAGE_BYTES {
        return Err(ProtocolError::Codec(
            "message exceeds protocol size limit".into(),
        ));
    }
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
                windows_customization: None,
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
    fn windows_customization_round_trips_and_rejects_markup() {
        let mut req = sample_request();
        let customization = WindowsCustomization {
            bypass_requirements: true,
            local_account: Some("Ana María".into()),
            regional: Some(RegionalSettings {
                input_locale: "0409:00000409".into(),
                system_locale: "en-PH".into(),
                user_locale: "fil-PH".into(),
                ui_language: "en-US".into(),
                time_zone: Some("Singapore Standard Time".into()),
            }),
            silent_install_index: Some(6),
            ..WindowsCustomization::default()
        };
        if let HelperOperation::WriteMedia {
            windows_customization,
            ..
        } = &mut req.operation
        {
            *windows_customization = Some(Box::new(customization.clone()));
        }
        let decoded: HelperRequest =
            decode_line(&encode_line(&req).expect("encode")).expect("decode");
        assert_eq!(decoded, req);
        assert!(decoded.validate().is_ok());

        let mut bad = customization.clone();
        bad.regional.as_mut().expect("regional").time_zone = Some("<x/>".into());
        assert!(bad.validate().is_err());
        let mut bad = customization.clone();
        bad.regional.as_mut().expect("regional").user_locale = "en-US\"><x".into();
        assert!(bad.validate().is_err());
        let mut bad = customization;
        bad.local_account = Some("a\nb".into());
        assert!(bad.validate().is_err());
    }

    #[test]
    fn requests_without_customization_still_decode() {
        let mut json =
            String::from_utf8(encode_line(&sample_request()).expect("encode")).expect("UTF-8");
        json = json.replace(",\"windows_customization\":null", "");
        assert!(!json.contains("windows_customization"));
        let decoded: HelperRequest = decode_line(json.as_bytes()).expect("decode");
        assert_eq!(decoded, sample_request());
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

    #[test]
    fn rejects_oversized_messages_before_json_decode() {
        let oversized = vec![b' '; MAX_MESSAGE_BYTES + 1];
        let error = decode_line::<HelperRequest>(&oversized).expect_err("oversized input");
        assert!(error.to_string().contains("size limit"));
    }
}
