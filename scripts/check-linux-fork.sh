#!/usr/bin/env bash
# Compile and check the fork's Git, editor, and syntax behavior before publishing.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname -s)" == Linux ]] || { echo "These checks require Linux." >&2; exit 1; }
source scripts/linux-env.sh
export RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}"
# Check fork changes against the incoming release during a merge. Generated
# upstream files may contain whitespace that should not be rewritten by the fork.
check_base=HEAD
if git rev-parse --verify --quiet MERGE_HEAD >/dev/null; then
  check_base=MERGE_HEAD
fi
# (Generated iOS bindings keep the generator's own trailing spaces, which
# CI checks byte for byte: not this check's to flag.)
generated=(':!apps/ios/CoreFFI/zeron_coreFFI.h' ':!apps/ios/Zeron/Core/Generated')
git diff --check "$check_base" -- . "${generated[@]}"
git diff --cached --check "$check_base" -- . "${generated[@]}"
python3 scripts/tests/test_update_upstream.py
python3 scripts/tests/test_sync_zgx_fork.py
python3 scripts/tests/test_snapshot_release_notes.py
python3 scripts/snapshot-release-notes.py --check
python3 scripts/tests/test_fork_workflows.py
python3 scripts/check-fork-workflows.py
cargo test --locked -p zeron-engine checkout_ --lib -- --test-threads=1
cargo test --locked -p zeron-engine --test local_first --test project_terminals \
  --test terminal_shell_path --test worktree_environments --test worktree_on_run -- --test-threads=1
cargo test --locked -p zeron-syntax -p zeron-theme -- --test-threads=1
cargo build --locked -p zeron
# The app (apps/zgx with the fork's plugins): every upstream copy has a
# recorded base and is resolved, the What's new snapshot is current, and the
# fork plugins, their host gateway and the providers page pass their tests.
# Its zgx fork is a local repository, so a machine without it (CI) skips
# the build.
if [[ -d apps/zgx/plugins-fork ]]; then
  python3 scripts/sync-zgx-fork.py --check
  python3 apps/zgx/scripts/snapshot-fork-release-notes.py
  zgx_fork="$(sed -n 's|.*git = "file://\([^"]*\)".*|\1|p' apps/zgx/Cargo.toml | head -1)"
  if [[ -z "$zgx_fork" || -d "$zgx_fork" ]]; then
    (cd apps/zgx && cargo test --locked --release --features beta --bin zeron-zgx -- \
      plugins::fork_tests plugins::workbench agents:: --test-threads=1)
  else
    echo "Skipped the app's tests: its zgx fork ($zgx_fork) isn't on this machine."
  fi
fi
