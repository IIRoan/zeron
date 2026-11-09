#![cfg(target_os = "linux")]
//! A project sandbox with a real container: a service Zeron starts for the
//! checkout runs inside it, with the checkout at the same path, and closing
//! the service ends its processes there. Needs rootless podman and the
//! helper: `scripts/build-sandbox-helper.sh`, then
//! `cargo test -p zeron-engine --test sandbox_podman -- --ignored`.
use serde_json::json;
use std::{collections::HashMap, path::Path, process::Command, time::Duration};
use zeron_engine::{Terminals, sandbox};

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
