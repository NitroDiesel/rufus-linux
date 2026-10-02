# UEFI:NTFS boot partition files

These files are the complete contents of upstream Rufus
`res/uefi/uefi-ntfs.img` (1048576 bytes, SHA-256
`72683fa1250eeea772d3399277b434d4e55ba8dd0dc926e52d817e701fc2eb9e`),
extracted unchanged with `mcopy -s -i uefi-ntfs.img ::/ .`.

Rufus Linux formats the 1 MiB `UEFI:NTFS` partition itself (FAT, label
`UEFI_NTFS`) and copies these files onto it, which does not need raw-device
access and therefore no administrator password.

Contents, per the upstream `res/uefi/readme.txt`:

- Secure Boot signed UEFI:NTFS 2.8 bootloaders (`EFI/Boot/boot*.efi`) from
  <https://github.com/pbatard/uefi-ntfs>, GPLv2 or later.
- Secure Boot signed NTFS drivers derived from ntfs-3g 1.9
  (`EFI/Rufus/ntfs_*.efi`), GPLv2 or later.
- exFAT (and ARM/RISC-V NTFS) drivers from EfiFs 1.12
  (<https://github.com/pbatard/efifs>), GPLv3.

Source code for all of the above is available from the linked projects.

```
2a991a37ddfccd8152b043c3cc507bf578708ffb9f8f4c84c72a919d6c4457e3  uefi-ntfs/EFI/Boot/bootaa64.efi
990acb5c432dcbc91f6b77f62a7578a20874f4ac636b64d0952c6c29ad1b92d9  uefi-ntfs/EFI/Boot/bootarm.efi
32f7c8cb505ce7b32f560a9c51fe6abe14361823a46cb1541039cb52164769c1  uefi-ntfs/EFI/Boot/bootia32.efi
f314d864e5d9e54a7b1e4d981d6cd9b6ef70a9ff55f7f0913c0b25e55fc13846  uefi-ntfs/EFI/Boot/bootriscv64.efi
5e22e6209ea557fce49cdbab7d06be4fc99e65d45c4fba01da928e763776bb94  uefi-ntfs/EFI/Boot/bootx64.efi
629e567847ba028cb6ba1f75af12b1ace2094a6b1e70cddbfe1a99a82cdd0511  uefi-ntfs/EFI/Rufus/exfat_aa64.efi
c53fc4e59a6b71be191830ae37b7096850abef7235902f96afb4ee5b26b7924f  uefi-ntfs/EFI/Rufus/exfat_arm.efi
2cf3e47edd53540c052c2620451e68e6fab2554b10b89dab9d578f3f7ba7816a  uefi-ntfs/EFI/Rufus/exfat_ia32.efi
ff036f92211e375d21658bd4fe16f7dc4c3efbb98ced93dd68abef590f2c2613  uefi-ntfs/EFI/Rufus/exfat_riscv64.efi
21a5969dcd7b6c149b1dc9408c591749ba9c62fb264e2852cc70061fe3defff6  uefi-ntfs/EFI/Rufus/exfat_x64.efi
887a7c62414fc1584e199fe43e12d134829a56f8d3a91db67cdddd5b98864b85  uefi-ntfs/EFI/Rufus/ntfs_aa64.efi
822cd007caa4bbacd692797e3cba9ec1f9e28b7be3eb30c61ffac4725bb5cc1e  uefi-ntfs/EFI/Rufus/ntfs_arm.efi
a5c02c3774c71620f4d6582495ee2d1c4df4f3cd6bd9986209f4b1f5a90933cf  uefi-ntfs/EFI/Rufus/ntfs_ia32.efi
54befd00ed303abf1ebe38904097336a052e2e82333e319d6ef0fdc3b8f24afc  uefi-ntfs/EFI/Rufus/ntfs_riscv64.efi
d77e7f1c317a42467d3f7ade7b3e0a20996b9bf541492fbc15d6245d8d46dcac  uefi-ntfs/EFI/Rufus/ntfs_x64.efi
b672afcb23e1d329652b84b717e1f5fb703d497ab5ef503fbc55db6786c3dffc  uefi-ntfs/README.txt
```
