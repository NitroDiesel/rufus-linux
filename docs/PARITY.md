# Upstream Rufus feature parity

Goal: every upstream Rufus feature exists in Rufus Linux, implemented safely
on Linux. This tracker lists each upstream feature, its status here, and the
planned order. Upstream reference: `src/rufus.rc`, `src/rufus.c`,
`src/format.c`, `src/iso.c`, and `res/loc/rufus.loc` of
[pbatard/rufus](https://github.com/pbatard/rufus).

Status: **done** works end to end; **partial** works with a stated gap;
**planned** is not yet implemented. [`CAPABILITIES.md`](CAPABILITIES.md)
remains the authority on what a release enables.

## Drive properties

| Upstream feature | Status | Notes |
|---|---|---|
| Device list with hot-plug refresh | done | sysfs + mountinfo; system disks rejected. |
| List USB hard drives (advanced) | done | Expert option. |
| Boot selection: Disk or ISO image | partial | Raw, ISOHybrid, compressed, VHD/VHDX, and Windows ISOs done (0.1.6); non-hybrid Linux ISO file-copy planned. |
| Boot selection: Non bootable | done | Format only. |
| Boot selection: FreeDOS | planned | Needs redistributable FreeDOS files and boot sectors. |
| Boot selection: Syslinux, ReactOS, GRUB, GRUB4DOS, UEFI:NTFS (cheat mode) | planned | UEFI:NTFS payload is bundled (0.1.6); the cheat-mode entries follow bootloader installation. |
| Image option: Standard Windows installation | done | 0.1.6. |
| Image option: Windows To Go | planned | Needs wimlib apply, BCD, and offline registry. |
| Persistent partition size (Linux live) | planned | Slider exists; capability stays blocked. |
| Partition scheme MBR / GPT / super floppy | done | Super Floppy Disk is offered for non-bootable formats only, as upstream (0.1.7). |
| Target system BIOS / UEFI / BIOS or UEFI | partial | Honored for Windows media (0.1.6): UEFI:NTFS for UEFI, Windows 7 MBR and boot record for BIOS. Follows the scheme like upstream. |
| Add fixes for old BIOSes (extra partition, alignment) | planned | |
| Use Rufus MBR with BIOS ID | planned | |
| Runtime UEFI media validation (uefi-md5sum) | planned | |
| UEFI bootloader revocation / DBX checks | planned | Needs signed DBX data. |
| Download ISO (Fido) | planned | Needs HTTPS + signature policy from SAFETY.md. |
| ISO volume name proposed as label | done | 0.1.5. |

## Format options

| Upstream feature | Status | Notes |
|---|---|---|
| Volume label | done | Converted per filesystem like `ToValidLabel` (0.1.5). |
| File system FAT, FAT32, exFAT, NTFS, UDF, ext2/3/4 | done | ReFS has no Linux formatter. |
| Cluster size | done | |
| Quick format / slow format | done | |
| Check device for bad blocks | partial | Four-pattern write/read test; upstream pass-count choice planned. |
| Create extended label and icon files (autorun.inf) | planned | |

## Windows media

| Upstream feature | Status | Notes |
|---|---|---|
| Windows installer from ISO (NTFS/exFAT + UEFI:NTFS, or FAT32) | done | 0.1.6, with BIOS boot records on MBR. |
| Split install.wim > 4 GB on FAT32 | planned | |
| Windows User Experience dialog: bypass TPM/Secure Boot/RAM, no online account, local account, regional options, no data collection, no BitLocker, QoL improvements, SkuSiPolicy.p7b, S-Mode | done | 1.0.0. Shown on Start for Windows 10/11 media with upstream's options per build; choices persist. The answer file is `autounattend.xml` at the media root with upstream's `RunSynchronous` bypass (upstream's own fallback), since Rufus Linux does not rewrite `boot.wim`. Includes upstream's empty `appraiserres.dll` and signed Setup wrapper for in-place upgrades. S-Mode appears while Advanced is open, like upstream's expert mode. |
| Silent erase-and-install option | done | 1.0.0, with the edition choice, the three acknowledgements, and the ` (SILENT)` label. |
| Windows CA 2023 signed bootloaders | planned | Next: needs extracting `EFI_EX`/`Fonts_EX` from the LZX-compressed `boot.wim`. |
| Windows To Go | planned | |
| VHD/VHDX/FFU write | partial | VHD/VHDX done; FFU blocked. |

## Other tools

| Upstream feature | Status | Notes |
|---|---|---|
| MD5/SHA-1/SHA-256/SHA-512 checksums | done | |
| Log window | done | Persistent log planned. |
| About dialog | done | |
| Light/dark theme | done | Linux addition. |
| Drive capture to VHD/VHDX/ISO/FFU | planned | Needs a user-opened output descriptor. |
| Language selection (i18n) | planned | `rufus-i18n` groundwork only. |
| Update check | planned | Native packages update through the distribution. |
| Settings persistence and expert/cheat-mode shortcuts | partial | Display units (0.1.8) and Windows User Experience choices (1.0.0) persist; other settings and shortcuts planned. |
| Drag-and-drop image selection | planned | |

## Linux additions

- Plug-and-play: every write goes through udisks2, so the AppImage needs
  nothing installed (0.1.6). The native helper is the fallback.
- Selectable size and speed units for progress, with time left (0.1.8); binary KiB/MiB/GiB and decimal KB/MB/GB sizes (1.0.1).

## Order

1. ~~Windows installer media with UEFI:NTFS and BIOS boot records~~ (0.1.6);
   FAT32 with split WIM remains.
2. ~~Windows User Experience options through `autounattend.xml`~~ (1.0.0);
   the Windows CA 2023 bootloader option remains.
3. ~~Plug-and-play AppImage through udisks2~~ (0.1.6).
4. Non-hybrid Linux ISO file-copy with Syslinux/GRUB, then persistence.
5. FreeDOS, bad-block passes, extended label/icon, old-BIOS fixes, Rufus MBR.
6. Drive capture, Windows To Go, ISO download, revocation checks, i18n.

Each step ships as its own reviewed PR and release with tests, and updates
this file and `CAPABILITIES.md`.
