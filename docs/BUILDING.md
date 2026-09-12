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

The desktop pins a direct winit-backend dependency to the same version as Slint
to set the initial native window size before creating the OpenGL drawable. It
reuses the existing renderer dependencies. Keep the Rust initial dimensions and
Slint preferred dimensions together when changing the default window size.

The X11 startup regression requires Xvfb, xauth, xdotool, ImageMagick, Mesa, and
a session D-Bus launcher. Run it without a window manager on a private display:

```sh
timeout 90s xvfb-run -a -s '-screen 0 2200x1800x24' \
  dbus-run-session bash scripts/ci/desktop-render.sh \
  target/release/rufus-linux target/ui-validation/ci
```

It launches only the unprivileged GUI, captures light/dark frames at three
scale factors and sizes, and checks for unpainted startup gutters. It does not
write devices or replace keyboard, modal, real-desktop, or physical-media tests.

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

The in-development virtual disk integration test additionally needs `qemu-utils
libnbd-bin` on Debian/Ubuntu or `qemu-img libnbd` on Fedora/Arch. CI installs these
test providers explicitly. On the continuation branch, host `qemu-nbd` and
`nbdinfo` also enable optional read-only desktop capacity inspection; `nbdcopy`
is not needed for that preview. They are not yet native runtime dependencies
and their presence does not enable VHD/VHDX conversion. Run the desktop preview
test as a non-root user with `cargo test -p rufus-linux desktop_virtual_preview
--locked -- --ignored`. See
[`VIRTUAL_DISKS.md`](VIRTUAL_DISKS.md) for the remaining gates.

## Packaging metadata

The `packaging/` directory contains integration metadata and starter recipes:

- `desktop/` — freedesktop desktop entry;
- `metainfo/` — AppStream metadata;
- `polkit/` — authorization policy for the narrow helper;
- `tmpfiles/` — volatile root-owned runtime directory;
- `debian/`, `rpm/`, `arch/` — distribution recipes;
- `appimage/` — AppImage staging, verification, launcher, and security notes.

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

## AppImage

Build the AppImage GUI on the pinned Rocky Linux 8 baseline used by the release
workflow, then stage and verify it with:

```sh
packaging/appimage/stage-appdir.sh target/release/rufus-linux RufusLinux.AppDir
appimagetool --runtime-file runtime-x86_64 RufusLinux.AppDir rufus-linux.AppImage
packaging/appimage/verify-appimage.sh rufus-linux.AppImage 0.1.2 2.28
```

The release workflow supplies SHA-verified appimagetool and type-2 runtime
artifacts and embeds GitHub zsync update information. Do not stage the helper,
polkit policy, setuid files, partitioning tools, formatters, glibc, or graphics
drivers in the AppDir. The native package owns the privileged integration.
