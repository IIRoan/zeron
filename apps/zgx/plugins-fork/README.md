# Linux fork plugins

These independent zgx plugins carry the fork's customizations. Upstream's
own files stay as upstream ships them: `ui/`, `plugins-sdk/`,
`plugins-builtin/`, `plugins-dev/` and `docs/`.

| Plugin | Changes |
| --- | --- |
| `zeron-git` — Zeron Git (Fork) | The original fork's Git controls, grouped staging, branch/stash operations, discard recovery, and syntax/character highlighting. Includes its own diff components. |
| `project-services` — Project Services | The existing engine's service manager, output, service settings and shared databases (its own draft-safe sheet), and the project's worktree settings (project defaults and each worktree's: branch names, env files, dependencies, setup workflow, app services, advanced) as a section of the project's settings page. |
| `project-services` additions (1.2) | Each app service's **Start after** (`dependsOn`: databases and other app services), and startup progress from the engine's `startups` ("Waiting for …"); Stop also ends a start. |
| `project-sandbox` — Project Sandbox | The project's devcontainer sandbox: its section of the project's settings page (config, host and published ports, read-only files, GitHub login, commit signing; Turn on, Apply and rebuild, Turn off), and a tab with its container, what it reaches and publishes, build progress, the build log and a two-press Rebuild. |
| `windows-vm` — Windows VM | The project's Windows VM: its section of the project's settings page (automatic ports, ports either way, services that start it, the viewer; Connect, Start, Stop, Reconnect ports, Disconnect), and a tab with the VM's screen (a click opens the live viewer), its health, ports, and a command to run in PowerShell or on the desktop. |
| `linux-workbench` — Linux Workbench | Screen overrides for the original fork layout and look: activity rail and full-height tools left, chats/projects right, flush columns in the theme's own colours, the title bar's controls over the chat list, and Services in the left tools panel. The rail opens Sandbox and Windows VM when they're on; each chat keeps its own files panel width and shell dock height. Settings → Projects: a project's row (or its gear and menu in the chat list) opens its settings page, where the plugins put their sections (`slot:linux-workbench.project-environment`, `slot:linux-workbench.project-settings`, the project as `zeron.context("project")`); a plugin opens it with `zeron.open { settings = "project:<id>" }`. |

The engine does the work for every one of them (the services manager,
`crates/engine/src/sandbox.rs`, `crates/engine/src/windows_vm.rs`, the Git
implementation); agents use the same through MCP (`service_*`, `sandbox_*`,
`vm_*`). These plugins are the person's side of it.

## The SDK and the fork's addition

A plugin that uses the SDK carries `zeron.luau`, a byte-for-byte copy of
upstream's `plugins-sdk/zeron.luau`, and `fork_sdk.luau`, a copy of
`plugins-fork/fork_sdk.luau` (`src/plugins/fork_tests.rs` checks both):

```lua
local zeron = require("./fork_sdk")(require("./zeron")("my-plugin"), "my-plugin")
```

`fork_sdk.luau` adds `zeron.workbench(method, context, params)`, the
project tools gateway below, and `zeron.ports.parse`/`format` and
`zeron.words` for port and name lists written as the engine reads them.

## Motion

Each plugin also carries `fork_motion.luau`, a copy of
`plugins-fork/fork_motion.luau` (Linux Workbench's in its `ui/`; the same
test checks them). Whatever moves content uses it, so it moves as Zeron's
own lists and panels do (`@ds/motion`'s pace and ease, and Reduce motion):

- `fm.each(list, key, build, opts)`: a keyed list whose rows slide to new
  places, fade in and fade out (a staged file sliding to Staged). A row is
  built again as its item changes (a moving `ui.each` builds a row once).
  `fm.rows(opts)` gives `ui.each` the same motion when its rows read their
  items live.
- `fm.show(on, node, box?)`: `ui.show` whose room opens and closes as it
  fades. `fm.fade(on, node)`: a control that fades where it stands.
- `fm.swap(opts)`: `ui.presence` options for content replacing other
  content (the old fades, then the new).

## The project tools gateway

`zeron.workbench` calls one of a bounded list of engine methods through the
host (`src/plugins/workbench.rs`): no IPC token or general engine RPC is
lent. `context` names a known `session` or `project`; the host resolves the
device, folder and project from its own records, overwriting whatever the
plugin sent, and an optional captured `scope = { device, project, cwd }`
refuses a checkout switch while a form or menu from before is open. The
plugin's grants and content hash are checked on every call.

| Group | Methods | Needs |
| --- | --- | --- |
| Git | `GetCheckout*`, `SetCheckoutStaged`, `CommitCheckoutStaged`, `RunCheckoutGitAction`, discard and restore, `ListGitHistory`, `SearchGitHistory`, `ListRefs`, `HighlightGitDiff` (answered by the host) | `sessions.read`, `files.read`, `commands: ["git"]` |
| Services | `GetProjectTerminals`, `SaveProjectTerminals`, `ControlProjectTerminals`, worktree settings and setup, `DeleteWorktree` (branch kept), `ProjectServiceTerminal` (a configured service's own PTY only) | `sessions.read`, `files.read`, `commands: ["sh"]` |
| Project sandbox | `GetProjectSandbox`, `SetProjectSandbox` | the same as Services |
| Windows VM | `GetProjectWindowsVm`, `SetProjectWindowsVm`, `ControlProjectWindowsVm` (start, stop, view, diagnostics, screenshot, exec, launch; no raw input) | the same as Services |

The sandbox and the VM are project-wide: they're addressed by the project
alone. A VM screenshot comes back as a `host:` image the host decoded
(`plugin/<id>/windows-vm/<device>/<project>`), replaced in place by the
next one, since a plugin reads no file outside its folder.

## Bundling and installing

The demo and Linux beta bundle these folders as separate plugins. Settings -> Customize
-> Plugins enables or disables each one. Disabling Linux Workbench restores
the stock layout; Git and Services can also run in the stock interface.
The stock Git and GitHub plugins are disabled to avoid duplicate controls.

For a new release, `scripts/bundle-fork-plugins.py --resources <release>`
checks its bundle (the folder holding `ui/`, beside the app); add `--apply` to copy these separate plugin folders.
The bundler also includes the shared file-icon SVGs and their licence in
Zeron Git's folder, so its file icons can load inside the plugin's snapshot.
Keep the stock Git/GitHub plugin switches off. Their implementations remain
available for switching back.

The native host supplies a bounded project-tools SDK and shared syntax
parser. All custom presentation and controls live in these plugin folders.
No regular Zeron profile, project configuration, or provider login is copied
into the demo.

Git selection follows the original GPUI panel: Ctrl/Command toggles entries,
Shift extends a range, and row actions apply to the selected entries on the
same index/worktree side. Queued writes move ordinary files immediately,
keep their selection, and reconcile against Git before enabling other writes
after a failure. Conflicts require confirmation if markers remain.

Services opens from the left activity rail, alongside Explorer and Source
Control. Its filterable vertical list uses the app's normal selection,
status, buttons and context menus. Output is beside the list in wide panels
and below it in narrow panels. The chat has no Services dock. Ordinary shell
terminals have a separate optional bottom dock, toggled with Ctrl+J.
Project settings use section navigation beside the form; app-service settings
remain independent from environment and database settings.
Both tool plugins carry a private copy of the design-system context menu
with a larger invisible anchor, avoiding a degenerate Wayland positioner
when a popup opens near the bottom of the window.

Linux Workbench makes dropdown and menu surfaces opaque using the current
theme's solid menu colour, while retaining the window's glass setting. Git
and Services apply the same popup tokens in their own views so their menus
also remain opaque when Linux Workbench is disabled.

The Files pane uses a flat tree beside the editor when its own width permits.
Narrow panes use an edge-attached drawer sharing the Files toolbar, without
an inset card, rounded outer frame, or floating shadow. File operations and
editor state still use Zeron's existing workspace implementation.

The [complete porting map](PORTING.md) accounts for all 34 fork-only commits,
with individual feature destinations, validation boundaries and native
framework differences. Workspace search, recent/fuzzy branch checkout,
confirmed missing-branch creation, persistent per-repository commit drafts,
local project icons and offline release notes are included.

On Linux this app is **Zeron** itself (`zeron`, application id
`sh.zeron.app`): a window on the engine `zeron-host.service` runs on the
regular profile (`~/.zeron`, port 27654), which it never starts or manages.
It was installed separately as Zeron Beta, with a profile of its own, until
upstream retired the GPUI app in v0.3.20.

Build `apps/zgx` with `--features beta` (upstream's name for the Zeron app's
build: production's edge and sign-in), and from the same commit the engine
(`cargo build --release -p zeron --bin zeron-engine`), the phone gateway
(`-p zeron-rpc --bin zeron-remote`) and the sandbox helper.
`scripts/install-linux-fork.py --app <binary> --native-source <manifest>
--engine <zeron-engine> --remote <zeron-remote> --sandbox-helper <helper>`
checks the package; `--apply` installs it into
`~/.local/share/zeron-linux-fork/releases/`, switches `current`, and makes
`~/.local/bin/zeron` the fork's launcher (`scripts/launch-zeron.py`). The
native-source manifest maps every `apps/zgx/src/*.rs` path (recursively,
relative to the repository root) to its SHA-256 at build time. The engine's
`--build-id` must name this checkout's commit. A release is flat, as the
Linux tarball is: the app as `zeron`, with `ui/`, `plugins-builtin/` and
`zeron-engine` beside it; the app runs its engine commands (`zeron headless`,
`zeron mcp`) in `zeron-engine`. The installer seeds stock Git/GitHub switches
off only for a profile without decisions. About & updates says updates come
from the fork's installer. `zeron --status` lists this install's windows;
`zeron --stop` stops them.

After updating `docs/releases/changelog.json`, refresh the offline plugin
snapshot with `python3 apps/zgx/scripts/snapshot-fork-release-notes.py --apply`.
Without `--apply`, the command only checks that they match.
