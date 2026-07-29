//! UI-facing application state and planning glue.

use std::path::PathBuf;

use rufus_core::capability::Capability;
use rufus_core::device::{BlockDevice, DeviceClass};
use rufus_core::plan::{
    build_steps, BootMode, FileSystem, ImageSource, ImageSourceKind, OperationPlan, PartitionPlan,
    PartitionScheme, PlanError, VerificationLevel, WriteMode,
};
use rufus_core::progress::{JobId, ProgressStage};
use rufus_core::safety::{confirmation_message, SafetyPolicy, SafetySnapshot};
use rufus_helper_protocol::{
    FormatSpec, HelperEvent, HelperOperation, HelperRequest, HelperResult, SourceSpec,
    TargetIdentity, PROTOCOL_VERSION,
};
use rufus_image::ImageReport;
use rufus_linux_platform::{list_block_devices, probe_capabilities};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootSelection {
    DiskOrIso,
    NonBootable,
    FreeDos,
    WindowsToGo,
}

impl BootSelection {
    pub fn label(self) -> &'static str {
        match self {
            Self::DiskOrIso => "Disk or ISO image",
            Self::NonBootable => "Non bootable",
            Self::FreeDos => "FreeDOS",
            Self::WindowsToGo => "Windows To Go",
        }
    }

    pub fn from_label(s: &str) -> Self {
        match s {
            "Non bootable" => Self::NonBootable,
            "FreeDOS" => Self::FreeDos,
            "Windows To Go" => Self::WindowsToGo,
            _ => Self::DiskOrIso,
        }
    }
}

pub struct AppState {
    pub devices: Vec<BlockDevice>,
    pub selected_device: usize,
    pub boot_selection: BootSelection,
    pub image_path: Option<PathBuf>,
    pub image_report: Option<ImageReport>,
    pub image_summary: String,
    pub image_notes: String,
    pub partition_scheme_label: String,
    pub target_system_label: String,
    pub filesystem_label: String,
    pub cluster_label: String,
    pub volume_label: String,
    pub quick_format: bool,
    pub check_bad_blocks: bool,
    pub verify_write: bool,
    pub list_usb_hdd: bool,
    pub list_fixed_disks: bool,
    pub persistence_enabled: bool,
    pub persistence_gb: f64,
    pub persistence_max_gb: f64,
    pub can_start: bool,
    pub is_busy: bool,
    pub status_phase: String,
    pub status_operation: String,
    pub status_progress: f64,
    pub status_telemetry: String,
    pub status_tone: String,
    pub status_active: bool,
    pub status_line: String,
    pub log: Vec<String>,
    pub capability_hint: String,
    capabilities: rufus_core::capability::CapabilityReport,
}

impl AppState {
    pub fn new() -> Self {
        let capabilities = probe_capabilities();
        let mut s = Self {
            devices: Vec::new(),
            selected_device: 0,
            boot_selection: BootSelection::DiskOrIso,
            image_path: None,
            image_report: None,
            image_summary: "No image selected".into(),
            image_notes: String::new(),
            partition_scheme_label: "GPT".into(),
            target_system_label: "UEFI (non CSM)".into(),
            filesystem_label: "FAT32".into(),
            cluster_label: "Default".into(),
            volume_label: "RUFUS".into(),
            quick_format: true,
            check_bad_blocks: false,
            verify_write: true,
            list_usb_hdd: false,
            list_fixed_disks: false,
            persistence_enabled: false,
            persistence_gb: 0.0,
            persistence_max_gb: 0.0,
            can_start: false,
            is_busy: false,
            status_phase: "READY".into(),
            status_operation: "Ready".into(),
            status_progress: 0.0,
            status_telemetry: String::new(),
            status_tone: "neutral".into(),
            status_active: false,
            status_line: "Select a device and image, then Start.".into(),
            log: vec![format!("Rufus Linux {} ready.", env!("CARGO_PKG_VERSION"))],
            capability_hint: String::new(),
            capabilities,
        };
        s.refresh_devices();
        s.recompute();
        s
    }

    pub fn push_log(&mut self, line: String) {
        self.log.push(line);
        if self.log.len() > 500 {
            self.log.drain(0..self.log.len() - 500);
        }
    }

    pub fn selected(&self) -> Option<&BlockDevice> {
        self.devices.get(self.selected_device)
    }

