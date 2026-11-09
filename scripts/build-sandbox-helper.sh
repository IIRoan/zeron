#!/usr/bin/env bash
# Build the static musl `zeron-sandbox` helper (crates/sandbox), which runs on
# the host and inside any sandbox image. Prints the binary's path.
set -euo pipefail
cd "$(dirname "$0")/.."
out="$PWD/target/sandbox-helper"
mkdir -p "$out"
podman run --rm --security-opt label=disable \
  -v "$PWD/crates/sandbox:/src:ro" -v "$out:/out" -w /src \
  -e CARGO_TARGET_DIR=/out/build docker.io/library/rust:1-alpine \
  sh -c 'cargo build --release --locked && cp /out/build/release/zeron-sandbox /out/zeron-sandbox'
file "$out/zeron-sandbox" | grep -Eq 'static(-pie|ally) linked' || { echo "zeron-sandbox is not static" >&2; exit 1; }
echo "$out/zeron-sandbox"
