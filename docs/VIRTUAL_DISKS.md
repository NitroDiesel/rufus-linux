# Virtual disk conversion work in progress

VHD/VHDX conversion is still blocked by the desktop and the helper's
`validate_request` allowlist. Keep both gates closed until the requirements
below are met. The private helper backend is implemented and tested, but is
not a released feature or an approved way to write physical media.

## Tested implementation

`crates/rufus-helper/src/virtual_disk.rs` uses `nbdinfo --size` and `nbdcopy`
with a read-only `qemu-nbd` subprocess. Each subprocess receives the bound
source descriptor as stdin. Fixed QEMU options select `vpc` or `vhdx` over the
`file` driver at `/proc/self/fd/0`. There are no user-supplied QEMU options,
network URLs, kernel NBD devices, or decoded temporary disk images.

The helper protects the source, then inspects virtual capacity before target
preparation. It caps probe
output at 64 bytes and duration at 15 seconds, rejects a zero, unaligned, or
oversized export, and limits each provider's address space to 512 MiB.
Providers run as a non-root invoking user with cleared environment and no
new privileges. The existing writer hashes an ordered stdout stream, bounds
writes by both exported size and target capacity, and verifies target readback.

The explicit provider integration test generates patterned 8 MiB raw data,
converts it to fixed/dynamic VHD and fixed/dynamic VHDX, replaces the selected
pathname after binding, and compares every decoded byte and its SHA-256.
It checks size rejection and terminates a process group during active output.
It also rejects 11 damaged copies across those four formats: 511-byte
truncations, corrupt VHD footer checksums, checksummed type-4 VHD metadata,
a corrupt dynamic-VHD header signature, and corrupt checksums in both redundant
VHDX headers. Byte comparisons verify inspection leaves each input unchanged.
These are selected corruption fixtures, not exhaustive format validation or
genuine parent-chain coverage.

Two additional copies exercise 4K logical/physical sectors. The test follows
the generated fixed/dynamic VHDX metadata tables and changes both sector-size
values from 512 to 4096, leaving headers and payload mappings intact. These
standalone 8 MiB fixtures have no sector bitmap blocks. Both are rejected and
remain byte-for-byte unchanged; the original 512-byte-sector fixtures still
round-trip. This is generated metadata coverage, not a Windows-created 4Kn
operating-system image or a 4Kn physical-target test.

The provider integration also uses QEMU's pinned, replayable VHDX journal
fixture. It verifies read-only inspection refuses the unreplayed log without
changing the source bytes. A separate disposable control copy is repaired by
an unprivileged `qemu-img` process and then reports the expected 10 GiB capacity.
The original remains byte-for-byte unchanged. This tests a real journal case,
not an invalid header disguised as a pending log. It does not authorize product
repair or prove every journal state is rejected.
Run it with:

```sh
cargo test -p rufus-helper providers_roundtrip --locked -- --ignored
```

This test requires QEMU, libnbd tools, and test-only `bzip2`. CI runs it on
Debian, Fedora, and Arch; it is ignored in the provider-independent test suite.
Local file-only testing used QEMU 11.1.1 and libnbd 1.24.3. No physical USB or
guest boot test has been performed for this backend.

## Protected source copies

An open descriptor protects against pathname replacement, not changes to the
same inode. A concurrent writer can change VHD metadata after Rust validates
it but before QEMU reads it. Timestamp checks and advisory `flock` cannot
enforce source immutability. In particular, QEMU can interpret parent-dependent
VHD metadata as a standalone dynamic disk, producing incorrect output.

The owner approved private temporary source copies on 2026-09-10, including
source-file-sized storage when copy-on-write is unavailable. The implemented
`source_snapshot.rs` creates an anonymous root-owned `O_TMPFILE` in `/var/tmp`.
It tries `FICLONE`, then falls back to a bounded sparse copy. It checks source
timestamps and size for ordinary concurrent edits and preserves 256 MiB free
space. Failure and cancellation close the anonymous file, reclaiming storage.

Before parsing, the helper changes permissions to `0444`, opens a read-only
descriptor, and closes the writable descriptor. The unprivileged decoder cannot
reopen it for writing or change permissions. Tests run as root verify this and
confirm all four conversion fixtures remain unchanged after the original inode
is overwritten. `FICLONE` is atomic; the copy fallback is not a guaranteed
point-in-time snapshot, but its completed bytes are immutable during validation
and decoding. `/var/tmp` must support anonymous temporary files.

Run the protected-copy test as root using the compiled helper test executable
with `--exact source_snapshot::tests::protected_snapshot_is_stable_and_read_only
--ignored`. It operates only on anonymous regular-file fixtures. CI runs this
test explicitly, alongside root-launched provider tests that verify dropped UIDs.

