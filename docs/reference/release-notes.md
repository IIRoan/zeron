# Update summaries in the Linux fork

The desktop shows **What’s new** after starting an updated build. The modal
contains **This update** and **Previous release**, with upstream’s original
Markdown notes and a separate section for changes made in `IIRoan/zeron`.
Upstream notes may describe other platforms; they do not imply those features
are enabled in the Linux fork.

Open the summary again from the **Local/account menu → What’s new**, or search
for **release notes** or **changelog** in the command palette. **Show after
updates** controls automatic display. Closing the modal acknowledges the
current update even if you were viewing the previous release. The preference
and acknowledgement live in `release-notes-state.json` in the app’s data
directory, separate from project, worktree and service settings.

## Preparing an upstream update

`scripts/update-upstream.sh` captures release notes before its Linux checks and
merge commit. The manually triggered GitHub Action uses the same command; its
checked Git bundle contains the notes too. Importing that bundle keeps the
reviewed notes instead of regenerating them.

`scripts/snapshot-release-notes.py` creates `docs/releases/changelog.json` for
the exact merged upstream tag and its preceding version. Existing notes are
cached, and the updater lists new fork commits separately from upstream
commits. GitHub release notes are preferred. If the API is unavailable, it
uses fetched Git history and the modal labels that fallback. An explicit
`--offline` flag disables API access. The desktop reads the bundled catalog
without making a network request.

If note preparation also fails during a merge conflict, use the preparation
command printed by the updater after resolving the files, stage the catalog,
then rerun the checks.
`git merge --abort` restores the previous catalog along with the other files.

## Fork-only updates

The fork retains upstream’s package version to reduce merge conflicts. For an
update containing only fork changes, edit the current entry’s `fork_changes`
in `docs/releases/changelog.json` to describe the new behavior, and include
that file in the change. The modal uses a digest of the release version,
upstream commit, notes and fork changes rather than just the upstream version.
It therefore shows new fork notes even when the upstream version stays the
same. Publication-date changes and edits to older entries do not reopen it.
Downgrades are not announced automatically.

Validate with `python3 scripts/snapshot-release-notes.py --check` and
`scripts/check-linux-fork.sh`. Catalog validation also runs in the fork’s
Linux CI. Never replace this build with upstream binaries to obtain release
notes; the Linux launcher keeps the official automatic installer disabled.
