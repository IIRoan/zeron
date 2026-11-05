# Fork history and zgx demo feature map

Audited against `b7218f40` and the merged upstream tag `v0.2.136`
(`35561ead`). `git log --no-merges HEAD --not v0.2.136` identifies all
34 fork-only commits reachable from this checkout. Upstream commits and
merge commits were checked through their resulting source, rather than
counted again as fork features. The current uncommitted plugin work is
included below as well.

The presentation is owned by three independently switchable plugins:
Linux Workbench, Project Services, and Zeron Git (Fork). The bundled stock
Git and GitHub plugins remain disabled. Stock zgx UI and built-in plugin
source directories are unchanged.

The latest layout choice supersedes the original bottom Services bar:
Explorer, search, Git, history, and **Services live in the left tools panel**.
Projects/chats stay on the right. Services has a vertical filterable list
and adjacent output, stacking vertically in narrow panels. Ordinary shell
terminals have their own optional dock; Services never occupies that dock.

## Feature inventory

| Fork behaviour | Demo implementation | Evidence and limits |
| --- | --- | --- |
| Tools left; project chats right; remembered widths | `linux-workbench/screens/{shell,page,sidebar,fork_layout,prefs}.luau` | Geometry and restart checks. Preferences use engine-owned plugin storage so new signed plugin paths retain them. |
| File hierarchy, full checkout path and breadcrumbs | `linux-workbench/screens/files.luau`, `files_pane.luau` | Nested files opened in the real UI; directory and clickable breadcrumbs retained. Wide tree is flush with the editor; narrow tree is an edge drawer. |
| File operations, editor tabs, save/conflict handling and unsaved close guard | Existing zgx workspace/editor, reached through the layout plugin | Reuses the engine workspace protocol; no second file store or editor. |
| Editor find; content search with case, word, regex, globs, folder and ignored-file controls | Existing zgx editor, a scoped file-viewer adapter, and `linux-workbench/screens/files_search.luau`; bounded `files.search_content` native bridge | Real files, invalid regex, ignored files, include/exclude/folder, and UTF-8 match navigation and focused-editor Ctrl+F checked. |
| Staged/unstaged/conflict groups, Ctrl/Shift selection, queued staging, partial files, submodules | `zeron-git/{selection,staging,workbench,fork_changes}.luau` | Real index and UI checks; failed queue and conflict-marker regressions. Repository/index sides stay separate. |
| Syntax and changed-character highlights, split/unified diffs, wrapping | `zeron-git/ui/*`, `src/plugins/highlight.rs` | Shared fork syntax parser, full-document context, UTF-8 regression checks. |
| All Git commands, history, stashes, remotes, recoverable discard and conflict actions | `zeron-git/{commands,command_items,grouped_menu,view}.luau`; existing checkout engine | 45 commands in grouped menus and searchable commands. Existing engine integration covered discard/undo, stashes and local bare remotes; no external GitHub mutation. |
| Recent/fuzzy/full-ref branch picker; create a missing branch | `zeron-git/{branch_choices,branches}.luau` | Real recent/all/fuzzy search and confirmed branch creation. Destructive actions retain captured HEAD, branch, repository and checkout. |
| Commit drafts per repository; confirmation before staging an empty index; commit/push sequencing | `zeron-git/workbench.luau` | Stable storage survives tool changes; index-only commit and empty-index cancellation checked. Regression checks that push receives the new commit's HEAD and later typing survives completion. |
| Services, independent Start/Stop/Restart, live PTYs, readiness and shared databases | `project-services/{init,model,view,editor}.luau`; existing service manager | Sidebar controls, clicked-service targeting, retained output, separate pending states. Stop app services keeps databases; explicit Stop all includes them. |
| Foreground checkout handoff; hidden panel keeps services running | `project-services/init.luau` | Selection/focus owns activation, independently of whether Services or shell output is visible. Polls and settings reads do not activate checkouts. Engine retains process-group, port handoff and crash retry rules. |
| Project defaults and checkout overrides, env handling, dependency policies, variables and lifecycle commands | `project-services/worktrees.luau` and existing worktree engine | Section navigation, drafts across pages, selected-page saves, setup/logs, service overrides/inheritance checked. App services save independently from environment settings. |
| Worktree removal preserves branch/history; rejects dirty checkout unless confirmed | `project-services/worktrees.luau`, native `plugins/workbench.rs` and existing engine | Dirty removal refused; clean removal kept branch. Host fixes project routing, never deletes the main checkout, and forces branch preservation/history archival. Unsaved editors guard removal. |
| New chat in the same checkout | Sidebar context menu and `fork.chat.same_checkout` command | Uses exact device/folder/branch, without parent history or a provider session. Agent/model selection stays the standard composer. |
| Persistent local project icons and reset | `linux-workbench/screens/{project_icons,project_look}.luau`; host project identity | Local managed images, no account/upload required. Picker cancellation/stale results do not retarget another project. Native file chooser and managed-copy persistence were checked in the isolated profile. |
| Bundled fork-aware What's new, previous release and opt-out | `linux-workbench/screens/{release_notes,release_data}.luau` | Offline catalog, separate fork/upstream sections, Local menu and command palette. Preferences and acknowledgement stay in plugin storage. |
| Background commands and quiet children show running in chat rows | `src/sessions.rs` plus shared engine/harness | Live task counts/lists and child activity use the normal working indicator, with heartbeat expiry. Focused native regressions passed. |
| Linked Claude skills, resume identity, quiet subagent lifecycle and completion | Shared `crates/harness` and `crates/engine` from the fork | Same engine binary/source; no duplicate provider implementation. Paid/provider conversations were not started for this UI validation. |
| Interactive login-shell PATH, checkout env and process lifetime | Shared engine terminals/services/worktree implementation | Inherited by both ordinary terminals and services. The frontend does not construct a second command runner. |
| Direct mobile pairing, source preservation and iOS build tooling | Shared engine and existing `apps/ios`, `crates/mobile`, scripts/workflows | Source inherited. No claim of a new iOS device build, pairing session or deployment from this Linux UI task. |
| Manual checked upstream updates, release catalog and CI guards | Existing fork scripts and workflows | Remain repository maintenance tools. Installing this demo does not run the GPUI updater or replace regular Zeron. |

