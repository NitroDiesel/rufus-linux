//! Privileged operation engine. The binary is a thin CLI around this library
//! so unit tests can exercise validation and dry-run without root.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use rufus_core::plan::{FileSystem, PartitionScheme, VerificationLevel, WriteMode};
use rufus_core::progress::{Cancellability, CancellationToken, JobId, ProgressStage, ProgressUnit};
use rufus_helper_protocol::{
    encode_line, FormatSpec, HelperEvent, HelperOperation, HelperRequest, HelperResult,
    ProtocolError, TargetIdentity,
};
use rufus_linux_platform::{
    discover_devices, mount_belongs_to_identity, mount_records, path_is_on_block_device,
    DeviceIdentity, PlatformError,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Error)]
pub enum HelperError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Platform(#[from] PlatformError),
    #[error("target revalidation failed: {0}")]
    Revalidation(String),
    #[error("operation failed: {0}")]
    Operation(String),
    #[error("cancelled")]
    Cancelled,
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("tool missing: {0}")]
    MissingTool(String),
}

/// Fixed absolute paths for formatters. Never taken from the environment.
pub mod tools {
    pub const PARTED: &[&str] = &["/usr/sbin/parted", "/usr/bin/parted", "/sbin/parted"];
    pub const MKFS_FAT: &[&str] = &["/usr/sbin/mkfs.fat", "/usr/bin/mkfs.fat", "/sbin/mkfs.fat"];
    pub const MKFS_EXFAT: &[&str] = &[
        "/usr/sbin/mkfs.exfat",
        "/usr/bin/mkfs.exfat",
        "/sbin/mkfs.exfat",
    ];
    pub const MKFS_NTFS: &[&str] = &[
        "/usr/sbin/mkfs.ntfs",
        "/usr/bin/mkfs.ntfs",
        "/sbin/mkfs.ntfs",
    ];
    pub const MKE2FS: &[&str] = &["/usr/sbin/mke2fs", "/usr/bin/mke2fs", "/sbin/mke2fs"];
    pub const MKUDFFS: &[&str] = &["/usr/sbin/mkudffs", "/usr/bin/mkudffs", "/sbin/mkudffs"];
    pub const BLOCKDEV: &[&str] = &["/usr/sbin/blockdev", "/usr/bin/blockdev", "/sbin/blockdev"];
    pub const UDEVADM: &[&str] = &["/usr/bin/udevadm", "/usr/sbin/udevadm", "/sbin/udevadm"];
    pub const UMOUNT: &[&str] = &["/usr/bin/umount", "/usr/sbin/umount", "/bin/umount"];
    pub const SWAPOFF: &[&str] = &["/usr/sbin/swapoff", "/usr/bin/swapoff", "/sbin/swapoff"];
    pub const BADBLOCKS: &[&str] = &[
        "/usr/sbin/badblocks",
        "/usr/bin/badblocks",
        "/sbin/badblocks",
    ];
    pub const GZIP: &[&str] = &["/usr/bin/gzip", "/bin/gzip"];
    pub const BZIP2: &[&str] = &["/usr/bin/bzip2", "/bin/bzip2"];
    pub const XZ: &[&str] = &["/usr/bin/xz", "/bin/xz"];
    pub const ZSTD: &[&str] = &["/usr/bin/zstd", "/bin/zstd"];
    pub const BSDTAR: &[&str] = &["/usr/bin/bsdtar", "/bin/bsdtar"];
}

fn find_tool(candidates: &'static [&'static str]) -> Result<&'static str, HelperError> {
    candidates
        .iter()
        .copied()
        .find(|path| Path::new(path).is_file())
        .ok_or_else(|| HelperError::MissingTool(candidates.join(" or ")))
}

pub fn tool_for_filesystem(fs: FileSystem) -> Result<&'static str, HelperError> {
    let candidates = match fs {
        FileSystem::Fat | FileSystem::Fat32 => tools::MKFS_FAT,
        FileSystem::ExFat => tools::MKFS_EXFAT,
        FileSystem::Ntfs => tools::MKFS_NTFS,
        FileSystem::Ext2 | FileSystem::Ext3 | FileSystem::Ext4 => tools::MKE2FS,
        FileSystem::Udf => tools::MKUDFFS,
        FileSystem::Refs => {
            return Err(HelperError::Operation(
                "ReFS creation is not available on Linux".into(),
            ));
        }
    };
    find_tool(candidates)
}

