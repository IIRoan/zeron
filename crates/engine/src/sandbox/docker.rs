//! A read-only Docker API in each sandbox (its `DOCKER_HOST`), so a project's
//! tooling sees the project's shared services (its databases) running on this
//! machine, as `docker compose ps` asks. It lists and inspects only containers
//! started from the project's folders, in Compose v2's terms whatever made
//! them (podman-compose's labels filled in), and as members of whichever
//! compose project asks: shared services, the same from every checkout.
//! Nothing else: no create, run, exec, logs or changes, and no environment
//! variables (they can hold secrets).
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Map, Value, json};

/// Connections served at once; more are dropped, so agents can't tie up the
/// engine.
const MAX_CONNECTIONS: usize = 16;
/// The most an answer of this machine's container engine may be.
const MAX_RESPONSE: u64 = 32 * 1024 * 1024;
const WORKING_DIR: &str = "com.docker.compose.project.working_dir";
const PROJECT: &str = "com.docker.compose.project";

/// Serves `socket` for the project whose folders are `folders` (main and
/// worktrees), once per engine process.
pub(super) fn serve(socket: &Path, folders: [PathBuf; 2]) {
    static SERVING: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    let mut serving = SERVING.lock().unwrap_or_else(|e| e.into_inner());
    if serving.iter().any(|s| s == socket) {
        return;
    }
    let Some(upstream) = upstream() else {
        tracing::warn!("sandbox: no container engine on this machine to show the sandbox");
        return;
    };
    let _ = std::fs::remove_file(socket);
    let listener = match UnixListener::bind(socket) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, socket = %socket.display(), "sandbox: can't serve its Docker API");
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
            let (active, upstream, folders) = (active.clone(), upstream.clone(), folders.clone());
            std::thread::spawn(move || {
                let _ = handle(client, &upstream, &folders);
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
}

/// This machine's Docker API: `DOCKER_HOST`, podman's user socket, Docker's.
fn upstream() -> Option<PathBuf> {
    let from_env = std::env::var("DOCKER_HOST")
        .ok()
        .and_then(|host| host.strip_prefix("unix://").map(PathBuf::from));
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    [
        from_env,
        Some(runtime.join("podman/podman.sock")),
        Some(PathBuf::from("/var/run/docker.sock")),
    ]
    .into_iter()
    .flatten()
    .find(|socket| socket.exists())
}

struct Reply {
    status: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn json_reply(status: &str, value: &Value) -> Reply {
    Reply {
        status: status.into(),
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: value.to_string().into_bytes(),
    }
}

fn refused() -> Reply {
    json_reply(
        "403 Forbidden",
        &json!({"message": "Zeron's sandbox: Docker here only reads this project's containers on this machine (its shared services)"}),
    )
}

fn unreachable() -> Reply {
    json_reply(
        "502 Bad Gateway",
        &json!({"message": "Zeron's sandbox: this machine's container engine didn't answer"}),
    )
}

fn handle(mut client: UnixStream, upstream: &Path, folders: &[PathBuf; 2]) -> std::io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 64 * 1024 || client.read(&mut byte)? == 0 {
            return Ok(());
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    let mut line = head.lines().next().unwrap_or_default().split(' ');
    let (method, target) = (
        line.next().unwrap_or_default(),
        line.next().unwrap_or_default(),
    );
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = without_version(path);
    let reply = match (method, path) {
        ("GET" | "HEAD", "/_ping") | ("GET", "/version") => {
            request(upstream, method, target).unwrap_or_else(unreachable)
        }
        ("GET", "/containers/json") => list(upstream, query, folders),
        ("GET", path) => match path
            .strip_prefix("/containers/")
            .and_then(|rest| rest.strip_suffix("/json"))
        {
            Some(id) => inspect(upstream, id, folders),
            None => refused(),
        },
        _ => refused(),
    };
    let mut out = format!("HTTP/1.1 {}\r\n", reply.status);
    for (key, value) in &reply.headers {
        if !matches!(
            key.to_ascii_lowercase().as_str(),
            "content-length" | "transfer-encoding" | "connection"
        ) {
            out.push_str(&format!("{key}: {value}\r\n"));
        }
    }
    out.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        reply.body.len()
    ));
    client.write_all(out.as_bytes())?;
    if method != "HEAD" {
        client.write_all(&reply.body)?;
    }
    Ok(())
}

