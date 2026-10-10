//! Bounded, read-only directory listing of ISO images (UDF and ISO 9660).
//!
//! Upstream Rufus scans the image to decide how to build media (Windows
//! installer, EFI loader, files over 4 GiB) and then extracts it in-process.
//! The listing reads only descriptors and directories and records where each
//! file's bytes live; [`copy_file`] streams them. Every count, depth, and
//! directory size is bounded, and malformed structures end the scan with
//! `None` rather than a guess.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

const SECTOR: u64 = 2048;
const MAX_ENTRIES: usize = 200_000;
const MAX_DEPTH: usize = 48;
const MAX_DIRECTORY_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IsoEntry {
    /// Path from the root with `/` separators and no leading slash.
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    /// Where the file's bytes are stored, in order. Empty for directories.
    pub extents: Vec<Extent>,
}

/// A run of file data: `offset` into the image, or `None` for an
/// unrecorded (all-zero) run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub offset: Option<u64>,
    pub len: u64,
}

/// Stream exactly `entry.size` bytes of a listed file to `sink`.
pub fn copy_file(
    file: &mut File,
    entry: &IsoEntry,
    buffer: &mut [u8],
    mut sink: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<()> {
    if entry.is_dir || buffer.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a file"));
    }
    let mut remaining = entry.size;
    for extent in &entry.extents {
        let mut take = extent.len.min(remaining);
        let mut offset = extent.offset;
        if let Some(start) = offset {
            file.seek(SeekFrom::Start(start))?;
        }
        while take > 0 {
            let chunk = take.min(buffer.len() as u64) as usize;
            let buf = &mut buffer[..chunk];
            match offset {
                Some(ref mut at) => {
                    file.read_exact(buf)?;
                    *at += chunk as u64;
                }
                None => buf.fill(0),
            }
            sink(buf)?;
            take -= chunk as u64;
            remaining -= chunk as u64;
        }
        if remaining == 0 {
            break;
        }
    }
    if remaining != 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("{} is shorter than its recorded size", entry.path),
        ));
    }
    Ok(())
}

/// Read `len` bytes at `offset` within a listed file, or fewer at its end.
pub fn read_range(
    file: &mut File,
    entry: &IsoEntry,
    offset: u64,
    len: usize,
) -> io::Result<Vec<u8>> {
    if entry.is_dir {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a file"));
    }
    let end = entry.size.min(offset.saturating_add(len as u64));
    let mut out = Vec::with_capacity(end.saturating_sub(offset) as usize);
    let mut extent_start = 0u64;
    for extent in &entry.extents {
        let extent_end = extent_start + extent.len;
        let pos = offset + out.len() as u64;
        if pos >= end {
            break;
        }
        if pos < extent_end {
            let at = out.len();
            out.resize(at + (extent_end.min(end) - pos) as usize, 0);
            if let Some(base) = extent.offset {
                file.seek(SeekFrom::Start(base + (pos - extent_start)))?;
                file.read_exact(&mut out[at..])?;
            }
        }
        extent_start = extent_end;
    }
    Ok(out)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IsoListing {
    pub entries: Vec<IsoEntry>,
    /// True when the listing came from UDF rather than ISO 9660.
    pub udf: bool,
}

impl IsoListing {
    /// Case-insensitive lookup, as ISO 9660 and FAT media are.
    pub fn find(&self, path: &str) -> Option<&IsoEntry> {
        self.entries
            .iter()
            .find(|entry| entry.path.eq_ignore_ascii_case(path))
    }

    pub fn has_file(&self, path: &str) -> bool {
        self.find(path).is_some_and(|entry| !entry.is_dir)
    }

    pub fn total_file_bytes(&self) -> u64 {
        self.entries
            .iter()
            .filter(|entry| !entry.is_dir)
            .fold(0u64, |sum, entry| sum.saturating_add(entry.size))
    }

    pub fn largest_file(&self) -> Option<&IsoEntry> {
        self.entries
            .iter()
            .filter(|entry| !entry.is_dir)
            .max_by_key(|entry| entry.size)
    }
}

/// List the image, preferring UDF (Windows media keeps its files there and
/// only a README in ISO 9660) and falling back to ISO 9660 with Joliet names.
pub fn list(file: &mut File) -> io::Result<Option<IsoListing>> {
    if let Some(udf) = Udf::open(file)? {
        if let Some(listing) = udf.list(file)? {
            return Ok(Some(listing));
        }
    }
    Iso9660::list(file)
}

fn read_at(file: &mut File, offset: u64, buf: &mut [u8]) -> io::Result<bool> {
    file.seek(SeekFrom::Start(offset))?;
    match file.read_exact(buf) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e),
    }
}

