#!/usr/bin/env bash
set -euo pipefail

apt-get update
apt-get install -y --no-install-recommends pkg-config libfontconfig1-dev \
  libfreetype-dev libxkbcommon-dev libwayland-dev libx11-dev libxcb1-dev \
  dosfstools
cargo build --workspace --release --locked
cargo test --workspace --locked
