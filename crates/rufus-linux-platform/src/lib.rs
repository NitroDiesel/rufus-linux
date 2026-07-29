//! Linux platform integration kept separate from the portable planning core.
//!
//! Discovery reads sysfs rather than parsing human-facing `lsblk` output.
//! Destructive jobs must revalidate a returned [`DeviceIdentity`] inside the
//! privileged helper immediately before opening the block device.

use std::fmt;
use std::path::{Path, PathBuf};

use rufus_core::capability::{Capability, CapabilityReport, CapabilityState, MissingRequirement};
use rufus_core::device::{
    BlockDevice, DeviceClass, DeviceFingerprint, DeviceNumber, DeviceRisk, Transport,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub node: PathBuf,
    pub sysfs_path: PathBuf,
    pub kernel_name: String,
    pub major: u32,
    pub minor: u32,
    pub size_bytes: u64,
    pub logical_sector_size: u32,
    pub model: String,
    pub vendor: String,
    pub serial: String,
    pub transport: String,
    pub removable: bool,
    pub read_only: bool,
}

impl DeviceIdentity {
    pub fn stable_key(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.major, self.minor, self.size_bytes, self.serial
        )
    }

    pub fn display_name(&self) -> String {
        let product = [self.vendor.trim(), self.model.trim()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if product.is_empty() {
            self.kernel_name.clone()
        } else {
            product
        }
    }

    pub fn to_block_device(&self, mounts: &[MountRecord]) -> BlockDevice {
        let mut risks = Vec::new();
        let device_mounts = mounts
            .iter()
            .filter(|mount| mount_belongs_to_identity(mount, self))
            .collect::<Vec<_>>();
        if self.read_only {
            risks.push(DeviceRisk::ReadOnly);
        }
        if self.size_bytes == 0 {
            risks.push(DeviceRisk::IdentityUnstable);
        }
        if !self.removable && self.transport != "usb" {
            risks.push(DeviceRisk::Internal);
        }
        if device_mounts
            .iter()
            .any(|mount| is_critical_mount(&mount.mount_point))
        {
            if device_mounts.iter().any(|mount| {
                matches!(
                    mount.mount_point.to_string_lossy().as_ref(),
                    "/" | "/usr" | "/home"
                )
            }) {
                risks.push(DeviceRisk::ContainsRoot);
            }
            if device_mounts.iter().any(|mount| {
                matches!(
                    mount.mount_point.to_string_lossy().as_ref(),
                    "/boot" | "/boot/efi" | "/efi"
                )
            }) {
                risks.push(DeviceRisk::ContainsBoot);
            }
        }
        if device_mounts.iter().any(|mount| mount.filesystem == "swap") {
            risks.push(DeviceRisk::ContainsSwap);
        }
        if device_mounts
            .iter()
            .any(|mount| mount.mount_point != Path::new("/"))
            && !risks.contains(&DeviceRisk::ContainsRoot)
        {
            risks.push(DeviceRisk::MountedChildren);
        }

        let class = if self.removable {
            DeviceClass::Removable
        } else if self.transport == "usb" {
            DeviceClass::ExternalFixed
        } else if self.transport == "loop" || self.kernel_name.starts_with("loop") {
            DeviceClass::Virtual
        } else {
            DeviceClass::Internal
        };

        let transport = match self.transport.as_str() {
            "usb" => Transport::Usb,
            "nvme" => Transport::Nvme,
            "mmc" => Transport::Mmc,
            "virtio" => Transport::Virtio,
            "ata" | "sata" => Transport::Sata,
            "scsi" => Transport::Scsi,
            "loop" => Transport::Loop,
            "unknown" => Transport::Unknown,
            _ => Transport::Other,
        };

        BlockDevice {
            node: self.node.clone(),
            display_name: self.display_name(),
            vendor: nonempty(self.vendor.clone()),
            model: nonempty(self.model.clone()),
            class,
            transport,
            fingerprint: DeviceFingerprint {
                number: DeviceNumber::new(self.major, self.minor),
                canonical_sysfs_path: self.sysfs_path.clone(),
                size_bytes: self.size_bytes,
                logical_block_size: self.logical_sector_size,
                serial: nonempty(self.serial.clone()),
                wwn: None,
            },
            risks,
        }
    }
}

