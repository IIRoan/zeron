# Linux fork plugins

These independent zgx plugins carry the fork's customizations. Upstream's
own files stay as upstream ships them: `ui/`, `plugins-sdk/`,
`plugins-builtin/`, `plugins-dev/` and `docs/`.

| Plugin | Changes |
| --- | --- |
| `zeron-git` — Zeron Git (Fork) | The original fork's Git controls, grouped staging, branch/stash operations, discard recovery, and syntax/character highlighting. Includes its own diff components. |
| `project-services` — Project Services | The existing engine's service manager, output, service settings, shared databases, and checkout setup. Includes its own draft-safe settings sheet. |
| `project-services` additions (1.2) | Each app service's **Start after** (`dependsOn`: databases and other app services), and startup progress from the engine's `startups` ("Waiting for …"); Stop also ends a start. |
| `project-sandbox` — Project Sandbox | The project's devcontainer sandbox: its settings sheet (config, host and published ports, read-only files, GitHub login, commit signing; Turn on, Apply and rebuild, Turn off), and a tab with its container, what it reaches and publishes, build progress, the build log and a two-press Rebuild. |
| `windows-vm` — Windows VM | The project's Windows VM: its settings sheet (automatic ports, ports either way, services that start it, the viewer; Connect, Start, Stop, Reconnect ports, Disconnect), and a tab with the VM's screen (a click opens the live viewer), its health, ports, and a command to run in PowerShell or on the desktop. |
| `linux-workbench` — Linux Workbench | Screen overrides for the original fork layout: activity rail and full-height tools left, chats/projects right, and Services in the left tools panel. The rail opens Sandbox and Windows VM when they're on; each chat keeps its own files panel width and shell dock height. |

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

## Bundling and the beta

The demo and Linux beta bundle these folders as separate plugins. Settings -> Customize
-> Plugins enables or disables each one. Disabling Linux Workbench restores
the stock layout; Git and Services can also run in the stock interface.
The stock Git and GitHub plugins are disabled to avoid duplicate controls.

For a new release, `scripts/bundle-fork-plugins.py --resources <release>`
checks its bundle (the folder holding `ui/`, beside the app); add `--apply` to copy these separate plugin folders.
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

The Linux beta is installed separately as **Zeron Beta** (`zeron-beta`),
with application id `sh.zeron.beta`, engine port 27946, and its own profile
under `~/.local/share/zeron-beta/profile`. It uses the installed agent CLIs
and their normal login locations; it does not copy regular Zeron's chats,
settings or credentials, and contains no scripted demo provider.

Build `apps/zgx` with `--features beta` and `ZERON_BETA_VERSION` set, and
the engine from the same commit (`cargo build --release -p zeron --bin
zeron-engine`). `scripts/install-zgx-beta.py --frontend <binary>
--native-source <manifest> --engine <zeron-engine>` checks the package. Add
`--apply` to install it. The native-source manifest maps every
`apps/zgx/src/*.rs` path (recursively, relative to the repository root) to its
SHA-256 at build time. The engine's `--build-id` must name this checkout's
commit. A release is flat, as the Linux tarball is: the app with `ui/` and
`plugins-builtin/` beside it, where zgx looks for them. The installer preserves both regular Zeron and the demo and only
seeds stock Git/GitHub switches off for a new beta profile. Future installs
preserve the beta's plugin switches. About & updates identifies this local
beta and explains its separate manual updates. `zeron-beta --status` lists
only its processes; `zeron-beta --stop` stops only this beta.

An existing user's saved agent accounts and compatible preferences can be
transferred explicitly with `python3 scripts/migrate-zgx-beta-profile.py`.
It checks only unless `--apply` is supplied and requires the beta to be
stopped. It backs up overwritten beta files privately, merges account
identities while retaining newer credentials, translates UI preferences,
and copies service/worktree configuration without importing running
processes. Project registrations must be recreated through the beta's RPC
with the same project ids and its own device id. Regular Zeron's profile,
the beta's identity and its plugin switches remain separate.

After updating `docs/releases/changelog.json`, refresh the offline plugin
snapshot with `python3 apps/zgx/scripts/snapshot-fork-release-notes.py --apply`.
Without `--apply`, the command only checks that they match.
