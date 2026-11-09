#![cfg(unix)]
//! Project sandboxes (`zeron_engine::sandbox`), without podman or an image:
//! what's sandboxed, that nothing for it falls back to the host, that host
//! git can't be redirected by a checkout, and what agents can't do from
//! inside one. One test, because the registry is this process's data dir.
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, sync::Arc};
use zeron_engine::{EngineCore, HarnessRegistry, Terminals, sandbox};
use zeron_proto::{HarnessId, RunRequest};
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

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn request(cwd: &Path) -> RunRequest {
    serde_json::from_value(json!({"prompt": "hi", "cwd": cwd, "sandbox": "workspace-write", "mcp": {"name": "zeron", "command": "/usr/bin/zeron", "args": ["mcp"]}})).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sandboxed_project_never_runs_on_the_host_and_its_agents_stay_inside() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let (root, worktrees, other) = (
        base.join("repo"),
        base.join("worktrees/repo"),
        base.join("other"),
    );
    // SAFETY: this binary's only test, before any thread reads them.
    unsafe {
        std::env::set_var("ZERON_DATA_DIR", base.join("data"));
        // A helper that exists, for an image that doesn't: the container
        // can't start, which is what must never fall back to the host.
        std::env::set_var("ZERON_SANDBOX_HELPER", "/bin/true");
    }
    for dir in [&root, &other] {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
    }
    let worktree = worktrees.join("wt");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt",
            worktree.to_str().unwrap(),
        ],
    );
    std::fs::create_dir_all(base.join("data")).unwrap();
    std::fs::write(
        base.join("data/sandboxes.json"),
        json!({"projects": [{"root": root, "worktrees": worktrees, "config": base.join("devcontainer.json")}]}).to_string(),
    )
    .unwrap();

    // What's in it: the project, its subfolders and its worktrees.
    for inside in [&root, &root.join("src/new"), &worktree] {
        assert!(
            sandbox::for_path(inside).unwrap().is_some(),
            "{}",
            inside.display()
        );
    }
    assert!(sandbox::for_path(&other).unwrap().is_none());

    // Host git ignores a worktree's `.git` pointer once an agent rewrites it.
    let planted = base.join("PLANTED");
    let evil = worktree.join(".evil");
    std::fs::create_dir_all(&evil).unwrap();
    for part in ["HEAD", "objects", "refs"] {
        std::process::Command::new("cp")
            .arg("-r")
            .arg(root.join(".git").join(part))
            .arg(&evil)
            .status()
            .unwrap();
    }
    std::fs::write(
        evil.join("config"),
        format!("[core]\n\tfsmonitor = touch {}\n", planted.display()),
    )
    .unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", evil.display()),
    )
    .unwrap();
    let env: HashMap<_, _> = sandbox::git_env(&worktree).unwrap().into_iter().collect();
    assert_eq!(env["GIT_DIR"], root.join(".git/worktrees/wt"));
    assert_eq!(env["GIT_COMMON_DIR"], root.join(".git"));
    let mut status = tokio::process::Command::new("git");
    status.arg("-C").arg(&worktree).args(["status", "--short"]);
    sandbox::pin_git(&mut status, &worktree);
    assert!(status.output().await.unwrap().status.success());
    assert!(!planted.exists(), "pinned host git ran a planted fsmonitor");
    assert!(sandbox::git_env(&other).unwrap().is_empty());

    // Terminals and services fail closed; databases and other projects run here.
    let terminals = Terminals::new();
    let env = HashMap::new();
    for cwd in [&root, &worktree] {
        let refused = terminals
            .open_service(cwd.to_str().unwrap(), &env, "touch ran-on-host")
            .unwrap_err()
            .to_string();
        assert!(refused.contains("sandbox image isn't built"), "{refused}");
    }
    assert!(!root.join("ran-on-host").exists());
    terminals
        .open_host_service(root.to_str().unwrap(), &env, "true")
        .unwrap();
    terminals
        .open_service(other.to_str().unwrap(), &env, "true")
        .unwrap();

    // Agent runs: only Claude Code and Codex, only inside.
    let refused = sandbox::stamp_run(HarnessId::Cursor, &mut request(&root))
        .await
        .unwrap_err();
    assert!(
        refused.contains("supports Claude Code and Codex"),
        "{refused}"
    );
    let refused = sandbox::stamp_run(HarnessId::ClaudeCode, &mut request(&worktree))
        .await
        .unwrap_err();
    assert!(
        refused.contains("Couldn't start the project's sandbox"),
        "{refused}"
    );
    let mut outside = request(&other);
    sandbox::stamp_run(HarnessId::ClaudeCode, &mut outside)
        .await
        .unwrap();
    assert!(outside.env.is_empty() && outside.mcp.unwrap().command == "/usr/bin/zeron");

    // From inside, an agent can't start or reach work outside, nor edit the
    // databases the host runs.
    let core = EngineCore::assemble(
        &base.join("state"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    for (id, path) in [("project", &root), ("elsewhere", &other)] {
        core.workspace
            .create_space(id, &core.device_id, path.to_str().unwrap(), None, true)
            .unwrap();
    }
    for (id, space, cwd) in [
        ("inside", "project", &worktree),
        ("outside", "elsewhere", &other),
    ] {
        core.workspace
            .create_chat_hosted(
                id,
                Some(space),
                Some(&core.device_id),
                Some(&core.device_id),
                None,
                Some(cwd.to_string_lossy().into_owned()),
                None,
                None,
            )
            .unwrap();
    }
    let agent = zeron_rpc::memory_client(Arc::new(Agent {
        service: core.rpc_service(),
        chat: "inside".into(),
    }));
    let config = json!({"harness": "mock", "sandbox": "workspace-write", "access": "full-access"});
    for (method, params) in [
        (
            methods::MUTATE,
            json!({"op": "createChat", "chatId": "a", "spaceId": "elsewhere", "config": config}),
        ),
        (
            methods::MUTATE,
            json!({"op": "createChat", "chatId": "b", "spaceId": "project", "cwd": other, "config": config}),
        ),
        (
            methods::MUTATE,
            json!({"op": "createChat", "chatId": "c", "deviceId": core.device_id, "config": config}),
        ),
        (
            methods::MUTATE,
            json!({"op": "createChat", "chatId": "d", "spaceId": "project", "cloud": true, "config": config}),
        ),
        (
            methods::QUEUE_MESSAGE,
            json!({"chatId": "outside", "text": "run this on the host"}),
        ),
        (
            methods::EDIT_CHAT_SERVICE,
            json!({"chatId": "inside", "edit": {"op": "add", "kind": "database", "name": "DB", "command": "true", "readinessCommand": "true"}}),
        ),
    ] {
        let refused = agent
            .call(method, params.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(refused.contains("sandbox"), "{params}: {refused}");
    }
    agent
        .call(
            methods::MUTATE,
            json!({"op": "createChat", "chatId": "e", "spaceId": "project", "config": config}),
        )
        .await
        .unwrap();
    let added = agent
        .call(
            methods::EDIT_CHAT_SERVICE,
            json!({"chatId": "inside", "edit": {"op": "add", "name": "Web", "command": "true"}}),
        )
        .await
        .unwrap();
    assert_eq!(added["service"]["kind"], "service");

    // Command discovery would start the CLI on the host in the checkout.
    let local = zeron_rpc::memory_client(core.rpc_service());
    let commands = local
        .call(
            methods::LIST_COMMANDS,
            json!({"harness": "claude-code", "chatId": "inside"}),
        )
        .await
        .unwrap();
    assert_eq!(commands, json!([]));
    core.shutdown().await;
}
