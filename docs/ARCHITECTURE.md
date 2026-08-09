# Architecture

## Components

### Desktop application

The Slint application owns presentation, localization, device summaries, image analysis, option validation, operation planning, and progress rendering. It runs as the logged-in user and opens a native Wayland or X11 window through Slint's platform backend.

### Device service

The 0.1 device service reads `/sys/class/block`, `/sys/dev/block`, mountinfo, swap state and sysfs holder relationships. It records device ancestry rather than comparing only Linux major numbers. A lightweight watcher detects block-topology changes and coalesces automatic refreshes; manual refresh remains available.

### Image engine

The image engine identifies raw, ISO, ISOHybrid, WIM/ESD, VHD/VHDX and supported compressed containers. Analysis is read-only and bounded. Only raw, compressed-raw and ISOHybrid inputs currently produce an executable write plan; recognized container/deployment formats produce a blocking explanation.

### Operation planner

The planner converts a device snapshot, image report, and user options into an immutable sequence. It rejects incompatible combinations before privilege is requested. The helper validates the sequence again; desktop validation is never a security boundary.

### Privileged helper

The helper has a small allowlist: revalidate, unmount/swapoff, lock, test, partition, format, bounded disk-image write, verify and flush. External tools are selected from reviewed absolute paths, run with a cleared environment and never through a shell. The helper exchanges versioned NDJSON over standard I/O and exits after one operation.

## Dependency strategy

Prefer maintained system libraries for device identity and partition structures, and mature distribution tools for filesystems whose on-disk formats are complex. Capture tool versions in the log. Do not parse human-oriented localized output; request machine-readable output or use a library API.

Where exact output matters—MBR/PBR payloads, UEFI boot files, persistence configuration, WIM edits—keep fixture tests against known-good upstream media.

## UI state model

The main flow has four states:

1. **Choose** — select the physical device and source image.
2. **Configure** — compatible partition, target, filesystem, persistence, and advanced options.
3. **Confirm** — an immutable summary emphasizing the target identity and data-loss boundary.
4. **Write** — prepare, write, verify, and flush stages, with a safe cancellation path.

Options are capability-driven. Unsupported combinations are removed or disabled with a reason; they are never accepted and corrected silently after Start.

## Testing gates

- Unit tests for size arithmetic, partition layouts, device-graph exclusions, label validation, option compatibility, signed manifests, and decompression limits.
- Fixture tests for ISO/ISOHybrid/Windows/Linux image reports.
- Loop-device integration tests for every formatter and cancellation stage.
- QEMU tests using SeaBIOS and OVMF, with Secure Boot variants where licensed keys/assets permit.
- Hardware smoke tests covering USB flash, USB HDD/SSD opt-in, SD/MMC, 4Kn, >2 TiB, slow media, disconnect during write, and a busy filesystem.
- Packaging tests that verify the desktop/AppStream IDs, polkit action, helper path, runtime dependencies, and absence of permissive udev rules.