    pub fn select_device(&mut self, idx: usize) {
        if idx < self.devices.len() {
            self.selected_device = idx;
        }
        self.recompute();
    }

    pub fn refresh_devices(&mut self) {
        let show_fixed = self.list_fixed_disks || self.list_usb_hdd;
        match list_block_devices(show_fixed) {
            Ok(mut devices) => {
                let policy = SafetyPolicy {
                    show_fixed_disks: self.list_fixed_disks,
                    show_usb_hard_disks: self.list_usb_hdd,
                };
                devices.retain(|d| {
                    if d.has_risk(rufus_core::device::DeviceRisk::ContainsRoot)
                        || d.has_risk(rufus_core::device::DeviceRisk::ContainsBoot)
                    {
                        return false;
                    }
                    if !self.list_fixed_disks && d.class == DeviceClass::Internal {
                        return false;
                    }
                    if !self.list_usb_hdd && d.class == DeviceClass::ExternalFixed {
                        return false;
                    }
                    policy.is_listed(d) || d.class == DeviceClass::Removable
                });
                // When only USB HDD flag is on, allow ExternalFixed.
                if self.list_usb_hdd {
                    // already handled
                }
                self.devices = devices;
                if self.selected_device >= self.devices.len() {
                    self.selected_device = 0;
                }
                self.push_log(format!("Found {} candidate device(s).", self.devices.len()));
            }
            Err(e) => {
                self.devices.clear();
                self.push_log(format!("Device scan failed: {e}"));
            }
        }
        self.recompute();
    }

    pub fn set_image(&mut self, path: PathBuf) {
        match rufus_image::analyze(&path) {
            Ok(report) => {
                self.image_summary = format!(
                    "{} · {}",
                    report.display_kind(),
                    rufus_image::format_size(report.size_bytes)
                );
                self.image_notes = report.notes.join(" ");
                if let Some(fs) = report.preferred_filesystem {
                    if self.filesystem_available(fs.as_str()) {
                        self.filesystem_label = fs.as_str().to_owned();
                    }
                }
                self.persistence_enabled = report.persistence_supported
                    && self.capabilities.supports(Capability::LinuxPersistence);
                if self.persistence_enabled {
                    if let Some(dev) = self.selected() {
                        let free = dev.fingerprint.size_bytes.saturating_sub(report.size_bytes)
                            as f64
                            / (1024.0 * 1024.0 * 1024.0);
                        self.persistence_max_gb = free.max(0.0);
                    }
                }
                self.image_report = Some(report);
                self.image_path = Some(path);
                self.push_log(format!("Image: {}", self.image_summary));
            }
            Err(e) => {
                self.image_path = None;
                self.image_report = None;
                self.image_summary = "Failed to open image".into();
                self.image_notes = e.to_string();
                self.push_log(format!("Image error: {e}"));
            }
        }
        self.recompute();
    }

    pub fn available_filesystems(&self) -> Vec<String> {
        let candidates = [
            (Capability::FormatFat, "FAT"),
            (Capability::FormatFat32, "FAT32"),
            (Capability::FormatExfat, "exFAT"),
            (Capability::FormatNtfs, "NTFS"),
            (Capability::FormatUdf, "UDF"),
            (Capability::FormatExt2, "ext2"),
            (Capability::FormatExt3, "ext3"),
            (Capability::FormatExt4, "ext4"),
            (Capability::FormatRefs, "ReFS"),
        ];
        let mut out = Vec::new();
        for (cap, label) in candidates {
            if self.capabilities.supports(cap) || label == "ReFS" {
                // Keep ReFS visible but disabled via hint when selected.
                if self.capabilities.supports(cap) {
                    out.push(label.to_owned());
                } else if label == "ReFS" {
                    out.push("ReFS (unavailable)".to_owned());
                }
            }
        }
        if out.is_empty() {
            out.push("FAT32".into());
        }
        out
    }

    fn filesystem_available(&self, label: &str) -> bool {
        self.available_filesystems().iter().any(|f| f == label)
    }