Provider-independent tests inject low space and cancellation during the copy
fallback. They verify copying stops at the next chunk boundary, the partial
copy remains anonymous, the 256 MiB headroom boundary is enforced, sparse data
is preserved byte-for-byte, and a shortened source fails with unexpected EOF.
These injected tests do not fill a filesystem or prove the reflink path.

### Isolated filesystem evidence

Manual checks on 2026-09-11 used source commit `9bd9fc7`, kernel
`7.2.3-1-cachyos`, and btrfs-progs 7.1. Both ran through
`unshare --mount --propagation private`, so temporary mounts did not replace
host mounts outside the test process:

- A 4 MiB tmpfs at `/var/tmp` caused the protected-snapshot success fixture to
  exit 101 with the expected "not enough space" error and 256 MiB headroom
  explanation. This proves initial refusal on a genuinely undersized
  filesystem, not mid-copy ENOSPC handling.
- A 512 MiB Btrfs image stored on a private 1 GiB tmpfs exercised copy-on-write.
  The protected-snapshot test passed; `strace` observed
  `ioctl(5, BTRFS_IOC_CLONE or FICLONE, 3) = 0`. The same test confirmed the copy
  stayed unchanged after original-inode edits and denied the decoder write
  access.

The namespace exited, the temporary image was released, and host mount/loop
checks found no remaining test mounts or loop device. These checks are not yet
automated in CI and do not replace physical-media or mid-copy ENOSPC testing.

## Write cleanup and decoder deadlines

Image writes now share one completion path that terminates the decoder and
attempts destination synchronization on success, cancellation, and stream
errors. A failed flush remains an error; an earlier write/decoder failure is
preserved alongside cleanup failures. Regression tests cover read/write errors,
empty input, declared-size overflow after partial output, changed source size,
decoded-size mismatch, checksum mismatch, and a corrupt gzip CRC. Discard/error
devices make `fsync` fail deliberately to prove these error paths attempt it;
these fixtures do not write physical media.

Managed subprocesses observe exit with `waitid(WNOWAIT)`, signal remaining
group members while the leader PID is still reserved, then reap the leader.
Cleanup becomes a no-op after reaping. The descendant regression test preserves
the leader's exit status and checks that a surviving descendant releases stdout.
`ManagedChild` must remain the sole owner of child waits in production.

The owner approved a 120-second decoder inactivity limit on 2026-09-10. Both
compressed-image and virtual-disk stdout are nonblocking. Each read starts its
budget after the previous destination write and progress callback, excluding
time spent writing to the target. EOF passes the remaining budget to the child
exit wait, rather than granting another 120 seconds. A timeout uses the shared
cleanup path. This does not interrupt blocking kernel reads/writes or `fsync`.
It is not a total operation deadline, and other managed commands retain their
existing cancellable waits.

Regression tests use short injected deadlines to cover partial output followed
by a stall, delayed EOF with a still-running child, cancellation, and successful
reads separated by downstream work longer than the timeout.

## Remaining gates and provider limits

- Automate the isolated Btrfs and undersized-filesystem checks above, and test
  actual mid-copy filesystem exhaustion. Initial low-space refusal, reflink,
  and the copy fallback have local evidence.
- Add genuine parent-dependent VHD/VHDX fixtures before claiming those rejection
  boundaries are verified end to end. Generated fixed/dynamic 4Kn metadata and
  one real unreplayed-log fixture now verify refusal and source preservation.
  Cross-check Windows-created 4Kn media before extending sector-size support.
- Verify these cleanup paths on loop-backed targets and disposable physical
  media, including write failures and disconnects.
- Complete virtual-size destructive confirmation and native package dependencies.
  Read-only asynchronous desktop inspection is implemented below; it does not
  satisfy the write-path UI gate.
- Perform loop-backed and boot tests, then the disposable physical-media gate.
- Update capability claims and release versions before publishing installers.

The VHD guard currently requires a checksummed, version 1, 512-byte footer and
disk type 2 or 3. Parent-dependent VHD, legacy 511-byte footers, parent-dependent
VHDX, 4Kn VHDX, and VHDX requiring journal replay are not supported by this path.
Never repair the selected input automatically or fall back to copying container
bytes when the provider rejects it.

## Read-only desktop preview

The desktop worker opens the selected file read-only with `O_NOFOLLOW` and
`O_NONBLOCK`, then uses that descriptor for recognition and capacity inspection.
It reuses `inspect_virtual_disk_for_user` from the helper library, which rejects
root callers, non-regular files, writable descriptors, and path-only descriptors.
The provider keeps the existing 15-second probe limit and process-group cleanup.
The source inode is not protected against edits during this advisory preview.

