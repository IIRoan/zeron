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
git diff --check "$check_base"
git diff --cached --check "$check_base"
python3 scripts/tests/test_update_upstream.py
python3 scripts/tests/test_fork_workflows.py
python3 scripts/check-fork-workflows.py
cargo test --locked -p zeron-engine checkout_ --lib -- --test-threads=1
cargo test --locked -p zeron-syntax --lib -- --test-threads=1
cargo test --locked -p zeron-ui source_control --lib -- --test-threads=1
cargo test --locked -p zeron-ui file_close_modal --lib -- --test-threads=1
cargo build --locked -p zeron
