#![cfg(unix)]
use std::{path::Path, time::Duration};
use zeron_engine::{Terminals, project_terminals::ProjectTerminals};
use zeron_proto::{ProjectTerminalConfig, ProjectTerminalService, ProjectTerminalStatus as Status};

fn service(id: &str, command: &str, restart: bool) -> ProjectTerminalService {
    ProjectTerminalService {
        id: id.into(),
        name: id.into(),
        command: command.into(),
        directory: ".".into(),
        restart_on_failure: restart,
    }
}

async fn wait_for(store: &ProjectTerminals, root: &Path, id: &str, status: Status) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if store
                .snapshot("project", root)
                .unwrap()
                .runs
                .iter()
                .any(|r| r.service_id == id && r.status == status)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("service reaches expected status");
}

#[tokio::test]
async fn start_all_reuses_processes_and_stop_all_kills_child_processes() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let store = ProjectTerminals::open(state.path()).unwrap();
    let terminals = Terminals::new();
    store
        .save(
            "project",
            root.path(),
            ProjectTerminalConfig {
                services: vec![
                    service(
                        "backend",
                        "/bin/sh -c 'echo $$ > backend.pid; exec sleep 60'",
                        false,
                    ),
                    service(
                        "frontend",
                        "/bin/sh -c 'echo $$ > frontend.pid; exec sleep 60'",
                        false,
                    ),
                    service(
                        "cloudflared",
                        "/bin/sh -c 'echo $$ > cloudflared.pid; exec sleep 60'",
                        false,
                    ),
                ],
            },
        )
        .unwrap();
    let first = store
        .control(&terminals, "project", root.path(), None, "start")
        .unwrap();
    let second = store
        .control(&terminals, "project", root.path(), None, "start")
        .unwrap();
    assert_eq!(
        first.runs, second.runs,
        "Start All must not create duplicate PTYs"
    );
    let pids = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let pids = ["backend", "frontend", "cloudflared"]
                .into_iter()
                .map(|id| {
                    std::fs::read_to_string(root.path().join(format!("{id}.pid")))
                        .ok()
                        .and_then(|value| value.trim().parse::<i32>().ok())
                })
                .collect::<Option<Vec<_>>>();
            if let Some(pids) = pids {
                break pids;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    store
        .control(&terminals, "project", root.path(), None, "stop")
        .unwrap();
    assert!(
        store
            .snapshot("project", root.path())
            .unwrap()
            .runs
            .iter()
            .all(|r| r.status == Status::Stopped)
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            // A briefly unreaped zombie is already terminated, unlike a
            // leftover dev server still holding its port.
            let alive = pids.iter().any(|pid| {
                std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .ok()
                    .is_some_and(|s| !s.rsplit_once(") ").unwrap().1.starts_with('Z'))
            });
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Stop All terminates service children");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_windows_start_one_service_and_removing_it_cancels_supervision() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let store = ProjectTerminals::open(state.path()).unwrap();
    let terminals = Terminals::new();
    store
        .save(
            "project",
            root.path(),
            ProjectTerminalConfig {
                services: vec![service("backend", "exec sleep 60", true)],
            },
        )
        .unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let requests = (0..2)
        .map(|_| {
            let store = store.clone();
            let terminals = terminals.clone();
            let root = root.path().to_owned();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                store
                    .control(&terminals, "project", &root, None, "start")
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let mut ids = Vec::new();
    for request in requests {
        ids.push(
            request.await.unwrap().runs[0]
                .terminal
                .as_ref()
                .unwrap()
                .id
                .clone(),
        );
    }
    assert_eq!(
        ids[0], ids[1],
        "different windows share the same service process"
    );
    store
        .save("project", root.path(), ProjectTerminalConfig::default())
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let snapshot = store.snapshot("project", root.path()).unwrap();
    assert!(snapshot.config.services.is_empty());
    assert!(snapshot.runs.is_empty());
    assert!(
        !terminals.any_open(),
        "removing a service cannot trigger automatic recovery"
    );
}

#[tokio::test]
async fn failed_services_restart_with_a_limit_and_stop_cancels_backoff() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let store = ProjectTerminals::open(state.path()).unwrap();
    let terminals = Terminals::new();
    store
        .save(
            "project",
            root.path(),
            ProjectTerminalConfig {
                services: vec![service("crash", "/bin/sh -c 'exit 7'", true)],
            },
        )
        .unwrap();
    store
        .control(&terminals, "project", root.path(), None, "start")
        .unwrap();
    wait_for(&store, root.path(), "crash", Status::Failed).await;
    let failed = store
        .snapshot("project", root.path())
        .unwrap()
        .runs
        .remove(0);
    assert_eq!(failed.exit_code, Some(7));
    assert_eq!(failed.restarts, 3);
    store
        .control(&terminals, "project", root.path(), None, "start")
        .unwrap();
    wait_for(&store, root.path(), "crash", Status::Restarting).await;
    store
        .control(&terminals, "project", root.path(), None, "stop")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        store.snapshot("project", root.path()).unwrap().runs[0].status,
        Status::Stopped
    );
}

#[tokio::test]
async fn successful_exit_and_interrupted_engine_are_distinct_and_profiles_survive_restart() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let store = ProjectTerminals::open(state.path()).unwrap();
    let terminals = Terminals::new();
    let config = ProjectTerminalConfig {
        services: vec![
            service("success", "/bin/sh -c 'exit 0'", true),
            service("live", "exec sleep 60", false),
        ],
    };
    store.save("project", root.path(), config.clone()).unwrap();
    store
        .control(&terminals, "project", root.path(), None, "start")
        .unwrap();
    wait_for(&store, root.path(), "success", Status::Exited).await;
    let persisted = std::fs::read(state.path().join("project-terminals.json")).unwrap();
    store
        .control(&terminals, "project", root.path(), None, "stop")
        .unwrap();
    let recovered_state = tempfile::tempdir().unwrap();
    std::fs::write(
        recovered_state.path().join("project-terminals.json"),
        persisted,
    )
    .unwrap();
    let recovered = ProjectTerminals::open(recovered_state.path())
        .unwrap()
        .snapshot("project", root.path())
        .unwrap();
    assert_eq!(recovered.config, config);
    assert_eq!(
        recovered
            .runs
            .iter()
            .find(|r| r.service_id == "live")
            .unwrap()
            .status,
        Status::Interrupted
    );
    assert_eq!(
        recovered
            .runs
            .iter()
            .find(|r| r.service_id == "success")
            .unwrap()
            .status,
        Status::Exited
    );
    assert!(recovered.runs.iter().all(|r| r.terminal.is_none()));
}