/// `/v1.44/containers/json` → `/containers/json`.
fn without_version(path: &str) -> &str {
    path.strip_prefix("/v")
        .and_then(|rest| rest.split_once('/'))
        .filter(|(version, _)| version.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .map_or(path, |(version, _)| &path[version.len() + 2..])
}

/// One request to this machine's engine, answered whole (HTTP/1.0).
fn request(upstream: &Path, method: &str, target: &str) -> Option<Reply> {
    let mut stream = UnixStream::connect(upstream).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok()?;
    write!(stream, "{method} {target} HTTP/1.0\r\nHost: docker\r\n\r\n").ok()?;
    let mut bytes = Vec::new();
    stream.take(MAX_RESPONSE).read_to_end(&mut bytes).ok()?;
    let end = bytes.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&bytes[..end]).ok()?;
    let mut lines = head.split("\r\n");
    let status = lines.next()?.split_once(' ')?.1.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let mut body = bytes[end + 4..].to_vec();
    if headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked")
    }) {
        body = dechunk(&body)?;
    }
    Some(Reply {
        status,
        headers,
        body,
    })
}

fn dechunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line = body.windows(2).position(|w| w == b"\r\n")?;
        let size = std::str::from_utf8(&body[..line]).ok()?;
        let size = usize::from_str_radix(size.split(';').next()?.trim(), 16).ok()?;
        if size == 0 {
            return Some(out);
        }
        let data = body.get(line + 2..line + 2 + size)?;
        out.extend_from_slice(data);
        body = body.get(line + 4 + size..)?;
    }
}

/// Whether a container's compose labels say it was started from `folders`.
fn ours(labels: &Map<String, Value>, folders: &[PathBuf; 2]) -> bool {
    labels
        .get(WORKING_DIR)
        .and_then(Value::as_str)
        .is_some_and(|dir| folders.iter().any(|f| Path::new(dir).starts_with(f)))
}

/// The labels Compose v2 looks for, from podman-compose's where missing.
fn as_compose_v2(labels: &mut Map<String, Value>) {
    let hash = labels
        .get("io.podman.compose.config-hash")
        .cloned()
        .unwrap_or_else(|| "zeron".into());
    labels
        .entry("com.docker.compose.config-hash")
        .or_insert(hash);
    labels
        .entry("com.docker.compose.oneoff")
        .or_insert("False".into());
}

