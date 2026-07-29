# Third-party software and assets

This file is a packaging checklist, not a substitute for the license texts shipped by a distribution. Packagers must generate a complete dependency and asset inventory for each release.

## Upstream Rufus

Portions derived from the Rufus source tree are licensed under **GPL-3.0-or-later**. Preserve upstream copyright headers, the GPL text, modification notices, and complete corresponding source. The upstream project name and logo may also be subject to trademark rules separate from the GPL.

## Expected system dependencies

These tools and libraries are intended to remain system dependencies rather than copied into this repository:

| Component | Typical license | Purpose |
|---|---|---|
| Slint | GPL-3.0-only or commercial | Declarative desktop interface |
| winit and Slint render backends | Apache-2.0/MIT and component licenses | Wayland/X11 windowing and rendering |
| Fontconfig/FreeType | MIT-style and FTL/GPL | System font discovery and rendering |
| polkit | LGPL-2.0-or-later | Explicit privilege authorization |
| util-linux | GPL/LGPL components | Block flush, unmount, and swap management |
| GNU Parted | GPL-3.0-or-later | MBR/GPT partition creation |
| dosfstools | GPL-3.0-or-later | FAT creation/checking |
| exfatprogs | GPL-2.0-or-later | exFAT creation/checking |
| e2fsprogs | GPL/LGPL components | ext2/ext3/ext4 creation/checking |
| ntfs-3g/ntfsprogs | GPL-2.0-or-later | NTFS creation and access |
| udftools | GPL-2.0-or-later | UDF creation |
| libarchive | BSD-2-Clause | ZIP-compressed raw-image extraction |
| gzip, bzip2, xz, zstd | Various free-software licenses | Compressed raw-image decoding |

License versions above are orientation only. The installed package's license metadata is authoritative.

## Boot assets

FreeDOS, Syslinux, GRUB, GRUB4DOS, ReactOS, and UEFI:NTFS assets each carry their own licenses and source-offer requirements. Do not add a binary boot asset without:

1. its exact source/version and download URL;
2. its license text in `assets/licenses/`;
3. a reproducible way to obtain or rebuild it;
4. a recorded cryptographic digest; and
5. confirmation that modification does not invalidate a required Secure Boot signature.

## Microsoft material

Do **not** commit or redistribute MS-DOS files, `diskcopy.dll`, Windows ISO/WIM/ESD/FFU images, Windows bootloaders, `oscdimg.exe`, ADK files, or other Microsoft binaries. They are not covered by this project's GPL license. User-supplied material remains subject to its original license, and the user is responsible for having a valid Windows license where required.

## Translation reuse

Translations copied or adapted from upstream Rufus are derivative GPL material. Preserve translator credits and identify materially changed strings so they can be reviewed by native speakers.
