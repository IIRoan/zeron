# Project sandboxes (Linux)

A sandboxed project runs its agents, app services, worktree setup commands,
project actions and terminals inside one container per project, built from a
devcontainer config you choose. That covers the main folder and every Zeron
worktree of it. Shared databases stay on this machine.

```sh
zeron sandbox enable ~/Documents/muute-monorepo \
  --config ~/Documents/muute-monorepo/.devcontainer/devcontainer.json \
  --host-port 5432 --host-port 6379 --publish 3000 --publish 6006 \
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
  with the `devcontainer` CLI and podman. The image itself must hold the agent
  CLIs (`claude`, `codex`) and the project's toolchain: Zeron runs no
  lifecycle commands (`onCreateCommand`, `postStartCommand`, …) and ignores
  the config's run settings (`mounts`, `runArgs`, `remoteEnv`, users).
- `--host-port`: a port on this machine (a database) that the container
  reaches on its own `localhost`. `--publish`: a container port (a dev server)
  this machine reaches on `localhost`. Nothing else crosses, including Zeron's
  IPC port.
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
- Edit the config and run `enable` again to rebuild. `disable` keeps the image
  and the home volume.

## From an agent (Zeron MCP)

The same, from a chat: `sandbox_status` lists every sandboxed project with its
container's state, whether its image is built, and its last build log.
`chat.inSandbox` says whether the calling chat runs in one. `sandbox_enable
{"config": "/abs/devcontainer.json", "hostPorts": […], "publishPorts": […],
"readOnly": […], "project"?}` builds and starts a sandbox for the chat's
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
