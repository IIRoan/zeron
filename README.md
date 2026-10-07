# Zeron

Control your coding agents (Claude Code, Codex, Cursor, Devin, Grok, Hermes, Pi, Antigravity) locally by default, with optional multi-device sync.

*English | [简体中文](README.zh-CN.md) | [한국어](README.ko.md) | [日本語](README.ja.md)*

![Zeron desktop app](docs/media/readme/app-screenshot.jpg)

## Desktop app

Download the latest release for your platform from [GitHub Releases](https://github.com/zeronsh/zeron/releases/latest):

- **macOS** — `zeron-<version>-macos-arm64.dmg`
- **Windows** — `zeron-<version>-windows-x86_64-setup.exe`
- **Linux** — `zeron-<version>-linux-<arch>.tar.gz`, then run its `install.sh`

No account or network connection is needed; sessions stay on your device. The app updates itself.

## Headless (CLI)

For servers and other machines without a display, such as a VPS that keeps agents running after you close your laptop. Linux only:

```bash
curl -fsSL https://zeron.sh/install.sh | sh
zeron status
```

The installer starts the engine as a background service that survives reboots.

```bash
zeron status      # local/synced mode and engine status
zeron update      # update to the latest release
zeron daemon start|stop|restart|status
```

## Multi-device sync (optional)

Sign in to start an agent on one device and follow or drive it from another:

```bash
zeron daemon stop
zeron login        # or: zeron logout to return to local-only
zeron daemon start
```

Devices signed in to the same account can read and write each other's workspace files, so only sign in devices you trust. Existing local sessions are never uploaded.

## Sponsors

Thank you to [The Context Company](https://www.thecontextcompany.com/) for sponsoring Zeron. You can help fund Zeron's development too by [becoming a sponsor on GitHub](https://github.com/sponsors/zeronsh).

---

Developing or curious how it works? [Ask DeepWiki](https://deepwiki.com/zeronsh/zeron) or check out [ARCHITECTURE.md](ARCHITECTURE.md).

This Linux fork includes a source-control file list with per-file and per-repository
staging, separate staged/unstaged diffs, and submodule repositories. Click a file
to open its side-by-side diff in the main area; close the diff to return to chat.
Shift+click extends the selection through a range of files; Ctrl+click toggles
individual files. Use **+** or **−** on a selected row, or the selected-changes
command in **…**, to stage or unstage the selection together. Selections stay
within one repository and its staged or unstaged group.
Clicking outside Source Control clears its file selection and leaves the open
diff visible.
The Linux activity bar switches between Explorer, Source Control, Browser,
Terminal, History, and the view picker on the left. Projects and settings sit on
the right. Explorer shares the tool tab strip and keeps its file tree when its
tab is closed and reopened. Explorer files open in the center editor. Dark+ and
Light+ are the Linux defaults, with TypeScript syntax colors, bracket colors, and changed-text
highlights in split and unified diffs.

Enter a message at the top of Source Control and click **Commit** (or press
Ctrl+Enter) to commit the staged changes in the selected repository. Submodules
have their own commit target and message; Git hooks and signing settings apply.
Click a repository or its files in the list to choose the commit target.
The header's **Refresh** button immediately reloads file status and Git details,
spins while loading, and returns to its normal icon when loading finishes.

Source Control shows the selected branch and incoming/down / outgoing/up commit
counts against its last fetched upstream. The **…** dropdown contains **Fetch**,
**Pull**, **Push**, **Sync**, **Publish**, branch, stash, and remote actions. The
split **Commit** dropdown offers **Commit Staged**, **Commit & Push**,
**Commit & Sync**, amend, and undo. Remote actions use your existing Git remotes
and credential helpers. Pull honors your Git configuration; the **…** menu also
offers explicit merge/rebase pulls.
Git dropdowns use the available window height and scroll when their commands
do not fit.
The branch picker supports create, switch, rename, merge, rebase, and deletion of
merged branches. **…** also contains stashes, remote management, amend, undo last
local commit, and incoming/outgoing/recent history. Click a commit to view its
changes in the center; recent commits can be reverted with a new commit.

The curved-arrow action beside an unstaged file or **Changes** group, and
**Discard All**, request confirmation before restoring unstaged edits from the
index. Staged edits and submodule contents are kept. Select a submodule to handle
its changes separately. Discarding staged edits as well is an explicit **…**
action. **Undo Discard** restores the selection while the files, branch, and
staging still match its post-discard state. Recovery copies stay in the selected
repository's Git directory under `zeron-discard`; each recovery directory is
private to your user. During merge/rebase conflicts, select a file to open it or
accept the current/incoming version, stage the resolution, then continue or abort.

Run `scripts/dev-linux.sh` to build and launch a development instance. It uses
`target/linux-dev-data` and IPC port `27700`, independently of an installed Zeron,
and disables automatic binary replacement. Click **Source Control** in the left
activity bar to show changes; `Ctrl+R` also toggles the tool panel.
On this Homebrew-equipped Linux host
the launcher configures the native library paths automatically. See
[CONTRIBUTORS.md](CONTRIBUTORS.md#setup) for system dependencies on other hosts.

Licensed under the [MIT License](LICENSE).
