//! What a sandboxed run carries to its harness (in `RunRequest::env`), and the
//! `zeron-sandbox exec` command line that both the engine's terminals and the
//! harnesses build from it. The engine decides what is sandboxed
//! (`zeron_engine::sandbox`); this is only the shape of the hand-off.
use serde::{Deserialize, Serialize};

/// JSON [`Spec`]: run this request's CLI inside the container.
pub const SPEC_ENV: &str = "ZERON_SANDBOX";
/// The run's MCP servers, served on the host by `zeron-sandbox exec`.
pub const MCP_ENV: &str = "ZERON_SANDBOX_MCP";
/// Where the helper is mounted (read-only) inside every sandbox container.
pub const GUARD: &str = "/opt/zeron/zeron-sandbox";
const FIELD: char = '\u{1f}';
const RECORD: char = '\u{1e}';

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spec {
    /// The helper on the host.
    pub helper: String,
    pub container: String,
    /// Leases, MCP sockets and action scripts; mounted at the same path.
    pub run_dir: String,
}

impl Spec {
    /// Arguments for [`Spec::helper`]: run `command` in `cwd` inside the
    /// container, forwarding the variables named in `forward` from the
    /// helper's own environment (so their values stay out of argv).
    pub fn exec_args(
        &self,
        cwd: &str,
        tty: bool,
        forward: &[String],
        command: &[String],
    ) -> Vec<String> {
        let mut args = vec![
            "exec".into(),
            "--container".into(),
            self.container.clone(),
            "--guard".into(),
            GUARD.into(),
            "--run-dir".into(),
            self.run_dir.clone(),
            "--cwd".into(),
            cwd.into(),
        ];
        if tty {
            args.push("--tty".into());
        }
        for name in forward {
            args.extend(["--env".into(), name.clone()]);
        }
        args.push("--".into());
        args.extend(command.iter().cloned());
        args
    }
}

/// One [`MCP_ENV`] record: serve `command args` (with `env`) on `socket`.
pub fn mcp_record(
    socket: &str,
    command: &str,
    args: &[String],
    env: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut fields = vec![
        socket.to_owned(),
        command.to_owned(),
        args.len().to_string(),
    ];
    fields.extend(args.iter().cloned());
    fields.extend(env.iter().map(|(k, v)| format!("{k}={v}")));
    fields.join(&FIELD.to_string())
}

/// [`MCP_ENV`]'s value from its records.
pub fn mcp_records(records: &[String]) -> String {
    records.join(&RECORD.to_string())
}

/// `ports` as pasta takes them: runs of consecutive ports as `a-b`.
pub fn port_ranges(ports: &[u16]) -> Vec<String> {
    let mut sorted = ports.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut ranges: Vec<(u16, u16)> = Vec::new();
    for port in sorted {
        match ranges.last_mut() {
            Some((_, end)) if end.checked_add(1) == Some(port) => *end = port,
            _ => ranges.push((port, port)),
        }
    }
    ranges
        .into_iter()
        .map(|(a, b)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}-{b}")
            }
        })
        .collect()
}

/// Ports written as numbers or `a-b` ranges, separated by commas or spaces.
pub fn parse_ports(text: &str) -> Result<Vec<u16>, String> {
    let mut ports = Vec::new();
    for part in text
        .split([',', ' ', '\n'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let number = |p: &str| {
            p.trim()
                .parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or_else(|| format!("{part} isn't a port or a range of ports"))
        };
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b) = (number(a)?, number(b)?);
                if a > b {
                    return Err(format!("{part} isn't a port range"));
                }
                ports.extend(a..=b);
            }
            None => ports.push(number(part)?),
        }
    }
    Ok(ports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_args_put_the_command_after_the_separator() {
        let spec = Spec {
            helper: "/h".into(),
            container: "c".into(),
            run_dir: "/r".into(),
        };
        let args = spec.exec_args(
            "/repo",
            true,
            &["A".into()],
            &["claude".into(), "-p".into()],
        );
        assert_eq!(
            args,
            [
                "exec",
                "--container",
                "c",
                "--guard",
                GUARD,
                "--run-dir",
                "/r",
                "--cwd",
                "/repo",
                "--tty",
                "--env",
                "A",
                "--",
                "claude",
                "-p"
            ]
        );
        let env = [("K".to_string(), "v".to_string())].into();
        assert_eq!(
            mcp_record("/s", "zeron", &["mcp".into()], &env),
            "/s\u{1f}zeron\u{1f}1\u{1f}mcp\u{1f}K=v"
        );
    }

    #[test]
    fn ports_read_and_print_as_ranges() {
        assert_eq!(
            parse_ports("3000, 3300-3302\n6006 3001").unwrap(),
            vec![3000, 3300, 3301, 3302, 6006, 3001]
        );
        assert!(parse_ports("3000, http").unwrap_err().contains("http"));
        assert!(parse_ports("3302-3300").is_err());
        assert!(parse_ports("0").is_err());
        assert_eq!(
            port_ranges(&[3001, 3300, 3301, 3000, 3302, 6006, 3000]),
            vec!["3000-3001", "3300-3302", "6006"]
        );
    }
}