## Every fork-only commit

| Commit | Added behaviour | Destination |
| --- | --- | --- |
| `e2d0a6fc` | Workbench and source control | Linux Workbench + Zeron Git; shared checkout/syntax engine |
| `69c31d6d` | Checked upstream updater | Existing repository maintenance scripts |
| `98bc5e88` | Manual update documentation | Existing maintenance workflow |
| `cd0f5f7d` | Manual fork automation boundaries | Existing workflow guards |
| `877c7c09` | Linux fork validation | Existing checks; demo additionally has focused native and UI checks |
| `f6be1bb4` | Remove automatic fork CI triggers | Existing workflow policy |
| `5550c2f4` | Services/worktrees/Git and history | Project Services + Zeron Git; shared engine |
| `85016cff` | Interactive login-shell PATH | Shared engine terminals/services |
| `0daead89` | Fork update summary and offline release notes | Linux Workbench release notes + existing snapshot/updater scripts |
| `a96fd94b` | Services/worktrees/Git/update-summary refinements | All three plugins and shared engine |
| `c9243a48` | One saved list for databases/app services | Shared service profile manager; independent plugin editors |
| `24bd98bc` | Consistent motion and hover | Native zgx design-system components and reduced-motion support |
| `26c855a5` | GPUI Linux drag-out/test fixes | Platform-specific; see differences below |
| `1f0fbbec` | Search, branch picker, icons, widths, sidebar/status polish | Linux Workbench + Zeron Git + sessions bridge/shared engine |
| `87edaebf` | Project settings redesign and draft/focus safety | Project Services settings navigation, native fields/sheets |
| `2ebfdb53` | Clicked-service context controls | Project Services per-row context menu |
| `2a06e8ea` | Claude forked skills as linked subagents | Shared harness/engine |
| `f95b36f7` | Avoid fresh-child history scans | Shared harness |
| `18591172` | Keep resumed skill on its original card | Shared harness/engine |
| `702195c9` | Completed agent vs background server lifecycle | Shared engine; zgx running sidebar bridge |
| `cf066992` | Preserve desktop source for direct mobile | Existing packaging/source retention and shared mobile support |
| `a5e49d76` | Direct mobile control | Shared engine/mobile source |
| `6ec151ff` | Offer missing-branch creation | Zeron Git branch picker confirmation |
| `6096e4ec` | Terminal OSC 52 clipboard copy | Native zgx supports writes; see policy difference below |
| `d46b76c9` | Cold-cache direct iOS tests | Existing iOS workflow |
| `3659c54a` | One-command iOS preview | Existing iOS script/workflow |
| `d05c6c9e` | Cold Rust cache build allowance | Existing iOS workflow |
| `d7b52431` | Alternate macOS runner pool | Existing iOS workflow |
| `4f1001ee` | Retry both macOS runner pools | Existing iOS script/workflow |
| `316d9a44` | Generated Swift bindings restored | Existing generated mobile bindings |
| `d6124181` | Directly paired phone skips Cloud onboarding | Existing iOS source |
| `faa9a4eb` | WPE helper packaging guard | GPUI Linux package workflow; zgx uses its own WebKitGTK view |
| `d673397e` | Background commands in sidebar running status | Native sessions bridge with focused regression |
| `b7218f40` | Browser MCP/CI compatibility | Shared engine/browser protocol and existing CI changes |

## Native framework differences and validation boundaries

This is a source/feature audit, not a claim that GPUI and zgx have identical
platform behaviour. The old GPUI drag-out-to-a-new-native-window patch does
not transplant into zgx's different window/split-chat implementation. Native
zgx provides OSC 52 clipboard writes and denies clipboard reads by default;
the GPUI-specific five-second input gate is not implemented by that pinned
zgx terminal backend. These are explicit remaining platform differences.

The focused native tests cover permission/routing boundaries, selected
checkout binding, branch-preserving removal, staging failure reconciliation,
syntax/UTF-8 and live background/child status. UI validation uses the native
inspect API against an isolated demo engine, with real files, Git indexes,
linked worktrees, commands and PTYs. Linux inspect does not provide pixel
snapshots, so layout/style verification uses geometry, native design-system
components, theme tokens and actual interactions. It does not establish
pixel-perfect rendering or exercise real provider accounts, a phone, or CI.

Verification receipts and the complete commit/path inventory are retained
under `target/zgx-history-audit/`. The installed release bundles the receipts,
source hashes, source patch and rollback metadata. No changes are committed
or pushed by this porting task.
