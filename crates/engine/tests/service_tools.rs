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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_add_and_patch_entries_without_replacing_other_entries_or_settings() {
    let f = Fixture::new(false).await;
    let agent = tools(&f.core, "agent");
    let peer = tools(&f.core, "peer");
    let before = f
        .core
        .repos
        .worktree_settings()
        .effective(&f.root, &f.a)
        .unwrap();
    let listed = agent
        .call("service_list", json!({"includeConfig":true}))
        .await
        .unwrap();
    assert_eq!(
        listed["services"][0]["configuration"]["readinessTimeoutSeconds"],
        3
    );
    let added = agent
        .call(
            "service_add",
            json!({"name":"Worker","command":"echo $$ > worker.pid; exec sleep 120"}),
        )
        .await
        .unwrap();
    let id = added["service"]["id"].as_str().unwrap();
    assert_eq!(added["saved"], true);
    assert_eq!(added["service"]["status"], "stopped");
    assert_eq!(added["service"]["configuration"]["directory"], ".");
    assert!(
        !f.a.join("worker.pid").exists(),
        "saving must not run commands"
    );
    assert_eq!(f.snapshot("peer").await.config.services.len(), 3);
    agent
        .call(
            "service_update",
            json!({"serviceId":id,"name":"Jobs","restartOnFailure":true}),
        )
        .await
        .unwrap();
    let configs = agent
        .call("service_list", json!({"includeConfig":true}))
        .await
        .unwrap();
    let worker = configs["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap();
    assert_eq!(worker["configuration"]["name"], "Jobs");
    assert_eq!(
        worker["configuration"]["command"],
        "echo $$ > worker.pid; exec sleep 120"
    );
    assert_eq!(worker["configuration"]["restartOnFailure"], true);
    assert_eq!(configs["services"].as_array().unwrap().len(), 4);

    let db = agent.call("service_add", json!({"kind":"database","name":"Cache","command":"echo $$ > cache.pid; exec sleep 120","readinessCommand":"test -f cache.pid"})).await.unwrap();
    let db_id = db["service"]["id"].as_str().unwrap();
    assert_eq!(db["service"]["scope"], "project");
    assert_eq!(
        db["service"]["configuration"]["readinessTimeoutSeconds"],
        120
    );
    peer.call(
        "service_update",
        json!({"serviceId":db_id,"readinessTimeoutSeconds":7}),
    )
    .await
    .unwrap();
    let peer_db = peer
        .call("service_list", json!({"includeConfig":true}))
        .await
        .unwrap();
    assert!(
        peer_db["services"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == db_id && s["configuration"]["readinessTimeoutSeconds"] == 7)
    );
    assert_eq!(f.snapshot("agent").await.config.services.len(), 5);
    assert_eq!(f.snapshot("peer").await.config.services.len(), 4);
    let after = f
        .core
        .repos
        .worktree_settings()
        .effective(&f.root, &f.a)
        .unwrap();
    assert_eq!(before.dependencies, after.dependencies);
    assert_eq!(before.variables, after.variables);
    assert_eq!(before.env_files, after.env_files);
    assert_eq!(before.setup_command, after.setup_command);
    assert_eq!(before.service_mode, after.service_mode);
    assert!(
        !f.core
            .repos
            .worktree_settings()
            .is_prepared(&f.root, &f.a)
            .unwrap()
    );

    agent
        .call("service_start", json!({"serviceId":id}))
        .await
        .unwrap();
    let original_pid = pid(&f.a, "worker", None).await;
    let original = f.snapshot("agent").await;
    agent.call("service_update", json!({"serviceId":id,"command":"touch changed-command; echo $$ > worker.pid; exec sleep 120"})).await.unwrap();
    assert_eq!(
        terminal(&original, id),
        terminal(&f.snapshot("agent").await, id)
    );
    assert!(!f.a.join("changed-command").exists());
    agent
        .call("service_restart", json!({"serviceId":id}))
        .await
        .unwrap();
    pid(&f.a, "worker", Some(&original_pid)).await;
    assert!(f.a.join("changed-command").exists());
    assert_eq!(
        terminal(&original, "db"),
        terminal(&f.snapshot("agent").await, "db")
    );
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_configuration_edits_leave_saved_lists_and_workflow_settings_unchanged() {
    let f = Fixture::new(false).await;
    let agent = tools(&f.core, "agent");
    let before = f.snapshot("agent").await;
    for (name, args) in [
        ("service_add", json!({"name":"api","command":"true"})),
        (
            "service_add",
            json!({"name":"New","command":"true","directory":"../escape"}),
        ),
        (
            "service_add",
            json!({"name":"New","command":"true","directory":"/tmp"}),
        ),
        (
            "service_add",
            json!({"kind":"database","name":"New","command":"true"}),
        ),
        (
            "service_add",
            json!({"kind":"database","name":"New","command":"true","readinessCommand":"true","readinessTimeoutSeconds":0}),
        ),
        (
            "service_add",
            json!({"name":"New","command":"true","readinessCommand":"true"}),
        ),
        (
            "service_update",
            json!({"serviceId":"db","readinessCommand":" "}),
        ),
        (
            "service_update",
            json!({"serviceId":"api","readinessCommand":"true"}),
        ),
        ("service_update", json!({"serviceId":"api","name":"DB"})),
        ("service_update", json!({"serviceId":"api","command":" "})),
        (
            "service_update",
            json!({"serviceId":"missing","name":"New"}),
        ),
        ("service_update", json!({"serviceId":"api"})),
    ] {
        assert!(
            agent.call(name, args.clone()).await.is_err(),
            "{name}: {args}"
        );
        assert_eq!(f.snapshot("agent").await, before);
    }
    assert!(
        f.core
            .repos
            .worktree_settings()
            .effective(&f.root, &f.a)
            .unwrap()
            .services
            .is_none()
    );
    assert!(
        !f.core
            .repos
            .worktree_settings()
            .is_prepared(&f.root, &f.a)
            .unwrap()
    );
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_adds_and_partial_updates_keep_both_agents_changes() {
    let f = Fixture::new(false).await;
    let agent = tools(&f.core, "agent");
    let (first, second) = tokio::join!(
        agent.call("service_add", json!({"name":"First","command":"true"})),
        agent.call("service_add", json!({"name":"Second","command":"false"})),
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_ne!(first["service"]["id"], second["service"]["id"]);
    let (name, command) = tokio::join!(
        agent.call("service_update", json!({"serviceId":"api","name":"Server"})),
        agent.call(
            "service_update",
            json!({"serviceId":"api","command":"exec sleep 30"})
        ),
    );
    name.unwrap();
    command.unwrap();
    let saved = f.snapshot("agent").await;
    assert_eq!(saved.config.services.len(), 5);
    let api = saved
        .config
        .services
        .iter()
        .find(|s| s.id == "api")
        .unwrap();
    assert_eq!(api.name, "Server");
    assert_eq!(api.command, "exec sleep 30");
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn main_folder_edits_update_defaults_and_database_edits_preserve_checkout_overrides() {
    let f = Fixture::new(false).await;
    f.core
        .workspace
        .create_chat(
            "main",
            Some("project"),
            Some(&f.core.device_id),
            None,
            Some(f.root.to_string_lossy().into_owned()),
        )
        .unwrap();
    let main = tools(&f.core, "main");
    let agent = tools(&f.core, "agent");
    agent
        .call(
            "service_update",
            json!({"serviceId":"api","command":"checkout command"}),
        )
        .await
        .unwrap();
    main.call(
        "service_update",
        json!({"serviceId":"api","name":"Default API","command":"project command"}),
    )
    .await
    .unwrap();
    agent
        .call(
            "service_update",
            json!({"serviceId":"db","name":"Shared database"}),
        )
        .await
        .unwrap();
    for (chat, command) in [
        ("agent", "checkout command"),
        ("peer", "project command"),
        ("main", "project command"),
    ] {
        let snapshot = f.snapshot(chat).await;
        assert_eq!(
            snapshot
                .config
                .services
                .iter()
                .find(|s| s.id == "api")
                .unwrap()
                .command,
            command
        );
        assert_eq!(
            snapshot
                .config
                .services
                .iter()
                .find(|s| s.id == "db")
                .unwrap()
                .name,
            "Shared database"
        );
    }
    let added = main
        .call(
            "service_add",
            json!({"name":"Main worker","command":"true"}),
        )
        .await
        .unwrap();
    let id = added["service"]["id"].as_str().unwrap();
    assert!(
        f.snapshot("peer")
            .await
            .config
            .services
            .iter()
            .any(|s| s.id == id)
    );
    assert!(
        !f.snapshot("agent")
            .await
            .config
            .services
            .iter()
            .any(|s| s.id == id)
    );
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_authoring_cannot_override_chat_scope_or_bypass_access_modes() {
    let f = Fixture::new(false).await;
    let client = agent_client(&f.core, Some("agent"));
    let add = json!({"op":"add","name":"Escape","command":"true"});
    for params in [
        json!({"chatId":"peer","edit":add}),
        json!({"chatId":"agent","checkoutPath":f.b,"edit":add}),
        json!({"chatId":"agent","spaceId":"project","edit":add}),
    ] {
        assert!(
            client
                .call(methods::EDIT_CHAT_SERVICE, params)
                .await
                .is_err()
        );
    }
    for (access, sandbox, plan) in [
        (AccessMode::Supervised, SandboxLevel::WorkspaceWrite, false),
        (
            AccessMode::AutoAcceptEdits,
            SandboxLevel::WorkspaceWrite,
            false,
        ),
        (AccessMode::FullAccess, SandboxLevel::ReadOnly, false),
        (AccessMode::FullAccess, SandboxLevel::WorkspaceWrite, true),
    ] {
        f.core
            .workspace
            .set_chat_config(
                "agent",
                &ChatConfig {
                    harness: HarnessId::Mock,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    sandbox,
                    access,
                    plan,
                },
            )
            .unwrap();
        assert!(
            tools(&f.core, "agent")
                .call("service_add", json!({"name":"New","command":"true"}))
                .await
                .unwrap_err()
                .contains("full access")
        );
        assert!(
            tools(&f.core, "agent")
                .call(
                    "service_update",
                    json!({"serviceId":"db","command":"false"})
                )
                .await
                .unwrap_err()
                .contains("full access")
        );
    }
    assert_eq!(f.snapshot("agent").await.config.services.len(), 3);
    f.core.shutdown().await;
}
