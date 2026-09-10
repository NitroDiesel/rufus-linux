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
Run it with:

```sh
cargo test -p rufus-helper providers_roundtrip --locked -- --ignored
```

This test requires QEMU and libnbd tools. CI explicitly installs and runs it on
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

## Remaining gates and provider limits

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

- Add malformed-image fixtures, low-space/copy-interruption tests, and an
  inactivity deadline for a stalled decoding stream. Exercise the reflink path
  on a supporting filesystem; local testing has covered the copy fallback.
- Verify these cleanup paths on loop-backed targets and disposable physical
  media, including write failures and disconnects.
- Wire asynchronous desktop inspection and provider availability, truthful
  virtual-size confirmation, and native package dependencies. Use the UI skill
  and review/visual gates in `PROJECT_CONTEXT.md` for that change.
- Perform loop-backed and boot tests, then the disposable physical-media gate.
- Update capability claims and release versions before publishing installers.

The VHD guard currently requires a checksummed, version 1, 512-byte footer and
disk type 2 or 3. Parent-dependent VHD, legacy 511-byte footers, parent-dependent
VHDX, 4Kn VHDX, and VHDX requiring journal replay are not supported by this path.
Never repair the selected input automatically or fall back to copying container
bytes when the provider rejects it.

## Primary references

- [nbdcopy subprocess streaming](https://libguestfs.org/nbdcopy.1.html)
- [libnbd socket activation and descriptor handling](https://github.com/libguestfs/libnbd/blob/master/generator/states-connect-socket-activation.c)
- [QEMU VHD reader](https://github.com/qemu/qemu/blob/master/block/vpc.c)
- [QEMU VHDX reader](https://github.com/qemu/qemu/blob/master/block/vhdx.c)
- [Linux file lease semantics](https://man7.org/linux/man-pages/man2/F_SETLEASE.2const.html)
