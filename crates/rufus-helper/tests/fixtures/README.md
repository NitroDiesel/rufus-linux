# VHDX journal fixture

`dirty-log.vhdx.bz2` is an unmodified copy of QEMU's
`tests/qemu-iotests/sample_images/iotest-dirtylog-10G-4M.vhdx.bz2` from commit
[`e78835b722eb26f5a56370166e99b69e9751ea2a`](https://github.com/qemu/qemu/commit/e78835b722eb26f5a56370166e99b69e9751ea2a).

- SHA-256: `f294ddc9a9ab2a621cee73d6ac30ea692a8864ebd14be211e795d4c5b400adbb`
- Compressed size: 4,490 bytes. Expanded container: 30 MiB. Virtual disk: 10 GiB.
- Creator: Jeff Cody, Red Hat. Copyright 2013 Red Hat, Inc.
- QEMU iotest 070 is licensed GPL-2.0-or-later. The accompanying license text
  is in `COPYING.QEMU`.

The upstream commit describes a QEMU-generated dynamic image with a pending
data-sector log entry, whose replay was checked against Hyper-V. This is test
data, not an operating-system image or a boot asset. The Rufus test verifies
read-only refusal and unchanged bytes, then repairs a separate disposable
control copy to show that replay makes the image readable. No selected source
is repaired by the product.

Tests read this local fixture without downloading anything. To reproduce it,
fetch the exact file from the linked commit and verify the digest above before
running the explicit provider integration test. Test-only `bzip2` is required.
