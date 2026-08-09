# Building and packaging

## Source build

Minimum build dependencies:

- Rust stable toolchain (`cargo`, `rustc`)
- `pkg-config`
- Fontconfig, FreeType, libxkbcommon, and Wayland/X11 development files for Slint's native platform backend
- polkit and `pkexec` at runtime for destructive operations

```sh
cargo build
cargo test
cargo run
```

Use `cargo build --release` for packaging. Do not run the complete GUI with `sudo`; privilege belongs in the helper only.

## Runtime capability providers

Distribution installers include the filesystem formatters below so the complete format menu works immediately. Archive decoders remain optional. Provider detection still fails closed and shows a direct remedy if a tool is removed.

| Feature | Executables/libraries | Debian/Ubuntu | Fedora | Arch |
|---|---|---|---|---|
| Core device/partition | `parted`, `blockdev`, `umount`, `swapoff`, `udevadm` | `parted util-linux udev` | `parted util-linux systemd-udev` | `parted util-linux systemd` |
| FAT/FAT32 | `mkfs.fat` | `dosfstools` | `dosfstools` | `dosfstools` |
| exFAT | `mkfs.exfat` | `exfatprogs` | `exfatprogs` | `exfatprogs` |
| ext2/3/4 | `mke2fs` | `e2fsprogs` | `e2fsprogs` | `e2fsprogs` |
| NTFS | `mkfs.ntfs` / `mkntfs` | `ntfs-3g` | `ntfsprogs` | `ntfsprogs` |
| UDF | `mkudffs` | `udftools` | `udftools` | `udftools` |
| Archive formats | libarchive, xz, bzip2, zstd | `libarchive-tools xz-utils bzip2 zstd` | `libarchive xz bzip2 zstd` | `libarchive xz bzip2 zstd` |

Package names can change; verify them against the distribution release being targeted. A missing provider disables only its feature and displays the package/executable needed.

## Packaging metadata

The `packaging/` directory contains integration metadata and starter recipes:

- `desktop/` — freedesktop desktop entry;
- `metainfo/` — AppStream metadata;
- `polkit/` — authorization policy for the narrow helper;
- `tmpfiles/` — volatile root-owned runtime directory;
- `debian/`, `rpm/`, `arch/` — distribution recipes;
- `appimage/` — AppImage notes and launcher skeleton.

Recipes intentionally do not download proprietary boot or Windows assets. Release builders must be reproducible, use Cargo's locked dependencies, generate a software bill of materials, and preserve license texts.

## Install from a staged release build

The exact binaries depend on the final workspace layout. A conventional staged install is:

```sh
cargo build --release --locked
install -Dm0755 target/release/rufus-linux "$DESTDIR/usr/bin/rufus-linux"
install -Dm0755 target/release/rufus-linux-helper "$DESTDIR/usr/libexec/rufus-linux-helper"
install -Dm0644 packaging/desktop/io.github.nitrodiesel.rufus-linux.desktop \
  "$DESTDIR/usr/share/applications/io.github.nitrodiesel.rufus-linux.desktop"
install -Dm0644 packaging/metainfo/io.github.nitrodiesel.rufus-linux.metainfo.xml \
  "$DESTDIR/usr/share/metainfo/io.github.nitrodiesel.rufus-linux.metainfo.xml"
install -Dm0644 packaging/polkit/io.github.nitrodiesel.rufus-linux.policy \
  "$DESTDIR/usr/share/polkit-1/actions/io.github.nitrodiesel.rufus-linux.policy"
install -Dm0644 packaging/tmpfiles/rufus-linux.conf \
  "$DESTDIR/usr/lib/tmpfiles.d/rufus-linux.conf"
```

If the helper is not built, omit the helper, polkit policy, and tmpfiles rule. The desktop application must then remain in read-only/demo mode rather than attempting raw access itself.