/// Re-resolve the target and compare fingerprints strictly.
pub fn revalidate_target(expected: &TargetIdentity) -> Result<DeviceIdentity, HelperError> {
    let devices = discover_devices(true)?;
    let observed = devices
        .into_iter()
        .find(|device| device.node == expected.node)
        .ok_or_else(|| {
            HelperError::Revalidation(format!(
                "device {} no longer present",
                expected.node.display()
            ))
        })?;

    if observed.major != expected.fingerprint.number.major
        || observed.minor != expected.fingerprint.number.minor
    {
        return Err(HelperError::Revalidation(
            "device major:minor mismatch".into(),
        ));
    }
    if observed.size_bytes != expected.fingerprint.size_bytes {
        return Err(HelperError::Revalidation("device size changed".into()));
    }
    if observed.logical_sector_size != expected.fingerprint.logical_block_size {
        return Err(HelperError::Revalidation(
            "logical sector size changed".into(),
        ));
    }
    if let Some(serial) = &expected.fingerprint.serial {
        if !serial.is_empty() && observed.serial != *serial && !observed.serial.is_empty() {
            return Err(HelperError::Revalidation("device serial mismatch".into()));
        }
    }
    if observed.read_only {
        return Err(HelperError::Revalidation("device is read-only".into()));
    }
    let metadata = std::fs::metadata(&observed.node)?;
    if !metadata.file_type().is_block_device() {
        return Err(HelperError::Revalidation(
            "target is not a block device".into(),
        ));
    }

    let mounts = mount_records().unwrap_or_default();
    let block = observed.to_block_device(&mounts);
    for risk in [
        rufus_core::device::DeviceRisk::ContainsRoot,
        rufus_core::device::DeviceRisk::ContainsBoot,
        rufus_core::device::DeviceRisk::ContainsSwap,
        rufus_core::device::DeviceRisk::ActiveRaidMember,
        rufus_core::device::DeviceRisk::ActiveVolumeMember,
        rufus_core::device::DeviceRisk::ReadOnly,
        rufus_core::device::DeviceRisk::HasDependents,
        rufus_core::device::DeviceRisk::IdentityUnstable,
    ] {
        if block.has_risk(risk) {
            return Err(HelperError::Revalidation(format!(
                "target has blocking risk: {risk:?}"
            )));
        }
    }
    if sysfs_has_holders(&observed)? {
        return Err(HelperError::Revalidation(
            "target or a partition has active device-mapper/RAID holders".into(),
        ));
    }
    Ok(observed)
}

fn sysfs_has_holders(device: &DeviceIdentity) -> Result<bool, HelperError> {
    let class_path = Path::new("/sys/class/block").join(&device.kernel_name);
    let mut nodes = vec![class_path.clone()];
    if let Ok(entries) = std::fs::read_dir(&class_path) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(&device.kernel_name) {
                nodes.push(entry.path());
            }
        }
    }
    for node in nodes {
        let holders = node.join("holders");
        if let Ok(mut entries) = std::fs::read_dir(holders) {
            if entries.next().is_some() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Validate a request without performing destructive I/O.
pub fn validate_request(request: &HelperRequest) -> Result<(), HelperError> {
    request.validate()?;
    match &request.operation {
        HelperOperation::WriteMedia {
            write_mode,
            source,
            format,
            install_bootloader,
            ..
        } => {
            if *write_mode != WriteMode::DdImage {
                return Err(HelperError::Operation(
                    "only raw/ISOHybrid disk-image writing is enabled in this release".into(),
                ));
            }
            if !matches!(
                source.kind,
                rufus_core::plan::ImageSourceKind::Raw
                    | rufus_core::plan::ImageSourceKind::IsoHybrid
                    | rufus_core::plan::ImageSourceKind::CompressedRaw
            ) {
                return Err(HelperError::Operation(
                    "source kind cannot be safely raw-written".into(),
                ));
            }
            if install_bootloader.is_some() || format.persistence_bytes != 0 {
                return Err(HelperError::Operation(
                    "bootloader and persistence operations are not enabled".into(),
                ));
            }
            if !source.path.is_absolute() {
                return Err(HelperError::Protocol(ProtocolError::InvalidRequest(
                    "source must be absolute".into(),
                )));
            }
            let symlink_meta = std::fs::symlink_metadata(&source.path)?;
            if symlink_meta.file_type().is_symlink() {
                return Err(HelperError::Operation(
                    "source image symlinks are not accepted".into(),
                ));
            }
            let meta = std::fs::metadata(&source.path)?;
            if !meta.is_file() {
                return Err(HelperError::Operation(
                    "source is not a regular file".into(),
                ));
            }
            if meta.len() != source.size_bytes {
                return Err(HelperError::Operation(
                    "source image size changed since selection".into(),
                ));
            }
            if source.kind == rufus_core::plan::ImageSourceKind::CompressedRaw {
                decompressor_for(&source.path)?;
            }
        }
        HelperOperation::FormatMedia { format, .. } => {
            tool_for_filesystem(format.filesystem)?;
        }
        HelperOperation::CaptureImage { kind, .. } => {
            if *kind == rufus_core::plan::ImageSourceKind::Ffu {
                return Err(HelperError::Operation(
                    "FFU capture is not available on Linux".into(),
                ));
            }
            return Err(HelperError::Operation(
                "image capture is disabled until output file descriptors can be passed safely"
                    .into(),
            ));
        }
    }
    Ok(())
}

pub struct ExecutionOptions {
    /// When true, only validate and emit progress stages — no raw writes.
    pub dry_run: bool,
    pub cancel: CancellationToken,
}

impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            cancel: CancellationToken::new(),
        }
    }
}

