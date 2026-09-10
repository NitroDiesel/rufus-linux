#!/usr/bin/env bash
set -euo pipefail

dnf install -y cargo rust pkgconf-pkg-config fontconfig-devel freetype-devel \
  libxkbcommon-devel wayland-devel libX11-devel libxcb-devel dosfstools qemu-img libnbd
cargo build --workspace --release --locked
cargo test --workspace --locked
cargo test -p rufus-helper providers_roundtrip --locked -- --ignored
cargo test -p rufus-helper protected_snapshot --locked -- --ignored
