# AppImage packaging

The AppImage is a portable, unprivileged x86_64 desktop build for glibc-based
Linux distributions. It is built against glibc 2.28 and can run without FUSE
through AppImage's `--appimage-extract-and-run` fallback.

The artifact deliberately contains no privileged helper, polkit policy,
setuid file, partitioning tool, or filesystem formatter. Everything works
immediately: writing, formatting, and Windows installer media go through the
host's udisks2 daemon, which performs the privileged steps under its own polkit
policy. User-controlled AppImage bytes never run as root. Without udisks2,
destructive actions need the native package's root-owned helper instead.

The continuation branch links the helper library's read-only inspection API,
not the helper executable or authorization policy. Optional VHD/VHDX capacity
previews use host `/usr/bin/qemu-nbd` and `/usr/bin/nbdinfo` as the current user.
Those providers are not bundled; missing tools leave recognition available
and explain why disk size is unknown. Conversion writes additionally need host
`nbdcopy`.

`stage-appdir.sh` creates the AppDir from an old-glibc release binary.
`verify-appimage.sh` extracts and audits the final artifact, including its
contents, permissions, RPATH, glibc floor, metadata, and headless version path.
The release workflow pins and verifies appimagetool and its type-2 runtime.

Alpine/musl, NixOS/non-FHS, headless systems, non-x86_64 CPUs, and desktops
without a compatible display stack are outside this artifact's support claim.

The display stack is supplied by the host, including dynamically loaded
libraries that a `--version` loader check does not exercise. See the display
runtime table in [`docs/BUILDING.md`](../../docs/BUILDING.md) for package names.
The AppImage does not bundle those libraries. GUI startup is tested separately
from the cross-distribution `--version` checks.
