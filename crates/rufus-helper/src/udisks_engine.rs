//! Plug-and-play execution: the desktop user drives the system udisks2
//! daemon, which performs the privileged steps under its own polkit policy.
//! Nothing extra is installed and no code of ours runs as root.
//!
//! Formatting, partitioning, and Windows media need no password on an active
//! local session; reading or writing raw sectors (disk images, bad-block
//! tests, BIOS boot code) asks once through the desktop's polkit agent.

use fs2::FileExt as _;
use std::os::unix::fs::FileExt;
use std::path::Path;

use rufus_core::plan::{BootMode, ImageSourceKind};
use rufus_helper_protocol::WindowsCustomization;
use rufus_image::isofs;

use super::*;
use crate::udisks::{ObjectPath, Udisks};
use crate::unattend;
use crate::windows_media::{self as winmedia, Span};

const MIB: u64 = 1024 * 1024;
const GPT_LINUX_DATA: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";

/// Version of the reachable udisks2 daemon, or why it cannot be used.
pub fn udisks_readiness() -> Result<String, String> {
    if unsafe { libc::geteuid() } == 0 {
        return Err("udisks2 mode runs as the desktop user".into());
    }
    Udisks::connect()
        .and_then(|udisks| udisks.version())
        .map_err(|error| error.to_string())
}

/// Validate a request for udisks2 execution without touching the target.
pub fn validate_udisks_request(request: &HelperRequest) -> Result<(), HelperError> {
    let udisks = Udisks::connect()?;
    validate_with(request, &udisks)
}

fn validate_with(request: &HelperRequest, udisks: &Udisks) -> Result<(), HelperError> {
    request.validate()?;
    match &request.operation {
        HelperOperation::WriteMedia {
            write_mode,
            source,
            format,
            install_bootloader,
            windows_customization,
            ..
        } => {
            if windows_customization.is_some() && *write_mode != WriteMode::IsoFileCopy {
                return Err(HelperError::Operation(
                    "Windows customization needs Windows installer media".into(),
                ));
            }
            if install_bootloader.is_some() || format.persistence_bytes != 0 {
                return Err(HelperError::Operation(
                    "bootloader and persistence operations are not enabled".into(),
                ));
            }
            match write_mode {
                WriteMode::DdImage => match source.kind {
                    ImageSourceKind::Raw | ImageSourceKind::IsoHybrid => {}
                    ImageSourceKind::CompressedRaw => {
                        decompressor_for(&source.path)?;
                    }
                    ImageSourceKind::Vhd | ImageSourceKind::Vhdx => virtual_disk::require_tools()?,
                    _ => {
                        return Err(HelperError::Operation(
                            "source kind cannot be safely raw-written".into(),
                        ))
                    }
                },
                WriteMode::IsoFileCopy => {
                    if source.kind != ImageSourceKind::Iso {
                        return Err(HelperError::Operation(
                            "file-copy media needs an ISO image".into(),
                        ));
                    }
                    let needs =
                        winmedia::requirements(format.filesystem, format.scheme, format.boot_mode)?;
                    require_format(udisks, format.filesystem)?;
                    if needs.uefi_ntfs {
                        require_format(udisks, FileSystem::Fat32)?;
                    }
                    mkfs_arguments(udisks, format)?;
                }
                _ => {
                    return Err(HelperError::Operation(
                        "this write mode is not available yet".into(),
                    ))
                }
            }
        }
        HelperOperation::FormatMedia { format, .. } => {
            require_format(udisks, format.filesystem)?;
            mkfs_arguments(udisks, format)?;
        }
        HelperOperation::CaptureImage { .. } => {
            return Err(HelperError::Operation(
                "image capture is not available yet".into(),
            ))
        }
    }
    Ok(())
}

fn udisks_kind(filesystem: FileSystem) -> Result<&'static str, HelperError> {
    Ok(match filesystem {
        FileSystem::Fat | FileSystem::Fat32 => "vfat",
        FileSystem::ExFat => "exfat",
        FileSystem::Ntfs => "ntfs",
        FileSystem::Udf => "udf",
        FileSystem::Ext2 => "ext2",
        FileSystem::Ext3 => "ext3",
        FileSystem::Ext4 => "ext4",
        FileSystem::Refs => {
            return Err(HelperError::Operation(
                "ReFS creation is not available on Linux".into(),
            ))
        }
    })
}

fn require_format(udisks: &Udisks, filesystem: FileSystem) -> Result<(), HelperError> {
    let kind = udisks_kind(filesystem)?;
    udisks.can_format(kind)?.map_err(|missing| {
        HelperError::MissingTool(format!(
            "{} formatting needs {missing}",
            filesystem.as_str()
        ))
    })
}

/// Formatter arguments udisks2 has no dedicated option for.
fn mkfs_arguments(udisks: &Udisks, format: &FormatSpec) -> Result<Vec<String>, HelperError> {
    let mut arguments = Vec::new();
    match format.filesystem {
        FileSystem::Fat => arguments.extend(["-F".to_owned(), "16".to_owned()]),
        FileSystem::Fat32 => arguments.extend(["-F".to_owned(), "32".to_owned()]),
        _ => {}
    }
    if let Some(cluster) = format.cluster_size {
        match format.filesystem {
            FileSystem::Fat | FileSystem::Fat32 => {
                arguments.extend(["-s".to_owned(), (cluster / 512).max(1).to_string()])
            }
            FileSystem::ExFat | FileSystem::Ntfs => {
                arguments.extend(["-c".to_owned(), cluster.to_string()])
            }
            FileSystem::Ext2 | FileSystem::Ext3 | FileSystem::Ext4 => {
                if !matches!(cluster, 1024 | 2048 | 4096) {
                    return Err(HelperError::Operation(
                        "ext filesystems support 1 KiB, 2 KiB, or 4 KiB block sizes".into(),
                    ));
                }
                arguments.extend(["-b".to_owned(), cluster.to_string()]);
            }
            FileSystem::Udf | FileSystem::Refs => {}
        }
    }
    if !arguments.is_empty() && !udisks.supports_mkfs_args()? {
        return Err(HelperError::Operation(
            "this choice needs udisks 2.10 or newer; choose FAT32 or the default cluster size"
                .into(),
        ));
    }
    Ok(arguments)
}

fn partition_type(filesystem: FileSystem, scheme: PartitionScheme) -> &'static str {
    let linux = matches!(
        filesystem,
        FileSystem::Ext2 | FileSystem::Ext3 | FileSystem::Ext4
    );
    match (scheme, filesystem) {
        (PartitionScheme::Gpt, _) if linux => GPT_LINUX_DATA,
        (PartitionScheme::Gpt, _) => winmedia::GPT_BASIC_DATA,
        (_, FileSystem::Fat) => "0x0e",
        (_, FileSystem::Fat32) => "0x0c",
        _ if linux => "0x83",
        _ => "0x07",
    }
}

