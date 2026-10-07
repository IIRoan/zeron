#!/usr/bin/env bash
# Merge a stable upstream release into the Linux fork without rewriting history.
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { echo "$*" >&2; exit 1; }
[[ "$(uname -s)" == Linux ]] || fail "This updater requires Linux."
[[ $# -le 1 ]] || fail "Usage: scripts/update-upstream.sh [vX.Y.Z]"
git symbolic-ref --quiet HEAD >/dev/null || fail "Check out your fork branch first."
for state in MERGE_HEAD CHERRY_PICK_HEAD REVERT_HEAD rebase-merge rebase-apply; do
  [[ ! -e "$(git rev-parse --git-path "$state")" ]] || fail "Finish or abort the current Git operation before updating."
done
[[ -z "$(git status --porcelain)" ]] || fail "Commit or stash your changes before updating (including untracked files)."
git remote get-url upstream >/dev/null || fail "Missing upstream remote; see docs/LINUX_FORK.md."

release="${1:-}"
if [[ -z "$release" ]]; then
  command -v gh >/dev/null || fail "Install the GitHub CLI, or pass an explicit release tag."
  release="$(gh api repos/zeronsh/zeron/releases/latest --jq .tag_name)"
fi
[[ "$release" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "Expected a stable release tag such as v0.2.105."
git fetch upstream "refs/tags/$release:refs/tags/$release"
target="$(git rev-parse "refs/tags/$release^{commit}")"
if git merge-base --is-ancestor "$target" HEAD; then
  echo "$release is already included in this branch."
  exit 0
fi

backup="backup/pre-upstream-$release-$(date -u +%Y%m%dT%H%M%S)-$$"
git branch "$backup"
echo "Rollback point: $backup"
if ! git merge --no-ff --no-commit "$target"; then
  cat >&2 <<EOF
The merge needs attention. Your original version is saved in $backup.
Resolve the files shown by git status, then run:
  git add <resolved-files>
  scripts/check-linux-fork.sh && git commit
To cancel the merge: git merge --abort
EOF
  exit 1
fi

if ! scripts/check-linux-fork.sh; then
  echo "Checks failed. The merge remains uncommitted for repair; rerun scripts/check-linux-fork.sh, then git commit. To cancel: git merge --abort" >&2
  exit 1
fi
git commit -m "Merge upstream $release into Linux fork"
echo "Updated and checked $release. Publish when ready with: git push origin HEAD"