    pub fn recompute(&mut self) {
        let mut hints = Vec::new();
        if self.filesystem_label.starts_with("ReFS") {
            hints.push("ReFS creation is unavailable on Linux.".to_owned());
        }
        if self.boot_selection == BootSelection::FreeDos
            && !self.capabilities.supports(Capability::FreeDos)
        {
            hints.push("FreeDOS assets are not packaged yet.".to_owned());
        }
        if self.boot_selection == BootSelection::WindowsToGo {
            hints.push(
                "Windows To Go is not available yet; raw-copying WIM/ESD files is not valid."
                    .to_owned(),
            );
        }
        if !crate::helper_client::helper_available() {
            hints.push(
                "Install the packaged privileged helper and polkit to enable destructive actions."
                    .to_owned(),
            );
        }
        if let Some(reason) = self.operation_unavailable_reason() {
            hints.push(reason);
        }
        self.capability_hint = hints.join(" ");

        let has_device = self.selected().is_some();
        let needs_image = matches!(
            self.boot_selection,
            BootSelection::DiskOrIso | BootSelection::WindowsToGo
        );
        let has_image = self.image_path.is_some();
        self.can_start = has_device
            && !self.is_busy
            && (!needs_image || has_image)
            && !self.filesystem_label.starts_with("ReFS")
            && crate::helper_client::helper_available()
            && self.operation_unavailable_reason().is_none();

        if let Some(report) = &self.image_report {
            self.persistence_enabled = report.persistence_supported
                && self.boot_selection == BootSelection::DiskOrIso
                && self.capabilities.supports(Capability::LinuxPersistence);
        } else {
            self.persistence_enabled = false;
        }
    }

    fn operation_unavailable_reason(&self) -> Option<String> {
        let filesystem_capability = match self.filesystem_label.as_str() {
            "FAT" => Capability::FormatFat,
            "FAT32" => Capability::FormatFat32,
            "exFAT" => Capability::FormatExfat,
            "NTFS" => Capability::FormatNtfs,
            "UDF" => Capability::FormatUdf,
            "ext2" => Capability::FormatExt2,
            "ext3" => Capability::FormatExt3,
            "ext4" => Capability::FormatExt4,
            _ => Capability::FormatRefs,
        };
        if self.boot_selection == BootSelection::NonBootable
            && !self.capabilities.supports(filesystem_capability)
        {
            return Some(format!(
                "The formatter for {} is not installed.",
                self.filesystem_label
            ));
        }
        if self.check_bad_blocks && !self.capabilities.supports(Capability::BadBlocksCheck) {
            return Some("Install badblocks from e2fsprogs to test the complete device.".into());
        }
        match self.boot_selection {
            BootSelection::FreeDos => {
                return Some(
                    "FreeDOS creation is unavailable until verified redistributable boot files are packaged."
                        .into(),
                );
            }
            BootSelection::WindowsToGo => {
                return Some(
                    "Windows To Go needs a tested wimlib, partition, BCD, and registry workflow."
                        .into(),
                );
            }
            BootSelection::NonBootable => return None,
            BootSelection::DiskOrIso => {}
        }

        match self.image_report.as_ref().map(|report| report.kind) {
            None => None,
            Some(ImageSourceKind::Raw | ImageSourceKind::IsoHybrid) => None,
            Some(ImageSourceKind::CompressedRaw) => {
                (!self.capabilities.supports(Capability::CompressedImageWrite))
                    .then(|| "Install the matching decompressor for this image.".into())
            }
            Some(ImageSourceKind::Iso) => Some(
                "This ISO is not hybrid. File-copy boot media is not available yet; choose an ISOHybrid image."
                    .into(),
            ),
            Some(ImageSourceKind::Vhd | ImageSourceKind::Vhdx) => Some(
                "VHD/VHDX conversion is not enabled in this release; container bytes will never be copied as a disk image."
                    .into(),
            ),
            Some(ImageSourceKind::Wim | ImageSourceKind::Esd) => Some(
                "WIM/ESD files require a Windows deployment workflow and cannot be raw-written."
                    .into(),
            ),
            Some(ImageSourceKind::Ffu) => {
                Some("FFU apply is unavailable because Linux has no selected safe provider.".into())
            }
            Some(ImageSourceKind::None) => Some("Select a supported disk image.".into()),
        }
    }

