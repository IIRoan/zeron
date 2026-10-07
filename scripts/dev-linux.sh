#!/usr/bin/env bash
# Run this Linux fork alongside an installed Zeron, with independent local data.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname -s)" == Linux ]] || { echo "This development launcher requires Linux." >&2; exit 1; }

source scripts/linux-env.sh
if ! pkg-config --exists webkit2gtk-4.1 json-glib-1.0; then
  scripts/setup-linux-dev.sh
  source scripts/linux-env.sh
fi
export ZERON_DATA_DIR="${ZERON_DATA_DIR:-$PWD/target/linux-dev-data}"
export ZERON_IPC_PORT="${ZERON_IPC_PORT:-27700}"
export ZERON_AUTO_UPDATE=0
export ZERON_WINDOW_TITLE="${ZERON_WINDOW_TITLE:-Zeron Dev}"
# Debug builds retain large engine/UI futures on worker stacks.
export RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}"
export RUST_LOG="${RUST_LOG:-info,loro_internal=warn,loro=warn}"
cargo build --locked -p zeron
# The desktop uses the distribution's runtime libraries, like the installed app.
exec env -u LD_LIBRARY_PATH ./target/debug/zeron "$@"
