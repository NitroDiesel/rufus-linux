#!/usr/bin/env bash
set -euo pipefail

dnf install -y cargo rust pkgconf-pkg-config fontconfig-devel freetype-devel \
  libxkbcommon-devel wayland-devel libX11-devel libxcb-devel dosfstools
cargo build --workspace --release --locked
cargo test --workspace --locked