fn table_kind(scheme: PartitionScheme) -> &'static str {
    match scheme {
        PartitionScheme::Mbr => "dos",
        _ => "gpt",
    }
}

/// Stage and byte progress for one job.
struct Reporter<'a> {
    job_id: JobId,
    sink: &'a mut EventSink,
    step: u64,
    total: u64,
}

impl Reporter<'_> {
    fn stage(&mut self, stage: ProgressStage, detail: &str) {
        let event = progress(
            self.job_id,
            stage,
            self.step,
            Some(self.total),
            Some(detail.to_owned()),
        );
        emit(self.sink, event);
        self.step = (self.step + 1).min(self.total);
    }

    fn log(&mut self, line: impl Into<String>) {
        let event = HelperEvent::Log {
            job_id: self.job_id,
            line: line.into(),
        };
        emit(self.sink, event);
    }

    fn bytes(&mut self, stage: ProgressStage, meter: &mut Meter, done: u64, detail: &str) {
        if !meter.due(done) {
            return;
        }
        let event = HelperEvent::Progress {
            job_id: self.job_id,
            stage,
            unit: ProgressUnit::Bytes,
            completed: done,
            total: Some(meter.total),
            bytes_per_second: Some(meter.rate(done)),
            detail: Some(detail.to_owned()),
            cancellability: Cancellability::Immediate,
        };
        emit(self.sink, event);
    }
}

struct Meter {
    total: u64,
    started: Instant,
    last: Option<Instant>,
}

impl Meter {
    fn new(total: u64) -> Self {
        Self {
            total,
            started: Instant::now(),
            last: None,
        }
    }

    fn due(&mut self, done: u64) -> bool {
        let now = Instant::now();
        if done < self.total
            && self
                .last
                .is_some_and(|last| now.duration_since(last) < Duration::from_millis(200))
        {
            return false;
        }
        self.last = Some(now);
        true
    }

    fn rate(&self, done: u64) -> u64 {
        (done as f64 / self.started.elapsed().as_secs_f64().max(0.001)) as u64
    }
}

/// Unmounts through udisks2 when dropped, so failures never leave the
/// target mounted.
struct Mounted<'a> {
    udisks: &'a Udisks,
    block: ObjectPath,
    path: PathBuf,
    active: bool,
}

impl<'a> Mounted<'a> {
    fn new(udisks: &'a Udisks, block: &ObjectPath, options: &str) -> Result<Self, HelperError> {
        let path = match udisks.mount_points(block)?.into_iter().next() {
            // A desktop automounter got there first; it mounted as this user.
            Some(existing) if options.is_empty() => existing,
            Some(_) => {
                udisks.unmount(block)?;
                udisks.mount(block, options)?
            }
            None => udisks.mount(block, options)?,
        };
        Ok(Self {
            udisks,
            block: block.clone(),
            path,
            active: true,
        })
    }

    fn unmount(mut self) -> Result<(), HelperError> {
        sync_filesystem(&self.path)?;
        self.active = false;
        self.udisks.unmount(&self.block)
    }
}

impl Drop for Mounted<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.udisks.unmount(&self.block);
        }
    }
}