    pub fn action_name(&self) -> &'static str {
        match self.boot_selection {
            BootSelection::NonBootable => "Format device",
            _ => "Write image",
        }
    }

    pub fn build_confirm(&self) -> Result<String, String> {
        let dev = self.selected().ok_or("No device selected")?;
        let policy = SafetyPolicy {
            show_fixed_disks: self.list_fixed_disks,
            show_usb_hard_disks: self.list_usb_hdd,
        };
        let snapshot = SafetySnapshot {
            show_fixed_disks: self.list_fixed_disks,
            show_usb_hard_disks: self.list_usb_hdd,
            allow_zero_wipe: false,
            allow_fast_zero: false,
            allow_bad_blocks: self.check_bad_blocks,
            source_on_target: self.source_on_target(dev),
        };
        policy
            .evaluate_target(dev, &snapshot)
            .map_err(|e| e.to_string())?;

        // Also validate plan construction.
        self.build_plan().map_err(|e| e.to_string())?;

        let mut body = confirmation_message(
            self.action_name(),
            &dev.display_name,
            &dev.node.display().to_string(),
            &rufus_image::format_size(dev.fingerprint.size_bytes),
            dev.fingerprint.serial.as_deref(),
        );
        for extra in policy.extra_confirmations(dev, &snapshot, 1) {
            body.push_str("\n\n");
            body.push_str(extra);
        }
        Ok(body)
    }

    fn source_on_target(&self, dev: &BlockDevice) -> bool {
        let Some(path) = &self.image_path else {
            return false;
        };
        rufus_linux_platform::path_is_on_block_device(path, dev)
    }

    fn parse_scheme(&self) -> PartitionScheme {
        match self.partition_scheme_label.as_str() {
            "MBR" => PartitionScheme::Mbr,
            "Super floppy (disk image)" => PartitionScheme::SuperFloppy,
            _ => PartitionScheme::Gpt,
        }
    }

    fn parse_boot_mode(&self) -> BootMode {
        match self.boot_selection {
            BootSelection::NonBootable => BootMode::NonBootable,
            _ => match self.target_system_label.as_str() {
                "BIOS (CSM)" => BootMode::Bios,
                "BIOS or UEFI" => BootMode::Dual,
                _ => BootMode::Uefi,
            },
        }
    }

    fn parse_filesystem(&self) -> Result<FileSystem, PlanError> {
        let raw = self.filesystem_label.replace(" (unavailable)", "");
        FileSystem::parse(&raw).ok_or_else(|| {
            PlanError::IncompatibleOptions(format!("unknown filesystem {}", self.filesystem_label))
        })
    }

    fn parse_cluster_size(&self) -> Option<u32> {
        match self.cluster_label.as_str() {
            "512 bytes" => Some(512),
            "1024 bytes" => Some(1024),
            "2048 bytes" => Some(2048),
            "4096 bytes" => Some(4096),
            "8192 bytes" => Some(8192),
            "16 KB" => Some(16 * 1024),
            "32 KB" => Some(32 * 1024),
            "64 KB" => Some(64 * 1024),
            _ => None,
        }
    }

    fn write_mode(&self) -> WriteMode {
        match self.boot_selection {
            BootSelection::NonBootable => WriteMode::FormatOnly,
            BootSelection::FreeDos => WriteMode::FreeDos,
            BootSelection::WindowsToGo => WriteMode::WindowsToGo,
            BootSelection::DiskOrIso => {
                if let Some(report) = &self.image_report {
                    if report.isohybrid
                        || matches!(
                            report.kind,
                            ImageSourceKind::Raw
                                | ImageSourceKind::CompressedRaw
                                | ImageSourceKind::Vhd
                                | ImageSourceKind::Vhdx
                        )
                    {
                        WriteMode::DdImage
                    } else {
                        WriteMode::IsoFileCopy
                    }
                } else {
                    WriteMode::IsoFileCopy
                }
            }
        }
    }

    pub fn build_plan(&self) -> Result<OperationPlan, PlanError> {
        let dev = self
            .selected()
            .ok_or(PlanError::InvalidDevice("no device"))?;
        let filesystem = self.parse_filesystem()?;
        if filesystem == FileSystem::Refs {
            return Err(PlanError::UnavailableCapability(
                "ReFS creation is not available on Linux".into(),
            ));
        }
        let write_mode = self.write_mode();
        let partition = PartitionPlan {
            scheme: self.parse_scheme(),
            boot_mode: self.parse_boot_mode(),
            filesystem,
            label: self.volume_label.clone(),
            cluster_size: self.parse_cluster_size(),
            persistence_bytes: if self.persistence_enabled {
                (self.persistence_gb * 1024.0 * 1024.0 * 1024.0) as u64
            } else {
                0
            },
            quick_format: self.quick_format,
        };
        let verification = if self.verify_write {
            VerificationLevel::FullReadback
        } else {
            VerificationLevel::None
        };
        let source = match (&self.image_path, &self.image_report) {
            (Some(path), Some(report)) => Some(ImageSource {
                path: path.clone(),
                kind: report.kind,
                size_bytes: report.size_bytes,
                decompressed_size_bytes: report.decompressed_size_bytes,
                sha256: None,
            }),
            _ => None,
        };
        let steps = build_steps(write_mode, &partition, verification, self.check_bad_blocks);
        let plan = OperationPlan {
            job_id: JobId::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(1),
            ),
            target_node: dev.node.clone(),
            target_fingerprint: dev.fingerprint.clone(),
            source,
            partition,
            write_mode,
            verification,
            steps,
            action_name: self.action_name().into(),
        };
        plan.validate()?;
        Ok(plan)
    }

    pub fn build_helper_request(&self) -> Result<HelperRequest, String> {
        let plan = self.build_plan().map_err(|e| e.to_string())?;
        let dev = self.selected().ok_or("No device")?;
        let format = FormatSpec {
            scheme: plan.partition.scheme,
            boot_mode: plan.partition.boot_mode,
            filesystem: plan.partition.filesystem,
            label: plan.partition.label.clone(),
            cluster_size: plan.partition.cluster_size,
            persistence_bytes: plan.partition.persistence_bytes,
            quick_format: plan.partition.quick_format,
        };
        let operation = match plan.write_mode {
            WriteMode::FormatOnly => HelperOperation::FormatMedia {
                format,
                bad_blocks: self.check_bad_blocks,
            },
            other => {
                let source = plan.source.ok_or("Image required")?;
                HelperOperation::WriteMedia {
                    write_mode: other,
                    source: SourceSpec {
                        path: source.path,
                        kind: source.kind,
                        size_bytes: source.size_bytes,
                        decompressed_size_bytes: source.decompressed_size_bytes,
                        expected_sha256: None,
                    },
                    format,
                    verification: plan.verification,
                    bad_blocks: self.check_bad_blocks,
                    install_bootloader: None,
                }
            }
        };
        Ok(HelperRequest {
            protocol_version: PROTOCOL_VERSION,
            job_id: plan.job_id,
            target: TargetIdentity {
                node: dev.node.clone(),
                fingerprint: dev.fingerprint.clone(),
                display_name: dev.display_name.clone(),
                model: dev.model.clone().unwrap_or_default(),
                serial: dev.fingerprint.serial.clone().unwrap_or_default(),
            },
            operation,
            action_name: plan.action_name,
        })
    }

    pub fn begin_operation(&mut self) {
        self.is_busy = true;
        self.status_active = true;
        self.status_tone = "neutral".into();
        self.status_phase = "PREPARE".into();
        self.status_operation = "Starting…".into();
        self.status_progress = 0.0;
        self.status_line = "Authorizing and preparing…".into();
        self.push_log(format!("Starting {}.", self.action_name()));
        self.recompute();
    }

    pub fn handle_helper_event(&mut self, event: &HelperEvent) {
        match event {
            HelperEvent::Accepted { job_id } => {
                self.push_log(format!("Helper accepted job {job_id}"));
            }
            HelperEvent::Progress {
                stage,
                completed,
                total,
                detail,
                ..
            } => {
                self.status_phase = stage_label(*stage).into();
                self.status_operation = detail.clone().unwrap_or_else(|| format!("{stage:?}"));
                if let Some(t) = total {
                    if *t > 0 {
                        self.status_progress = (*completed as f64 / *t as f64) * 100.0;
                    }
                }
                self.status_telemetry = match total {
                    Some(t) => format!("{completed}/{t}"),
                    None => String::new(),
                };
                self.status_line = self.status_operation.clone();
            }
            HelperEvent::Log { line, .. } => self.push_log(line.clone()),
            HelperEvent::Finished { result, .. } => match result {
                HelperResult::Success => {}
                HelperResult::Cancelled { message } => self.push_log(message.clone()),
                HelperResult::Failed { message, .. } => self.push_log(message.clone()),
            },
        }
    }

    pub fn finish_ok(&mut self) {
        self.is_busy = false;
        self.status_active = false;
        self.status_phase = "DONE".into();
        self.status_operation = "Completed".into();
        self.status_progress = 100.0;
        self.status_tone = "neutral".into();
        self.status_line = "Operation completed successfully and the device was flushed.".into();
        self.push_log("Finished OK.".into());
        self.recompute();
    }

    pub fn fail_operation(&mut self, msg: String) {
        self.is_busy = false;
        self.status_active = false;
        self.status_phase = "ERROR".into();
        self.status_operation = "Failed".into();
        self.status_tone = "error".into();
        self.status_line = msg.clone();
        self.push_log(format!("Failed: {msg}"));
        self.recompute();
    }

    pub fn cancel_operation(&mut self) {
        self.is_busy = false;
        self.status_active = false;
        self.status_phase = "CANCELLED".into();
        self.status_operation = "Cancelled".into();
        self.status_tone = "warning".into();
        self.status_line =
            "Operation stopped. The target may be incomplete; rewrite or reformat it before use."
                .into();
        self.push_log("Operation cancelled; target may be incomplete.".into());
        self.recompute();
    }
}

