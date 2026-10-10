//! Windows installer media, laid out like upstream Rufus:
//!
//! * NTFS or exFAT data partition, plus a 1 MiB FAT `UEFI:NTFS` partition at
//!   the end of the disk whose signed bootloader chain-loads the data
//!   partition on UEFI systems. On GPT it is Microsoft basic data (never an
//!   ESP: two ESPs break Windows Setup) named `UEFI:NTFS` with the
//!   no-drive-letter attribute; on MBR it is type 0xEF.
//! * FAT32 needs no extra partition; the firmware boots it directly.
//! * BIOS targets get the Windows 7 MBR, an active data partition, and the
//!   ms-sys NTFS/FAT32 boot records that load BOOTMGR.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use rufus_core::plan::{BootMode, FileSystem, PartitionScheme};
use rufus_core::progress::CancellationToken;
use rufus_image::isofs::{self, IsoEntry, IsoListing};
use sha2::{Digest, Sha256};

use crate::{check_cancel, HelperError};

const MIB: u64 = 1024 * 1024;
/// Largest file FAT32 can hold.
pub(crate) const FAT32_MAX_FILE: u64 = 4 * 1024 * 1024 * 1024 - 1;
pub(crate) const UEFI_NTFS_SIZE: u64 = MIB;
pub(crate) const UEFI_NTFS_NAME: &str = "UEFI:NTFS";
pub(crate) const UEFI_NTFS_LABEL: &str = "UEFI_NTFS";
pub(crate) const MAIN_PARTITION_NAME: &str = "Main Data Partition";
pub(crate) const GPT_BASIC_DATA: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";
/// GPT_BASIC_DATA_ATTRIBUTE_NO_DRIVE_LETTER.
pub(crate) const GPT_NO_DRIVE_LETTER: u64 = 1 << 63;
pub(crate) const MBR_ACTIVE: u64 = 0x80;

macro_rules! payload {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_bytes!(concat!("../assets/uefi-ntfs/", $path)))),*]
    };
}

/// Contents of upstream `res/uefi/uefi-ntfs.img` (UEFI:NTFS 2.8, ntfs-3g and
/// EfiFs drivers); see `assets/uefi-ntfs/PROVENANCE.md`.
pub(crate) const UEFI_NTFS_FILES: &[(&str, &[u8])] = payload!(
    "README.txt",
    "EFI/Boot/bootaa64.efi",
    "EFI/Boot/bootarm.efi",
    "EFI/Boot/bootia32.efi",
    "EFI/Boot/bootriscv64.efi",
    "EFI/Boot/bootx64.efi",
    "EFI/Rufus/exfat_aa64.efi",
    "EFI/Rufus/exfat_arm.efi",
    "EFI/Rufus/exfat_ia32.efi",
    "EFI/Rufus/exfat_riscv64.efi",
    "EFI/Rufus/exfat_x64.efi",
    "EFI/Rufus/ntfs_aa64.efi",
    "EFI/Rufus/ntfs_arm.efi",
    "EFI/Rufus/ntfs_ia32.efi",
    "EFI/Rufus/ntfs_riscv64.efi",
    "EFI/Rufus/ntfs_x64.efi",
);

const MBR_WIN7: &[u8] = include_bytes!("../assets/ms-sys/mbr_win7_0x0.bin");
const BR_NTFS_0X0: &[u8] = include_bytes!("../assets/ms-sys/br_ntfs_0x0.bin");
const BR_NTFS_0X54: &[u8] = include_bytes!("../assets/ms-sys/br_ntfs_0x54.bin");
const BR_FAT32_0X0: &[u8] = include_bytes!("../assets/ms-sys/br_fat32_0x0.bin");
const BR_FAT32PE_0X52: &[u8] = include_bytes!("../assets/ms-sys/br_fat32pe_0x52.bin");
const BR_FAT32PE_0X3F0: &[u8] = include_bytes!("../assets/ms-sys/br_fat32pe_0x3f0.bin");
const BR_FAT32PE_0X1800: &[u8] = include_bytes!("../assets/ms-sys/br_fat32pe_0x1800.bin");

/// Bytes of the data partition that hold boot code, read and patched as one.
pub(crate) const BOOT_AREA_BYTES: usize = 0x2600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub offset: u64,
    pub size: u64,
}

impl Span {
    pub fn end(self) -> u64 {
        self.offset + self.size
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub data: Span,
    pub uefi_ntfs: Option<Span>,
}

/// What a Windows target needs, decided before anything is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Requirements {
    pub uefi_ntfs: bool,
    pub bios_boot: bool,
}

