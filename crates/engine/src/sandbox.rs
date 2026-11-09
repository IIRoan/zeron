//! Project sandboxes (Linux, rootless podman). A project registered with
//! `zeron sandbox enable` runs its agents (Claude Code and Codex), app
//! services, workflow commands, project actions and terminals in one
//! container. The image is built from a devcontainer config the person
//! chose; Zeron runs it with its own fixed flags, so the config can't loosen
//! them.
//!
//! - The registry (`<data dir>/sandboxes.json`) is written only by that
//!   command. No RPC or MCP tool edits it, so nothing an agent reaches can
//!   turn its sandbox off.
//! - Membership is by path (the project's folder or its worktrees folder),
//!   never by a `.git` file an agent could rewrite.
//! - A process for a sandboxed checkout starts in the container or fails; it
//!   never falls back to the host. Shared databases are the one host-side
//!   exception (`Terminals::open_host_service`), and agents can't edit them.
//! - What the host itself reads from the checkout is out of agents' reach:
//!   `.git` config and hooks (and any `readOnly` paths) are mounted read-only,
//!   and host git is pinned to the project's own git folder ([`git_env`]).
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeron_proto::sandbox::{GUARD, MCP_ENV, SPEC_ENV, Spec, mcp_record, mcp_records};
pub use zeron_proto::sandbox::{parse_ports, port_ranges};
use zeron_proto::{HarnessId, RunRequest};

use crate::EngineError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sandbox {
    /// The project's main folder.
    pub root: PathBuf,
    /// The folder Zeron creates this project's worktrees in.
    pub worktrees: PathBuf,
    /// The devcontainer.json the image is built from: outside every
    /// sandboxed project, or in this one's main folder (its folder is then
    /// mounted read-only).
    pub config: PathBuf,
    /// Host ports the container reaches on its own localhost (databases).
    #[serde(default)]
    pub host_ports: Vec<u16>,
    /// Container ports the host reaches on its localhost (dev servers).
    #[serde(default)]
    pub publish_ports: Vec<u16>,
    /// Paths in `root` the host reads (a database's compose file), mounted
    /// read-only so agents can't change what the host runs.
    #[serde(default)]
    pub read_only: Vec<PathBuf>,
}

#[derive(Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    projects: Vec<Sandbox>,
}