pub type EventSink = Box<dyn FnMut(HelperEvent) + Send>;

fn emit(sink: &mut EventSink, event: HelperEvent) {
    sink(event);
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), HelperError> {
    if cancel.is_requested() {
        Err(HelperError::Cancelled)
    } else {
        Ok(())
    }
}

fn progress(
    job_id: JobId,
    stage: ProgressStage,
    completed: u64,
    total: Option<u64>,
    detail: impl Into<Option<String>>,
) -> HelperEvent {
    HelperEvent::Progress {
        job_id,
        stage,
        unit: if total.is_some() {
            ProgressUnit::Steps
        } else {
            ProgressUnit::Indeterminate
        },
        completed,
        total,
        bytes_per_second: None,
        detail: detail.into(),
        cancellability: Cancellability::AtStageBoundary,
    }
}

/// Execute (or dry-run) a validated helper request.
pub fn execute(
    request: HelperRequest,
    options: ExecutionOptions,
    mut sink: EventSink,
) -> Result<HelperResult, HelperError> {
    validate_request(&request)?;
    emit(
        &mut sink,
        HelperEvent::Accepted {
            job_id: request.job_id,
        },
    );

    let stages_total = 7u64;
    let mut step = 0u64;

    emit(
        &mut sink,
        progress(
            request.job_id,
            ProgressStage::Authorizing,
            step,
            Some(stages_total),
            Some("request accepted".into()),
        ),
    );
    step += 1;
    check_cancel(&options.cancel)?;

    emit(
        &mut sink,
        progress(
            request.job_id,
            ProgressStage::Preparing,
            step,
            Some(stages_total),
            Some("revalidating target".into()),
        ),
    );
    step += 1;

    let observed = if options.dry_run {
        emit(
            &mut sink,
            HelperEvent::Log {
                job_id: request.job_id,
                line: format!(
                    "dry-run: skip revalidation of {}",
                    request.target.node.display()
                ),
            },
        );
        None
    } else {
        let identity = revalidate_target(&request.target)?;
        if let HelperOperation::WriteMedia { source, .. } = &request.operation {
            let mounts = mount_records().unwrap_or_default();
            let block = identity.to_block_device(&mounts);
            if path_is_on_block_device(&source.path, &block) {
                return Err(HelperError::Revalidation(
                    "source image is stored on the target device".into(),
                ));
            }
            if source.decompressed_size_bytes.unwrap_or(source.size_bytes) > identity.size_bytes {
                return Err(HelperError::Operation(
                    "image is larger than the target device".into(),
                ));
            }
        }
        Some(identity)
    };
    let target_node = observed
        .as_ref()
        .map(|identity| identity.node.as_path())
        .unwrap_or(request.target.node.as_path());
    check_cancel(&options.cancel)?;

    emit(
        &mut sink,
        progress(
            request.job_id,
            ProgressStage::Unmounting,
            step,
            Some(stages_total),
            Some("unmount target".into()),
        ),
    );
    step += 1;
    if let Some(identity) = observed.as_ref() {
        unmount_target(identity)?;
    }
    check_cancel(&options.cancel)?;

    let exclusive = if options.dry_run {
        None
    } else {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(target_node)?;
        let opened = file.metadata()?;
        if !opened.file_type().is_block_device()
            || libc::major(opened.rdev()) != request.target.fingerprint.number.major
            || libc::minor(opened.rdev()) != request.target.fingerprint.number.minor
        {
            return Err(HelperError::Revalidation(
                "opened target does not match the authorized block device".into(),
            ));
        }
        file.try_lock_exclusive().map_err(|error| {
            HelperError::Operation(format!("target is busy or could not be locked: {error}"))
        })?;
        Some(file)
    };

    match &request.operation {
        HelperOperation::WriteMedia {
            write_mode,
            source,
            format: _,
            verification,
            bad_blocks,
            ..
        } => {
            if *bad_blocks {
                emit(
                    &mut sink,
                    progress(
                        request.job_id,
                        ProgressStage::TestingMedia,
                        step,
                        Some(stages_total),
                        Some("bad-blocks check".into()),
                    ),
                );
                if !options.dry_run {
                    run_badblocks(target_node)?;
                }
            }
            step += 1;
            check_cancel(&options.cancel)?;

            emit(
                &mut sink,
                progress(
                    request.job_id,
                    match write_mode {
                        WriteMode::IsoFileCopy => ProgressStage::ExtractingFiles,
                        _ => ProgressStage::WritingImage,
                    },
                    step,
                    Some(stages_total),
                    Some(format!("write mode {:?}", write_mode)),
                ),
            );
            step += 1;
            let receipt = if !options.dry_run {
                let target = exclusive.as_ref().ok_or_else(|| {
                    HelperError::Operation("exclusive target handle was not available".into())
                })?;
                Some(write_image(
                    target,
                    source,
                    *write_mode,
                    &options.cancel,
                    request.target.fingerprint.size_bytes,
                    request.job_id,
                    &mut sink,
                )?)
            } else {
                emit(
                    &mut sink,
                    HelperEvent::Log {
                        job_id: request.job_id,
                        line: format!(
                            "dry-run: would write {} -> {}",
                            source.path.display(),
                            target_node.display()
                        ),
                    },
                );
                None
            };
            check_cancel(&options.cancel)?;

            emit(
                &mut sink,
                progress(
                    request.job_id,
                    ProgressStage::Syncing,
                    step,
                    Some(stages_total),
                    Some("flush".into()),
                ),
            );
            step += 1;
            if !options.dry_run {
                if let Some(file) = exclusive.as_ref() {
                    file.sync_all()?;
                }
                flush_device(target_node)?;
            }

            if *verification != VerificationLevel::None {
                emit(
                    &mut sink,
                    progress(
                        request.job_id,
                        ProgressStage::Verifying,
                        step,
                        Some(stages_total),
                        Some(format!("{:?}", verification)),
                    ),
                );
                if !options.dry_run && *verification == VerificationLevel::FullReadback {
                    let receipt = receipt.as_ref().ok_or_else(|| {
                        HelperError::Operation("write receipt was not available".into())
                    })?;
                    verify_device_hash(
                        target_node,
                        receipt.bytes_written,
                        &receipt.sha256,
                        &options.cancel,
                    )?;
                }
            }
        }
        HelperOperation::FormatMedia { format, bad_blocks } => {
            if *bad_blocks && !options.dry_run {
                run_badblocks(target_node)?;
            }
            run_partition_format(
                request.job_id,
                target_node,
                format,
                options.dry_run,
                &mut sink,
                &mut step,
                stages_total,
            )?;
            emit(
                &mut sink,
                progress(
                    request.job_id,
                    ProgressStage::Syncing,
                    step,
                    Some(stages_total),
                    Some("flush".into()),
                ),
            );
            if !options.dry_run {
                flush_device(target_node)?;
            }
        }
        HelperOperation::CaptureImage { .. } => unreachable!("capture is rejected by validation"),
    }

    emit(
        &mut sink,
        progress(
            request.job_id,
            ProgressStage::Finalizing,
            stages_total,
            Some(stages_total),
            Some("complete".into()),
        ),
    );

    let result = HelperResult::Success;
    emit(
        &mut sink,
        HelperEvent::Finished {
            job_id: request.job_id,
            result: result.clone(),
        },
    );
    Ok(result)
}

