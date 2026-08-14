# AppImage packaging

The AppImage is a portable, unprivileged x86_64 desktop build for glibc-based
Linux distributions. It is built against glibc 2.28 and can run without FUSE
through AppImage's `--appimage-extract-and-run` fallback.

The artifact deliberately contains no privileged helper, polkit policy,
setuid file, partitioning tool, or filesystem formatter. Device discovery,
image inspection, option selection, checksums, and logs work immediately.
Writing and formatting remain disabled until the matching native Rufus Linux
package installs the root-owned helper and exact-path polkit policy. This keeps
user-controlled AppImage bytes outside the privileged execution boundary.

`stage-appdir.sh` creates the AppDir from an old-glibc release binary.
`verify-appimage.sh` extracts and audits the final artifact, including its
contents, permissions, RPATH, glibc floor, metadata, and headless version path.
The release workflow pins and verifies appimagetool and its type-2 runtime.

Alpine/musl, NixOS/non-FHS, headless systems, non-x86_64 CPUs, and desktops
without a compatible display stack are outside this artifact's support claim.
