# Source Control behavior

This Linux fork uses Microsoft VS Code's source as a behavior reference, pinned
to commit `20f57d8a036b9b85b49689cfeb848ec8ddf09533`. The implementation is Rust/GPUI;
VS Code's TypeScript extension is not bundled or executed.

References:

- [Git commands](https://github.com/microsoft/vscode/blob/20f57d8a036b9b85b49689cfeb848ec8ddf09533/extensions/git/src/commands.ts): stage, unstage, clean, smart commit and undo commit.
- [Repository groups](https://github.com/microsoft/vscode/blob/20f57d8a036b9b85b49689cfeb848ec8ddf09533/extensions/git/src/repository.ts): Merge Changes, Staged Changes and Changes.
- [SCM resource interactions](https://github.com/microsoft/vscode/blob/20f57d8a036b9b85b49689cfeb848ec8ddf09533/src/vs/workbench/contrib/scm/browser/scmViewPane.ts): selection, context actions, navigation and sorting.
- [Primary action button](https://github.com/microsoft/vscode/blob/20f57d8a036b9b85b49689cfeb848ec8ddf09533/extensions/git/src/actionButton.ts): Commit, Continue, Publish Branch and Sync Changes.
- [Git operations](https://github.com/microsoft/vscode/blob/20f57d8a036b9b85b49689cfeb848ec8ddf09533/extensions/git/src/git.ts): applying stashes without `--index` by default.

## Supported interactions

- Separate merge, staged and working groups, including partially staged files
  and initialized submodules with independent indexes and commit drafts.
- Click to view a diff; Open File opens the existing working file. Deleted
  files and parent gitlinks do not offer an invalid editor action.
- Shift selects a range, Ctrl toggles resources, and Ctrl+A selects this
  repository's visible resources. Mixed staged/working selections are allowed;
  a stage or unstage action filters the appropriate index side. Clicking an
  unselected row's action affects only that row.
- Arrow keys, Home/End, Shift+arrow selection, Enter, Delete and Escape work
  while the resource list has focus. Shift+F10 opens the focused row's context
  menu. Text-input shortcuts remain local to the commit input.
- File context menus expose Open Changes, Open File, Stage/Unstage, Discard,
  Copy Path and Copy Relative Path. Conflicted files offer current/incoming
  version choices. Paths are passed literally to Git, including spaces and
  wildcard characters.
- Path, file-name and status sorting are available under More Actions.
- Commit uses the index only. When nothing is staged, a message and Commit
  open a Stage & Commit confirmation. Cancelling preserves the index and draft.
  Captured branch/head checks reject a changed checkout before staging.
- Merge conflicts block committing. Staging files containing unresolved
  conflict markers requires explicit confirmation; a batch is checked before
  any of its files are staged.
- Commit & Push publishes an unpublished branch, using its configured push
  destination or a remote chooser. A clean checkout offers Publish Branch or
  Sync Changes as its main action. The footer offers branch selection and sync.
- Fetch, pull (configured/merge/rebase), push, sync, branch management,
  merge/rebase continue/abort, commit history, amend, undo, cherry-pick, revert,
  stash and remote management use real Git and report failures.
- Stash Apply/Pop leave restored changes unstaged by default. Apply & Restore
  Staging requests `--index`. A failed pop retains its stash.
- Stash commands appear in a compact menu. Apply/Pop offer latest-stash
  shortcuts; choosing a particular stash opens a searchable quick picker.
  Stashes load asynchronously without branch/history scans. Only visible
  rows render, and descriptions are bounded before native text layout.
  Opening More Actions reuses metadata; Refresh reloads it explicitly.
- Undoing an initial commit keeps the files and restores an unborn branch;
  undoing a later commit keeps its changes staged. Both retain recovery refs
  and restore the commit message.

## Fork choices and limits

Git feedback uses window-local toasts, with no inline notices moving the list.
Refresh uses its existing spinner and diagnostic feedback, without a toast.
Discard uses the application's modal and recovery backup, not the system trash.
Undo refuses a commit known to be published. Destructive parent actions do not
discard submodule contents or unresolved merge resources.

This is parity for the supported Source Control operations, not VS Code's
entire Git extension. Tree view, staging selected hunks/lines, the three-way
merge editor, extension settings and cross-repository multi-selection are not
implemented here. Sorting is retained while the panel lives. Commits use saved
on-disk contents; unsaved editor buffers are not automatically staged.

## Validation

`cargo test -p zeron-engine --lib checkout_` exercises temporary repositories,
local bare remotes, submodules, conflict handling, literal paths, hooks,
stashes, publication and initial commits. `cargo test -p zeron-ui --lib
source_control::` exercises selection, focus, menus and confirmation modals.
The `source-control-fixture` feature provides a native Shell with a real engine
and isolated repositories for end-to-end UI verification.
