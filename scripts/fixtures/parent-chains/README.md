# Generate parent-chain fixtures

This optional .NET 8 tool generates VHD and VHDX differencing pairs containing
only synthetic byte patterns. Rufus and its normal CI do not require .NET or
DiscUtils. The compressed fixtures are committed separately and verified by
the Rust provider integration test.

```sh
dotnet restore scripts/fixtures/parent-chains/ParentChains.csproj --locked-mode
dotnet run --no-restore --project scripts/fixtures/parent-chains/ParentChains.csproj \
  -- target/new-parent-chains
bzip2 -9k target/new-parent-chains/parent.vhd target/new-parent-chains/child.vhd \
  target/new-parent-chains/parent.vhdx target/new-parent-chains/child.vhdx
```

The output directory must not already exist. Both disks have 1 GiB virtual
capacity; only the first 8 MiB contains test data. The generator checks that
reopening each child returns the expected inherited and overridden bytes in
that region, preserves capacity, and has a zero final byte. Its printed decoded
digest covers the first 8 MiB, not the entire virtual disk.

The pinned writer is `LTRData.DiscUtils.Vhd`/`Vhdx` 1.0.88, MIT-licensed,
from [commit f00254d](https://github.com/LTRData/DiscUtils/commit/f00254d88f8f119a99f6a36751baf0d5a7f26d65).
Package and transitive dependency content hashes are in `packages.lock.json`.
DiscUtils authors include Kenneth Bell, LordMike, and Olof Lagerkvist.

The older DiscUtils 0.16.13 VHDX differencing writer is unimplemented. The
maintained fork's 1.0.88 VHD writer also fails when its allocation table uses
the small stack buffer, returning a null array to the pool. The 1 GiB fixture
uses its heap-table path while keeping physical test data small. This is a
generator limitation, not a Rufus runtime workaround.

GUIDs, timestamps, and parent locator paths vary between runs. Regeneration is
not bit-for-bit reproduction. Recheck the pairs and deliberately update the
compressed fixture digests in the Rust tests when replacing them. No Windows
filesystem, boot sector, or operating-system payload is included.
