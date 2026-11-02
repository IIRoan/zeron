#!/usr/bin/env bash
# Build a signed iOS preview on GitHub's macOS runners and print the iPhone
# install link (~50 min). See docs/direct-mobile.md.
#
#   scripts/ios/build-preview.sh [git-ref]    (default: HEAD; committed code only)
#   IOS_RUNNER=macos-15 scripts/ios/build-preview.sh   # if macos-26 runners are busy
#
# Pushes the ref to a temporary `ios-preview` branch on IIRoan/zeron (no
# workflow fires on that push), runs "Direct instance iOS" with
# distribution=preview, waits, deletes the branch and prints the link.
set -euo pipefail

REPO=IIRoan/zeron
BRANCH=ios-preview
cd "$(dirname "$(readlink -f "$0")")/../.."
SHA=$(git rev-parse --verify "${1:-HEAD}^{commit}")
[ -z "$(git status --porcelain --untracked-files=no)" ] || echo "note: uncommitted changes are not included"
echo "Building $(git log -1 --format='%h %s' "$SHA")"

git push --force --quiet origin "$SHA:refs/heads/$BRANCH"
START=$(date -u -d '-2 min' +%FT%TZ)
OUT=$(gh workflow run ios-direct.yml -R "$REPO" --ref "$BRANCH" -f distribution=preview -f runner="${IOS_RUNNER:-macos-26}" 2>&1)
RUN=$(grep -o 'actions/runs/[0-9]*' <<<"$OUT" | grep -o '[0-9]*$' || true)
for _ in $(seq 30); do
  [ -n "$RUN" ] && break
  sleep 5
  RUN=$(gh run list -R "$REPO" -w ios-direct.yml -b "$BRANCH" -e workflow_dispatch -L 5 \
    --json databaseId,createdAt -q "[.[] | select(.createdAt >= \"$START\")][0].databaseId // empty")
done
[ -n "$RUN" ] || { echo "error: workflow run not found: $OUT" >&2; exit 1; }
echo "Run: https://github.com/$REPO/actions/runs/$RUN (takes ~50 min)"

ok=0
gh run watch "$RUN" -R "$REPO" --exit-status --interval 60 || ok=$?
git push --quiet origin --delete "$BRANCH" || true
[ "$ok" = 0 ] || { echo "error: build failed, see https://github.com/$REPO/actions/runs/$RUN" >&2; exit 1; }

PAGE=$(gh run view "$RUN" -R "$REPO" --log | grep -o 'https://expo\.dev/accounts/[^ ]*/builds/[0-9a-f-]*' | tail -1 || true)
[ -n "$PAGE" ] || { echo "error: no Expo link in the run log" >&2; exit 1; }
PROJECT=$(git show "$SHA:scripts/ios/expo/app.json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["expo"]["extra"]["eas"]["projectId"])')
MANIFEST="https://api.expo.dev/v2/projects/$PROJECT/builds/${PAGE##*/}/manifest.plist"
curl -fsS -o /dev/null "$MANIFEST" || { echo "error: install manifest not reachable: $MANIFEST" >&2; exit 1; }
ENC=${MANIFEST//:/%3A}; ENC=${ENC//\//%2F}

cat <<EOF

Zeron iOS preview is ready.
  Install (open on the iPhone, e.g. paste into Safari):
    itms-services://?action=download-manifest&url=$ENC
  Expo page (needs Expo login as iiroan):
    $PAGE
EOF