pub(crate) fn requirements(
    filesystem: FileSystem,
    scheme: PartitionScheme,
    boot_mode: BootMode,
) -> Result<Requirements, HelperError> {
    if !matches!(
        filesystem,
        FileSystem::Ntfs | FileSystem::ExFat | FileSystem::Fat32
    ) {
        return Err(HelperError::Operation(
            "Windows installer media needs NTFS, exFAT, or FAT32".into(),
        ));
    }
    if scheme == PartitionScheme::SuperFloppy {
        return Err(HelperError::Operation(
            "Windows installer media needs an MBR or GPT partition table".into(),
        ));
    }
    let uefi = matches!(boot_mode, BootMode::Uefi | BootMode::Dual);
    let bios = matches!(boot_mode, BootMode::Bios | BootMode::Dual);
    if !uefi && !bios {
        return Err(HelperError::Operation(
            "Windows installer media must be bootable".into(),
        ));
    }
    if bios && scheme != PartitionScheme::Mbr {
        return Err(HelperError::Operation(
            "BIOS boot needs the MBR partition scheme".into(),
        ));
    }
    if bios && filesystem == FileSystem::ExFat {
        return Err(HelperError::Operation(
            "BIOS boot of Windows media needs NTFS or FAT32; exFAT boots through UEFI only".into(),
        ));
    }
    Ok(Requirements {
        uefi_ntfs: uefi && filesystem != FileSystem::Fat32,
        bios_boot: bios,
    })
}

/// Upstream's arithmetic with 1 MiB alignment: the data partition starts at
/// 1 MiB and UEFI:NTFS takes the last aligned MiB before the backup GPT.
pub(crate) fn plan_layout(
    disk_size: u64,
    sector_size: u64,
    scheme: PartitionScheme,
    uefi_ntfs: bool,
) -> Result<Layout, HelperError> {
    if scheme == PartitionScheme::Mbr && disk_size / sector_size.max(1) > u64::from(u32::MAX) {
        return Err(HelperError::Operation(
            "MBR cannot address a disk this large; choose GPT".into(),
        ));
    }
    let reserved_tail = match scheme {
        PartitionScheme::Gpt => 33 * sector_size,
        _ => 0,
    };
    let usable_end = disk_size.saturating_sub(reserved_tail) / MIB * MIB;
    let (data_end, uefi) = if uefi_ntfs {
        let start = usable_end.saturating_sub(UEFI_NTFS_SIZE);
        (
            start,
            Some(Span {
                offset: start,
                size: UEFI_NTFS_SIZE,
            }),
        )
    } else {
        (usable_end, None)
    };
    if data_end <= MIB + 64 * MIB {
        return Err(HelperError::Operation(
            "the device is too small for Windows installer media".into(),
        ));
    }
    Ok(Layout {
        data: Span {
            offset: MIB,
            size: data_end - MIB,
        },
        uefi_ntfs: uefi,
    })
}

/// Reject what would fail half way: oversize files for FAT32 and media that
/// cannot hold the files. Runs before the first destructive step.
pub(crate) fn check_fits(
    listing: &IsoListing,
    filesystem: FileSystem,
    data_bytes: u64,
) -> Result<(), HelperError> {
    if matches!(filesystem, FileSystem::Fat32 | FileSystem::Fat) {
        if let Some(largest) = listing.largest_file() {
            if largest.size > FAT32_MAX_FILE {
                return Err(HelperError::Operation(format!(
                    "{} is {} bytes, larger than FAT32 allows; choose NTFS",
                    largest.path, largest.size
                )));
            }
        }
    }
    // File system metadata, cluster slack, and the MFT.
    let needed = listing.total_file_bytes()
        + listing.total_file_bytes() / 50
        + 64 * MIB
        + listing.entries.len() as u64 * 8192;
    if needed > data_bytes {
        return Err(HelperError::Operation(format!(
            "the device is too small: the image needs about {} MiB",
            needed / MIB
        )));
    }
    Ok(())
}

pub(crate) struct CopiedFile {
    path: PathBuf,
    size: u64,
    sha256: [u8; 32],
}

/// Destination for one entry. `IsoEntry::path` is already made of safe
/// names; refuse anything else rather than trust it.
fn destination(root: &Path, relative: &str) -> Result<PathBuf, HelperError> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(HelperError::Operation(format!(
            "unsafe path in image: {}",
            relative.display()
        )));
    }
    Ok(root.join(relative))
}

