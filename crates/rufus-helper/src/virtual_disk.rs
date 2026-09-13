//! Read-only virtual disks exported by unprivileged, descriptor-bound providers.

use super::*;
use rufus_core::plan::ImageSourceKind;
use std::os::unix::fs::FileExt as _;

const QEMU_NBD: &[&str] = &["/usr/bin/qemu-nbd"];
const NBD_INFO: &[&str] = &["/usr/bin/nbdinfo"];
const NBD_COPY: &[&str] = &["/usr/bin/nbdcopy"];

pub(super) fn is_virtual(kind: ImageSourceKind) -> bool {
    matches!(kind, ImageSourceKind::Vhd | ImageSourceKind::Vhdx)
}

pub(super) fn require_tools() -> Result<(), HelperError> {
    find_tool(QEMU_NBD)?;
    find_tool(NBD_INFO)?;
    find_tool(NBD_COPY)?;
    Ok(())
}

fn command(
    source: &File,
    kind: ImageSourceKind,
    user: &InvokingUser,
    inspect: bool,
) -> Result<Command, HelperError> {
    if user.uid == 0 {
        return Err(HelperError::Operation(
            "virtual disk providers require a non-root invoking user".into(),
        ));
    }
    let driver = match kind {
        ImageSourceKind::Vhd => "vpc",
        ImageSourceKind::Vhdx => "vhdx",
        _ => return Err(HelperError::Operation("not a virtual disk".into())),
    };
    let tool = find_tool(if inspect { NBD_INFO } else { NBD_COPY })?;
    let mut command = Command::new(tool);
    if inspect {
        command.arg("--size");
    } else {
        command.args([
            "--synchronous",
            "--request-size=1048576",
            "--no-extents",
            "--allocated",
        ]);
    }
    command.args([
        "--",
        "[",
        find_tool(QEMU_NBD)?,
        "--read-only",
        "--image-opts",
        &format!("driver={driver},file.driver=file,file.filename=/proc/self/fd/0"),
        "]",
    ]);
    if !inspect {
        command.arg("-");
    }
    let mut input = source.try_clone()?;
    input.seek(SeekFrom::Start(0))?;
    command
        .env_clear()
        .stdin(Stdio::from(input))
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    drop_decoder_privileges(&mut command, user);
    // SAFETY: resource limits and prctl are async-signal-safe Linux syscalls.
    unsafe {
        command.pre_exec(|| {
            let memory = libc::rlimit {
                rlim_cur: 512 * 1024 * 1024,
                rlim_max: 512 * 1024 * 1024,
            };
            let core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &memory) != 0
                || libc::setrlimit(libc::RLIMIT_CORE, &core) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(command)
}

// QEMU's VPC reader does not implement parent chains. Validate the header it
// will use before launching it, rather than trusting a successful export.
fn validate_vhd(source: &File) -> Result<u64, HelperError> {
    let mut footer = [0u8; 512];
    source.read_exact_at(&mut footer, 0)?;
    if &footer[..8] != b"conectix" {
        let offset =
            source.metadata()?.len().checked_sub(512).ok_or_else(|| {
                HelperError::Operation("VHD has no complete 512-byte footer".into())
            })?;
        source.read_exact_at(&mut footer, offset)?;
    }
    let value =
        |offset| u32::from_be_bytes(footer[offset..offset + 4].try_into().expect("four bytes"));
    let checksum = value(64);
    footer[64..68].fill(0);
    let expected = !footer.iter().map(|byte| u32::from(*byte)).sum::<u32>();
    let version = u32::from_be_bytes(footer[12..16].try_into().expect("four bytes"));
    let kind = u32::from_be_bytes(footer[60..64].try_into().expect("four bytes"));
    if &footer[..8] != b"conectix" || version != 0x0001_0000 || checksum != expected {
        return Err(HelperError::Operation(
            "VHD footer is invalid or uses an unsupported legacy layout".into(),
        ));
    }
    if !matches!(kind, 2 | 3) {
        return Err(HelperError::Operation(
            "only standalone fixed or dynamic VHDs are supported; parent-dependent VHDs are rejected".into(),
        ));
    }
    let size = u64::from_be_bytes(footer[48..56].try_into().expect("eight bytes"));
    if size == 0 || size % 512 != 0 {
        return Err(HelperError::Operation(
            "VHD footer capacity is zero or unaligned".into(),
        ));
    }
    Ok(size)
}

pub(super) fn inspect(
    source: &File,
    kind: ImageSourceKind,
    user: &InvokingUser,
    cancel: &CancellationToken,
    capacity: u64,
) -> Result<u64, HelperError> {
    check_cancel(cancel)?;
    let declared_size = if kind == ImageSourceKind::Vhd {
        Some(validate_vhd(source)?)
    } else {
        None
    };
    let mut command = command(source, kind, user, true)?;
    let output = read_size_output(&mut command, cancel, Duration::from_secs(15))?;
    let size = std::str::from_utf8(&output)
        .ok()
        .and_then(|text| text.strip_suffix('\n'))
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse::<u64>().ok())
        .ok_or_else(|| HelperError::Operation("invalid virtual disk size from provider".into()))?;
    validate_export_size(size, declared_size, capacity)
}

fn validate_export_size(
    size: u64,
    declared_size: Option<u64>,
    capacity: u64,
) -> Result<u64, HelperError> {
    if size == 0 || size % 512 != 0 || size > capacity {
        return Err(HelperError::Operation(
            "virtual disk size is zero, unaligned, or exceeds target capacity".into(),
        ));
    }
    if let Some(declared) = declared_size {
        if size != declared {
            return Err(HelperError::Operation(format!(
                "VHD provider capacity ({size} bytes) differs from the footer ({declared} bytes); refusing ambiguous geometry"
            )));
        }
    }
    Ok(size)
}

fn read_size_output(
    command: &mut Command,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<Vec<u8>, HelperError> {
    let mut child = ManagedChild::spawn(command, "virtual disk inspection")?;
    let mut stdout =
        child.child_mut().stdout.take().ok_or_else(|| {
            HelperError::Operation("virtual disk inspection stdout missing".into())
        })?;
    set_nonblocking(stdout.as_raw_fd())?;
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let result = (|| {
        let mut buffer = [0; 64];
        loop {
            check_cancel(cancel)?;
            if Instant::now() >= deadline {
                return Err(HelperError::Operation(
                    "virtual disk inspection timed out".into(),
                ));
            }
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    if let Some(status) = child.try_wait()? {
                        if status.success() {
                            return Ok(output);
                        }
                        return Err(HelperError::Operation(
                            "virtual disk provider rejected the image; it may be malformed, parent-dependent, or require repair".into(),
                        ));
                    }
                }
                Ok(count) => {
                    if output.len() + count > 64 {
                        return Err(HelperError::Operation(
                            "virtual disk inspection output exceeded its limit".into(),
                        ));
                    }
                    output.extend_from_slice(&buffer[..count]);
                }
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    })();
    child.terminate_and_reap()?;
    result
}

pub(super) fn decoder(
    source: &File,
    kind: ImageSourceKind,
    user: &InvokingUser,
) -> Result<ManagedChild, HelperError> {
    if kind == ImageSourceKind::Vhd {
        validate_vhd(source)?;
    }
    ManagedChild::spawn(
        &mut command(source, kind, user, false)?,
        "virtual disk decoder",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rufus_helper_protocol::SourceSpec;
    use std::fs::OpenOptions;
    use std::os::unix::fs::DirBuilderExt;

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "rufus-vhd-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .expect("private fixture directory");
            Self { dir }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).expect("remove generated fixtures");
        }
    }

    fn footer(kind: u32) -> [u8; 512] {
        let mut footer = [0u8; 512];
        footer[..8].copy_from_slice(b"conectix");
        footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        footer[48..56].copy_from_slice(&4096u64.to_be_bytes());
        footer[60..64].copy_from_slice(&kind.to_be_bytes());
        let checksum = !footer.iter().map(|byte| u32::from(*byte)).sum::<u32>();
        footer[64..68].copy_from_slice(&checksum.to_be_bytes());
        footer
    }

    #[test]
    fn vhd_guard_rejects_parent_dependencies_and_corruption() {
        let fixture = Fixture::new();
        let path = fixture.dir.join("guard.vhd");
        for kind in [0, 1, 2, 3, 4, u32::MAX] {
            std::fs::write(&path, footer(kind)).expect("write footer");
            let result = validate_vhd(&File::open(&path).expect("open fixture"));
            assert_eq!(result.is_ok(), matches!(kind, 2 | 3), "type {kind}");
        }
        let mut corrupt = footer(2);
        corrupt[64] ^= 1;
        std::fs::write(&path, corrupt).expect("write corrupt checksum");
        assert!(validate_vhd(&File::open(&path).expect("open fixture")).is_err());
        let mut fixed = vec![0u8; 4096];
        fixed.extend_from_slice(&footer(2));
        std::fs::write(&path, &fixed).expect("write fixed footer");
        assert!(validate_vhd(&File::open(&path).expect("open fixture")).is_ok());
        fixed.pop();
        std::fs::write(&path, fixed).expect("write legacy footer");
        assert!(validate_vhd(&File::open(&path).expect("open fixture")).is_err());
        for size in [0u64, 513] {
            let mut invalid = footer(3);
            invalid[48..56].copy_from_slice(&size.to_be_bytes());
            invalid[64..68].fill(0);
            let checksum = !invalid.iter().map(|byte| u32::from(*byte)).sum::<u32>();
            invalid[64..68].copy_from_slice(&checksum.to_be_bytes());
            std::fs::write(&path, invalid).expect("write invalid capacity");
            let error = validate_vhd(&File::open(&path).expect("open fixture"))
                .expect_err("invalid declared capacity");
            assert!(error.to_string().contains("capacity is zero or unaligned"));
        }
    }

    #[test]
    fn vhd_export_must_match_declared_capacity() {
        let declared = 1024 * 1024 * 1024;
        for size in [1_073_479_680, declared + 512] {
            let error = validate_export_size(size, Some(declared), u64::MAX)
                .expect_err("geometry mismatch must not become an export size");
            assert!(error.to_string().contains("refusing ambiguous geometry"));
        }
        assert_eq!(
            validate_export_size(declared, Some(declared), declared).expect("matching capacity"),
            declared
        );
        assert!(validate_export_size(declared, Some(declared), declared - 512).is_err());
    }

    #[test]
    fn size_probe_output_and_runtime_are_bounded() {
        let mut oversized = Command::new("/usr/bin/head");
        oversized
            .args(["-c", "256", "/dev/zero"])
            .stdout(Stdio::piped());
        let error = read_size_output(
            &mut oversized,
            &CancellationToken::new(),
            Duration::from_secs(2),
        )
        .expect_err("oversized output");
        assert!(error.to_string().contains("output exceeded"));

        let mut stalled = Command::new("/usr/bin/sleep");
        stalled.arg("10").stdout(Stdio::piped());
        let started = Instant::now();
        let error = read_size_output(
            &mut stalled,
            &CancellationToken::new(),
            Duration::from_millis(50),
        )
        .expect_err("probe deadline");
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    fn vhdx_sector_fields(encoded: &[u8]) -> [usize; 2] {
        // Follow the generated fixture's region/metadata tables rather than
        // assuming where QEMU placed the metadata values. MS-VHDX sections 2.5/2.6.
        let region_guid = [
            0x06, 0xa2, 0x7c, 0x8b, 0x90, 0x47, 0x9a, 0x4b, 0xb8, 0xfe, 0x57, 0x5f, 0x05, 0x0f,
            0x88, 0x6e,
        ];
        let table = &encoded[192 * 1024..256 * 1024];
        assert_eq!(&table[..4], b"regi");
        let count = u32::from_le_bytes(table[8..12].try_into().expect("region count")) as usize;
        let region = table[16..16 + count * 32]
            .chunks_exact(32)
            .find(|entry| entry[..16] == region_guid)
            .expect("metadata region");
        let offset = usize::try_from(u64::from_le_bytes(
            region[16..24].try_into().expect("metadata offset"),
        ))
        .expect("fixture offset fits usize");
        let metadata = &encoded[offset..offset + 65536];
        assert_eq!(&metadata[..8], b"metadata");
        let count =
            u16::from_le_bytes(metadata[10..12].try_into().expect("metadata count")) as usize;
        [
            [
                0x1d, 0xbf, 0x41, 0x81, 0x6f, 0xa9, 0x09, 0x47, 0xba, 0x47, 0xf2, 0x33, 0xa8, 0xfa,
                0xab, 0x5f,
            ],
            [
                0xc7, 0x48, 0xa3, 0xcd, 0x5d, 0x44, 0x71, 0x44, 0x9c, 0xc9, 0xe9, 0x88, 0x52, 0x51,
                0xc5, 0x56,
            ],
        ]
        .map(|guid| {
            let entry = metadata[32..32 + count * 32]
                .chunks_exact(32)
                .find(|entry| entry[..16] == guid)
                .expect("sector size metadata");
            assert_eq!(&entry[20..24], &4u32.to_le_bytes());
            let value = offset
                + u32::from_le_bytes(entry[16..20].try_into().expect("value offset")) as usize;
            assert_eq!(&encoded[value..value + 4], &512u32.to_le_bytes());
            value
        })
    }

    fn reject_unsupported_copies(
        source: &File,
        path: &Path,
        kind: ImageSourceKind,
        user: &InvokingUser,
    ) {
        let mut encoded = Vec::new();
        let mut input = source.try_clone().expect("clone fixture descriptor");
        input.seek(SeekFrom::Start(0)).expect("rewind fixture");
        input.read_to_end(&mut encoded).expect("read fixture");
        let reject = |bytes: &[u8], expected: Option<&str>| {
            std::fs::write(path, bytes).expect("write rejection fixture");
            let source = File::open(path).expect("bind rejection fixture");
            let error = inspect(&source, kind, user, &CancellationToken::new(), u64::MAX)
                .expect_err("unsupported image must not produce an export size");
            if let Some(expected) = expected {
                assert!(error.to_string().contains(expected), "{error}");
            }
            assert_eq!(std::fs::read(path).expect("read after rejection"), bytes);
        };
        reject(&encoded[..511], None);
        match kind {
            ImageSourceKind::Vhd => {
                let footer_offset = if &encoded[..8] == b"conectix" {
                    0
                } else {
                    encoded.len() - 512
                };
                let mut damaged = encoded.clone();
                damaged[footer_offset + 64] ^= 1;
                reject(&damaged, Some("VHD footer is invalid"));

                // Preserve a valid checksum so rejection proves the parent-type guard.
                let mut parent = encoded.clone();
                let footer = &mut parent[footer_offset..footer_offset + 512];
                footer[60..64].copy_from_slice(&4u32.to_be_bytes());
                footer[64..68].fill(0);
                let checksum = !footer.iter().map(|byte| u32::from(*byte)).sum::<u32>();
                footer[64..68].copy_from_slice(&checksum.to_be_bytes());
                reject(&parent, Some("parent-dependent VHDs are rejected"));

                if footer_offset == 0 {
                    let header_offset =
                        u64::from_be_bytes(encoded[16..24].try_into().expect("header offset"));
                    let header_offset = usize::try_from(header_offset).expect("fixture offset");
                    assert_eq!(&encoded[header_offset..header_offset + 8], b"cxsparse");
                    damaged = encoded;
                    damaged[header_offset] ^= 1;
                    reject(&damaged, Some("provider rejected the image"));
                }
            }
            ImageSourceKind::Vhdx => {
                let mut four_kn = encoded.clone();
                for offset in vhdx_sector_fields(&encoded) {
                    four_kn[offset..offset + 4].copy_from_slice(&4096u32.to_le_bytes());
                }
                // These standalone 8 MiB fixtures have no sector bitmap blocks;
                // changing sector size leaves their payload block mapping intact.
                reject(&four_kn, Some("provider rejected the image"));
                // QEMU can use either redundant header; invalidate both CRC fields.
                for offset in [64 * 1024, 128 * 1024] {
                    assert_eq!(&encoded[offset..offset + 4], b"head");
                    encoded[offset + 4] ^= 1;
                }
                reject(&encoded, Some("provider rejected the image"));
            }
            _ => unreachable!("virtual disk fixture"),
        }
    }

    #[test]
    #[ignore = "requires qemu-img, qemu-nbd, nbdinfo, nbdcopy, and bzip2; run explicitly in CI"]
    fn providers_roundtrip_bound_sources_and_reject_oversized_targets() {
        require_tools().expect("required providers");
        let fixture = Fixture::new();
        let raw = fixture.dir.join("original.raw");
        let mut payload = vec![0u8; 8 * 1024 * 1024];
        payload[512..4608].fill(0x5a);
        payload[7 * 1024 * 1024..7 * 1024 * 1024 + 65536].fill(0xa5);
        std::fs::write(&raw, &payload).expect("write raw fixture");
        let uid = unsafe { libc::geteuid() };
        let user = InvokingUser::from_uid(if uid == 0 { 65534 } else { uid }).expect("test user");
        for (name, driver, options, kind) in [
            (
                "fixed.vhd",
                "vpc",
                "subformat=fixed,force_size=on",
                ImageSourceKind::Vhd,
            ),
            (
                "dynamic.vhd",
                "vpc",
                "subformat=dynamic,force_size=on",
                ImageSourceKind::Vhd,
            ),
            (
                "fixed.vhdx",
                "vhdx",
                "subformat=fixed",
                ImageSourceKind::Vhdx,
            ),
            (
                "dynamic.vhdx",
                "vhdx",
                "subformat=dynamic",
                ImageSourceKind::Vhdx,
            ),
        ] {
            let path = fixture.dir.join(name);
            let output = Command::new("/usr/bin/qemu-img")
                .args(["convert", "-f", "raw", "-O", driver, "-o", options])
                .arg(&raw)
                .arg(&path)
                .output()
                .expect("create virtual disk");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let source_file = File::open(&path).expect("bind virtual disk");
            let mut source = SourceSpec {
                path: path.clone(),
                kind,
                size_bytes: source_file.metadata().expect("metadata").len(),
                decompressed_size_bytes: None,
                expected_sha256: None,
            };
            // Providers must continue reading the selected inode, not this replacement.
            std::fs::rename(&path, fixture.dir.join(format!("{name}.bound")))
                .expect("rename bound source");
            std::fs::write(&path, b"replacement is not a virtual disk").expect("replace pathname");
            let source_file = if uid == 0 {
                let snapshot = source_snapshot::create(&source_file, &CancellationToken::new())
                    .expect("protect source before parsing");
                std::fs::write(
                    fixture.dir.join(format!("{name}.bound")),
                    b"changed original inode",
                )
                .expect("mutate original after protected copy");
                snapshot
            } else {
                source_file
            };
            assert!(inspect(
                &source_file,
                kind,
                &user,
                &CancellationToken::new(),
                payload.len() as u64 - 1
            )
            .is_err());
            source.decompressed_size_bytes = Some(
                inspect(
                    &source_file,
                    kind,
                    &user,
                    &CancellationToken::new(),
                    payload.len() as u64,
                )
                .expect("inspect size"),
            );
            assert_eq!(source.decompressed_size_bytes, Some(payload.len() as u64));
            let destination_path = fixture.dir.join(format!("{name}.result"));
            let destination = File::create(&destination_path).expect("create test destination");
            let mut sink: EventSink = Box::new(|_| {});
            let receipt = write_image(
                &destination,
                WriteSource {
                    file: &source_file,
                    spec: &source,
                    invoking_user: Some(&user),
                },
                WriteMode::DdImage,
                &CancellationToken::new(),
                payload.len() as u64,
                JobId(1),
                &mut sink,
            )
            .expect("stream virtual disk");
            assert_eq!(receipt.bytes_written, payload.len() as u64);
            assert_eq!(receipt.sha256, <[u8; 32]>::from(Sha256::digest(&payload)));
            assert_eq!(
                std::fs::read(&destination_path).expect("read decoded output"),
                payload
            );
            verify_device_hash(
                &destination_path,
                receipt.bytes_written,
                &receipt.sha256,
                &CancellationToken::new(),
            )
            .expect("readback verification");
            let mut child =
                decoder(&source_file, kind, &user).expect("start decoder for cancellation");
            let mut stdout = child.child_mut().stdout.take().expect("decoder stdout");
            let mut prefix = [0; 512];
            stdout
                .read_exact(&mut prefix)
                .expect("decoder is actively streaming");
            let status = std::fs::read_to_string(format!("/proc/{}/status", child.child.id()))
                .expect("decoder process status");
            let uid_line = status
                .lines()
                .find(|line| line.starts_with("Uid:"))
                .expect("process UIDs");
            assert!(uid_line
                .split_whitespace()
                .skip(1)
                .all(|uid| uid == user.uid.to_string()));
            assert!(child.group_exists().expect("active process group"));
            child.terminate_and_reap().expect("cancel process group");
            assert!(child.reaped, "decoder leader was not reaped");
            // A killed descendant may remain a zombie until its new parent reaps it.
            // Check pipe closure instead of requiring immediate PGID disappearance.
            set_nonblocking(stdout.as_raw_fd()).expect("nonblocking decoder stdout");
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut remaining = [0; 65536];
            loop {
                assert!(
                    Instant::now() < deadline,
                    "decoder retained stdout after cancellation"
                );
                match stdout.read(&mut remaining) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => panic!("read cancelled decoder stdout: {error}"),
                }
            }
            reject_unsupported_copies(
                &source_file,
                &fixture.dir.join(format!("{name}.damaged")),
                kind,
                &user,
            );
        }
        reject_unreplayed_journal(&fixture, &user);
        reject_parent_chains(&fixture, &user);
    }

    fn unpack_fixture(name: &str, checksum: &str, size: usize) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let compressed = std::fs::read(&path).expect("read pinned fixture");
        assert_eq!(format!("{:x}", Sha256::digest(compressed)), checksum);
        let output = Command::new("/usr/bin/bzip2")
            .arg("-dc")
            .arg(path)
            .output()
            .expect("decompress pinned fixture");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), size);
        output.stdout
    }

    fn reject_parent_chains(fixture: &Fixture, user: &InvokingUser) {
        let mut originals = Vec::new();
        for (name, checksum, size, kind, parent) in [
            (
                "parent.vhd",
                "e35f5236e25f3b47b81b80f2471a978dd13b931893a4a91b32cc4601da6c687e",
                8_394_752,
                ImageSourceKind::Vhd,
                true,
            ),
            (
                "child.vhd",
                "31bcb2b830e256a0be839276a352f9f76c473d9e8f4d54c72879f706de986ac4",
                4_200_448,
                ImageSourceKind::Vhd,
                false,
            ),
            (
                "parent.vhdx",
                "3c78e4ffe7554c34de6a219c948135d827a2f4fa032fdccc71b1a63a3b244cae",
                12_582_912,
                ImageSourceKind::Vhdx,
                true,
            ),
            (
                "child.vhdx",
                "4f3efb282aff3ae7109527e6586caa8e1ec4494e5acea64019495888cfcd8b26",
                9_437_184,
                ImageSourceKind::Vhdx,
                false,
            ),
        ] {
            let bytes = unpack_fixture(&format!("{name}.bz2"), checksum, size);
            let path = fixture.dir.join(name);
            std::fs::write(&path, &bytes).expect("write parent-chain fixture");
            let source = File::open(&path).expect("bind parent-chain fixture");
            let result = inspect(&source, kind, user, &CancellationToken::new(), u64::MAX);
            if parent {
                if kind == ImageSourceKind::Vhd {
                    // Older QEMU treats DiscUtils' creator as CHS-sized. Refuse
                    // that export, rather than silently omitting its final 256 KiB.
                    let mut probe = command(&source, kind, user, true).expect("raw size probe");
                    let size = read_size_output(
                        &mut probe,
                        &CancellationToken::new(),
                        Duration::from_secs(15),
                    )
                    .expect("standalone VHD provider must open");
                    match size.as_slice() {
                        b"1073741824\n" => {
                            assert_eq!(result.expect("full-capacity export"), 1_073_741_824)
                        }
                        b"1073479680\n" => assert!(result
                            .expect_err("CHS mismatch must be refused")
                            .to_string()
                            .contains("refusing ambiguous geometry")),
                        _ => panic!("unexpected VHD provider capacity: {size:?}"),
                    }
                    let mut legacy = bytes.clone();
                    for offset in [0, legacy.len() - 512] {
                        let footer = &mut legacy[offset..offset + 512];
                        assert_eq!(&footer[..8], b"conectix");
                        footer[28..32].copy_from_slice(b"vpc ");
                        footer[64..68].fill(0);
                        let checksum = !footer.iter().map(|byte| u32::from(*byte)).sum::<u32>();
                        footer[64..68].copy_from_slice(&checksum.to_be_bytes());
                    }
                    let legacy_path = fixture.dir.join("legacy-chs.vhd");
                    std::fs::write(&legacy_path, &legacy).expect("write legacy geometry control");
                    let legacy_source = File::open(&legacy_path).expect("bind legacy control");
                    let error = inspect(
                        &legacy_source,
                        kind,
                        user,
                        &CancellationToken::new(),
                        u64::MAX,
                    )
                    .expect_err("legacy CHS export must be refused on all providers");
                    assert!(error.to_string().contains("refusing ambiguous geometry"));
                    assert_eq!(
                        std::fs::read(legacy_path).expect("unchanged legacy control"),
                        legacy
                    );
                } else {
                    assert_eq!(
                        result.expect("standalone VHDX parent must open"),
                        1_073_741_824
                    );
                }
            } else {
                let error = result.expect_err("parent-dependent child must be refused");
                let expected = if kind == ImageSourceKind::Vhd {
                    "parent-dependent VHDs are rejected"
                } else {
                    "provider rejected the image"
                };
                assert!(error.to_string().contains(expected), "{name}: {error}");
            }
            originals.push((path, bytes));
        }
        for (path, bytes) in originals {
            assert_eq!(std::fs::read(path).expect("unchanged chain fixture"), bytes);
        }
    }

    fn reject_unreplayed_journal(fixture: &Fixture, user: &InvokingUser) {
        let bytes = unpack_fixture(
            "dirty-log.vhdx.bz2",
            "f294ddc9a9ab2a621cee73d6ac30ea692a8864ebd14be211e795d4c5b400adbb",
            30 * 1024 * 1024,
        );
        let original_path = fixture.dir.join("unreplayed.vhdx");
        std::fs::write(&original_path, &bytes).expect("write journal fixture");
        let original = File::open(&original_path).expect("bind journal fixture");
        let error = inspect(
            &original,
            ImageSourceKind::Vhdx,
            user,
            &CancellationToken::new(),
            u64::MAX,
        )
        .expect_err("read-only provider must refuse journal replay");
        assert!(
            error.to_string().contains("provider rejected the image"),
            "{error}"
        );
        assert_eq!(
            std::fs::read(&original_path).expect("unchanged source"),
            bytes
        );

        // Repair only a disposable control copy, never the selected source.
        // Success afterward proves the fixture is replayable, not simply corrupt.
        let control_path = fixture.dir.join("replayed-control.vhdx");
        std::fs::write(&control_path, &bytes).expect("write control copy");
        let control = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&control_path)
            .expect("open writable control");
        if unsafe { libc::geteuid() } == 0 {
            // SAFETY: control is a live descriptor for this test's private regular file.
            assert_eq!(
                unsafe { libc::fchown(control.as_raw_fd(), user.uid, user.gid) },
                0
            );
        }
        let mut repair = Command::new("/usr/bin/qemu-img");
        repair
            .args(["check", "-r", "all", "-f", "vhdx", "/proc/self/fd/0"])
            .env_clear()
            .stdin(Stdio::from(control))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        drop_decoder_privileges(&mut repair, user);
        let repaired = repair.output().expect("repair disposable control copy");
        assert!(
            repaired.status.success(),
            "{}",
            String::from_utf8_lossy(&repaired.stderr)
        );
        assert_eq!(
            inspect(
                &File::open(&control_path).expect("bind replayed control"),
                ImageSourceKind::Vhdx,
                user,
                &CancellationToken::new(),
                u64::MAX,
            )
            .expect("replayed control must open"),
            10 * 1024 * 1024 * 1024
        );
        assert_eq!(
            std::fs::read(&original_path).expect("original after control"),
            bytes
        );
    }
}
