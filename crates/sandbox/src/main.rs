//! Runs Zeron's agents and terminals inside a project's sandbox container.
//!
//! `exec` runs on the host: it holds a lease (an flock on a file in the
//! project's run folder, which the container sees at the same path), serves
//! the run's MCP servers on unix sockets there, and starts `podman exec`.
//! `guard` runs inside the container: it starts the command in its own process
//! group and ends that group once the lease is free, because killing a
//! `podman exec` client leaves its process running. `relay` connects an MCP
//! client in the container to one of `exec`'s sockets; `idle` keeps the
//! container alive.
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio, exit};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
    fn getpid() -> i32;
    fn isatty(fd: i32) -> i32;
    fn tcsetpgrp(fd: i32, pgrp: i32) -> i32;
    fn signal(signal: i32, handler: usize) -> usize;
}

const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;
const SIGHUP: i32 = 1;
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
const SIGTTOU: i32 = 22;
const SIG_DFL: usize = 0;
const SIG_IGN: usize = 1;
/// Exit status when the host side is gone before or while the command runs.
const HOST_GONE: i32 = 125;
/// How long `exec` waits for the guard to end its command. The engine stops a
/// service within 3 s, and the guard's escalation (`end_group`) fits in that.
const GUARD_WAIT: Duration = Duration::from_secs(10);
/// MCP servers the host side serves for this run, set by Zeron.
const MCP_ENV: &str = "ZERON_SANDBOX_MCP";
const FIELD: char = '\u{1f}';
const RECORD: char = '\u{1e}';

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("exec") => host_exec(&args[1..]),
        Some("guard") => guard(&args[1..]),
        Some("relay") => relay(&args[1..]),
        Some("idle") => loop {
            thread::sleep(Duration::from_secs(3600));
        },
        _ => {
            eprintln!("usage: zeron-sandbox exec|guard|relay|idle ...");
            2
        }
    };
    exit(code);
}

fn fail(message: impl std::fmt::Display) -> i32 {
    eprintln!("zeron-sandbox: {message}");
    HOST_GONE
}

struct ExecOptions {
    engine: String,
    container: String,
    run_dir: PathBuf,
    guard: String,
    cwd: String,
    tty: bool,
    forward: Vec<String>,
    command: Vec<String>,
}

fn parse_exec(args: &[String]) -> Result<ExecOptions, String> {
    let mut options = ExecOptions {
        engine: "podman".into(),
        container: String::new(),
        run_dir: PathBuf::new(),
        guard: String::new(),
        cwd: String::new(),
        tty: false,
        forward: Vec::new(),
        command: Vec::new(),
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = || rest.next().cloned().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--engine" => options.engine = value()?,
            "--container" => options.container = value()?,
            "--run-dir" => options.run_dir = value()?.into(),
            "--guard" => options.guard = value()?,
            "--cwd" => options.cwd = value()?,
            "--env" => options.forward.push(value()?),
            "--tty" => options.tty = true,
            "--" => {
                options.command = rest.cloned().collect();
                break;
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    if options.container.is_empty() || options.guard.is_empty() || options.cwd.is_empty() {
        return Err("--container, --guard and --cwd are required".into());
    }
    if options.command.is_empty() {
        return Err("no command after --".into());
    }
    Ok(options)
}

fn host_exec(args: &[String]) -> i32 {
    let options = match parse_exec(args) {
        Ok(options) => options,
        Err(message) => return fail(message),
    };
    let leases = options.run_dir.join("leases");
    if let Err(error) = fs::create_dir_all(&leases) {
        return fail(format!("{}: {error}", leases.display()));
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let lease_path = leases.join(format!("{}-{nanos}", std::process::id()));
    // Opened close-on-exec, so neither podman nor conmon inherits it: the lock
    // is held exactly as long as this process lives.
    let lease = match File::create(&lease_path) {
        Ok(file) => file,
        Err(error) => return fail(format!("{}: {error}", lease_path.display())),
    };
    if unsafe { flock(lease.as_raw_fd(), LOCK_EX) } != 0 {
        return fail(format!("could not lock {}", lease_path.display()));
    }
    let held = guard_path(&lease_path);
    if let Err(error) = File::create(&held) {
        return fail(format!("{}: {error}", held.display()));
    }
    let sockets = match serve_mcp(std::env::var(MCP_ENV).unwrap_or_default()) {
        Ok(sockets) => sockets,
        Err(message) => return fail(message),
    };

    let mut podman = Command::new(&options.engine);
    podman.args(["exec", "-i"]);
    if options.tty {
        podman.arg("-t");
    }
    podman.args(["-w", &options.cwd]);
    for name in &options.forward {
        podman.args(["-e", name]);
    }
    podman
        .arg(&options.container)
        .arg(&options.guard)
        .arg("guard")
        .arg(&lease_path)
        .arg("--")
        .args(&options.command)
        .env_remove(MCP_ENV);
    let code = match podman.status() {
        Ok(status) => code_of(status),
        Err(error) => fail(format!("{}: {error}", options.engine)),
    };
    // Killing the podman client leaves the command running until the guard
    // sees the lease free. Wait for it, so whoever stopped this process knows
    // the command, and the ports it held, are gone (a service handing over to
    // another checkout starts its replacement right after).
    drop(lease);
    wait_for_guard(&held);
    let _ = fs::remove_file(&lease_path);
    let _ = fs::remove_file(&held);
    for socket in sockets {
        let _ = fs::remove_file(socket);
    }
    code
}

/// One unix socket per MCP server. Each connection starts the server on the
/// host with its own arguments and environment and splices it to the socket.
fn serve_mcp(spec: String) -> Result<Vec<PathBuf>, String> {
    let mut sockets = Vec::new();
    for record in spec.split(RECORD).filter(|r| !r.is_empty()) {
        let fields: Vec<&str> = record.split(FIELD).collect();
        let (socket, command, count) = match fields.as_slice() {
            [socket, command, count, ..] => (
                PathBuf::from(socket),
                command.to_string(),
                count.parse::<usize>().map_err(|e| e.to_string())?,
            ),
            _ => return Err("malformed MCP record".into()),
        };
        let args: Vec<String> = fields
            .iter()
            .skip(3)
            .take(count)
            .map(|s| s.to_string())
            .collect();
        let env: Vec<(String, String)> = fields
            .iter()
            .skip(3 + count)
            .filter_map(|kv| {
                kv.split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
            })
            .collect();
        let _ = fs::remove_file(&socket);
        let listener =
            UnixListener::bind(&socket).map_err(|e| format!("{}: {e}", socket.display()))?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        sockets.push(socket);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (command, args, env) = (command.clone(), args.clone(), env.clone());
                thread::spawn(move || {
                    let _ = splice_server(stream, &command, &args, &env);
                });
            }
        });
    }
    Ok(sockets)
}

