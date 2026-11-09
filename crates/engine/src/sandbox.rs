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
use zeron_proto::{HarnessId, RunRequest};

use crate::EngineError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sandbox {
    /// The project's main folder.
    pub root: PathBuf,
    /// The folder Zeron creates this project's worktrees in.
    pub worktrees: PathBuf,
    /// The devcontainer.json the image is built from, outside the project.
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
    Ok(projects
        .into_iter()
        .find(|s| path.starts_with(&s.root) || path.starts_with(&s.worktrees)))
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
    /// host's git runs (submodules' too), and the `readOnly` paths.
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
        fn modules(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if path.join("HEAD").exists() {
                    out.extend(
                        [path.join("config"), path.join("hooks")]
                            .into_iter()
                            .filter(|p| p.exists()),
                    );
                    modules(&path.join("modules"), out);
                } else {
                    // A module name with slashes (`apps/device-apps`).
                    modules(&path, out);
                }
            }
        }
        modules(&git.join("modules"), &mut paths);
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

    fn run_args(&self, helper: &Path, image: &str) -> Result<Vec<String>, EngineError> {
        let ports: Vec<String> = self
            .host_ports
            .iter()
            .map(|p| format!("-T,{p}"))
            .chain(self.publish_ports.iter().map(|p| format!("-t,{p}")))
            .collect();
        let network = if ports.is_empty() {
            "pasta".to_owned()
        } else {
            format!("pasta:{}", ports.join(","))
        };
        let container = self.container();
        let path = |p: &Path| p.to_string_lossy().into_owned();
        let mut args: Vec<String> = [
            "run",
            "-d",
            "--name",
            container.as_str(),
            "--init",
            "--userns=keep-id",
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
            "-e",
            "HOME=/home/zeron",
        ]
        .map(String::from)
        .into();
        args.extend(["-v".into(), format!("{container}-home:/home/zeron:U")]);
        for dir in [&self.root, &self.worktrees] {
            args.extend(["-v".into(), format!("{0}:{0}", path(dir))]);
        }
        args.extend(["-v".into(), format!("{0}:{0}:ro", path(&self.run_dir()))]);
        args.extend(["-v".into(), format!("{}:{GUARD}:ro", path(helper))]);
        for protected in self.protected()? {
            args.extend(["-v".into(), format!("{0}:{0}:ro", path(&protected))]);
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
    let hash: String = Sha256::digest(args.join("\0").as_bytes())
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
    podman(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
    Ok(spec)
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
    Ok(vec![
        ("GIT_DIR", git_dir),
        ("GIT_COMMON_DIR", common),
        ("GIT_WORK_TREE", work_tree),
    ])
}

/// [`git_env`] on a host git `cmd` in `dir`. A sandboxed checkout it can't
/// vouch for gets a git folder that doesn't exist, so git fails rather than
/// read one an agent chose.
pub fn pin_git(cmd: &mut tokio::process::Command, dir: &Path) {
    match git_env(dir) {
        Ok(env) => {
            cmd.envs(env);
        }
        Err(error) => {
            tracing::warn!(%error, dir = %dir.display(), "sandbox: refusing host git");
            cmd.env("GIT_DIR", "/nonexistent/zeron-sandbox-refused");
        }
    }
}

/// For a sandboxed run: start the container, point its MCP servers at
/// sockets the host side serves, and tell the harness to wrap its CLI.
/// Only Claude Code and Codex know how; any other harness is refused.
pub async fn stamp_run(harness: HarnessId, request: &mut RunRequest) -> Result<(), String> {
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
pub const LOGIN_NOTE: &str = "Agents log in once inside the container: open a terminal for this project in Zeron (it opens there) and run `claude`, then `codex login`.";

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
    // Inside this project, or any sandboxed one, its agents could change
    // what this image is built from.
    let projects = load()?;
    if std::iter::once(&sandbox)
        .chain(&projects)
        .any(|s| sandbox.config.starts_with(&s.root) || sandbox.config.starts_with(&s.worktrees))
    {
        return Err(EngineError::Other(
            "Keep the devcontainer config outside this project and any sandboxed one, so agents can't change it".into(),
        ));
    }
    sandbox.protected()?;
    let mut build = Command::new("devcontainer");
    build
        .args(["build", "--docker-path", "podman", "--workspace-folder"])
        .arg(&sandbox.root)
        .arg("--config")
        .arg(&sandbox.config)
        .args(["--image-name", &sandbox.image()]);
    // The engine may run without the login shell's PATH (Homebrew, npm).
    if let Some(path) = zeron_harness::composed_login_shell_path() {
        build.env("PATH", path);
    }
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
        for expected in [
            "--userns=keep-id --cap-drop=ALL --security-opt no-new-privileges",
            "--network pasta:-T,5432,-t,3000",
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
    }

    #[test]
    fn resolve_keeps_the_missing_tail_of_a_path() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().canonicalize().unwrap();
        assert_eq!(resolve(&temp.path().join("not/yet")), real.join("not/yet"));
    }
}
