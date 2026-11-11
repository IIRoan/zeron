//! Commit signing for a sandbox whose project turned it on: git there runs
//! the helper as its `gpg.program`, which hands what git signs to this
//! socket. This machine signs it with its own signing key (`git config
//! --global user.signingkey`, through its gpg and agent), but only git's own
//! signing call (`--status-fd=2 -bsau`) on a commit or tag object: agents
//! never hold the key, choose another, decrypt, or sign anything else.
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Signing requests served at once.
const MAX_CONNECTIONS: usize = 4;
/// The most a commit or tag to sign may be.
const MAX_OBJECT: u64 = 4 * 1024 * 1024;

/// Serves `socket` once per engine process.
pub(super) fn serve(socket: &Path) {
    static SERVING: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    let mut serving = SERVING.lock().unwrap_or_else(|e| e.into_inner());
    if serving.iter().any(|s| s == socket) {
        return;
    }
    let _ = std::fs::remove_file(socket);
    let listener = match UnixListener::bind(socket) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, socket = %socket.display(), "sandbox: can't serve commit signing");
            return;
        }
    };
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));
    serving.push(socket.to_path_buf());
    let active = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for client in listener.incoming().flatten() {
            if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                active.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            let active = active.clone();
            std::thread::spawn(move || {
                let _ = handle(client, &signer);
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
}

/// This machine's signing: its git's gpg program and signing key.
fn signer() -> Result<(String, String), String> {
    let global = |key: &str| {
        Command::new("git")
            .args(["config", "--global", "--get", key])
            .stdin(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let key = global("user.signingkey")
        .ok_or("This machine's git has no user.signingkey to sign with")?;
    Ok((global("gpg.program").unwrap_or_else(|| "gpg".into()), key))
}

/// One request: `<args, one per line>\n\n<object>`; the answer is
/// `<exit code>\n<stderr length>\n<stderr><stdout>`.
fn handle(
    mut client: UnixStream,
    signer: &dyn Fn() -> Result<(String, String), String>,
) -> std::io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut request = Vec::new();
    (&mut client)
        .take(MAX_OBJECT + 4096)
        .read_to_end(&mut request)?;
    let (code, stderr, stdout) = match sign(&request, signer) {
        Ok(signed) => signed,
        Err(why) => (2, format!("gpg: {why}\n").into_bytes(), Vec::new()),
    };
    client.write_all(format!("{code}\n{}\n", stderr.len()).as_bytes())?;
    client.write_all(&stderr)?;
    client.write_all(&stdout)
}

fn sign(
    request: &[u8],
    signer: &dyn Fn() -> Result<(String, String), String>,
) -> Result<(i32, Vec<u8>, Vec<u8>), String> {
    let split = request
        .windows(2)
        .position(|w| w == b"\n\n")
        .ok_or("a request without arguments")?;
    let args: Vec<&str> = std::str::from_utf8(&request[..split])
        .map_err(|_| "arguments that aren't text")?
        .split('\n')
        .collect();
    let object = &request[split + 2..];
    if !matches!(args.as_slice(), ["--status-fd=2", "-bsau", _]) {
        return Err("Zeron's sandbox signs commits and tags only (git's -bsau); verify signatures on this machine".into());
    }
    if !is_commit_or_tag(object) {
        return Err("Zeron's sandbox signs git commits and tags only".into());
    }
    let (program, key) = signer()?;
    let mut child = Command::new(&program)
        .args(["--status-fd=2", "-bsau", &key])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("gpg has no stdin")?
        .write_all(object)
        .map_err(|e| e.to_string())?;
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    Ok((
        output.status.code().unwrap_or(2),
        output.stderr,
        output.stdout,
    ))
}

/// A git commit (`tree <id>` … `committer`) or annotated tag (`object <id>`,
/// `type`, `tag`, `tagger`), by its header.
fn is_commit_or_tag(object: &[u8]) -> bool {
    let end = object
        .windows(2)
        .position(|w| w == b"\n\n")
        .unwrap_or(object.len());
    let Ok(header) = std::str::from_utf8(&object[..end]) else {
        return false;
    };
    let lines: Vec<&str> = header.lines().collect();
    let id = |line: &str, field: &str| {
        line.strip_prefix(field)
            .and_then(|rest| rest.strip_prefix(' '))
            .is_some_and(|id| {
                matches!(id.len(), 40 | 64) && id.chars().all(|c| c.is_ascii_hexdigit())
            })
    };
    let has = |field: &str| lines.iter().any(|l| l.starts_with(&format!("{field} ")));
    match lines.first() {
        Some(first) if id(first, "tree") => has("committer"),
        Some(first) if id(first, "object") => has("type") && has("tag") && has("tagger"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A <a@x> 1 +0000\ncommitter A <a@x> 1 +0000\n\nmessage\n";

    fn ask(socket: &Path, args: &str, object: &str) -> (String, String) {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream
            .write_all(format!("{args}\n\n{object}").as_bytes())
            .unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        let (code, rest) = reply.split_once('\n').unwrap();
        let (length, rest) = rest.split_once('\n').unwrap();
        let length: usize = length.parse().unwrap();
        (
            code.to_owned(),
            rest[..length].to_owned() + "|" + &rest[length..],
        )
    }

    #[test]
    fn a_sandbox_gets_commits_and_tags_signed_and_nothing_else() {
        let temp = tempfile::tempdir().unwrap();
        // A gpg that signs with what it was asked for and says so.
        let gpg = temp.path().join("gpg");
        std::fs::write(
            &gpg,
            "#!/bin/sh\necho \"[GNUPG:] SIG_CREATED D $*\" >&2\ncat >/dev/null\necho SIGNATURE\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gpg, std::fs::Permissions::from_mode(0o755)).unwrap();
        let socket = temp.path().join("sign.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let program = gpg.to_string_lossy().into_owned();
        std::thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let program = program.clone();
                let _ = handle(client, &move || Ok((program.clone(), "HOSTKEY".into())));
            }
        });

        let (code, out) = ask(&socket, "--status-fd=2\n-bsau\nAGENTKEY", COMMIT);
        assert_eq!(code, "0");
        assert!(
            out.contains("SIG_CREATED D --status-fd=2 -bsau HOSTKEY")
                && out.ends_with("|SIGNATURE\n"),
            "signed with this machine's key, whatever was asked: {out}"
        );
        let tag = "object 4b825dc642cb6eb9a060e54bf8d69288fbee4904\ntype commit\ntag v1\ntagger A <a@x> 1 +0000\n\nv1\n";
        assert_eq!(ask(&socket, "--status-fd=2\n-bsau\nK", tag).0, "0");

        for (args, object) in [
            ("--status-fd=2\n-bsau\nK", "please sign this contract\n"),
            ("--status-fd=2\n-bsau\nK", "tree nothex\ncommitter A\n\nm\n"),
            ("--decrypt", COMMIT),
            (
                "--keyid-format=long\n--status-fd=1\n--verify\n/tmp/sig\n-",
                COMMIT,
            ),
        ] {
            let (code, out) = ask(&socket, args, object);
            assert_eq!(code, "2", "{args}: {out}");
            assert!(!out.contains("SIG_CREATED"), "gpg never ran: {out}");
        }
    }
}