fn sync_filesystem(path: &Path) -> Result<(), HelperError> {
    let directory = File::open(path)?;
    if unsafe { libc::syncfs(directory.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

/// Execute a request as the desktop user through udisks2.
pub fn execute_with_udisks(
    mut request: HelperRequest,
    options: ExecutionOptions,
    mut sink: EventSink,
) -> Result<HelperResult, HelperError> {
    let uid = unsafe { libc::geteuid() };
    if uid == 0 {
        return Err(HelperError::Operation(
            "udisks2 mode runs as the desktop user, never as root".into(),
        ));
    }
    let cancel = options.cancel.clone();
    let udisks = Udisks::connect()?;
    validate_with(&request, &udisks)?;
    let user = InvokingUser::from_uid(uid)?;
    let source_file = match &request.operation {
        HelperOperation::WriteMedia { source, .. } => Some(bind_source(source, &user)?),
        _ => None,
    };
    if let (HelperOperation::WriteMedia { source, .. }, Some(file)) =
        (&mut request.operation, source_file.as_ref())
    {
        if virtual_disk::is_virtual(source.kind) {
            let size = virtual_disk::inspect(
                file,
                source.kind,
                &user,
                &cancel,
                request.target.fingerprint.size_bytes,
            )?;
            if source
                .decompressed_size_bytes
                .is_some_and(|expected| expected != size)
            {
                return Err(HelperError::Operation(
                    "virtual disk size changed since selection".into(),
                ));
            }
            source.decompressed_size_bytes = Some(size);
        }
    }

    emit(
        &mut sink,
        HelperEvent::Accepted {
            job_id: request.job_id,
        },
    );
    let mut report = Reporter {
        job_id: request.job_id,
        sink: &mut sink,
        step: 0,
        total: 9,
    };
    report.log(format!("Using udisks2 {}", udisks.version()?));
    report.stage(ProgressStage::Preparing, "Checking the target device");
    let identity = revalidate_target(&request.target)?;
    if let (HelperOperation::WriteMedia { source, .. }, Some(file)) =
        (&request.operation, source_file.as_ref())
    {
        if source_is_on_target(file, &identity)? {
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
    let disk = udisks.block_for_device(identity.major, identity.minor)?;
    check_udisks_identity(&udisks, &disk, &request.target)?;
    check_cancel(&cancel)?;

    report.stage(ProgressStage::Unmounting, "Unmounting the target");
    unmount_all(&udisks, &disk)?;
    let identity = revalidate_target(&request.target)?;
    check_cancel(&cancel)?;

    let target = Target {
        udisks: &udisks,
        disk: &disk,
        identity: &identity,
        cancel: &cancel,
    };
    match &request.operation {
        HelperOperation::WriteMedia {
            write_mode: WriteMode::DdImage,
            source,
            verification,
            bad_blocks,
            ..
        } => {
            let file = source_file
                .as_ref()
                .ok_or_else(|| HelperError::Operation("bound source descriptor missing".into()))?;
            write_disk_image(
                &target,
                WriteSource {
                    file,
                    spec: source,
                    invoking_user: Some(&user),
                },
                *verification,
                *bad_blocks,
                &mut report,
            )?;
        }
        HelperOperation::WriteMedia {
            format,
            verification,
            bad_blocks,
            windows_customization,
            ..
        } => {
            let file = source_file
                .as_ref()
                .ok_or_else(|| HelperError::Operation("bound source descriptor missing".into()))?;
            write_windows_media(
                &target,
                file,
                WindowsMediaOptions {
                    format,
                    verification: *verification,
                    bad_blocks: *bad_blocks,
                    customization: windows_customization
                        .as_deref()
                        .filter(|options| !options.is_empty()),
                },
                &mut report,
            )?;
        }
        HelperOperation::FormatMedia { format, bad_blocks } => {
            format_media(&target, format, *bad_blocks, &mut report)?;
        }
        HelperOperation::CaptureImage { .. } => {
            return Err(HelperError::Operation(
                "image capture is not available yet".into(),
            ))
        }
    }
    let _ = udisks.rescan(&disk);

    report.step = report.total;
    report.stage(ProgressStage::Finalizing, "Complete");
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

struct Target<'a> {
    udisks: &'a Udisks,
    disk: &'a ObjectPath,
    identity: &'a DeviceIdentity,
    cancel: &'a CancellationToken,
}

impl Target<'_> {
    /// Exclusive read-write descriptor for the whole disk (asks once for
    /// authorization). Fails while any of its partitions is mounted.
    fn open_raw(&self) -> Result<File, HelperError> {
        let file = self
            .udisks
            .open(self.disk, "rw", libc::O_EXCL | libc::O_CLOEXEC)?;
        let opened = file.metadata()?;
        if !opened.file_type().is_block_device()
            || libc::major(opened.rdev()) != self.identity.major
            || libc::minor(opened.rdev()) != self.identity.minor
        {
            return Err(HelperError::Revalidation(
                "udisks2 opened a different device than the one selected".into(),
            ));
        }
        Ok(file)
    }
}

fn check_udisks_identity(
    udisks: &Udisks,
    disk: &ObjectPath,
    expected: &TargetIdentity,
) -> Result<(), HelperError> {
    if udisks.size(disk)? != expected.fingerprint.size_bytes {
        return Err(HelperError::Revalidation(
            "udisks2 reports a different device size".into(),
        ));
    }
    if udisks.read_only(disk)? {
        return Err(HelperError::Revalidation("device is read-only".into()));
    }
    let serial = udisks.drive_serial(disk)?;
    if !expected.serial.is_empty() && !serial.is_empty() && serial != expected.serial {
        return Err(HelperError::Revalidation(
            "udisks2 reports a different serial number".into(),
        ));
    }
    Ok(())
}

fn unmount_all(udisks: &Udisks, disk: &ObjectPath) -> Result<(), HelperError> {
    let mut blocks = udisks.partitions(disk)?;
    blocks.push(disk.clone());
    for block in blocks {
        if !udisks.mount_points(&block)?.is_empty() {
            udisks.unmount(&block)?;
        }
    }
    Ok(())
}

fn write_disk_image(
    target: &Target<'_>,
    source: WriteSource<'_>,
    verification: VerificationLevel,
    bad_blocks: bool,
    report: &mut Reporter<'_>,
) -> Result<(), HelperError> {
    report.stage(ProgressStage::Authorizing, "Opening the device");
    let device = target.open_raw()?;
    device.try_lock_exclusive().map_err(|error| {
        HelperError::Operation(format!("target is busy or could not be locked: {error}"))
    })?;
    if bad_blocks {
        test_bad_blocks(&device, target.identity.size_bytes, target.cancel, report)?;
    }
    report.stage(ProgressStage::WritingImage, "Writing the image");
    let receipt = write_image(
        &device,
        source,
        WriteMode::DdImage,
        target.cancel,
        target.identity.size_bytes,
        report.job_id,
        &mut *report.sink,
    )?;
    report.stage(ProgressStage::Syncing, "Flushing");
    device.sync_all()?;
    if verification == VerificationLevel::FullReadback {
        report.stage(ProgressStage::Verifying, "Reading the device back");
        verify_file_hash(
            &device,
            receipt.bytes_written,
            &receipt.sha256,
            target.cancel,
        )?;
    }
    Ok(())
}

/// `badblocks -w`: write each pattern over the device, then read it back.
fn test_bad_blocks(
    device: &File,
    size: u64,
    cancel: &CancellationToken,
    report: &mut Reporter<'_>,
) -> Result<(), HelperError> {
    const PATTERNS: [u8; 4] = [0xaa, 0x55, 0xff, 0x00];
    const CHUNK: u64 = 4 * MIB;
    report.stage(ProgressStage::TestingMedia, "Checking for bad blocks");
    let mut meter = Meter::new(size * 2 * PATTERNS.len() as u64);
    let mut done = 0u64;
    let mut written = vec![0u8; CHUNK as usize];
    let mut read = vec![0u8; CHUNK as usize];
    let mut bad = 0u64;
    for pattern in PATTERNS {
        written.fill(pattern);
        let label = format!("Bad-block test: writing 0x{pattern:02x}");
        let mut offset = 0;
        while offset < size {
            check_cancel(cancel)?;
            let length = CHUNK.min(size - offset) as usize;
            device.write_all_at(&written[..length], offset)?;
            offset += length as u64;
            done += length as u64;
            report.bytes(ProgressStage::TestingMedia, &mut meter, done, &label);
        }
        device.sync_all()?;
        drop_cached_pages(device)?;
        let label = format!("Bad-block test: reading 0x{pattern:02x}");
        let mut offset = 0;
        while offset < size {
            check_cancel(cancel)?;
            let length = CHUNK.min(size - offset) as usize;
            match device.read_exact_at(&mut read[..length], offset) {
                Ok(()) => {
                    bad += read[..length]
                        .chunks(512)
                        .filter(|sector| sector.iter().any(|byte| *byte != pattern))
                        .count() as u64
                }
                Err(_) => bad += length as u64 / 512,
            }
            offset += length as u64;
            done += length as u64;
            report.bytes(ProgressStage::TestingMedia, &mut meter, done, &label);
        }
    }
    if bad > 0 {
        return Err(HelperError::Operation(format!(
            "bad-block test found {bad} unreliable sectors; this device should not be used"
        )));
    }
    report.log("Bad-block test passed.");
    Ok(())
}

fn drop_cached_pages(file: &File) -> Result<(), HelperError> {
    let status = unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status).into());
    }
    Ok(())
}

/// Hash `size` bytes from the device itself rather than the page cache.
pub(crate) fn verify_file_hash(
    device: &File,
    size: u64,
    expected: &[u8; 32],
    cancel: &CancellationToken,
) -> Result<(), HelperError> {
    drop_cached_pages(device)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 4 * MIB as usize];
    let mut offset = 0u64;
    while offset < size {
        check_cancel(cancel)?;
        let wanted = (size - offset).min(buffer.len() as u64) as usize;
        device.read_exact_at(&mut buffer[..wanted], offset)?;
        hasher.update(&buffer[..wanted]);
        offset += wanted as u64;
    }
    let actual: [u8; 32] = hasher.finalize().into();
    if actual != *expected {
        return Err(HelperError::Operation(
            "verification hash did not match the written image".into(),
        ));
    }
    Ok(())
}