fn data_dir() -> PathBuf {
    std::env::var_os("ZERON_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::repos::home_dir().join(".zeron"))
}

fn registry_path() -> PathBuf {
    data_dir().join("sandboxes.json")
}

/// Unreadable is an error, not "no sandboxes": that would run on the host.
fn load() -> Result<Vec<Sandbox>, EngineError> {
    match std::fs::read(registry_path()) {
        Ok(bytes) => serde_json::from_slice::<Registry>(&bytes)
            .map(|registry| registry.projects)
            .map_err(|e| {
                EngineError::Other(format!(
                    "{} is unreadable ({e}): fix it, or run zeron sandbox disable",
                    registry_path().display()
                ))
            }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

fn save(projects: Vec<Sandbox>) -> Result<(), EngineError> {
    let path = registry_path();
    std::fs::create_dir_all(data_dir())?;
    let mut file = tempfile::NamedTempFile::new_in(data_dir())?;
    std::io::Write::write_all(
        &mut file,
        &serde_json::to_vec_pretty(&Registry { projects })
            .map_err(|e| EngineError::Other(e.to_string()))?,
    )?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// `path` with symlinks resolved as far as it exists (a worktree being
/// created has no folder yet).
fn resolve(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut at = path;
    loop {
        if let Ok(real) = at.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(real, |p: PathBuf, part| p.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The sandbox `path` is in, if any.
pub fn for_path(path: &Path) -> Result<Option<Sandbox>, EngineError> {
    let projects = load()?;
    if projects.is_empty() {
        return Ok(None);
    }
    let path = resolve(path);
    if let Some(sandbox) = projects
        .iter()
        .find(|s| path.starts_with(&s.root) || path.starts_with(&s.worktrees))
    {
        return Ok(Some(sandbox.clone()));
    }
    // A linked worktree of a sandboxed project outside its folders: not in
    // the container, so it fails rather than run here. Its `.git` file is
    // outside every container too, so agents can't make or unmake this.
    if let Some(checkout) = path.ancestors().find(|dir| dir.join(".git").exists())
        && let Some(root) = crate::workspace_host::linked_worktree_root(checkout)
        && let Some(sandbox) = projects
            .iter()
            .find(|s| resolve(Path::new(&root)) == s.root)
    {
        return Err(EngineError::Other(format!(
            "{} is a worktree of the sandboxed project {}, outside its worktrees folder {}. Move it there (git worktree move) to use it in Zeron",
            checkout.display(),
            sandbox.root.display(),
            sandbox.worktrees.display()
        )));
    }
    Ok(None)
}

/// `path` as the container sees it (same-path mounts of resolved folders).
pub fn container_path(path: &Path) -> String {
    resolve(path).to_string_lossy().into_owned()
}

impl Sandbox {
    fn key(&self) -> String {
        let digest = Sha256::digest(self.root.to_string_lossy().as_bytes());
        digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
    }

    pub fn container(&self) -> String {
        format!("zeron-sandbox-{}", self.key())
    }

    pub fn image(&self) -> String {
        format!("localhost/zeron-sandbox-{}", self.key())
    }

    pub fn run_dir(&self) -> PathBuf {
        data_dir().join("sandbox").join(self.key())
    }

    /// What the container mounts read-only: the git config and hooks the
    /// host's git runs (submodules' too), a config in the project with its
    /// folder (the config alone at the top), and the `readOnly` paths.
    fn protected(&self) -> Result<Vec<PathBuf>, EngineError> {
        let git = self.root.join(".git");
        let config = git.join("config");
        if config.is_file() {
            let output = Command::new("git")
                .args(["config", "--file"])
                .arg(&config)
                .args([
                    "--get-regexp",
                    r"^(extensions\.worktreeconfig|include\.path|includeif\..*\.path)$",
                ])
                .output()?;
            if !output.stdout.is_empty() {
                return Err(EngineError::Other(
                    "This repository's .git/config uses includes or extensions.worktreeConfig, which agents could redirect; remove them to sandbox it".into(),
                ));
            }
        }
        let mut paths: Vec<PathBuf> = [config, git.join("hooks")]
            .into_iter()
            .filter(|p| p.exists())
            .collect();
        // Agents shape `.git/modules` and `.git/worktrees`: a symlink there is
        // skipped, never followed (its target would be mounted).
        fn real(path: &Path) -> bool {
            path.symlink_metadata()
                .is_ok_and(|m| !m.file_type().is_symlink())
        }
        fn modules(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                if path.join("HEAD").exists() {
                    out.extend(
                        [path.join("config"), path.join("hooks")]
                            .into_iter()
                            .filter(|p| real(p)),
                    );
                    modules(&path.join("modules"), out);
                } else {
                    // A module name with slashes (`apps/device-apps`).
                    modules(&path, out);
                }
            }
        }
        modules(&git.join("modules"), &mut paths);
        // The git folders of linked worktrees outside the project's folders
        // (`for_path` refuses those checkouts): the container doesn't have
        // the checkouts, but a rewritten `commondir` there would point their
        // host git at a config agents wrote.
        for admin in std::fs::read_dir(git.join("worktrees"))
            .into_iter()
            .flatten()
            .flatten()
        {
            if !admin.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let admin = admin.path();
            let inside = std::fs::read_to_string(admin.join("gitdir")).is_ok_and(|gitdir| {
                let checkout = resolve(&admin.join(gitdir.trim()));
                checkout.starts_with(&self.root) || checkout.starts_with(&self.worktrees)
            });
            if !inside {
                paths.push(admin);
            }
        }
        if self.config.starts_with(&self.root) {
            paths.push(match self.config.parent() {
                Some(folder) if folder != self.root => folder.to_path_buf(),
                _ => self.config.clone(),
            });
        }
        for path in &self.read_only {
            // Inside the project: anything else would show the container a
            // file of this machine's.
            if path.is_absolute()
                || path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(EngineError::Other(format!(
                    "Read-only path {} must be relative to the project, inside it",
                    path.display()
                )));
            }
            let path = self.root.join(path);
            if !path.exists() {
                return Err(EngineError::Other(format!(
                    "Read-only path {} doesn't exist",
                    path.display()
                )));
            }
            paths.push(path);
        }
        Ok(paths)
    }

    /// [`Self::protected`] as mounts over the project's folder, in order:
    /// each folder between it and a protected path onto itself (`false`),
    /// then the protected paths read-only (`true`). A read-only mount holds
    /// only while the folders above it stay put (`mv .git .git-old` carries
    /// `.git/config`'s mount away and leaves a `.git` agents write), and a
    /// mount point can't be renamed.
    fn mounts(&self) -> Result<Vec<(PathBuf, bool)>, EngineError> {
        let protected = self.protected()?;
        // Never through a symlink: what it points at would be mounted
        // instead (writable, for a folder above a protected path).
        for path in &protected {
            let Ok(inside) = path.strip_prefix(&self.root) else {
                continue;
            };
            let mut at = self.root.clone();
            for part in inside.components() {
                at.push(part);
                if at
                    .symlink_metadata()
                    .is_ok_and(|m| m.file_type().is_symlink())
                {
                    return Err(EngineError::Other(format!(
                        "{} is a symlink, which the sandbox won't mount through. Replace it with what it points at",
                        at.display()
                    )));
                }
            }
        }
        let mut folders: Vec<PathBuf> = protected
            .iter()
            .flat_map(|path| path.ancestors().skip(1))
            .filter(|folder| {
                folder.starts_with(&self.root)
                    && *folder != self.root.as_path()
                    // In a read-only one, nothing moves anyway.
                    && !protected.iter().any(|p| folder.starts_with(p))
            })
            .map(Path::to_path_buf)
            .collect();
        // Parents first.
        folders.sort();
        folders.dedup();
        Ok(folders
            .into_iter()
            .map(|folder| (folder, false))
            .chain(protected.into_iter().map(|path| (path, true)))
            .collect())
    }

    fn run_args(&self, helper: &Path, image: &str) -> Result<Vec<String>, EngineError> {
        // Its ports are pasta's options ([`Self::pasta_options`]), given
        // through a containers.conf ([`ensure`]): podman splits the
        // `pasta:` ones on the commas pasta's port lists need.
        let network = "pasta".to_owned();
        let container = self.container();
        let path = |p: &Path| p.to_string_lossy().into_owned();
        // This machine's user, whatever the image says (devcontainer images
        // end on `USER root`, and a capability-less root can't read the
        // checkout). A bare uid keeps the image's groups for it (npm, go).
        let user = format!("--user={}", unsafe { libc::getuid() });
        let mut args: Vec<String> = [
            "run",
            "-d",
            "--name",
            container.as_str(),
            "--init",
            "--userns=keep-id",
            user.as_str(),
            "--cap-drop=ALL",
            "--security-opt",
            "no-new-privileges",
            // ponytail: no SELinux relabel of the checkouts (slow on large
            // trees); isolation is the user namespace, dropped capabilities
            // and no-new-privileges. Relabel with `:z` if that's not enough.
            "--security-opt",
            "label=disable",
            "--network",
            network.as_str(),
            // IPv4 only, so a server on `localhost` listens where the
            // published loopback port reaches (pasta's ::1 forwards reset
            // connections to IPv4-only servers).
            "--sysctl",
            "net.ipv6.conf.all.disable_ipv6=1",
            "--sysctl",
            "net.ipv6.conf.default.disable_ipv6=1",
            "-e",
            "HOME=/home/zeron",
            // What a project's own tooling can check to adapt (no ssh keys,
            // no daemon).
            "-e",
            "ZERON_SANDBOX=1",
        ]
        .map(String::from)
        .into();
        args.extend(["-v".into(), format!("{container}-home:/home/zeron:U")]);
        for dir in [&self.root, &self.worktrees] {
            args.extend(["-v".into(), format!("{0}:{0}", path(dir))]);
        }
        args.extend(["-v".into(), format!("{0}:{0}:ro", path(&self.run_dir()))]);
        args.extend(["-v".into(), format!("{}:{GUARD}:ro", path(helper))]);
        for (mount, read_only) in self.mounts()? {
            let ro = if read_only { ":ro" } else { "" };
            args.extend(["-v".into(), format!("{0}:{0}{ro}", path(&mount))]);
        }
        args.extend([
            "--entrypoint".into(),
            GUARD.into(),
            image.into(),
            "idle".into(),
        ]);
        Ok(args)
    }
}

/// The helper on the host: beside Zeron, or `ZERON_SANDBOX_HELPER`.
fn helper() -> Result<PathBuf, EngineError> {
    std::env::var_os("ZERON_SANDBOX_HELPER")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .map(|exe| exe.with_file_name("zeron-sandbox"))
        })
        .filter(|path| path.is_file())
        .ok_or_else(|| {
            EngineError::Other("The zeron-sandbox helper isn't installed beside Zeron".into())
        })
}

/// `devcontainer <subcommand>` on the project's config, with podman.
fn devcontainer(sandbox: &Sandbox, subcommand: &str) -> Command {
    let mut cmd = Command::new("devcontainer");
    cmd.args([subcommand, "--docker-path", "podman", "--workspace-folder"])
        .arg(&sandbox.root)
        .arg("--config")
        .arg(&sandbox.config);
    // The engine may run without the login shell's PATH (Homebrew, npm).
    if let Some(path) = zeron_harness::composed_login_shell_path() {
        cmd.env("PATH", path);
    }
    cmd
}

fn podman(args: &[&str]) -> Result<String, EngineError> {
    let output = Command::new("podman")
        .args(args)
        .output()
        .map_err(|e| EngineError::Other(format!("podman: {e}")))?;
    if !output.status.success() {
        return Err(EngineError::Other(format!(
            "podman {}: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Start the project's container if it isn't running with these settings
/// (it's recreated when they change; the home volume, with agent logins,
/// stays). Blocking: a few podman calls.
pub fn ensure(sandbox: &Sandbox) -> Result<Spec, EngineError> {
    static STARTING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = STARTING.lock().unwrap_or_else(|e| e.into_inner());
    let helper = helper()?;
    std::fs::create_dir_all(sandbox.run_dir().join("scripts"))?;
    std::fs::create_dir_all(&sandbox.worktrees)?;
    let image = podman(&["image", "inspect", "--format", "{{.Id}}", &sandbox.image()]).map_err(|_| {
        EngineError::Other(format!(
            "This project's sandbox image isn't built. Run: zeron sandbox enable {} --config {}",
            sandbox.root.display(),
            sandbox.config.display()
        ))
    })?;
    let mut args = sandbox.run_args(&helper, &image)?;
    let conf = sandbox.run_dir().join("containers.conf");
    let pasta = sandbox.pasta_conf();
    let hash: String = Sha256::digest(format!("{}\0{pasta}", args.join("\0")).as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    let container = sandbox.container();
    let spec = Spec {
        helper: helper.to_string_lossy().into_owned(),
        container: container.clone(),
        run_dir: sandbox.run_dir().to_string_lossy().into_owned(),
    };
    let state = podman(&[
        "container",
        "inspect",
        "--format",
        "{{.State.Running}} {{index .Config.Labels \"zeron.sandbox\"}}",
        &container,
    ])
    .ok();
    if state.as_deref() == Some(format!("true {hash}").as_str()) {
        return Ok(spec);
    }
    if state.is_some() {
        podman(&["rm", "-f", "-t", "1", &container])?;
    }
    args.splice(
        2..2,
        ["--label".to_owned(), format!("zeron.sandbox={hash}")],
    );
    std::fs::write(&conf, pasta)?;
    let output = Command::new("podman")
        .args(&args)
        .env("CONTAINERS_CONF_OVERRIDE", &conf)
        .output()
        .map_err(|e| EngineError::Other(format!("podman: {e}")))?;
    if !output.status.success() {
        return Err(EngineError::Other(format!(
            "podman run: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(spec)
}

/// Ports this machine keeps for itself: a server in a sandbox never takes
/// them here, even while they're free. On Zeron's IPC port it would answer
/// the app as the engine; on a login's fixed callback port (ChatGPT's 1455),
/// the browser; on a host port, the container's own clients.
fn reserved_ports(host_ports: &[u16]) -> Vec<u16> {
    let ipc = std::env::var("ZERON_IPC_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(27654);
    let mut reserved = vec![ipc, 1455];
    reserved.extend(host_ports);
    reserved
}

impl Sandbox {
    /// pasta's options: every port a server in the container opens (or
    /// only the `publish_ports`, when given) is published on this machine's
    /// loopback while it's open, never on other interfaces, and delivered to
    /// the container's loopback; the container reaches only the `host_ports`
    /// here; no UDP either way.
    fn pasta_options(&self) -> Vec<String> {
        let reserved = reserved_ports(&self.host_ports);
        let publish: Vec<u16> = if self.publish_ports.is_empty() {
            (1024..=u16::MAX).collect()
        } else {
            self.publish_ports.clone()
        }
        .into_iter()
        .filter(|p| !reserved.contains(p))
        .collect();
        // IPv4 only, like the container (its IPv6 is off: configuring it
        // there fails).
        let mut options = vec![
            "-4".into(),
            "-u".into(),
            "none".into(),
            "-U".into(),
            "none".into(),
        ];
        options.push("-T".into());
        options.push(match port_ranges(&self.host_ports) {
            ranges if ranges.is_empty() => "none".into(),
            ranges => ranges.join(","),
        });
        if !publish.is_empty() {
            options.push("-t".into());
            options.push(format!(
                "127.0.0.1/{},auto",
                port_ranges(&publish).join(",")
            ));
            options.push("--host-lo-to-ns-lo".into());
        } else {
            options.extend(["-t".into(), "none".into()]);
        }
        options
    }

    /// [`Self::pasta_options`] as the containers.conf podman reads them from.
    fn pasta_conf(&self) -> String {
        let quoted: Vec<String> = self
            .pasta_options()
            .iter()
            .map(|o| format!("\"{o}\""))
            .collect();
        format!("[network]\npasta_options = [{}]\n", quoted.join(", "))
    }
}

/// A terminal or service shell in the container: the image's login shell,
/// with the terminal's variables forwarded by name (never the host's PATH or
/// HOME). `script` is the action script, in the run folder the container
/// mounts.
pub fn shell_command(
    spec: &Spec,
    cwd: &str,
    service: bool,
    environment: &std::collections::HashMap<String, String>,
    script: Option<&Path>,
) -> portable_pty::CommandBuilder {
    let mut values: Vec<(String, String)> = [
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("TERM_PROGRAM", "Zeron"),
    ]
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .into();
    values.extend(
        environment
            .iter()
            .filter(|(name, _)| !matches!(name.as_str(), "PATH" | "HOME"))
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    if let Some(script) = script {
        values.push((
            "ZERON_ACTION_SCRIPT".into(),
            script.to_string_lossy().into_owned(),
        ));
    }
    let names: Vec<String> = values.iter().map(|(name, _)| name.clone()).collect();
    let mut command = vec!["@shell".to_owned()];
    if service {
        command.extend(["-c".into(), ". \"$ZERON_ACTION_SCRIPT\"".into()]);
    }
    let mut cmd = portable_pty::CommandBuilder::new(&spec.helper);
    cmd.args(spec.exec_args(&container_path(Path::new(cwd)), true, &names, &command));
    cmd.cwd(cwd);
    for (name, value) in values {
        cmd.env(name, value);
    }
    cmd
}

/// Environment for host git in `dir`: the project's own git folder, never
/// one a `.git` file or `commondir` inside the container points at.
pub fn git_env(dir: &Path) -> Result<Vec<(&'static str, PathBuf)>, EngineError> {
    let Some(sandbox) = for_path(dir)? else {
        return Ok(Vec::new());
    };
    let dir = resolve(dir);
    let common = sandbox.root.join(".git");
    let (git_dir, work_tree) = if dir.starts_with(&sandbox.root) {
        (common.clone(), sandbox.root.clone())
    } else {
        let checkout = dir
            .strip_prefix(&sandbox.worktrees)
            .ok()
            .and_then(|rest| rest.components().next())
            .map(|name| sandbox.worktrees.join(name))
            .ok_or_else(|| EngineError::Other("Not a checkout of this project".into()))?;
        let marker = checkout.join(".git");
        let admin = std::fs::read_dir(common.join("worktrees"))?
            .flatten()
            .map(|entry| entry.path())
            .find(|admin| {
                std::fs::read_to_string(admin.join("gitdir"))
                    .is_ok_and(|gitdir| resolve(Path::new(gitdir.trim())) == marker)
            })
            .ok_or_else(|| {
                EngineError::Other(format!(
                    "{} isn't a worktree of this project",
                    checkout.display()
                ))
            })?;
        (admin, checkout)
    };
    // Not in a submodule (or other nested repository): which git folder it
    // uses is the agents' to say (its `.git` file, `.gitmodules`), and a
    // worktree's submodule git folders, or one initialized after the
    // container started, aren't read-only. Its git runs jailed ([`pin_git`]).
    let mut at = work_tree.clone();
    for part in dir
        .strip_prefix(&work_tree)
        .unwrap_or(Path::new(""))
        .components()
    {
        at.push(part);
        if at.join(".git").exists() {
            return Err(EngineError::Other(format!(
                "{} is a nested repository of a sandboxed project; its git runs only in the container",
                at.display()
            )));
        }
    }
    Ok(vec![
        ("GIT_DIR", git_dir),
        ("GIT_COMMON_DIR", common),
        ("GIT_WORK_TREE", work_tree),
    ])
}

/// Git commands that need this machine itself: its network and credentials
/// (ssh, the gh helper), its signing key, or the project's `.git/config`
/// (read-only in the jail: branch tracking, renames, worktrees) and the real
/// list of its worktrees (a jailed `worktree prune` would miss those outside
/// it). For a sandboxed checkout they run here pinned ([`git_env`]) with
/// submodule recursion off and hooks from `.git/hooks`; every other git
/// command runs jailed ([`jail`]).
const HOST_GIT: &[&str] = &[
    "push",
    "fetch",
    "pull",
    "ls-remote",
    "remote",
    "commit",
    "tag",
    "merge",
    "rebase",
    "cherry-pick",
    "revert",
    "am",
    "branch",
    "config",
    "switch",
    "checkout",
    "worktree",
];

/// Config for a host-side git of a sandboxed checkout: git never starts a
/// child in a submodule, whose git folder and config are the agents'.
const NO_SUBMODULE_RECURSION: &[(&str, &str)] = &[
    ("diff.ignoreSubmodules", "dirty"),
    ("status.submoduleSummary", "false"),
    ("submodule.recurse", "false"),
    ("fetch.recurseSubmodules", "false"),
    ("push.recurseSubmodules", "no"),
];

/// The git subcommand in `args` (after `-C dir`, `-c key=value` and other
/// global options).
fn git_subcommand<'a>(mut args: impl Iterator<Item = &'a std::ffi::OsStr>) -> Option<&'a str> {
    while let Some(arg) = args.next() {
        let arg = arg.to_str()?;
        match arg {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path" => {
                args.next();
            }
            _ if arg.starts_with('-') => {}
            _ => return Some(arg),
        }
    }
    None
}

/// Makes host git `cmd` in `dir` safe for a sandboxed checkout, whose
/// repositories agents shape (`.git` files, submodule git folders and their
/// config, attributes). Call it last, once the command's arguments and
/// environment are set. A command it can't make safe fails instead.
///
/// - [`HOST_GIT`] and `gh` run here, pinned to the project's own git folder
///   (read-only to agents) with [`NO_SUBMODULE_RECURSION`]; never inside a
///   nested repository.
/// - Everything else runs in a jail ([`jail`]) with no network and none of
///   this machine's files but the system's and the project's, so whatever an
///   agent's config makes git run there reaches nothing agents can't.
pub fn pin_git(cmd: &mut tokio::process::Command, dir: &Path) {
    let sandbox = match for_path(dir) {
        Ok(Some(sandbox)) => sandbox,
        Ok(None) => return,
        Err(error) => return refuse_git(cmd, dir, &error),
    };
    let std = cmd.as_std();
    let git = Path::new(std.get_program()).file_name() == Some(std::ffi::OsStr::new("git"));
    let host = !git || git_subcommand(std.get_args()).is_some_and(|s| HOST_GIT.contains(&s));
    if !host {
        match jail(&sandbox, cmd) {
            Ok(jailed) => *cmd = jailed,
            Err(error) => refuse_git(cmd, dir, &error),
        }
        return;
    }
    match git_env(dir) {
        Ok(env) => {
            // Hooks only from the project's own (read-only) hooks folder,
            // never a `core.hooksPath` in the work tree, which agents edit.
            let hooks = env
                .iter()
                .find(|(k, _)| *k == "GIT_COMMON_DIR")
                .map(|(_, common)| common.join("hooks"));
            cmd.envs(env);
            let count = std::env::var("GIT_CONFIG_COUNT")
                .ok()
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
            let count = cmd
                .as_std()
                .get_envs()
                .find(|(k, _)| *k == "GIT_CONFIG_COUNT")
                .and_then(|(_, v)| v?.to_str()?.parse().ok())
                .unwrap_or(count);
            let hooks = hooks.map(|h| h.to_string_lossy().into_owned());
            let config = NO_SUBMODULE_RECURSION
                .iter()
                .map(|(k, v)| (*k, *v))
                .chain(hooks.as_deref().map(|h| ("core.hooksPath", h)));
            let mut added = 0;
            for (i, (key, value)) in config.enumerate() {
                cmd.env(format!("GIT_CONFIG_KEY_{}", count + i), key)
                    .env(format!("GIT_CONFIG_VALUE_{}", count + i), value);
                added += 1;
            }
            cmd.env("GIT_CONFIG_COUNT", (count + added).to_string());
        }
        Err(error) => refuse_git(cmd, dir, &error),
    }
}

/// A git folder that doesn't exist: git fails rather than read one an agent
/// chose.
fn refuse_git(cmd: &mut tokio::process::Command, dir: &Path, error: &EngineError) {
    tracing::warn!(%error, dir = %dir.display(), "sandbox: refusing host git");
    cmd.env("GIT_DIR", "/nonexistent/zeron-sandbox-refused");
}

/// `cmd` (git) in bubblewrap: no network, a fresh /tmp, the system's files
/// read-only, the project and its worktrees writable but for what this
/// machine reads or runs from them (the container's read-only paths), any
/// index or object store the command names, and this machine's git config
/// read-only (identity, ignores). Nothing else of this machine: no home, no
/// ssh or gpg agent, and only the environment git needs.
fn jail(
    sandbox: &Sandbox,
    cmd: &tokio::process::Command,
) -> Result<tokio::process::Command, EngineError> {
    let std = cmd.as_std();
    let git = which("git").ok_or_else(|| EngineError::Other("git isn't installed".into()))?;
    let mut jail = tokio::process::Command::new("bwrap");
    jail.args([
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--ro-bind",
        "/usr",
        "/usr",
        "--ro-bind",
        "/etc",
        "/etc",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
    ]);
    for top in ["/bin", "/lib", "/lib64", "/sbin"] {
        let top = Path::new(top);
        match std::fs::read_link(top) {
            Ok(target) => jail.arg("--symlink").arg(target).arg(top),
            Err(_) if top.is_dir() => jail.arg("--ro-bind").arg(top).arg(top),
            Err(_) => continue,
        };
    }
    // A git outside /usr (Homebrew) brings its prefix.
    if let Some(prefix) = git.parent().and_then(Path::parent)
        && !git.starts_with("/usr")
    {
        jail.arg("--ro-bind").arg(prefix).arg(prefix);
    }
    for dir in [&sandbox.root, &sandbox.worktrees] {
        jail.arg("--bind").arg(dir).arg(dir);
    }
    for (mount, read_only) in sandbox.mounts()? {
        let bind = if read_only { "--ro-bind" } else { "--bind" };
        jail.arg(bind).arg(&mount).arg(&mount);
    }
    let env = |name: &str| {
        std.get_envs()
            .find(|(k, _)| *k == name)
            .and_then(|(_, v)| v.map(std::ffi::OsStr::to_os_string))
    };
    let mut stores = Vec::new();
    if let Some(index) = env("GIT_INDEX_FILE") {
        stores.extend(Path::new(&index).parent().map(Path::to_path_buf));
    }
    if let Some(objects) = env("GIT_OBJECT_DIRECTORY") {
        stores.push(PathBuf::from(objects));
    }
    if let Some(alternates) = env("GIT_ALTERNATE_OBJECT_DIRECTORIES") {
        stores.extend(std::env::split_paths(&alternates));
    }
    for store in stores.into_iter().filter(|s| s.is_absolute() && s.is_dir()) {
        if !store.starts_with(&sandbox.root) && !store.starts_with(&sandbox.worktrees) {
            jail.arg("--bind").arg(&store).arg(&store);
        }
    }
    let home = crate::repos::home_dir();
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    for file in [home.join(".gitconfig"), config.join("git")] {
        jail.arg("--ro-bind-try").arg(&file).arg(&file);
    }
    if let Some(cwd) = std.get_current_dir() {
        jail.arg("--chdir").arg(cwd);
        jail.current_dir(cwd);
    }
    jail.arg("--").arg(&git).args(std.get_args());
    // Only what git needs, then the command's own (never the engine's
    // environment: its tokens would be the jail's to read).
    jail.env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config);
    for name in ["LANG", "LC_ALL", "LC_CTYPE", "TZ"] {
        if let Some(value) = std::env::var_os(name) {
            jail.env(name, value);
        }
    }
    for (key, value) in std.get_envs() {
        if let Some(value) = value {
            jail.env(key, value);
        }
    }
    Ok(jail)
}

/// `program` on this process's PATH.
fn which(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// For a sandboxed run: start the container, give it this machine's login
/// for the CLI (`accounts`), point its MCP servers at sockets the host side
/// serves, and tell the harness to wrap its CLI. Only Claude Code and Codex
/// know how; any other harness is refused.
pub async fn stamp_run(
    harness: HarnessId,
    request: &mut RunRequest,
    accounts: Option<&crate::AgentAccounts>,
) -> Result<(), String> {
    let cwd = PathBuf::from(&request.cwd);
    let Some(sandbox) = for_path(&cwd).map_err(|e| e.to_string())? else {
        return Ok(());
    };
    if !matches!(harness, HarnessId::ClaudeCode | HarnessId::Codex) {
        return Err(
            "This project runs its agents in a sandbox, which supports Claude Code and Codex"
                .into(),
        );
    }
    let spec = tokio::task::spawn_blocking(move || ensure(&sandbox))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("Couldn't start the project's sandbox: {e}"))?;
    if let Some(accounts) = accounts {
        sync_login(accounts, &spec.container, harness).await;
        keep_logins_fresh(accounts);
        let container = spec.container.clone();
        if let Ok(Err(error)) =
            tokio::task::spawn_blocking(move || install_git_identity(&container)).await
        {
            tracing::warn!(%error, "sandbox: couldn't hand over the git identity");
        }
    }
    let mut records = Vec::new();
    for server in request.mcp.iter_mut().chain(request.extra_mcp.iter_mut()) {
        let socket = format!(
            "{}/mcp-{}.sock",
            spec.run_dir,
            &uuid::Uuid::new_v4().simple().to_string()[..16]
        );
        records.push(mcp_record(
            &socket,
            &server.command,
            &server.args,
            &server.env,
        ));
        server.command = GUARD.into();
        server.args = vec!["relay".into(), socket];
    }
    request.env.insert(MCP_ENV.into(), mcp_records(&records));
    request.env.insert(
        SPEC_ENV.into(),
        serde_json::to_string(&spec).map_err(|e| e.to_string())?,
    );
    Ok(())
}

/// Puts this machine's `harness` login in the container's home
/// ([`crate::AgentAccounts::sandbox_login`]). A failure is logged, not
/// fatal: the CLI then says it isn't signed in.
async fn sync_login(accounts: &crate::AgentAccounts, container: &str, harness: HarnessId) {
    let login = match accounts.sandbox_login(harness).await {
        Ok(Some(login)) => login,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%error, ?harness, "sandbox: no login to hand over");
            return;
        }
    };
    let container = container.to_owned();
    let written = tokio::task::spawn_blocking(move || install_login(&container, login.0, &login.1))
        .await
        .map_err(|e| EngineError::Other(e.to_string()))
        .and_then(|r| r);
    if let Err(error) = written {
        tracing::warn!(%error, ?harness, "sandbox: couldn't hand over the login");
    }
}

/// Writes `contents` to `path` under the container's home from inside it,
/// as its user: a link an agent planted in the home volume can only point
/// within the container. Owner-only, atomic, and left alone when unchanged.
fn install_login(container: &str, path: &str, contents: &str) -> Result<(), EngineError> {
    use std::io::Write;
    let mut child = Command::new("podman")
        .args([
            "exec",
            "-i",
            container,
            "sh",
            "-c",
            r#"cd && umask 077 && mkdir -p "$(dirname "$1")" && cat > "$1.zeron" && { cmp -s "$1.zeron" "$1" && rm -f "$1.zeron" || mv -f "$1.zeron" "$1"; }"#,
            "sh",
            path,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| EngineError::Other(format!("podman: {e}")))?;
    child
        .stdin
        .take()
        .ok_or_else(|| EngineError::Other("podman exec has no stdin".into()))?
        .write_all(contents.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(EngineError::Other(format!(
            "podman exec: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// This machine's git identity (`user.name`, `user.email`) for the commits
/// agents make in the container, so they're authored as here. Not its
/// signing key: those commits are unsigned.
fn install_git_identity(container: &str) -> Result<(), EngineError> {
    let get = |key: &str| {
        Command::new("git")
            .args(["config", "--global", "--get", key])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let (Some(name), Some(email)) = (get("user.name"), get("user.email")) else {
        return Ok(());
    };
    podman(&[
        "exec",
        container,
        "sh",
        "-c",
        r#"git config --global user.name "$1" && git config --global user.email "$2""#,
        "sh",
        &name,
        &email,
    ])
    .map(|_| ())
}

/// Every minute, the logins in each running sandbox are handed over again,
/// so a parked session (whose CLI re-reads its file before its token runs
/// out) never meets an expired one. Started by the first sandboxed run.
fn keep_logins_fresh(accounts: &crate::AgentAccounts) {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let accounts = accounts.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let running = tokio::task::spawn_blocking(|| {
                load()
                    .unwrap_or_default()
                    .iter()
                    .map(Sandbox::container)
                    .filter(|c| {
                        podman(&["container", "inspect", "--format", "{{.State.Running}}", c])
                            .is_ok_and(|state| state == "true")
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            for container in running {
                for harness in [HarnessId::ClaudeCode, HarnessId::Codex] {
                    sync_login(&accounts, &container, harness).await;
                }
            }
        }
    });
}

// ── `zeron sandbox` ────────────────────────────────────────────────────────

/// Where Zeron creates `root`'s worktrees by default (`Repos::create_worktree`).
/// A custom location chosen in Settings isn't read here: pass `--worktrees`.
pub fn default_worktrees(root: &Path) -> PathBuf {
    let base = std::env::var_os("ZERON_WORKTREES_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::repos::home_dir().join(".zeron").join("worktrees"));
    base.join(resolve(root).file_name().unwrap_or_default())
}

/// What agents do inside a sandbox once it's up.
pub const LOGIN_NOTE: &str = "Agents there use this machine's Claude Code and Codex logins (access tokens only; this machine keeps refreshing them), handed over when they start.";

/// A registered project and its container, as `status` and the MCP tools show it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    #[serde(flatten)]
    pub sandbox: Sandbox,
    pub container: String,
    /// podman's state (`running`, `exited`, …), or `not created`.
    pub state: String,
    pub image_built: bool,
    /// The last image build's output, when Zeron ran it.
    pub build_log: Option<PathBuf>,
}

impl Sandbox {
    fn build_log(&self) -> PathBuf {
        self.run_dir().join("build.log")
    }

    /// Refuses a file the image is built from that agents could change: in a
    /// worktrees folder, another sandboxed project, or this one outside its
    /// read-only paths.
    fn check_input(
        &self,
        input: &Path,
        projects: &[Sandbox],
        protected: &[PathBuf],
    ) -> Result<(), EngineError> {
        let writable = input.starts_with(&self.worktrees)
            || (input.starts_with(&self.root) && !protected.iter().any(|p| input.starts_with(p)))
            || projects.iter().any(|s| {
                s.root != self.root
                    && (input.starts_with(&s.root) || input.starts_with(&s.worktrees))
            });
        if writable {
            return Err(EngineError::Other(format!(
                "{} builds this sandbox's image, but agents could change it there. Keep the config in this project's own folder (that folder is then read-only; list the other files it uses in this project as read-only paths) or outside every sandboxed project",
                input.display()
            )));
        }
        Ok(())
    }

    /// The compose files and Dockerfile the config names, as the devcontainer
    /// CLI reads them. Files those name in turn (a compose `include`) aren't.
    fn image_inputs(&self) -> Result<Vec<PathBuf>, EngineError> {
        let output = devcontainer(self, "read-configuration")
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| {
                EngineError::Other(format!(
                    "devcontainer CLI: {e} (install @devcontainers/cli)"
                ))
            })?;
        if !output.status.success() {
            return Err(EngineError::Other(format!(
                "devcontainer read-configuration failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let read: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| EngineError::Other(format!("devcontainer read-configuration: {e}")))?;
        let config = &read["configuration"];
        let mut names: Vec<&str> = match &config["dockerComposeFile"] {
            serde_json::Value::Array(files) => files.iter().filter_map(|f| f.as_str()).collect(),
            file => file.as_str().into_iter().collect(),
        };
        names.extend(config["build"]["dockerfile"].as_str());
        names.extend(config["dockerFile"].as_str());
        let folder = self.config.parent().unwrap_or(&self.root);
        Ok(names
            .into_iter()
            .map(|name| resolve(&folder.join(name)))
            .collect())
    }

    pub fn status(&self) -> Status {
        let container = self.container();
        Status {
            state: podman(&[
                "container",
                "inspect",
                "--format",
                "{{.State.Status}}",
                &container,
            ])
            .unwrap_or_else(|_| "not created".into()),
            image_built: podman(&["image", "exists", &self.image()]).is_ok(),
            build_log: Some(self.build_log()).filter(|log| log.is_file()),
            container,
            sandbox: self.clone(),
        }
    }
}

/// Every registered project, with its container's state.
pub fn statuses() -> Result<Vec<Status>, EngineError> {
    Ok(load()?.iter().map(Sandbox::status).collect())
}

/// The project at `root` (exactly, not a project around it), if sandboxed.
pub fn status_of(root: &Path) -> Result<Option<Status>, EngineError> {
    let root = resolve(root);
    Ok(load()?.iter().find(|s| s.root == root).map(Sandbox::status))
}

/// Build the image, register the project and start its container. The
/// build's output goes to the terminal, or with `log_to_file`, to the
/// project's build log (an error ends with its tail).
pub fn enable(mut sandbox: Sandbox, log_to_file: bool) -> Result<Status, EngineError> {
    let real = |path: &Path, what: &str| {
        path.canonicalize()
            .map_err(|e| EngineError::Other(format!("{what} {}: {e}", path.display())))
    };
    sandbox.root = real(&sandbox.root, "Project folder")?;
    sandbox.config = real(&sandbox.config, "Devcontainer config")?;
    std::fs::create_dir_all(&sandbox.worktrees)?;
    sandbox.worktrees = real(&sandbox.worktrees, "Worktrees folder")?;
    let projects = load()?;
    let protected = sandbox.protected()?;
    sandbox.check_input(&sandbox.config, &projects, &protected)?;
    for input in sandbox.image_inputs()? {
        sandbox.check_input(&input, &projects, &protected)?;
    }
    let mut build = devcontainer(&sandbox, "build");
    build.args(["--image-name", &sandbox.image()]);
    let log = sandbox.build_log();
    if log_to_file {
        std::fs::create_dir_all(sandbox.run_dir())?;
        let file = std::fs::File::create(&log)?;
        build
            .stdout(file.try_clone()?)
            .stderr(file)
            .stdin(std::process::Stdio::null());
    }
    let status = build.status().map_err(|e| {
        EngineError::Other(format!(
            "devcontainer CLI: {e} (install @devcontainers/cli)"
        ))
    })?;
    if !status.success() {
        let tail = match log_to_file {
            true => {
                let text = std::fs::read_to_string(&log).unwrap_or_default();
                let lines: Vec<&str> = text.lines().collect();
                format!(":\n{}", lines[lines.len().saturating_sub(15)..].join("\n"))
            }
            false => String::new(),
        };
        return Err(EngineError::Other(format!(
            "devcontainer build failed{tail}"
        )));
    }
    let mut projects = projects;
    projects.retain(|s| s.root != sandbox.root);
    projects.push(sandbox.clone());
    save(projects)?;
    ensure(&sandbox)?;
    Ok(sandbox.status())
}

/// Unregister the project and remove its container. Its image and home
/// volume (agent logins) stay, so enabling it again is quick.
pub fn disable(root: &Path) -> Result<(), EngineError> {
    let root = resolve(root);
    let mut projects = load()?;
    let Some(index) = projects.iter().position(|s| s.root == root) else {
        return Err(EngineError::Other(format!(
            "{} isn't sandboxed",
            root.display()
        )));
    };
    let sandbox = projects.remove(index);
    save(projects)?;
    let _ = podman(&["rm", "-f", "-t", "1", &sandbox.container()]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(dir: &Path) -> Sandbox {
        Sandbox {
            root: dir.join("repo"),
            worktrees: dir.join("worktrees"),
            config: dir.join("devcontainer.json"),
            host_ports: vec![5432],
            publish_ports: vec![3000],
            read_only: vec!["compose.yaml".into()],
        }
    }

    #[test]
    fn run_args_fix_the_flags_and_mount_what_the_host_runs_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let s = sandbox(&temp.path().canonicalize().unwrap());
        assert!(
            s.run_args(Path::new("/h"), "img").is_err(),
            "a missing read-only path is refused"
        );
        std::fs::create_dir_all(s.root.join(".git/hooks")).unwrap();
        std::fs::create_dir_all(s.root.join(".git/modules/apps/ui/hooks")).unwrap();
        std::fs::write(
            s.root.join(".git/modules/apps/ui/HEAD"),
            "ref: refs/heads/main\n",
        )
        .unwrap();
        std::fs::write(s.root.join(".git/modules/apps/ui/config"), "").unwrap();
        std::fs::write(s.root.join(".git/config"), "[core]\n").unwrap();
        std::fs::write(s.root.join("compose.yaml"), "").unwrap();
        let args = s.run_args(Path::new("/h"), "img").unwrap().join(" ");
        let pinned = format!(
            "--userns=keep-id --user={} --cap-drop=ALL --security-opt no-new-privileges",
            unsafe { libc::getuid() }
        );
        for expected in [
            pinned.as_str(),
            "--network pasta --sysctl net.ipv6.conf.all.disable_ipv6=1",
            &format!("{0}/.git/config:{0}/.git/config:ro", s.root.display()),
            &format!("{0}/.git/hooks:{0}/.git/hooks:ro", s.root.display()),
            &format!(
                "{0}/.git/modules/apps/ui/config:{0}/.git/modules/apps/ui/config:ro",
                s.root.display()
            ),
            &format!("{0}/compose.yaml:{0}/compose.yaml:ro", s.root.display()),
            &format!("/h:{GUARD}:ro"),
            &format!("--entrypoint {GUARD} img idle"),
        ] {
            assert!(args.contains(expected), "{expected} missing from {args}");
        }
        std::fs::write(s.root.join(".git/config"), "[include]\n\tpath = ../evil\n").unwrap();
        assert!(
            s.run_args(Path::new("/h"), "img").is_err(),
            "a repo config include is refused"
        );
        // Ports: the listed ones (or every one a server opens), on loopback
        // only, never this machine's own; host ports explicit; no UDP.
        let options = s.pasta_options().join(" ");
        assert!(options.contains("-T 5432"), "{options}");
        assert!(
            options.contains("-t 127.0.0.1/3000,auto --host-lo-to-ns-lo"),
            "{options}"
        );
        assert!(options.contains("-u none -U none"), "{options}");
        let open = Sandbox {
            publish_ports: Vec::new(),
            ..s.clone()
        };
        let options = open.pasta_options().join(" ");
        assert!(
            options.contains("-t 127.0.0.1/1024-1454,1456-5431,5433-27653,27655-65535,auto"),
            "{options}"
        );
        assert!(
            open.pasta_conf()
                .starts_with("[network]\npasta_options = [\"-4\", \"-u\", \"none\"")
        );
    }

    #[test]
    fn a_config_in_the_project_is_read_only_with_what_it_builds_from() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let mut s = sandbox(&base);
        s.config = s.root.join(".devcontainer/devcontainer.json");
        let compose = s.root.join("compose.devcontainer.yaml");
        let other = Sandbox {
            root: base.join("other"),
            worktrees: base.join("other-worktrees"),
            ..sandbox(&base)
        };
        std::fs::create_dir_all(s.root.join(".devcontainer")).unwrap();
        std::fs::write(s.root.join("compose.yaml"), "").unwrap();
        let protected = s.protected().unwrap();
        assert!(protected.contains(&s.root.join(".devcontainer")));
        let projects = [other.clone()];
        let check = |path: &Path, protected: &[PathBuf]| s.check_input(path, &projects, protected);
        assert!(check(&s.config, &protected).is_ok());
        assert!(check(&s.root.join(".devcontainer/Dockerfile"), &protected).is_ok());
        assert!(check(&base.join("elsewhere/devcontainer.json"), &protected).is_ok());
        assert!(check(&compose, &protected).is_err(), "not read-only");
        assert!(
            check(
                &s.worktrees.join("wt/.devcontainer/devcontainer.json"),
                &protected
            )
            .is_err()
        );
        assert!(check(&other.root.join("devcontainer.json"), &protected).is_err());
        assert!(check(&other.worktrees.join("devcontainer.json"), &protected).is_err());
        s.read_only.push("compose.devcontainer.yaml".into());
        std::fs::write(&compose, "").unwrap();
        let protected = s.protected().unwrap();
        assert!(s.check_input(&compose, &projects, &protected).is_ok());
        // At the top, only the config itself, never the whole project.
        s.config = s.root.join(".devcontainer.json");
        let protected = s.protected().unwrap();
        assert!(protected.contains(&s.config) && !protected.contains(&s.root));
    }

    #[test]
    fn folders_above_what_the_host_reads_stay_put_and_outside_worktrees_are_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let s = Sandbox {
            read_only: Vec::new(),
            ..sandbox(&base)
        };
        let git = s.root.join(".git");
        std::fs::create_dir_all(git.join("hooks")).unwrap();
        std::fs::create_dir_all(git.join("modules/apps/ui")).unwrap();
        std::fs::write(git.join("modules/apps/ui/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(git.join("modules/apps/ui/config"), "").unwrap();
        std::fs::write(git.join("config"), "[core]\n").unwrap();
        for (name, checkout) in [
            ("inside", s.worktrees.join("inside")),
            ("outside", base.join("elsewhere")),
        ] {
            let admin = git.join("worktrees").join(name);
            std::fs::create_dir_all(&admin).unwrap();
            std::fs::write(
                admin.join("gitdir"),
                format!("{}\n", checkout.join(".git").display()),
            )
            .unwrap();
        }
        // Symlinks agents plant there are skipped, never mounted through.
        let elsewhere = base.join("elsewhere-git");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("HEAD"), "").unwrap();
        std::fs::write(elsewhere.join("config"), "").unwrap();
        #[cfg(unix)]
        for link in [
            "modules/linked",
            "worktrees/linked",
            "modules/apps/ui/hooks",
        ] {
            std::os::unix::fs::symlink(&elsewhere, git.join(link)).unwrap();
        }
        let folder = |rel: &str| (s.root.join(rel), false);
        let read_only = |rel: &str| (s.root.join(rel), true);
        assert_eq!(
            s.mounts().unwrap(),
            [
                folder(".git"),
                folder(".git/modules"),
                folder(".git/modules/apps"),
                folder(".git/modules/apps/ui"),
                folder(".git/worktrees"),
                read_only(".git/config"),
                read_only(".git/hooks"),
                read_only(".git/modules/apps/ui/config"),
                read_only(".git/worktrees/outside"),
            ]
        );
        // A protected path the person names through a symlink is refused.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(elsewhere.join("config"), s.root.join("compose.yaml"))
                .unwrap();
            let linked = Sandbox {
                read_only: vec!["compose.yaml".into()],
                ..s.clone()
            };
            let refused = linked.mounts().unwrap_err().to_string();
            assert!(refused.contains("is a symlink"), "{refused}");
        }
    }

    #[test]
    fn resolve_keeps_the_missing_tail_of_a_path() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().canonicalize().unwrap();
        assert_eq!(resolve(&temp.path().join("not/yet")), real.join("not/yet"));
    }
}
