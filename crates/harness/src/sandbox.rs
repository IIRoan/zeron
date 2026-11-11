//! A sandboxed run's CLI starts inside its project's container
//! (`zeron_proto::sandbox`, stamped by the engine): the same program (found
//! on the image's PATH), arguments and folder. Only the run's own variables
//! are forwarded, never the host's PATH, HOME or login-shell environment.
use std::collections::BTreeMap;
use std::process::Stdio;

use tokio::process::Command;
use zeron_proto::sandbox::{MCP_ENV, SPEC_ENV, Spec};

use crate::HarnessError;

/// `cmd` unchanged outside a sandbox; otherwise the same command through
/// `zeron-sandbox exec`. A stamped run that can't be wrapped fails.
pub(crate) fn wrap(cmd: Command, env: &BTreeMap<String, String>) -> Result<Command, HarnessError> {
    let Some(spec) = env.get(SPEC_ENV) else {
        return Ok(cmd);
    };
    let refuse = |why: String| {
        HarnessError::NotSignedIn(format!("Couldn't start the agent in its sandbox: {why}"))
    };
    let spec: Spec = serde_json::from_str(spec).map_err(|e| refuse(e.to_string()))?;
    let std = cmd.as_std();
    let program = std::path::Path::new(std.get_program())
        .file_name()
        .ok_or_else(|| refuse("no program".into()))?
        .to_string_lossy()
        .into_owned();
    let cwd = std
        .get_current_dir()
        .ok_or_else(|| refuse("no working folder".into()))?;
    let cwd = cwd
        .canonicalize()
        .map_err(|e| refuse(format!("{}: {e}", cwd.display())))?;
    let values: Vec<(String, String)> = std
        .get_envs()
        .filter_map(|(key, value)| Some((key.to_str()?.to_owned(), value?.to_str()?.to_owned())))
        .filter(|(key, _)| {
            (env.contains_key(key) || key.starts_with("CLAUDE_CODE_") || key.starts_with("ZERON_"))
                && !matches!(
                    key.as_str(),
                    "PATH" | "HOME" | "CLAUDE_CONFIG_DIR" | "CODEX_HOME" | SPEC_ENV | MCP_ENV
                )
        })
        .collect();
    let names: Vec<String> = values.iter().map(|(key, _)| key.clone()).collect();
    // This machine's CLI, for an image without one of its own: the
    // container's login shells look in `<run dir>/bin` last
    // (`zeron_engine::sandbox`).
    #[cfg(unix)]
    if let Err(error) = provide_cli(
        std::path::Path::new(std.get_program()),
        std::path::Path::new(&spec.run_dir),
        &program,
    ) {
        tracing::warn!(%error, "sandbox: couldn't hand this machine's CLI to the container");
    }
    let command: Vec<String> = std::iter::once(program)
        .chain(std.get_args().map(|arg| arg.to_string_lossy().into_owned()))
        .collect();
    let mut wrapped = Command::new(&spec.helper);
    wrapped
        .args(spec.exec_args(&cwd.to_string_lossy(), false, &names, &command))
        .envs(values)
        .env_remove(SPEC_ENV);
    match env.get(MCP_ENV) {
        Some(mcp) => wrapped.env(MCP_ENV, mcp),
        None => wrapped.env_remove(MCP_ENV),
    };
    // Its own group: teardown reaches the helper, whose exit ends the run's
    // processes in the container.
    crate::process::owned::configure(&mut wrapped);
    wrapped
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    Ok(wrapped)
}

/// Puts the CLI `exe` at `<run_dir>/bin/<name>`, unless it's there already: a
/// hard link, else (another filesystem, or a file another user owns) a copy
/// with its modification time, so an unchanged CLI is never copied again.
#[cfg(unix)]
fn provide_cli(
    exe: &std::path::Path,
    run_dir: &std::path::Path,
    name: &str,
) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let real = exe.canonicalize()?;
    let source = std::fs::metadata(&real)?;
    let dir = run_dir.join("bin");
    let target = dir.join(name);
    if std::fs::metadata(&target).is_ok_and(|t| {
        (t.dev(), t.ino()) == (source.dev(), source.ino())
            || (t.len(), t.mtime()) == (source.len(), source.mtime())
    }) {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let staged = dir.join(format!(".{name}.{}.{nanos}", std::process::id()));
    if std::fs::hard_link(&real, &staged).is_err() {
        std::fs::copy(&real, &staged)?;
        std::fs::File::options()
            .write(true)
            .open(&staged)?
            .set_modified(source.modified()?)?;
    }
    std::fs::rename(&staged, &target)
}

