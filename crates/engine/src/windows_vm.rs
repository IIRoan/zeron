//! A project's Windows VM (`docs/windows-vm.md`): which projects may use the
//! VM on this machine, which of their ports cross to it, and the `winvm`
//! tool (`scripts/winvm`) that runs it.
//!
//! - The VM itself (disk, QEMU, its SSH key) belongs to `winvm`, outside the
//!   engine's data. The engine keeps only the registry
//!   (`<data dir>/windows-vm.json`) and runs `winvm` for the projects in it.
//! - The registry is written by `zeron vm` and the app's project settings,
//!   never by an agent: an agent can use its project's VM, not connect it to
//!   more of this machine.
//! - The guest reaches the internet and the listed `to_vm` ports, nothing
//!   else here: not this machine's other localhost ports, so not Zeron's own
//!   either. That is `winvm`'s network setup; nothing in this module widens it.
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::EngineError;

/// Ports listed per direction, at most.
pub const MAX_PORTS: usize = 64;
/// What a tool call returns of a command's output, per stream (its end).
const OUTPUT_LIMIT: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsVm {
    /// The project's main folder.
    pub root: PathBuf,
    /// Ports on this machine the VM reaches on its own localhost (the
    /// project's dev servers: a sandboxed project's published ports).
    #[serde(default)]
    pub to_vm: Vec<u16>,
    /// VM ports this machine reaches on its localhost (a debugger).
    #[serde(default)]
    pub from_vm: Vec<u16>,
    /// Open the viewer window when the VM starts.
    #[serde(default)]
    pub viewer: bool,
    /// Names of the project's services that start the VM with them (the
    /// Windows app's own service): starting one starts the VM too.
    #[serde(default)]
    pub start_with: Vec<String>,
    /// Connect the ports the project's servers listen on (in any of its
    /// worktrees, sandboxed or not) without listing them: `to_vm` then only
    /// adds to them.
    #[serde(default = "yes")]
    pub auto_ports: bool,
}

fn yes() -> bool {
    true
}

#[derive(Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    projects: Vec<WindowsVm>,
}