The summary distinguishes container file size from virtual disk size. Unknown
capacity stays unknown, with a provider or parsing explanation. Configuration,
checksums, and Start are disabled during inspection. Replacement cancels the old
token; generation checks discard stale results. Closing requests cancellation
and waits for completion. Blocking kernel filesystem I/O is not interruptible
by this token, so a stalled filesystem can delay exit.

The non-root desktop fixture creates VHD/VHDX containers, checks reported virtual
capacity and unchanged source bytes, and confirms conversion remains blocked.
State tests cover stale results, request refusal while inspecting, and shutdown
cancellation. The shared image analyzer rejects named pipes without waiting for
a writer. No preview test writes a block device.

The provider cancellation fixture checks leader reaping and bounded stdout EOF,
not immediate PGID disappearance. Killed descendants can briefly remain zombies
until their new parent reaps them; that made the previous root CI assertion race.
Production process signaling is unchanged.

### Desktop verification and remaining UI checks

Local checks on 2026-09-12 passed 80 provider-independent workspace tests,
the non-root desktop preview fixture, the root-only preview rejection test,
and both root-launched protected-source/provider fixtures. Formatting, Clippy
with warnings denied, and the optimized workspace build passed.

An isolated Xvfb/Openbox session exercised the native file chooser and displayed
an 8 MiB VHDX container as a 16 MiB virtual disk. Light/dark layouts were checked
at 560x720 and a maximized 1600x979 client size; 480x560 was also checked for
scrolling and clipped controls. Fixed side gutters keep the workbench full-width
on small windows and capped at 760 pixels when maximized.

The modal checks found and fixed Tab escaping to background controls. Shared
`ModalOverlay` now traps Tab/Backtab, handles Escape, and restores the safe
button after a scrim click. Log and About were exercised with outside clicks,
wheel input, repeated forward/backward Tab, Escape, and Space activation.
Confirmation uses the same overlay with a two-button focus cycle; that cycle
still needs a disposable-device UI smoke test before a release.

The isolated X11 startup offset was reproduced without a window manager: the
frame shifted upward by 120 pixels, leaving a black strip below. Slint 1.9.2
creates an initial 800x600 native window before requesting the workbench's
560x720 preferred size. Its GLX resize path is a no-op, and the 120-pixel size
difference matches the observed offset. A stale drawable is the inferred cause;
the graphics driver was not instrumented.

The desktop now sets the native initial size before graphics-context creation,
using the pinned winit backend's public window-attributes hook. Native DPI uses
logical dimensions; an explicit positive `SLINT_SCALE_FACTOR` uses physical
dimensions. Other backend selections retain Slint's normal selection behavior.
Keep these dimensions synchronized with `AppWindow`'s preferred dimensions.

On 2026-09-12, the corrected no-window-manager frame matched its post-resize
capture pixel-for-pixel. The new `scripts/ci/desktop-render.sh` passed all 18
startup, minimum-size, and large-window checks across light/dark and scale
factors 1, 1.25, and 2. It checks the full-height canvas gutter, which detects
the old black strip; it is not a complete visual or interaction test. Separate
Openbox captures covered both themes at 560x720 and maximized size, and native
X11 scaling at 2 produced a complete 1120x1440 startup frame. The 80 regular
workspace tests, formatting, Clippy, and optimized desktop build passed again.

Recheck on a normal X11/Wayland desktop before the next installer release.
Local screenshots are in ignored `target/ui-validation/`; they are evidence
artifacts, not a dependency of the cloud-agent workflow.

## Primary references

- [VHDX logical-sector requirements](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/e45dcf18-f45b-4507-8760-e76fa538a61b).
  Fixture table GUIDs and layout also follow the
  [QEMU VHDX reader](https://github.com/qemu/qemu/blob/v10.0.0/block/vhdx.c).

- [QEMU journal fixture provenance](https://github.com/qemu/qemu/commit/e78835b722eb26f5a56370166e99b69e9751ea2a),
  pinned locally with its digest and license in `crates/rufus-helper/tests/fixtures/`.
  The full provider suite passed as both the desktop user and root with dropped
  provider privileges on 2026-09-12, alongside 80 regular workspace tests and Clippy.

- [nbdcopy subprocess streaming](https://libguestfs.org/nbdcopy.1.html)
- [libnbd socket activation and descriptor handling](https://github.com/libguestfs/libnbd/blob/master/generator/states-connect-socket-activation.c)
- [QEMU VHD reader](https://github.com/qemu/qemu/blob/master/block/vpc.c)
- [QEMU VHDX reader](https://github.com/qemu/qemu/blob/master/block/vhdx.c)
- [Linux file lease semantics](https://man7.org/linux/man-pages/man2/F_SETLEASE.2const.html)