/// This machine's Claude Code plugins, for a sandboxed run: its CLI has a home
/// of its own, so they load with `--plugin-dir` from `~/.claude/plugins/cache`,
/// which the container mounts read-only at the same path
/// (`zeron_engine::sandbox`). One turned off here stays off.
pub(crate) fn claude_plugin_dirs(
    home: &std::path::Path,
    cwd: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let config = home.join(".claude");
    let cache = config.join("plugins/cache");
    let settings: serde_json::Value = std::fs::read(config.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    crate::skills::claude_plugin_installs(&config, cwd)
        .into_iter()
        .filter(|(id, path)| {
            path.starts_with(&cache)
                && settings["enabledPlugins"][id.as_str()] != false
                && seen.insert(id.clone())
        })
        .map(|(_, path)| path)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_sandbox_gets_this_machines_cli_once_per_version() {
        use std::os::unix::fs::MetadataExt;
        let temp = tempfile::tempdir().unwrap();
        let (cli, run) = (temp.path().join("claude-2.1"), temp.path().join("run"));
        std::fs::write(&cli, "v1").unwrap();
        provide_cli(&cli, &run, "claude").unwrap();
        let provided = run.join("bin/claude");
        let ino = |p: &std::path::Path| std::fs::metadata(p).unwrap().ino();
        assert_eq!(
            ino(&provided),
            ino(&cli),
            "a hard link on the same filesystem"
        );
        provide_cli(&cli, &run, "claude").unwrap();
        assert_eq!(ino(&provided), ino(&cli), "unchanged: left alone");
        let newer = temp.path().join("claude-2.2");
        std::fs::write(&newer, "v2!").unwrap();
        provide_cli(&newer, &run, "claude").unwrap();
        assert_eq!(std::fs::read_to_string(&provided).unwrap(), "v2!");
        assert_eq!(
            std::fs::read_dir(run.join("bin")).unwrap().count(),
            1,
            "nothing staged is left behind"
        );
    }

    #[test]
    fn a_sandboxed_claude_loads_this_machines_installed_plugins_that_are_on() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join(".claude/plugins/cache");
        let install = json_path;
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            home.path().join(".claude/plugins/installed_plugins.json"),
            format!(
                r#"{{"plugins":{{
                    "on@m":[{{"installPath":{}}},{{"installPath":{}}}],
                    "off@m":[{{"installPath":{}}}],
                    "elsewhere@m":[{{"installPath":"/opt/elsewhere"}}],
                    "project@m":[{{"installPath":{},"projectPath":"/other/project"}}]
                }}}}"#,
                install(&cache.join("m/on/1")),
                install(&cache.join("m/on/2")),
                install(&cache.join("m/off/1")),
                install(&cache.join("m/project/1")),
            ),
        )
        .unwrap();
        std::fs::write(
            home.path().join(".claude/settings.json"),
            r#"{"enabledPlugins":{"on@m":true,"off@m":false}}"#,
        )
        .unwrap();
        assert_eq!(
            claude_plugin_dirs(home.path(), std::path::Path::new("/repo")),
            [cache.join("m/on/1")],
            "only cached installs that apply here and aren't off, once each"
        );
    }

    fn json_path(path: &std::path::Path) -> String {
        serde_json::to_string(&path.to_string_lossy()).unwrap()
    }

    #[test]
    fn a_stamped_run_execs_the_same_cli_in_the_container_with_only_its_own_variables() {
        let spec = Spec {
            helper: "/h/zeron-sandbox".into(),
            container: "c".into(),
            run_dir: "/r".into(),
        };
        let env: BTreeMap<String, String> = [
            (SPEC_ENV.to_owned(), serde_json::to_string(&spec).unwrap()),
            (MCP_ENV.to_owned(), "records".to_owned()),
            ("GH_TOKEN".to_owned(), "t".to_owned()),
        ]
        .into();
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let mut cmd = Command::new("/usr/local/bin/claude");
        cmd.args(["--print", "--verbose"])
            .current_dir(&dir)
            .env("GH_TOKEN", "t")
            .env("CLAUDE_CODE_FORWARD_SUBAGENT_TEXT", "1")
            .env("PATH", "/host/bin")
            .env("AWS_SECRET_ACCESS_KEY", "host secret")
            .env(SPEC_ENV, &env[SPEC_ENV]);
        let wrapped = wrap(cmd, &env).unwrap();
        let std = wrapped.as_std();
        assert_eq!(std.get_program(), "/h/zeron-sandbox");
        let args: Vec<_> = std
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let tail = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(&args[tail + 1..], ["claude", "--print", "--verbose"]);
        assert!(args.contains(&dir.to_string_lossy().into_owned()));
        let forwarded: Vec<_> = args
            .windows(2)
            .filter(|w| w[0] == "--env")
            .map(|w| w[1].clone())
            .collect();
        assert_eq!(forwarded.len(), 2, "{forwarded:?}");
        assert!(
            forwarded.contains(&"GH_TOKEN".to_owned())
                && forwarded.contains(&"CLAUDE_CODE_FORWARD_SUBAGENT_TEXT".to_owned())
        );
        assert!(
            !args
                .iter()
                .any(|a| a.contains("host secret") || a == "PATH")
        );
        let envs: BTreeMap<_, _> = std
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(envs[MCP_ENV].as_deref(), Some("records"));
        assert_eq!(envs[SPEC_ENV], None, "the spec never reaches the container");

        let mut plain = Command::new("claude");
        plain.arg("-p");
        assert_eq!(
            wrap(plain, &BTreeMap::new())
                .unwrap()
                .as_std()
                .get_program(),
            "claude"
        );
        let broken: BTreeMap<String, String> =
            [(SPEC_ENV.to_owned(), "not json".to_owned())].into();
        assert!(
            wrap(Command::new("claude"), &broken).is_err(),
            "a stamped run never falls back to the host"
        );
    }
}