fn splice_server(
    stream: UnixStream,
    command: &str,
    args: &[String],
    env: &[(String, String)],
) -> io::Result<()> {
    let mut child = Command::new(command)
        .args(args)
        .envs(env.iter().cloned())
        .env_remove(MCP_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let to_child = child.stdin.take().expect("piped stdin");
    let from_child = child.stdout.take().expect("piped stdout");
    let reader = stream.try_clone()?;
    let writer = stream.try_clone()?;
    let inbound = thread::spawn(move || pump(reader, to_child));
    pump(from_child, writer);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = inbound.join();
    let _ = child.kill();
    child.wait().map(|_| ())
}

fn guard(args: &[String]) -> i32 {
    let (lease_path, command) = match args {
        [lease, separator, command @ ..] if separator == "--" && !command.is_empty() => {
            (lease, command)
        }
        _ => return fail("usage: zeron-sandbox guard <lease> -- <command...>"),
    };
    let Ok(lease) = File::open(lease_path) else {
        return fail("the host session is gone");
    };
    // Held until this process exits, so `exec` waits for the command to end.
    // Taken before the lease check: once `exec` has the lock, it no longer
    // waits, so nothing may start. (An older `exec` makes no such file.)
    let held = File::open(guard_path(Path::new(lease_path))).ok();
    if held
        .as_ref()
        .is_some_and(|file| unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0)
        || host_gone(&lease)
    {
        return fail("the host session is gone");
    }
    let mut child = login_command(command);
    child.process_group(0);
    let tty = unsafe { isatty(0) } == 1;
    if tty {
        // The command owns the terminal, so job control and Ctrl-C reach it.
        unsafe {
            child.pre_exec(|| {
                signal(SIGTTOU, SIG_IGN);
                tcsetpgrp(0, getpid());
                signal(SIGTTOU, SIG_DFL);
                Ok(())
            });
        }
    }
    let mut child = match child.spawn() {
        Ok(child) => child,
        Err(error) => return fail(format!("{}: {error}", command[0])),
    };
    let group = child.id() as i32;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return code_of(status),
            Ok(None) => {}
            Err(error) => return fail(error),
        }
        if host_gone(&lease) {
            end_group(group, &mut child);
            return HOST_GONE;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// The host side holds the lease while it lives; getting it means it is gone.
fn host_gone(lease: &File) -> bool {
    unsafe { flock(lease.as_raw_fd(), LOCK_EX | LOCK_NB) == 0 }
}

/// What the guard holds while its command runs, beside the lease.
fn guard_path(lease: &Path) -> PathBuf {
    lease.with_extension("guard")
}

/// Returns once no guard holds `path`, or after [`GUARD_WAIT`].
fn wait_for_guard(path: &Path) {
    let Ok(file) = File::open(path) else { return };
    let started = std::time::Instant::now();
    while unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0
        && started.elapsed() < GUARD_WAIT
    {
        thread::sleep(Duration::from_millis(20));
    }
}

/// Hang up first (an interactive shell passes it to its jobs), then escalate,
/// all within the 3 s the engine gives a stopping service.
fn end_group(group: i32, child: &mut std::process::Child) {
    for (signal, tenths) in [(SIGHUP, 10), (SIGTERM, 15), (SIGKILL, 0)] {
        unsafe { kill(-group, signal) };
        for _ in 0..tenths {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    let _ = child.wait();
}

/// `@shell` is the image's interactive login shell; anything else runs through
/// a login `sh`, so it is found on the image's own PATH.
fn login_command(command: &[String]) -> Command {
    if command[0] == "@shell" {
        let shell = if on_path("bash") { "bash" } else { "sh" };
        let mut cmd = Command::new(shell);
        cmd.arg("-l").args(&command[1..]);
        return cmd;
    }
    let mut cmd = Command::new("sh");
    cmd.args(["-l", "-c", "exec \"$0\" \"$@\""]).args(command);
    cmd
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| is_file(&dir.join(name))))
        .unwrap_or(false)
}

fn is_file(path: &Path) -> bool {
    path.metadata().map(|m| m.is_file()).unwrap_or(false)
}

fn relay(args: &[String]) -> i32 {
    let [socket] = args else {
        return fail("usage: zeron-sandbox relay <socket>");
    };
    let stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(error) => return fail(format!("{socket}: {error}")),
    };
    let writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(error) => return fail(error),
    };
    thread::spawn(move || {
        pump(io::stdin(), &writer);
        let _ = writer.shutdown(std::net::Shutdown::Write);
    });
    pump(stream, io::stdout());
    0
}

