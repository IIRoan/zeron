#!/usr/bin/env bash
# Source before Cargo on Linux. Keep development libraries local to this repo.
ZERON_DEV_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -x /home/linuxbrew/.linuxbrew/bin/pkg-config ]]; then
  export PATH="/home/linuxbrew/.linuxbrew/bin:$PATH"
  export PKG_CONFIG=/home/linuxbrew/.linuxbrew/bin/pkg-config
  export LD_LIBRARY_PATH="/home/linuxbrew/.linuxbrew/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  export LIBRARY_PATH="/home/linuxbrew/.linuxbrew/lib:/home/linuxbrew/.linuxbrew/lib/gcc/current${LIBRARY_PATH:+:$LIBRARY_PATH}"
fi
# Some immutable Linux hosts provide g++ through Homebrew without a c++ alias.
# Luau's bundled native runtime needs an explicit compiler on those hosts.
if [[ -z "${CXX:-}" ]] && ! command -v c++ >/dev/null 2>&1; then
  for zeron_cxx in g++ clang++; do
    if command -v "$zeron_cxx" >/dev/null 2>&1; then
      export CXX="$(command -v "$zeron_cxx")"
      break
    fi
  done
fi
if [[ -d "$ZERON_DEV_ROOT/target/linux-sysroot/usr/lib64/pkgconfig" ]]; then
  export PKG_CONFIG_PATH="$ZERON_DEV_ROOT/target/linux-sysroot/usr/lib64/pkgconfig:$ZERON_DEV_ROOT/target/linux-sysroot/usr/share/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
fi