fn run_partition_format(
    job_id: JobId,
    target_node: &Path,
    format: &FormatSpec,
    dry_run: bool,
    sink: &mut EventSink,
    step: &mut u64,
    stages_total: u64,
) -> Result<(), HelperError> {
    emit(
        sink,
        progress(
            job_id,
            ProgressStage::Wiping,
            *step,
            Some(stages_total),
            Some("wipe leading sectors".into()),
        ),
    );
    *step += 1;
    if !dry_run {
        wipe_leading(target_node)?;
    }

    emit(
        sink,
        progress(
            job_id,
            ProgressStage::Partitioning,
            *step,
            Some(stages_total),
            Some(format!("{:?}", format.scheme)),
        ),
    );
    *step += 1;
    if !dry_run {
        create_partition_table(target_node, format.scheme)?;
    }

    emit(
        sink,
        progress(
            job_id,
            ProgressStage::Formatting,
            *step,
            Some(stages_total),
            Some(format.filesystem.as_str().into()),
        ),
    );
    *step += 1;
    if !dry_run {
        let part = if format.scheme == PartitionScheme::SuperFloppy {
            target_node.to_owned()
        } else {
            wait_for_first_partition(target_node)?
        };
        format_partition(&part, format)?;
    }
    Ok(())
}

fn unmount_target(identity: &DeviceIdentity) -> Result<(), HelperError> {
    let mut mounted = mount_records()
        .unwrap_or_default()
        .into_iter()
        .filter(|mount| mount_belongs_to_identity(mount, identity))
        .collect::<Vec<_>>();
    mounted.sort_by_key(|mount| std::cmp::Reverse(mount.mount_point.as_os_str().len()));
    if !mounted.is_empty() {
        let umount = find_tool(tools::UMOUNT)?;
        for mount in mounted {
            let status = Command::new(umount)
                .arg(&mount.mount_point)
                .env_clear()
                .status()
                .map_err(|error| HelperError::Operation(format!("unmount: {error}")))?;
            if !status.success() {
                return Err(HelperError::Operation(format!(
                    "could not unmount {}",
                    mount.mount_point.display()
                )));
            }
        }
    }

    if let Ok(swaps) = std::fs::read_to_string("/proc/swaps") {
        for source in swaps
            .lines()
            .skip(1)
            .filter_map(|line| line.split_whitespace().next())
        {
            if node_is_device_or_partition(Path::new(source), &identity.node) {
                let swapoff = find_tool(tools::SWAPOFF)?;
                let status = Command::new(swapoff)
                    .arg(source)
                    .env_clear()
                    .status()
                    .map_err(|error| HelperError::Operation(format!("swapoff: {error}")))?;
                if !status.success() {
                    return Err(HelperError::Operation(format!(
                        "could not disable swap on {source}"
                    )));
                }
            }
        }
    }

    if mount_records()
        .unwrap_or_default()
        .iter()
        .any(|mount| mount_belongs_to_identity(mount, identity))
    {
        return Err(HelperError::Operation(
            "target still has mounted filesystems".into(),
        ));
    }
    Ok(())
}