fn data_dir() -> PathBuf {
    std::env::var_os("ZERON_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::repos::home_dir().join(".zeron"))
}

fn registry_path() -> PathBuf {
    data_dir().join("windows-vm.json")
}

fn load() -> Result<Vec<WindowsVm>, EngineError> {
    match std::fs::read(registry_path()) {
        Ok(bytes) => serde_json::from_slice::<Registry>(&bytes)
            .map(|registry| registry.projects)
            .map_err(|e| {
                EngineError::Other(format!(
                    "{} is unreadable ({e}): fix it, or run zeron vm disable",
                    registry_path().display()
                ))
            }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

fn save(projects: Vec<WindowsVm>) -> Result<(), EngineError> {
    std::fs::create_dir_all(data_dir())?;
    let mut file = tempfile::NamedTempFile::new_in(data_dir())?;
    std::io::Write::write_all(
        &mut file,
        &serde_json::to_vec_pretty(&Registry { projects })
            .map_err(|e| EngineError::Other(e.to_string()))?,
    )?;
    file.persist(registry_path()).map_err(|e| e.error)?;
    Ok(())
}

fn resolve(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Sorted, without repeats; refuses port 0, too many, and a port listed in
/// both directions (each side would need it free and taken at once).
fn normalize(mut vm: WindowsVm) -> Result<WindowsVm, EngineError> {
    for (what, ports) in [
        ("to the VM", &mut vm.to_vm),
        ("from the VM", &mut vm.from_vm),
    ] {
        ports.sort_unstable();
        ports.dedup();
        if ports.contains(&0) {
            return Err(EngineError::Other(format!(
                "Port 0 can't be connected {what}"
            )));
        }
        if ports.len() > MAX_PORTS {
            return Err(EngineError::Other(format!(
                "At most {MAX_PORTS} ports can be connected {what}"
            )));
        }
    }
    if let Some(port) = vm.to_vm.iter().find(|port| vm.from_vm.contains(port)) {
        return Err(EngineError::Other(format!(
            "Port {port} is listed in both directions: it can only go one way"
        )));
    }
    vm.start_with = vm
        .start_with
        .iter()
        .map(|name| name.trim().to_owned())
        .collect();
    vm.start_with.retain(|name| !name.is_empty());
    vm.start_with.sort_unstable();
    vm.start_with.dedup();
    Ok(vm)
}

/// Whether one of the services `started` (their names) is one the project
/// starts the VM with.
fn starts_with(vm: &WindowsVm, started: &[&str]) -> bool {
    vm.start_with.iter().any(|name| {
        started
            .iter()
            .any(|service| service.trim().eq_ignore_ascii_case(name))
    })
}

/// A service start for the project at `root`: when it names one of the
/// project's `start_with` services, start the VM too, in the background.
/// The service's own start never waits for Windows or fails with it; the
/// settings page and `vm_status` show how the VM's start went.
pub fn start_with_services(root: &Path, started: &[&str]) {
    let Ok(Some(vm)) = for_root(root) else {
        return;
    };
    if !starts_with(&vm, started) {
        return;
    }
    tokio::spawn(async move {
        match start(&vm).await {
            Ok(status) => {
                if let Some(error) = status["tunnelError"].as_str() {
                    tracing::warn!(
                        error,
                        "windows vm: started with a service, ports not connected"
                    );
                }
            }
            Err(error) => tracing::warn!(%error, "windows vm: couldn't start with a service"),
        }
    });
}

/// Every project connected to the VM.
pub fn projects() -> Result<Vec<WindowsVm>, EngineError> {
    load()
}

/// The project at `root` (exactly), if it is connected to the VM.
pub fn for_root(root: &Path) -> Result<Option<WindowsVm>, EngineError> {
    let root = resolve(root);
    Ok(load()?.into_iter().find(|vm| vm.root == root))
}

/// Connect a project to the VM, or replace how it is connected. Starts
/// nothing: [`start`] does.
pub fn enable(mut vm: WindowsVm) -> Result<WindowsVm, EngineError> {
    if !vm.root.is_dir() {
        return Err(EngineError::Other(format!(
            "{} is not a folder",
            vm.root.display()
        )));
    }
    vm.root = resolve(&vm.root);
    let vm = normalize(vm)?;
    let mut projects = load()?;
    projects.retain(|other| other.root != vm.root);
    projects.push(vm.clone());
    projects.sort_by(|a, b| a.root.cmp(&b.root));
    save(projects)?;
    Ok(vm)
}

/// Stop treating `root` as connected. The VM itself is left as it is.
pub fn disable(root: &Path) -> Result<(), EngineError> {
    let root = resolve(root);
    let mut projects = load()?;
    let before = projects.len();
    projects.retain(|vm| vm.root != root);
    if projects.len() == before {
        return Err(EngineError::Other(format!(
            "{} is not connected to the Windows VM",
            root.display()
        )));
    }
    save(projects)
}

/// `winvm`: `ZERON_WINVM`, else `~/.local/bin/winvm`, else on `PATH`.
pub fn tool() -> PathBuf {
    if let Some(path) = std::env::var_os("ZERON_WINVM") {
        return PathBuf::from(path);
    }
    let local = crate::repos::home_dir().join(".local/bin/winvm");
    if local.is_file() {
        local
    } else {
        PathBuf::from("winvm")
    }
}

/// Whether this machine can run the VM at all.
pub fn available() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let tool = tool();
    tool.is_file()
        || std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(&tool).is_file()))
}

struct Ran {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Ran {
    fn error(&self) -> EngineError {
        let text = String::from_utf8_lossy(&self.stderr);
        let text = text.trim().trim_start_matches("winvm: ");
        EngineError::Other(if text.is_empty() {
            format!("winvm failed (exit {:?})", self.code)
        } else {
            text.to_owned()
        })
    }
}

/// Run `winvm args`, stopping it at `timeout`.
async fn run(args: &[&str], timeout: Duration) -> Result<Ran, EngineError> {
    let mut command = tokio::process::Command::new(tool());
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| {
            EngineError::Other(format!(
                "winvm {} gave no result within {} s",
                args.first().copied().unwrap_or_default(),
                timeout.as_secs()
            ))
        })?
        .map_err(|e| {
            EngineError::Other(format!(
                "Can't run {} ({e}): see docs/windows-vm.md for its setup",
                tool().display()
            ))
        })?;
    Ok(Ran {
        code: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn port_list(ports: &[u16]) -> String {
    ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// `winvm up`'s (or `winvm ports`') arguments for these ports.
fn port_args(command: &str, ports: &Ports, viewer: bool) -> Vec<String> {
    let mut args = vec![command.to_owned(), "--json".to_owned()];
    if !ports.to_vm.is_empty() {
        args.extend(["--to-vm".to_owned(), port_list(&ports.to_vm)]);
    }
    if !ports.from_vm.is_empty() {
        args.extend(["--from-vm".to_owned(), port_list(&ports.from_vm)]);
    }
    if viewer {
        args.push("--view".to_owned());
    }
    args
}

/// The ports that cross, each way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Ports {
    to_vm: Vec<u16>,
    from_vm: Vec<u16>,
}

/// A project's folders: its main one and where its worktrees are made.
fn folders(vm: &WindowsVm) -> [PathBuf; 2] {
    [vm.root.clone(), crate::sandbox::default_worktrees(&vm.root)]
}

/// The ports every connected project has: the ones each lists, and for a
/// project with `auto_ports` the ones in `listening` (a server's folder and
/// port) under its folders. Never a port this machine keeps for itself
/// (`reserved`) or a privileged one found listening; a port a project wants
/// from the VM stays that way. At most [`MAX_PORTS`], listed ports first.
fn wanted_ports(projects: &[WindowsVm], listening: &[(PathBuf, u16)], reserved: &[u16]) -> Ports {
    let mut from_vm: Vec<u16> = projects.iter().flat_map(|vm| vm.from_vm.clone()).collect();
    from_vm.sort_unstable();
    from_vm.dedup();
    from_vm.truncate(MAX_PORTS);
    let allowed = |port: &u16| !from_vm.contains(port) && !reserved.contains(port);
    let mut to_vm: Vec<u16> = projects
        .iter()
        .flat_map(|vm| vm.to_vm.clone())
        .filter(allowed)
        .collect();
    to_vm.sort_unstable();
    to_vm.dedup();
    let automatic: Vec<[PathBuf; 2]> = projects
        .iter()
        .filter(|vm| vm.auto_ports)
        .map(folders)
        .collect();
    let mut found: Vec<u16> = listening
        .iter()
        .filter(|(folder, port)| {
            *port >= 1024
                && allowed(port)
                && automatic
                    .iter()
                    .any(|own| own.iter().any(|dir| folder.starts_with(dir)))
        })
        .map(|(_, port)| *port)
        .filter(|port| !to_vm.contains(port))
        .collect();
    found.sort_unstable();
    found.dedup();
    to_vm.extend(found);
    to_vm.truncate(MAX_PORTS);
    to_vm.sort_unstable();
    Ports { to_vm, from_vm }
}

/// Every server listening on this machine's loopback now, as its folder and
/// port (sandbox containers' published servers included). `None`: the scan
/// itself failed, which is not "nothing listens".
async fn listening() -> Option<Vec<(PathBuf, u16)>> {
    tokio::task::spawn_blocking(|| {
        let mut origins = zeron_preview::discovery::Origins::default();
        zeron_preview::discovery::listeners(&mut origins).map(|found| {
            found
                .into_iter()
                .map(|listener| (listener.cwd, listener.address.port()))
                .collect()
        })
    })
    .await
    .ok()
    .flatten()
}

fn reserved() -> Vec<u16> {
    let mut ports = crate::sandbox::reserved_ports(&[]);
    ports.extend(zeron_harness::agent_ports::reserved());
    ports
}

/// How long a port stays connected after its server stopped listening: a
/// dev server that restarts keeps its port.
const PORT_LINGER: Duration = Duration::from_secs(45);

/// While the engine runs: keep the running VM's ports the ones the connected
/// projects' servers listen on, so nobody lists a worktree's ports by hand.
/// Ports are added and dropped on the live connection (`winvm ports`); the
/// ones that stay keep their open connections.
pub fn keep_ports_current() {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if STARTED.set(()).is_err() || !cfg!(target_os = "linux") {
        return;
    }
    tokio::spawn(async move {
        let mut seen: std::collections::HashMap<(PathBuf, u16), std::time::Instant> =
            Default::default();
        let mut applied: Option<Ports> = None;
        let mut idle = 0u32;
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let projects = projects().unwrap_or_default();
            if !projects.iter().any(|vm| vm.auto_ports) || !available() {
                applied = None;
                continue;
            }
            // A stopped VM is asked about only every half minute.
            if idle > 0 {
                idle -= 1;
                continue;
            }
            let Some(found) = listening().await else {
                continue;
            };
            let now = std::time::Instant::now();
            for server in found {
                seen.insert(server, now);
            }
            seen.retain(|_, last| now.duration_since(*last) < PORT_LINGER);
            let lingering: Vec<(PathBuf, u16)> = seen.keys().cloned().collect();
            let wanted = wanted_ports(&projects, &lingering, &reserved());
            if applied.as_ref() == Some(&wanted) {
                continue;
            }
            let running = status()
                .await
                .is_ok_and(|vm| vm["state"].as_str() == Some("running"));
            if !running {
                applied = None;
                idle = 5;
                continue;
            }
            let args = port_args("ports", &wanted, false);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            match run(&args, Duration::from_secs(60)).await {
                // 5: connected, except ports that are taken on the other side.
                Ok(ran) if matches!(ran.code, Some(0 | 5)) => {
                    tracing::info!(to_vm = ?wanted.to_vm, "windows vm: ports connected");
                    applied = Some(wanted);
                }
                Ok(ran) => {
                    tracing::warn!(error = %ran.error(), "windows vm: couldn't connect ports")
                }
                Err(error) => tracing::warn!(%error, "windows vm: couldn't connect ports"),
            }
        }
    });
}

/// The VM as `winvm status --json` reports it: `state` is `stopped`,
/// `starting` or `running`; `toVm` / `fromVm` are the connected ports.
pub async fn status() -> Result<Value, EngineError> {
    let ran = run(&["status", "--json"], Duration::from_secs(20)).await?;
    if ran.code != Some(0) {
        return Err(ran.error());
    }
    serde_json::from_slice(&ran.stdout)
        .map_err(|e| EngineError::Other(format!("winvm status: {e}")))
}

/// Start the VM if it is not running, wait for Windows, and connect the
/// connected projects' ports (`vm` decides whether the viewer opens).
/// Returns its status; a `tunnelError` there means the VM is up but some
/// ports are not connected.
pub async fn start(vm: &WindowsVm) -> Result<Value, EngineError> {
    let ports = wanted_ports(
        &projects()?,
        &listening().await.unwrap_or_default(),
        &reserved(),
    );
    let args = port_args("up", &ports, vm.viewer);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let ran = run(&args, Duration::from_secs(700)).await?;
    match serde_json::from_slice::<Value>(&ran.stdout) {
        Ok(status) => Ok(status),
        Err(_) => Err(ran.error()),
    }
}

/// The guest's health in one reading: load, memory, disk, display, its
/// biggest processes, listening ports, stopped services and the last hour's
/// errors (`winvm diag`).
pub async fn diagnostics() -> Result<Value, EngineError> {
    let ran = run(&["diag"], Duration::from_secs(120)).await?;
    match serde_json::from_slice::<Value>(&ran.stdout) {
        Ok(health) if ran.code == Some(0) => Ok(health),
        _ => Err(ran.error()),
    }
}

/// Shut Windows down and stop the VM.
pub async fn stop() -> Result<(), EngineError> {
    let ran = run(&["down"], Duration::from_secs(400)).await?;
    if ran.code == Some(0) {
        Ok(())
    } else {
        Err(ran.error())
    }
}

/// Open the viewer window on this machine's desktop.
pub async fn view() -> Result<(), EngineError> {
    let ran = run(&["view"], Duration::from_secs(20)).await?;
    if ran.code == Some(0) {
        Ok(())
    } else {
        Err(ran.error())
    }
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    if text.len() <= OUTPUT_LIMIT {
        return text.into_owned();
    }
    let mut start = text.len() - OUTPUT_LIMIT;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[… earlier output dropped]\n{}", &text[start..])
}

/// `winvm`'s own "not running" (3) or "still starting" (4), which it says
/// on stderr and nothing else. A command in the guest may exit with the same
/// codes: that is the command's result.
fn not_running(ran: &Ran) -> bool {
    matches!(ran.code, Some(3 | 4)) && ran.stdout.is_empty() && ran.stderr.starts_with(b"winvm: ")
}

/// Run a PowerShell command in the guest as its administrator. There is no
/// desktop in that session: [`launch`] starts programs with windows.
pub async fn exec(
    command: &str,
    cwd: Option<&str>,
    timeout_secs: u64,
) -> Result<Value, EngineError> {
    let timeout = timeout_secs.to_string();
    let mut args = vec!["exec", "--timeout", &timeout];
    if let Some(cwd) = cwd {
        args.extend(["--cwd", cwd]);
    }
    args.extend(["--", command]);
    let ran = run(&args, Duration::from_secs(timeout_secs + 30)).await?;
    if not_running(&ran) {
        return Err(ran.error());
    }
    Ok(json!({
        "exitCode": ran.code,
        "timedOut": ran.code == Some(124),
        "stdout": tail(&ran.stdout),
        "stderr": tail(&ran.stderr),
    }))
}

/// Start a program on the guest's logged-in desktop.
pub async fn launch(command: &str) -> Result<(), EngineError> {
    let ran = run(&["launch", "--", command], Duration::from_secs(90)).await?;
    if ran.code == Some(0) {
        Ok(())
    } else {
        Err(ran.error())
    }
}

/// The guest's screen as a PNG, and its size.
pub async fn screenshot() -> Result<(Vec<u8>, Value), EngineError> {
    let file = tempfile::Builder::new()
        .prefix("winvm-")
        .suffix(".png")
        .tempfile()?;
    let path = file.path().to_string_lossy().into_owned();
    let ran = run(&["screenshot", &path, "--json"], Duration::from_secs(40)).await?;
    if ran.code != Some(0) {
        return Err(ran.error());
    }
    let info = serde_json::from_slice(&ran.stdout).unwrap_or(Value::Null);
    Ok((std::fs::read(file.path())?, info))
}

/// Pointer and keyboard input on the guest's screen: `action` is `click`,
/// `doubleclick`, `rightclick`, `move` (with `x y`), `type` (text) or `key`
/// (combinations like `ctrl+s`).
pub async fn input(action: &str, values: &[String]) -> Result<(), EngineError> {
    let mut args = vec!["input", action];
    args.extend(values.iter().map(String::as_str));
    let ran = run(&args, Duration::from_secs(120)).await?;
    if ran.code == Some(0) {
        Ok(())
    } else {
        Err(ran.error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm(to_vm: &[u16], from_vm: &[u16]) -> WindowsVm {
        WindowsVm {
            root: "/project".into(),
            to_vm: to_vm.to_vec(),
            from_vm: from_vm.to_vec(),
            viewer: false,
            start_with: Vec::new(),
            auto_ports: false,
        }
    }

    #[test]
    fn a_projects_listening_servers_are_its_ports_without_being_listed() {
        let mut project = vm(&[9000], &[9222]);
        project.auto_ports = true;
        let other = WindowsVm {
            root: "/other".into(),
            ..vm(&[3000], &[])
        };
        let worktree = crate::sandbox::default_worktrees(Path::new("/project")).join("feature");
        let listening = [
            (PathBuf::from("/project/apps/frontend"), 3352),
            (worktree, 3353),
            (PathBuf::from("/project"), 9222), // wanted from the VM: stays that way
            (PathBuf::from("/project"), 27654), // this machine's own
            (PathBuf::from("/project"), 80),   // privileged
            (PathBuf::from("/other/app"), 4000), // a project that lists its ports itself
            (PathBuf::from("/elsewhere"), 5000), // nobody's
        ];
        let ports = wanted_ports(&[project.clone(), other], &listening, &[27654]);
        assert_eq!(ports.to_vm, [3000, 3352, 3353, 9000]);
        assert_eq!(ports.from_vm, [9222]);
        // Listed ports are kept when there are too many.
        let many: Vec<_> = (2000..2100)
            .map(|port| (PathBuf::from("/project"), port))
            .collect();
        let ports = wanted_ports(&[project], &many, &[]);
        assert_eq!(ports.to_vm.len(), MAX_PORTS);
        assert!(ports.to_vm.contains(&9000));
    }

    #[test]
    fn a_registry_without_the_setting_connects_ports_automatically() {
        let old: WindowsVm = serde_json::from_str(r#"{"root":"/project","toVm":[3000]}"#).unwrap();
        assert!(old.auto_ports);
    }

    #[test]
    fn a_guest_exit_code_is_not_mistaken_for_a_stopped_vm() {
        let ran = |stdout: &str, stderr: &str| Ran {
            code: Some(3),
            stdout: stdout.into(),
            stderr: stderr.into(),
        };
        assert!(not_running(&ran(
            "",
            "winvm: the VM is not running (winvm up)\n"
        )));
        assert!(!not_running(&ran("", "")));
        assert!(!not_running(&ran("three\n", "")));
        assert!(!not_running(&ran("", "robocopy: files were copied\n")));
    }

    #[test]
    fn the_vm_starts_with_the_services_the_project_names() {
        let mut project = vm(&[3000], &[]);
        project.start_with = vec![" Desktop ".into(), "desktop".into(), "".into()];
        let project = normalize(project).unwrap();
        assert_eq!(project.start_with, ["Desktop", "desktop"]);
        assert!(starts_with(&project, &["api", "desktop"]));
        assert!(!starts_with(&project, &["api", "desktop-mac"]));
        assert!(!starts_with(&vm(&[3000], &[]), &["desktop"]));
    }

    #[test]
    fn ports_are_sorted_deduplicated_and_one_way() {
        let normal = normalize(vm(&[3001, 3000, 3001], &[9222])).unwrap();
        assert_eq!(
            (normal.to_vm, normal.from_vm),
            (vec![3000, 3001], vec![9222])
        );
        for bad in [
            vm(&[0], &[]),
            vm(&[3000], &[3000]),
            vm(&(1..=65).collect::<Vec<_>>(), &[]),
        ] {
            assert!(normalize(bad).is_err());
        }
    }

    #[test]
    fn up_names_only_the_ports_there_are() {
        assert_eq!(port_args("up", &Ports::default(), false), ["up", "--json"]);
        let ports = Ports {
            to_vm: vec![3000, 3001],
            from_vm: vec![9222],
        };
        assert_eq!(
            port_args("up", &ports, true),
            [
                "up",
                "--json",
                "--to-vm",
                "3000,3001",
                "--from-vm",
                "9222",
                "--view"
            ]
        );
    }

    #[test]
    fn long_output_keeps_its_end() {
        assert_eq!(tail(b"short"), "short");
        let long = "é".repeat(OUTPUT_LIMIT);
        let kept = tail(long.as_bytes());
        assert!(kept.starts_with("[… earlier output dropped]\n") && kept.ends_with('é'));
        assert!(kept.len() <= OUTPUT_LIMIT + 40);
    }
}