fn nonempty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn is_critical_mount(path: &Path) -> bool {
    matches!(
        path.to_string_lossy().as_ref(),
        "/" | "/usr" | "/boot" | "/boot/efi" | "/efi" | "/home"
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlatformError {
    Unsupported(&'static str),
    Io(String),
    InvalidKernelData { path: PathBuf, value: String },
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(message) => f.write_str(message),
            Self::Io(message) => f.write_str(message),
            Self::InvalidKernelData { path, value } => {
                write!(f, "invalid kernel value `{value}` in {}", path.display())
            }
        }
    }
}

impl std::error::Error for PlatformError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountRecord {
    pub major: u32,
    pub minor: u32,
    pub mount_point: PathBuf,
    pub source: String,
    pub filesystem: String,
}

/// Return whether a mounted block node is this disk or one of its partitions.
pub fn mount_belongs_to_identity(mount: &MountRecord, device: &DeviceIdentity) -> bool {
    let dev_link = PathBuf::from(format!("/sys/dev/block/{}:{}", mount.major, mount.minor));
    if let Ok(mounted_sysfs) = std::fs::canonicalize(dev_link) {
        if mounted_sysfs.starts_with(&device.sysfs_path) {
            return true;
        }
    }

    source_is_device_or_partition(&mount.source, &device.node)
}

/// Conservative source-on-target check based on the longest containing mount.
pub fn path_is_on_block_device(path: &Path, device: &BlockDevice) -> bool {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let mut mounts = mount_records().unwrap_or_default();
    mounts.sort_by_key(|mount| std::cmp::Reverse(mount.mount_point.as_os_str().len()));
    let Some(mount) = mounts
        .iter()
        .find(|mount| canonical.starts_with(&mount.mount_point))
    else {
        return false;
    };

    let dev_link = PathBuf::from(format!("/sys/dev/block/{}:{}", mount.major, mount.minor));
    std::fs::canonicalize(dev_link)
        .map(|mounted_sysfs| mounted_sysfs.starts_with(&device.fingerprint.canonical_sysfs_path))
        .unwrap_or_else(|_| source_is_device_or_partition(&mount.source, &device.node))
}

fn source_is_device_or_partition(source: &str, disk: &Path) -> bool {
    let disk = disk.to_string_lossy();
    if source == disk {
        return true;
    }
    let suffix = source.strip_prefix(disk.as_ref()).unwrap_or_default();
    if disk
        .chars()
        .last()
        .is_some_and(|character| character.is_ascii_digit())
    {
        suffix
            .strip_prefix('p')
            .is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
    } else {
        !suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit())
    }
}

pub fn discover_devices(show_fixed: bool) -> Result<Vec<DeviceIdentity>, PlatformError> {
    discover_devices_from(Path::new("/sys/class/block"), show_fixed)
}

pub fn discover_devices_from(
    sys_block: &Path,
    show_fixed: bool,
) -> Result<Vec<DeviceIdentity>, PlatformError> {
    #[cfg(target_os = "linux")]
    {
        linux::discover_devices_from(sys_block, show_fixed)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (sys_block, show_fixed);
        Err(PlatformError::Unsupported(
            "block-device discovery is available only on Linux",
        ))
    }
}

/// Discover devices and map them into core [`BlockDevice`] values with risk tags.
pub fn list_block_devices(show_fixed: bool) -> Result<Vec<BlockDevice>, PlatformError> {
    let mounts = mount_records().unwrap_or_default();
    let identities = discover_devices(show_fixed)?;
    Ok(identities
        .into_iter()
        .map(|id| id.to_block_device(&mounts))
        .collect())
}

pub fn mount_records() -> Result<Vec<MountRecord>, PlatformError> {
    parse_mountinfo(Path::new("/proc/self/mountinfo"))
}

