# BIOS boot records

Byte arrays from upstream Rufus `src/ms-sys/inc/` (ms-sys, GPLv2 or later),
converted unchanged to binary:

| File | Upstream array | Use |
|---|---|---|
| `mbr_win7_0x0.bin` | `mbr_win7.h` | Windows 7 MBR boot code (first 440 bytes) |
| `br_ntfs_0x0.bin`, `br_ntfs_0x54.bin` | `br_ntfs_0x0.h`, `br_ntfs_0x54.h` | NTFS boot record that loads BOOTMGR |
| `br_fat32_0x0.bin`, `br_fat32pe_0x52.bin`, `br_fat32pe_0x3f0.bin`, `br_fat32pe_0x1800.bin` | `br_fat32_0x0.h`, `br_fat32pe_*.h` | FAT32 boot record that loads BOOTMGR |

They are written exactly where upstream `write_win7_mbr`, `write_ntfs_br`,
and `write_fat_32_pe_br` write them, keeping the BIOS Parameter Block.
