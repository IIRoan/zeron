# Worktree environments

Open the projects dropdown and click the project's settings icon. On the new-chat screen, you can also open **Current checkout / New worktree → Worktree settings…** for the selected project. Both shortcuts open settings without creating a session. The **Project defaults** page configures new worktrees and any existing checkout that has no override. Choose a worktree in the left list to see its setup state and customize that checkout. **Use project defaults** removes its override. Draft settings stay in the dialog while switching between worktrees; **Save settings** saves the selected page.

To remove a checkout, select it in the worktree list and choose **Remove worktree…**. The confirmation keeps its Git branch and committed work. Uncommitted changes block removal unless you explicitly select **Discard uncommitted changes**. Stop active agents and save or close unsaved editors first. Removal stops that checkout's services, saves its Follow env files, runs the saved cleanup/removal workflow, and refreshes the list. If the removed checkout was the active one, its Follow env files return to the main project folder. Setup that was waiting for the same checkout is skipped instead of recreating files in it. Worktree folders deleted outside Zeron drop out of the list. Sessions using the checkout are archived with their conversation history retained. The main project folder cannot be removed here. Unsaved settings drafts do not change the removal workflow; save them before removing a worktree.

## Branch names

Set a branch prefix such as `feature/`, or leave it empty. It applies to new worktree branches and their automatic title rename. Existing branches retain their names. Zeron records the branches it creates so deleting a worktree with a custom prefix does not delete an unrelated branch the user switched to.

## Environment files

Discovery inspects filenames in the main project, skips dependency and build folders, and never reads env values. Detected files default to **Follow**. Add more relative paths manually or use **Add detected env files**. Click a file's handling button to cycle through:

- **Follow:** the latest edits from the previous active checkout go to the next checkout, including returning to the main project. Until a worktree is activated, the main project folder counts as the active checkout, so edits made there carry into the first worktree. If the file is missing or unreadable in the checkout being left, Zeron keeps the last saved copy and still switches. Files with the same profile name share that private environment; different profile names stay separate. A replaced destination gets a private backup with recovery metadata.
- **Copy:** copy the main project's file only when the destination does not exist. Later source changes and retries never overwrite that checkout's edits.
- **Link:** explicitly share the main project's file through a symbolic link. Edits affect every checkout using that link. An existing unrelated destination is preserved and setup reports the conflict.
- **Skip:** leave the path to a custom workflow.

Missing sources, paths outside the checkout, and conflicting destination links are reported. Secrets, profiles, backups, and settings stay in the owning engine's private data directory, outside the repository. Env files and setup output are created with mode 0600. Backup metadata records the checkout, relative filename, profile, and timestamp beside the backed-up file.

Extra `NAME=value` variables apply to workflow commands, services, and newly opened worktree shell terminals, including terminals opened in a subfolder of the worktree. Tools and agents running in the worktree can load the copied or linked env files in their usual way. Variables are not injected into the agent provider process.

## Dependencies and commands

The **Dependencies** dropdown states what Zeron will do automatically. New projects default to **Copy node_modules automatically** when installed dependency folders are detected, or **Install dependencies per worktree** when there are none. Existing saved choices are preserved.

**Copy** creates independent copies of selected `node_modules` folders, preserving package-manager links and executable files. Existing folders are never overwritten. **Install** runs a separate install in the new checkout; Zeron detects Bun, pnpm, Yarn, or npm from lockfiles, or uses the configured install command. Without any lockfile, Zeron runs `npm install --no-package-lock`, so the install does not add a new `package-lock.json` to the branch. **Share** links selected folders to the project's dependencies. **Let my setup command handle dependencies** skips built-in dependency management. After changing dependencies on a branch, install inside that worktree to update its independent copy.

After env/dependency preparation, the custom setup command runs. Optional activation commands run when entering a different checkout; cleanup runs before deleting a worktree. Choose whether workflow commands run in the project folder or checkout. Each receives `ZERON_PROJECT_ROOT`, `ZERON_CHECKOUT_ROOT`, `ZERON_WORKTREE_PATH`, and compatible `CODEX_SOURCE_TREE_PATH` / `CODEX_WORKTREE_PATH` values. Install always runs in the checkout. Commands use the login shell (or `/bin/sh` when the login shell cannot run POSIX commands, such as csh, tcsh, or nu) and must finish within the configured timeout.

