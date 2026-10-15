# Source Control behavior

The Rust/GPUI implementation uses [VS Code's Git extension](https://github.com/microsoft/vscode/tree/20f57d8a036b9b85b49689cfeb848ec8ddf09533/extensions/git)
as a behavior reference. VS Code's TypeScript extension is not bundled or executed.

## Repository and index state

Merge, staged, and working changes are separate groups. Partially staged files
and initialized submodules retain independent indexes and commit drafts.
Selections and mutations stay within their repository and index side.

Commit reads the index. A commit with no staged changes requires confirmation
before staging; branch and HEAD checks reject stale requests. Hooks and signing
use the repository's Git configuration. Pending staging operations disable Commit
until real Git status reconciles the result.

Discard confirms before replacing working files. Staged edits and submodule
contents are preserved unless explicitly selected. Undo Discard requires matching
files, branch, and index state; recovery copies live in the repository's private
`zeron-discard` directory with bounded retention.

Conflicted files require explicit resolution. During rebase, Git's current side
is the upstream being rebased onto and its incoming side is the replayed commit.
Paths are passed literally, including spaces and wildcard characters.

## Branches and remote operations

Searchable commands cover checkout, pull, merge, rebase, stashes, and remotes.
Pull from merges a selected remote branch into the current branch without
changing its configured upstream. Remote checkout uses a local tracking branch.
Stash Apply/Pop leave restored edits unstaged unless restoring the index is
explicitly requested; a failed pop retains its stash.

Remote operations use existing Git remotes and credential helpers. Failures are
reported rather than replaced with optimistic success. Mutating requests verify
the captured checkout before acting.

## Validation

`scripts/check-linux-fork.sh` runs the Git engine and UI regressions, including
literal paths, partial staging, conflicts, submodules, remote branch operations,
and recovery behavior.