/// `GET /containers/json`: this project's containers, matched against the
/// request's label filters, all in the compose project it names.
fn list(upstream: &Path, query: &str, folders: &[PathBuf; 2]) -> Reply {
    let param = |name: &str| {
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| crate::auth::url_decode(value))
    };
    let filters: Value = param("filters")
        .and_then(|f| serde_json::from_str(&f).ok())
        .unwrap_or_default();
    let wanted: Vec<String> = match &filters["label"] {
        Value::Object(labels) => labels
            .iter()
            .filter(|(_, on)| on.as_bool() != Some(false))
            .map(|(label, _)| label.clone())
            .collect(),
        Value::Array(labels) => labels
            .iter()
            .filter_map(|l| l.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    let project = wanted
        .iter()
        .find_map(|rule| rule.strip_prefix(&format!("{PROJECT}=")));
    let all = param("all").filter(|a| matches!(a.as_str(), "1" | "true"));
    let target = format!("/containers/json?all={}", all.is_some());
    let Some(reply) = request(upstream, "GET", &target) else {
        return unreachable();
    };
    let Ok(Value::Array(containers)) = serde_json::from_slice::<Value>(&reply.body) else {
        return unreachable();
    };
    let shown: Vec<Value> = containers
        .into_iter()
        .filter_map(|mut container| {
            let labels = container.get_mut("Labels")?.as_object_mut()?;
            if !ours(labels, folders) {
                return None;
            }
            as_compose_v2(labels);
            if let Some(project) = project {
                labels.insert(PROJECT.into(), project.into());
            }
            let matches = wanted.iter().all(|rule| match rule.split_once('=') {
                Some((key, _)) if key == PROJECT => true,
                Some((key, value)) => labels.get(key).and_then(Value::as_str) == Some(value),
                None => labels.contains_key(rule),
            });
            matches.then_some(container)
        })
        .collect();
    json_reply("200 OK", &Value::Array(shown))
}

/// `GET /containers/{id}/json`: one of this project's containers, without its
/// environment.
fn inspect(upstream: &Path, id: &str, folders: &[PathBuf; 2]) -> Reply {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return refused();
    }
    let Some(reply) = request(upstream, "GET", &format!("/containers/{id}/json")) else {
        return unreachable();
    };
    let Ok(mut container) = serde_json::from_slice::<Value>(&reply.body) else {
        return refused();
    };
    let Some(config) = container.get_mut("Config").and_then(Value::as_object_mut) else {
        return refused();
    };
    match config.get_mut("Labels").and_then(Value::as_object_mut) {
        Some(labels) if ours(labels, folders) => as_compose_v2(labels),
        _ => return refused(),
    }
    config.insert("Env".into(), json!([]));
    json_reply("200 OK", &container)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A container engine answering `answers` (path → JSON) and recording
    /// what it was asked.
    fn engine(dir: &Path, answers: Vec<(&'static str, Value)>) -> PathBuf {
        let socket = dir.join("engine.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                let line = String::from_utf8_lossy(&head)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let target = line.split(' ').nth(1).unwrap_or_default();
                let body = answers
                    .iter()
                    .find(|(path, _)| target.starts_with(path))
                    .map_or_else(|| "{}".to_owned(), |(_, value)| value.to_string());
                let _ = write!(
                    stream,
                    "HTTP/1.0 200 OK\r\nAPI-Version: 1.44\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
                    body.len()
                );
            }
        });
        socket
    }

    fn ask(socket: &Path, request: &str) -> (String, Value) {
        let mut stream = UnixStream::connect(socket).unwrap();
        write!(stream, "{request} HTTP/1.1\r\nHost: docker\r\n\r\n").unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        let (head, body) = reply.split_once("\r\n\r\n").unwrap();
        (
            head.lines().next().unwrap().to_owned(),
            serde_json::from_str(body).unwrap_or(Value::Null),
        )
    }

    #[test]
    fn a_sandbox_reads_only_its_projects_containers_as_compose_does() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("muute");
        let folders = [root.clone(), temp.path().join("worktrees")];
        let container = |name: &str, dir: &Path| {
            json!({"Names": [name], "State": "running", "Labels": {
                WORKING_DIR: dir, PROJECT: "muute", "com.docker.compose.service": "db",
                "io.podman.compose.config-hash": "abc"}})
        };
        let upstream = engine(
            temp.path(),
            vec![
                (
                    "/containers/json",
                    json!([
                        container("db", &root),
                        container("theirs", &temp.path().join("other"))
                    ]),
                ),
                (
                    "/containers/db/json",
                    json!({"Config": {"Env": ["SECRET=1"], "Labels": {WORKING_DIR: root}}}),
                ),
                (
                    "/containers/theirs/json",
                    json!({"Config": {"Labels": {WORKING_DIR: "/elsewhere"}}}),
                ),
            ],
        );
        let socket = temp.path().join("sandbox.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (up, fs) = (upstream.clone(), folders.clone());
        std::thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let _ = handle(client, &up, &fs);
            }
        });

        // Compose v2's `ps` from a worktree: its own project name, its labels.
        let filters = "%7B%22label%22%3A%7B%22com.docker.compose.config-hash%22%3Atrue%2C%22com.docker.compose.oneoff%3DFalse%22%3Atrue%2C%22com.docker.compose.project%3Dbright-tree%22%3Atrue%2C%22com.docker.compose.service%3Ddb%22%3Atrue%7D%7D";
        let (status, listed) = ask(
            &socket,
            &format!("GET /v1.44/containers/json?filters={filters}"),
        );
        assert_eq!(status, "HTTP/1.1 200 OK");
        let listed = listed.as_array().unwrap();
        assert_eq!(listed.len(), 1, "only this project's: {listed:?}");
        assert_eq!(listed[0]["Labels"][PROJECT], "bright-tree");
        assert_eq!(listed[0]["Labels"]["com.docker.compose.config-hash"], "abc");

        let (status, inspected) = ask(&socket, "GET /v1.44/containers/db/json");
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(inspected["Config"]["Env"], json!([]), "no environment");
        assert!(
            ask(&socket, "GET /v1.44/containers/theirs/json")
                .0
                .contains("403")
        );
        assert!(
            ask(&socket, "GET /v1.44/containers/..%2F..%2Fx/json")
                .0
                .contains("403")
        );
        for refused in [
            "POST /v1.44/containers/create",
            "POST /v1.44/containers/db/exec",
            "GET /v1.44/containers/db/logs",
            "DELETE /v1.44/containers/db",
        ] {
            assert!(ask(&socket, refused).0.contains("403"), "{refused}");
        }
        assert_eq!(ask(&socket, "GET /_ping").0, "HTTP/1.1 200 OK");
    }
}
