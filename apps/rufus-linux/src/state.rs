//! UI-facing application state and planning glue.

use std::path::PathBuf;

use rufus_core::capability::Capability;
use rufus_core::device::{BlockDevice, DeviceClass};
use rufus_core::plan::{
    build_steps, BootMode, FileSystem, ImageSource, ImageSourceKind, OperationPlan, PartitionPlan,
    PartitionScheme, PlanError, VerificationLevel, WriteMode,
};
use rufus_core::progress::{CancellationToken, JobId, ProgressStage};
use rufus_core::safety::{confirmation_message, SafetyPolicy, SafetySnapshot};
use rufus_helper_protocol::{
    FormatSpec, HelperEvent, HelperOperation, HelperRequest, HelperResult, SourceSpec,
    TargetIdentity, PROTOCOL_VERSION,
};
use rufus_image::ImageReport;
use rufus_linux_platform::{list_block_devices, probe_capabilities};

use crate::helper_client::{detect_backend, Backend};
use crate::settings::Settings;
use crate::units::{SizeUnit, SpeedUnit};

pub const DEFAULT_VOLUME_LABEL: &str = "RUFUS";
/// Upstream's name for a whole-device filesystem without a partition table.
pub const SUPER_FLOPPY_LABEL: &str = "Super Floppy Disk";
/// Largest file FAT32 can store.
const FAT32_MAX_FILE: u64 = 4 * 1024 * 1024 * 1024 - 1;
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
    pub selected_device: Option<usize>,
    pub boot_selection: BootSelection,
    pub image_path: Option<PathBuf>,
    pub image_report: Option<ImageReport>,
    pub image_summary: String,
    pub image_notes: String,
    pub image_inspecting: bool,
    pub close_after_inspection: bool,
    image_generation: u64,
    image_cancel: Option<CancellationToken>,
    pub partition_scheme_label: String,
    pub target_system_label: String,
    pub filesystem_label: String,
    pub cluster_label: String,
    pub volume_label: String,
    /// Set once the user types a label; image selection then stops proposing one.
    pub volume_label_edited: bool,
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
    last_sample: Option<crate::units::Sample>,
    pub settings: Settings,
    pub status_tone: String,
    pub status_active: bool,
    pub status_line: String,
    pub log: Vec<String>,
    pub capability_hint: String,
    capabilities: rufus_core::capability::CapabilityReport,
    /// How writes run here, or why they cannot.
    pub backend: Result<Backend, String>,
}

impl AppState {
    pub fn new() -> Self {
        let capabilities = probe_capabilities();
        let mut s = Self {
            devices: Vec::new(),
            selected_device: None,
            boot_selection: BootSelection::DiskOrIso,
            image_path: None,
            image_report: None,
            image_summary: "No image selected".into(),
            image_notes: String::new(),
            image_inspecting: false,
            close_after_inspection: false,
            image_generation: 0,
            image_cancel: None,
            partition_scheme_label: "GPT".into(),
            target_system_label: "UEFI (non CSM)".into(),
            filesystem_label: "FAT32".into(),
            cluster_label: "Default".into(),
            volume_label: DEFAULT_VOLUME_LABEL.into(),
            volume_label_edited: false,
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
            last_sample: None,
            settings: Settings::load(),
            status_tone: "neutral".into(),
            status_active: false,
            status_line: "Select a device and image, then Start.".into(),
            log: vec![format!("Rufus Linux {} ready.", env!("CARGO_PKG_VERSION"))],
            capability_hint: String::new(),
            capabilities,
            backend: detect_backend(),
        };
        match &s.backend {
            Ok(backend) => s.push_log(format!("Disk access through {}.", backend.describe())),
            Err(reason) => s.push_log(reason.clone()),
        }
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
        self.selected_device
            .and_then(|index| self.devices.get(index))
    }

    pub fn select_device(&mut self, idx: usize) {
        if idx < self.devices.len() {
            self.selected_device = Some(idx);
        }
        self.recompute();
    }