fn read_vec(file: &mut File, offset: u64, len: u64) -> io::Result<Option<Vec<u8>>> {
    if len > MAX_DIRECTORY_BYTES {
        return Ok(None);
    }
    let mut buf = vec![0u8; len as usize];
    Ok(read_at(file, offset, &mut buf)?.then_some(buf))
}

fn u16le(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64le(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// A name that is safe to join into a path: no separators, `.` or `..`.
fn safe_name(name: String) -> Option<String> {
    let invalid = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\0')
        || name.chars().any(char::is_control);
    (!invalid).then_some(name)
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

struct Listing {
    entries: Vec<IsoEntry>,
}

impl Listing {
    fn push(&mut self, entry: IsoEntry) -> Option<()> {
        (self.entries.len() < MAX_ENTRIES).then(|| self.entries.push(entry))
    }
}

// ---------------------------------------------------------------- UDF

struct Udf {
    partition_start: u32,
    root: u32,
}

struct UdfEntry {
    is_dir: bool,
    size: u64,
    extents: Vec<Extent>,
}

impl Udf {
    fn open(file: &mut File) -> io::Result<Option<Self>> {
        let mut anchor = [0u8; 24];
        if !read_at(file, 256 * SECTOR, &mut anchor)?
            || u16le(&anchor, 0) != Some(2)
            || u32le(&anchor, 12) != Some(256)
        {
            return Ok(None);
        }
        let (Some(length), Some(location)) = (u32le(&anchor, 16), u32le(&anchor, 20)) else {
            return Ok(None);
        };
        let mut partition_start = None;
        let mut fsd = None;
        let mut descriptor = [0u8; 512];
        for sector in 0..(length / SECTOR as u32).min(64) {
            let offset = (u64::from(location) + u64::from(sector)) * SECTOR;
            if !read_at(file, offset, &mut descriptor)? {
                return Ok(None);
            }
            match u16le(&descriptor, 0) {
                // Partition Descriptor.
                Some(5) => partition_start = u32le(&descriptor, 188),
                // Logical Volume Descriptor.
                Some(6) => {
                    let block_size = u32le(&descriptor, 212);
                    let map_count = u32le(&descriptor, 268);
                    // Only type 1 (physical) maps with 2 KiB blocks, as on
                    // Windows media; metadata/virtual maps end the scan.
                    if block_size != Some(SECTOR as u32)
                        || map_count != Some(1)
                        || descriptor[440] != 1
                    {
                        return Ok(None);
                    }
                    fsd = u32le(&descriptor, 252);
                }
                Some(8) => break,
                _ => {}
            }
        }
        let (Some(partition_start), Some(fsd)) = (partition_start, fsd) else {
            return Ok(None);
        };
        let mut set = [0u8; 512];
        let fsd_offset = (u64::from(partition_start) + u64::from(fsd)) * SECTOR;
        if !read_at(file, fsd_offset, &mut set)? || u16le(&set, 0) != Some(256) {
            return Ok(None);
        }
        let Some(root) = u32le(&set, 404) else {
            return Ok(None);
        };
        Ok(Some(Self {
            partition_start,
            root,
        }))
    }

    fn block_offset(&self, block: u32) -> u64 {
        (u64::from(self.partition_start) + u64::from(block)) * SECTOR
    }

    /// Read a (Extended) File Entry and the location of its data.
    fn file_entry(&self, file: &mut File, block: u32) -> io::Result<Option<UdfEntry>> {
        let mut entry = vec![0u8; SECTOR as usize];
        let entry_offset = self.block_offset(block);
        if !read_at(file, entry_offset, &mut entry)? {
            return Ok(None);
        }
        let (ea_at, ad_at, ads_start) = match u16le(&entry, 0) {
            Some(261) => (168, 172, 176),
            Some(266) => (208, 212, 216),
            _ => return Ok(None),
        };
        let file_type = entry[27];
        let flags = u16le(&entry, 34).unwrap_or(0);
        let (Some(size), Some(ea_len), Some(ad_len)) = (
            u64le(&entry, 56),
            u32le(&entry, ea_at),
            u32le(&entry, ad_at),
        ) else {
            return Ok(None);
        };
        let start = ads_start + ea_len as usize;
        let Some(ads) = entry.get(start..start + ad_len as usize) else {
            return Ok(None);
        };
        let ad_size = match flags & 7 {
            0 => 8,
            1 => 16,
            // Embedded: the data is the allocation descriptor area itself.
            3 => {
                if size > u64::from(ad_len) {
                    return Ok(None);
                }
                let extents = vec![Extent {
                    offset: Some(entry_offset + start as u64),
                    len: size,
                }];
                return Ok(Some(UdfEntry {
                    is_dir: file_type == 4,
                    size,
                    extents,
                }));
            }
            _ => return Ok(None),
        };
        let mut extents = Vec::new();
        for ad in ads.chunks_exact(ad_size) {
            let (Some(raw_len), Some(position)) = (u32le(ad, 0), u32le(ad, 4)) else {
                return Ok(None);
            };
            let len = u64::from(raw_len & 0x3fff_ffff);
            if len == 0 {
                break;
            }
            extents.push(match raw_len >> 30 {
                0 => Extent {
                    offset: Some(self.block_offset(position)),
                    len,
                },
                1 | 2 => Extent { offset: None, len },
                // Continuation of the descriptor list in another block.
                _ => return Ok(None),
            });
        }
        let recorded = extents
            .iter()
            .fold(0u64, |sum, e| sum.saturating_add(e.len));
        if recorded < size {
            return Ok(None);
        }
        Ok(Some(UdfEntry {
            is_dir: file_type == 4,
            size,
            extents,
        }))
    }

    fn read_directory(&self, file: &mut File, entry: &UdfEntry) -> io::Result<Option<Vec<u8>>> {
        if entry.size > MAX_DIRECTORY_BYTES {
            return Ok(None);
        }
        let listed = IsoEntry {
            path: String::new(),
            size: entry.size,
            is_dir: false,
            extents: entry.extents.clone(),
        };
        let mut out = Vec::with_capacity(entry.size as usize);
        let mut buffer = vec![0u8; 64 * 1024];
        match copy_file(file, &listed, &mut buffer, |chunk| {
            out.extend_from_slice(chunk);
            Ok(())
        }) {
            Ok(()) => Ok(Some(out)),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn list(&self, file: &mut File) -> io::Result<Option<IsoListing>> {
        let mut listing = Listing {
            entries: Vec::new(),
        };
        let mut visited = HashSet::new();
        let mut stack = vec![(self.root, String::new(), 0usize)];
        while let Some((block, path, depth)) = stack.pop() {
            if depth > MAX_DEPTH || !visited.insert(block) {
                return Ok(None);
            }
            let Some(entry) = self.file_entry(file, block)?.filter(|entry| entry.is_dir) else {
                return Ok(None);
            };
            let Some(directory) = self.read_directory(file, &entry)? else {
                return Ok(None);
            };
            let mut at = 0usize;
            while at + 38 <= directory.len() {
                let fid = &directory[at..];
                if u16le(fid, 0) != Some(257) {
                    return Ok(None);
                }
                let characteristics = fid[18];
                let name_len = usize::from(fid[19]);
                let (Some(icb), Some(iu_len)) = (u32le(fid, 24), u16le(fid, 36)) else {
                    return Ok(None);
                };
                let name_at = 38 + usize::from(iu_len);
                let record = (name_at + name_len + 3) & !3;
                let Some(raw_name) = fid.get(name_at..name_at + name_len) else {
                    return Ok(None);
                };
                at += record;
                // Skip the parent link and deleted entries.
                if characteristics & 0x0c != 0 {
                    continue;
                }
                let Some(name) = decode_cs0(raw_name).and_then(safe_name) else {
                    return Ok(None);
                };
                let child = join(&path, &name);
                let Some(entry) = self.file_entry(file, icb)? else {
                    return Ok(None);
                };
                let is_dir = entry.is_dir;
                if listing
                    .push(IsoEntry {
                        path: child.clone(),
                        size: if is_dir { 0 } else { entry.size },
                        is_dir,
                        extents: if is_dir { Vec::new() } else { entry.extents },
                    })
                    .is_none()
                {
                    return Ok(None);
                }
                if is_dir {
                    stack.push((icb, child, depth + 1));
                }
            }
        }
        Ok(Some(IsoListing {
            entries: listing.entries,
            udf: true,
        }))
    }
}

/// OSTA CS0 file identifier: compression ID 8 (Latin-1) or 16 (UTF-16BE).
fn decode_cs0(raw: &[u8]) -> Option<String> {
    let (&compression, bytes) = raw.split_first()?;
    match compression {
        8 => Some(bytes.iter().map(|&b| char::from(b)).collect()),
        16 if bytes.len() % 2 == 0 => char::decode_utf16(
            bytes
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]])),
        )
        .collect::<Result<String, _>>()
        .ok(),
        _ => None,
    }
}

// ---------------------------------------------------------- ISO 9660

struct Iso9660;

impl Iso9660 {
    fn list(file: &mut File) -> io::Result<Option<IsoListing>> {
        // Prefer a Joliet Supplementary Volume Descriptor for full names.
        let mut root = None;
        let mut joliet = false;
        let mut descriptor = [0u8; 190];
        for sector in 16..48u64 {
            if !read_at(file, sector * SECTOR, &mut descriptor)? || &descriptor[1..6] != b"CD001" {
                break;
            }
            match descriptor[0] {
                1 if root.is_none() => root = Some(descriptor[156..190].to_vec()),
                2 if matches!(&descriptor[88..91], b"%/@" | b"%/C" | b"%/E") => {
                    root = Some(descriptor[156..190].to_vec());
                    joliet = true;
                }
                255 => break,
                _ => {}
            }
        }
        let Some(root) = root else {
            return Ok(None);
        };
        let (Some(extent), Some(length)) = (u32le(&root, 2), u32le(&root, 10)) else {
            return Ok(None);
        };
        let mut listing = Listing {
            entries: Vec::new(),
        };
        let mut visited = HashSet::new();
        let mut stack = vec![(extent, length, String::new(), 0usize)];
        while let Some((extent, length, path, depth)) = stack.pop() {
            if depth > MAX_DEPTH || !visited.insert(extent) {
                return Ok(None);
            }
            let Some(directory) = read_vec(file, u64::from(extent) * SECTOR, u64::from(length))?
            else {
                return Ok(None);
            };
            let mut at = 0usize;
            while at < directory.len() {
                let record_len = usize::from(directory[at]);
                if record_len == 0 {
                    // Records never span sectors; skip the sector's padding.
                    at = (at / SECTOR as usize + 1) * SECTOR as usize;
                    continue;
                }
                let Some(record) = directory.get(at..at + record_len) else {
                    return Ok(None);
                };
                at += record_len;
                if record.len() < 34 {
                    return Ok(None);
                }
                let name_len = usize::from(record[32]);
                let Some(raw_name) = record.get(33..33 + name_len) else {
                    return Ok(None);
                };
                // Self and parent entries.
                if raw_name == [0] || raw_name == [1] {
                    continue;
                }
                let flags = record[25];
                let is_dir = flags & 2 != 0;
                let (Some(child_extent), Some(size)) = (u32le(record, 2), u32le(record, 10)) else {
                    return Ok(None);
                };
                let name = if joliet {
                    if name_len % 2 != 0 {
                        return Ok(None);
                    }
                    char::decode_utf16(
                        raw_name
                            .chunks_exact(2)
                            .map(|pair| u16::from_be_bytes([pair[0], pair[1]])),
                    )
                    .collect::<Result<String, _>>()
                    .ok()
                } else {
                    Some(raw_name.iter().map(|&b| char::from(b)).collect())
                };
                let Some(name) = name.map(|n| strip_version(&n)).and_then(safe_name) else {
                    return Ok(None);
                };
                let child = join(&path, &name);
                let extent = Extent {
                    offset: Some(u64::from(child_extent) * SECTOR),
                    len: u64::from(size),
                };
                // Multi-extent files (flag 0x80) repeat the record per extent.
                if let Some(previous) = listing.entries.last_mut() {
                    if !is_dir && previous.path == child && !previous.is_dir {
                        previous.size = previous.size.saturating_add(u64::from(size));
                        previous.extents.push(extent);
                        continue;
                    }
                }
                if listing
                    .push(IsoEntry {
                        path: child.clone(),
                        size: if is_dir { 0 } else { u64::from(size) },
                        is_dir,
                        extents: if is_dir { Vec::new() } else { vec![extent] },
                    })
                    .is_none()
                {
                    return Ok(None);
                }
                if is_dir {
                    stack.push((child_extent, size, child, depth + 1));
                }
            }
        }
        Ok(Some(IsoListing {
            entries: listing.entries,
            udf: false,
        }))
    }
}

/// `NAME.EXT;1` -> `NAME.EXT`, and a bare trailing dot is dropped.
fn strip_version(name: &str) -> String {
    let base = name.split_once(';').map_or(name, |(base, _)| base);
    base.strip_suffix('.').unwrap_or(base).to_owned()
}

#[cfg(any(test, feature = "fixtures"))]
#[doc(hidden)]
pub mod fixture {
    //! Builders for minimal, valid UDF and ISO 9660 images.

    use super::SECTOR;

    const S: usize = SECTOR as usize;

    pub enum Node {
        /// A file of `pattern` bytes; large ones are stored as zero runs.
        File(&'static str, u64),
        /// A file with exactly these contents.
        Bytes(&'static str, Vec<u8>),
        Dir(&'static str, Vec<Node>),
    }

    fn put16(b: &mut [u8], at: usize, v: u16) {
        b[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn put32(b: &mut [u8], at: usize, v: u32) {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put64(b: &mut [u8], at: usize, v: u64) {
        b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn cs0(name: &str) -> Vec<u8> {
        let mut out = vec![16u8];
        out.extend(name.encode_utf16().flat_map(u16::to_be_bytes));
        out
    }

    /// A UDF-only image whose partition starts at sector 300.
    pub fn udf(root: Vec<Node>) -> Vec<u8> {
        let partition = 300usize;
        let mut image = vec![0u8; (partition + 2) * S];
        // Anchor -> VDS at 257..260.
        let anchor = 256 * S;
        put16(&mut image, anchor, 2);
        put32(&mut image, anchor + 12, 256);
        put32(&mut image, anchor + 16, 3 * SECTOR as u32);
        put32(&mut image, anchor + 20, 257);
        let pd = 257 * S;
        put16(&mut image, pd, 5);
        put32(&mut image, pd + 188, partition as u32);
        let lvd = 258 * S;
        put16(&mut image, lvd, 6);
        put32(&mut image, lvd + 212, SECTOR as u32);
        put32(&mut image, lvd + 252, 0); // FSD at block 0
        put32(&mut image, lvd + 268, 1);
        image[lvd + 440] = 1;
        put16(&mut image, 259 * S, 8);
        // FSD at block 0; root File Entry at block 1.
        put16(&mut image, partition * S, 256);
        put32(&mut image, partition * S + 404, 1);
        let mut next_block = 2u32;
        write_dir(&mut image, partition, 1, root, &mut next_block);
        image
    }

    fn ensure(image: &mut Vec<u8>, partition: usize, block: u32) -> usize {
        let at = (partition + block as usize) * S;
        if image.len() < at + S {
            image.resize(at + S, 0);
        }
        at
    }

    /// Byte `i` of every fixture file, so copies can be checked.
    pub fn pattern(i: u64) -> u8 {
        (i % 251) as u8
    }

    fn file_entry(
        image: &mut Vec<u8>,
        partition: usize,
        block: u32,
        dir: bool,
        size: u64,
        data: Option<u32>,
    ) {
        let at = ensure(image, partition, block);
        put16(image, at, 261);
        image[at + 27] = if dir { 4 } else { 5 };
        put16(image, at + 34, 0); // short_ad
        put64(image, at + 56, size);
        put32(image, at + 168, 0);
        if let Some(data) = data {
            put32(image, at + 172, 8);
            put32(image, at + 176, size as u32);
            put32(image, at + 180, data);
        } else {
            // Large stand-ins: unrecorded (zero) extents of just under 1 GiB.
            let mut remaining = size;
            let mut ads = 0usize;
            while remaining > 0 {
                let len = remaining.min(0x3fff_f800);
                put32(image, at + 176 + ads * 8, (1 << 30) | len as u32);
                remaining -= len;
                ads += 1;
            }
            put32(image, at + 172, (ads * 8) as u32);
        }
    }

    /// Store small file contents in fresh blocks; returns the first block.
    fn file_data(image: &mut Vec<u8>, partition: usize, size: u64, next: &mut u32) -> Option<u32> {
        if size > 64 * 1024 {
            return None;
        }
        let first = *next;
        let blocks = (size as usize).div_ceil(S).max(1) as u32;
        *next += blocks;
        let at = ensure(image, partition, first + blocks - 1) - (blocks as usize - 1) * S;
        for i in 0..size {
            image[at + i as usize] = pattern(i);
        }
        Some(first)
    }

    fn bytes_data(image: &mut Vec<u8>, partition: usize, bytes: &[u8], next: &mut u32) -> u32 {
        let first = *next;
        let blocks = bytes.len().div_ceil(S).max(1) as u32;
        *next += blocks;
        let at = ensure(image, partition, first + blocks - 1) - (blocks as usize - 1) * S;
        image[at..at + bytes.len()].copy_from_slice(bytes);
        first
    }

    fn write_dir(
        image: &mut Vec<u8>,
        partition: usize,
        entry_block: u32,
        children: Vec<Node>,
        next: &mut u32,
    ) {
        let data_block = *next;
        *next += 1;
        let mut fids = Vec::new();
        // Parent link.
        let mut parent = vec![0u8; 40];
        put16(&mut parent, 0, 257);
        parent[18] = 0x0a;
        fids.extend(parent);
        let mut subdirs = Vec::new();
        for child in children {
            let (name, is_dir) = match &child {
                Node::File(name, _) | Node::Bytes(name, _) => (*name, false),
                Node::Dir(name, _) => (*name, true),
            };
            let icb = *next;
            *next += 1;
            let ident = cs0(name);
            let len = (38 + ident.len() + 3) & !3;
            let mut fid = vec![0u8; len];
            put16(&mut fid, 0, 257);
            fid[18] = if is_dir { 2 } else { 0 };
            fid[19] = ident.len() as u8;
            put32(&mut fid, 24, icb);
            fid[38..38 + ident.len()].copy_from_slice(&ident);
            fids.extend(fid);
            match child {
                Node::File(_, size) => {
                    let data = file_data(image, partition, size, next);
                    file_entry(image, partition, icb, false, size, data);
                }
                Node::Bytes(_, bytes) => {
                    let data = bytes_data(image, partition, &bytes, next);
                    file_entry(image, partition, icb, false, bytes.len() as u64, Some(data));
                }
                Node::Dir(_, grandchildren) => subdirs.push((icb, grandchildren)),
            }
        }
        file_entry(
            image,
            partition,
            entry_block,
            true,
            fids.len() as u64,
            Some(data_block),
        );
        let at = ensure(image, partition, data_block);
        image[at..at + fids.len()].copy_from_slice(&fids);
        for (icb, grandchildren) in subdirs {
            write_dir(image, partition, icb, grandchildren, next);
        }
    }

    /// An ISO 9660 image (no Joliet) with uppercase `NAME;1` identifiers.
    pub fn iso9660(root: Vec<Node>) -> Vec<u8> {
        let mut image = vec![0u8; 20 * S];
        let pvd = 16 * S;
        image[pvd] = 1;
        image[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        image[17 * S] = 255;
        image[17 * S + 1..17 * S + 6].copy_from_slice(b"CD001");
        let mut next = 19u32;
        let root_extent = next;
        next += 1;
        let record = dir_record(root_extent, S as u32, true, &[0]);
        image[pvd + 156..pvd + 156 + record.len()].copy_from_slice(&record);
        write_iso_dir(&mut image, root_extent, root, &mut next);
        image
    }

    fn dir_record(extent: u32, size: u32, dir: bool, name: &[u8]) -> Vec<u8> {
        let len = (33 + name.len() + 1) & !1;
        let mut r = vec![0u8; len];
        r[0] = len as u8;
        put32(&mut r, 2, extent);
        r[6..10].copy_from_slice(&extent.to_be_bytes());
        put32(&mut r, 10, size);
        r[14..18].copy_from_slice(&size.to_be_bytes());
        r[25] = if dir { 2 } else { 0 };
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name);
        r
    }

    fn write_iso_dir(image: &mut Vec<u8>, extent: u32, children: Vec<Node>, next: &mut u32) {
        let mut records = dir_record(extent, S as u32, true, &[0]);
        records.extend(dir_record(extent, S as u32, true, &[1]));
        let mut subdirs = Vec::new();
        for child in children {
            match child {
                Node::File(name, size) => {
                    let ident = format!("{};1", name.to_ascii_uppercase());
                    let data = *next;
                    *next += (size as usize).div_ceil(S).max(1) as u32;
                    let at = data as usize * S;
                    if image.len() < at + size as usize {
                        image.resize(at + size as usize, 0);
                    }
                    for i in 0..size {
                        image[at + i as usize] = pattern(i);
                    }
                    records.extend(dir_record(data, size as u32, false, ident.as_bytes()));
                }
                Node::Bytes(name, bytes) => {
                    let ident = format!("{};1", name.to_ascii_uppercase());
                    let data = *next;
                    *next += bytes.len().div_ceil(S).max(1) as u32;
                    let at = data as usize * S;
                    if image.len() < at + bytes.len() {
                        image.resize(at + bytes.len(), 0);
                    }
                    image[at..at + bytes.len()].copy_from_slice(&bytes);
                    records.extend(dir_record(
                        data,
                        bytes.len() as u32,
                        false,
                        ident.as_bytes(),
                    ));
                }
                Node::Dir(name, grandchildren) => {
                    let child_extent = *next;
                    *next += 1;
                    records.extend(dir_record(
                        child_extent,
                        S as u32,
                        true,
                        name.to_ascii_uppercase().as_bytes(),
                    ));
                    subdirs.push((child_extent, grandchildren));
                }
            }
        }
        let at = extent as usize * S;
        if image.len() < at + S {
            image.resize(at + S, 0);
        }
        image[at..at + records.len()].copy_from_slice(&records);
        for (child_extent, grandchildren) in subdirs {
            write_iso_dir(image, child_extent, grandchildren, next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{iso9660, udf, Node};
    use super::*;
    use std::io::Write;

    fn listed(name: &str, bytes: &[u8]) -> Option<IsoListing> {
        let path = std::env::temp_dir().join(format!("rufus-isofs-{}-{name}", std::process::id()));
        File::create(&path)
            .and_then(|mut f| f.write_all(bytes))
            .expect("write fixture");
        let mut file = File::open(&path).expect("open fixture");
        let result = list(&mut file).expect("list fixture");
        std::fs::remove_file(&path).expect("remove fixture");
        result
    }

    fn windows_tree() -> Vec<Node> {
        vec![
            Node::File("bootmgr", 413_738),
            Node::Dir(
                "efi",
                vec![Node::Dir(
                    "boot",
                    vec![Node::File("bootx64.efi", 2_854_232)],
                )],
            ),
            Node::Dir("sources", vec![Node::File("install.wim", 6_000_000_000)]),
        ]
    }

    #[test]
    fn udf_listing_reads_nested_windows_layout() {
        let listing = listed("udf", &udf(windows_tree())).expect("UDF listing");
        assert!(listing.udf);
        assert!(listing.has_file("bootmgr"));
        assert!(listing.has_file("EFI/BOOT/BOOTX64.EFI"));
        assert_eq!(
            listing.find("sources/install.wim").map(|e| e.size),
            Some(6_000_000_000)
        );
        assert_eq!(
            listing.largest_file().map(|e| e.path.as_str()),
            Some("sources/install.wim")
        );
        assert!(listing.find("efi/boot").is_some_and(|e| e.is_dir));
    }

    #[test]
    fn iso9660_listing_strips_versions_and_matches_case_insensitively() {
        let listing = listed(
            "iso",
            &iso9660(vec![
                Node::Dir("isolinux", vec![Node::File("isolinux.bin", 38_912)]),
                Node::File("readme.txt", 120),
            ]),
        )
        .expect("ISO 9660 listing");
        assert!(!listing.udf);
        assert!(listing.has_file("isolinux/isolinux.bin"));
        assert!(listing.has_file("README.TXT"));
        assert_eq!(listing.total_file_bytes(), 38_912 + 120);
    }

    #[test]
    fn unsafe_names_and_cycles_end_the_scan() {
        let mut bytes = udf(vec![Node::File("ok", 1)]);
        // Point the root File Entry's data at itself: the FID tag check fails.
        let root_entry = (300 + 1) * SECTOR as usize;
        bytes[root_entry + 180..root_entry + 184].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(listed("cycle", &bytes), None);

        // A ".." identifier is refused rather than joined into a path.
        let bytes = udf(vec![Node::File("..", 1)]);
        assert_eq!(listed("dotdot", &bytes), None);
    }

    #[test]
    fn copy_file_streams_recorded_and_unrecorded_extents() {
        use super::fixture::pattern;
        for (name, bytes) in [
            (
                "udf-copy",
                udf(vec![
                    Node::Dir("d", vec![Node::File("small.bin", 5000)]),
                    Node::File("big.bin", 200_000),
                ]),
            ),
            (
                "iso-copy",
                iso9660(vec![Node::Dir("d", vec![Node::File("small.bin", 5000)])]),
            ),
        ] {
            let path =
                std::env::temp_dir().join(format!("rufus-isofs-{}-{name}", std::process::id()));
            std::fs::write(&path, &bytes).expect("write fixture");
            let mut file = File::open(&path).expect("open fixture");
            let listing = list(&mut file).expect("list").expect("listing");
            let small = listing.find("d/small.bin").expect("small file").clone();
            let mut copied = Vec::new();
            let mut buffer = vec![0u8; 1000];
            copy_file(&mut file, &small, &mut buffer, |chunk| {
                copied.extend_from_slice(chunk);
                Ok(())
            })
            .expect("copy small file");
            assert_eq!(copied.len(), 5000, "{name}");
            assert!(
                copied
                    .iter()
                    .enumerate()
                    .all(|(i, &b)| b == pattern(i as u64)),
                "{name}"
            );
            if let Some(big) = listing.find("big.bin") {
                let mut total = 0u64;
                let mut buffer = vec![0u8; 8 << 20];
                copy_file(&mut file, big, &mut buffer, |chunk| {
                    total += chunk.len() as u64;
                    Ok(())
                })
                .expect("copy unrecorded file");
                assert_eq!(total, 200_000);
            }
            std::fs::remove_file(&path).expect("remove fixture");
        }
    }

    #[test]
    fn non_iso_input_is_not_listed() {
        assert_eq!(listed("empty", &[0u8; 64 * 1024]), None);
    }
}
