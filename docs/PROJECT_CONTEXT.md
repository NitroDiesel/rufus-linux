# Project context for continuing agents

This is the durable handoff for agents continuing Rufus Linux. Read it before
planning changes, then follow the linked specialist documents for the branch
of work you are touching. Update this file when a release changes the baseline,
a safety invariant changes, or a capability moves between available, blocked,
and planned.

## Product identity

Rufus Linux is an independent, Linux-native port inspired by
[Rufus](https://github.com/pbatard/rufus). It is not produced, endorsed, or
supported by the upstream Rufus project. Preserve that disclaimer anywhere the
name and original public-domain Rufus icon could imply official affiliation.

The product target is a trustworthy USB device workbench for Arch Linux,
Debian/Ubuntu, and Fedora. Feature parity means implementing an upstream concept
safely on Linux; it does not mean exposing a control before its full operation
is implemented and verified.

## Current baseline: 0.1.2

Version 0.1.2 is the continuation baseline. Its source and packaging were
merged in [PR #6](https://github.com/NitroDiesel/rufus-linux/pull/6), and its
public installers belong in the
[v0.1.2 release](https://github.com/NitroDiesel/rufus-linux/releases/tag/v0.1.2).

Completed product work includes:

- a native Slint desktop application with aligned compact controls, light and
  dark themes, keyboard-operable custom buttons, true blocking modals, and a
  continuous Windows-style progress bar;
- automatic block-topology watching and USB hot-plug refresh, with target
  selection preserved by strict device identity and refresh deferred during a
  confirmation or active operation;
- safe formatting for MBR, GPT, and super-floppy layouts using installed FAT,
  FAT32, exFAT, NTFS, UDF, and ext2/3/4 providers;
- bounded raw, compressed-raw, and ISOHybrid writing, optional readback
  verification, checksums, bad-block testing, cancellation, flush, and clear
  terminal status reporting;
- a short-lived polkit-authorized helper with target revalidation, source file
  descriptor binding, symlink rejection, fixed-path tool allowlists, privilege
  dropping for decoders, managed child process groups, cooperative
  cancellation, and destination synchronization;
- native Debian, Fedora, and Arch packages containing the GUI, root-owned
  helper, exact-path polkit policy, desktop metadata, and required providers;
- a portable x86_64 AppImage built on a pinned glibc 2.28 baseline, with zsync
  metadata, extraction fallback, AppDir denylist checks, cross-distribution
  loader tests, and GitHub build provenance;
- the original upstream Rufus PNG icon set at 16 through 512 pixels. The icon
  files are public domain, courtesy of PC Unleashed; keep the attribution in
  `assets/icons/LICENSE.txt` and `THIRD_PARTY.md`.

The authoritative feature truth is
[`CAPABILITIES.md`](CAPABILITIES.md). Recognized but blocked flows include
ordinary non-hybrid ISO file-copy media, Windows installer media, Windows To
Go, VHD/VHDX conversion, FreeDOS, persistence, bootloader installation, Windows
11 customization, Secure Boot revocation checks, and drive capture. Keep these
disabled with a reason until an end-to-end implementation and its fixtures,
integration tests, and boot tests exist.

## Architecture map

The user-facing flow is:

```text
Slint UI (unprivileged user)
  -> Linux device/image inspection
  -> immutable operation plan and target-specific confirmation
  -> pkexec authorization
  -> one versioned NDJSON request
  -> root-owned one-operation helper
  -> structured progress, verification, flush, and exit
```

Repository ownership is divided as follows:

- `apps/rufus-linux/` owns the Slint UI, user-session state, hot-plug watcher,
  capability presentation, and helper client.
- `crates/rufus-core/` owns domain types, device identity, eligibility,
  operation planning, safety decisions, and progress types.
- `crates/rufus-linux-platform/` owns Linux sysfs, mount, swap, holder, and
  external-provider discovery.
- `crates/rufus-image/` owns bounded image recognition and analysis.
- `crates/rufus-helper-protocol/` owns the versioned and size-bounded NDJSON
  request/event contract.
- `crates/rufus-helper/` owns the narrow privileged executor and all destructive
  disk operations.
- `crates/rufus-boot/`, `rufus-downloads/`, and `rufus-i18n/` contain planned or
  partial supporting domains; their presence does not make a product flow
  available.
- `packaging/` owns distro integration, AppImage staging/auditing, icons,
  desktop metadata, and the polkit policy.
- `.github/workflows/ci.yml` is the normal quality gate;
  `.github/workflows/release.yml` builds and publishes all four installer
  formats.

Read [`ARCHITECTURE.md`](ARCHITECTURE.md) before moving responsibilities between
modules or changing the operation state model.

## Safety invariants

Read [`SAFETY.md`](SAFETY.md) before changing discovery, target selection,
authorization, helper protocol, destructive operations, cancellation, external
tools, downloads, or logs. The following are release boundaries, not optional
implementation details:

- The desktop stays unprivileged. Destructive work runs only through
  `/usr/bin/pkexec` and the root-owned `/usr/libexec/rufus-linux-helper`.
- The helper accepts a typed allowlisted operation, never a shell command or an
  arbitrary root-owned output path.
- Device paths are display names, not identity. Revalidate the stable path,
  major/minor, size, model, serial, topology, mounts, swap, and holders after
  authorization and immediately before writing.
- Bind and validate the source once, reject symlinks and source-on-target
  layouts, bound all decompression and writes, lock exclusively, and report
  success only after verification and flush complete.
- Cancellation terminates managed process groups cooperatively, reaps direct
  children, and synchronizes the destination before returning the incomplete
  media warning.
- Keep external tools on reviewed absolute paths with cleared environments.
  Keep permissive udev rules, setuid launchers, direct `sudo` fallbacks, and a
  root GUI outside the design.

### AppImage trust boundary

The AppImage is a portable **unprivileged frontend**, not a self-contained
privileged disk writer. It must not contain the helper, polkit policy,
formatters, partitioning tools, setuid files, glibc, or graphics drivers.
Discovery, inspection, checksums, option selection, and logs work portably.
Writing and formatting become available only when a matching native package
has installed the fixed-path, root-owned helper and policy.

The GUI checks helper and policy ownership, write permissions, exact policy
action/path, effective polkit registration, and helper version immediately
before launch. Preserve the fixed privileged identity; running bytes from an
AppImage mount or user-owned extraction directory through `pkexec` would break
the reviewed trust boundary.

Read [`packaging/appimage/README.md`](../packaging/appimage/README.md) before
changing AppImage contents or launch behavior.

## UI and product behavior

The visual direction is a compact “Device Workbench,” not a clone of the
Windows window chrome. Preserve these resolved decisions:

- no redundant in-content “Rufus Linux / Device Workbench” title block;
- a right-aligned, evenly spaced toolbar and a refresh control aligned with the
  device selector;
- normalized 36-pixel action buttons and 13-pixel action labels, with visible
  focus rings and Space/Enter activation;
- Log, About, and destructive confirmation overlays block all interaction with
  the workbench, support Escape, and give initial focus to the safe action;
- essential status text and badges share a centerline, and progress uses one
  continuous trough/fill rather than segmented blocks;
- the original Rufus icon is used for the window, desktop integration, native
  packages, and AppImage at its exact source sizes.

For UI work, use the `frontend-design` skill and a bounded UI review subagent
when the agent environment provides them. Verify both themes at the minimum
560x720 window and maximized size, including keyboard traversal and modal click
blocking. Keep the UI truthful: unavailable choices stay disabled or absent
with a concise reason.

## Known continuation points

These are known follow-ups, not claims that the current release is broken:

- Run a packaged, real-polkit smoke test on disposable physical USB media for
  formatting, raw writing, verification, cancellation, and disconnect during
  write. Automated tests and file/loop-backed tests do not replace this gate.
- Desktop theme startup currently uses `RUFUS_LINUX_THEME` or `GTK_THEME`.
  Portal-backed system theme detection and persistence of a manual
  system/light/dark preference remain unimplemented.
- `rufus-i18n` contains catalog groundwork but the Slint application still has
  hard-coded English strings. Wire one catalog source before claiming
  localization.
- FreeDOS and Windows To Go remain visible roadmap choices that resolve to a
  blocking explanation. If this interaction changes, decide deliberately
  between visible-disabled roadmap items and hiding unavailable modes, then
  test the chosen behavior.
- Filesystem choices are provider-driven. NTFS appears only when a supported
  `mkfs.ntfs`/`mkntfs` provider is installed; native packages install the
  provider, while the AppImage deliberately relies on the host and still needs
  the native helper for destructive formatting.
- The AppImage compatibility claim is x86_64 glibc 2.28+ desktop Linux with a
  FUSE extraction fallback. Alpine/musl, NixOS/non-FHS layouts, headless hosts,
  and systems without polkit are not covered by a universal “any distro” claim.

## Build, test, and release gates

Read [`BUILDING.md`](BUILDING.md) before changing dependencies, distro recipes,
runtime providers, install paths, or release artifacts.

Run the normal source gates from the repository root:

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --release --locked
git diff --check
```

Changes to desktop metadata or authorization also require:

```sh
desktop-file-validate packaging/desktop/io.github.nitrodiesel.rufus-linux.desktop
appstreamcli validate --no-net packaging/metainfo/io.github.nitrodiesel.rufus-linux.metainfo.xml
xmllint --noout packaging/polkit/io.github.nitrodiesel.rufus-linux.policy
```

The release workflow must produce one Debian package, RPM, Arch package,
AppImage, AppImage zsync file, and `SHA256SUMS`. The AppImage verifier must prove
its architecture, glibc 2.28 ceiling, dependency closure, lack of RPATH, update
information, metadata, executable permissions, unprivileged contents, direct
version launch, extraction fallback, Xvfb GUI startup, and loader compatibility
on Debian, Fedora, and Arch.

Release publication supports a `v*` tag or a matching `release/v<PACKAGE_VERSION>`
branch. The release branch path exists so an authenticated repository agent can
publish through the reviewed workflow without locally stored GitHub credentials.
The publish job creates the missing tag from the exact release commit, uploads
all installers, marks the release as full/latest, and attaches provenance.

Before bumping a release, find every version-bearing file with `rg` rather than
updating only Cargo metadata. At minimum, reconcile the workspace version,
lockfile, UI version, distro recipes/changelogs, AppStream release, build docs,
and `PACKAGE_VERSION`. After publication, download the public `SHA256SUMS` and
AppImage and verify them independently.

## Continuation workflow

1. Read this file and the specialist document for the requested branch of work.
2. Inspect `git status`, recent commits, open pull requests, and the public
   release before assuming the handoff state is unchanged.
3. Trace the current implementation and tests before editing. Treat the
   capability matrix as a claim to prove, not a backlog to infer from.
4. Keep each change inside the existing privilege and ownership boundaries.
5. Update tests and the relevant documentation in the same change. If feature
   availability changes, update `CAPABILITIES.md` and this baseline.
6. Run every applicable local gate, then require checks for the exact pull
   request head before merging.
7. For a release, verify the public assets and full-release status after the
   workflow completes; a successful build artifact alone is not publication.

Commit material Codex changes with the co-author trailer required by the root
`AGENTS.md` so GitHub records the Codex contribution.