Workflow variables are actual shell environment variables, so quote them rather than inserting literal branch names or paths into command text:

| Variable | Value |
| --- | --- |
| `"$branchname"` / `"$ZERON_BRANCH_NAME"` | Current checkout branch, or the intended new branch during creation; empty for a detached checkout |
| `"$worktreename"` / `"$ZERON_WORKTREE_NAME"` | Worktree folder name |
| `"$worktreepath"` / `"$ZERON_CHECKOUT_ROOT"` | Absolute worktree path, including the intended path before creation |
| `"$projectroot"` / `"$ZERON_PROJECT_ROOT"` | Absolute main project path |
| `"$basebranch"` / `"$ZERON_BASE_BRANCH"` | Selected starting ref during creation; main project's current branch for later hooks |

**Custom creation and removal commands** optionally replace Zeron's Git commands. Leave them blank to keep standard creation/removal. These two commands always run in the main project folder. The sequence is create → prepare env/dependencies → setup, and cleanup → remove. For example:

```sh
git worktree add -b "$branchname" "$worktreepath" "$basebranch"
git worktree remove --force "$worktreepath"
```

Custom creation must produce a linked checkout in this project at the requested path on the requested branch. Removal must actually remove the requested checkout and its Git registration. Zeron checks these outcomes; a command that returns zero without doing the work is reported as an error. When custom creation fails or produces the wrong result, Zeron removes the Git worktree registration and the new branch it made, and the folder if it is empty. A non-empty folder that Git does not know about is left in place. Branch names are refreshed for later commands, so rename or switching to a user branch is respected. Timeout/cancellation stops workflow child processes.

Successful setup is idempotent for unchanged environment settings. Changing a branch prefix, command timeout, service command, or service policy does not reinstall dependencies. **Prepare worktree** or **Run setup again** stops that checkout's services and explicitly reruns setup; save edits first. A failed or interrupted setup remains visible, has bounded output, and can be retried or cancelled. Worktree creation associates the checkout with the conversation even when setup fails; agent dispatch and service starts wait for successful preparation. Agent dispatch prepares only worktrees Zeron created. A linked worktree you made yourself, for example with `git worktree add`, is not changed when an agent runs in it; starting its services or choosing **Prepare worktree** prepares it. This prevents retrying an agent from creating another failed checkout. Old setup Actions remain available as a compatibility fallback when no custom setup command is configured.

## Page layout

Project defaults follow a worktree's lifecycle: **Branch names**, **Shared databases**, **Environment files**, **Dependencies**, **Setup workflow**, **App services** and **Advanced**. **Save settings** saves the page's own settings and, on Project defaults, its databases; it never saves app services. **App services** lists the services that apply and opens the service editor. On a worktree page, **Customize for this worktree…** gives the worktree its own app services without freezing its other settings, and **Use project services** returns to the project's list. **Use project defaults** resets the worktree's environment settings and keeps its own app services. Unsaved drafts, including database edits, stay in the dialog while you switch pages and are never replaced by background refreshes.

If a window is connected to an older Zeron engine that is still running after an update, the databases section explains that Zeron must be quit completely and reopened, instead of saving databases the old engine would turn into ordinary services.

## Service groups

Choose the concurrency policy in the **Service groups** dropdown in project defaults. It applies to every checkout:

- **One active service group:** when switching conversations, stop and wait for the old checkout's process groups, prepare/activate the new checkout, then restart the previously running configured services there.
- **A group per worktree:** switching conversations leaves other worktree services running. Each checkout has its own PTYs and configured variables; allocate separate ports yourself or in a custom workflow. Use Copy or Link env files; Follow is rejected with parallel services.

Two conversations using the same checkout share its services. Only foreground selection or explicit Start/Restart activates an environment; status polling and opening settings do not move env files or services. Commands and crash-restart preferences can be overridden per checkout. Stop targets the selected checkout's services. Existing terminal crash detection and bounded restart backoff continue to apply.

To use another agent in the same environment, choose **New chat in this checkout** from a chat's context menu or the command palette. Select Codex, Claude Code, or another available agent in the model picker and send your first message. The new chat uses the exact same folder and device, with no copied history or provider session. The existing conversation stays available. The **Same checkout** label confirms that no new worktree or environment setup will run; services are shared by both conversations.
