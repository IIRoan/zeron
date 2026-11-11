//! Commit signing for a sandbox whose project turned it on: git there runs
//! the helper as its `gpg.program`, which hands what git signs to this
//! socket. This machine signs it as its own git would (`git config --global`
//! `user.signingkey` and `gpg.format`: gpg, gpgsm or ssh-keygen, with their
//! agents), but only git's own signing call (`--status-fd=2 -bsau`) on a
//! commit or tag object: agents never hold the key, choose another, decrypt,
//! or sign anything else.
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// Signing requests served at once.
const MAX_CONNECTIONS: usize = 4;
/// The most a commit or tag to sign may be.
const MAX_OBJECT: u64 = 4 * 1024 * 1024;

/// The sockets served, each with the flag that ends its listener.
static SERVING: std::sync::Mutex<Vec<(PathBuf, Arc<AtomicBool>)>> =
    std::sync::Mutex::new(Vec::new());

/// Serves `socket`, once, until [`stop`].
pub(super) fn serve(socket: &Path) {
    let mut serving = SERVING.lock().unwrap_or_else(|e| e.into_inner());
    if serving.iter().any(|(served, _)| served == socket) {
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
    let stopped = Arc::new(AtomicBool::new(false));
    serving.push((socket.to_path_buf(), stopped.clone()));
    let active = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for client in listener.incoming().flatten() {
            if stopped.load(Ordering::SeqCst) {
                break;
            }
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

/// Stops serving `socket` and removes it: its project doesn't sign commits
/// (any more), so nothing in its container may reach this machine's key.
pub(super) fn stop(socket: &Path) {
    let mut serving = SERVING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = serving.iter().position(|(served, _)| served == socket) {
        let (_, stopped) = serving.remove(at);
        stopped.store(true, Ordering::SeqCst);
        // Wakes the listener, which sees the flag and ends.
        let _ = UnixStream::connect(socket);
    }
    // Also one an earlier engine process left behind.
    let _ = std::fs::remove_file(socket);
}

/// How this machine's git signs: the program and key of its `gpg.format`.
pub(super) struct Signer {
    program: String,
    key: String,
    ssh: bool,
}

pub(super) fn signer() -> Result<Signer, String> {
    let config = super::host_git_config;
    let key = config("user.signingkey")
        .ok_or("This machine's git has no signing key (git config --global user.signingkey)")?;
    let format = config("gpg.format").unwrap_or_else(|| "openpgp".into());
    let own = config(&format!("gpg.{format}.program"));
    let (program, ssh) = match format.as_str() {
        "openpgp" => (own.or_else(|| config("gpg.program")), false),
        "x509" => (own.or(Some("gpgsm".into())), false),
        "ssh" => (own.or(Some("ssh-keygen".into())), true),
        other => {
            return Err(format!(
                "This machine's git signs with gpg.format {other}, which Zeron's sandbox doesn't know"
            ));
        }
    };
    Ok(Signer {
        program: program.unwrap_or_else(|| "gpg".into()),
        key,
        ssh,
    })
}

/// A signing program's exit code, what it said on stderr, and its stdout.
type Signed = (i32, Vec<u8>, Vec<u8>);

/// One request: `<args, one per line>\n\n<object>`; the answer is
/// `<exit code>\n<stderr length>\n<stderr><stdout>`.
fn handle(
    mut client: UnixStream,
    signer: &dyn Fn() -> Result<Signer, String>,
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

fn sign(request: &[u8], signer: &dyn Fn() -> Result<Signer, String>) -> Result<Signed, String> {
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
    let Signer { program, key, ssh } = signer()?;
    if ssh {
        return sign_ssh(&program, &key, object);
    }
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

/// Signs as git does with `gpg.format ssh`: `ssh-keygen -Y sign -n git -f
/// <key> <file>` leaves the signature in `<file>.sig`. git in the container
/// asked its "gpg", so the answer reads as gpg's: git takes the signature as
/// it comes, and whoever verifies it tells its kind by its armor.
fn sign_ssh(program: &str, key: &str, object: &[u8]) -> Result<Signed, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let buffer = dir.path().join("object");
    std::fs::write(&buffer, object).map_err(|e| e.to_string())?;
    let mut command = Command::new(program);
    command.args(["-Y", "sign", "-n", "git", "-f"]);
    // A key written out in the config is looked up in the agent (`-U`);
    // anything else names its file.
    match key
        .strip_prefix("key::")
        .or(key.starts_with("ssh-").then_some(key))
    {
        Some(public) => {
            let file = dir.path().join("key.pub");
            std::fs::write(&file, public).map_err(|e| e.to_string())?;
            command.arg(file).arg("-U")
        }
        None => match key.strip_prefix("~/") {
            Some(rest) => command.arg(crate::repos::home_dir().join(rest)),
            None => command.arg(key),
        },
    };
    let output = command
        .arg(&buffer)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !output.status.success() {
        return Ok((output.status.code().unwrap_or(2), output.stderr, Vec::new()));
    }
    let signature = std::fs::read(buffer.with_extension("sig")).map_err(|e| e.to_string())?;
    Ok((0, b"\n[GNUPG:] SIG_CREATED \n".to_vec(), signature))
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
                let _ = handle(client, &move || {
                    Ok(Signer {
                        program: program.clone(),
                        key: "HOSTKEY".into(),
                        ssh: false,
                    })
                });
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

    #[test]
    fn a_socket_is_gone_once_its_project_stops_signing() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("sign.sock");
        serve(&socket);
        assert!(UnixStream::connect(&socket).is_ok());
        stop(&socket);
        assert!(!socket.exists());
        assert!(UnixStream::connect(&socket).is_err());
        // Turned on again: served again.
        serve(&socket);
        assert!(UnixStream::connect(&socket).is_ok());
        stop(&socket);
    }

    #[test]
    fn an_ssh_key_signs_what_git_verifies_as_an_ssh_signature() {
        let temp = tempfile::tempdir().unwrap();
        let key = temp.path().join("key");
        let made = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return; // no ssh-keygen on this machine
        }
        let (code, status, signature) =
            sign_ssh("ssh-keygen", &key.to_string_lossy(), COMMIT.as_bytes()).unwrap();
        assert_eq!(code, 0);
        // What git looks for in its gpg's status output.
        assert!(String::from_utf8_lossy(&status).contains("\n[GNUPG:] SIG_CREATED "));
        assert!(signature.starts_with(b"-----BEGIN SSH SIGNATURE-----"));
    }
}
