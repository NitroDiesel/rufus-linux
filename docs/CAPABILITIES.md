# Capability and compatibility matrix

Rufus Linux preserves upstream concepts where Linux has a safe implementation. It does not claim that a visible option works until the privileged helper can complete it end to end. **Available** means enabled in this release; **blocked** means the input is recognized but Start remains disabled with a reason; **planned** means no complete product flow exists yet.

## Available in 0.1

| Capability | Linux implementation | Notes |
|---|---|---|
| Removable-device discovery | Linux sysfs and `/proc/self/mountinfo` | Automatic hotplug refresh plus a manual fallback; root, boot, home, read-only, held and unstable targets are rejected. USB HDDs and fixed disks are explicit expert opt-ins. |
| Authorized destructive operations | Short-lived `/usr/libexec/rufus-linux-helper` through `pkexec` | The desktop never links or executes privileged disk code. Target identity is independently resolved after authorization. |
| Raw image write | Bounded streaming copy to an exclusively locked target | Supports `.img`, `.raw`, and other raw disk images. Source size, target identity and target capacity are rechecked. |
| ISOHybrid disk-image write | Same raw writer | Hybrid ISO media is written byte-for-byte. Non-hybrid ISO file-copy mode is blocked. |
| Compressed raw images | Fixed-path gzip, bzip2, xz/lzma, zstd and bsdtar providers | Decompressed output is capacity-bounded. Decoder failure or size mismatch is fatal. ZIP input should contain one disk image. |
| Write verification | SHA-256 of the bytes streamed, followed by target readback | Success is reported only after hash match, `fsync`, and block cache flush. |
| Cancellation | Separate helper process termination with UI event polling | Cancellation leaves an explicit warning that the target may be incomplete. |
| MBR/GPT/super-floppy formatting | `parted`, kernel partition-table reread and udev settle | Super-floppy correctly formats the whole device. |
| FAT/FAT32, exFAT, NTFS, UDF, ext2/3/4 | Distribution formatter tools | Only installed providers appear. Supported cluster/block sizes are passed to the formatter. |
| Bad-block overwrite test | `badblocks -w` | Separate destructive confirmation; requires e2fsprogs. |
| MD5, SHA-1, SHA-256, SHA-512 | RustCrypto | Runs off the UI thread. MD5/SHA-1 are comparison hashes, never trust decisions. |
| Light and dark presentation | Device Workbench Slint UI | Scrollable at small window sizes, with prominent device identity and a continuous Windows-style write-progress track. |
| Arch, Debian and Fedora integration | PKGBUILD, complete Debian metadata, RPM spec, desktop/AppStream/polkit metadata | Release URLs and checksums are finalized by release automation. |
| Portable x86_64 desktop | AppImage built against glibc 2.28 with FUSE extraction fallback | Inspection and checksums need no installation. Writes still require the root-owned helper from a matching native package. |

## Recognized but blocked

| Capability | Why it is blocked |
|---|---|
| Ordinary/non-hybrid ISO file-copy media | Needs audited ISO extraction, mount lifecycle, bootloader installation and fixture/QEMU coverage. |
| Windows installer media | Depends on ISO file-copy, split-WIM support, boot files and tested UEFI:NTFS handling. |
| Windows To Go | A WIM/ESD cannot be raw-copied. A real implementation needs partition, wimlib apply, BCD and offline-registry work. |
| VHD/VHDX input | Container bytes are never treated as raw sectors. A future flow will use bounded qemu-img conversion. |
| FFU apply/capture | No maintained, independently verifiable Linux servicing provider has been selected. |
| ReFS creation | Linux has no safe production ReFS formatter. |
| FreeDOS | Redistributable system files and exact boot-sector provenance are not packaged yet. |
| MS-DOS | Proprietary Microsoft system files are never bundled or fetched. |
| Linux persistence | Partition layout and distro-specific `persistence.conf`/casper behavior need image fixtures and boot tests. |
| Syslinux, GRUB2, GRUB4DOS, ReactOS and UEFI:NTFS installation | Planning types exist, but no incomplete bootloader path is exposed as success. |
| Windows 11 setup customization | Requires generated unattend files, offline WIM/registry edits and versioned fixtures. |
| UEFI runtime validation and Secure Boot revocation checks | Verified payload, SBAT/SVN/DBX parsing and signed update data are not packaged. |
| Drive capture | Root must write through a user-opened file descriptor; arbitrary root-owned output paths are intentionally rejected. |

## Planned secondary workflows

- Optical disc or mounted media to ISO through read-only providers.
- Signed in-app download catalog with explicit image selection.
- Package-manager-aware update policy.
- Full gettext/Fluent catalogs and RTL layouts.
- Settings persistence, log export, image drag-and-drop, and CLI preselection.
- VHD/VHDX capture after safe file-descriptor passing is available.

## Expert controls

USB hard drives and fixed/internal disks remain separated from ordinary removable devices and require an explicit session opt-in. Enabling visibility never bypasses root/boot/home, swap, holder, identity, source-on-target, size, unmount, lock, or flush checks.

Controls that weaken the safety boundary—ignoring size checks, shared writes, silent target selection, or arbitrary helper commands—are not accepted as feature-parity requirements.