fn stage_label(stage: ProgressStage) -> &'static str {
    match stage {
        ProgressStage::Preparing => "PREPARE",
        ProgressStage::Authorizing => "AUTH",
        ProgressStage::Unmounting => "UNMOUNT",
        ProgressStage::TestingMedia => "TEST",
        ProgressStage::Wiping => "WIPE",
        ProgressStage::Partitioning => "PARTITION",
        ProgressStage::Formatting => "FORMAT",
        ProgressStage::WritingImage => "WRITE",
        ProgressStage::ExtractingFiles => "EXTRACT",
        ProgressStage::InstallingBootloader => "BOOT",
        ProgressStage::ApplyingCustomization => "CUSTOM",
        ProgressStage::Syncing => "SYNC",
        ProgressStage::Verifying => "VERIFY",
        ProgressStage::Finalizing => "FINAL",
    }
}

pub trait DeviceListLabel {
    fn list_label(&self) -> String;
}

impl DeviceListLabel for BlockDevice {
    fn list_label(&self) -> String {
        format!(
            "{} ({}) — {}",
            self.display_name,
            self.node.display(),
            rufus_image::format_size(self.fingerprint.size_bytes)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rufus_core::device::{DeviceFingerprint, DeviceNumber, DeviceRisk, Transport};
    use std::path::PathBuf;

    fn sample_device() -> BlockDevice {
        BlockDevice {
            node: PathBuf::from("/dev/sdb"),
            display_name: "Test Stick".into(),
            vendor: Some("Test".into()),
            model: Some("Stick".into()),
            class: DeviceClass::Removable,
            transport: Transport::Usb,
            fingerprint: DeviceFingerprint {
                number: DeviceNumber::new(8, 16),
                canonical_sysfs_path: PathBuf::from("/sys/devices/example"),
                size_bytes: 32 * 1024 * 1024 * 1024,
                logical_block_size: 512,
                serial: Some("SN".into()),
                wwn: None,
            },
            risks: vec![],
        }
    }

    #[test]
    fn format_only_plan_without_image() {
        let mut st = AppState::new();
        st.devices = vec![sample_device()];
        st.selected_device = 0;
        st.boot_selection = BootSelection::NonBootable;
        st.filesystem_label = "FAT32".into();
        st.recompute();
        let plan = st.build_plan().expect("plan");
        assert_eq!(plan.write_mode, WriteMode::FormatOnly);
    }

    #[test]
    fn root_device_fails_confirm() {
        let mut st = AppState::new();
        let mut dev = sample_device();
        dev.risks.push(DeviceRisk::ContainsRoot);
        st.devices = vec![dev];
        st.selected_device = 0;
        st.boot_selection = BootSelection::NonBootable;
        st.recompute();
        assert!(st.build_confirm().is_err());
    }

    #[test]
    fn refs_plan_rejected() {
        let mut st = AppState::new();
        st.devices = vec![sample_device()];
        st.filesystem_label = "ReFS (unavailable)".into();
        st.boot_selection = BootSelection::NonBootable;
        assert!(st.build_plan().is_err());
    }
}