    pub fn refresh_devices(&mut self) {
        // udisks2 may have started or stopped since the last scan.
        if self.backend.is_err() {
            self.backend = detect_backend();
        }
        let selected_fingerprint = self.selected().map(|device| device.fingerprint.clone());
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
                self.selected_device = match selected_fingerprint.as_ref() {
                    Some(fingerprint) => devices
                        .iter()
                        .position(|device| device.fingerprint.matches(fingerprint)),
                    None => (!devices.is_empty()).then_some(0),
                };
                self.devices = devices;
                self.push_log(format!("Found {} candidate device(s).", self.devices.len()));
            }
            Err(e) => {
                self.devices.clear();
                self.selected_device = None;
                self.push_log(format!("Device scan failed: {e}"));
            }
        }
        self.recompute();
    }

    pub fn begin_image_inspection(&mut self, path: PathBuf) -> Option<(u64, CancellationToken)> {
        if self.is_busy || self.close_after_inspection {
            return None;
        }
        self.cancel_image_inspection();
        self.image_generation = self.image_generation.wrapping_add(1);
        let cancel = CancellationToken::new();
        self.image_cancel = Some(cancel.clone());
        self.image_inspecting = true;
        self.image_path = Some(path);
        self.image_report = None;
        self.image_summary = "Inspecting image…".into();
        self.image_notes.clear();
        self.recompute();
        Some((self.image_generation, cancel))
    }

    pub fn cancel_image_inspection(&self) {
        if let Some(cancel) = &self.image_cancel {
            cancel.request();
        }
    }

    pub fn finish_image_inspection(
        &mut self,
        generation: u64,
        result: Result<ImageReport, rufus_image::ImageError>,
    ) -> bool {
        if generation != self.image_generation || !self.image_inspecting {
            return false;
        }
        self.image_inspecting = false;
        self.image_cancel = None;
        match result {
            Ok(report) => {
                self.image_summary = if let Some(expanded) = report.decompressed_size_bytes {
                    format!(
                        "{} · {} file · {} disk",
                        report.display_kind(),
                        rufus_image::format_size(report.size_bytes),
                        rufus_image::format_size(expanded)
                    )
                } else {
                    format!(
                        "{} · {}",
                        report.display_kind(),
                        rufus_image::format_size(report.size_bytes)
                    )
                };
                self.image_notes = report.notes.join(" ");
                // Like upstream Rufus, propose the image's own volume name
                // (some distros boot only from a USB carrying that label).
                if !self.volume_label_edited {
                    self.volume_label = report
                        .label_hint
                        .clone()
                        .unwrap_or_else(|| DEFAULT_VOLUME_LABEL.into());
                }
                if let Some(fs) = report.preferred_filesystem {
                    if self.filesystem_available(fs.as_str()) {
                        self.filesystem_label = fs.as_str().to_owned();
                    }
                }
                if report.windows_installer {
                    // Upstream's default for current Windows media.
                    self.partition_scheme_label = "GPT".into();
                    self.target_system_label = "UEFI (non CSM)".into();
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
                self.image_path = Some(report.path.clone());
                self.image_report = Some(report);
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
        true
    }

    #[cfg(test)]
    pub fn set_image(&mut self, path: PathBuf) {
        let (generation, _) = self
            .begin_image_inspection(path.clone())
            .expect("begin image test");
        self.finish_image_inspection(generation, rufus_image::analyze(&path));
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
        candidates
            .into_iter()
            .map(|(cap, label)| {
                if self.capabilities.supports(cap) {
                    label.to_owned()
                } else {
                    format!("{label} (unavailable)")
                }
            })
            .collect()
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
        if let Err(reason) = &self.backend {
            hints.push(reason.clone());
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
            && self.backend.is_ok()
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
        if self.image_inspecting {
            return Some("Wait for image inspection to finish.".into());
        }
        let filesystem_name = self.filesystem_label.replace(" (unavailable)", "");
        let filesystem_capability = match filesystem_name.as_str() {
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
            if filesystem_name == "ReFS" {
                return Some("ReFS creation is unavailable on Linux.".into());
            }
            return Some(format!(
                "Install the formatter provider for {filesystem_name} to enable this option."
            ));
        }
        let own_bad_block_test = matches!(self.backend, Ok(Backend::Udisks { .. }));
        if self.check_bad_blocks
            && !own_bad_block_test
            && !self.capabilities.supports(Capability::BadBlocksCheck)
        {
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
            Some(ImageSourceKind::Iso) => self.windows_media_unavailable_reason(),
            Some(ImageSourceKind::Vhd | ImageSourceKind::Vhdx) => {
                (!self.capabilities.supports(Capability::VirtualDiskWrite)).then(|| {
                    "Install qemu-nbd, nbdinfo, and nbdcopy to write this virtual disk. Container bytes are never copied as a disk image.".into()
                })
            }
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

    /// Why the selected options cannot build Windows installer media.
    fn windows_media_unavailable_reason(&self) -> Option<String> {
        let report = self.image_report.as_ref()?;
        if !report.windows_installer {
            return Some(
                "This ISO is not hybrid and is not Windows installation media. File-copy boot media for Linux ISOs is not available yet; choose an ISOHybrid image."
                    .into(),
            );
        }
        if let Ok(backend) = &self.backend {
            if !backend.supports_file_copy() {
                return Some(
                    "Windows installer media is created through the udisks2 disk service; install or start udisks2."
                        .into(),
                );
            }
        }
        let filesystem = self.parse_filesystem().ok()?;
        if !matches!(
            filesystem,
            FileSystem::Ntfs | FileSystem::ExFat | FileSystem::Fat32
        ) {
            return Some("Windows installer media needs NTFS, exFAT, or FAT32.".into());
        }
        if filesystem == FileSystem::Fat32
            && report
                .largest_file_bytes
                .is_some_and(|size| size > FAT32_MAX_FILE)
        {
            return Some(
                "This image has a file larger than 4 GB, which FAT32 cannot store. Choose NTFS."
                    .into(),
            );
        }
        let scheme = self.parse_scheme();
        if scheme == PartitionScheme::SuperFloppy {
            return Some("Windows installer media needs the MBR or GPT partition scheme.".into());
        }
        let boot_mode = self.parse_boot_mode();
        let bios = matches!(boot_mode, BootMode::Bios | BootMode::Dual);
        let uefi = matches!(boot_mode, BootMode::Uefi | BootMode::Dual);
        if bios && scheme != PartitionScheme::Mbr {
            return Some("BIOS boot needs the MBR partition scheme.".into());
        }
        if bios && filesystem == FileSystem::ExFat {
            return Some(
                "BIOS boot needs NTFS or FAT32; exFAT media boots through UEFI only.".into(),
            );
        }
        if bios && !report.has_bios {
            return Some("This image has no BIOS boot manager; choose UEFI (non CSM).".into());
        }
        if uefi && !report.has_efi {
            return Some("This image has no UEFI boot loader; choose BIOS (CSM).".into());
        }
        None
    }

    /// Partition schemes offered for the current boot selection. As upstream,
    /// a super floppy disk is a plain format only: bootable media needs a
    /// partition table.
    pub fn partition_choices(&self) -> Vec<&'static str> {
        let mut choices = vec!["MBR", "GPT"];
        if self.boot_selection == BootSelection::NonBootable {
            choices.push(SUPER_FLOPPY_LABEL);
        }
        choices
    }

    pub fn select_boot(&mut self, selection: BootSelection) {
        self.boot_selection = selection;
        if !self
            .partition_choices()
            .contains(&self.partition_scheme_label.as_str())
        {
            // Return to the startup default rather than an unexpected scheme.
            self.select_partition_scheme("GPT");
        } else {
            self.recompute();
        }
    }

    /// Keep the target system consistent with the scheme, as upstream does.
    pub fn select_partition_scheme(&mut self, label: &str) {
        self.partition_scheme_label = label.to_owned();
        let (has_bios, has_efi) = self
            .image_report
            .as_ref()
            .map_or((true, true), |report| (report.has_bios, report.has_efi));
        match self.parse_scheme() {
            PartitionScheme::Gpt => self.target_system_label = "UEFI (non CSM)".into(),
            PartitionScheme::Mbr if has_bios && has_efi => {
                self.target_system_label = "BIOS or UEFI".into()
            }
            PartitionScheme::Mbr if has_bios => self.target_system_label = "BIOS (CSM)".into(),
            _ => {}
        }
        self.recompute();
    }

    pub fn action_name(&self) -> &'static str {
        match self.boot_selection {
            BootSelection::NonBootable => "Format device",
            _ => "Write image",
        }
    }

    pub fn build_confirm(&self) -> Result<String, String> {
        if self.image_inspecting {
            return Err("Wait for image inspection to finish.".into());
        }
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
        let plan = self.build_plan().map_err(|e| e.to_string())?;

        let mut body = confirmation_message(
            self.action_name(),
            &dev.display_name,
            &dev.node.display().to_string(),
            &rufus_image::format_size(dev.fingerprint.size_bytes),
            dev.fingerprint.serial.as_deref(),
        );
        if plan.write_mode != WriteMode::FormatOnly {
            if let Some(source) = &plan.source {
                body.push_str("\n\n");
                body.push_str(&crate::confirmation::source_details(source));
            }
        }
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
            SUPER_FLOPPY_LABEL => PartitionScheme::SuperFloppy,
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

    pub fn edit_volume_label(&mut self, label: &str) {
        self.volume_label = label.to_owned();
        self.volume_label_edited = !label.is_empty();
    }

    /// Explains when the chosen filesystem will store a shortened label.
    pub fn volume_label_note(&self) -> String {
        let Ok(filesystem) = self.parse_filesystem() else {
            return String::new();
        };
        let written = filesystem.volume_label(&self.volume_label);
        if written == self.volume_label {
            String::new()
        } else {
            format!(
                "{} will store this label as “{written}”.",
                filesystem.as_str()
            )
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
            label: filesystem.volume_label(&self.volume_label),
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
        let source = match (&self.image_path, &self.image_report, write_mode) {
            (_, _, WriteMode::FormatOnly) => None,
            (Some(path), Some(report), _) => Some(ImageSource {
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
        if let Some(reason) = self.operation_unavailable_reason() {
            return Err(reason);
        }
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
        self.last_sample = None;
        self.status_telemetry.clear();
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
                unit,
                completed,
                total,
                bytes_per_second,
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
                self.last_sample = Some(crate::units::Sample {
                    unit: *unit,
                    completed: *completed,
                    total: *total,
                    bytes_per_second: *bytes_per_second,
                });
                self.refresh_telemetry();
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

    pub fn set_display_units(&mut self, size: Option<SizeUnit>, speed: Option<SpeedUnit>) {
        if let Some(size) = size {
            self.settings.size_unit = size;
        }
        if let Some(speed) = speed {
            self.settings.speed_unit = speed;
        }
        self.settings.save();
        self.refresh_telemetry();
    }

    fn refresh_telemetry(&mut self) {
        self.status_telemetry = self
            .last_sample
            .map(|sample| {
                crate::units::telemetry(&sample, self.settings.size_unit, self.settings.speed_unit)
            })
            .unwrap_or_default();
    }

    pub fn finish_ok(&mut self) {
        self.is_busy = false;
        self.last_sample = None;
        self.status_telemetry.clear();
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
        st.selected_device = Some(0);
        st.boot_selection = BootSelection::NonBootable;
        st.filesystem_label = "FAT32".into();
        st.recompute();
        let plan = st.build_plan().expect("plan");
        assert_eq!(plan.write_mode, WriteMode::FormatOnly);
    }

    #[test]
    fn confirmation_names_the_source_and_distinguishes_virtual_capacity() {
        let mut st = AppState::new();
        st.devices = vec![sample_device()];
        st.selected_device = Some(0);
        st.set_image(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
        let report = st.image_report.as_mut().expect("regular source fixture");
        report.kind = ImageSourceKind::Vhdx;
        report.size_bytes = 8 * 1024 * 1024;
        report.decompressed_size_bytes = Some(16 * 1024 * 1024);
        let body = st.build_confirm().expect("advisory confirmation fixture");
        assert!(body.contains("File size: 8.0 MB\nDisk size: 16.0 MB"));
        assert!(
            body.find("Serial: SN").expect("target identity")
                < body.find("Image:").expect("source details")
        );
        if st.capabilities.supports(Capability::VirtualDiskWrite) {
            let request = st.build_helper_request().expect("conversion request");
            let rufus_helper_protocol::HelperOperation::WriteMedia { source, .. } =
                request.operation
            else {
                panic!("conversion must use a media write");
            };
            assert_eq!(source.kind, ImageSourceKind::Vhdx);
        } else {
            assert!(
                st.build_helper_request()
                    .expect_err("missing conversion providers")
                    .contains("qemu-nbd"),
                "missing providers must fail closed"
            );
        }

        st.image_report
            .as_mut()
            .expect("fixture")
            .decompressed_size_bytes = Some(64 * 1024 * 1024 * 1024);
        assert!(
            st.build_confirm().is_err(),
            "expanded image must fit the target"
        );
        st.boot_selection = BootSelection::NonBootable;
        let body = st
            .build_confirm()
            .expect("format ignores stale selected image");
        assert!(!body.contains("Image:"));
        assert!(st.build_plan().expect("format plan").source.is_none());
    }

    #[test]
    fn image_inspection_blocks_requests_and_ignores_stale_results() {
        let mut st = AppState::new();
        st.devices = vec![sample_device()];
        st.selected_device = Some(0);
        let (first, cancelled) = st
            .begin_image_inspection("first.img".into())
            .expect("first request");
        assert!(st.image_report.is_none());
        assert!(!st.can_start);
        assert!(st
            .build_confirm()
            .expect_err("inspection confirmation guard")
            .contains("inspection"));
        st.boot_selection = BootSelection::NonBootable;
        assert!(st
            .build_helper_request()
            .expect_err("inspection request guard")
            .contains("inspection"));
        let (second, _) = st
            .begin_image_inspection("second.img".into())
            .expect("replacement request");
        assert!(cancelled.is_requested());
        assert!(!st.finish_image_inspection(
            first,
            Err(rufus_image::ImageError::Unsupported("stale".into()))
        ));
        assert!(st.image_inspecting);
        assert_eq!(
            st.image_path.as_deref(),
            Some(std::path::Path::new("second.img"))
        );
        assert!(st.finish_image_inspection(
            second,
            Err(rufus_image::ImageError::Unsupported("current error".into()))
        ));
        assert!(!st.image_inspecting);
        assert!(st.image_report.is_none());
        assert!(st.image_path.is_none());
        assert_eq!(
            st.image_notes,
            "unsupported or unreadable image: current error"
        );
    }

    fn report_with_label(label: Option<&str>) -> ImageReport {
        // Tests run in parallel; every fixture needs its own file.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "rufus-label-{}-{}.img",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&path, vec![0; 4096]).expect("write image fixture");
        let mut report = rufus_image::analyze(&path).expect("analyze image fixture");
        std::fs::remove_file(&path).expect("remove image fixture");
        report.label_hint = label.map(str::to_owned);
        report
    }

    fn inspect(st: &mut AppState, report: ImageReport) {
        let (generation, _) = st
            .begin_image_inspection(report.path.clone())
            .expect("begin inspection");
        assert!(st.finish_image_inspection(generation, Ok(report)));
    }

    #[test]
    fn image_volume_name_is_proposed_until_the_user_types_a_label() {
        let mut st = AppState::new();
        st.devices = vec![sample_device()];
        st.selected_device = Some(0);

        inspect(&mut st, report_with_label(Some("CCCOMA_X64FRE_EN-US_DV9")));
        assert_eq!(st.volume_label, "CCCOMA_X64FRE_EN-US_DV9");
        inspect(&mut st, report_with_label(None));
        assert_eq!(st.volume_label, DEFAULT_VOLUME_LABEL);

        st.edit_volume_label("MY_STICK");
        inspect(&mut st, report_with_label(Some("ARCH_202610")));
        assert_eq!(st.volume_label, "MY_STICK");

        // Clearing the field hands the label back to image proposals.
        st.edit_volume_label("");
        inspect(&mut st, report_with_label(Some("ARCH_202610")));
        assert_eq!(st.volume_label, "ARCH_202610");
    }

    #[test]
    fn plan_stores_a_label_the_filesystem_accepts() {
        let mut st = AppState::new();
        st.devices = vec![sample_device()];
        st.selected_device = Some(0);
        st.boot_selection = BootSelection::NonBootable;
        st.edit_volume_label("CCCOMA_X64FRE_EN-US_DV9");

        st.filesystem_label = "FAT32".into();
        st.recompute();
        assert_eq!(
            st.build_plan().expect("FAT32 plan").partition.label,
            "CCCOMA_X64F"
        );
        assert_eq!(
            st.volume_label_note(),
            "FAT32 will store this label as “CCCOMA_X64F”."
        );

        if st.filesystem_available("NTFS") {
            st.filesystem_label = "NTFS".into();
            st.recompute();
            let plan = st.build_plan().expect("NTFS plan");
            assert_eq!(plan.partition.label, "CCCOMA_X64FRE_EN-US_DV9");
            assert_eq!(st.volume_label_note(), "");
        }
    }

    #[test]
    fn image_inspection_respects_active_operations_and_shutdown() {
        let mut st = AppState::new();
        st.is_busy = true;
        assert!(st.begin_image_inspection("ignored.img".into()).is_none());
        st.is_busy = false;
        let (_, cancel) = st
            .begin_image_inspection("image.img".into())
            .expect("inspection request");
        st.close_after_inspection = true;
        st.cancel_image_inspection();
        assert!(cancel.is_requested());
        assert!(st.begin_image_inspection("ignored.img".into()).is_none());
    }

    #[test]
    fn root_device_fails_confirm() {
        let mut st = AppState::new();
        let mut dev = sample_device();
        dev.risks.push(DeviceRisk::ContainsRoot);
        st.devices = vec![dev];
        st.selected_device = Some(0);
        st.boot_selection = BootSelection::NonBootable;
        st.recompute();
        assert!(st.build_confirm().is_err());
    }

    #[test]
    fn renamed_fixed_vhd_cannot_produce_a_write_request() {
        let path = std::env::temp_dir().join(format!(
            "rufus-renamed-vhd-{}-{}.img",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        let mut bytes = vec![0u8; 4096 + 512];
        bytes[4096..4104].copy_from_slice(b"conectix");
        std::fs::write(&path, bytes).expect("write renamed VHD fixture");
        let mut st = AppState::new();
        st.devices = vec![sample_device()];
        st.selected_device = Some(0);
        st.set_image(path.clone());
        assert_eq!(
            st.image_report.as_ref().map(|report| report.kind),
            Some(ImageSourceKind::Vhd)
        );
        let error = st
            .build_helper_request()
            .expect_err("this footer-only fixture is not a writable virtual disk");
        assert!(
            error.contains("qemu-nbd") || error.contains("inspect a valid VHD/VHDX"),
            "recognized VHD must not be raw-written: {error}"
        );

        std::fs::write(&path, vec![0u8; 4096]).expect("replace with raw fixture");
        st.set_image(path.clone());
        assert!(st.build_helper_request().is_ok());
        std::fs::remove_file(path).expect("remove image fixture");
    }

    #[test]
    fn refs_plan_rejected() {
        let mut st = AppState::new();
        st.devices = vec![sample_device()];
        st.filesystem_label = "ReFS (unavailable)".into();
        st.boot_selection = BootSelection::NonBootable;
        assert!(st.build_plan().is_err());
    }

    fn windows_report() -> ImageReport {
        let mut report = report_with_label(Some("CCCOMA_X64FRE_EN-US_DV9"));
        report.kind = ImageSourceKind::Iso;
        report.windows_installer = true;
        report.has_efi = true;
        report.has_bios = true;
        report.preferred_filesystem = Some(FileSystem::Ntfs);
        report.largest_file_bytes = Some(8_155_984_950);
        report
    }

    #[test]
    fn windows_iso_options_follow_upstream_rules() {
        let mut st = AppState::new();
        st.backend = Ok(Backend::Udisks {
            version: "2.11.2".into(),
        });
        st.devices = vec![sample_device()];
        st.selected_device = Some(0);
        inspect(&mut st, windows_report());
        st.filesystem_label = "NTFS".into();
        st.recompute();
        assert_eq!(st.partition_scheme_label, "GPT");
        assert_eq!(st.target_system_label, "UEFI (non CSM)");
        assert_eq!(st.windows_media_unavailable_reason(), None);
        assert_eq!(
            st.build_plan().expect("Windows plan").write_mode,
            WriteMode::IsoFileCopy
        );

        st.filesystem_label = "FAT32".into();
        let reason = st
            .windows_media_unavailable_reason()
            .expect("FAT32 too small");
        assert!(reason.contains("larger than 4 GB"), "{reason}");

        st.filesystem_label = "exFAT".into();
        assert_eq!(st.windows_media_unavailable_reason(), None);

        st.filesystem_label = "NTFS".into();
        st.select_partition_scheme("MBR");
        assert_eq!(st.target_system_label, "BIOS or UEFI");
        assert_eq!(st.windows_media_unavailable_reason(), None);
        st.filesystem_label = "exFAT".into();
        assert!(st
            .windows_media_unavailable_reason()
            .expect("exFAT has no BIOS boot record")
            .contains("UEFI only"));

        st.filesystem_label = "NTFS".into();
        st.target_system_label = "BIOS (CSM)".into();
        st.select_partition_scheme("GPT");
        assert_eq!(st.target_system_label, "UEFI (non CSM)");

        st.backend = Ok(Backend::NativeHelper);
        assert!(st
            .windows_media_unavailable_reason()
            .expect("native helper cannot copy files")
            .contains("udisks2"));
    }

    #[test]
    fn non_windows_iso_still_explains_the_missing_file_copy_mode() {
        let mut st = AppState::new();
        let mut report = windows_report();
        report.windows_installer = false;
        inspect(&mut st, report);
        assert!(st
            .windows_media_unavailable_reason()
            .expect("Linux file-copy is not available")
            .contains("ISOHybrid"));
    }

    #[test]
    fn super_floppy_is_offered_for_plain_formats_only() {
        let mut st = AppState::new();
        assert!(!st.partition_choices().contains(&SUPER_FLOPPY_LABEL));

        st.select_boot(BootSelection::NonBootable);
        assert_eq!(st.partition_choices(), ["MBR", "GPT", SUPER_FLOPPY_LABEL]);
        st.select_partition_scheme(SUPER_FLOPPY_LABEL);
        assert_eq!(st.parse_scheme(), PartitionScheme::SuperFloppy);

        st.select_boot(BootSelection::DiskOrIso);
        assert_eq!(st.partition_scheme_label, "GPT");
        assert_eq!(st.target_system_label, "UEFI (non CSM)");
        assert!(!st.partition_choices().contains(&SUPER_FLOPPY_LABEL));

        st.select_boot(BootSelection::NonBootable);
        st.select_partition_scheme("MBR");
        st.select_boot(BootSelection::DiskOrIso);
        assert_eq!(
            st.partition_scheme_label, "MBR",
            "a still-offered scheme is kept"
        );
    }
}
