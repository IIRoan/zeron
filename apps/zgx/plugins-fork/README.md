# Linux fork plugins

These independent zgx plugins carry the fork's customizations. The stock
`plugins-builtin/git`, `plugins-builtin/github`, and `ui` stay unchanged.

| Plugin | Changes |
| --- | --- |
| `zeron-git` — Zeron Git (Fork) | The original fork's Git controls, grouped staging, branch/stash operations, discard recovery, and syntax/character highlighting. Includes its own diff components. |
| `project-services` — Project Services | The existing engine's service manager, output, service settings, shared databases, and checkout setup. Includes its own draft-safe settings sheet. |
| `linux-workbench` — Linux Workbench | Screen overrides for the original fork layout: activity rail and full-height tools left, chats/projects right, and Services in the left tools panel. |

The demo and Linux beta bundle these folders as separate plugins. Settings -> Customize
-> Plugins enables or disables each one. Disabling Linux Workbench restores
the stock layout; Git and Services can also run in the stock interface.
The stock Git and GitHub plugins are disabled to avoid duplicate controls.

For a new release, `scripts/bundle-fork-plugins.py --resources <Resources>`
checks its bundle; add `--apply` to copy these separate plugin folders.
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

Build `apps/zgx` with `--features beta` and `ZERON_BETA_VERSION` set.
`scripts/install-zgx-beta.py --frontend <binary> --native-source <manifest>
--engine-release <verified-release>` checks the package. Add `--apply` to
install it. The native-source manifest maps every `apps/zgx/src/*.rs` path
(recursively, relative to the repository root) to its SHA-256 at build time.
The existing engine release must include its `build.json` source/checksum
receipt. The installer preserves both regular Zeron and the demo and only
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