fn format_volume(
    udisks: &Udisks,
    volume: &ObjectPath,
    format: &FormatSpec,
) -> Result<(), HelperError> {
    let arguments = mkfs_arguments(udisks, format)?;
    let label = (!format.label.is_empty()).then_some(format.label.as_str());
    let owned = matches!(
        format.filesystem,
        FileSystem::Ext2 | FileSystem::Ext3 | FileSystem::Ext4
    );
    udisks.format(
        volume,
        udisks_kind(format.filesystem)?,
        label,
        &arguments,
        !format.quick_format,
        owned,
    )
}

fn format_media(
    target: &Target<'_>,
    format: &FormatSpec,
    bad_blocks: bool,
    report: &mut Reporter<'_>,
) -> Result<(), HelperError> {
    if bad_blocks {
        report.stage(ProgressStage::Authorizing, "Opening the device");
        let device = target.open_raw()?;
        test_bad_blocks(&device, target.identity.size_bytes, target.cancel, report)?;
    }
    check_cancel(target.cancel)?;
    let volume = if format.scheme == PartitionScheme::SuperFloppy {
        target.disk.clone()
    } else {
        report.stage(ProgressStage::Partitioning, table_kind(format.scheme));
        target.udisks.format(
            target.disk,
            table_kind(format.scheme),
            None,
            &[],
            false,
            false,
        )?;
        target.udisks.create_partition(
            target.disk,
            MIB,
            0,
            partition_type(format.filesystem, format.scheme),
            "",
        )?
    };
    check_cancel(target.cancel)?;
    report.stage(ProgressStage::Formatting, format.filesystem.as_str());
    format_volume(target.udisks, &volume, format)
}

struct WindowsMediaOptions<'a> {
    format: &'a FormatSpec,
    verification: VerificationLevel,
    bad_blocks: bool,
    customization: Option<&'a WindowsCustomization>,
}

/// Read the image's Windows details and refuse customization it cannot take.
fn customization_plan(
    iso: &mut File,
    listing: &isofs::IsoListing,
    options: &WindowsCustomization,
) -> Result<(unattend::AnswerFile, u32), HelperError> {
    let windows = rufus_image::windows::inspect(iso, listing)
        .filter(|windows| windows.is_windows_10_or_later())
        .ok_or_else(|| {
            HelperError::Operation(
                "Windows User Experience options need Windows 10 or later media".into(),
            )
        })?;
    if windows.has_panther_unattend || listing.has_file(unattend::ROOT_ANSWER_FILE) {
        return Err(HelperError::Operation(
            "this image already has its own answer file".into(),
        ));
    }
    if let Some(index) = options.silent_install_index {
        if !windows
            .editions
            .iter()
            .any(|edition| edition.index == index)
        {
            return Err(HelperError::Operation(format!(
                "this image has no Windows edition {index}"
            )));
        }
    }
    let arch = windows.arch.ok_or_else(|| {
        HelperError::Operation("the Windows architecture of this image is unknown".into())
    })?;
    let language = windows.setup_language.as_deref().unwrap_or("en-US");
    Ok((
        unattend::answer_file(options, arch, language),
        windows.build,
    ))
}

