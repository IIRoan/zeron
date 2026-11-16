# Project sandboxes (Linux)

A sandboxed project runs its agents, app services, worktree setup commands,
project actions and terminals inside one container per project, built from a
devcontainer config you choose. That covers the main folder and every Zeron
worktree of it. Shared databases stay on this machine.

```sh
zeron sandbox enable ~/Documents/muute-monorepo \
  --config ~/Documents/muute-monorepo/.devcontainer/devcontainer.json \
  --host-port 5432 --host-port 6379 --publish 3000 --publish 3300-3399 \
  --read-only compose.devcontainer.yaml --read-only compose.db-only.yaml \
  --read-only bunfig.toml
zeron sandbox status
zeron sandbox disable ~/Documents/muute-monorepo
```

- `--config` is the project's own devcontainer.json, or one outside every
  sandboxed project. A config in the project has its folder mounted read-only
  (just the file for a top-level `.devcontainer.json`), and `enable` refuses
  it until the compose files or Dockerfile it names elsewhere in the project
  are `--read-only` paths, so agents can't change what the image is built
  from. Agents still edit those files in worktrees. `enable` builds the image
  with the `devcontainer` CLI and podman. The image itself must hold the
  project's toolchain: Zeron runs no lifecycle commands (`onCreateCommand`,
  `postStartCommand`, …) and ignores the config's run settings (`mounts`,
  `runArgs`, `remoteEnv`, users). The agent CLIs it may leave out: when it has
  no `claude` or `codex` of its own, agents there run this machine's (linked
  read-only into the project's run folder before each run, last on the login
  shells' `PATH`; this machine's Claude Code needs a glibc image).
- `--host-port`: a port on this machine (a database) that the container
  reaches on its own `localhost`. `--publish`: a container port (a dev server)
  this machine reaches on `localhost`; publish the ranges worktrees take their
  ports from too (`3300-3399`). Both take `a-b` ranges. Nothing else crosses,
  including Zeron's IPC port. A server a chat or service starts there shows as
  that chat's preview on its published port.
- `--read-only`: files in the project that this machine reads, such as a
  database service's compose file, or a `bunfig.toml` a readiness check loads.
  `.git/config` and `.git/hooks` (submodules' too) are always read-only.
- Agents use this machine's Claude Code and Codex logins. Zeron writes each
  CLI's own credential file into the container's home when a run starts, and
  every minute while the container runs, with the access token only: refresh
  tokens are single-use, so a copy that refreshed would sign this machine out.
  This machine stays the only refresher. A Claude login near expiry is
  refreshed first, under Claude Code's own lock; Codex's is refreshed by
  Codex runs here (its tokens last days). The files are written from inside
  the container, so nothing an agent plants in its home reaches this machine.
- Agents there have this machine's skills and Claude Code plugins:
  `~/.claude/skills`, `~/.agents/skills` and `~/.claude/plugins/cache` are
  mounted read-only at their own paths (agents read them, never change what
  agents here load), the container's `~/.claude/skills` and
  `~/.agents/skills` link to them (unless its home has its own), and each
  installed plugin that isn't turned off here loads with `--plugin-dir`. A
  skill that links outside those folders doesn't resolve there.
  `~/.codex/skills` isn't shared: Codex writes its own there.
- `--share-github` (off by default) hands agents this machine's GitHub login:
  `gh auth token` of its active account goes into the container's
  `~/.config/gh/hosts.yml` (for a `gh` in the image) and git's credential
  store, and git there reads GitHub's SSH remotes over HTTPS (no SSH key goes
  in). Agents can then do anything that account can, including push and
  editing workflows. Turning it off takes both files out again when the
  container is recreated (a login you made in the container yourself is left
  alone), but a token they copied keeps working until you revoke it
  (`gh auth refresh`, or on GitHub).
- The container's environment has `ZERON_SANDBOX=1`, for a project's own
  tooling to adapt (no ssh keys there, no container daemon).