pub fn parse_mountinfo(path: &Path) -> Result<Vec<MountRecord>, PlatformError> {
    let input = std::fs::read_to_string(path).map_err(|error| {
        PlatformError::Io(format!("could not read {}: {error}", path.display()))
    })?;
    let mut records = Vec::new();
    for line in input.lines() {
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        let left = before.split_whitespace().collect::<Vec<_>>();
        let right = after.split_whitespace().collect::<Vec<_>>();
        if left.len() < 5 || right.len() < 2 {
            continue;
        }
        let Some((major, minor)) = left[2].split_once(':') else {
            continue;
        };
        let (Ok(major), Ok(minor)) = (major.parse(), minor.parse()) else {
            continue;
        };
        records.push(MountRecord {
            major,
            minor,
            mount_point: PathBuf::from(unescape_mount_field(left[4])),
            filesystem: right[0].to_owned(),
            source: unescape_mount_field(right[1]),
        });
    }
    Ok(records)
}

fn unescape_mount_field(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

/// Probe installed tools and return a capability report.
pub fn probe_capabilities() -> CapabilityReport {
    let mut report = CapabilityReport::new();

    let set_tool = |report: &mut CapabilityReport,
                    cap: Capability,
                    tools: &[&str],
                    packages: &str| {
        let found = tools.iter().any(|t| Path::new(t).exists());
        if found {
            report.set(cap, CapabilityState::Available);
        } else {
            report.set(
                cap,
                CapabilityState::Unavailable {
                    missing: vec![MissingRequirement {
                        id: tools[0].to_owned(),
                        explanation: format!("required tool not found ({})", tools.join(" or ")),
                        remedy: Some(format!("install package providing these tools: {packages}")),
                    }],
                },
            );
        }
    };

    report.set(Capability::RawImageWrite, CapabilityState::Available);
    set_tool(
        &mut report,
        Capability::CompressedImageWrite,
        &[
            "/usr/bin/gzip",
            "/usr/bin/xz",
            "/usr/bin/bzip2",
            "/usr/bin/zstd",
            "/usr/bin/bsdtar",
        ],
        "gzip / xz / bzip2 / zstd / libarchive",
    );
    report.set(
        Capability::IsoExtraction,
        CapabilityState::Unavailable {
            missing: vec![MissingRequirement {
                id: "iso-file-copy".into(),
                explanation: "ISO file-copy boot setup is not implemented in this release".into(),
                remedy: Some("use a hybrid ISO in disk-image mode".into()),
            }],
        },
    );
    report.set(
        Capability::ImageCaptureDd,
        CapabilityState::Unavailable {
            missing: vec![MissingRequirement {
                id: "fd-passing".into(),
                explanation: "safe helper-to-user file descriptor passing is not implemented"
                    .into(),
                remedy: None,
            }],
        },
    );
    report.set(
        Capability::ImageCaptureVhd,
        if Path::new("/usr/bin/qemu-img").exists() {
            CapabilityState::Available
        } else {
            CapabilityState::Unavailable {
                missing: vec![MissingRequirement {
                    id: "qemu-img".into(),
                    explanation: "VHD capture needs qemu-img".into(),
                    remedy: Some("install qemu-utils / qemu-img / qemu-base".into()),
                }],
            }
        },
    );
    report.set(
        Capability::ImageCaptureVhdx,
        report
            .state(Capability::ImageCaptureVhd)
            .cloned()
            .unwrap_or(CapabilityState::Available),
    );
    report.set(
        Capability::ImageCaptureFfu,
        CapabilityState::Unavailable {
            missing: vec![MissingRequirement {
                id: "linux-only".into(),
                explanation: "FFU apply/capture is not available on Linux".into(),
                remedy: None,
            }],
        },
    );

    set_tool(
        &mut report,
        Capability::FormatFat,
        &["/usr/sbin/mkfs.fat", "/sbin/mkfs.fat", "/usr/bin/mkfs.fat"],
        "dosfstools",
    );
    set_tool(
        &mut report,
        Capability::FormatFat32,
        &["/usr/sbin/mkfs.fat", "/sbin/mkfs.fat", "/usr/bin/mkfs.fat"],
        "dosfstools",
    );
    set_tool(
        &mut report,
        Capability::FormatExfat,
        &[
            "/usr/sbin/mkfs.exfat",
            "/sbin/mkfs.exfat",
            "/usr/bin/mkfs.exfat",
        ],
        "exfatprogs",
    );
    set_tool(
        &mut report,
        Capability::FormatNtfs,
        &[
            "/usr/sbin/mkfs.ntfs",
            "/sbin/mkfs.ntfs",
            "/usr/bin/mkfs.ntfs",
        ],
        "ntfs-3g",
    );
    set_tool(
        &mut report,
        Capability::FormatUdf,
        &["/usr/sbin/mkudffs", "/sbin/mkudffs", "/usr/bin/mkudffs"],
        "udftools",
    );
    set_tool(
        &mut report,
        Capability::FormatExt2,
        &["/usr/sbin/mke2fs", "/sbin/mke2fs", "/usr/bin/mke2fs"],
        "e2fsprogs",
    );
    set_tool(
        &mut report,
        Capability::FormatExt3,
        &["/usr/sbin/mke2fs", "/sbin/mke2fs"],
        "e2fsprogs",
    );
    set_tool(
        &mut report,
        Capability::FormatExt4,
        &["/usr/sbin/mke2fs", "/sbin/mke2fs"],
        "e2fsprogs",
    );
    report.set(
        Capability::FormatRefs,
        CapabilityState::Unavailable {
            missing: vec![MissingRequirement {
                id: "linux-only".into(),
                explanation: "ReFS creation has no safe Linux formatter".into(),
                remedy: None,
            }],
        },
    );

    report.set(
        Capability::FreeDos,
        CapabilityState::Unavailable {
            missing: vec![MissingRequirement {
                id: "assets/freedos".into(),
                explanation: "FreeDOS assets not packaged yet".into(),
                remedy: Some("package redistributable FreeDOS files under assets/freedos".into()),
            }],
        },
    );
    set_tool(
        &mut report,
        Capability::Syslinux,
        &[
            "/usr/bin/syslinux",
            "/usr/sbin/syslinux",
            "/usr/bin/extlinux",
        ],
        "syslinux",
    );
    set_tool(
        &mut report,
        Capability::Grub,
        &[
            "/usr/sbin/grub-install",
            "/usr/sbin/grub2-install",
            "/usr/bin/grub-install",
        ],
        "grub / grub2",
    );
    report.set(
        Capability::UefiNtfs,
        CapabilityState::Unavailable {
            missing: vec![MissingRequirement {
                id: "assets/uefi/uefi-ntfs.img".into(),
                explanation: "signed UEFI:NTFS payload not packaged".into(),
                remedy: Some("vendor verified UEFI:NTFS payload under assets/uefi".into()),
            }],
        },
    );
    report.set(
        Capability::LinuxPersistence,
        CapabilityState::Unavailable {
            missing: vec![MissingRequirement {
                id: "persistence-layout".into(),
                explanation: "persistent partition creation is not implemented in this release"
                    .into(),
                remedy: None,
            }],
        },
    );
    report.set(
        Capability::WindowsInstallerCustomization,
        if Path::new("/usr/bin/wimlib-imagex").exists() || Path::new("/usr/bin/wiminfo").exists() {
            CapabilityState::Available
        } else {
            CapabilityState::Unavailable {
                missing: vec![MissingRequirement {
                    id: "wimlib-imagex".into(),
                    explanation: "Windows image work needs wimlib".into(),
                    remedy: Some("install wimtools / wimlib-utils / wimlib".into()),
                }],
            }
        },
    );
    report.set(
        Capability::WindowsToGo,
        CapabilityState::Unavailable {
            missing: vec![MissingRequirement {
                id: "experimental".into(),
                explanation: "Windows To Go remains experimental on Linux".into(),
                remedy: Some("install wimlib; expect incomplete Windows servicing parity".into()),
            }],
        },
    );
    report.set(Capability::WindowsIsoDownload, CapabilityState::Available);
    report.set(Capability::UefiShellDownload, CapabilityState::Available);
    report.set(Capability::ImageChecksums, CapabilityState::Available);
    report.set(
        Capability::UefiMediaValidation,
        CapabilityState::Unavailable {
            missing: vec![MissingRequirement {
                id: "uefi-validation-payload".into(),
                explanation: "verified UEFI validation payload is not packaged".into(),
                remedy: None,
            }],
        },
    );
    set_tool(
        &mut report,
        Capability::BadBlocksCheck,
        &["/usr/sbin/badblocks", "/sbin/badblocks"],
        "e2fsprogs",
    );

    report
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{DeviceIdentity, PlatformError};
    use std::fs;
    use std::path::{Path, PathBuf};

    pub(super) fn discover_devices_from(
        sys_block: &Path,
        show_fixed: bool,
    ) -> Result<Vec<DeviceIdentity>, PlatformError> {
        let entries = fs::read_dir(sys_block).map_err(|error| {
            PlatformError::Io(format!("could not read {}: {error}", sys_block.display()))
        })?;
        let mut devices = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| PlatformError::Io(error.to_string()))?;
            let kernel_name = entry.file_name().to_string_lossy().into_owned();
            if is_partition_name(&kernel_name)
                || kernel_name.starts_with("loop")
                || kernel_name.starts_with("ram")
                || kernel_name.starts_with("dm-")
                || kernel_name.starts_with("zram")
            {
                continue;
            }
            let path = entry.path();
            let removable = read_trimmed(path.join("removable")) == "1";
            let transport =
                infer_transport(&fs::canonicalize(&path).unwrap_or_else(|_| path.clone()));
            // USB non-removable (HDD/SSD enclosures) stay hidden unless show_fixed.
            let is_usb = transport == "usb";
            if !show_fixed && !removable && !is_usb {
                continue;
            }
            // Default list: removable media only. USB HDD requires show_fixed flag
            // (mapped from advanced "list USB hard drives" in the UI).
            if !show_fixed && !removable {
                continue;
            }
            let Some((major, minor)) = parse_dev(&path.join("dev"))? else {
                continue;
            };
            let sectors = match parse_u64(&path.join("size")) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let logical_sector_size = parse_u64(&path.join("queue/logical_block_size"))
                .unwrap_or(512)
                .try_into()
                .unwrap_or(512);
            let size_bytes = match sectors.checked_mul(512) {
                Some(v) => v,
                None => continue,
            };
            // Skip tiny devices (floppy-like) under 8 MiB — matches upstream minimum.
            if size_bytes < 8 * 1024 * 1024 {
                continue;
            }
            let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            devices.push(DeviceIdentity {
                node: PathBuf::from("/dev").join(&kernel_name),
                sysfs_path: canonical.clone(),
                kernel_name,
                major,
                minor,
                size_bytes,
                logical_sector_size,
                model: read_trimmed(canonical.join("device/model")),
                vendor: read_trimmed(canonical.join("device/vendor")),
                serial: first_nonempty(&[
                    read_trimmed(canonical.join("device/serial")),
                    read_trimmed(canonical.join("serial")),
                    read_by_id_serial(&path),
                ]),
                transport,
                removable,
                read_only: read_trimmed(path.join("ro")) == "1",
            });
        }
        devices.sort_by(|a, b| a.node.cmp(&b.node));
        Ok(devices)
    }

    fn first_nonempty(values: &[String]) -> String {
        values
            .iter()
            .find(|v| !v.is_empty())
            .cloned()
            .unwrap_or_default()
    }

    fn read_by_id_serial(_path: &Path) -> String {
        String::new()
    }

    fn read_trimmed(path: impl AsRef<Path>) -> String {
        fs::read_to_string(path)
            .unwrap_or_default()
            .trim_matches(char::from(0))
            .trim()
            .to_owned()
    }

    fn parse_u64(path: &Path) -> Result<u64, PlatformError> {
        let value = read_trimmed(path);
        value.parse().map_err(|_| PlatformError::InvalidKernelData {
            path: path.to_owned(),
            value,
        })
    }

    fn parse_dev(path: &Path) -> Result<Option<(u32, u32)>, PlatformError> {
        let value = read_trimmed(path);
        let Some((major, minor)) = value.split_once(':') else {
            return Ok(None);
        };
        let major = major
            .parse()
            .map_err(|_| PlatformError::InvalidKernelData {
                path: path.to_owned(),
                value: value.clone(),
            })?;
        let minor = minor
            .parse()
            .map_err(|_| PlatformError::InvalidKernelData {
                path: path.to_owned(),
                value,
            })?;
        Ok(Some((major, minor)))
    }

    fn infer_transport(path: &Path) -> String {
        let text = path.to_string_lossy();
        for candidate in ["usb", "nvme", "mmc", "virtio", "ata", "scsi"] {
            if text.contains(candidate) {
                return candidate.to_owned();
            }
        }
        "unknown".to_owned()
    }

    fn is_partition_name(name: &str) -> bool {
        // nvme0n1p1, mmcblk0p1
        if let Some((_, suffix)) = name.rsplit_once('p') {
            if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                // Avoid treating nvme0n1 as partition: need digit before p for nvme style
                if name.contains("nvme") || name.contains("mmcblk") || name.contains("loop") {
                    return true;
                }
            }
        }
        // sda1, vda2
        if name.starts_with("sd") || name.starts_with("vd") || name.starts_with("hd") {
            return name.chars().last().is_some_and(|c| c.is_ascii_digit());
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_mountinfo_and_escapes() {
        let path = std::env::temp_dir().join(format!(
            "rufus-mountinfo-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        std::fs::write(
            &path,
            "36 25 8:17 / /media/My\\040USB rw,nosuid - vfat /dev/sdb1 rw\n",
        )
        .expect("write mountinfo fixture");
        let records = parse_mountinfo(&path).expect("parse mountinfo fixture");
        let _ = std::fs::remove_file(path);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].major, 8);
        assert_eq!(records[0].minor, 17);
        assert_eq!(records[0].mount_point, PathBuf::from("/media/My USB"));
        assert_eq!(records[0].filesystem, "vfat");
    }

    #[test]
    fn capability_probe_marks_refs_unavailable() {
        let report = probe_capabilities();
        assert!(!report.supports(Capability::FormatRefs));
        assert!(!report.supports(Capability::ImageCaptureFfu));
    }

    #[test]
    fn identity_maps_internal_risk() {
        let id = DeviceIdentity {
            node: PathBuf::from("/dev/sda"),
            sysfs_path: PathBuf::from("/sys/devices/pci/ata/sda"),
            kernel_name: "sda".into(),
            major: 8,
            minor: 0,
            size_bytes: 512_000_000_000,
            logical_sector_size: 512,
            model: "SYSTEM".into(),
            vendor: "ATA".into(),
            serial: "SYS1".into(),
            transport: "ata".into(),
            removable: false,
            read_only: false,
        };
        let dev = id.to_block_device(&[]);
        assert!(dev.has_risk(DeviceRisk::Internal));
        assert_eq!(dev.class, DeviceClass::Internal);
    }

    #[test]
    fn root_mount_tags_contains_root() {
        let id = DeviceIdentity {
            node: PathBuf::from("/dev/sda"),
            sysfs_path: PathBuf::from("/sys/devices/pci/ata/sda"),
            kernel_name: "sda".into(),
            major: 8,
            minor: 0,
            size_bytes: 512_000_000_000,
            logical_sector_size: 512,
            model: "SYSTEM".into(),
            vendor: "ATA".into(),
            serial: "SYS1".into(),
            transport: "ata".into(),
            removable: false,
            read_only: false,
        };
        let mounts = vec![MountRecord {
            major: 8,
            minor: 1,
            mount_point: PathBuf::from("/"),
            source: "/dev/sda1".into(),
            filesystem: "ext4".into(),
        }];
        let dev = id.to_block_device(&mounts);
        assert!(dev.has_risk(DeviceRisk::ContainsRoot));
    }

    #[test]
    fn unrelated_disk_with_same_major_is_not_tagged_as_root() {
        let id = DeviceIdentity {
            node: PathBuf::from("/dev/sdb"),
            sysfs_path: PathBuf::from("/sys/devices/pci/usb/block/sdb"),
            kernel_name: "sdb".into(),
            major: 8,
            minor: 16,
            size_bytes: 32_000_000_000,
            logical_sector_size: 512,
            model: "USB".into(),
            vendor: "TEST".into(),
            serial: "USB1".into(),
            transport: "usb".into(),
            removable: true,
            read_only: false,
        };
        let mounts = vec![MountRecord {
            major: 8,
            minor: 1,
            mount_point: PathBuf::from("/"),
            source: "/dev/sda1".into(),
            filesystem: "ext4".into(),
        }];
        let dev = id.to_block_device(&mounts);
        assert!(!dev.has_risk(DeviceRisk::ContainsRoot));
    }
}