fn write_windows_media(
    target: &Target<'_>,
    source: &File,
    options: WindowsMediaOptions<'_>,
    report: &mut Reporter<'_>,
) -> Result<(), HelperError> {
    let WindowsMediaOptions {
        format,
        verification,
        bad_blocks,
        customization,
    } = options;
    let udisks = target.udisks;
    let cancel = target.cancel;
    // Everything that can be checked is checked before the first write.
    let needs = winmedia::requirements(format.filesystem, format.scheme, format.boot_mode)?;
    let mut iso = source.try_clone()?;
    let listing = isofs::list(&mut iso)?
        .ok_or_else(|| HelperError::Operation("the ISO file system could not be read".into()))?;
    let has = |path: &str| listing.has_file(path);
    if !(has("sources/install.wim") || has("sources/install.esd") || has("sources/install.swm")) {
        return Err(HelperError::Operation(
            "this ISO is not Windows installation media".into(),
        ));
    }
    if needs.bios_boot && !has("bootmgr") {
        return Err(HelperError::Operation(
            "this image has no BIOS boot manager; choose UEFI".into(),
        ));
    }
    if format.boot_mode != BootMode::Bios
        && ![
            "efi/boot/bootx64.efi",
            "efi/boot/bootia32.efi",
            "efi/boot/bootaa64.efi",
        ]
        .iter()
        .any(|path| has(path))
    {
        return Err(HelperError::Operation(
            "this image has no UEFI boot loader; choose BIOS".into(),
        ));
    }
    let layout = winmedia::plan_layout(
        target.identity.size_bytes,
        u64::from(target.identity.logical_sector_size),
        format.scheme,
        needs.uefi_ntfs,
    )?;
    winmedia::check_fits(&listing, format.filesystem, layout.data.size)?;
    let customization = match customization {
        Some(options) => Some((customization_plan(&mut iso, &listing, options)?, options)),
        None => None,
    };
    report.log(format!(
        "Windows media: {} files, {} MiB, {}{}",
        listing.entries.iter().filter(|entry| !entry.is_dir).count(),
        listing.total_file_bytes() / MIB,
        format.filesystem.as_str(),
        if needs.uefi_ntfs { " + UEFI:NTFS" } else { "" }
    ));

    if bad_blocks {
        report.stage(ProgressStage::Authorizing, "Opening the device");
        let device = target.open_raw()?;
        test_bad_blocks(&device, target.identity.size_bytes, cancel, report)?;
    }
    check_cancel(cancel)?;

    report.stage(ProgressStage::Partitioning, table_kind(format.scheme));
    udisks.format(
        target.disk,
        table_kind(format.scheme),
        None,
        &[],
        false,
        false,
    )?;
    let gpt = format.scheme == PartitionScheme::Gpt;
    let data = udisks.create_partition(
        target.disk,
        layout.data.offset,
        layout.data.size,
        partition_type(format.filesystem, format.scheme),
        if gpt {
            winmedia::MAIN_PARTITION_NAME
        } else {
            ""
        },
    )?;
    let data_geometry = udisks.partition_geometry(&data)?;
    let uefi = match layout.uefi_ntfs {
        Some(span) => Some(create_uefi_ntfs(udisks, target.disk, span, gpt)?),
        None => None,
    };
    if let Some((_, uefi_span)) = &uefi {
        let data_span = Span {
            offset: data_geometry.offset,
            size: data_geometry.size,
        };
        if data_span.end() > uefi_span.offset {
            return Err(HelperError::Operation(
                "the partitions were not created where planned".into(),
            ));
        }
    }
    if needs.bios_boot {
        udisks.set_partition_flags(&data, winmedia::MBR_ACTIVE)?;
    }
    check_cancel(cancel)?;

    report.stage(ProgressStage::Formatting, format.filesystem.as_str());
    format_volume(udisks, &data, format)?;
    if let Some((uefi_block, _)) = &uefi {
        udisks.format(
            uefi_block,
            "vfat",
            Some(winmedia::UEFI_NTFS_LABEL),
            &[],
            false,
            false,
        )?;
        let mounted = Mounted::new(udisks, uefi_block, "")?;
        winmedia::write_uefi_ntfs_files(&mounted.path)?;
        mounted.unmount()?;
        if gpt {
            udisks.set_partition_flags(uefi_block, winmedia::GPT_NO_DRIVE_LETTER)?;
        }
        report.log("Installed the UEFI:NTFS boot partition.");
    }
    check_cancel(cancel)?;

    report.stage(ProgressStage::ExtractingFiles, "Copying Windows files");
    let mounted = Mounted::new(udisks, &data, "")?;
    let total = listing.total_file_bytes();
    let mut meter = Meter::new(total);
    let mut copies = {
        let mut on_progress = |done: u64, path: &str| {
            report.bytes(
                ProgressStage::ExtractingFiles,
                &mut meter,
                done,
                &format!("Copying {path}"),
            )
        };
        winmedia::copy_listing(&mut iso, &listing, &mounted.path, cancel, &mut on_progress)?
    };
    if let Some(((answer, build), options)) = &customization {
        report.stage(
            ProgressStage::ApplyingCustomization,
            "Applying Windows customization",
        );
        for line in &answer.log {
            report.log(line.clone());
        }
        winmedia::apply_customization(
            &mounted.path,
            answer,
            options.bypass_requirements,
            *build,
            &mut copies,
            &mut |line| report.log(line),
        )?;
    }
    let written_root = mounted.path.clone();
    report.stage(ProgressStage::Syncing, "Flushing the copied files");
    mounted.unmount()?;
    check_cancel(cancel)?;

    if needs.bios_boot {
        report.stage(
            ProgressStage::InstallingBootloader,
            "Writing BIOS boot code",
        );
        install_bios_boot_code(target, data_geometry.offset, format.filesystem)?;
        report.log("Installed the Windows 7 MBR and the partition boot record.");
    }

    if verification != VerificationLevel::None {
        report.stage(ProgressStage::Verifying, "Reading the copied files back");
        let mounted = Mounted::new(udisks, &data, "ro")?;
        let mut meter = Meter::new(winmedia::copied_bytes(&copies));
        let mut on_progress = |done: u64| {
            report.bytes(
                ProgressStage::Verifying,
                &mut meter,
                done,
                "Verifying copied files",
            )
        };
        winmedia::verify_copies(
            &copies,
            &written_root,
            &mounted.path,
            cancel,
            &mut on_progress,
        )?;
        mounted.unmount()?;
    }
    Ok(())
}

fn create_uefi_ntfs(
    udisks: &Udisks,
    disk: &ObjectPath,
    span: Span,
    gpt: bool,
) -> Result<(ObjectPath, Span), HelperError> {
    let (kind, name) = if gpt {
        (winmedia::GPT_BASIC_DATA, winmedia::UEFI_NTFS_NAME)
    } else {
        ("0xef", "")
    };
    let block = udisks.create_partition(disk, span.offset, span.size, kind, name)?;
    let geometry = udisks.partition_geometry(&block)?;
    if geometry.size < winmedia::UEFI_NTFS_SIZE - 64 * 1024 {
        return Err(HelperError::Operation(
            "the UEFI:NTFS partition came out too small".into(),
        ));
    }
    Ok((
        block,
        Span {
            offset: geometry.offset,
            size: geometry.size,
        },
    ))
}