#[tokio::test]
async fn start_all_preflights_directories_and_does_not_cross_project_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let store = ProjectTerminals::open(state.path()).unwrap();
    let terminals = Terminals::new();
    std::os::unix::fs::symlink(outside.path(), root.path().join("outside")).unwrap();
    let mut escaped = service("escape", "touch unexpected", false);
    escaped.directory = "outside".into();
    store
        .save(
            "project",
            root.path(),
            ProjectTerminalConfig {
                services: vec![service("valid", "touch unexpected", false), escaped],
            },
        )
        .unwrap();
    assert!(
        store
            .control(&terminals, "project", root.path(), None, "start")
            .is_err()
    );
    assert!(!terminals.any_open());
    assert!(store.snapshot("project", outside.path()).is_err());
    assert!(!root.path().join("unexpected").exists());
    let mut invalid = service("invalid", "echo ok", false);
    invalid.directory = "../".into();
    assert!(
        store
            .save(
                "project",
                root.path(),
                ProjectTerminalConfig {
                    services: vec![invalid]
                }
            )
            .is_err()
    );
}

fn git_fixture(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct WorktreeFixture {
    _temporary: tempfile::TempDir,
    root: std::path::PathBuf,
    a: std::path::PathBuf,
    b: std::path::PathBuf,
    core: zeron_engine::EngineCore,
    client: zeron_rpc::RpcClient,
}

impl WorktreeFixture {
    async fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        git_fixture(&root, &["init", "-qb", "main"]);
        std::fs::write(root.join("tracked.txt"), "fixture\n").unwrap();
        git_fixture(&root, &["add", "."]);
        git_fixture(&root, &["commit", "-qm", "fixture"]);
        let a = temporary.path().join("agent-a");
        let b = temporary.path().join("agent-b");
        for (path, branch) in [(&a, "agent-a"), (&b, "agent-b")] {
            git_fixture(
                &root,
                &["worktree", "add", "-qb", branch, path.to_str().unwrap()],
            );
        }
        // A linked checkout remains valid without a branch ref pointing to it.
        git_fixture(&a, &["checkout", "-q", "--detach"]);
        let core = zeron_engine::EngineCore::assemble(
            &temporary.path().join("data"),
            std::sync::Arc::new(zeron_engine::default_registry()),
            zeron_proto::HarnessId::Mock,
            None,
        )
        .unwrap();
        core.workspace
            .create_space(
                "project",
                &core.device_id,
                root.to_str().unwrap(),
                None,
                true,
            )
            .unwrap();
        for (id, path) in [("a", &a), ("b", &b), ("also-b", &b)] {
            core.workspace
                .create_chat(
                    id,
                    Some("project"),
                    Some(&core.device_id),
                    None,
                    Some(path.to_string_lossy().into_owned()),
                )
                .unwrap();
        }
        let client = zeron_rpc::memory_client(core.rpc_service());
        Self {
            _temporary: temporary,
            root,
            a,
            b,
            core,
            client,
        }
    }

    async fn control(
        &self,
        chat: &str,
        action: &str,
        service: Option<&str>,
    ) -> zeron_proto::ProjectTerminalsSnapshot {
        self.client
            .call_as(
                zeron_rpc::methods::CONTROL_PROJECT_TERMINALS,
                serde_json::json!({
                    "spaceId":"project", "chatId":chat, "action":action, "serviceId":service,
                }),
            )
            .await
            .unwrap()
    }

    fn save(&self, services: Vec<ProjectTerminalService>) {
        self.core
            .project_actions
            .project_terminals
            .save("project", &self.root, ProjectTerminalConfig { services })
            .unwrap();
    }
}

