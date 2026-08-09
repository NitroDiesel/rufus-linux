//! Privileged operation engine. The binary is a thin CLI around this library
//! so unit tests can exercise validation and dry-run without root.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::ptr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use rufus_core::plan::{FileSystem, PartitionScheme, VerificationLevel, WriteMode};
use rufus_core::progress::{Cancellability, CancellationToken, JobId, ProgressStage, ProgressUnit};
use rufus_helper_protocol::{
    encode_line, FormatSpec, HelperEvent, HelperOperation, HelperRequest, HelperResult,
    ProtocolError, TargetIdentity,
};
use rufus_linux_platform::{
    discover_devices, mount_belongs_to_identity, mount_records, DeviceIdentity, PlatformError,
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

#[derive(Clone, Debug)]
struct InvokingUser {
    uid: libc::uid_t,
    gid: libc::gid_t,
    username: CString,
}

impl InvokingUser {
    fn for_execution() -> Result<Self, HelperError> {
        let effective_uid = unsafe { libc::geteuid() };
        let uid = if effective_uid == 0 {
            let value = std::env::var("PKEXEC_UID").map_err(|_| {
                HelperError::Operation(
                    "root execution requires a valid PKEXEC_UID from pkexec".into(),
                )
            })?;
            parse_pkexec_uid(&value)?
        } else {
            effective_uid
        };
        Self::from_uid(uid)
    }

    fn from_uid(uid: libc::uid_t) -> Result<Self, HelperError> {
        let mut buffer_size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
        if buffer_size < 1024 {
            buffer_size = 16 * 1024;
        }
        let mut buffer_size = usize::try_from(buffer_size)
            .unwrap_or(16 * 1024)
            .min(1024 * 1024);

        loop {
            let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
            let mut result = ptr::null_mut();
            let mut buffer = vec![0u8; buffer_size];
            let status = unsafe {
                libc::getpwuid_r(
                    uid,
                    record.as_mut_ptr(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut result,
                )
            };
            if status == libc::ERANGE && buffer_size < 1024 * 1024 {
                buffer_size = (buffer_size * 2).min(1024 * 1024);
                continue;
            }
            if status != 0 {
                return Err(HelperError::Operation(format!(
                    "could not resolve invoking uid {uid}: {}",
                    io::Error::from_raw_os_error(status)
                )));
            }
            if result.is_null() {
                return Err(HelperError::Operation(format!(
                    "PKEXEC_UID {uid} does not identify a local user"
                )));
            }
            let record = unsafe { record.assume_init() };
            if record.pw_uid != uid || record.pw_name.is_null() {
                return Err(HelperError::Operation(
                    "invoking user account data was inconsistent".into(),
                ));
            }
            let username = unsafe { CStr::from_ptr(record.pw_name) }.to_owned();
            if username.as_bytes().is_empty() {
                return Err(HelperError::Operation(
                    "invoking user account has an empty name".into(),
                ));
            }
            return Ok(Self {
                uid,
                gid: record.pw_gid,
                username,
            });
        }
    }
}

fn parse_pkexec_uid(value: &str) -> Result<libc::uid_t, HelperError> {
    value.parse::<libc::uid_t>().map_err(|_| {
        HelperError::Operation("PKEXEC_UID must be an unsigned numeric user id".into())
    })
}

fn supplementary_groups() -> Result<Vec<libc::gid_t>, HelperError> {
    let count = unsafe { libc::getgroups(0, ptr::null_mut()) };
    if count < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let mut groups = vec![0; count as usize];
    if count > 0 {
        let read = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
        if read != count {
            return Err(io::Error::last_os_error().into());
        }
    }
    Ok(groups)
}

fn set_supplementary_groups(groups: &[libc::gid_t]) -> io::Result<()> {
    let pointer = if groups.is_empty() {
        ptr::null()
    } else {
        groups.as_ptr()
    };
    if unsafe { libc::setgroups(groups.len(), pointer) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_effective_uid(uid: libc::uid_t) -> io::Result<()> {
    if unsafe { libc::seteuid(uid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_effective_gid(gid: libc::gid_t) -> io::Result<()> {
    if unsafe { libc::setegid(gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn restore_root_credentials(
    original_gid: libc::gid_t,
    original_groups: &[libc::gid_t],
) -> Result<(), HelperError> {
    set_effective_uid(0)?;
    set_effective_gid(original_gid)?;
    set_supplementary_groups(original_groups)?;
    Ok(())
}

fn open_source_as_user(path: &Path, user: &InvokingUser) -> Result<File, HelperError> {
    let open = || {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
    };

    let effective_uid = unsafe { libc::geteuid() };
    if effective_uid != 0 || user.uid == 0 {
        return Ok(open()?);
    }

    // The helper binary is single-threaded. Temporarily adopting the caller's
    // effective credentials makes every pathname component and the final open
    // obey exactly the caller's DAC permissions, including supplementary groups.
    let original_gid = unsafe { libc::getegid() };
    let original_groups = supplementary_groups()?;
    if unsafe { libc::initgroups(user.username.as_ptr(), user.gid) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    if let Err(error) = set_effective_gid(user.gid) {
        set_supplementary_groups(&original_groups)?;
        return Err(error.into());
    }
    if let Err(error) = set_effective_uid(user.uid) {
        set_effective_gid(original_gid)?;
        set_supplementary_groups(&original_groups)?;
        return Err(error.into());
    }

    let opened = open();
    restore_root_credentials(original_gid, &original_groups)?;
    Ok(opened?)
}

fn bind_source(
    source: &rufus_helper_protocol::SourceSpec,
    user: &InvokingUser,
) -> Result<File, HelperError> {
    let file = open_source_as_user(&source.path, user)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(HelperError::Operation(
            "source is not a regular file".into(),
        ));
    }
    if metadata.len() != source.size_bytes {
        return Err(HelperError::Operation(
            "source image size changed since selection".into(),
        ));
    }
    Ok(file)
}

fn source_is_on_target(source: &File, target: &DeviceIdentity) -> Result<bool, HelperError> {
    let source_device = source.metadata()?.dev();
    let source_sysfs = PathBuf::from(format!(
        "/sys/dev/block/{}:{}",
        libc::major(source_device),
        libc::minor(source_device)
    ));
    match std::fs::canonicalize(source_sysfs) {
        Ok(path) => Ok(path.starts_with(&target.sysfs_path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
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

    validate_observed_identity(expected, &observed)?;
    if observed.read_only {
        return Err(HelperError::Revalidation("device is read-only".into()));
    }
    let metadata = std::fs::metadata(&observed.node)?;
    if !metadata.file_type().is_block_device() {
        return Err(HelperError::Revalidation(
            "target is not a block device".into(),
        ));
    }

    let mounts = mount_records()?;
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

fn validate_observed_identity(
    expected: &TargetIdentity,
    observed: &DeviceIdentity,
) -> Result<(), HelperError> {
    if observed.major != expected.fingerprint.number.major
        || observed.minor != expected.fingerprint.number.minor
    {
        return Err(HelperError::Revalidation(
            "device major:minor mismatch".into(),
        ));
    }
    if observed.sysfs_path != expected.fingerprint.canonical_sysfs_path {
        return Err(HelperError::Revalidation(
            "canonical sysfs path mismatch".into(),
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

    let fingerprint_serial = expected.fingerprint.serial.as_deref().unwrap_or_default();
    if expected.serial != fingerprint_serial {
        return Err(HelperError::Revalidation(
            "request contains inconsistent serial identity".into(),
        ));
    }
    if observed.serial != fingerprint_serial {
        return Err(HelperError::Revalidation("device serial mismatch".into()));
    }
    if observed.model != expected.model {
        return Err(HelperError::Revalidation("device model mismatch".into()));
    }
    Ok(())
}

fn sysfs_has_holders(device: &DeviceIdentity) -> Result<bool, HelperError> {
    let class_path = Path::new("/sys/class/block").join(&device.kernel_name);
    let mut nodes = vec![class_path.clone()];
    for entry in std::fs::read_dir(&class_path)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(&device.kernel_name) {
            nodes.push(entry.path());
        }
    }
    for node in nodes {
        let holders = node.join("holders");
        let mut entries = std::fs::read_dir(holders)?;
        if entries.next().transpose()?.is_some() {
            return Ok(true);
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

struct ManagedChild {
    child: Child,
    process_group: libc::pid_t,
    reaped: bool,
}

impl ManagedChild {
    fn spawn(command: &mut Command, context: &str) -> Result<Self, HelperError> {
        // SAFETY: only async-signal-safe syscalls run between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() == 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "helper exited before child setup completed",
                    ));
                }
                Ok(())
            });
        }
        let child = command
            .spawn()
            .map_err(|error| HelperError::Operation(format!("{context}: {error}")))?;
        let process_group = libc::pid_t::try_from(child.id())
            .map_err(|_| HelperError::Operation(format!("{context}: invalid child pid")))?;
        Ok(Self {
            child,
            process_group,
            reaped: false,
        })
    }

    fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>, HelperError> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    fn wait(&mut self) -> Result<ExitStatus, HelperError> {
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }

    fn signal_group(&self, signal: libc::c_int) -> io::Result<()> {
        if unsafe { libc::kill(-self.process_group, signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    fn group_exists(&self) -> io::Result<bool> {
        if unsafe { libc::kill(-self.process_group, 0) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            Some(libc::EPERM) => Ok(true),
            _ => Err(error),
        }
    }

    fn terminate_and_reap(&mut self) -> Result<(), HelperError> {
        self.signal_group(libc::SIGTERM)?;
        let deadline = Instant::now() + Duration::from_millis(750);
        while Instant::now() < deadline {
            if !self.reaped {
                let _ = self.try_wait()?;
            }
            if !self.group_exists()? {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if self.group_exists()? {
            self.signal_group(libc::SIGKILL)?;
        }
        if !self.reaped {
            let _ = self.wait()?;
        }
        Ok(())
    }

    fn wait_with_cancellation(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<ExitStatus, HelperError> {
        loop {
            if cancel.is_requested() {
                self.terminate_and_reap()?;
                return Err(HelperError::Cancelled);
            }
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.signal_group(libc::SIGKILL);
            let _ = self.child.wait();
            self.reaped = true;
        }
    }
}

fn run_command(
    command: &mut Command,
    cancel: &CancellationToken,
    context: &str,
) -> Result<ExitStatus, HelperError> {
    let mut child = ManagedChild::spawn(command, context)?;
    child.wait_with_cancellation(cancel)
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
    let invoking_user = if options.dry_run {
        let effective_uid = unsafe { libc::geteuid() };
        if effective_uid == 0 {
            None
        } else {
            Some(InvokingUser::from_uid(effective_uid)?)
        }
    } else {
        Some(InvokingUser::for_execution()?)
    };
    let source_file = match (&request.operation, invoking_user.as_ref()) {
        (HelperOperation::WriteMedia { source, .. }, Some(user)) => {
            Some(bind_source(source, user)?)
        }
        _ => None,
    };
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
            let source_file = source_file.as_ref().ok_or_else(|| {
                HelperError::Revalidation("bound source descriptor was unavailable".into())
            })?;
            if source_is_on_target(source_file, &identity)? {
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
        unmount_target(identity, &options.cancel)?;
    }
    check_cancel(&options.cancel)?;

    // Unmounting and swap deactivation change kernel state and create a race
    // window. Resolve the complete identity again immediately before opening
    // the destructive target.
    let observed = if options.dry_run {
        None
    } else {
        Some(revalidate_target(&request.target)?)
    };
    let target_node = observed
        .as_ref()
        .map(|identity| identity.node.as_path())
        .unwrap_or(request.target.node.as_path());

    let exclusive = if options.dry_run {
        None
    } else {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(target_node)?;
        let opened = file.metadata()?;
        let observed = observed.as_ref().ok_or_else(|| {
            HelperError::Revalidation("post-unmount target identity was unavailable".into())
        })?;
        let opened_major = libc::major(opened.rdev());
        let opened_minor = libc::minor(opened.rdev());
        if !opened.file_type().is_block_device()
            || opened_major != observed.major
            || opened_minor != observed.minor
            || opened_major != request.target.fingerprint.number.major
            || opened_minor != request.target.fingerprint.number.minor
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
                    run_badblocks(target_node, &options.cancel)?;
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
                let source_file = source_file.as_ref().ok_or_else(|| {
                    HelperError::Operation("bound source descriptor was not available".into())
                })?;
                Some(write_image(
                    target,
                    WriteSource {
                        file: source_file,
                        spec: source,
                        invoking_user: invoking_user.as_ref(),
                    },
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
                flush_device(target_node, &options.cancel)?;
                reread_partition_table(target_node, &options.cancel)?;
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
                run_badblocks(target_node, &options.cancel)?;
            }
            let mut format_context = PartitionFormatContext {
                job_id: request.job_id,
                dry_run: options.dry_run,
                sink: &mut sink,
                step: &mut step,
                stages_total,
                cancel: &options.cancel,
            };
            run_partition_format(target_node, format, &mut format_context)?;
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
                flush_device(target_node, &options.cancel)?;
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

struct PartitionFormatContext<'a> {
    job_id: JobId,
    dry_run: bool,
    sink: &'a mut EventSink,
    step: &'a mut u64,
    stages_total: u64,
    cancel: &'a CancellationToken,
}

fn run_partition_format(
    target_node: &Path,
    format: &FormatSpec,
    context: &mut PartitionFormatContext<'_>,
) -> Result<(), HelperError> {
    emit(
        context.sink,
        progress(
            context.job_id,
            ProgressStage::Wiping,
            *context.step,
            Some(context.stages_total),
            Some("wipe leading sectors".into()),
        ),
    );
    *context.step += 1;
    if !context.dry_run {
        wipe_leading(target_node, context.cancel)?;
    }

    emit(
        context.sink,
        progress(
            context.job_id,
            ProgressStage::Partitioning,
            *context.step,
            Some(context.stages_total),
            Some(format!("{:?}", format.scheme)),
        ),
    );
    *context.step += 1;
    if !context.dry_run {
        create_partition_table(target_node, format.scheme, context.cancel)?;
    }

    emit(
        context.sink,
        progress(
            context.job_id,
            ProgressStage::Formatting,
            *context.step,
            Some(context.stages_total),
            Some(format.filesystem.as_str().into()),
        ),
    );
    *context.step += 1;
    if !context.dry_run {
        let part = if format.scheme == PartitionScheme::SuperFloppy {
            target_node.to_owned()
        } else {
            wait_for_first_partition(target_node, context.cancel)?
        };
        format_partition(&part, format, context.cancel)?;
    }
    Ok(())
}

fn unmount_target(
    identity: &DeviceIdentity,
    cancel: &CancellationToken,
) -> Result<(), HelperError> {
    let mut mounted = mount_records()?
        .into_iter()
        .filter(|mount| mount_belongs_to_identity(mount, identity))
        .collect::<Vec<_>>();
    mounted.sort_by_key(|mount| std::cmp::Reverse(mount.mount_point.as_os_str().len()));
    if !mounted.is_empty() {
        let umount = find_tool(tools::UMOUNT)?;
        for mount in mounted {
            let mut command = Command::new(umount);
            command.arg(&mount.mount_point).env_clear();
            let status = run_command(&mut command, cancel, "unmount")?;
            if !status.success() {
                return Err(HelperError::Operation(format!(
                    "could not unmount {}",
                    mount.mount_point.display()
                )));
            }
        }
    }

    let swaps = std::fs::read_to_string("/proc/swaps")?;
    let swap_sources =
        parse_swap_sources(&swaps).map_err(|message| HelperError::Revalidation(message.into()))?;
    for source in swap_sources {
        if node_is_device_or_partition(Path::new(source), &identity.node) {
            let swapoff = find_tool(tools::SWAPOFF)?;
            let mut command = Command::new(swapoff);
            command.arg(source).env_clear();
            let status = run_command(&mut command, cancel, "swapoff")?;
            if !status.success() {
                return Err(HelperError::Operation(format!(
                    "could not disable swap on {source}"
                )));
            }
        }
    }

    if mount_records()?
        .iter()
        .any(|mount| mount_belongs_to_identity(mount, identity))
    {
        return Err(HelperError::Operation(
            "target still has mounted filesystems".into(),
        ));
    }
    Ok(())
}

fn parse_swap_sources(input: &str) -> Result<Vec<&str>, &'static str> {
    let mut lines = input.lines();
    let header = lines.next().ok_or("swap state is empty")?;
    let columns = header.split_whitespace().collect::<Vec<_>>();
    if columns.len() < 5 || columns[0] != "Filename" {
        return Err("swap state header is malformed");
    }

    let mut sources = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 5 {
            return Err("swap state entry is malformed");
        }
        sources.push(fields[0]);
    }
    Ok(sources)
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

fn wipe_leading(node: &Path, cancel: &CancellationToken) -> Result<(), HelperError> {
    let mut file = std::fs::OpenOptions::new().write(true).open(node)?;
    let zeros = vec![0u8; 1024 * 1024];
    for _ in 0..8 {
        if cancel.is_requested() {
            file.sync_all()?;
            return Err(HelperError::Cancelled);
        }
        file.write_all(&zeros)?;
    }
    file.sync_all()?;
    Ok(())
}

fn create_partition_table(
    node: &Path,
    scheme: PartitionScheme,
    cancel: &CancellationToken,
) -> Result<(), HelperError> {
    let label = match scheme {
        PartitionScheme::Mbr => "dos",
        PartitionScheme::Gpt => "gpt",
        PartitionScheme::SuperFloppy => {
            // No partition table — whole device is the filesystem.
            return Ok(());
        }
    };
    let parted = find_tool(tools::PARTED)?;
    let mut command = Command::new(parted);
    command
        .args(["-s"])
        .arg(node)
        .args(["mklabel", label])
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin");
    let status = run_command(&mut command, cancel, "parted")?;
    if !status.success() {
        return Err(HelperError::Operation("parted mklabel failed".into()));
    }
    if scheme != PartitionScheme::SuperFloppy {
        let mut command = Command::new(parted);
        command
            .args(["-s"])
            .arg(node)
            .args(["mkpart", "primary", "0%", "100%"])
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin");
        let status = run_command(&mut command, cancel, "parted mkpart")?;
        if !status.success() {
            return Err(HelperError::Operation("parted mkpart failed".into()));
        }
    }
    reread_partition_table(node, cancel)?;
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

fn wait_for_first_partition(
    disk: &Path,
    cancel: &CancellationToken,
) -> Result<PathBuf, HelperError> {
    reread_partition_table(disk, cancel)?;
    if let Ok(udevadm) = find_tool(tools::UDEVADM) {
        let mut command = Command::new(udevadm);
        command.args(["settle", "--timeout=10"]).env_clear();
        let _ = run_command(&mut command, cancel, "udevadm settle")?;
    }
    let partition = first_partition_node(disk);
    for _ in 0..100 {
        check_cancel(cancel)?;
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

fn reread_partition_table(node: &Path, cancel: &CancellationToken) -> Result<(), HelperError> {
    let blockdev = find_tool(tools::BLOCKDEV)?;
    let mut command = Command::new(blockdev);
    command.arg("--rereadpt").arg(node).env_clear();
    let status = run_command(&mut command, cancel, "partition reread")?;
    if !status.success() {
        return Err(HelperError::Operation(
            "kernel rejected the new partition table".into(),
        ));
    }
    Ok(())
}

fn format_partition(
    part: &Path,
    format: &FormatSpec,
    cancel: &CancellationToken,
) -> Result<(), HelperError> {
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
    let status = run_command(&mut cmd, cancel, tool)?;
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

struct WriteSource<'a> {
    file: &'a File,
    spec: &'a rufus_helper_protocol::SourceSpec,
    invoking_user: Option<&'a InvokingUser>,
}

fn write_image(
    target: &File,
    source: WriteSource<'_>,
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

    let mut source_reader = source.file.try_clone()?;
    source_reader.seek(SeekFrom::Start(0))?;
    let mut decoder_child = None;
    let mut reader: Box<dyn Read> =
        if source.spec.kind == rufus_core::plan::ImageSourceKind::CompressedRaw {
            let (tool, arguments) = decompressor_for(&source.spec.path)?;
            let mut command = Command::new(tool);
            command
                .args(arguments)
                .env_clear()
                .stdin(Stdio::from(source_reader))
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            if let Some(user) = source.invoking_user {
                drop_decoder_privileges(&mut command, user);
            }
            let mut child = ManagedChild::spawn(&mut command, "decompressor")?;
            let stdout = child
                .child_mut()
                .stdout
                .take()
                .ok_or_else(|| HelperError::Operation("decompressor stdout missing".into()))?;
            set_nonblocking(stdout.as_raw_fd())?;
            decoder_child = Some(child);
            Box::new(stdout)
        } else {
            Box::new(source_reader)
        };

    let mut destination = target.try_clone()?;
    destination.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut written = 0u64;
    let started = Instant::now();
    let mut last_progress = Instant::now();
    loop {
        if cancel.is_requested() {
            cancel_image_write(&destination, &mut decoder_child)?;
            return Err(HelperError::Cancelled);
        }
        let count = match reader.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
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
                    total: source.spec.decompressed_size_bytes.or_else(|| {
                        (source.spec.kind != rufus_core::plan::ImageSourceKind::CompressedRaw)
                            .then_some(source.spec.size_bytes)
                    }),
                    bytes_per_second: Some((written as f64 / elapsed) as u64),
                    detail: Some(format!("Writing {}", source.spec.path.display())),
                    cancellability: Cancellability::Immediate,
                },
            );
            last_progress = Instant::now();
        }
    }
    if cancel.is_requested() {
        cancel_image_write(&destination, &mut decoder_child)?;
        return Err(HelperError::Cancelled);
    }
    destination.sync_all()?;

    if written == 0 {
        return Err(HelperError::Operation("source image was empty".into()));
    }
    if source.spec.kind != rufus_core::plan::ImageSourceKind::CompressedRaw
        && written != source.spec.size_bytes
    {
        return Err(HelperError::Operation(
            "source image changed while it was being read".into(),
        ));
    }
    if let Some(expected) = source.spec.decompressed_size_bytes {
        if written != expected {
            return Err(HelperError::Operation(format!(
                "decompressed size mismatch: expected {expected}, wrote {written}"
            )));
        }
    }
    if let Some(mut child) = decoder_child {
        let status = child.wait_with_cancellation(cancel)?;
        if !status.success() {
            return Err(HelperError::Operation(
                "decompressor reported invalid or truncated input".into(),
            ));
        }
    }
    let digest: [u8; 32] = hasher.finalize().into();
    if let Some(expected) = &source.spec.expected_sha256 {
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

fn set_nonblocking(fd: libc::c_int) -> Result<(), HelperError> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error().into());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn cancel_image_write(
    destination: &File,
    decoder: &mut Option<ManagedChild>,
) -> Result<(), HelperError> {
    let decoder_result = decoder
        .as_mut()
        .map_or(Ok(()), ManagedChild::terminate_and_reap);
    let sync_result = destination.sync_all();
    decoder_result?;
    sync_result?;
    Ok(())
}

fn drop_decoder_privileges(command: &mut Command, user: &InvokingUser) {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let uid = user.uid;
    let gid = user.gid;
    // SAFETY: only async-signal-safe credential syscalls run between fork and
    // exec. The decoder needs no filesystem access because its archive is stdin.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::setgroups(0, ptr::null()) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::setgid(gid) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::setuid(uid) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn decompressor_for(path: &Path) -> Result<(&'static str, Vec<&'static str>), HelperError> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "gz" | "z" => Ok((find_tool(tools::GZIP)?, vec!["-dc"])),
        "bz2" => Ok((find_tool(tools::BZIP2)?, vec!["-dc"])),
        "xz" | "lzma" => Ok((find_tool(tools::XZ)?, vec!["-dc"])),
        "zst" | "zstd" => Ok((find_tool(tools::ZSTD)?, vec!["-dc"])),
        "zip" => Ok((find_tool(tools::BSDTAR)?, vec!["-xOf", "-"])),
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

fn flush_device(node: &Path, cancel: &CancellationToken) -> Result<(), HelperError> {
    let blockdev = find_tool(tools::BLOCKDEV)?;
    let mut command = Command::new(blockdev);
    command.args(["--flushbufs"]).arg(node).env_clear();
    let status = run_command(&mut command, cancel, "device flush")?;
    if !status.success() {
        return Err(HelperError::Operation(
            "device cache flush was rejected".into(),
        ));
    }
    Ok(())
}

fn run_badblocks(node: &Path, cancel: &CancellationToken) -> Result<(), HelperError> {
    let badblocks = find_tool(tools::BADBLOCKS)?;
    let mut command = Command::new(badblocks);
    command.args(["-w", "-s"]).arg(node).env_clear();
    let status = run_command(&mut command, cancel, "badblocks")?;
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

    fn observed_identity() -> DeviceIdentity {
        DeviceIdentity {
            node: PathBuf::from("/dev/sdb"),
            sysfs_path: PathBuf::from("/sys/devices/example"),
            kernel_name: "sdb".into(),
            major: 8,
            minor: 16,
            size_bytes: 16_000_000_000,
            logical_sector_size: 512,
            model: "Test".into(),
            vendor: "Example".into(),
            serial: "TEST".into(),
            transport: "usb".into(),
            removable: true,
            read_only: false,
        }
    }

    #[test]
    fn observed_identity_comparison_is_strict() {
        let request = format_request();
        let mut observed = observed_identity();
        assert!(validate_observed_identity(&request.target, &observed).is_ok());

        observed.sysfs_path = PathBuf::from("/sys/devices/replacement");
        assert!(validate_observed_identity(&request.target, &observed).is_err());
        observed = observed_identity();
        observed.serial.clear();
        assert!(validate_observed_identity(&request.target, &observed).is_err());
        observed = observed_identity();
        observed.model = "Replacement".into();
        assert!(validate_observed_identity(&request.target, &observed).is_err());
    }

    #[test]
    fn request_serial_fields_must_agree() {
        let mut request = format_request();
        request.target.serial = "REPLACEMENT".into();
        let error = validate_observed_identity(&request.target, &observed_identity())
            .expect_err("inconsistent request identity must be rejected");
        assert!(error.to_string().contains("inconsistent serial"));
    }

    #[test]
    fn swap_state_parser_fails_closed() {
        let input = "Filename Type Size Used Priority\n/dev/sdb1 partition 1024 0 -2\n";
        assert_eq!(parse_swap_sources(input), Ok(vec!["/dev/sdb1"]));
        assert!(parse_swap_sources("").is_err());
        assert!(parse_swap_sources("Filename Type\n/dev/sdb1\n").is_err());
    }

    #[test]
    fn pkexec_uid_must_be_numeric() {
        assert_eq!(parse_pkexec_uid("1000").expect("numeric uid"), 1000);
        assert!(parse_pkexec_uid("").is_err());
        assert!(parse_pkexec_uid("-1").is_err());
        assert!(parse_pkexec_uid("user").is_err());
    }

    #[test]
    fn managed_child_is_terminated_and_reaped_on_cancellation() {
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let mut child = ManagedChild::spawn(&mut command, "sleep fixture")
            .expect("spawn managed child fixture");
        let pid = child.process_group;
        child
            .terminate_and_reap()
            .expect("terminate managed child fixture");
        assert!(child.reaped);
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[test]
    fn run_command_honors_cancellation_while_child_is_running() {
        let cancel = CancellationToken::new();
        let requester = cancel.clone();
        let cancellation_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            requester.request();
        });
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let started = Instant::now();
        assert!(matches!(
            run_command(&mut command, &cancel, "sleep fixture"),
            Err(HelperError::Cancelled)
        ));
        cancellation_thread
            .join()
            .expect("join cancellation thread");
        assert!(started.elapsed() < Duration::from_secs(2));
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
    fn request_validation_does_not_open_the_source_as_root() {
        let mut request = format_request();
        request.operation = HelperOperation::WriteMedia {
            write_mode: WriteMode::DdImage,
            source: SourceSpec {
                path: PathBuf::from("/source/is/opened/later/by-the-invoking-user.img"),
                kind: ImageSourceKind::Raw,
                size_bytes: 4096,
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
        validate_request(&request).expect("validation must not open the source path");
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
        let source_file = File::open(&source_path).expect("open source fixture");
        let mut events: EventSink = Box::new(|_| {});
        let receipt = write_image(
            &target,
            WriteSource {
                file: &source_file,
                spec: &source,
                invoking_user: None,
            },
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

    #[test]
    fn raw_writer_returns_cancelled_after_flushing_the_destination() {
        let base = std::env::temp_dir();
        let unique = format!(
            "rufus-helper-cancelled-write-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        );
        let source_path = base.join(format!("{unique}.img"));
        let target_path = base.join(format!("{unique}.target"));
        let payload = vec![0x5a; 1024];
        std::fs::write(&source_path, &payload).expect("write image fixture");
        let source_file = File::open(&source_path).expect("open source fixture");
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
        let cancel = CancellationToken::new();
        cancel.request();
        let mut events: EventSink = Box::new(|_| {});

        assert!(matches!(
            write_image(
                &target,
                WriteSource {
                    file: &source_file,
                    spec: &source,
                    invoking_user: None,
                },
                WriteMode::DdImage,
                &cancel,
                payload.len() as u64 * 2,
                JobId::new(4),
                &mut events,
            ),
            Err(HelperError::Cancelled)
        ));

        let _ = std::fs::remove_file(source_path);
        let _ = std::fs::remove_file(target_path);
    }

    #[test]
    fn bound_source_descriptor_survives_path_replacement() {
        let base = std::env::temp_dir();
        let unique = format!(
            "rufus-helper-bound-source-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        );
        let source_path = base.join(format!("{unique}.img"));
        let moved_path = base.join(format!("{unique}.opened"));
        let target_path = base.join(format!("{unique}.target"));
        let original = b"original image bytes";
        let replacement = b"replacement contents";
        assert_eq!(original.len(), replacement.len());
        std::fs::write(&source_path, original).expect("write original source");

        let source = SourceSpec {
            path: source_path.clone(),
            kind: ImageSourceKind::Raw,
            size_bytes: original.len() as u64,
            decompressed_size_bytes: None,
            expected_sha256: None,
        };
        let current_user =
            InvokingUser::from_uid(unsafe { libc::geteuid() }).expect("resolve current test user");
        let source_file = bind_source(&source, &current_user).expect("bind source descriptor");
        std::fs::rename(&source_path, &moved_path).expect("move opened source");
        std::fs::write(&source_path, replacement).expect("replace source path");

        let target = File::create(&target_path).expect("create target fixture");
        target
            .set_len((original.len() * 2) as u64)
            .expect("size target fixture");
        let mut events: EventSink = Box::new(|_| {});
        write_image(
            &target,
            WriteSource {
                file: &source_file,
                spec: &source,
                invoking_user: None,
            },
            WriteMode::DdImage,
            &CancellationToken::new(),
            (original.len() * 2) as u64,
            JobId::new(2),
            &mut events,
        )
        .expect("write bound source");

        let written = std::fs::read(&target_path).expect("read target fixture");
        assert_eq!(&written[..original.len()], original);
        let _ = std::fs::remove_file(source_path);
        let _ = std::fs::remove_file(moved_path);
        let _ = std::fs::remove_file(target_path);
    }

    #[test]
    fn compressed_writer_reads_the_bound_descriptor_from_stdin() {
        let Ok(gzip) = find_tool(tools::GZIP) else {
            return;
        };
        let base = std::env::temp_dir();
        let unique = format!(
            "rufus-helper-bound-compressed-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        );
        let source_path = base.join(format!("{unique}.gz"));
        let moved_path = base.join(format!("{unique}.opened"));
        let target_path = base.join(format!("{unique}.target"));
        let payload = vec![0x3c; 128 * 1024 + 31];

        let mut encoder = Command::new(gzip)
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start gzip fixture encoder");
        encoder
            .stdin
            .take()
            .expect("gzip stdin")
            .write_all(&payload)
            .expect("write gzip input");
        let compressed = encoder
            .wait_with_output()
            .expect("finish gzip fixture encoder");
        assert!(compressed.status.success());
        std::fs::write(&source_path, &compressed.stdout).expect("write compressed source");

        let source = SourceSpec {
            path: source_path.clone(),
            kind: ImageSourceKind::CompressedRaw,
            size_bytes: compressed.stdout.len() as u64,
            decompressed_size_bytes: Some(payload.len() as u64),
            expected_sha256: None,
        };
        let current_user =
            InvokingUser::from_uid(unsafe { libc::geteuid() }).expect("resolve current test user");
        let source_file = bind_source(&source, &current_user).expect("bind compressed source");
        std::fs::rename(&source_path, &moved_path).expect("move opened source");
        std::fs::write(&source_path, vec![0u8; compressed.stdout.len()])
            .expect("replace compressed source path");

        let target = File::create(&target_path).expect("create target fixture");
        target
            .set_len((payload.len() * 2) as u64)
            .expect("size target fixture");
        let mut events: EventSink = Box::new(|_| {});
        write_image(
            &target,
            WriteSource {
                file: &source_file,
                spec: &source,
                invoking_user: Some(&current_user),
            },
            WriteMode::DdImage,
            &CancellationToken::new(),
            (payload.len() * 2) as u64,
            JobId::new(3),
            &mut events,
        )
        .expect("write bound compressed source");

        let written = std::fs::read(&target_path).expect("read target fixture");
        assert_eq!(&written[..payload.len()], payload.as_slice());
        let _ = std::fs::remove_file(source_path);
        let _ = std::fs::remove_file(moved_path);
        let _ = std::fs::remove_file(target_path);
    }

    #[test]
    fn bound_source_rejects_symlinks_and_size_changes() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir();
        let unique = format!(
            "rufus-helper-bound-validation-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        );
        let source_path = base.join(format!("{unique}.img"));
        let link_path = base.join(format!("{unique}.link"));
        std::fs::write(&source_path, b"source").expect("write source fixture");
        symlink(&source_path, &link_path).expect("create source symlink");
        let current_user =
            InvokingUser::from_uid(unsafe { libc::geteuid() }).expect("resolve current test user");

        let mut source = SourceSpec {
            path: link_path.clone(),
            kind: ImageSourceKind::Raw,
            size_bytes: 6,
            decompressed_size_bytes: None,
            expected_sha256: None,
        };
        assert!(bind_source(&source, &current_user).is_err());
        source.path = source_path.clone();
        source.size_bytes = 7;
        assert!(bind_source(&source, &current_user).is_err());

        let _ = std::fs::remove_file(link_path);
        let _ = std::fs::remove_file(source_path);
    }
}
