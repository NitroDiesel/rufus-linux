//! Read-only image analysis: kind detection, size bounds, checksums, and capability hints.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use md5::{Digest as _, Md5};
use rufus_core::plan::{BootMode, FileSystem, ImageSourceKind, WriteMode};
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use thiserror::Error;

pub mod isofs;

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
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    analyze_file(path, &mut file)
}

/// Analyze the opened descriptor; the path is used only for names and hints.
pub fn analyze_file(path: &Path, file: &mut File) -> Result<ImageReport, ImageError> {
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(ImageError::NotAFile(path.to_owned()));
    }
    let size_bytes = meta.len();
    file.seek(SeekFrom::Start(0))?;
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

    if header.starts_with(b"conectix") || ext == "vhd" || detect_vhd_footer(file, size_bytes)? {
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
    let is_iso = detect_iso(file)?;
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

        if is_iso {
            report.label_hint = read_iso_label(file)?;
            if let Some(listing) = isofs::list(file)? {
                apply_listing(&mut report, &listing);
            } else {
                report
                    .notes
                    .push("The ISO file system could not be listed.".into());
            }
        }
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

fn detect_vhd_footer(file: &mut File, size_bytes: u64) -> Result<bool, ImageError> {
    // Fixed VHDs have no leading header; older writers used a 511-byte footer.
    for footer_size in [512, 511] {
        if let Some(offset) = size_bytes.checked_sub(footer_size) {
            file.seek(SeekFrom::Start(offset))?;
            let mut cookie = [0; 8];
            file.read_exact(&mut cookie)?;
            if &cookie == b"conectix" {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn detect_iso(file: &mut File) -> Result<bool, ImageError> {
    // Primary Volume Descriptor at sector 16 (2048-byte sectors) + 1 byte type + "CD001"
    file.seek(SeekFrom::Start(16 * 2048 + 1))?;
    let mut magic = [0u8; 5];
    let n = file.read(&mut magic)?;
    Ok(n == 5 && &magic == b"CD001")
}

/// Classify an ISO from its contents, as upstream Rufus's image scan does.
fn apply_listing(report: &mut ImageReport, listing: &isofs::IsoListing) {
    let any = |paths: &[&str]| paths.iter().any(|path| listing.has_file(path));
    let has_dir = |path: &str| listing.find(path).is_some_and(|entry| entry.is_dir);
    report.has_efi = any(&[
        "efi/boot/bootx64.efi",
        "efi/boot/bootia32.efi",
        "efi/boot/bootaa64.efi",
    ]);
    report.has_bios = report.has_bios
        || any(&[
            "bootmgr",
            "isolinux/isolinux.bin",
            "boot/grub/i386-pc/eltorito.img",
        ]);
    report.windows_installer = any(&[
        "sources/install.wim",
        "sources/install.esd",
        "sources/install.swm",
    ]) && any(&["bootmgr", "bootmgr.efi"]);
    report.largest_file_bytes = listing.largest_file().map(|entry| entry.size);
    if report.windows_installer {
        report.preferred_filesystem = Some(FileSystem::Ntfs);
        report.preferred_boot_mode = BootMode::Uefi;
        report.notes.push("Windows installer media.".into());
    } else if ["casper", "live", "isolinux", "boot/grub", "arch"]
        .iter()
        .any(|dir| has_dir(dir))
    {
        report.linux_live = true;
        report.persistence_supported = has_dir("casper") || has_dir("live");
    }
}

const ISO_SECTOR: u64 = 2048;
/// Upper bound on Volume Descriptor Sequence sectors scanned for a UDF label.
const UDF_MAX_VDS_SECTORS: u32 = 64;

/// The volume name upstream Rufus proposes as the USB label: the UDF logical
/// volume identifier when present (Windows media), else the ISO 9660 one.
fn read_iso_label(file: &mut File) -> Result<Option<String>, ImageError> {
    if let Some(label) = read_udf_label(file)? {
        return Ok(Some(label));
    }
    let mut pvd = [0u8; 72];
    if !read_at(file, 16 * ISO_SECTOR, &mut pvd)? || pvd[0] != 1 || &pvd[1..6] != b"CD001" {
        return Ok(None);
    }
    Ok(clean_label(pvd[40..72].iter().map(|&b| char::from(b))))
}

fn read_udf_label(file: &mut File) -> Result<Option<String>, ImageError> {
    // Anchor Volume Descriptor Pointer (tag 2) at sector 256.
    let mut anchor = [0u8; 24];
    if !read_at(file, 256 * ISO_SECTOR, &mut anchor)?
        || u16::from_le_bytes([anchor[0], anchor[1]]) != 2
        || u32::from_le_bytes([anchor[12], anchor[13], anchor[14], anchor[15]]) != 256
    {
        return Ok(None);
    }
    let length = u32::from_le_bytes([anchor[16], anchor[17], anchor[18], anchor[19]]);
    let location = u32::from_le_bytes([anchor[20], anchor[21], anchor[22], anchor[23]]);
    let sectors = (length / ISO_SECTOR as u32).min(UDF_MAX_VDS_SECTORS);
    let mut descriptor = [0u8; 212];
    for sector in 0..sectors {
        let offset = (u64::from(location) + u64::from(sector)) * ISO_SECTOR;
        if !read_at(file, offset, &mut descriptor)? {
            return Ok(None);
        }
        match u16::from_le_bytes([descriptor[0], descriptor[1]]) {
            // Logical Volume Descriptor: identifier is a 128-byte dstring at 84.
            6 => return Ok(decode_dstring(&descriptor[84..212])),
            // Terminating Descriptor.
            8 => return Ok(None),
            _ => {}
        }
    }
    Ok(None)
}

/// Decode an OSTA CS0 dstring: compression ID 8 (Latin-1) or 16 (UTF-16BE),
/// with the used length in the final byte.
fn decode_dstring(field: &[u8]) -> Option<String> {
    let used = usize::from(*field.last()?);
    if used < 2 || used >= field.len() {
        return None;
    }
    let bytes = &field[1..used];
    match field[0] {
        8 => clean_label(bytes.iter().map(|&b| char::from(b))),
        16 => clean_label(
            char::decode_utf16(
                bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_be_bytes([pair[0], pair[1]])),
            )
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)),
        ),
        _ => None,
    }
}

fn clean_label(chars: impl Iterator<Item = char>) -> Option<String> {
    let label: String = chars
        .take_while(|&c| c != '\0')
        .filter(|c| !c.is_control())
        .collect();
    let label = label.trim_end();
    (!label.is_empty()).then(|| label.to_owned())
}

/// Read exactly `buf.len()` bytes at `offset`; false when the file is shorter.
fn read_at(file: &mut File, offset: u64, buf: &mut [u8]) -> Result<bool, ImageError> {
    file.seek(SeekFrom::Start(offset))?;
    match file.read_exact(buf) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e.into()),
    }
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

    fn iso_fixture(volume_id: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; 18 * 2048];
        let pvd = 16 * 2048;
        bytes[pvd] = 1;
        bytes[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        bytes[pvd + 40..pvd + 72].fill(b' ');
        bytes[pvd + 40..pvd + 40 + volume_id.len()].copy_from_slice(volume_id);
        bytes
    }

    fn add_udf_label(bytes: &mut Vec<u8>, compression: u8, encoded: &[u8]) {
        bytes.resize(260 * 2048, 0);
        let anchor = 256 * 2048;
        bytes[anchor..anchor + 2].copy_from_slice(&2u16.to_le_bytes());
        bytes[anchor + 12..anchor + 16].copy_from_slice(&256u32.to_le_bytes());
        bytes[anchor + 16..anchor + 20].copy_from_slice(&(3 * 2048u32).to_le_bytes());
        bytes[anchor + 20..anchor + 24].copy_from_slice(&257u32.to_le_bytes());
        // A Primary Volume Descriptor precedes the Logical Volume Descriptor.
        bytes[257 * 2048..257 * 2048 + 2].copy_from_slice(&1u16.to_le_bytes());
        let lvd = 258 * 2048;
        bytes[lvd..lvd + 2].copy_from_slice(&6u16.to_le_bytes());
        bytes[lvd + 84] = compression;
        bytes[lvd + 85..lvd + 85 + encoded.len()].copy_from_slice(encoded);
        bytes[lvd + 84 + 127] = u8::try_from(encoded.len() + 1).expect("short dstring");
    }

    fn analyze_bytes(name: &str, bytes: &[u8]) -> ImageReport {
        let path = temp_path(name).with_extension("iso");
        std::fs::write(&path, bytes).expect("write ISO fixture");
        let report = analyze(&path).expect("analyze ISO fixture");
        std::fs::remove_file(&path).expect("remove ISO fixture");
        report
    }

    #[test]
    fn windows_media_is_recognized_from_contents_not_its_name() {
        use crate::isofs::fixture::{udf, Node};
        let mut bytes = udf(vec![
            Node::File("bootmgr", 473_364),
            Node::Dir(
                "efi",
                vec![Node::Dir(
                    "boot",
                    vec![Node::File("bootx64.efi", 3_087_400)],
                )],
            ),
            Node::Dir("sources", vec![Node::File("install.wim", 8_155_984_950)]),
        ]);
        let pvd = 16 * 2048;
        bytes[pvd] = 1;
        bytes[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        let report = analyze_bytes("plain-name", &bytes);
        assert_eq!(report.kind, ImageSourceKind::Iso);
        assert!(report.windows_installer);
        assert!(report.has_efi);
        assert!(report.has_bios);
        assert_eq!(report.preferred_filesystem, Some(FileSystem::Ntfs));
        assert_eq!(report.largest_file_bytes, Some(8_155_984_950));

        // A file named like Windows is not treated as Windows media.
        let report = analyze_bytes("windows-11", &iso_fixture(b"NOT_WINDOWS"));
        assert!(!report.windows_installer);
        assert!(!report.has_efi);
    }

    #[test]
    fn iso9660_volume_id_becomes_trimmed_label_hint() {
        let report = analyze_bytes("pvd-label", &iso_fixture(b"ARCH_202610"));
        assert_eq!(report.label_hint.as_deref(), Some("ARCH_202610"));
    }

    #[test]
    fn udf_logical_volume_id_is_preferred_like_windows_media() {
        let mut bytes = iso_fixture(b"ISO9660_NAME");
        let utf16: Vec<u8> = "CCCOMA_X64FRE_EN-US_DV9"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        add_udf_label(&mut bytes, 16, &utf16);
        let report = analyze_bytes("udf16-label", &bytes);
        assert_eq!(
            report.label_hint.as_deref(),
            Some("CCCOMA_X64FRE_EN-US_DV9")
        );

        let mut bytes = iso_fixture(b"ISO9660_NAME");
        add_udf_label(&mut bytes, 8, b"UDF8_NAME  ");
        let report = analyze_bytes("udf8-label", &bytes);
        assert_eq!(report.label_hint.as_deref(), Some("UDF8_NAME"));
    }

    #[test]
    fn blank_or_malformed_volume_ids_give_no_label_hint() {
        let report = analyze_bytes("blank-label", &iso_fixture(b""));
        assert_eq!(report.label_hint, None);

        // A UDF anchor pointing past the end of the file falls back to ISO 9660.
        let mut bytes = iso_fixture(b"FALLBACK");
        add_udf_label(&mut bytes, 16, &[0, b'X']);
        bytes.truncate(257 * 2048);
        let report = analyze_bytes("short-udf", &bytes);
        assert_eq!(report.label_hint.as_deref(), Some("FALLBACK"));

        // Unknown dstring compression is ignored rather than guessed.
        let mut bytes = iso_fixture(b"FALLBACK");
        add_udf_label(&mut bytes, 254, b"GARBAGE");
        let report = analyze_bytes("bad-udf", &bytes);
        assert_eq!(report.label_hint.as_deref(), Some("FALLBACK"));
    }

    #[test]
    fn bound_analysis_uses_descriptor_metadata_after_path_replacement() {
        let path = temp_path("bound.img");
        let moved = temp_path("moved.img");
        std::fs::write(&path, vec![0; 4096]).expect("source fixture");
        let mut source = File::open(&path).expect("bind source");
        std::fs::rename(&path, &moved).expect("rename source");
        std::fs::write(&path, b"replacement").expect("replacement source");
        let report = analyze_file(&path, &mut source).expect("analyze descriptor");
        assert_eq!(report.size_bytes, 4096);
        assert_eq!(report.kind, ImageSourceKind::Raw);
        std::fs::remove_file(path).expect("remove replacement");
        std::fs::remove_file(moved).expect("remove original fixture");
    }

    #[test]
    fn detects_fixed_vhd_footer_without_vhd_extension() {
        for footer_size in [512, 511] {
            let path = temp_path("fixed-vhd").with_extension("img");
            let mut bytes = vec![0u8; 4096 + footer_size];
            bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
            bytes[4096..4104].copy_from_slice(b"conectix");
            std::fs::write(&path, bytes).expect("write VHD fixture");
            let report = analyze(&path).expect("analyze renamed VHD");
            std::fs::remove_file(&path).expect("remove VHD fixture");
            assert_eq!(
                report.kind,
                ImageSourceKind::Vhd,
                "footer size {footer_size}"
            );
        }
    }

    #[test]
    fn raw_images_without_vhd_footer_remain_raw() {
        for size in [0, 8, 510, 511, 512, 4096] {
            let path = temp_path("raw").with_extension("img");
            let mut bytes = vec![0u8; size];
            if size >= 1024 {
                bytes[512..520].copy_from_slice(b"conectix");
            }
            std::fs::write(&path, bytes).expect("write raw fixture");
            let report = analyze(&path).expect("analyze raw fixture");
            std::fs::remove_file(&path).expect("remove raw fixture");
            assert_eq!(report.kind, ImageSourceKind::Raw, "image size {size}");
        }
    }

    #[test]
    fn named_pipe_is_rejected_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let path = temp_path("fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("fixture path");
        // SAFETY: name is a live NUL-terminated pathname.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let result = analyze(&path);
        std::fs::remove_file(&path).expect("remove FIFO fixture");
        assert!(matches!(result, Err(ImageError::NotAFile(_))));
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