/// Copies `from` to `to` as data arrives, until either end closes. Not
/// `io::copy`: between a unix socket and a pipe it splices, and that holds
/// what it read until the socket's EOF, so an MCP request sat unanswered
/// while its client kept the connection open.
fn pump(mut from: impl Read, mut to: impl Write) {
    let mut buffer = [0u8; 16 * 1024];
    while let Ok(read) = from.read(&mut buffer) {
        if read == 0
            || to
                .write_all(&buffer[..read])
                .and_then(|_| to.flush())
                .is_err()
        {
            break;
        }
    }
}

fn code_of(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_relayed_server_answers_while_its_client_keeps_the_connection_open() {
        let (client, server) = UnixStream::pair().unwrap();
        thread::spawn(move || splice_server(server, "cat", &[], &[]));
        let mut writer = client.try_clone().unwrap();
        writer.write_all(b"{\"id\":1}\n").unwrap();
        // No EOF: an MCP client waits for the reply first.
        let mut reader = client;
        reader
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reply = [0u8; 9];
        reader.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"{\"id\":1}\n");
    }

    #[test]
    fn parses_exec_options_and_command() {
        let options = parse_exec(&args(&[
            "--container",
            "c",
            "--guard",
            "/g",
            "--cwd",
            "/repo",
            "--run-dir",
            "/run",
            "--env",
            "A",
            "--tty",
            "--",
            "claude",
            "--print",
        ]))
        .unwrap();
        assert_eq!(options.container, "c");
        assert_eq!(options.forward, ["A"]);
        assert!(options.tty);
        assert_eq!(options.command, ["claude", "--print"]);
        assert!(parse_exec(&args(&["--container", "c", "--", "x"])).is_err());
    }

    #[test]
    fn lease_is_free_only_after_its_holder_closes_it() {
        let dir = std::env::temp_dir().join(format!("zeron-sandbox-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lease");
        let holder = File::create(&path).unwrap();
        assert_eq!(unsafe { flock(holder.as_raw_fd(), LOCK_EX) }, 0);
        assert!(!host_gone(&File::open(&path).unwrap()));
        drop(holder);
        assert!(host_gone(&File::open(&path).unwrap()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn guard_ends_the_command_when_the_lease_frees() {
        let dir = std::env::temp_dir().join(format!("zeron-sandbox-guard-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lease");
        let holder = File::create(&path).unwrap();
        unsafe { flock(holder.as_raw_fd(), LOCK_EX) };
        let lease = path.to_string_lossy().into_owned();
        let started = std::time::Instant::now();
        let worker = thread::spawn(move || guard(&args(&[&lease, "--", "sleep", "30"])));
        thread::sleep(Duration::from_millis(500));
        drop(holder);
        assert_eq!(worker.join().unwrap(), HOST_GONE);
        assert!(started.elapsed() < Duration::from_secs(10));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn exec_waits_until_the_guard_has_ended_its_command() {
        let dir = std::env::temp_dir().join(format!("zeron-sandbox-wait-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lease");
        let holder = File::create(&path).unwrap();
        unsafe { flock(holder.as_raw_fd(), LOCK_EX) };
        File::create(guard_path(&path)).unwrap();
        let lease = path.to_string_lossy().into_owned();
        // Ignores the hang-up, so it ends only at the SIGTERM a second later.
        let worker = thread::spawn(move || {
            guard(&args(&[&lease, "--", "sh", "-c", "trap '' HUP; sleep 30"]))
        });
        thread::sleep(Duration::from_millis(500));
        let freed = std::time::Instant::now();
        drop(holder);
        wait_for_guard(&guard_path(&path));
        assert!(
            freed.elapsed() >= Duration::from_secs(1),
            "{:?}",
            freed.elapsed()
        );
        assert_eq!(worker.join().unwrap(), HOST_GONE);
        fs::remove_dir_all(dir).unwrap();
    }
}
