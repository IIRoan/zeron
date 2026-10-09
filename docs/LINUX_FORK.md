# Maintaining the Linux fork

This fork lives at [IIRoan/zeron](https://github.com/IIRoan/zeron). `origin` is
the fork and `upstream` is [zeronsh/zeron](https://github.com/zeronsh/zeron).
Keep upstream history intact: merge releases into `main`, then push to your fork.
The original platform code stays available to make future merges easier; use
the Linux launcher for this custom desktop.

## Update to a new release

Commit or stash any local edits, then run:

```bash
scripts/update-upstream.sh
```

The updater uses the GitHub CLI to find the latest stable release, fetches its
tag from upstream, saves a backup branch, merges it, runs the Linux checks, and
commits the result. It never pushes automatically or replaces your dirty work.
You can also choose a release explicitly: `scripts/update-upstream.sh v0.2.105`.
Re-running it after a successful update is harmless.

The command also bundles the current and previous upstream release notes.
The desktop presents them in **What’s new**, with fork changes shown separately.
See [update summaries](reference/release-notes.md) for offline handling,
preferences and maintaining notes for fork-only updates.

If upstream and the fork changed the same code, Git leaves the merge open. Run
`git status`, resolve those files while keeping the fork behavior, stage the
resolved files, and finish with:

```bash
scripts/check-linux-fork.sh && git commit
```

`git merge --abort` cancels a pending merge. The printed `backup/pre-upstream-*`
branch also retains the version from before the update. Avoid force pushes and
wholesale "ours" conflict resolutions: they can silently drop upstream fixes.

After the checks pass, publish and run the new version:

```bash
git push origin main
scripts/dev-linux.sh
```

Close the previous **Zeron Dev** window first, answering any unsaved-file dialog.
The launcher builds locally, uses independent data in `target/linux-dev-data`
and IPC port `27700`, and disables the official binary updater. Updates to the
installed official app do not update this custom build.

## Manually update on GitHub

Open **Actions → Update Linux fork from upstream → Run workflow**, select
`main`, and optionally enter a release tag. This workflow has no schedule or
push trigger. It runs the same updater command on a temporary branch and provides
a Git bundle artifact only after the Linux checks and build pass. Apply its
checked result locally with the one command printed in the run summary:
`scripts/update-upstream.sh --from-run RUN_ID`. It never pushes or merges directly
to `main` on GitHub. If already up to date, it exits without creating an artifact.
Conflicts and failed checks produce diagnostic artifacts for manual repair.

Review the local result, push with `git push origin main`, and restart with
`scripts/dev-linux.sh`. The Action needs no additional secret or write deploy
key; its API token remains read-only. This also handles releases that change
workflow files, which GitHub's built-in Action token cannot push. Existing
upstream CI files are preserved with job guards that restrict them to
`zeronsh/zeron`. Their inherited workflows are disabled in `IIRoan/zeron`.
The fork enables its own Linux tests and the manual Linux updater. This avoids
automatic macOS/Windows builds, edge deployments, official releases, and
scheduled SDK updates in your fork.

**Linux fork tests** runs on relevant pushes to `main` and pull requests, and
can also be started manually. It runs the native Linux core and UI suites
directly, and builds the desktop. It uses
read-only repository permissions and has no publishing step. New commits cancel
older runs for the same branch. Documentation-only changes skip the builds.

Both the manual Action and the Linux check command validate workflow isolation.
If a future merge introduces an unguarded upstream job, the checks fail before
the updater commits or packages it. Add the same upstream repository guard to
the new job, rerun the checks, and finish the merge. Deployment configuration
still belongs to the official project; keep its jobs guarded when merging.

## Remote setup on another checkout

```bash
git remote set-url origin https://github.com/IIRoan/zeron.git
git remote add upstream https://github.com/zeronsh/zeron.git
git remote set-url --push upstream DISABLED
git config remote.pushDefault origin
git config user.name IIRoan
git config user.email 82589897+IIRoan@users.noreply.github.com
```

If `upstream` already exists, use `git remote set-url upstream` instead of adding
it. Authenticate pushes with the GitHub account that owns the fork. The local
updater only fetches upstream; it does not need permission to push there.
Commit identity is repository-local so the fork uses IIRoan rather than a work
account. Use a signing key registered for this identity if you sign commits.

Keep custom UI and Git behavior in the existing `shell/workbench`,
`source_control`, `checkout_changes`, `checkout_discard`, and `checkout_git`
modules where possible. Small integration points are easier to reconcile with
upstream releases than replacing entire shared files.