- `DOCKER_HOST` there is a read-only Docker API Zeron serves (a socket in the
  run folder): it lists and inspects this project's containers on this
  machine (those compose started from its folders, such as its shared
  databases), so `docker compose ps` sees them running, from any checkout
  (Compose v2 there; podman-compose's labels are filled in). It refuses
  everything else: no `run`, `exec`, `logs` or changes, and no environment
  variables. So a test suite that uses the project's running database works;
  one that starts its own containers (testcontainers) doesn't.
- Agents' commits there are authored with this machine's `user.name` and
  `user.email`. With `--sign-commits` (off by default) they're signed too:
  git there runs the helper as its `gpg.program`, which hands what git signs
  to a socket in the run folder, and this machine signs it as its own git
  would: with its `user.signingkey` and `gpg.format` (gpg, gpgsm or an SSH
  key, through their agents; a pinentry may ask here). `enable` refuses the
  setting on a machine without a signing key. Only git's own signing call on
  a commit or tag object is signed, always with that key: agents never hold
  it, can't pick another, decrypt, or sign anything else. Verifying
  signatures (`git log --show-signature`) isn't relayed. Without it, commits
  there are unsigned, a repository with `commit.gpgsign` refuses them, and
  the socket is gone: turning the setting off closes it at once.
- Edit the config and run `enable` again to rebuild. `disable` keeps the image
  and the home volume.

## In the app

Project settings → Environment (project defaults) has a Sandbox group with
the same settings: devcontainer config, host ports, published ports,
read-only files, Share GitHub login and Sign commits. While it builds, the page shows the
step it's on and Show log opens the build's log, live; the sidebar shows the
build too until it ends (a failed one until its log is opened). Turn on / Apply and rebuild builds the image and starts the
container (minutes; it carries on if the page closes), and Turn off removes
it. Agents never reach these settings (`GetProjectSandbox` /
`SetProjectSandbox` are app-only).

The workbench's left rail has an **Environment** entry: its panel has a
Sandbox card with the container's name and state, its config and ports, and
buttons to open a terminal in the container (from another terminal:
`podman exec -it <container> bash`), rebuild the image and container with
the settings they have, and show the last build's log. See
[Windows VM](windows-vm.md#in-the-app).

## Git on this machine

Zeron's own git for a sandboxed checkout (Source Control, diffs, turn
snapshots, worktrees) never runs on this machine against what agents shape:
`.git` files, submodule git folders and their config, attributes.

- Local commands (status, diff, log, add, restore, reset, stash, submodules,
  …) run in a bubblewrap jail (`bwrap` is required): no network,
  a fresh `/tmp`, the system's files read-only, the project and its worktrees
  writable except the container's read-only paths, any index or object store
  the command names, and this machine's git config read-only. Nothing else of
  this machine: no home, no ssh or gpg agent, no inherited environment.
  Whatever an agent's config makes git run there reaches only what agents can
  already change. Submodules work in full.
- Commands that need this machine itself (commit, tag, merge, rebase, pull,
  push, fetch, `gh`) or write the project's `.git/config` (branch, config,
  switch, checkout, worktree) run here, pinned to the project's own git folder
  (read-only to agents), with submodule recursion off and hooks only from
  `.git/hooks`. They're refused inside a submodule.
- MCP servers reach agents in the container through the helper's relay
  (a unix socket in the run folder): Zeron's own tools work there.

## Worktrees

Worktrees are created and removed by Zeron (jailed git), and their setup and
cleanup commands run in the container, like everything else of the checkout.
Each is its own checkout and branch; the container sees new ones at once (the
worktrees folder is mounted).

## From an agent (Zeron MCP)

The same, from a chat: `sandbox_status` lists every sandboxed project with its
container's state, whether its image is built, and its last build log.
`chat.inSandbox` says whether the calling chat runs in one. `sandbox_enable
{"config": "/abs/devcontainer.json", "hostPorts": […], "publishPorts": […],
"readOnly": […], "shareGithub"?: true, "project"?}` builds and starts a sandbox for the chat's
project, or the named one; calling it again replaces the settings.
`sandbox_disable {"project"?}` removes one. A build can take minutes. If the
tool call times out, the build carries on, so check `sandbox_status`.

Changes come only from a chat with full access outside read-only/plan mode
that isn't in any sandbox, which could run `zeron sandbox` itself. A chat
inside a sandbox can read sandboxes but never set one up or change one, its
own included. The config and the files it builds from must be out of every
sandbox's reach: read-only in this project, or outside every sandboxed one
(never in a worktrees folder), so no sandbox's agents can shape an image.
Read-only paths must be relative and inside the project.

## What it guarantees

- Only `zeron sandbox` and the MCP tools above write the registry,
  `~/.zeron/sandboxes.json`, and only for callers already outside every
  sandbox with full access. Nothing in a sandbox can change it.
- Membership is by path: the project folder and its worktrees folder.
- Nothing for a sandboxed checkout falls back to the host. If the container
  can't start, the run, service or terminal fails with the reason. Only Claude
  Code and Codex can run there; other providers are refused.
- The container runs with Zeron's own flags: rootless podman,
  `--userns=keep-id` as this machine's user (whatever `USER` the image
  sets), all capabilities dropped, `no-new-privileges`, and only the
  project, its worktrees, the helper and a read-only run folder mounted.
- Agents in a sandbox can only create and message chats in the same sandbox,
  and can't add or edit the project's databases, since those run on the host.
- Zeron's own host-side git for these checkouts is pinned to the project's git
  folder (`GIT_DIR`/`GIT_COMMON_DIR`), so a rewritten `.git` file or
  `commondir` can't make it run an agent's config. Command and skill discovery
  is skipped for them.

## Known limits

- All agents of a project share one container and one user, so they aren't
  isolated from each other.
- Agents there can read the access tokens they're handed (as with any login
  of theirs), though never a refresh token.
- Plugins approved to run programs other than git (in the engine) still run
  them on the host, in the folders they name. The zgx beta interface's
  window-side plugin runner isn't pinned.
- A database service's own inputs must be read-only (`--read-only`) or live
  outside the project. Zeron can't know which files a script reads. The same
  goes for files a config's compose file pulls in (`include`, `build`): Zeron
  checks only the compose files and Dockerfile the config names.
- Agents can't change `.git/config` from inside, so `git push -u` (which
  writes it) fails there.
- SELinux labels aren't applied (`label=disable`, for speed on large
  checkouts). Isolation comes from the user namespace and dropped
  capabilities.

## With a Windows VM

A sandboxed project can also be connected to a [Windows VM](windows-vm.md).
The container's published ports are on this machine's `localhost`, so listing
them as the VM's `--to-vm` ports makes `localhost:3000` the same server in
the container, on this machine and in Windows. A VM port (`--from-vm`) reaches
the container once it is also one of the sandbox's host ports.
