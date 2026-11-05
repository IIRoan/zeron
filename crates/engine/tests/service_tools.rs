#![cfg(unix)]
//! MCP -> scoped agent RPC -> real service PTYs, without the desktop open.
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_mcp::{Origin, Tools, Zeron};
use zeron_proto::*;
use zeron_rpc::{Caller, RpcError, RpcReply, RpcService, methods};

struct Agent {
    service: Arc<dyn RpcService>,
    chat: Option<String>,
}
#[async_trait::async_trait]
impl RpcService for Agent {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        assert!(
            zeron_rpc::agent_allowed(method),
            "MCP needs an app-only method: {method}"
        );
        zeron_rpc::with_caller(
            Caller::Agent {
                chat: self.chat.clone(),
            },
            self.service.handle(method, params),
        )
        .await
    }
}

fn agent_client(core: &EngineCore, chat: Option<&str>) -> zeron_rpc::RpcClient {
    zeron_rpc::memory_client(Arc::new(Agent {
        service: core.rpc_service(),
        chat: chat.map(str::to_owned),
    }))
}
fn tools(core: &EngineCore, chat: &str) -> Tools {
    Tools::new(Arc::new(Zeron::with_client(
        agent_client(core, Some(chat)),
        Origin {
            chat_id: Some(chat.into()),
            device_id: Some(core.device_id.clone()),
            side_chats_only: false,
        },
    )))
}
fn git(root: &Path, args: &[&str]) {
    let result = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn service(id: &str, database: bool) -> ProjectTerminalService {
    ProjectTerminalService {
        id: id.into(),
        name: id.into(),
        directory: ".".into(),
        restart_on_failure: false,
        command: format!("echo $$ > {id}.pid; exec sleep 120"),
        setup_prerequisite: database.then(|| ProjectServiceReadiness {
            command: format!("test -f {id}.pid"),
            timeout_seconds: 3,
        }),
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    core: EngineCore,
    root: PathBuf,
    a: PathBuf,
    b: PathBuf,
}
impl Fixture {
    async fn new(readiness_fails: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "Fixture",
            ],
        );
        git(&root, &["worktree", "add", "-b", "a", a.to_str().unwrap()]);
        git(&root, &["worktree", "add", "-b", "b", b.to_str().unwrap()]);
        let core = EngineCore::assemble(
            &temp.path().join("state"),
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
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
        for (id, project, device, cwd) in [
            ("agent", Some("project"), core.device_id.as_str(), &a),
            ("peer", Some("project"), core.device_id.as_str(), &b),
            ("remote", Some("project"), "other-device", &root),
            ("projectless", None, core.device_id.as_str(), &root),
        ] {
            core.workspace
                .create_chat_hosted(
                    id,
                    project,
                    Some(device),
                    Some(device),
                    None,
                    Some(cwd.to_string_lossy().into_owned()),
                    None,
                    None,
                )
                .unwrap();
        }
        let client = zeron_rpc::memory_client(core.rpc_service());
        let mut db = service("db", true);
        if readiness_fails {
            db.setup_prerequisite = Some(ProjectServiceReadiness {
                command: "false".into(),
                timeout_seconds: 1,
            });
        }
        client
            .call(
                methods::SAVE_WORKTREE_SETTINGS,
                json!({"spaceId":"project","databases":[db],"settings":WorktreeSettings {
                    dependencies: WorktreeDependencyMode::Skip,
                    service_mode: WorktreeServiceMode::Parallel,
                    ..Default::default()
                }}),
            )
            .await
            .unwrap();
        client
            .call(
                methods::SAVE_PROJECT_TERMINALS,
                json!({"spaceId":"project","config":ProjectTerminalConfig {
                    services: vec![service("api", false), service("web", false)],
                }}),
            )
            .await
            .unwrap();
        Self {
            _temp: temp,
            core,
            root,
            a,
            b,
        }
    }
    async fn snapshot(&self, chat: &str) -> ProjectTerminalsSnapshot {
        zeron_rpc::memory_client(self.core.rpc_service())
            .call_as(
                methods::GET_PROJECT_TERMINALS,
                json!({"spaceId":"project","chatId":chat}),
            )
            .await
            .unwrap()
    }
}
async fn pid(root: &Path, id: &str, previous: Option<&str>) -> String {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(root.join(format!("{id}.pid")))
                && previous != Some(pid.as_str())
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("service process started")
}
fn terminal(snapshot: &ProjectTerminalsSnapshot, id: &str) -> String {
    snapshot
        .runs
        .iter()
        .find(|r| r.service_id == id)
        .unwrap()
        .terminal
        .as_ref()
        .unwrap()
        .id
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_control_checkout_services_and_shared_databases_through_mcp() {
    let f = Fixture::new(false).await;
    let agent = tools(&f.core, "agent");
    let peer = tools(&f.core, "peer");
    let list = agent.call("service_list", json!({})).await.unwrap();
    assert_eq!(list["checkout"], f.a.to_str().unwrap());
    assert_eq!(list["services"].as_array().unwrap().len(), 3);
    assert!(
        list["services"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["status"] == "stopped")
    );
    assert!(!list.to_string().contains("command"));
    assert!(
        !f.core
            .repos
            .worktree_settings()
            .is_prepared(&f.root, &f.a)
            .unwrap()
    );
    assert!(
        agent
            .call("service_start", json!({"serviceId":"missing"}))
            .await
            .unwrap_err()
            .contains("Unknown service")
    );
    assert!(
        !f.core
            .repos
            .worktree_settings()
            .is_prepared(&f.root, &f.a)
            .unwrap()
    );
    assert!(!f.root.join("db.pid").exists());

    agent
        .call("service_start", json!({"serviceId":"db"}))
        .await
        .unwrap();
    let db_pid = pid(&f.root, "db", None).await;
    let db = agent
        .call("service_status", json!({"serviceId":"db"}))
        .await
        .unwrap();
    assert_eq!(db["service"]["kind"], "database");
    assert_eq!(db["service"]["scope"], "project");
    assert_eq!(db["service"]["readiness"], "ready");
    agent
        .call("service_start", json!({"serviceId":"api"}))
        .await
        .unwrap();
    let api_pid = pid(&f.a, "api", None).await;
    agent
        .call("service_start", json!({"serviceId":"web"}))
        .await
        .unwrap();
    pid(&f.a, "web", None).await;
    let before = f.snapshot("agent").await;
    agent
        .call("service_start", json!({"serviceId":"api"}))
        .await
        .unwrap();
    assert_eq!(
        terminal(&before, "api"),
        terminal(&f.snapshot("agent").await, "api")
    );
    agent
        .call("service_restart", json!({"serviceId":"api"}))
        .await
        .unwrap();
    pid(&f.a, "api", Some(&api_pid)).await;
    let after = f.snapshot("agent").await;
    assert_ne!(terminal(&before, "api"), terminal(&after, "api"));
    assert_eq!(terminal(&before, "web"), terminal(&after, "web"));
    assert_eq!(terminal(&before, "db"), terminal(&after, "db"));

    peer.call("service_start", json!({"serviceId":"api"}))
        .await
        .unwrap();
    pid(&f.b, "api", None).await;
    let other = f.snapshot("peer").await;
    assert_ne!(terminal(&after, "api"), terminal(&other, "api"));
    assert_eq!(terminal(&after, "db"), terminal(&other, "db"));
    agent
        .call("service_stop", json!({"serviceId":"api"}))
        .await
        .unwrap();
    assert_eq!(
        agent
            .call("service_status", json!({"serviceId":"api"}))
            .await
            .unwrap()["service"]["status"],
        "stopped"
    );
    assert_eq!(
        terminal(&other, "api"),
        terminal(&f.snapshot("peer").await, "api")
    );

    peer.call("service_restart", json!({"serviceId":"db"}))
        .await
        .unwrap();
    pid(&f.root, "db", Some(&db_pid)).await;
    assert_eq!(
        terminal(&other, "api"),
        terminal(&f.snapshot("peer").await, "api")
    );
    assert_ne!(
        terminal(&other, "db"),
        terminal(&f.snapshot("agent").await, "db")
    );
    peer.call("service_stop", json!({"serviceId":"db"}))
        .await
        .unwrap();
    let stopped = agent
        .call("service_status", json!({"serviceId":"db"}))
        .await
        .unwrap();
    assert_eq!(stopped["service"]["status"], "stopped");
    assert_eq!(stopped["service"]["readiness"], Value::Null);
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_rpc_rejects_scope_overrides_and_invalid_actions_without_side_effects() {
    let f = Fixture::new(false).await;
    let client = agent_client(&f.core, Some("agent"));
    for (method, params) in [
        (methods::GET_CHAT_SERVICES, json!({"chatId":"peer"})),
        (
            methods::GET_CHAT_SERVICES,
            json!({"chatId":"agent","checkoutPath":f.b}),
        ),
        (
            methods::CONTROL_CHAT_SERVICE,
            json!({"chatId":"peer","serviceId":"api","action":"start"}),
        ),
        (
            methods::CONTROL_CHAT_SERVICE,
            json!({"chatId":"agent","action":"stop"}),
        ),
        (
            methods::CONTROL_CHAT_SERVICE,
            json!({"chatId":"agent","serviceId":"api","action":"activate"}),
        ),
        (
            methods::CONTROL_CHAT_SERVICE,
            json!({"chatId":"agent","serviceId":"api","action":"start","command":"touch injected"}),
        ),
    ] {
        assert!(client.call(method, params).await.is_err());
    }
    assert!(
        agent_client(&f.core, None)
            .call(methods::GET_CHAT_SERVICES, json!({"chatId":"agent"}))
            .await
            .is_err()
    );
    for (id, message) in [
        ("projectless", "no project"),
        ("remote", "this device"),
        ("missing", "not found"),
    ] {
        assert!(
            tools(&f.core, id)
                .call("service_list", json!({}))
                .await
                .unwrap_err()
                .contains(message)
        );
    }
    assert!(
        !f.core
            .repos
            .worktree_settings()
            .is_prepared(&f.root, &f.a)
            .unwrap()
    );
    assert!(!f.root.join("db.pid").exists());
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn database_readiness_failures_are_visible_and_prevent_app_start() {
    let f = Fixture::new(true).await;
    let agent = tools(&f.core, "agent");
    assert!(
        agent
            .call("service_start", json!({"serviceId":"api"}))
            .await
            .is_err()
    );
    let status = agent
        .call("service_status", json!({"serviceId":"db"}))
        .await
        .unwrap();
    assert_eq!(status["service"]["readiness"], "notReady");
    assert!(!f.a.join("api.pid").exists());
    f.core.shutdown().await;
}
