//! Read-only image analysis: kind detection, size bounds, checksums, and capability hints.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use md5::{Digest as _, Md5};
use rufus_core::plan::{BootMode, FileSystem, ImageSourceKind, WriteMode};
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImageError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("unsupported or unreadable image: {0}")]
    Unsupported(String),
    #[error("path is not a regular file: {0}")]
    NotAFile(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checksums {
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub sha512: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageReport {
    pub path: PathBuf,
    pub kind: ImageSourceKind,
    pub size_bytes: u64,
    pub decompressed_size_bytes: Option<u64>,
    pub label_hint: Option<String>,
    pub preferred_filesystem: Option<FileSystem>,
    pub preferred_boot_mode: BootMode,
    pub preferred_write_mode: WriteMode,
    pub isohybrid: bool,
    pub has_efi: bool,
    pub has_bios: bool,
    pub windows_installer: bool,
    pub linux_live: bool,
    pub persistence_supported: bool,
    pub largest_file_bytes: Option<u64>,
    pub notes: Vec<String>,
}

impl ImageReport {
    pub fn display_kind(&self) -> &'static str {
        match self.kind {
            ImageSourceKind::None => "None",
            ImageSourceKind::Iso => "ISO",
            ImageSourceKind::IsoHybrid => "ISOHybrid",
            ImageSourceKind::Raw => "Disk image",
            ImageSourceKind::CompressedRaw => "Compressed disk image",
            ImageSourceKind::Vhd => "VHD",
            ImageSourceKind::Vhdx => "VHDX",
            ImageSourceKind::Wim => "WIM",
            ImageSourceKind::Esd => "ESD",
            ImageSourceKind::Ffu => "FFU",
        }
    }
}

/// Probe an image file without extracting contents.
pub fn analyze(path: &Path) -> Result<ImageReport, ImageError> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(ImageError::NotAFile(path.to_owned()));
    }
    let size_bytes = meta.len();
    let mut file = File::open(path)?;
    let mut header = [0u8; 512];
    let n = file.read(&mut header)?;
    let header = &header[..n];

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let mut report = ImageReport {
        path: path.to_owned(),
        kind: ImageSourceKind::Raw,
        size_bytes,
        decompressed_size_bytes: None,
        label_hint: None,
        preferred_filesystem: Some(FileSystem::Fat32),
        preferred_boot_mode: BootMode::Dual,
        preferred_write_mode: WriteMode::DdImage,
        isohybrid: false,
        has_efi: false,
        has_bios: false,
        windows_installer: false,
        linux_live: false,
        persistence_supported: false,
        largest_file_bytes: None,
        notes: Vec::new(),
    };

    // Compression wrappers by magic / extension.
    if header.starts_with(&[0x1f, 0x8b])
        || header.starts_with(b"BZh")
        || header.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00])
        || header.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
        || matches!(ext.as_str(), "gz" | "bz2" | "xz" | "zst" | "zip" | "z")
    {
        report.kind = ImageSourceKind::CompressedRaw;
        report.preferred_write_mode = WriteMode::DdImage;
        report
            .notes
            .push("Compressed images are written in DD mode after streaming decompression.".into());
        return Ok(report);
    }

    // VHD: "conectix" at offset 0 for fixed/dynamic footer-less connectix header at start of dynamic
    if header.starts_with(b"conectix") || ext == "vhd" {
        report.kind = ImageSourceKind::Vhd;
        report.notes.push(
            "VHD input is recognized but conversion is not available in this release.".into(),
        );
        return Ok(report);
    }
    // VHDX
    if header.starts_with(b"vhdxfile") || ext == "vhdx" {
        report.kind = ImageSourceKind::Vhdx;
        report.notes.push(
            "VHDX input is recognized but conversion is not available in this release.".into(),
        );
        return Ok(report);
    }

    // WIM / ESD
    if header.starts_with(b"MSWIM") || matches!(ext.as_str(), "wim" | "esd") {
        report.kind = if ext == "esd" {
            ImageSourceKind::Esd
        } else {
            ImageSourceKind::Wim
        };
        report.windows_installer = true;
        report.preferred_filesystem = Some(FileSystem::Ntfs);
        report.preferred_write_mode = WriteMode::WindowsToGo;
        report.notes.push(
            "WIM/ESD input is recognized but Windows deployment is not available in this release."
                .into(),
        );
        return Ok(report);
    }

    // FFU — identify only; creation/apply unavailable on Linux.
    if ext == "ffu" {
        report.kind = ImageSourceKind::Ffu;
        report
            .notes
            .push("FFU apply is unavailable on Linux; do not treat raw images as FFU.".into());
        return Ok(report);
    }

    // ISO 9660: "CD001" at offset 0x8001 (primary volume descriptor)
    let is_iso = detect_iso(&mut file)?;
    if is_iso || ext == "iso" {
        report.kind = ImageSourceKind::Iso;
        report.preferred_write_mode = WriteMode::IsoFileCopy;

        // ISOHybrid: MBR signature at 510-511
        file.seek(SeekFrom::Start(510))?;
        let mut mbr_sig = [0u8; 2];
        if file.read(&mut mbr_sig)? == 2 && mbr_sig == [0x55, 0xaa] {
            report.isohybrid = true;
            report.kind = ImageSourceKind::IsoHybrid;
            report
                .notes
                .push("ISOHybrid image: raw disk-image mode is available.".into());
        }

        // Lightweight content heuristics from the path name (full ISO walk is optional).
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.contains("win") || name.contains("windows") {
            report.windows_installer = true;
            report.preferred_filesystem = Some(FileSystem::Ntfs);
            report.has_efi = true;
            report.notes.push(
                "Windows installer detected by name; confirm with full analysis after open.".into(),
            );
        }
        if name.contains("ubuntu")
            || name.contains("fedora")
            || name.contains("arch")
            || name.contains("debian")
            || name.contains("mint")
            || name.contains("manjaro")
            || name.contains("pop")
            || name.contains("kali")
        {
            report.linux_live = true;
            report.persistence_supported = true;
            report.has_efi = true;
            report.has_bios = true;
            report.preferred_boot_mode = BootMode::Dual;
            report
                .notes
                .push("Linux live image heuristics enabled persistence option.".into());
        }

        // Prefer dual boot for generic ISOs.
        report.has_efi = true;
        report.has_bios = report.has_bios || report.isohybrid;
        return Ok(report);
    }

    // Raw disk image fallback.
    if header.len() >= 512 && header[510] == 0x55 && header[511] == 0xaa {
        report.has_bios = true;
        report
            .notes
            .push("MBR signature found; treating as raw disk image.".into());
    } else {
        report
            .notes
            .push("Unknown image; raw DD write will be offered.".into());
    }
    report.kind = ImageSourceKind::Raw;
    report.preferred_write_mode = WriteMode::DdImage;
    Ok(report)
}

