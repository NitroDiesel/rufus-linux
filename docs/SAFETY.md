# Safety and privilege model

The safety boundary is more important than feature parity. The desktop process must be usable without root privileges and must never gain ambient raw-disk access.

## Process boundary

```text
Slint desktop application (user)
  -> inspect Linux sysfs, mountinfo, and swap state
  -> build an immutable operation plan
  -> show target-specific confirmation
  -> request one polkit authorization
  -> send one versioned request to a narrow helper over standard I/O

Privileged helper (root, no UI)
  -> independently resolve and revalidate the target
  -> unmount/swapoff and obtain exclusive access
  -> execute only allowlisted operations
  -> stream structured progress/errors
  -> flush, reread, close, and exit
```

The helper does not accept arbitrary commands, shell fragments, output paths, environment-controlled tool names, or relative paths. It receives a versioned operation enum, an absolute regular source path, target stable identity, expected major/minor, size, model, serial, and an explicit byte limit. It rejects source symlinks and rechecks source metadata after authorization. Device capture stays disabled until the desktop can pass a user-opened output file descriptor.

## Target eligibility

A candidate is rejected if it backs or contains any of:

- `/`, `/usr`, `/boot`, `/boot/efi`, or the running application's executable;
- the selected image file;
- active swap, LVM physical volumes in active use, mounted RAID members, or active device-mapper parents/children;
- a loop device whose backing file is on the target;
- a read-only, missing, zero-size, or unexpectedly changed device.

USB HDDs/SSDs and large memory cards are hidden by default. Enabling them is session-visible and does not bypass the checks above.

Do not trust `/dev/sdX` names. Capture a stable `/dev/disk/by-id` path where available, kernel major/minor, size, logical/physical sector sizes, model, serial, transport, and device graph. Resolve all of them again after authorization and immediately before the first write. Any mismatch cancels the operation.

## Destructive confirmation

The final confirmation names the device, model, serial (or a clearly marked unavailable value), capacity, and partitions that will be removed. It uses the exact action name—“Write image,” “Format device,” “Erase device”—and states that all data will be destroyed.

Additional confirmations are required for:

- a fixed disk or non-USB removable disk;
- multiple existing partitions;
- a bad-block/fake-capacity test;
- full zeroing or fast zeroing;
- a Windows installer configured to erase a destination silently.

The silent installer retains three separate acknowledgements: other destination disks will be disconnected, the media will not be left in an unintended computer, and the user accepts responsibility for resulting data loss.

## Write sequence invariants

1. Open the source read-only and reject symlinks unless the user selected the resolved regular file.
2. Verify the source is not on the target device graph.
3. Unmount every target filesystem, disable target-backed swap, and recheck the
   device graph before opening the target.
4. Refuse if any consumer remains. Never silently fall back from exclusive to shared write access.
5. Check source size, decompressed upper bound, target size, partition arithmetic, sector alignment, filesystem limits, and integer overflow.
6. Write within an explicit byte range. Short reads/writes are errors.
7. On cancellation, stop at a safe boundary, flush, and explain that the device may be unusable.
8. `fsync` written descriptors, request the kernel block flush, reread the partition table, then close.
9. Report success only after flushing has completed. Verification failures are failures, not warnings.

## Downloads and trust

- Require HTTPS with normal certificate and hostname validation.
- Accept release data, DBX data, helper payloads, and boot assets only after detached signature and digest verification.
- Pin the signing identity/public key in reviewed source; key rotation requires a signed transition.
- Fail closed on a missing, malformed, unknown, expired, or invalid signature.
- Preserve UEFI revoked-bootloader warnings and distinguish “not revoked,” “revoked,” and “could not determine.”
- Never use MD5 or SHA-1 as a trust decision; those hashes exist only for user comparison with legacy published values.

## Logging and privacy

Logs include the operation plan, stable device identity, tools/versions, stage transitions, byte counts, warnings, and error causes. They exclude authorization tokens, user passwords, full network credentials, and unrelated device contents. Persistent logging is opt-in.

## Polkit policy

The packaged policy in `packaging/polkit/` authorizes a single helper entry point. Authorization is expected for each destructive operation (`auth_admin`), is never granted to inactive sessions, and is not replaced by permissive udev rules such as `MODE="0666"`.

The AppImage never supplies the executable that polkit authorizes. Portable
user-owned bytes remain unprivileged; destructive actions require the same
root-owned `/usr/libexec/rufus-linux-helper` and exact-path policy installed by
a native package. The desktop checks file ownership, write permissions, policy
content, and the effective registered action before launching `pkexec`.