fn node_is_device_or_partition(candidate: &Path, disk: &Path) -> bool {
    let candidate = candidate.to_string_lossy();
    let disk = disk.to_string_lossy();
    if candidate == disk {
        return true;
    }
    let suffix = candidate.strip_prefix(disk.as_ref()).unwrap_or_default();
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

fn wipe_leading(node: &Path) -> Result<(), HelperError> {
    let mut file = std::fs::OpenOptions::new().write(true).open(node)?;
    let zeros = vec![0u8; 1024 * 1024];
    for _ in 0..8 {
        file.write_all(&zeros)?;
    }
    file.sync_all()?;
    Ok(())
}

fn create_partition_table(node: &Path, scheme: PartitionScheme) -> Result<(), HelperError> {
    let label = match scheme {
        PartitionScheme::Mbr => "dos",
        PartitionScheme::Gpt => "gpt",
        PartitionScheme::SuperFloppy => {
            // No partition table — whole device is the filesystem.
            return Ok(());
        }
    };
    let parted = find_tool(tools::PARTED)?;
    let status = Command::new(parted)
        .args(["-s"])
        .arg(node)
        .args(["mklabel", label])
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin")
        .status()
        .map_err(|e| HelperError::Operation(format!("parted: {e}")))?;
    if !status.success() {
        return Err(HelperError::Operation("parted mklabel failed".into()));
    }
    if scheme != PartitionScheme::SuperFloppy {
        let status = Command::new(parted)
            .args(["-s"])
            .arg(node)
            .args(["mkpart", "primary", "0%", "100%"])
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin")
            .status()
            .map_err(|e| HelperError::Operation(format!("parted mkpart: {e}")))?;
        if !status.success() {
            return Err(HelperError::Operation("parted mkpart failed".into()));
        }
    }
    reread_partition_table(node)?;
    Ok(())
}

fn first_partition_node(disk: &Path) -> PathBuf {
    let name = disk.to_string_lossy();
    if name.chars().last().is_some_and(|c| c.is_ascii_digit()) {
        PathBuf::from(format!("{name}p1"))
    } else {
        PathBuf::from(format!("{name}1"))
    }
}

fn wait_for_first_partition(disk: &Path) -> Result<PathBuf, HelperError> {
    reread_partition_table(disk)?;
    if let Ok(udevadm) = find_tool(tools::UDEVADM) {
        let _ = Command::new(udevadm)
            .args(["settle", "--timeout=10"])
            .env_clear()
            .status();
    }
    let partition = first_partition_node(disk);
    for _ in 0..100 {
        if partition.exists() {
            return Ok(partition);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(HelperError::Operation(format!(
        "partition {} did not appear",
        partition.display()
    )))
}

fn reread_partition_table(node: &Path) -> Result<(), HelperError> {
    let blockdev = find_tool(tools::BLOCKDEV)?;
    let status = Command::new(blockdev)
        .arg("--rereadpt")
        .arg(node)
        .env_clear()
        .status()
        .map_err(|error| HelperError::Operation(format!("partition reread: {error}")))?;
    if !status.success() {
        return Err(HelperError::Operation(
            "kernel rejected the new partition table".into(),
        ));
    }
    Ok(())
}

fn format_partition(part: &Path, format: &FormatSpec) -> Result<(), HelperError> {
    let tool = tool_for_filesystem(format.filesystem)?;
    let mut cmd = Command::new(tool);
    cmd.env_clear().env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
    match format.filesystem {
        FileSystem::Fat => {
            cmd.args(["-F", "16", "-n", &format.label]);
            if let Some(cluster) = format.cluster_size {
                cmd.args(["-s", &(cluster / 512).max(1).to_string()]);
            }
        }
        FileSystem::Fat32 => {
            cmd.args(["-F", "32", "-n", &format.label]);
            if let Some(cluster) = format.cluster_size {
                cmd.args(["-s", &(cluster / 512).max(1).to_string()]);
            }
        }
        FileSystem::ExFat => {
            cmd.args(["-n", &format.label]);
            if let Some(cluster) = format.cluster_size {
                cmd.args(["-c", &cluster.to_string()]);
            }
        }
        FileSystem::Ntfs => {
            if format.quick_format {
                cmd.arg("-f");
            }
            cmd.args(["-L", &format.label]);
            if let Some(cluster) = format.cluster_size {
                cmd.args(["-c", &cluster.to_string()]);
            }
        }
        FileSystem::Ext2 => {
            cmd.args(["-t", "ext2", "-L", &format.label, "-F"]);
            add_ext_block_size(&mut cmd, format.cluster_size)?;
        }
        FileSystem::Ext3 => {
            cmd.args(["-t", "ext3", "-L", &format.label, "-F"]);
            add_ext_block_size(&mut cmd, format.cluster_size)?;
        }
        FileSystem::Ext4 => {
            cmd.args(["-t", "ext4", "-L", &format.label, "-F"]);
            add_ext_block_size(&mut cmd, format.cluster_size)?;
        }
        FileSystem::Udf => {
            cmd.args(["-l", &format.label]);
        }
        FileSystem::Refs => {
            return Err(HelperError::Operation("ReFS unavailable".into()));
        }
    }
    cmd.arg(part);
    let status = cmd
        .status()
        .map_err(|e| HelperError::Operation(format!("{tool}: {e}")))?;
    if !status.success() {
        return Err(HelperError::Operation(format!("{tool} failed")));
    }
    Ok(())
}

fn add_ext_block_size(cmd: &mut Command, size: Option<u32>) -> Result<(), HelperError> {
    if let Some(size) = size {
        if !matches!(size, 1024 | 2048 | 4096) {
            return Err(HelperError::Operation(
                "ext filesystems support 1 KiB, 2 KiB, or 4 KiB block sizes".into(),
            ));
        }
        cmd.args(["-b", &size.to_string()]);
    }
    Ok(())
}

#[derive(Debug)]
struct WriteReceipt {
    bytes_written: u64,
    sha256: [u8; 32],
}

fn write_image(
    target: &File,
    source: &rufus_helper_protocol::SourceSpec,
    mode: WriteMode,
    cancel: &CancellationToken,
    max_bytes: u64,
    job_id: JobId,
    sink: &mut EventSink,
) -> Result<WriteReceipt, HelperError> {
    if mode != WriteMode::DdImage {
        return Err(HelperError::Operation(
            "only disk-image writes are enabled".into(),
        ));
    }

    let mut decoder_child = None;
    let mut reader: Box<dyn Read> =
        if source.kind == rufus_core::plan::ImageSourceKind::CompressedRaw {
            let (tool, arguments) = decompressor_for(&source.path)?;
            let mut command = Command::new(tool);
            command
                .args(arguments)
                .arg(&source.path)
                .env_clear()
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            let mut child = command
                .spawn()
                .map_err(|error| HelperError::Operation(format!("decompressor: {error}")))?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| HelperError::Operation("decompressor stdout missing".into()))?;
            decoder_child = Some(child);
            Box::new(stdout)
        } else {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(&source.path)?;
            if !file.metadata()?.is_file() {
                return Err(HelperError::Operation(
                    "source changed and is no longer a regular file".into(),
                ));
            }
            Box::new(file)
        };

    let mut destination = target.try_clone()?;
    destination.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut written = 0u64;
    let started = Instant::now();
    let mut last_progress = Instant::now();
    loop {
        check_cancel(cancel)?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        written = written
            .checked_add(count as u64)
            .ok_or_else(|| HelperError::Operation("image size overflow".into()))?;
        if written > max_bytes {
            return Err(HelperError::Operation(
                "decompressed image exceeds target capacity".into(),
            ));
        }
        destination.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        if last_progress.elapsed() >= std::time::Duration::from_millis(200) {
            let elapsed = started.elapsed().as_secs_f64().max(0.001);
            emit(
                sink,
                HelperEvent::Progress {
                    job_id,
                    stage: ProgressStage::WritingImage,
                    unit: ProgressUnit::Bytes,
                    completed: written,
                    total: source.decompressed_size_bytes.or_else(|| {
                        (source.kind != rufus_core::plan::ImageSourceKind::CompressedRaw)
                            .then_some(source.size_bytes)
                    }),
                    bytes_per_second: Some((written as f64 / elapsed) as u64),
                    detail: Some(format!("Writing {}", source.path.display())),
                    cancellability: Cancellability::Immediate,
                },
            );
            last_progress = Instant::now();
        }
    }
    destination.sync_all()?;

    if written == 0 {
        return Err(HelperError::Operation("source image was empty".into()));
    }
    if source.kind != rufus_core::plan::ImageSourceKind::CompressedRaw
        && written != source.size_bytes
    {
        return Err(HelperError::Operation(
            "source image changed while it was being read".into(),
        ));
    }
    if let Some(expected) = source.decompressed_size_bytes {
        if written != expected {
            return Err(HelperError::Operation(format!(
                "decompressed size mismatch: expected {expected}, wrote {written}"
            )));
        }
    }
    if let Some(mut child) = decoder_child {
        let status = child
            .wait()
            .map_err(|error| HelperError::Operation(format!("decompressor wait: {error}")))?;
        if !status.success() {
            return Err(HelperError::Operation(
                "decompressor reported invalid or truncated input".into(),
            ));
        }
    }
    let digest: [u8; 32] = hasher.finalize().into();
    if let Some(expected) = &source.expected_sha256 {
        if hex_lower(&digest) != expected.to_ascii_lowercase() {
            return Err(HelperError::Operation(
                "source SHA-256 did not match the expected value".into(),
            ));
        }
    }
    Ok(WriteReceipt {
        bytes_written: written,
        sha256: digest,
    })
}

fn decompressor_for(path: &Path) -> Result<(&'static str, Vec<&'static str>), HelperError> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "gz" | "z" => Ok((find_tool(tools::GZIP)?, vec!["-dc", "--"])),
        "bz2" => Ok((find_tool(tools::BZIP2)?, vec!["-dc", "--"])),
        "xz" | "lzma" => Ok((find_tool(tools::XZ)?, vec!["-dc", "--"])),
        "zst" | "zstd" => Ok((find_tool(tools::ZSTD)?, vec!["-dc", "--"])),
        "zip" => Ok((find_tool(tools::BSDTAR)?, vec!["-xOf"])),
        _ => Err(HelperError::Operation(
            "compressed image extension is not supported".into(),
        )),
    }
}