fn detect_iso(file: &mut File) -> Result<bool, ImageError> {
    // Primary Volume Descriptor at sector 16 (2048-byte sectors) + 1 byte type + "CD001"
    file.seek(SeekFrom::Start(16 * 2048 + 1))?;
    let mut magic = [0u8; 5];
    let n = file.read(&mut magic)?;
    Ok(n == 5 && &magic == b"CD001")
}

/// Compute selected checksums of a file. Legacy MD5/SHA-1 are for user comparison only.
pub fn compute_checksums(
    path: &Path,
    want_md5: bool,
    want_sha1: bool,
    want_sha256: bool,
    want_sha512: bool,
) -> Result<Checksums, ImageError> {
    let mut file = File::open(path)?;
    let mut md5 = want_md5.then(Md5::new);
    let mut sha1 = want_sha1.then(Sha1::new);
    let mut sha256 = want_sha256.then(Sha256::new);
    let mut sha512 = want_sha512.then(Sha512::new);
    let mut buf = [0u8; 1024 * 256];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        if let Some(h) = md5.as_mut() {
            h.update(chunk);
        }
        if let Some(h) = sha1.as_mut() {
            h.update(chunk);
        }
        if let Some(h) = sha256.as_mut() {
            h.update(chunk);
        }
        if let Some(h) = sha512.as_mut() {
            h.update(chunk);
        }
    }
    Ok(Checksums {
        md5: md5.map(|h| hex_lower(&h.finalize())),
        sha1: sha1.map(|h| hex_lower(&h.finalize())),
        sha256: sha256.map(|h| hex_lower(&h.finalize())),
        sha512: sha512.map(|h| hex_lower(&h.finalize())),
    })
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Human-readable size using binary units (GiB-style) for UI labels.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rufus-image-{}-{}-{}",
            name,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ))
    }

    #[test]
    fn detects_iso_magic() {
        let path = temp_path("iso");
        let mut f = File::create(&path).expect("create ISO fixture");
        f.write_all(&vec![0u8; 16 * 2048])
            .expect("write ISO prefix");
        f.write_all(&[0x01]).expect("write descriptor type");
        f.write_all(b"CD001").expect("write ISO magic");
        f.write_all(&vec![0u8; 2048]).expect("write ISO descriptor");
        drop(f);
        let report = analyze(&path).expect("analyze ISO fixture");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(
            report.kind,
            ImageSourceKind::Iso | ImageSourceKind::IsoHybrid
        ));
    }

    #[test]
    fn checksum_sha256_known() {
        let path = temp_path("hash");
        std::fs::write(&path, b"rufus-linux").expect("write hash fixture");
        let sums =
            compute_checksums(&path, false, false, true, false).expect("compute fixture checksum");
        let _ = std::fs::remove_file(&path);
        // printf 'rufus-linux' | sha256sum
        assert_eq!(
            sums.sha256.as_deref(),
            Some("d0c3fc3d357a2b942cee0f7bb30469e9a5b8d9f925519864fd367a63fa758aad")
        );
    }

    #[test]
    fn format_size_units() {
        assert_eq!(format_size(512), "512 B");
        assert!(format_size(5 * 1024 * 1024).contains("MB"));
    }
}
