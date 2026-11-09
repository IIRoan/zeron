# Project sandboxes (Linux)

A sandboxed project runs its agents, app services, worktree setup commands,
project actions and terminals inside one container per project, built from a
devcontainer config you choose. That covers the main folder and every Zeron
worktree of it. Shared databases stay on this machine.

```sh
zeron sandbox enable ~/Documents/muute-monorepo \
  --config ~/.config/zeron/sandboxes/muute-monorepo/devcontainer.json \
  --host-port 5432 --host-port 6379 --publish 3000 --publish 6006 \
  --read-only compose.db-only.yaml --read-only bunfig.toml
zeron sandbox status
zeron sandbox disable ~/Documents/muute-monorepo
```

- `--config` is a devcontainer.json kept **outside** the project, so agents
  can't change it. `enable` builds its image with the `devcontainer` CLI and
  podman. Install the agent CLIs (`claude`, `codex`) and the project's
  toolchain in that image.
- `--host-port`: a port on this machine (a database) that the container
  reaches on its own `localhost`. `--publish`: a container port (a dev server)
  this machine reaches on `localhost`. Nothing else crosses, including Zeron's
  IPC port.
- `--read-only`: files in the project that this machine reads, such as a
  database service's compose file, or a `bunfig.toml` a readiness check loads.
  `.git/config` and `.git/hooks` (submodules' too) are always read-only.
- Agents log in once inside the container. Open a terminal for the project in
  Zeron (it opens in the container), then run `claude` and `codex login`. The
  logins live in the container's home volume and survive restarts.
- Edit the config and run `enable` again to rebuild. `disable` keeps the image
  and the home volume.

## What it guarantees

- Only `zeron sandbox` (run by you, on the host) writes the registry,
  `~/.zeron/sandboxes.json`. No RPC or MCP tool can change it.
- Membership is by path: the project folder and its worktrees folder.
- Nothing for a sandboxed checkout falls back to the host. If the container
  can't start, the run, service or terminal fails with the reason. Only Claude
  Code and Codex can run there; other providers are refused.
- The container runs with Zeron's own flags: rootless podman,
  `--userns=keep-id`, all capabilities dropped, `no-new-privileges`, and only
  the project, its worktrees, the helper and a read-only run folder mounted.
- Agents in a sandbox can only create and message chats in the same sandbox,
  and can't add or edit the project's databases, since those run on the host.
- Zeron's own host-side git for these checkouts is pinned to the project's git
  folder (`GIT_DIR`/`GIT_COMMON_DIR`), so a rewritten `.git` file or
  `commondir` can't make it run an agent's config. Command and skill discovery
  is skipped for them.

## Known limits

- All agents of a project share one container and one user, so they aren't
  isolated from each other.
- Plugins approved to run programs other than git (in the engine) still run
  them on the host, in the folders they name. The zgx beta interface's
  window-side plugin runner isn't pinned.
- A database service's own inputs must be read-only (`--read-only`) or live
  outside the project. Zeron can't know which files a script reads.
- Agents can't change `.git/config` from inside, so `git push -u` (which
  writes it) fails there.
- SELinux labels aren't applied (`label=disable`, for speed on large
  checkouts). Isolation comes from the user namespace and dropped
  capabilities.
