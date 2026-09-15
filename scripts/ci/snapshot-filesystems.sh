#!/usr/bin/env bash
# Exercise /var/tmp only inside a private mount namespace.
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'Run this isolated test as root.' >&2; exit 1; }
binary=$(realpath "${1:?helper test executable required}")
[[ -f "$binary" && -x "$binary" ]]
for tool in unshare mount umount mkfs.btrfs strace; do command -v "$tool" >/dev/null; done
unshare --mount --propagation private bash -euo pipefail -c '
    scratch=$(mktemp -d /tmp/rufus-snapshot-fs.XXXXXXXX)
    scratch_mounted=0
    target_mounted=0
    cleanup() {
        if [[ "$target_mounted" == 1 ]]; then umount /var/tmp; fi
        if [[ "$scratch_mounted" == 1 ]]; then umount "$scratch"; fi
        rmdir "$scratch"
    }
    trap cleanup EXIT
    mount -t tmpfs -o size=768M,mode=0700,nosuid,nodev,noexec rufus-snapshot-storage "$scratch"
    scratch_mounted=1

    mount -t tmpfs -o size=4M,mode=0700,nosuid,nodev,noexec rufus-snapshot-small /var/tmp
    target_mounted=1
    status=0
    "$1" --exact source_snapshot::tests::protected_snapshot_is_stable_and_read_only \
      --ignored >"$scratch/low-space.log" 2>&1 || status=$?
    if [[ "$status" != 101 ]] || ! grep -Fq "not enough space" "$scratch/low-space.log"; then
        cat "$scratch/low-space.log"
        echo "Expected initial low-space refusal, got exit $status" >&2
        exit 1
    fi
    umount /var/tmp
    target_mounted=0
    echo "Verified refusal on an undersized private tmpfs"

    truncate -s 512M "$scratch/image.btrfs"
    mkfs.btrfs -q "$scratch/image.btrfs"
    mount -t btrfs -o loop,nosuid,nodev,noexec "$scratch/image.btrfs" /var/tmp
    target_mounted=1
    strace -f -e trace=ioctl -e raw=ioctl -o "$scratch/reflink.trace" \
      "$1" --exact source_snapshot::tests::protected_snapshot_is_stable_and_read_only --ignored
    if ! grep -Eq "ioctl\([^,]+, 0x40049409, [^)]+\) += 0$" "$scratch/reflink.trace"; then
        cat "$scratch/reflink.trace"
        echo "The protected snapshot did not prove a successful FICLONE" >&2
        exit 1
    fi
    echo "Verified protected source immutability using Btrfs FICLONE"
' isolated-snapshot-filesystems "$binary"