#[cfg(target_os = "linux")]
fn process_alive(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .map(|(_, fields)| !fields.starts_with("Z ") && !fields.starts_with("X "))
        })
        .unwrap_or(false)
}

async fn new_pid(root: &Path, name: &str, previous: Option<i32>) -> i32 {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(pid) = std::fs::read_to_string(root.join(format!("{name}.pid")))
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
                && Some(pid) != previous
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("replacement binds and reports its pid")
}

#[cfg(target_os = "linux")]
fn listening_service(name: &str) -> ProjectTerminalService {
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    service(
        name,
        &format!(
            "exec python3 -u -c 'import socket,os,pathlib,time; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind((\"127.0.0.1\",{port})); s.listen(); pathlib.Path(\"{name}.pid\").write_text(str(os.getpid())); print(os.getcwd(),flush=True); time.sleep(60)'"
        ),
        false,
    )
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn stop_terminates_child_job_groups_and_keeps_other_services_running() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let store = ProjectTerminals::open(state.path()).unwrap();
    let terminals = Terminals::new();
    std::fs::write(
        root.path().join("backend.py"),
        r#"
import subprocess, time
subprocess.Popen(['python3', '-c', '''
import os, pathlib, signal, socket, time
signal.signal(signal.SIGHUP, signal.SIG_IGN)
os.setpgid(0, 0)
server = socket.socket()
server.bind(('127.0.0.1', 0))
server.listen()
pathlib.Path('backend.pid').write_text(str(os.getpid()))
time.sleep(60)
'''])
time.sleep(60)
"#,
    )
    .unwrap();
    store
        .save(
            "project",
            root.path(),
            ProjectTerminalConfig {
                services: vec![
                    service("backend", "exec python3 backend.py", false),
                    listening_service("frontend"),
                ],
            },
        )
        .unwrap();
    store
        .control(&terminals, "project", root.path(), None, "start")
        .unwrap();
    let backend = new_pid(root.path(), "backend", None).await;
    let frontend = new_pid(root.path(), "frontend", None).await;
    let fields = std::fs::read_to_string(format!("/proc/{backend}/stat")).unwrap();
    let fields: Vec<_> = fields
        .rsplit_once(") ")
        .unwrap()
        .1
        .split_whitespace()
        .collect();
    assert_ne!(
        fields[2], fields[3],
        "the child owns a separate job group inside the service session"
    );
    store
        .control(&terminals, "project", root.path(), Some("backend"), "stop")
        .unwrap();
    let backend_stopped = !process_alive(backend);
    let frontend_running = process_alive(frontend);
    // Always clean up this regression's intentionally stubborn fixture child.
    if !backend_stopped {
        unsafe {
            libc::kill(backend, libc::SIGKILL);
        }
    }
    store
        .control(&terminals, "project", root.path(), None, "stop")
        .unwrap();
    assert!(
        backend_stopped,
        "Stop must kill child processes in separate job groups"
    );
    assert!(frontend_running, "stopping Backend must not stop Frontend");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn agents_share_one_service_set_and_handoff_releases_ports_before_restarting() {
    let f = WorktreeFixture::new().await;
    f.save(vec![
        listening_service("backend"),
        listening_service("frontend"),
        service("tunnel", "exec sleep 60", false),
    ]);
    f.control("a", "start", Some("backend")).await;
    let first = f.control("a", "start", Some("frontend")).await;
    let old_backend = new_pid(&f.a, "backend", None).await;
    let old_frontend = new_pid(&f.a, "frontend", None).await;
    let next = f.control("b", "activate", None).await;
    assert_eq!(next.checkout.as_deref(), f.b.to_str());
    assert!(
        !process_alive(old_backend) && !process_alive(old_frontend),
        "old processes are dead before handoff returns"
    );
    let b_backend = new_pid(&f.b, "backend", None).await;
    let b_frontend = new_pid(&f.b, "frontend", None).await;
    assert!(process_alive(b_backend) && process_alive(b_frontend));
    assert_eq!(next.runs.len(), 2, "an unstarted service stays unstarted");
    assert!(
        next.runs
            .iter()
            .all(|r| r.terminal.as_ref().unwrap().cwd == f.b.to_string_lossy())
    );
    assert_ne!(
        first.runs[0].terminal.as_ref().unwrap().id,
        next.runs[0].terminal.as_ref().unwrap().id
    );

    let polled: zeron_proto::ProjectTerminalsSnapshot = f
        .client
        .call_as(
            zeron_rpc::methods::GET_PROJECT_TERMINALS,
            serde_json::json!({"spaceId":"project","chatId":"a"}),
        )
        .await
        .unwrap();
    assert_eq!(
        polled.runs.iter().all(|r|r.status==Status::Stopped), true,
        "background polling reads the old checkout without activating its services"
    );
    assert_eq!(
        f.control("also-b", "activate", None).await.runs,
        next.runs,
        "two agents in one checkout reuse processes"
    );
    assert_eq!(f.control("b", "start", None).await.runs.len(), 3);
    f.control("b", "stop", Some("backend")).await;
    let returned = f.control("a", "activate", None).await;
    assert_eq!(
        returned
            .runs
            .iter()
            .find(|r| r.service_id == "backend")
            .unwrap()
            .status,
        Status::Stopped
    );
    new_pid(&f.a, "frontend", Some(old_frontend)).await;
    assert!(!process_alive(b_backend) && !process_alive(b_frontend));
    f.control("a", "stop", None).await;
    assert!(!f.core.terminals.any_open());
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_worktree_starts_leave_only_one_process_per_service() {
    let f = WorktreeFixture::new().await;
    f.save(vec![
        listening_service("backend"),
        listening_service("frontend"),
    ]);
    let a = f.control("a", "start", None);
    let b = f.control("b", "start", None);
    let _ = tokio::join!(a, b);
    let a=f.core.project_actions.project_terminals.checkout_snapshot("project",&f.root,&f.a,&f.core.repos.worktree_settings().effective(&f.root,&f.a).unwrap()).unwrap();
    let b=f.core.project_actions.project_terminals.checkout_snapshot("project",&f.root,&f.b,&f.core.repos.worktree_settings().effective(&f.root,&f.b).unwrap()).unwrap();
    let current=if a.runs.iter().any(|r|r.status==Status::Running){a}else{b};
    let active = std::path::PathBuf::from(current.checkout.as_ref().unwrap());
    let other = if active == f.a { &f.b } else { &f.a };
    for name in ["backend", "frontend"] {
        assert!(process_alive(new_pid(&active, name, None).await));
        if let Ok(pid) = std::fs::read_to_string(other.join(format!("{name}.pid"))) {
            assert!(!process_alive(pid.trim().parse().unwrap()));
        }
    }
    assert!(current.runs.iter().all(|r| r.status == Status::Running
        && r.terminal.as_ref().unwrap().cwd == active.to_string_lossy()));
    f.control(if active==f.a {"a"} else {"b"}, "stop", None).await;
}

#[tokio::test]
async fn switching_worktrees_cancels_old_crash_recovery_and_rejects_unrelated_agents() {
    let f = WorktreeFixture::new().await;
    std::fs::write(f.a.join("fail"), "").unwrap();
    f.save(vec![service(
        "backend",
        "echo attempt >> attempts; if test -f fail; then exit 7; fi; exec sleep 60",
        true,
    )]);
    f.control("a", "start", None).await;
    tokio::time::timeout(Duration::from_secs(15),async{loop{let snapshot=f.core.project_actions.project_terminals.checkout_snapshot("project",&f.root,&f.a,&f.core.repos.worktree_settings().effective(&f.root,&f.a).unwrap()).unwrap();if snapshot.runs.iter().any(|r|r.status==Status::Restarting){break;}tokio::time::sleep(Duration::from_millis(20)).await;}}).await.unwrap();
    let b = f.control("b", "activate", None).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        std::fs::read_to_string(f.a.join("attempts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(f.b.join("attempts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(f.control("also-b", "activate", None).await.runs, b.runs);
    let outside = f._temporary.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    f.core
        .workspace
        .create_space(
            "other",
            &f.core.device_id,
            outside.to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    f.core
        .workspace
        .create_chat(
            "foreign",
            Some("other"),
            Some(&f.core.device_id),
            None,
            Some(outside.to_string_lossy().into_owned()),
        )
        .unwrap();
    assert!(
        f.client
            .call(
                zeron_rpc::methods::CONTROL_PROJECT_TERMINALS,
                serde_json::json!({"spaceId":"project","chatId":"foreign","action":"activate"})
            )
            .await
            .is_err()
    );
    f.core
        .workspace
        .set_chat_cwd("a", outside.to_str().unwrap())
        .unwrap();
    assert!(
        f.client
            .call(
                zeron_rpc::methods::CONTROL_PROJECT_TERMINALS,
                serde_json::json!({"spaceId":"project","chatId":"a","action":"activate"})
            )
            .await
            .is_err()
    );
    assert_eq!(
        f.core
            .project_actions
            .project_terminals
            .checkout_snapshot("project",&f.root,&f.b,&f.core.repos.worktree_settings().effective(&f.root,&f.b).unwrap())
            .unwrap()
            .runs,
        b.runs
    );
    f.control("b", "stop", None).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn failed_handoff_stops_old_services_instead_of_serving_the_wrong_worktree() {
    let f = WorktreeFixture::new().await;
    std::fs::create_dir(f.a.join("web")).unwrap();
    let mut frontend = listening_service("frontend");
    frontend.directory = "web".into();
    f.save(vec![listening_service("backend"), frontend]);
    f.control("a", "start", None).await;
    let backend = new_pid(&f.a, "backend", None).await;
    let frontend = new_pid(&f.a.join("web"), "frontend", None).await;
    assert!(
        f.client
            .call(
                zeron_rpc::methods::CONTROL_PROJECT_TERMINALS,
                serde_json::json!({"spaceId":"project","chatId":"b","action":"activate"})
            )
            .await
            .is_err()
    );
    assert!(!process_alive(backend) && !process_alive(frontend));
    let snapshot = f
        .core
        .project_actions
        .project_terminals
        .checkout_snapshot("project", &f.root, &f.b, &f.core.repos.worktree_settings().effective(&f.root,&f.b).unwrap())
        .unwrap();
    assert!(snapshot.runs.iter().all(|r| r.status == Status::Stopped));
    assert_eq!(snapshot.checkout.as_deref(), f.b.to_str());
    assert!(!f.core.terminals.any_open());
}
