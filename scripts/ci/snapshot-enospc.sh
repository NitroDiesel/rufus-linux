#!/usr/bin/env bash
# Fill only a private, bounded tmpfs; never a host filesystem or block device.
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'Run this isolated test as root.' >&2; exit 1; }
binary=$(realpath "${1:?helper test executable required}")
[[ -f "$binary" && -x "$binary" ]]
unshare --mount --propagation private bash -euo pipefail -c '
    scratch=$(mktemp -d /tmp/rufus-snapshot-enospc.XXXXXXXX)
    mounted=0
    cleanup() {
        if [[ "$mounted" == 1 ]]; then umount "$scratch"; fi
        rmdir "$scratch"
    }
    trap cleanup EXIT
    mount -t tmpfs -o size=272M,mode=0700,nosuid,nodev,noexec rufus-snapshot-test "$scratch"
    mounted=1
    RUFUS_SNAPSHOT_TEST_DIR="$scratch" "$1" --exact \
      source_snapshot::tests::snapshot_copy_reports_real_enospc_and_reclaims_space --ignored
' isolated-snapshot-test "$binary"
