#![cfg(unix)]
//! A project's Windows VM end to end, with a stand-in `winvm`: the app
//! connects the project, the chat's MCP tools use the VM, an agent can't
//! change the connection, and a `startWith` service brings the VM up.
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc, time::Duration};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_mcp::{Origin, Tools, Zeron};
use zeron_proto::*;
use zeron_rpc::{Caller, RpcError, RpcReply, RpcService, methods};

struct Agent {
    service: Arc<dyn RpcService>,
    chat: String,
}
#[async_trait::async_trait]
impl RpcService for Agent {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        let caller = Caller::Agent {
            chat: Some(self.chat.clone()),
        };
        zeron_rpc::with_caller(caller, self.service.handle(method, params)).await
    }
}

fn agent(core: &EngineCore, chat: &str) -> zeron_rpc::RpcClient {
    zeron_rpc::memory_client(Arc::new(Agent {
        service: core.rpc_service(),
        chat: chat.into(),
    }))
}

fn tools(core: &EngineCore, chat: &str) -> Tools {
    Tools::new(Arc::new(Zeron::with_client(
        agent(core, chat),
        Origin {
            chat_id: Some(chat.into()),
            device_id: Some(core.device_id.clone()),
            side_chats_only: false,
            waits_on_background: false,
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

fn service(id: &str) -> ProjectTerminalService {
    ProjectTerminalService {
        depends_on: Some(Vec::new()),
        id: id.into(),
        name: id.to_uppercase(),
        directory: ".".into(),
        restart_on_failure: false,
        command: "exec sleep 120".into(),
        setup_prerequisite: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connected_project_uses_the_vm_and_only_the_person_connects_it() {
    let temp = tempfile::tempdir().unwrap();
    let calls = temp.path().join("winvm.calls");
    let winvm = temp.path().join("winvm");
    std::fs::write(
        &winvm,
        format!(
            "#!/bin/sh\necho \"$*\" >> {}\ncase \"$1\" in\n  status|up) echo '{{\"state\":\"running\",\"toVm\":[3000,3001],\"fromVm\":[]}}';;\n  exec) echo 'hello from windows'; exit 7;;\n  diag) echo '{{\"os\":\"Windows 11\",\"cpuLoadPercent\":3}}';;\n  screenshot) printf 'PNG bytes' > \"$2\"; echo '{{\"width\":1280,\"height\":800}}';;\nesac\n",
            calls.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&winvm, std::fs::Permissions::from_mode(0o755)).unwrap();
    // One test in this binary: nothing else reads the environment meanwhile.
    unsafe {
        std::env::set_var("ZERON_DATA_DIR", temp.path().join("data"));
        std::env::set_var("ZERON_WINVM", &winvm);
    }
    let core = EngineCore::assemble(
        &temp.path().join("state"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    for project in ["project", "other"] {
        let root = temp.path().join(project);
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
        core.workspace
            .create_space(project, &core.device_id, root.to_str().unwrap(), None, true)
            .unwrap();
        core.workspace
            .create_chat_hosted(
                &format!("{project}-chat"),
                Some(project),
                Some(core.device_id.as_str()),
                Some(core.device_id.as_str()),
                None,
                Some(root.to_string_lossy().into_owned()),
                None,
                None,
            )
            .unwrap();
    }
    let app = zeron_rpc::memory_client(core.rpc_service());
    let connect =
        json!({"spaceId":"project","enabled":true,"toVm":[3001,3000],"startWith":["web"]});

    // Not connected: the tools say how the person connects it; an agent can't.
    let error = tools(&core, "project-chat")
        .call("vm_status", json!({}))
        .await
        .unwrap_err();
    assert!(error.contains("not connected"), "{error}");
    for method in [
        methods::SET_PROJECT_WINDOWS_VM,
        methods::GET_PROJECT_WINDOWS_VM,
        methods::CONTROL_PROJECT_WINDOWS_VM,
    ] {
        assert!(!zeron_rpc::agent_allowed(method));
        let refused = agent(&core, "project-chat")
            .call(method, connect.clone())
            .await;
        assert!(refused.is_err(), "{method} answered an agent");
    }

    let view: Value = app
        .call_as(methods::SET_PROJECT_WINDOWS_VM, connect)
        .await
        .unwrap();
    assert_eq!(view["settings"]["toVm"], json!([3000, 3001]));
    assert_eq!(view["settings"]["startWith"], json!(["web"]));
    assert_eq!(view["vm"]["state"], "running");

    let status = tools(&core, "project-chat")
        .call("vm_status", json!({}))
        .await
        .unwrap();
    assert!(status.to_string().contains("running"), "{status}");
    let ran = tools(&core, "project-chat")
        .call("vm_exec", json!({"command":"hostname","cwd":"C:\\src"}))
        .await
        .unwrap()
        .to_string();
    assert!(
        ran.contains("hello from windows") && ran.contains('7'),
        "{ran}"
    );
    let health = tools(&core, "project-chat")
        .call("vm_diagnostics", json!({}))
        .await
        .unwrap()
        .to_string();
    assert!(health.contains("Windows 11"), "{health}");
    // The person uses the VM from the app's Environment panel: each action
    // answers with what it found, next to the VM's state.
    let control = |action: &'static str, command: Option<&'static str>| {
        app.call_as::<Value>(
            methods::CONTROL_PROJECT_WINDOWS_VM,
            json!({"spaceId":"project","action":action,"command":command}),
        )
    };
    let health = control("diagnostics", None).await.unwrap();
    assert_eq!(health["result"]["os"], "Windows 11");
    assert_eq!(health["vm"]["state"], "running");
    let ran = control("exec", Some("hostname")).await.unwrap();
    assert_eq!(ran["result"]["exitCode"], 7);
    assert_eq!(ran["result"]["stdout"], "hello from windows\n");
    let screen = control("screenshot", None).await.unwrap();
    assert_eq!(screen["result"]["mimeType"], "image/png");
    // base64 of the stand-in's "PNG bytes".
    assert_eq!(screen["result"]["data"], "UE5HIGJ5dGVz");
    let launched = control("launch", Some("notepad.exe")).await.unwrap();
    assert_eq!(launched["result"]["launched"], true);
    assert!(control("view", None).await.unwrap()["result"].is_null());
    // A command is needed where one runs, and an unknown action is refused
    // before anything reaches the VM.
    for (action, command) in [("exec", None), ("launch", Some("  ")), ("format", None)] {
        assert!(control(action, command).await.is_err(), "{action}");
    }
    let ran = std::fs::read_to_string(&calls).unwrap();
    for call in [
        "diag",
        "exec --timeout 120 -- hostname",
        "launch -- notepad.exe",
        "view",
    ] {
        assert!(ran.lines().any(|line| line == call), "{call} not in {ran}");
    }
    assert!(!ran.contains("format"));
    // A project that is not connected has nothing to run in.
    let unconnected = app
        .call_as::<Value>(
            methods::CONTROL_PROJECT_WINDOWS_VM,
            json!({"spaceId":"other","action":"diagnostics"}),
        )
        .await;
    assert!(unconnected.unwrap_err().to_string().contains("Connect"));

    // Another project's chat has no VM, and no chat acts for another.
    let error = tools(&core, "other-chat")
        .call("vm_exec", json!({"command":"hostname"}))
        .await
        .unwrap_err();
    assert!(error.contains("not connected"), "{error}");
    let other = agent(&core, "other-chat")
        .call(
            methods::CHAT_WINDOWS_VM,
            json!({"chatId":"project-chat","action":"status"}),
        )
        .await;
    assert!(other.is_err());

    // Starting the service the project names starts the VM with its ports;
    // another service doesn't.
    app.call(
        methods::SAVE_WORKTREE_SETTINGS,
        json!({"spaceId":"project","databases":[],"settings":WorktreeSettings {
            dependencies: WorktreeDependencyMode::Skip,
            ..Default::default()
        }}),
    )
    .await
    .unwrap();
    app.call(
        methods::SAVE_PROJECT_TERMINALS,
        json!({"spaceId":"project","config":ProjectTerminalConfig { services: vec![service("api"), service("web")] }}),
    )
    .await
    .unwrap();
    let ups = || {
        std::fs::read_to_string(&calls)
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("up "))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    for id in ["api", "web"] {
        app.call(
            methods::CONTROL_PROJECT_TERMINALS,
            json!({"spaceId":"project","serviceId":id,"action":"start"}),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(700)).await;
        let expected: &[&str] = if id == "api" {
            &[]
        } else {
            &["up --json --to-vm 3000,3001"]
        };
        assert_eq!(ups(), expected, "after starting {id}");
    }
    app.call(
        methods::CONTROL_PROJECT_TERMINALS,
        json!({"spaceId":"project","action":"stop"}),
    )
    .await
    .unwrap();

    // Disconnected: the chat loses the VM.
    let view: Value = app
        .call_as(
            methods::SET_PROJECT_WINDOWS_VM,
            json!({"spaceId":"project","enabled":false}),
        )
        .await
        .unwrap();
    assert!(view["settings"].is_null());
    assert!(
        tools(&core, "project-chat")
            .call("vm_status", json!({}))
            .await
            .is_err()
    );
}