fn install_bios_boot_code(
    target: &Target<'_>,
    data_offset: u64,
    filesystem: FileSystem,
) -> Result<(), HelperError> {
    unmount_all(target.udisks, target.disk)?;
    let device = target.open_raw()?;
    let mut sector = [0u8; 512];
    device.read_exact_at(&mut sector, 0)?;
    winmedia::patch_mbr(&mut sector)?;
    device.write_all_at(&sector, 0)?;
    let sector_size = u64::from(target.identity.logical_sector_size.max(512));
    let start = u32::try_from(data_offset / sector_size)
        .map_err(|_| HelperError::Operation("partition start is beyond MBR reach".into()))?;
    let mut area = vec![0u8; winmedia::BOOT_AREA_BYTES];
    device.read_exact_at(&mut area, data_offset)?;
    winmedia::patch_boot_record(&mut area, filesystem, start)?;
    device.write_all_at(&area, data_offset)?;
    device.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_types_follow_upstream() {
        assert_eq!(
            partition_type(FileSystem::Ntfs, PartitionScheme::Gpt),
            winmedia::GPT_BASIC_DATA
        );
        assert_eq!(
            partition_type(FileSystem::Ext4, PartitionScheme::Gpt),
            GPT_LINUX_DATA
        );
        assert_eq!(
            partition_type(FileSystem::Fat32, PartitionScheme::Mbr),
            "0x0c"
        );
        assert_eq!(
            partition_type(FileSystem::Fat, PartitionScheme::Mbr),
            "0x0e"
        );
        assert_eq!(
            partition_type(FileSystem::Ntfs, PartitionScheme::Mbr),
            "0x07"
        );
        assert_eq!(
            partition_type(FileSystem::ExFat, PartitionScheme::Mbr),
            "0x07"
        );
        assert_eq!(
            partition_type(FileSystem::Ext4, PartitionScheme::Mbr),
            "0x83"
        );
    }

    #[test]
    fn verification_reads_the_written_bytes() {
        let mut file = tempfile_with(b"rufus verification payload");
        let digest: [u8; 32] = Sha256::digest(b"rufus verification payload").into();
        let cancel = CancellationToken::new();
        verify_file_hash(&file, 26, &digest, &cancel).expect("matching hash");
        file.write_all_at(b"R", 0).expect("corrupt");
        assert!(verify_file_hash(&file, 26, &digest, &cancel).is_err());
        file.flush().expect("flush");
    }

    fn tempfile_with(bytes: &[u8]) -> File {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_TMPFILE)
            .mode(0o600)
            .open(std::env::temp_dir())
            .expect("anonymous file");
        file.write_all(bytes).expect("write payload");
        file
    }

    struct LoopDisk<'a> {
        udisks: &'a Udisks,
        block: ObjectPath,
    }

    impl Drop for LoopDisk<'_> {
        fn drop(&mut self) {
            let _ = unmount_all(self.udisks, &self.block);
            let _ = self.udisks.loop_delete(&self.block);
        }
    }

    fn windows_fixture(dir: &Path) -> PathBuf {
        use rufus_image::isofs::fixture::{self, Node};
        let path = dir.join("windows.iso");
        std::fs::write(
            &path,
            fixture::udf(vec![
                Node::File("bootmgr", 4096),
                Node::Dir(
                    "efi",
                    vec![Node::Dir("boot", vec![Node::File("bootx64.efi", 9000)])],
                ),
                Node::Dir(
                    "sources",
                    vec![
                        Node::File("boot.wim", 70_000),
                        Node::File("install.wim", 3_000_000),
                    ],
                ),
            ]),
        )
        .expect("write ISO fixture");
        path
    }

    /// A Windows 11 24H2 layout with a real WIM index, for customization.
    fn windows_11_fixture(dir: &Path) -> PathBuf {
        use rufus_image::isofs::fixture::{self, Node};
        let install = rufus_image::wim::fixture(
            r#"<WIM><IMAGE INDEX="1"><NAME>Windows 11 Pro</NAME><DISPLAYNAME>Windows 11 Pro</DISPLAYNAME>
<WINDOWS><ARCH>9</ARCH><VERSION><MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>26100</BUILD></VERSION></WINDOWS></IMAGE></WIM>"#,
        );
        let mut setup = vec![0u8; 0x200];
        setup[..2].copy_from_slice(b"MZ");
        setup[0x3c] = 0x80;
        setup[0x80..0x84].copy_from_slice(b"PE\0\0");
        setup[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        let path = dir.join("windows11.iso");
        std::fs::write(
            &path,
            fixture::udf(vec![
                Node::File("bootmgr", 4096),
                Node::File("bootmgr.efi", 4096),
                Node::Bytes("setup.exe", setup),
                Node::Dir(
                    "efi",
                    vec![Node::Dir("boot", vec![Node::File("bootx64.efi", 9000)])],
                ),
                Node::Dir(
                    "sources",
                    vec![
                        Node::File("appraiserres.dll", 3000),
                        Node::Bytes("install.wim", install),
                    ],
                ),
            ]),
        )
        .expect("write ISO fixture");
        path
    }

    /// The Windows User Experience options through the real daemon: the
    /// answer file and bypass files land on the media and pass verification.
    #[test]
    #[ignore = "needs udisks2 and an active desktop session; attaches loop devices"]
    fn loop_device_windows_media_carries_the_answer_file() {
        let udisks = Udisks::connect().expect("udisks2");
        let dir = std::env::temp_dir().join(format!("rufus-udisks-wue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("fixture directory");
        let iso_path = windows_11_fixture(&dir);
        let disk_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(dir.join("disk.img"))
            .expect("backing file");
        disk_file.set_len(320 * MIB).expect("size backing file");
        let block = udisks.loop_setup(&disk_file).expect("loop setup");
        let disk = LoopDisk {
            udisks: &udisks,
            block,
        };
        let number = udisks.device_number(&disk.block).expect("device number");
        let identity = DeviceIdentity {
            node: PathBuf::from("/dev/loop"),
            sysfs_path: PathBuf::from("/sys/devices/virtual/block/loop"),
            kernel_name: "loop".into(),
            major: libc::major(number),
            minor: libc::minor(number),
            size_bytes: udisks.size(&disk.block).expect("size"),
            logical_sector_size: 512,
            model: String::new(),
            vendor: String::new(),
            serial: String::new(),
            transport: String::new(),
            removable: true,
            read_only: false,
        };
        let cancel = CancellationToken::new();
        let target = Target {
            udisks: &udisks,
            disk: &disk.block,
            identity: &identity,
            cancel: &cancel,
        };
        let format = FormatSpec {
            scheme: PartitionScheme::Gpt,
            boot_mode: BootMode::Uefi,
            filesystem: FileSystem::Ntfs,
            label: "CCCOMA_X64FRE_EN-US_DV9".into(),
            cluster_size: None,
            persistence_bytes: 0,
            quick_format: true,
        };
        let customization = WindowsCustomization {
            bypass_requirements: true,
            no_online_account: true,
            local_account: Some("rufus".into()),
            no_data_collection: true,
            ..WindowsCustomization::default()
        };
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected = std::sync::Arc::clone(&events);
        let mut sink: EventSink = Box::new(move |event| {
            collected.lock().expect("events").push(event);
        });
        let mut report = Reporter {
            job_id: JobId::new(1),
            sink: &mut sink,
            step: 0,
            total: 10,
        };
        let source = File::open(&iso_path).expect("open ISO fixture");
        write_windows_media(
            &target,
            &source,
            WindowsMediaOptions {
                format: &format,
                verification: VerificationLevel::FullReadback,
                bad_blocks: false,
                customization: Some(&customization),
            },
            &mut report,
        )
        .expect("customized Windows media");
        let logged: Vec<String> = events
            .lock()
            .expect("events")
            .iter()
            .filter_map(|event| match event {
                HelperEvent::Log { line, .. } => Some(line.clone()),
                _ => None,
            })
            .collect();
        assert!(
            logged.iter().any(|line| line == "• Bypass SB/TPM/RAM"),
            "{logged:?}"
        );

        let data = udisks
            .partitions(&disk.block)
            .expect("partitions")
            .into_iter()
            .find(|part| {
                udisks
                    .partition_geometry(part)
                    .is_ok_and(|geometry| geometry.number == 1)
            })
            .expect("data partition");
        let mounted = Mounted::new(&udisks, &data, "ro").expect("mount data");
        let answer =
            std::fs::read_to_string(mounted.path.join("autounattend.xml")).expect("answer file");
        assert!(answer.contains("BypassTPMCheck"));
        assert!(answer.contains("<Name>rufus</Name>"));
        assert_eq!(
            std::fs::metadata(mounted.path.join("sources/appraiserres.dll"))
                .expect("placeholder")
                .len(),
            0
        );
        assert!(mounted.path.join("sources/appraiserres.bak").is_file());
        assert!(mounted.path.join("setup.dll").is_file());
        assert_eq!(
            std::fs::read(mounted.path.join("setup.exe"))
                .expect("wrapper")
                .len(),
            include_bytes!("../assets/setup/setup_x64.exe").len()
        );
        mounted.unmount().expect("unmount data");
        drop(disk);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real Windows ISO, customized, on a loop device whose backing file
    /// is kept for a firmware boot test:
    /// `RUFUS_WINDOWS_ISO=… RUFUS_TEST_DISK=/path/disk.img [RUFUS_TEST_SILENT=1]`.
    #[test]
    #[ignore = "needs udisks2, a Windows ISO, and about 9 GiB of disk space"]
    fn real_windows_iso_becomes_customized_media() {
        let iso_path = std::env::var_os("RUFUS_WINDOWS_ISO").expect("RUFUS_WINDOWS_ISO");
        let disk_path = std::env::var_os("RUFUS_TEST_DISK").expect("RUFUS_TEST_DISK");
        let silent = std::env::var_os("RUFUS_TEST_SILENT").is_some();
        let udisks = Udisks::connect().expect("udisks2");
        let disk_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&disk_path)
            .expect("backing file");
        disk_file
            .set_len(9 * 1024 * MIB)
            .expect("size backing file");
        let block = udisks.loop_setup(&disk_file).expect("loop setup");
        let disk = LoopDisk {
            udisks: &udisks,
            block,
        };
        let number = udisks.device_number(&disk.block).expect("device number");
        let identity = DeviceIdentity {
            node: PathBuf::from("/dev/loop"),
            sysfs_path: PathBuf::from("/sys/devices/virtual/block/loop"),
            kernel_name: "loop".into(),
            major: libc::major(number),
            minor: libc::minor(number),
            size_bytes: udisks.size(&disk.block).expect("size"),
            logical_sector_size: 512,
            model: String::new(),
            vendor: String::new(),
            serial: String::new(),
            transport: String::new(),
            removable: true,
            read_only: false,
        };
        let cancel = CancellationToken::new();
        let target = Target {
            udisks: &udisks,
            disk: &disk.block,
            identity: &identity,
            cancel: &cancel,
        };
        let format = FormatSpec {
            scheme: PartitionScheme::Gpt,
            boot_mode: BootMode::Uefi,
            filesystem: FileSystem::Ntfs,
            label: "WIN11 (SILENT)".into(),
            cluster_size: None,
            persistence_bytes: 0,
            quick_format: true,
        };
        let mut iso = File::open(&iso_path).expect("open ISO");
        let listing = isofs::list(&mut iso).expect("list").expect("listing");
        let windows = rufus_image::windows::inspect(&mut iso, &listing).expect("Windows ISO");
        let customization = WindowsCustomization {
            bypass_requirements: true,
            no_online_account: true,
            local_account: Some("rufus".into()),
            no_data_collection: true,
            regional: silent.then(|| rufus_helper_protocol::RegionalSettings {
                input_locale: "0409:00000409".into(),
                system_locale: "en-US".into(),
                user_locale: "en-US".into(),
                ui_language: "en-US".into(),
                time_zone: Some("UTC".into()),
            }),
            silent_install_index: silent.then(|| windows.editions[0].index),
            ..WindowsCustomization::default()
        };
        let mut sink: EventSink = Box::new(|event| {
            if let HelperEvent::Log { line, .. } = event {
                eprintln!("{line}");
            }
        });
        let mut report = Reporter {
            job_id: JobId::new(1),
            sink: &mut sink,
            step: 0,
            total: 10,
        };
        write_windows_media(
            &target,
            &iso,
            WindowsMediaOptions {
                format: &format,
                verification: VerificationLevel::None,
                bad_blocks: false,
                customization: Some(&customization),
            },
            &mut report,
        )
        .expect("customized Windows media");
    }

    /// End to end through the real daemon on a loop device the test owns:
    /// partitioning, mkfs, UEFI:NTFS, file copy, and read-back verification.
    /// BIOS boot code is then applied to the backing file, which needs no
    /// authorization, and the volume must still mount and verify.
    #[test]
    #[ignore = "needs udisks2 and an active desktop session; attaches loop devices"]
    fn loop_devices_become_windows_media() {
        let udisks = Udisks::connect().expect("udisks2");
        let dir = std::env::temp_dir().join(format!("rufus-udisks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("fixture directory");
        let iso_path = windows_fixture(&dir);
        for (scheme, filesystem, _target_system) in [
            (PartitionScheme::Gpt, FileSystem::Ntfs, BootMode::Uefi),
            (PartitionScheme::Mbr, FileSystem::Ntfs, BootMode::Dual),
            (PartitionScheme::Gpt, FileSystem::ExFat, BootMode::Uefi),
            (PartitionScheme::Mbr, FileSystem::Fat32, BootMode::Dual),
        ] {
            let disk_path = dir.join("disk.img");
            let disk_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&disk_path)
                .expect("backing file");
            disk_file.set_len(320 * MIB).expect("size backing file");
            let block = udisks.loop_setup(&disk_file).expect("loop setup");
            let disk = LoopDisk {
                udisks: &udisks,
                block,
            };
            let number = udisks.device_number(&disk.block).expect("device number");
            let identity = DeviceIdentity {
                node: PathBuf::from("/dev/loop"),
                sysfs_path: PathBuf::from("/sys/devices/virtual/block/loop"),
                kernel_name: "loop".into(),
                major: libc::major(number),
                minor: libc::minor(number),
                size_bytes: udisks.size(&disk.block).expect("size"),
                logical_sector_size: 512,
                model: String::new(),
                vendor: String::new(),
                serial: String::new(),
                transport: String::new(),
                removable: true,
                read_only: false,
            };
            // The lookups execute_with_udisks performs before writing.
            assert_eq!(
                udisks
                    .block_for_device(identity.major, identity.minor)
                    .expect("resolve by device number"),
                disk.block
            );
            let mut expected = TargetIdentity {
                node: identity.node.clone(),
                fingerprint: rufus_core::device::DeviceFingerprint {
                    number: rufus_core::device::DeviceNumber::new(identity.major, identity.minor),
                    canonical_sysfs_path: identity.sysfs_path.clone(),
                    size_bytes: identity.size_bytes,
                    logical_block_size: 512,
                    serial: None,
                    wwn: None,
                },
                display_name: "loop".into(),
                model: String::new(),
                serial: String::new(),
            };
            check_udisks_identity(&udisks, &disk.block, &expected).expect("identity");
            expected.fingerprint.size_bytes += 512;
            assert!(check_udisks_identity(&udisks, &disk.block, &expected).is_err());
            let cancel = CancellationToken::new();
            let target = Target {
                udisks: &udisks,
                disk: &disk.block,
                identity: &identity,
                cancel: &cancel,
            };
            let format = FormatSpec {
                scheme,
                // Raw boot code is applied below without authorization.
                boot_mode: BootMode::Uefi,
                filesystem,
                label: filesystem.volume_label("CCCOMA_X64FRE_EN-US_DV9"),
                cluster_size: None,
                persistence_bytes: 0,
                quick_format: true,
            };
            let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let collected = std::sync::Arc::clone(&events);
            let mut sink: EventSink = Box::new(move |event| {
                collected.lock().expect("events").push(event);
            });
            let mut report = Reporter {
                job_id: JobId::new(1),
                sink: &mut sink,
                step: 0,
                total: 9,
            };
            let source = File::open(&iso_path).expect("open ISO fixture");
            write_windows_media(
                &target,
                &source,
                WindowsMediaOptions {
                    format: &format,
                    verification: VerificationLevel::FullReadback,
                    bad_blocks: false,
                    customization: None,
                },
                &mut report,
            )
            .unwrap_or_else(|error| panic!("{scheme:?} {filesystem:?}: {error}"));
            assert!(events.lock().expect("events").iter().any(|event| matches!(
                event,
                HelperEvent::Progress {
                    stage: ProgressStage::ExtractingFiles,
                    unit: ProgressUnit::Bytes,
                    ..
                }
            )));

            let partitions = udisks.partitions(&disk.block).expect("partitions");
            let mut geometry = partitions
                .iter()
                .map(|part| (udisks.partition_geometry(part).expect("geometry"), part))
                .collect::<Vec<_>>();
            geometry.sort_by_key(|(geometry, _)| geometry.number);
            let needs =
                winmedia::requirements(filesystem, scheme, BootMode::Uefi).expect("requirements");
            assert_eq!(geometry.len(), if needs.uefi_ntfs { 2 } else { 1 });
            assert_eq!(geometry[0].0.offset, MIB);
            if needs.uefi_ntfs {
                let (uefi, part) = &geometry[1];
                assert_eq!(uefi.size, MIB);
                assert_eq!(uefi.offset, geometry[0].0.offset + geometry[0].0.size);
                if scheme == PartitionScheme::Gpt {
                    let name: String = udisks.partition_property(part, "Name").expect("name");
                    let flags: u64 = udisks.partition_property(part, "Flags").expect("flags");
                    let kind: String = udisks.partition_property(part, "Type").expect("type");
                    assert_eq!(name, "UEFI:NTFS");
                    assert_eq!(
                        flags & winmedia::GPT_NO_DRIVE_LETTER,
                        winmedia::GPT_NO_DRIVE_LETTER
                    );
                    assert_eq!(kind, winmedia::GPT_BASIC_DATA);
                } else {
                    let kind: String = udisks.partition_property(part, "Type").expect("type");
                    assert_eq!(kind, "0xef");
                }
                let mounted = Mounted::new(&udisks, part, "ro").expect("mount UEFI:NTFS");
                assert_eq!(
                    std::fs::read(mounted.path.join("EFI/Boot/bootx64.efi")).expect("bootx64"),
                    winmedia::UEFI_NTFS_FILES
                        .iter()
                        .find(|(path, _)| *path == "EFI/Boot/bootx64.efi")
                        .expect("payload")
                        .1
                );
                mounted.unmount().expect("unmount UEFI:NTFS");
            }

            if scheme == PartitionScheme::Mbr {
                // What install_bios_boot_code writes, applied to the backing file.
                udisks
                    .set_partition_flags(geometry[0].1, winmedia::MBR_ACTIVE)
                    .expect("mark active");
                unmount_all(&udisks, &disk.block).expect("unmount");
                let data_offset = geometry[0].0.offset;
                let mut sector = [0u8; 512];
                disk_file.read_exact_at(&mut sector, 0).expect("read MBR");
                let table = sector[0x1be..0x1fe].to_vec();
                winmedia::patch_mbr(&mut sector).expect("patch MBR");
                assert_eq!(&sector[0x1be..0x1fe], &table[..]);
                disk_file.write_all_at(&sector, 0).expect("write MBR");
                let mut area = vec![0u8; winmedia::BOOT_AREA_BYTES];
                disk_file
                    .read_exact_at(&mut area, data_offset)
                    .expect("read PBR");
                winmedia::patch_boot_record(&mut area, filesystem, (data_offset / 512) as u32)
                    .unwrap_or_else(|error| panic!("{filesystem:?} boot record: {error}"));
                disk_file
                    .write_all_at(&area, data_offset)
                    .expect("write PBR");
                disk_file.sync_all().expect("sync backing file");
                drop_cached_pages(&disk_file).expect("drop cache");
                udisks.rescan(&disk.block).expect("rescan");
            }

            // The boot-code writes make udev re-probe and re-create objects.
            let data = (0..100)
                .find_map(|_| {
                    std::thread::sleep(Duration::from_millis(100));
                    udisks
                        .partitions(&disk.block)
                        .ok()?
                        .into_iter()
                        .find(|part| {
                            udisks
                                .partition_geometry(part)
                                .is_ok_and(|geometry| geometry.number == 1)
                                && udisks.mount_points(part).is_ok()
                        })
                })
                .expect("data partition after rescan");
            let mounted = Mounted::new(&udisks, &data, "ro")
                .unwrap_or_else(|error| panic!("{scheme:?} {filesystem:?} mount: {error}"));
            let mut iso = File::open(&iso_path).expect("reopen ISO");
            let listing = isofs::list(&mut iso).expect("list").expect("listing");
            for entry in listing.entries.iter().filter(|entry| !entry.is_dir) {
                let mut expected = Vec::new();
                isofs::copy_file(&mut iso, entry, &mut [0u8; 65536], |chunk| {
                    expected.extend_from_slice(chunk);
                    Ok(())
                })
                .expect("read ISO entry");
                let actual = std::fs::read(mounted.path.join(&entry.path))
                    .unwrap_or_else(|error| panic!("{}: {error}", entry.path));
                assert!(actual == expected, "{} differs", entry.path);
            }
            mounted.unmount().expect("unmount data");
            // Optionally keep the media for a firmware boot test, with a
            // real EFI application standing in for Windows Setup.
            if let Some(keep) = std::env::var_os("RUFUS_TEST_KEEP_DIR") {
                if let Some(app) = std::env::var_os("RUFUS_TEST_EFI_APP") {
                    let mounted = Mounted::new(&udisks, &data, "").expect("mount data");
                    std::fs::copy(app, mounted.path.join("efi/boot/bootx64.efi"))
                        .expect("install EFI application");
                    mounted.unmount().expect("unmount data");
                }
                drop(disk);
                std::fs::copy(
                    &disk_path,
                    Path::new(&keep).join(format!("{scheme:?}-{filesystem:?}.img")),
                )
                .expect("keep disk image");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
