#![cfg(target_os = "linux")]
//! A project sandbox with a real container: a service Zeron starts for the
//! checkout runs inside it, with the checkout at the same path, and closing
//! the service ends its processes there. Needs rootless podman and the
//! helper: `scripts/build-sandbox-helper.sh`, then
//! `cargo test -p zeron-engine --test sandbox_podman -- --ignored`.
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, process::Command, sync::Arc, time::Duration};
use zeron_engine::{EngineCore, HarnessRegistry, Terminals, sandbox};
use zeron_proto::HarnessId;
use zeron_rpc::{Caller, RpcError, RpcReply, RpcService, methods};

struct Agent {
    service: Arc<dyn RpcService>,
    chat: String,
}
#[async_trait::async_trait]
impl RpcService for Agent {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        assert!(zeron_rpc::agent_allowed(method), "{method}");
        zeron_rpc::with_caller(
            Caller::Agent {
                chat: Some(self.chat.clone()),
            },
            self.service.handle(method, params),
        )
        .await
    }
}

fn podman(args: &[&str]) -> String {
    let out = Command::new("podman").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "podman {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn sleeping(container: &str) -> usize {
    podman(&[
        "exec",
        container,
        "sh",
        "-c",
        "ps -o comm | grep -c '^sleep$' || true",
    ])
    .parse()
    .unwrap()
}

async fn eventually(what: &str, check: impl Fn() -> bool) {
    for _ in 0..100 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs rootless podman and target/sandbox-helper/zeron-sandbox"]
async fn a_sandboxed_service_runs_in_the_container_and_ends_with_it() {
    let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/sandbox-helper/zeron-sandbox")
        .canonicalize()
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let root = base.join("repo");
    std::fs::create_dir_all(&root).unwrap();
    // SAFETY: this binary's only test, before any thread reads them.
    unsafe {
        std::env::set_var("ZERON_DATA_DIR", base.join("data"));
        std::env::set_var("ZERON_SANDBOX_HELPER", &helper);
        std::env::set_var("ZERON_WORKTREES_DIR", base.join("worktrees"));
    }
    std::fs::create_dir_all(base.join("data")).unwrap();
    std::fs::write(
        base.join("data/sandboxes.json"),
        json!({"projects": [{"root": root, "worktrees": base.join("worktrees"), "config": base.join("devcontainer.json")}]}).to_string(),
    )
    .unwrap();
    let sandbox = sandbox::for_path(&root).unwrap().unwrap();
    podman(&["pull", "-q", "docker.io/library/alpine:3.20"]);
    podman(&["tag", "docker.io/library/alpine:3.20", &sandbox.image()]);

    let terminals = Terminals::new();
    let env = HashMap::from([("MARK".to_owned(), "from-zeron".to_owned())]);
    let session = terminals
        .open_service(
            root.to_str().unwrap(),
            &env,
            "test -f /run/.containerenv && echo \"$MARK $PWD\" > inside; exec sleep 300",
        )
        .unwrap();
    let inside = root.join("inside");
    eventually("the service to write from the container", || {
        inside.exists()
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(&inside).unwrap().trim(),
        format!("from-zeron {}", root.display())
    );
    let container = sandbox.container();
    eventually("its process in the container", || sleeping(&container) == 1).await;
    terminals.close(&session.id).unwrap();
    eventually("its process to end with the service", || {
        sleeping(&container) == 0
    })
    .await;

    // Set up through the RPC the MCP `sandbox_enable` tool calls: a
    // full-access chat outside every sandbox builds and starts one for its
    // own project; inside it, the same chat can't change it.
    let project = base.join("project");
    let config = base.join("config");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("Dockerfile"),
        "FROM docker.io/library/alpine:3.20\n",
    )
    .unwrap();
    std::fs::write(
        config.join("devcontainer.json"),
        r#"{"build":{"dockerfile":"Dockerfile"}}"#,
    )
    .unwrap();
    let core = EngineCore::assemble(
        &base.join("state"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    core.workspace
        .create_space(
            "project",
            &core.device_id,
            project.to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    core.workspace
        .create_chat_hosted(
            "host",
            Some("project"),
            Some(&core.device_id),
            Some(&core.device_id),
            None,
            Some(project.to_string_lossy().into_owned()),
            None,
            None,
        )
        .unwrap();
    let agent = zeron_rpc::memory_client(Arc::new(Agent {
        service: core.rpc_service(),
        chat: "host".into(),
    }));
    let enabled = agent
        .call(methods::SET_SANDBOX, json!({"chatId": "host", "enabled": true, "config": config.join("devcontainer.json"), "publishPorts": [3999]}))
        .await
        .unwrap();
    assert_eq!(enabled["chat"]["inSandbox"], true, "{enabled}");
    let mine = enabled["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["root"] == json!(project))
        .unwrap()
        .clone();
    assert_eq!(
        (mine["state"].as_str(), mine["imageBuilt"].as_bool()),
        (Some("running"), Some(true)),
        "{mine}"
    );
    let refused = agent
        .call(
            methods::SET_SANDBOX,
            json!({"chatId": "host", "enabled": false}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("runs in a project sandbox"), "{refused}");
    let second = sandbox::for_path(&project).unwrap().unwrap();
    sandbox::disable(&project).unwrap();
    core.shutdown().await;
    for args in [
        vec!["rm", "-f", "-t", "0", &second.container()],
        vec![
            "volume",
            "rm",
            "-f",
            &format!("{}-home", second.container()),
        ],
        vec!["rmi", &second.image()],
    ] {
        let _ = Command::new("podman").args(args).output();
    }

    let _ = Command::new("podman")
        .args(["rm", "-f", "-t", "0", &container])
        .output();
    let _ = Command::new("podman")
        .args(["volume", "rm", "-f", &format!("{container}-home")])
        .output();
    let _ = Command::new("podman")
        .args(["rmi", &sandbox.image()])
        .output();
}
