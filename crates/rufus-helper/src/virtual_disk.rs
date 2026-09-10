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
fn validate_vhd(source: &File) -> Result<(), HelperError> {
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
    Ok(())
}

pub(super) fn inspect(
    source: &File,
    kind: ImageSourceKind,
    user: &InvokingUser,
    cancel: &CancellationToken,
    capacity: u64,
) -> Result<u64, HelperError> {
    check_cancel(cancel)?;
    if kind == ImageSourceKind::Vhd {
        validate_vhd(source)?;
    }
    let mut command = command(source, kind, user, true)?;
    let output = read_size_output(&mut command, cancel, Duration::from_secs(15))?;
    let size = std::str::from_utf8(&output)
        .ok()
        .and_then(|text| text.strip_suffix('\n'))
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse::<u64>().ok())
        .ok_or_else(|| HelperError::Operation("invalid virtual disk size from provider".into()))?;
    if size == 0 || size % 512 != 0 || size > capacity {
        return Err(HelperError::Operation(
            "virtual disk size is zero, unaligned, or exceeds target capacity".into(),
        ));
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

    #[test]
    #[ignore = "requires qemu-img, qemu-nbd, nbdinfo, and nbdcopy; run explicitly in CI"]
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
            child.terminate_and_reap().expect("cancel process group");
            assert!(!child.group_exists().expect("process group state"));
        }
    }
}