fn verify_device_hash(
    device: &Path,
    size: u64,
    expected: &[u8; 32],
    cancel: &CancellationToken,
) -> Result<(), HelperError> {
    let mut input = File::open(device)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        check_cancel(cancel)?;
        let wanted = remaining.min(buffer.len() as u64) as usize;
        let count = input.read(&mut buffer[..wanted])?;
        if count == 0 {
            return Err(HelperError::Operation(
                "verification ended before all written bytes were read".into(),
            ));
        }
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    let actual: [u8; 32] = hasher.finalize().into();
    if actual != *expected {
        return Err(HelperError::Operation(
            "verification hash did not match the written image".into(),
        ));
    }
    Ok(())
}

fn flush_device(node: &Path) -> Result<(), HelperError> {
    let blockdev = find_tool(tools::BLOCKDEV)?;
    let status = Command::new(blockdev)
        .args(["--flushbufs"])
        .arg(node)
        .env_clear()
        .status()
        .map_err(|error| HelperError::Operation(format!("device flush: {error}")))?;
    if !status.success() {
        return Err(HelperError::Operation(
            "device cache flush was rejected".into(),
        ));
    }
    Ok(())
}

fn run_badblocks(node: &Path) -> Result<(), HelperError> {
    let badblocks = find_tool(tools::BADBLOCKS)?;
    let status = Command::new(badblocks)
        .args(["-w", "-s"])
        .arg(node)
        .env_clear()
        .status()
        .map_err(|error| HelperError::Operation(format!("badblocks: {error}")))?;
    if !status.success() {
        return Err(HelperError::Operation(
            "bad-block test found errors or was interrupted".into(),
        ));
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// Build a sample dry-run request for CLI smoke tests.
pub fn sample_dry_request(target_node: PathBuf) -> HelperRequest {
    use rufus_core::device::{DeviceFingerprint, DeviceNumber};
    use rufus_core::plan::BootMode;
    use rufus_helper_protocol::PROTOCOL_VERSION;

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(1);

    HelperRequest {
        protocol_version: PROTOCOL_VERSION,
        job_id: JobId::new(nanos),
        target: TargetIdentity {
            node: target_node.clone(),
            fingerprint: DeviceFingerprint {
                number: DeviceNumber::new(0, 0),
                canonical_sysfs_path: PathBuf::from("/sys/devices/virtual/dry-run"),
                size_bytes: 8 * 1024 * 1024 * 1024,
                logical_block_size: 512,
                serial: Some("DRYRUN".into()),
                wwn: None,
            },
            display_name: "Dry-run target".into(),
            model: "dry".into(),
            serial: "DRYRUN".into(),
        },
        operation: HelperOperation::FormatMedia {
            format: FormatSpec {
                scheme: PartitionScheme::Gpt,
                boot_mode: BootMode::NonBootable,
                filesystem: FileSystem::Fat32,
                label: "RUFUS".into(),
                cluster_size: None,
                persistence_bytes: 0,
                quick_format: true,
            },
            bad_blocks: false,
        },
        action_name: "Format device".into(),
    }
}

/// Encode events as NDJSON for the CLI.
pub fn write_event_line(out: &mut impl Write, event: &HelperEvent) -> Result<(), HelperError> {
    let bytes = encode_line(event)?;
    out.write_all(&bytes)?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rufus_core::device::{DeviceFingerprint, DeviceNumber};
    use rufus_core::plan::{BootMode, ImageSourceKind, PartitionScheme};
    use rufus_core::progress::JobId;
    use rufus_helper_protocol::{
        FormatSpec, HelperOperation, HelperRequest, SourceSpec, PROTOCOL_VERSION,
    };
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn format_request() -> HelperRequest {
        HelperRequest {
            protocol_version: PROTOCOL_VERSION,
            job_id: JobId::new(7),
            target: TargetIdentity {
                node: PathBuf::from("/dev/sdb"),
                fingerprint: DeviceFingerprint {
                    number: DeviceNumber::new(8, 16),
                    canonical_sysfs_path: PathBuf::from("/sys/devices/example"),
                    size_bytes: 16_000_000_000,
                    logical_block_size: 512,
                    serial: Some("TEST".into()),
                    wwn: None,
                },
                display_name: "Test".into(),
                model: "Test".into(),
                serial: "TEST".into(),
            },
            operation: HelperOperation::FormatMedia {
                format: FormatSpec {
                    scheme: PartitionScheme::Gpt,
                    boot_mode: BootMode::NonBootable,
                    filesystem: FileSystem::Fat32,
                    label: "TEST".into(),
                    cluster_size: None,
                    persistence_bytes: 0,
                    quick_format: true,
                },
                bad_blocks: false,
            },
            action_name: "Format device".into(),
        }
    }

    #[test]
    fn dry_run_format_emits_success() {
        let events: Arc<Mutex<Vec<HelperEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_c = events.clone();
        let result = execute(
            format_request(),
            ExecutionOptions {
                dry_run: true,
                cancel: CancellationToken::new(),
            },
            Box::new(move |ev| {
                events_c.lock().expect("lock").push(ev);
            }),
        )
        .expect("dry-run");
        assert_eq!(result, HelperResult::Success);
        let log = events.lock().expect("lock");
        assert!(log
            .iter()
            .any(|e| matches!(e, HelperEvent::Accepted { .. })));
        assert!(log.iter().any(|e| matches!(
            e,
            HelperEvent::Finished {
                result: HelperResult::Success,
                ..
            }
        )));
    }

    #[test]
    fn rejects_refs() {
        let mut req = format_request();
        if let HelperOperation::FormatMedia { format, .. } = &mut req.operation {
            format.filesystem = FileSystem::Refs;
        }
        let err = validate_request(&req).expect_err("refs");
        assert!(err.to_string().contains("ReFS"));
    }

    #[test]
    fn rejects_ffu_capture() {
        let mut req = format_request();
        req.operation = HelperOperation::CaptureImage {
            output: PathBuf::from("/tmp/out.ffu"),
            kind: ImageSourceKind::Ffu,
        };
        let err = validate_request(&req).expect_err("ffu");
        assert!(err.to_string().contains("FFU"));
    }

    #[test]
    fn version_is_nonempty() {
        assert_eq!(HELPER_VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn rejects_file_copy_mode_before_destructive_io() {
        let source_path =
            std::env::temp_dir().join(format!("rufus-helper-source-{}", std::process::id()));
        std::fs::write(&source_path, b"image").expect("write source fixture");
        let mut request = format_request();
        request.operation = HelperOperation::WriteMedia {
            write_mode: WriteMode::IsoFileCopy,
            source: SourceSpec {
                path: source_path.clone(),
                kind: ImageSourceKind::Iso,
                size_bytes: 5,
                decompressed_size_bytes: None,
                expected_sha256: None,
            },
            format: FormatSpec {
                scheme: PartitionScheme::Gpt,
                boot_mode: BootMode::Uefi,
                filesystem: FileSystem::Fat32,
                label: "TEST".into(),
                cluster_size: None,
                persistence_bytes: 0,
                quick_format: true,
            },
            verification: VerificationLevel::FullReadback,
            bad_blocks: false,
            install_bootloader: None,
        };
        let error = validate_request(&request).expect_err("file-copy mode must be rejected");
        let _ = std::fs::remove_file(source_path);
        assert!(error.to_string().contains("only raw/ISOHybrid"));
    }

    #[test]
    fn raw_writer_is_bounded_and_hashes_written_bytes() {
        let base = std::env::temp_dir();
        let unique = format!(
            "rufus-helper-write-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        );
        let source_path = base.join(format!("{unique}.img"));
        let target_path = base.join(format!("{unique}.target"));
        let payload = vec![0x5a; 1024 * 1024 + 17];
        std::fs::write(&source_path, &payload).expect("write image fixture");
        let target = File::create(&target_path).expect("create target fixture");
        target
            .set_len((payload.len() * 2) as u64)
            .expect("size target fixture");
        let source = SourceSpec {
            path: source_path.clone(),
            kind: ImageSourceKind::Raw,
            size_bytes: payload.len() as u64,
            decompressed_size_bytes: None,
            expected_sha256: None,
        };
        let mut events: EventSink = Box::new(|_| {});
        let receipt = write_image(
            &target,
            &source,
            WriteMode::DdImage,
            &CancellationToken::new(),
            payload.len() as u64 * 2,
            JobId::new(1),
            &mut events,
        )
        .expect("write raw fixture");
        assert_eq!(receipt.bytes_written, payload.len() as u64);
        let written = std::fs::read(&target_path).expect("read target fixture");
        assert_eq!(&written[..payload.len()], payload.as_slice());
        let _ = std::fs::remove_file(source_path);
        let _ = std::fs::remove_file(target_path);
    }
}
