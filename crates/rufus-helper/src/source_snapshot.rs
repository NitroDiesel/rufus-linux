//! Anonymous, root-owned snapshots for parsers that need stable source bytes.

use super::*;
use std::os::fd::FromRawFd;

pub(super) fn create(source: &File, cancel: &CancellationToken) -> Result<File, HelperError> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(HelperError::Operation(
            "protected source snapshots require the privileged helper".into(),
        ));
    }
    check_cancel(cancel)?;
    let before = source.metadata()?;
    if !before.is_file() || before.len() == 0 {
        return Err(HelperError::Operation(
            "snapshot source must be a nonempty regular file".into(),
        ));
    }
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/var/tmp")?;
    if directory.metadata()?.uid() != 0 {
        return Err(HelperError::Operation(
            "snapshot directory must be root-owned".into(),
        ));
    }
    reserve_space(&directory, 0)?;
    // No pathname is ever created. Closing the final descriptor reclaims space.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: openat returned a new owned descriptor.
    let mut snapshot = unsafe { File::from_raw_fd(descriptor) };
    // FICLONE is an atomic copy-on-write snapshot when both filesystems support it.
    let cloned = unsafe {
        libc::ioctl(
            snapshot.as_raw_fd(),
            0x4004_9409 as libc::c_ulong,
            source.as_raw_fd(),
        )
    } == 0;
    if !cloned {
        snapshot.set_len(0)?;
        copy_bounded(source, &mut snapshot, before.len(), cancel)?;
    }
    check_cancel(cancel)?;
    let after = source.metadata()?;
    if before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
        || snapshot.metadata()?.len() != before.len()
    {
        return Err(HelperError::Operation(
            "source changed while creating its snapshot".into(),
        ));
    }
    if unsafe { libc::fchmod(snapshot.as_raw_fd(), 0o444) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let readonly = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(format!("/proc/self/fd/{}", snapshot.as_raw_fd()))?;
    drop(snapshot);
    Ok(readonly)
}

fn copy_bounded(
    source: &File,
    destination: &mut File,
    size: u64,
    cancel: &CancellationToken,
) -> Result<(), HelperError> {
    use std::os::unix::fs::FileExt as _;
    let mut buffer = vec![0; 1024 * 1024];
    let mut offset = 0;
    while offset < size {
        check_cancel(cancel)?;
        let count = usize::try_from((size - offset).min(buffer.len() as u64))
            .map_err(|_| HelperError::Operation("snapshot size overflow".into()))?;
        source.read_exact_at(&mut buffer[..count], offset)?;
        if buffer[..count].iter().all(|byte| *byte == 0) {
            destination.seek(SeekFrom::Current(count as i64))?;
        } else {
            reserve_space(destination, count as u64)?;
            destination.write_all(&buffer[..count])?;
        }
        offset += count as u64;
    }
    destination.set_len(size)?;
    check_cancel(cancel)
}

fn reserve_space(file: &File, bytes: u64) -> Result<(), HelperError> {
    let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::fstatvfs(file.as_raw_fd(), info.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: successful fstatvfs initialized the structure.
    let info = unsafe { info.assume_init() };
    let available = u128::from(info.f_bavail) * u128::from(info.f_frsize);
    if available < u128::from(bytes) + 256 * 1024 * 1024 {
        return Err(HelperError::Operation("not enough space in /var/tmp for a protected source snapshot; 256 MiB must remain free".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires root; uses only anonymous regular files under /var/tmp"]
    fn protected_snapshot_is_stable_and_read_only() {
        assert_eq!(
            unsafe { libc::geteuid() },
            0,
            "run this file-only test as root"
        );
        let descriptor = unsafe {
            libc::open(
                c"/var/tmp".as_ptr(),
                libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
                0o600,
            )
        };
        assert!(descriptor >= 0, "anonymous source fixture");
        let mut source = unsafe { File::from_raw_fd(descriptor) };
        let payload = vec![0x5a; 2 * 1024 * 1024];
        source.write_all(&payload).expect("write original fixture");
        let mut snapshot = create(&source, &CancellationToken::new()).expect("protected snapshot");
        source.seek(SeekFrom::Start(0)).expect("rewind original");
        source
            .write_all(&vec![0xa5; payload.len()])
            .expect("change original inode");
        let mut actual = Vec::new();
        snapshot.read_to_end(&mut actual).expect("read snapshot");
        assert_eq!(actual, payload);
        let metadata = snapshot.metadata().expect("snapshot metadata");
        assert_eq!(metadata.uid(), 0);
        assert_eq!(metadata.mode() & 0o777, 0o444);
        assert_eq!(metadata.nlink(), 0);
        assert!(snapshot.write_all(b"changed").is_err());

        let user = InvokingUser::from_uid(65534).expect("unprivileged test account");
        let mut command = Command::new("/usr/bin/test");
        command
            .args(["-w", "/proc/self/fd/0"])
            .stdin(Stdio::from(snapshot))
            .env_clear();
        drop_decoder_privileges(&mut command, &user);
        assert_eq!(
            command.status().expect("check decoder write access").code(),
            Some(1)
        );
        let cancel = CancellationToken::new();
        cancel.request();
        assert!(matches!(
            create(&source, &cancel),
            Err(HelperError::Cancelled)
        ));
    }
}
