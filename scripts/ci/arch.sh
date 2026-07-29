#!/usr/bin/env bash
set -euo pipefail

pacman -Syu --noconfirm --needed rust cargo pkgconf fontconfig freetype2 \
  libxkbcommon wayland libx11 libxcb dosfstools
cargo build --workspace --release --locked
cargo test --workspace --locked
