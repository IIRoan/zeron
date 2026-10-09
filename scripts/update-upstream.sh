#!/usr/bin/env bash
# Merge a stable upstream release into the Linux fork without rewriting history.
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { echo "$*" >&2; exit 1; }
[[ "$(uname -s)" == Linux ]] || fail "This updater requires Linux."
from_run=false
if [[ "${1:-}" == --from-run && $# == 2 && "$2" =~ ^[0-9]+$ ]]; then
  from_run=true
elif [[ $# -gt 1 || "${1:-}" == --* ]]; then
  fail "Usage: scripts/update-upstream.sh [vX.Y.Z | --from-run RUN_ID]"
fi
git symbolic-ref --quiet HEAD >/dev/null || fail "Check out your fork branch first."
for state in MERGE_HEAD CHERRY_PICK_HEAD REVERT_HEAD rebase-merge rebase-apply; do
  [[ ! -e "$(git rev-parse --git-path "$state")" ]] || fail "Finish or abort the current Git operation before updating."
done
[[ -z "$(git status --porcelain)" ]] || fail "Commit or stash your changes before updating (including untracked files)."
if $from_run; then
  command -v gh >/dev/null || fail "Install the GitHub CLI to download the checked update."
  metadata="$(gh api "repos/IIRoan/zeron/actions/runs/$2" --jq '[.conclusion,.event,.path,.head_branch] | @tsv')"
  read -r conclusion event workflow branch <<< "$metadata"
  [[ "$conclusion" == success && "$event" == workflow_dispatch && "$workflow" == .github/workflows/update-linux-fork.yml && "$branch" == main ]] \
    || fail "Expected a successful manual Linux fork update run on IIRoan/zeron main."
  bundle_dir="$(mktemp -d)"
  trap 'rm -rf -- "$bundle_dir"' EXIT
  gh run download "$2" --repo IIRoan/zeron --name linux-fork-update --dir "$bundle_dir"
  bundle="$bundle_dir/linux-fork-update.bundle"
  git bundle verify "$bundle"
  heads="$(git bundle list-heads "$bundle")"
  [[ "$heads" != *$'\n'* ]] || fail "Expected one checked update branch in the bundle."
  read -r target ref <<< "$heads"
  [[ "$ref" =~ ^refs/heads/maintenance/upstream-[0-9]+-[0-9]+$ ]] || fail "Unexpected update branch in the bundle."
  git fetch "$bundle" "$ref"
  target="$(git rev-parse FETCH_HEAD)"
  release="Action run $2"
  backup_label="run-$2"
else
  git remote get-url upstream >/dev/null || fail "Missing upstream remote; see docs/LINUX_FORK.md."
  release="${1:-}"
  if [[ -z "$release" ]]; then
    command -v gh >/dev/null || fail "Install the GitHub CLI, or pass an explicit release tag."
    release="$(gh api repos/zeronsh/zeron/releases/latest --jq .tag_name)"
  fi
  [[ "$release" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "Expected a stable release tag such as v0.2.105."
  git fetch upstream "refs/tags/$release:refs/tags/$release"
  target="$(git rev-parse "refs/tags/$release^{commit}")"
  backup_label="$release"
fi
if git merge-base --is-ancestor "$target" HEAD; then
  echo "$release is already included in this branch."
  exit 0
fi

backup="backup/pre-upstream-$backup_label-$(date -u +%Y%m%dT%H%M%S)-$$"
fork_head="$(git rev-parse HEAD)"
git branch "$backup"
echo "Rollback point: $backup"
merge_ok=true
if ! git merge --no-ff --no-commit "$target"; then
  merge_ok=false
fi
# Bundle notes even when the merge needs manual repair. GitHub outages use
# local upstream history; notes never depend on the official binary updater.
# Checked Action bundles already contain their reviewed changelog.
if ! $from_run; then
  if ! python3 scripts/snapshot-release-notes.py --tag "$release" --fork-head "$fork_head"; then
    echo "Resolve the merge, then prepare its release notes before running checks:" >&2
    echo "  python3 scripts/snapshot-release-notes.py --tag $release --fork-head $fork_head" >&2
    echo "  git add docs/releases/changelog.json" >&2
    echo "The merge remains uncommitted. To cancel: git merge --abort" >&2
    exit 1
  fi
  git add -- docs/releases/changelog.json
fi
if ! $merge_ok; then
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