fn create_new(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

fn ensure_parent(path: &Path, root: &Path) -> Result<(), HelperError> {
    if let Some(parent) = path.parent() {
        if parent != root {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

/// Copy every file of the image into `root`, hashing as it goes.
pub(crate) fn copy_listing(
    iso: &mut File,
    listing: &IsoListing,
    root: &Path,
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(u64, &str),
) -> Result<Vec<CopiedFile>, HelperError> {
    let mut buffer = vec![0u8; 4 * MIB as usize];
    let mut copied = Vec::new();
    let mut done = 0u64;
    for entry in &listing.entries {
        check_cancel(cancel)?;
        let path = destination(root, &entry.path)?;
        if entry.is_dir {
            std::fs::create_dir_all(&path)?;
            continue;
        }
        ensure_parent(&path, root)?;
        copied.push(copy_entry(
            iso,
            entry,
            &path,
            &mut buffer,
            cancel,
            &mut |written| {
                done += written;
                progress(done, &entry.path);
            },
        )?);
    }
    Ok(copied)
}

fn copy_entry(
    iso: &mut File,
    entry: &IsoEntry,
    path: &Path,
    buffer: &mut [u8],
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(u64),
) -> Result<CopiedFile, HelperError> {
    let mut output = create_new(path).map_err(|error| {
        HelperError::Operation(format!("could not create {}: {error}", entry.path))
    })?;
    let mut hasher = Sha256::new();
    isofs::copy_file(iso, entry, buffer, |chunk| {
        if cancel.is_requested() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        output.write_all(chunk)?;
        hasher.update(chunk);
        progress(chunk.len() as u64);
        Ok(())
    })
    .map_err(|error| {
        if cancel.is_requested() {
            HelperError::Cancelled
        } else {
            HelperError::Operation(format!("could not copy {}: {error}", entry.path))
        }
    })?;
    output.sync_all()?;
    Ok(CopiedFile {
        path: path.to_owned(),
        size: entry.size,
        sha256: hasher.finalize().into(),
    })
}

pub(crate) fn copied_bytes(copies: &[CopiedFile]) -> u64 {
    copies.iter().map(|copy| copy.size).sum()
}

/// Read every copied file back and compare it with what was written.
pub(crate) fn verify_copies(
    copies: &[CopiedFile],
    root_written: &Path,
    root_now: &Path,
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(u64),
) -> Result<(), HelperError> {
    let mut buffer = vec![0u8; 4 * MIB as usize];
    let mut done = 0u64;
    for copy in copies {
        let relative = copy.path.strip_prefix(root_written).map_err(|_| {
            HelperError::Operation("copied file is outside the target volume".into())
        })?;
        let mut input = File::open(root_now.join(relative))?;
        let mut hasher = Sha256::new();
        let mut read = 0u64;
        loop {
            check_cancel(cancel)?;
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            read += count as u64;
            done += count as u64;
            progress(done);
        }
        let digest: [u8; 32] = hasher.finalize().into();
        if read != copy.size || digest != copy.sha256 {
            return Err(HelperError::Operation(format!(
                "verification failed: {} differs from the image",
                relative.display()
            )));
        }
    }
    Ok(())
}

pub(crate) fn write_uefi_ntfs_files(root: &Path) -> Result<(), HelperError> {
    for (relative, bytes) in UEFI_NTFS_FILES {
        let path = destination(root, relative)?;
        ensure_parent(&path, root)?;
        let mut output = create_new(&path)?;
        output.write_all(bytes)?;
        output.sync_all()?;
    }
    Ok(())
}

const SETUP_WRAPPER_X64: &[u8] = include_bytes!("../assets/setup/setup_x64.exe");
const SETUP_WRAPPER_ARM64: &[u8] = include_bytes!("../assets/setup/setup_arm64.exe");
const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
const IMAGE_FILE_MACHINE_ARM64: u16 = 0xaa64;
/// Windows 11 24H2, whose in-place upgrades need the Setup wrapper.
const SETUP_WRAPPER_MIN_BUILD: u32 = 26000;

/// A file Rufus adds to the media after the copy, verified like the rest.
fn add_file(
    root: &Path,
    relative: &str,
    bytes: &[u8],
    copies: &mut Vec<CopiedFile>,
) -> Result<(), HelperError> {
    let path = destination(root, relative)?;
    ensure_parent(&path, root)?;
    let mut output = create_new(&path)
        .map_err(|error| HelperError::Operation(format!("could not create {relative}: {error}")))?;
    output.write_all(bytes)?;
    output.sync_all()?;
    copies.push(CopiedFile {
        path,
        size: bytes.len() as u64,
        sha256: Sha256::digest(bytes).into(),
    });
    Ok(())
}

/// The copied file at `relative`, matched without regard to case as the
/// image's own file systems do.
fn find_copy<'a>(
    root: &Path,
    relative: &str,
    copies: &'a mut [CopiedFile],
) -> Option<&'a mut CopiedFile> {
    let wanted = root.join(relative);
    copies.iter_mut().find(|copy| {
        copy.path
            .to_str()
            .zip(wanted.to_str())
            .is_some_and(|(have, want)| have.eq_ignore_ascii_case(want))
    })
}

/// Rename a copied file, keeping its verification record.
fn rename_copy(
    root: &Path,
    relative: &str,
    new_name: &str,
    copies: &mut [CopiedFile],
) -> Result<Option<PathBuf>, HelperError> {
    let Some(copy) = find_copy(root, relative, copies) else {
        return Ok(None);
    };
    let original = copy.path.clone();
    let renamed = original.with_file_name(new_name);
    if std::fs::symlink_metadata(&renamed).is_ok() {
        return Err(HelperError::Operation(format!(
            "{} already exists on the media",
            renamed.display()
        )));
    }
    std::fs::rename(&original, &renamed)?;
    copy.path = renamed;
    Ok(Some(original))
}

fn pe_machine(bytes: &[u8]) -> Option<u16> {
    let at = u32::from_le_bytes(bytes.get(0x3c..0x40)?.try_into().ok()?) as usize;
    if bytes.get(at..at + 4)? != b"PE\0\0" {
        return None;
    }
    Some(u16::from_le_bytes(
        bytes.get(at + 4..at + 6)?.try_into().ok()?,
    ))
}

/// Upstream's `ApplyWindowsCustomization` for installer media: the answer
/// file, and for the hardware bypass, an empty `appraiserres.dll` and the
/// Setup wrapper so in-place upgrades skip the checks too.
pub(crate) fn apply_customization(
    root: &Path,
    answer: &crate::unattend::AnswerFile,
    bypass_requirements: bool,
    build: u32,
    copies: &mut Vec<CopiedFile>,
    log: &mut dyn FnMut(String),
) -> Result<(), HelperError> {
    if bypass_requirements {
        // Setup extracts its own appraiserres.dll when the file is missing,
        // so it must be present and empty.
        if let Some(original) =
            rename_copy(root, "sources/appraiserres.dll", "appraiserres.bak", copies)?
        {
            log("Renamed 'sources/appraiserres.dll' → 'sources/appraiserres.bak'".into());
            let relative = original
                .strip_prefix(root)
                .map_err(|_| HelperError::Operation("file is outside the target volume".into()))?
                .to_string_lossy()
                .into_owned();
            add_file(root, &relative, &[], copies)?;
            log("Created 'sources/appraiserres.dll' placeholder".into());
        }
        if build >= SETUP_WRAPPER_MIN_BUILD {
            install_setup_wrapper(root, copies, log)?;
        }
    }
    add_file(root, answer.path, answer.xml.as_bytes(), copies)?;
    log(format!("Created '{}'", answer.path));
    Ok(())
}

fn install_setup_wrapper(
    root: &Path,
    copies: &mut Vec<CopiedFile>,
    log: &mut dyn FnMut(String),
) -> Result<(), HelperError> {
    let Some(setup) = find_copy(root, "setup.exe", copies) else {
        return Ok(());
    };
    let mut file = File::open(&setup.path)?;
    let mut head = vec![0u8; 4096];
    let count = file.read(&mut head)?;
    let wrapper = match pe_machine(&head[..count]) {
        Some(IMAGE_FILE_MACHINE_AMD64) => SETUP_WRAPPER_X64,
        Some(IMAGE_FILE_MACHINE_ARM64) => SETUP_WRAPPER_ARM64,
        other => {
            log(format!(
                "WARNING: Unsupported arch {:#x} -- in-place upgrade wrapper will not be added",
                other.unwrap_or(0)
            ));
            return Ok(());
        }
    };
    if rename_copy(root, "setup.exe", "setup.dll", copies)?.is_none() {
        return Ok(());
    }
    log("Renamed 'setup.exe' → 'setup.dll'".into());
    add_file(root, "setup.exe", wrapper, copies)?;
    log("Created 'setup.exe' bypass wrapper (from embedded)".into());
    Ok(())
}

/// Install the Windows 7 boot code into sector 0, keeping the disk
/// signature and partition table.
pub(crate) fn patch_mbr(sector: &mut [u8]) -> Result<(), HelperError> {
    if sector.len() < 512 {
        return Err(HelperError::Operation("MBR buffer is too short".into()));
    }
    sector[..MBR_WIN7.len()].copy_from_slice(MBR_WIN7);
    sector[0x1fe] = 0x55;
    sector[0x1ff] = 0xaa;
    Ok(())
}

fn put(area: &mut [u8], at: usize, bytes: &[u8]) {
    area[at..at + bytes.len()].copy_from_slice(bytes);
}

/// The boot code finds its partition through the BPB "hidden sectors"
/// field, which mkfs leaves at 0 when the kernel reports no geometry (loop
/// and some card-reader devices). Windows formats with 63/255 geometry.
fn set_bpb_location(sector: &mut [u8], partition_start: u32) {
    sector[0x1c..0x20].copy_from_slice(&partition_start.to_le_bytes());
    if sector[0x18..0x1c] == [0, 0, 0, 0] {
        sector[0x18..0x1a].copy_from_slice(&63u16.to_le_bytes());
        sector[0x1a..0x1c].copy_from_slice(&255u16.to_le_bytes());
    }
}

/// Install the BOOTMGR-loading partition boot record, keeping the BIOS
/// Parameter Block written by mkfs. `area` is the first
/// [`BOOT_AREA_BYTES`] of the data partition, which starts at sector
/// `partition_start`.
pub(crate) fn patch_boot_record(
    area: &mut [u8],
    filesystem: FileSystem,
    partition_start: u32,
) -> Result<(), HelperError> {
    if area.len() < BOOT_AREA_BYTES {
        return Err(HelperError::Operation(
            "boot area buffer is too short".into(),
        ));
    }
    match filesystem {
        FileSystem::Ntfs => {
            if &area[3..11] != b"NTFS    " {
                return Err(HelperError::Operation(
                    "the new volume does not have an NTFS boot sector".into(),
                ));
            }
            put(area, 0, BR_NTFS_0X0);
            put(area, 0x54, BR_NTFS_0X54);
            set_bpb_location(area, partition_start);
        }
        FileSystem::Fat32 => {
            let reserved = u16::from_le_bytes([area[0x0e], area[0x0f]]) as usize;
            let backup = u16::from_le_bytes([area[0x32], area[0x33]]) as usize;
            let mut copies = vec![0usize];
            // Keep the backup boot sector in step when it sits where mkfs puts it.
            if backup == 6 && reserved * 512 >= 0xc00 + 0x1800 + 0x200 {
                copies.push(0xc00);
            }
            for base in copies {
                if &area[base + 0x52..base + 0x5a] != b"FAT32   " {
                    return Err(HelperError::Operation(
                        "the new volume does not have a FAT32 boot sector".into(),
                    ));
                }
                put(area, base, BR_FAT32_0X0);
                put(area, base + 0x52, BR_FAT32PE_0X52);
                put(area, base + 0x3f0, BR_FAT32PE_0X3F0);
                put(area, base + 0x1800, BR_FAT32PE_0X1800);
                // Physical drive number: first hard disk.
                area[base + 0x40] = 0x80;
                set_bpb_location(&mut area[base..], partition_start);
            }
        }
        _ => {
            return Err(HelperError::Operation(
                "BIOS boot records exist for NTFS and FAT32 only".into(),
            ))
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rufus_image::isofs::fixture::{self, Node};
    use std::os::unix::fs::DirBuilderExt;

    const GIB: u64 = 1024 * MIB;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "rufus-winmedia-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .expect("temporary directory");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn layouts_match_upstream_partitioning() {
        let size = 16 * GIB + 12345;
        let gpt = plan_layout(size, 512, PartitionScheme::Gpt, true).expect("gpt layout");
        let uefi = gpt.uefi_ntfs.expect("UEFI:NTFS partition");
        assert_eq!(gpt.data.offset, MIB);
        assert_eq!(gpt.data.end(), uefi.offset);
        assert_eq!(uefi.size, MIB);
        assert_eq!(uefi.offset % MIB, 0);
        assert!(uefi.end() <= size - 33 * 512);

        let mbr = plan_layout(16 * GIB, 512, PartitionScheme::Mbr, true).expect("mbr layout");
        assert_eq!(mbr.uefi_ntfs.expect("UEFI:NTFS").end(), 16 * GIB);

        let fat = plan_layout(16 * GIB, 512, PartitionScheme::Mbr, false).expect("fat32");
        assert_eq!(fat.uefi_ntfs, None);
        assert_eq!(fat.data.end(), 16 * GIB);

        assert!(plan_layout(32 * MIB, 512, PartitionScheme::Gpt, true).is_err());
        assert!(plan_layout(3 * 1024 * GIB, 512, PartitionScheme::Mbr, true).is_err());
    }

    #[test]
    fn requirements_follow_the_target_system() {
        let uefi = requirements(FileSystem::Ntfs, PartitionScheme::Gpt, BootMode::Uefi)
            .expect("NTFS for UEFI");
        assert!(uefi.uefi_ntfs && !uefi.bios_boot);
        let fat = requirements(FileSystem::Fat32, PartitionScheme::Gpt, BootMode::Uefi)
            .expect("FAT32 for UEFI");
        assert!(!fat.uefi_ntfs);
        let dual = requirements(FileSystem::Ntfs, PartitionScheme::Mbr, BootMode::Dual)
            .expect("BIOS or UEFI");
        assert!(dual.uefi_ntfs && dual.bios_boot);
        assert!(requirements(FileSystem::Ntfs, PartitionScheme::Gpt, BootMode::Bios).is_err());
        assert!(requirements(FileSystem::ExFat, PartitionScheme::Mbr, BootMode::Dual).is_err());
        assert!(requirements(FileSystem::Ext4, PartitionScheme::Gpt, BootMode::Uefi).is_err());
    }

    #[test]
    fn fat32_rejects_files_over_four_gib_before_writing() {
        let listing = IsoListing {
            entries: vec![IsoEntry {
                path: "sources/install.wim".into(),
                size: FAT32_MAX_FILE + 1,
                is_dir: false,
                extents: Vec::new(),
            }],
            udf: true,
        };
        let error = check_fits(&listing, FileSystem::Fat32, 64 * GIB)
            .expect_err("FAT32 cannot hold the file")
            .to_string();
        assert!(error.contains("choose NTFS"), "{error}");
        check_fits(&listing, FileSystem::Ntfs, 64 * GIB).expect("NTFS holds it");
        assert!(check_fits(&listing, FileSystem::Ntfs, 4 * GIB).is_err());
    }

    #[test]
    fn copies_and_verifies_every_file_of_the_image() {
        let dir = TempDir::new("copy");
        let image_path = dir.0.join("windows.iso");
        std::fs::write(
            &image_path,
            fixture::udf(vec![
                Node::File("bootmgr", 1000),
                Node::Dir(
                    "sources",
                    vec![
                        Node::File("boot.wim", 70_000),
                        Node::File("install.wim", 200_000),
                    ],
                ),
                Node::Dir("efi", vec![Node::Dir("boot", vec![])]),
            ]),
        )
        .expect("write fixture");
        let mut iso = File::open(&image_path).expect("open fixture");
        let listing = isofs::list(&mut iso).expect("list").expect("UDF listing");
        let root = dir.0.join("volume");
        std::fs::create_dir(&root).expect("volume root");

        let cancel = CancellationToken::new();
        let mut last = 0;
        let copies = copy_listing(&mut iso, &listing, &root, &cancel, &mut |done, _| {
            last = done
        })
        .expect("copy");
        assert_eq!(last, listing.total_file_bytes());
        assert_eq!(copies.len(), 3);
        assert!(root.join("efi/boot").is_dir());
        let bootmgr = std::fs::read(root.join("bootmgr")).expect("bootmgr");
        assert!(bootmgr
            .iter()
            .enumerate()
            .all(|(i, byte)| *byte == fixture::pattern(i as u64)));
        assert_eq!(
            std::fs::metadata(root.join("sources/install.wim"))
                .expect("install.wim")
                .len(),
            200_000
        );
        verify_copies(&copies, &root, &root, &cancel, &mut |_| {}).expect("verify");

        let mut corrupt = std::fs::OpenOptions::new()
            .write(true)
            .open(root.join("sources/boot.wim"))
            .expect("open copy");
        corrupt.write_all(b"x").expect("corrupt copy");
        let error = verify_copies(&copies, &root, &root, &cancel, &mut |_| {})
            .expect_err("corruption must be detected")
            .to_string();
        assert!(error.contains("boot.wim"), "{error}");

        // A second copy never overwrites existing files.
        assert!(copy_listing(&mut iso, &listing, &root, &cancel, &mut |_, _| {}).is_err());
    }

    fn pe_stub(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0u8; 0x200];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        bytes[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    #[test]
    fn customization_adds_the_answer_file_and_upgrade_bypasses_and_stays_verifiable() {
        let dir = TempDir::new("customize");
        let image_path = dir.0.join("windows.iso");
        std::fs::write(
            &image_path,
            fixture::udf(vec![
                Node::Bytes("setup.exe", pe_stub(IMAGE_FILE_MACHINE_AMD64)),
                Node::Dir(
                    "sources",
                    vec![
                        Node::File("appraiserres.dll", 3000),
                        Node::File("install.wim", 5000),
                    ],
                ),
            ]),
        )
        .expect("write fixture");
        let mut iso = File::open(&image_path).expect("open fixture");
        let listing = isofs::list(&mut iso).expect("list").expect("UDF listing");
        let root = dir.0.join("volume");
        std::fs::create_dir(&root).expect("volume root");
        let cancel = CancellationToken::new();
        let mut copies =
            copy_listing(&mut iso, &listing, &root, &cancel, &mut |_, _| {}).expect("copy");

        let options = rufus_helper_protocol::WindowsCustomization {
            bypass_requirements: true,
            no_online_account: true,
            ..Default::default()
        };
        let answer = crate::unattend::answer_file(
            &options,
            rufus_image::windows::WindowsArch::Amd64,
            "en-US",
        );
        let mut log = Vec::new();
        apply_customization(&root, &answer, true, 26100, &mut copies, &mut |line| {
            log.push(line)
        })
        .expect("customize");

        assert_eq!(
            std::fs::read_to_string(root.join("autounattend.xml")).expect("answer file"),
            answer.xml
        );
        assert_eq!(
            std::fs::metadata(root.join("sources/appraiserres.dll"))
                .expect("placeholder")
                .len(),
            0
        );
        assert_eq!(
            std::fs::metadata(root.join("sources/appraiserres.bak"))
                .expect("backup")
                .len(),
            3000
        );
        assert_eq!(
            std::fs::read(root.join("setup.dll")).expect("original setup"),
            pe_stub(IMAGE_FILE_MACHINE_AMD64)
        );
        assert_eq!(
            std::fs::read(root.join("setup.exe")).expect("wrapper"),
            SETUP_WRAPPER_X64
        );
        assert!(log.iter().any(|line| line.contains("bypass wrapper")));
        verify_copies(&copies, &root, &root, &cancel, &mut |_| {}).expect("verify");
        assert_eq!(
            copied_bytes(&copies),
            listing.total_file_bytes() + SETUP_WRAPPER_X64.len() as u64 + answer.xml.len() as u64
        );
    }

    #[test]
    fn older_builds_and_unknown_setup_architectures_keep_the_original_setup() {
        let dir = TempDir::new("no-wrapper");
        let root = dir.0.clone();
        let setup = pe_stub(0x014c);
        std::fs::write(root.join("setup.exe"), &setup).expect("setup");
        let mut copies = vec![CopiedFile {
            path: root.join("setup.exe"),
            size: setup.len() as u64,
            sha256: Sha256::digest(&setup).into(),
        }];
        let answer = crate::unattend::answer_file(
            &rufus_helper_protocol::WindowsCustomization {
                no_data_collection: true,
                ..Default::default()
            },
            rufus_image::windows::WindowsArch::X86,
            "en-US",
        );
        let mut log = Vec::new();
        apply_customization(&root, &answer, true, 26100, &mut copies, &mut |line| {
            log.push(line)
        })
        .expect("customize");
        assert_eq!(std::fs::read(root.join("setup.exe")).expect("setup"), setup);
        assert!(log
            .iter()
            .any(|line| line.contains("Unsupported arch 0x14c")));
        assert!(root.join("sources/$OEM$/$$/Panther/unattend.xml").is_file());
        let cancel = CancellationToken::new();
        verify_copies(&copies, &root, &root, &cancel, &mut |_| {}).expect("verify");
    }

    #[test]
    fn setup_wrapper_matches_the_pinned_upstream_files() {
        let digest = |bytes: &[u8]| crate::hex_lower(&Sha256::digest(bytes));
        assert_eq!(
            digest(SETUP_WRAPPER_X64),
            "11df838dc69378187e1e1aaf32d34384157642d07096c6e49c1d0e7375634544"
        );
        assert_eq!(
            digest(SETUP_WRAPPER_ARM64),
            "14bd07f559513890a0f6565df3927392b4fe6b8e6fc3f5e832e9d69c8b7bb7eb"
        );
        assert_eq!(
            pe_machine(SETUP_WRAPPER_X64),
            Some(IMAGE_FILE_MACHINE_AMD64)
        );
        assert_eq!(
            pe_machine(SETUP_WRAPPER_ARM64),
            Some(IMAGE_FILE_MACHINE_ARM64)
        );
    }

    #[test]
    fn unsafe_destinations_are_refused() {
        let root = Path::new("/media/target");
        assert!(destination(root, "../escape").is_err());
        assert!(destination(root, "/etc/passwd").is_err());
        assert!(destination(root, "").is_err());
        assert_eq!(
            destination(root, "sources/boot.wim").expect("normal path"),
            root.join("sources/boot.wim")
        );
    }

    #[test]
    fn uefi_ntfs_payload_matches_the_pinned_upstream_files() {
        let mut hasher = Sha256::new();
        for (path, bytes) in UEFI_NTFS_FILES {
            hasher.update(path.as_bytes());
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
        let total: usize = UEFI_NTFS_FILES.iter().map(|(_, bytes)| bytes.len()).sum();
        // Must fit the 1 MiB partition with FAT12 metadata.
        assert!(total < 1000 * 1024, "{total}");
        let digest: [u8; 32] = hasher.finalize().into();
        assert_eq!(
            crate::hex_lower(&digest),
            "110ae917ea8b33c6f0f13150d3eb0f8d1a3d987c20a21b62f01dd72a0f2476a7"
        );
        let dir = TempDir::new("uefi");
        write_uefi_ntfs_files(&dir.0).expect("write payload");
        assert_eq!(
            std::fs::read(dir.0.join("EFI/Boot/bootx64.efi")).expect("bootx64"),
            UEFI_NTFS_FILES
                .iter()
                .find(|(path, _)| *path == "EFI/Boot/bootx64.efi")
                .expect("bootx64 entry")
                .1
        );
    }

    #[test]
    fn mbr_boot_code_keeps_signature_and_partition_table() {
        let mut sector = [0u8; 512];
        sector[0x1b8..0x1bc].copy_from_slice(&[1, 2, 3, 4]);
        sector[0x1be] = 0x80;
        sector[0x1c2] = 0x07;
        patch_mbr(&mut sector).expect("patch MBR");
        assert_eq!(&sector[..MBR_WIN7.len()], MBR_WIN7);
        assert_eq!(&sector[0x1b8..0x1bc], &[1, 2, 3, 4]);
        assert_eq!((sector[0x1be], sector[0x1c2]), (0x80, 0x07));
        assert_eq!(&sector[0x1fe..], &[0x55, 0xaa]);
    }

    #[test]
    fn boot_records_keep_the_bios_parameter_block() {
        let mut ntfs = vec![0u8; BOOT_AREA_BYTES];
        ntfs[3..11].copy_from_slice(b"NTFS    ");
        ntfs[0x0b..0x54].fill(0xbb);
        ntfs[0x18..0x20].fill(0);
        patch_boot_record(&mut ntfs, FileSystem::Ntfs, 2048).expect("NTFS record");
        assert!(ntfs[0x0b..0x18].iter().all(|byte| *byte == 0xbb));
        assert!(ntfs[0x20..0x54].iter().all(|byte| *byte == 0xbb));
        assert_eq!(&ntfs[0x18..0x20], &[63, 0, 255, 0, 0, 8, 0, 0]);
        assert_eq!(&ntfs[0x54..0x54 + BR_NTFS_0X54.len()], BR_NTFS_0X54);
        assert_eq!(&ntfs[0x1fe..0x200], &[0x55, 0xaa]);

        let mut fat = vec![0u8; BOOT_AREA_BYTES];
        fat[0x0e..0x10].copy_from_slice(&32u16.to_le_bytes());
        fat[0x32..0x34].copy_from_slice(&6u16.to_le_bytes());
        fat[0x47..0x52].copy_from_slice(b"CCCOMA_X64F");
        fat[0x52..0x5a].copy_from_slice(b"FAT32   ");
        fat[0xc52..0xc5a].copy_from_slice(b"FAT32   ");
        fat[0x18..0x1c].copy_from_slice(&[32, 0, 64, 0]);
        patch_boot_record(&mut fat, FileSystem::Fat32, 2048).expect("FAT32 record");
        assert_eq!(&fat[0x47..0x52], b"CCCOMA_X64F");
        assert_eq!(&fat[0x18..0x1c], &[32, 0, 64, 0], "real geometry is kept");
        for base in [0, 0xc00] {
            assert_eq!(&fat[base + 3..base + 11], b"MSWIN4.1");
            assert_eq!(fat[base + 0x40], 0x80);
            assert_eq!(&fat[base + 0x1c..base + 0x20], &2048u32.to_le_bytes());
            assert_eq!(&fat[base + 0x1800..base + 0x1a00], BR_FAT32PE_0X1800);
        }

        let mut blank = vec![0u8; BOOT_AREA_BYTES];
        assert!(patch_boot_record(&mut blank, FileSystem::Ntfs, 2048).is_err());
        assert!(patch_boot_record(&mut blank, FileSystem::Fat32, 2048).is_err());
    }
}
