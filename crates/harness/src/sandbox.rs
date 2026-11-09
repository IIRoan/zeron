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

#[cfg(test)]
mod tests {
    use super::*;

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
