#![cfg(unix)]
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use zeron_engine::{
    Repos, Terminals,
    project_terminals::ProjectTerminals,
    worktree_settings::{WorktreeSettingsStore, normalize},
};
use zeron_proto::*;

struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
    a: PathBuf,
    b: PathBuf,
    store: WorktreeSettingsStore,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for p in [&root, &a, &b] {
            std::fs::create_dir(p).unwrap();
        }
        let store = WorktreeSettingsStore::new(&temp.path().join("state"));
        Self {
            temp,
            root,
            a,
            b,
            store,
        }
    }
    fn settings(&self, mode: WorktreeEnvMode) -> WorktreeSettings {
        std::fs::write(self.root.join(".env.local"), b"TEST_VALUE=source\n").unwrap();
        WorktreeSettings {
            dependencies: WorktreeDependencyMode::Skip,
            env_files: vec![WorktreeEnvFile {
                path: ".env.local".into(),
                mode,
            }],
            ..Default::default()
        }
    }
    fn phase(&self, cwd: &Path) -> WorktreeSetupPhase {
        self.store
            .snapshot(
                &self.root,
                Some(cwd),
                vec![(cwd.to_string_lossy().into_owned(), "test".into())],
            )
            .unwrap()
            .worktrees
            .iter()
            .find(|t| Path::new(&t.path) == cwd)
            .unwrap()
            .state
            .phase
    }
}

#[tokio::test]
async fn independent_env_copies_preserve_edits_and_settings_survive_restart() {
    let f = Fixture::new();
    f.store
        .save(&f.root, None, Some(f.settings(WorktreeEnvMode::Copy)))
        .unwrap();
    f.store.prepare(&f.root, &f.a, false).await.unwrap();
    f.store.prepare(&f.root, &f.b, false).await.unwrap();
    std::fs::write(f.a.join(".env.local"), b"TEST_VALUE=edited\n").unwrap();
    std::fs::write(f.root.join(".env.local"), b"TEST_VALUE=source_changed\n").unwrap();
    f.store.prepare(&f.root, &f.a, true).await.unwrap();
    assert_eq!(
        std::fs::read(f.a.join(".env.local")).unwrap(),
        b"TEST_VALUE=edited\n"
    );
    assert_eq!(
        std::fs::read(f.b.join(".env.local")).unwrap(),
        b"TEST_VALUE=source\n"
    );
    let reopened = WorktreeSettingsStore::new(&f.temp.path().join("state"));
    assert!(reopened.is_prepared(&f.root, &f.a).unwrap());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(f.a.join(".env.local"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test]
async fn follow_moves_latest_edits_both_directions_and_keeps_a_destination_backup() {
    let f = Fixture::new();
    f.store
        .save(&f.root, None, Some(f.settings(WorktreeEnvMode::Follow)))
        .unwrap();
    f.store.activate(&f.root, &f.a).await.unwrap();
    std::fs::write(f.a.join(".env.local"), b"TEST_VALUE=from_a\n").unwrap();
    std::fs::write(f.b.join(".env.local"), b"TEST_VALUE=old_b\n").unwrap();
    f.store.activate(&f.root, &f.b).await.unwrap();
    assert_eq!(
        std::fs::read(f.b.join(".env.local")).unwrap(),
        b"TEST_VALUE=from_a\n"
    );
    std::fs::write(f.b.join(".env.local"), b"TEST_VALUE=from_b\n").unwrap();
    f.store.activate(&f.root, &f.a).await.unwrap();
    assert_eq!(
        std::fs::read(f.a.join(".env.local")).unwrap(),
        b"TEST_VALUE=from_b\n"
    );
    f.store.cleanup(&f.root, &f.b).await.unwrap(); // inactive cleanup cannot overwrite latest active edits
    std::fs::write(f.a.join(".env.local"), b"TEST_VALUE=latest_a\n").unwrap();
    f.store.activate(&f.root, &f.root).await.unwrap();
    assert_eq!(
        std::fs::read(f.root.join(".env.local")).unwrap(),
        b"TEST_VALUE=latest_a\n"
    );
    let backups = f
        .temp
        .path()
        .join("state/worktree-environments/env-backups");
    assert!(std::fs::read_dir(backups).unwrap().count() >= 2);
}

#[tokio::test]
async fn named_follow_profiles_do_not_mix_env_edits() {
    let f = Fixture::new();
    let config = f.settings(WorktreeEnvMode::Follow);
    f.store.save(&f.root, None, Some(config.clone())).unwrap();
    let mut other = config;
    other.env_profile = "other".into();
    f.store.save(&f.root, Some(&f.b), Some(other)).unwrap();
    f.store.activate(&f.root, &f.a).await.unwrap();
    std::fs::write(f.a.join(".env.local"), b"TEST_VALUE=first_profile\n").unwrap();
    f.store.activate(&f.root, &f.b).await.unwrap();
    assert_eq!(
        std::fs::read(f.b.join(".env.local")).unwrap(),
        b"TEST_VALUE=source\n"
    );
    f.store.activate(&f.root, &f.a).await.unwrap();
    assert_eq!(
        std::fs::read(f.a.join(".env.local")).unwrap(),
        b"TEST_VALUE=first_profile\n"
    );
}

#[tokio::test]
async fn link_shares_env_and_install_hook_receives_the_right_checkout() {
    let f = Fixture::new();
    let mut settings = f.settings(WorktreeEnvMode::Link);
    settings.dependencies = WorktreeDependencyMode::Install;
    settings.install_command = "printf '%s' \"$ZERON_CHECKOUT_ROOT\" > install.marker".into();
    settings.setup_command =
        "test -f install.marker && printf '%s' \"$APP_NAME\" > setup.marker".into();
    settings
        .variables
        .insert("APP_NAME".into(), "test_app".into());
    std::fs::write(f.a.join("package.json"), "{}").unwrap();
    f.store.save(&f.root, None, Some(settings)).unwrap();
    f.store.prepare(&f.root, &f.a, false).await.unwrap();
    assert!(
        std::fs::symlink_metadata(f.a.join(".env.local"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(f.a.join("install.marker")).unwrap(),
        f.a.to_string_lossy()
    );
    assert_eq!(
        std::fs::read(f.a.join("setup.marker")).unwrap(),
        b"test_app"
    );
    std::fs::write(f.a.join(".env.local"), b"TEST_VALUE=linked\n").unwrap();
    assert_eq!(
        std::fs::read(f.root.join(".env.local")).unwrap(),
        b"TEST_VALUE=linked\n"
    );
    f.store.prepare(&f.root, &f.a, true).await.unwrap();
}

#[tokio::test]
async fn dependency_copy_keeps_relative_executable_links_and_is_independent() {
    let f = Fixture::new();
    let nm = f.root.join("node_modules");
    std::fs::create_dir_all(nm.join(".bin")).unwrap();
    std::fs::create_dir(nm.join("package")).unwrap();
    std::fs::write(nm.join("package/cli"), "first").unwrap();
    std::os::unix::fs::symlink("../package/cli", nm.join(".bin/tool")).unwrap();
    let settings = WorktreeSettings {
        dependencies: WorktreeDependencyMode::Copy,
        ..Default::default()
    };
    f.store.save(&f.root, None, Some(settings)).unwrap();
    f.store.prepare(&f.root, &f.a, false).await.unwrap();
    std::fs::write(nm.join("package/cli"), "second").unwrap();
    assert_eq!(
        std::fs::read_to_string(f.a.join("node_modules/.bin/tool")).unwrap(),
        "first"
    );
}

#[tokio::test]
async fn overrides_and_inheritance_change_only_the_selected_worktree() {
    let f = Fixture::new();
    let defaults = f.settings(WorktreeEnvMode::Copy);
    f.store.save(&f.root, None, Some(defaults.clone())).unwrap();
    let mut override_ = defaults.clone();
    override_.variables.insert("APP_PORT".into(), "4101".into());
    f.store
        .save(&f.root, Some(&f.a), Some(override_.clone()))
        .unwrap();
    assert_eq!(f.store.effective(&f.root, &f.a).unwrap(), override_);
    assert_eq!(f.store.effective(&f.root, &f.b).unwrap(), defaults);
    f.store.save(&f.root, Some(&f.a), None).unwrap();
    assert_eq!(f.store.effective(&f.root, &f.a).unwrap(), defaults);
    let mut parallel = defaults;
    parallel.service_mode = WorktreeServiceMode::Parallel;
    f.store.save(&f.root, None, Some(parallel)).unwrap();
    assert!(
        f.store
            .save(
                &f.root,
                Some(&f.a),
                Some(f.settings(WorktreeEnvMode::Follow))
            )
            .is_err()
    );
}

#[tokio::test]
async fn failed_setup_is_visible_retryable_and_success_is_idempotent() {
    let f = Fixture::new();
    let mut settings = WorktreeSettings {
        dependencies: WorktreeDependencyMode::Skip,
        setup_command: "test -f allow.setup && printf 'done\\n' >> setup.count".into(),
        ..Default::default()
    };
    f.store.save(&f.root, None, Some(settings.clone())).unwrap();
    assert!(f.store.prepare(&f.root, &f.a, false).await.is_err());
    assert_eq!(f.phase(&f.a), WorktreeSetupPhase::Failed);
    std::fs::write(f.a.join("allow.setup"), "").unwrap();
    f.store.prepare(&f.root, &f.a, true).await.unwrap();
    f.store.prepare(&f.root, &f.a, false).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(f.a.join("setup.count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    settings.branch_prefix = "custom/".into();
    settings.service_mode = WorktreeServiceMode::Parallel;
    f.store.save(&f.root, None, Some(settings)).unwrap();
    f.store.prepare(&f.root, &f.a, false).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(f.a.join("setup.count"))
            .unwrap()
            .lines()
            .count(),
        1,
        "cosmetic and service settings must not reinstall dependencies"
    );
}

#[tokio::test]
async fn setup_output_keeps_recent_errors_and_retry_output_after_reaching_the_limit() {
    let f = Fixture::new();
    let mut settings = WorktreeSettings {
        dependencies: WorktreeDependencyMode::Skip,
        setup_command: "head -c 300000 /dev/zero | tr '\\000' x; printf '\\nLATEST_SETUP_ERROR\\n'; exit 7".into(),
        ..Default::default()
    };
    f.store.save(&f.root, None, Some(settings.clone())).unwrap();
    assert!(f.store.prepare(&f.root, &f.a, false).await.is_err());
    let output = f.store.log(&f.root, &f.a).unwrap();
    assert!(output.contains("LATEST_SETUP_ERROR"));
    assert!(output.len() <= 256 * 1024);
    settings.setup_command = "printf 'RETRY_SUCCEEDED\\n'".into();
    f.store.save(&f.root, None, Some(settings)).unwrap();
    f.store.prepare(&f.root, &f.a, true).await.unwrap();
    assert!(f.store.log(&f.root, &f.a).unwrap().contains("RETRY_SUCCEEDED"));
    assert_eq!(f.phase(&f.a), WorktreeSetupPhase::Ready);
}

#[tokio::test]
async fn cancelling_setup_terminates_its_child_and_records_interruption() {
    let f = Fixture::new();
    let settings = WorktreeSettings {
        dependencies: WorktreeDependencyMode::Skip,
        setup_command: "/bin/sh -c 'echo $$ > setup.pid; exec sleep 60'".into(),
        ..Default::default()
    };
    f.store.save(&f.root, None, Some(settings)).unwrap();
    let store = f.store.clone();
    let root = f.root.clone();
    let cwd = f.a.clone();
    let setup = tokio::spawn(async move { store.prepare(&root, &cwd, false).await });
    let pid = wait_pid(&f.a.join("setup.pid")).await;
    f.store.cancel(&f.a).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(8), setup)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(f.phase(&f.a), WorktreeSetupPhase::Interrupted);
    assert_dead(pid).await;
}

#[tokio::test]
async fn timeout_stops_workflow_processes_and_does_not_claim_ready() {
    let f = Fixture::new();
    let settings = WorktreeSettings {
        dependencies: WorktreeDependencyMode::Skip,
        setup_command: "/bin/sh -c 'echo $$ > setup.pid; exec sleep 60'".into(),
        command_timeout_seconds: 1,
        ..Default::default()
    };
    f.store.save(&f.root, None, Some(settings)).unwrap();
    assert!(f.store.prepare(&f.root, &f.a, false).await.is_err());
    assert_eq!(f.phase(&f.a), WorktreeSetupPhase::Failed);
    assert_dead(wait_pid(&f.a.join("setup.pid")).await).await;
}

#[tokio::test]
async fn unsafe_paths_and_existing_symlinks_are_preserved() {
    let f = Fixture::new();
    let mut config = f.settings(WorktreeEnvMode::Copy);
    config.env_files[0].path = "../outside.env".into();
    assert!(normalize(config).is_err());
    let outside = f.temp.path().join("outside.env");
    std::fs::write(&outside, "preserve").unwrap();
    std::os::unix::fs::symlink(&outside, f.a.join(".env.local")).unwrap();
    f.store
        .save(&f.root, None, Some(f.settings(WorktreeEnvMode::Copy)))
        .unwrap();
    assert!(f.store.prepare(&f.root, &f.a, false).await.is_err());
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "preserve");
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().into()
}

#[tokio::test]
async fn custom_prefix_survives_title_rename_and_delete_preserves_a_user_branch() {
    let f = Fixture::new();
    git(&f.root, &["init", "-b", "main"]);
    git(&f.root, &["config", "user.name", "Test"]);
    git(&f.root, &["config", "user.email", "test@example.invalid"]);
    std::fs::write(f.root.join("README"), "test").unwrap();
    git(&f.root, &["add", "README"]);
    git(&f.root, &["commit", "-m", "initial"]);
    let repos = Repos::with_worktrees_root(
        &f.temp.path().join("repos"),
        "test-device",
        f.temp.path().join("worktrees"),
    );
    repos
        .worktree_settings()
        .save(
            &f.root,
            None,
            Some(WorktreeSettings {
                branch_prefix: "iiroan/".into(),
                dependencies: WorktreeDependencyMode::Skip,
                ..Default::default()
            }),
        )
        .unwrap();
    let wt = repos.create_worktree(&f.root, "main").await.unwrap();
    assert!(wt.branch.starts_with("iiroan/"));
    let renamed = repos
        .rename_worktree_branch(Path::new(&wt.path), &wt.branch, "Improve setup")
        .await
        .unwrap();
    assert_eq!(renamed, "iiroan/improve-setup");
    git(Path::new(&wt.path), &["switch", "-c", "my-own-branch"]);
    repos
        .delete_worktree(&f.root, Path::new(&wt.path))
        .await
        .unwrap();
    assert_eq!(
        git(&f.root, &["branch", "--list", "my-own-branch"]),
        "my-own-branch"
    );
    assert!(repos.delete_worktree(&f.root, &f.root).await.is_err());
}

fn svc() -> ProjectTerminalConfig {
    ProjectTerminalConfig{services:vec![ProjectTerminalService{id:"backend".into(),name:"Backend".into(),command:"/bin/sh -c 'echo $$ > service.pid; echo \"$TEST_PORT\" > port.marker; exec sleep 60'".into(),directory:".".into(),restart_on_failure:false}]}
}

#[tokio::test]
async fn exclusive_handoff_stops_old_process_before_start_and_parallel_keeps_separate_groups() {
    let f = Fixture::new();
    let mut settings = WorktreeSettings {
        dependencies: WorktreeDependencyMode::Skip,
        ..Default::default()
    };
    settings.variables.insert("TEST_PORT".into(), "4100".into());
    f.store.save(&f.root, None, Some(settings.clone())).unwrap();
    let profiles = ProjectTerminals::open(&f.temp.path().join("services")).unwrap();
    let terminals = Terminals::new();
    profiles.save("project", &f.root, svc()).unwrap();
    let a = profiles
        .control_checkout(
            &terminals, &f.store, "project", &f.root, &f.a, None, "start",
        )
        .await
        .unwrap();
    let first = wait_pid(&f.a.join("service.pid")).await;
    let same = profiles
        .control_checkout(
            &terminals, &f.store, "project", &f.root, &f.a, None, "start",
        )
        .await
        .unwrap();
    assert_eq!(
        a.runs, same.runs,
        "two sessions in the same checkout reuse services"
    );
    let b = profiles
        .control_checkout(
            &terminals, &f.store, "project", &f.root, &f.b, None, "activate",
        )
        .await
        .unwrap();
    assert!(
        b.runs
            .iter()
            .any(|r| r.status == ProjectTerminalStatus::Running)
    );
    assert_dead(first).await;
    let second = wait_pid(&f.b.join("service.pid")).await;
    assert_eq!(
        std::fs::read_to_string(f.b.join("port.marker"))
            .unwrap()
            .trim(),
        "4100"
    );
    settings.service_mode = WorktreeServiceMode::Parallel;
    f.store.save(&f.root, None, Some(settings.clone())).unwrap();
    let mut a_settings = settings;
    a_settings
        .variables
        .insert("TEST_PORT".into(), "4101".into());
    f.store.save(&f.root, Some(&f.a), Some(a_settings)).unwrap();
    profiles
        .control_checkout(
            &terminals, &f.store, "project", &f.root, &f.a, None, "start",
        )
        .await
        .unwrap();
    let parallel = wait_new_pid(&f.a.join("service.pid"), first).await;
    assert!(process_alive(second));
    assert_ne!(parallel, second);
    assert_eq!(
        std::fs::read_to_string(f.a.join("port.marker"))
            .unwrap()
            .trim(),
        "4101"
    );
    profiles
        .control_checkout(&terminals, &f.store, "project", &f.root, &f.a, None, "stop")
        .await
        .unwrap();
    assert_dead(parallel).await;
    assert!(process_alive(second));
    profiles.shutdown();
    assert_dead(second).await;
}

async fn wait_pid(path: &Path) -> i32 {
    wait_new_pid(path, 0).await
}
async fn wait_new_pid(path: &Path, old: i32) -> i32 {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(pid) = std::fs::read_to_string(path)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
                && pid != old
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
fn process_alive(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .is_ok_and(|s| !s.rsplit_once(')').unwrap().1.trim().starts_with('Z'))
}
async fn assert_dead(pid: i32) {
    tokio::time::timeout(Duration::from_secs(7), async {
        while process_alive(pid) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("child terminated and released its resources");
}

#[tokio::test]
async fn dropped_setup_recovers_as_interrupted_and_can_be_retried() {
    let f = Fixture::new();
    f.store
        .save(
            &f.root,
            None,
            Some(WorktreeSettings {
                dependencies: WorktreeDependencyMode::Skip,
                setup_command: "/bin/sh -c 'echo $$ > setup.pid; exec sleep 60'".into(),
                ..Default::default()
            }),
        )
        .unwrap();
    let store = f.store.clone();
    let root = f.root.clone();
    let checkout = f.a.clone();
    let task = tokio::spawn(async move { store.prepare(&root, &checkout, false).await });
    let pid = wait_pid(&f.a.join("setup.pid")).await;
    task.abort();
    let _ = task.await;
    assert_dead(pid).await;
    let reopened = WorktreeSettingsStore::new(&f.temp.path().join("state"));
    let snapshot = reopened.snapshot(&f.root, Some(&f.a), vec![]).unwrap();
    assert!(
        snapshot
            .worktrees
            .iter()
            .any(|t| t.state.phase == WorktreeSetupPhase::Interrupted)
    );
    reopened
        .save(
            &f.root,
            None,
            Some(WorktreeSettings {
                dependencies: WorktreeDependencyMode::Skip,
                setup_command: "printf 'recovered' > setup.marker".into(),
                ..Default::default()
            }),
        )
        .unwrap();
    reopened.prepare(&f.root, &f.a, true).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(f.a.join("setup.marker")).unwrap(),
        "recovered"
    );
}

#[tokio::test]
async fn project_root_workflows_get_checkout_context_and_activation_only_runs_on_switch() {
    let f = Fixture::new();
    f.store.save(&f.root,None,Some(WorktreeSettings{dependencies:WorktreeDependencyMode::Skip,commands_in_project:true,setup_command:"test \"$PWD\" = \"$ZERON_PROJECT_ROOT\" && test \"$CODEX_WORKTREE_PATH\" = \"$ZERON_CHECKOUT_ROOT\" && printf 'prepared' > \"$ZERON_CHECKOUT_ROOT/setup.marker\"".into(),activate_command:"printf '%s\\n' \"$ZERON_CHECKOUT_ROOT\" >> activations.log".into(),cleanup_command:"test -d \"$ZERON_CHECKOUT_ROOT\" && printf 'cleaned' > \"$ZERON_CHECKOUT_ROOT/cleanup.marker\"".into(),..Default::default()})).unwrap();
    f.store.activate(&f.root, &f.a).await.unwrap();
    f.store.activate(&f.root, &f.a).await.unwrap();
    f.store.activate(&f.root, &f.b).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(f.root.join("activations.log"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    f.store.cleanup(&f.root, &f.a).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(f.a.join("cleanup.marker")).unwrap(),
        "cleaned"
    );
}

#[tokio::test]
async fn rpc_worktree_settings_are_project_scoped_and_service_polls_do_not_activate() {
    use std::sync::Arc;
    use zeron_rpc::methods;
    let f = Fixture::new();
    git(&f.root, &["init", "-b", "main"]);
    std::fs::write(f.root.join("README"), "test").unwrap();
    git(&f.root, &["add", "README"]);
    git(
        &f.root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    );
    std::fs::remove_dir(&f.a).unwrap();
    git(
        &f.root,
        &["worktree", "add", "--detach", f.a.to_str().unwrap(), "HEAD"],
    );
    let core = zeron_engine::EngineCore::assemble_with_profile(
        zeron_engine::EngineProfile::local(&f.temp.path().join("engine")).unwrap(),
        Arc::new(zeron_engine::HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    core.workspace
        .create_space(
            "project",
            &core.device_id,
            f.root.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    core.workspace
        .create_chat(
            "agent",
            Some("project"),
            Some(&core.device_id),
            None,
            Some(f.a.to_string_lossy().into_owned()),
        )
        .unwrap();
    let client = zeron_rpc::memory_client(core.rpc_service());
    let config = WorktreeSettings {
        dependencies: WorktreeDependencyMode::Skip,
        ..Default::default()
    };
    let saved: WorktreeSettingsSnapshot = client
        .call_as(
            methods::SAVE_WORKTREE_SETTINGS,
            serde_json::json!({"spaceId":"project","settings":config}),
        )
        .await
        .unwrap();
    assert!(saved.checkout.is_none());
    assert!(saved.worktrees.iter().any(|t| Path::new(&t.path) == f.a));
    assert!(
        client
            .call(
                methods::SAVE_WORKTREE_SETTINGS,
                serde_json::json!({"spaceId":"project","checkoutPath":f.b,"settings":config})
            )
            .await
            .is_err()
    );
    let local: WorktreeSettingsSnapshot = client
        .call_as(
            methods::SAVE_WORKTREE_SETTINGS,
            serde_json::json!({"spaceId":"project","checkoutPath":f.a,"settings":config}),
        )
        .await
        .unwrap();
    assert!(local.has_overrides);
    let cfg = svc();
    client
        .call(
            methods::SAVE_PROJECT_TERMINALS,
            serde_json::json!({"spaceId":"project","config":cfg}),
        )
        .await
        .unwrap();
    let root: ProjectTerminalsSnapshot = client
        .call_as(
            methods::GET_PROJECT_TERMINALS,
            serde_json::json!({"spaceId":"project"}),
        )
        .await
        .unwrap();
    assert_eq!(root.config, cfg);
    let poll: ProjectTerminalsSnapshot = client
        .call_as(
            methods::GET_PROJECT_TERMINALS,
            serde_json::json!({"spaceId":"project","chatId":"agent"}),
        )
        .await
        .unwrap();
    assert_eq!(poll.checkout.as_deref(), f.a.to_str());
    assert!(poll.runs.is_empty());
    assert!(
        !core
            .repos
            .worktree_settings()
            .is_prepared(&f.root, &f.a)
            .unwrap(),
        "polling must not prepare or activate a checkout"
    );
    let prepared: WorktreeSettingsSnapshot = client
        .call_as(
            methods::PREPARE_WORKTREE,
            serde_json::json!({"spaceId":"project","checkoutPath":f.a}),
        )
        .await
        .unwrap();
    assert!(
        prepared
            .worktrees
            .iter()
            .any(|t| t.state.phase == WorktreeSetupPhase::Ready)
    );
    core.shutdown().await;
}

#[tokio::test]
async fn preparing_again_preserves_the_active_follow_environment() {
    let f = Fixture::new();
    let mut settings = f.settings(WorktreeEnvMode::Follow);
    f.store.save(&f.root, None, Some(settings.clone())).unwrap();
    f.store.activate(&f.root, &f.a).await.unwrap();
    std::fs::write(f.a.join(".env.local"), b"TEST_VALUE=latest\n").unwrap();
    settings.setup_command = "printf done > setup.marker".into();
    f.store.save(&f.root, None, Some(settings)).unwrap();
    f.store.prepare(&f.root, &f.a, true).await.unwrap();
    assert_eq!(
        std::fs::read(f.a.join(".env.local")).unwrap(),
        b"TEST_VALUE=latest\n"
    );
    f.store.prepare(&f.root, &f.b, false).await.unwrap();
    assert_eq!(
        std::fs::read(f.b.join(".env.local")).unwrap(),
        b"TEST_VALUE=latest\n"
    );
}

#[tokio::test]
async fn independent_install_rejects_a_previously_shared_dependency_link() {
    let f = Fixture::new();
    std::fs::create_dir(f.root.join("node_modules")).unwrap();
    std::fs::write(f.root.join("node_modules/sentinel"), "preserve").unwrap();
    std::fs::write(f.a.join("package.json"), "{}").unwrap();
    let mut settings = WorktreeSettings {
        dependencies: WorktreeDependencyMode::Link,
        ..Default::default()
    };
    f.store.save(&f.root, None, Some(settings.clone())).unwrap();
    f.store.prepare(&f.root, &f.a, false).await.unwrap();
    settings.dependencies = WorktreeDependencyMode::Install;
    settings.install_command = "printf changed > node_modules/sentinel".into();
    f.store.save(&f.root, None, Some(settings)).unwrap();
    assert!(f.store.prepare(&f.root, &f.a, true).await.is_err());
    assert_eq!(
        std::fs::read_to_string(f.root.join("node_modules/sentinel")).unwrap(),
        "preserve"
    );
}

#[tokio::test]
async fn engine_shutdown_cancels_an_activation_workflow_and_rejects_new_setup() {
    let f = Fixture::new();
    f.store
        .save(
            &f.root,
            None,
            Some(WorktreeSettings {
                dependencies: WorktreeDependencyMode::Skip,
                activate_command: "/bin/sh -c 'echo $$ > activate.pid; exec sleep 60'".into(),
                ..Default::default()
            }),
        )
        .unwrap();
    let store = f.store.clone();
    let root = f.root.clone();
    let checkout = f.a.clone();
    let task = tokio::spawn(async move { store.activate(&root, &checkout).await });
    let pid = wait_pid(&f.a.join("activate.pid")).await;
    f.store.shutdown();
    assert!(
        tokio::time::timeout(Duration::from_secs(8), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_dead(pid).await;
    assert_eq!(f.phase(&f.a), WorktreeSetupPhase::Interrupted);
    assert!(f.store.prepare(&f.root, &f.b, false).await.is_err());
}

#[tokio::test]
async fn removing_a_default_service_stops_inheriting_groups_but_keeps_worktree_overrides() {
    let f = Fixture::new();
    f.store
        .save(
            &f.root,
            None,
            Some(WorktreeSettings {
                dependencies: WorktreeDependencyMode::Skip,
                service_mode: WorktreeServiceMode::Parallel,
                ..Default::default()
            }),
        )
        .unwrap();
    let profiles = ProjectTerminals::open(&f.temp.path().join("services")).unwrap();
    let terminals = Terminals::new();
    profiles
        .save_defaults(&f.store, "project", &f.root, svc())
        .await
        .unwrap();
    let mut override_ = f.store.effective(&f.root, &f.b).unwrap();
    override_.services = Some(svc());
    profiles
        .save_environment(&f.store, "project", &f.root, Some(&f.b), Some(override_))
        .await
        .unwrap();
    profiles
        .control_checkout(
            &terminals, &f.store, "project", &f.root, &f.a, None, "start",
        )
        .await
        .unwrap();
    profiles
        .control_checkout(
            &terminals, &f.store, "project", &f.root, &f.b, None, "start",
        )
        .await
        .unwrap();
    let a = wait_pid(&f.a.join("service.pid")).await;
    let b = wait_pid(&f.b.join("service.pid")).await;
    profiles
        .save_defaults(
            &f.store,
            "project",
            &f.root,
            ProjectTerminalConfig::default(),
        )
        .await
        .unwrap();
    assert_dead(a).await;
    assert!(process_alive(b));
    profiles.shutdown();
    assert_dead(b).await;
}

#[tokio::test]
async fn discovery_defaults_to_copying_existing_modules_and_keeps_saved_install_choices() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.root.join("node_modules/example")).unwrap();
    std::fs::write(
        f.root.join("node_modules/example/index.js"),
        "module.exports = 1;",
    )
    .unwrap();
    let initial = f.store.defaults(&f.root).unwrap();
    assert_eq!(initial.dependencies, WorktreeDependencyMode::Copy);
    assert!(initial.dependency_paths.contains(&"node_modules".into()));
    f.store.prepare(&f.root, &f.a, false).await.unwrap();
    assert_eq!(
        std::fs::read(f.a.join("node_modules/example/index.js")).unwrap(),
        b"module.exports = 1;"
    );
    let mut installed = initial;
    installed.dependencies = WorktreeDependencyMode::Install;
    f.store.save(&f.root, None, Some(installed)).unwrap();
    let reopened = WorktreeSettingsStore::new(&f.temp.path().join("state"));
    assert_eq!(
        reopened.defaults(&f.root).unwrap().dependencies,
        WorktreeDependencyMode::Install
    );
    let empty = Fixture::new();
    assert_eq!(
        empty.store.defaults(&empty.root).unwrap().dependencies,
        WorktreeDependencyMode::Install
    );
}

fn workflow_repository() -> (Fixture, Repos) {
    let f = Fixture::new();
    git(&f.root, &["init", "-b", "main"]);
    git(&f.root, &["config", "user.name", "Test"]);
    git(&f.root, &["config", "user.email", "test@example.invalid"]);
    std::fs::write(f.root.join("README"), "test").unwrap();
    git(&f.root, &["add", "README"]);
    git(&f.root, &["commit", "-m", "initial"]);
    let repos = Repos::with_worktrees_root(
        &f.temp.path().join("repos"),
        "test-device",
        f.temp.path().join("worktrees with spaces"),
    );
    (f, repos)
}

#[tokio::test]
async fn settings_removal_preserves_commits_and_rejects_dirty_trees_before_cleanup() {
    let (f, repos) = workflow_repository();
    repos.worktree_settings().save(&f.root, None, Some(WorktreeSettings {
        dependencies: WorktreeDependencyMode::Skip,
        cleanup_command: "printf done > \"$projectroot/cleanup.done\"".into(),
        ..Default::default()
    })).unwrap();
    let wt = repos.create_worktree(&f.root, "main").await.unwrap();
    let path = Path::new(&wt.path);
    std::fs::write(path.join("README"), "committed only on the worktree branch").unwrap();
    git(path, &["add", "README"]);
    git(path, &["commit", "-m", "worktree commit"]);
    let commit = git(path, &["rev-parse", "HEAD"]);
    std::fs::write(path.join("README"), "unsaved work").unwrap();
    std::fs::write(path.join("notes.txt"), "untracked work").unwrap();
    let error = repos.remove_worktree(&f.root, path, false, false).await.unwrap_err();
    assert!(error.to_string().contains("uncommitted changes"));
    assert!(path.exists());
    assert!(!f.root.join("cleanup.done").exists());
    assert_eq!(std::fs::read_to_string(path.join("README")).unwrap(), "unsaved work");
    assert!(repos.remove_worktree(path, &f.root, true, false).await.is_err());
    assert!(repos.remove_worktree(&f.root, &f.a, true, false).await.is_err());
    repos.remove_worktree(&f.root, path, true, false).await.unwrap();
    assert!(!path.exists());
    assert!(f.root.join("cleanup.done").exists());
    assert_eq!(git(&f.root, &["rev-parse", &wt.branch]), commit);
    assert_eq!(git(&f.root, &["branch", "--show-current"]), "main");
    repos.remove_worktree(&f.root, path, false, false).await.unwrap();
    let clean = repos.create_worktree(&f.root, "main").await.unwrap();
    repos.remove_worktree(&f.root, Path::new(&clean.path), false, false).await.unwrap();
    assert!(!Path::new(&clean.path).exists());
    assert!(!git(&f.root, &["branch", "--list", &clean.branch]).is_empty());
}

#[tokio::test]
async fn settings_removal_checks_submodule_edits_and_removes_clean_initialized_submodules() {
    let (f, repos) = workflow_repository();
    git(&f.a, &["init", "-b", "main"]);
    git(&f.a, &["config", "user.name", "Fixture"]);
    git(&f.a, &["config", "user.email", "fixture@example.invalid"]);
    std::fs::write(f.a.join("README"), "module fixture").unwrap();
    git(&f.a, &["add", "README"]);
    git(&f.a, &["commit", "-m", "module fixture"]);
    git(&f.root, &["-c", "protocol.file.allow=always", "submodule", "add", f.a.to_str().unwrap(), "module"]);
    git(&f.root, &["commit", "-am", "add module"]);
    let wt = repos.create_worktree(&f.root, "main").await.unwrap();
    let path = Path::new(&wt.path);
    git(path, &["-c", "protocol.file.allow=always", "submodule", "update", "--init"]);
    std::fs::write(path.join("module/README"), "keep module edits").unwrap();
    assert!(repos.remove_worktree(&f.root, path, false, false).await.is_err());
    assert_eq!(std::fs::read_to_string(path.join("module/README")).unwrap(), "keep module edits");
    std::fs::write(path.join("module/README"), "module fixture").unwrap();
    std::fs::write(path.join("module/untracked.txt"), "keep this too").unwrap();
    assert!(repos.remove_worktree(&f.root, path, false, false).await.is_err());
    std::fs::remove_file(path.join("module/untracked.txt")).unwrap();
    repos.remove_worktree(&f.root, path, false, false).await.unwrap();
    assert!(!path.exists());
    assert!(!git(&f.root, &["branch", "--list", &wt.branch]).is_empty());
}

#[tokio::test]
async fn settings_removal_rpc_stops_services_before_cleanup_and_archives_sessions() {
    let (f, _) = workflow_repository();
    std::fs::write(f.root.join(".gitignore"), "service.pid\n").unwrap();
    git(&f.root, &["add", ".gitignore"]);
    git(&f.root, &["commit", "-m", "ignore service state"]);
    let path = f.temp.path().join("linked-worktree");
    git(&f.root, &["worktree", "add", "-b", "iiroan/removal", path.to_str().unwrap(), "main"]);
    let core = zeron_engine::EngineCore::assemble(
        &f.temp.path().join("engine"), std::sync::Arc::new(zeron_engine::HarnessRegistry::new()),
        HarnessId::Mock, None,
    ).unwrap();
    core.workspace.create_space("project", &core.device_id, f.root.to_str().unwrap(), None, true).unwrap();
    core.workspace.create_chat("session", Some("project"), Some(&core.device_id), None, None).unwrap();
    core.workspace.set_chat_cwd("session", path.to_str().unwrap()).unwrap();
    core.repos.worktree_settings().save(&f.root, None, Some(WorktreeSettings {
        dependencies: WorktreeDependencyMode::Skip,
        cleanup_command: "if kill -0 \"$(cat \"$worktreepath/service.pid\")\" 2>/dev/null; then exit 9; fi; printf done > \"$projectroot/cleanup.done\"".into(),
        ..Default::default()
    })).unwrap();
    core.repos.worktree_settings().remember(&f.root, &path, "iiroan/removal", "iiroan/").unwrap();
    core.project_actions.project_terminals.save("project", &f.root, ProjectTerminalConfig {
        services: svc().services,
    }).unwrap();
    let client = zeron_rpc::memory_client(core.rpc_service());
    client.call(zeron_rpc::methods::CONTROL_PROJECT_TERMINALS, serde_json::json!({
        "spaceId":"project", "checkoutPath":path,"action":"start"
    })).await.unwrap();
    let pid = wait_pid(&path.join("service.pid")).await;
    std::fs::write(path.join("notes.txt"), "keep this until explicitly discarded").unwrap();
    let mut params = serde_json::json!({
        "repoPath":f.root,"worktreePath":path,"force":false,"deleteBranch":false,"archiveSessions":true,
    });
    assert!(client.call(zeron_rpc::methods::DELETE_WORKTREE, params.clone()).await.is_err());
    assert!(process_alive(pid));
    assert!(!core.workspace.chat("session").unwrap().unwrap().archived);
    assert!(!f.root.join("cleanup.done").exists());
    params["force"] = true.into();
    client.call(zeron_rpc::methods::DELETE_WORKTREE, params.clone()).await.unwrap();
    assert_dead(pid).await;
    assert!(!path.exists());
    assert!(f.root.join("cleanup.done").exists());
    assert!(core.workspace.chat("session").unwrap().unwrap().archived);
    assert!(git(&f.root, &["branch", "--list", "iiroan/removal"]).contains("iiroan/removal"));
    client.call(zeron_rpc::methods::DELETE_WORKTREE, params).await.unwrap();
    assert!(core.workspace.chat("session").unwrap().unwrap().archived);
    core.shutdown().await;
}

#[tokio::test]
async fn custom_creation_cleanup_and_removal_receive_real_refs_in_lifecycle_order() {
    let (f, repos) = workflow_repository();
    let config = WorktreeSettings {
        dependencies: WorktreeDependencyMode::Skip,
        branch_prefix: "iiroan/".into(),
        create_command: r#"printf '%s\n' "$branchname" "$basebranch" "$worktreename" "$worktreepath" > "$projectroot/create-context"; git worktree add -b "$branchname" "$worktreepath" "$basebranch""#.into(),
        setup_command: r#"printf '%s\n' "$branchname" "$ZERON_BRANCH_NAME" "$worktreepath" > setup-context"#.into(),
        cleanup_command: r#"printf '%s\n' "$branchname" > "$projectroot/cleanup-context"; test -f "$worktreepath/setup-context""#.into(),
        remove_command: r#"test -f "$projectroot/cleanup-context" && printf '%s\n' "$branchname" "$worktreename" > "$projectroot/remove-context" && git worktree remove --force "$worktreepath""#.into(),
        ..Default::default()
    };
    let store = repos.worktree_settings();
    store.save(&f.root, None, Some(config)).unwrap();
    let wt = repos.create_worktree(&f.root, "main").await.unwrap();
    let path = Path::new(&wt.path);
    assert!(store.is_prepared(&f.root, path).unwrap());
    assert_eq!(
        std::fs::read_to_string(f.root.join("create-context"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec![
            wt.branch.as_str(),
            "main",
            wt.name.as_str(),
            wt.path.as_str()
        ]
    );
    assert_eq!(
        std::fs::read_to_string(path.join("setup-context"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec![wt.branch.as_str(), wt.branch.as_str(), wt.path.as_str()]
    );
    // Shell metacharacters in branch names must remain data in env expansion.
    let user_branch = "user/quote'$(false)";
    git(path, &["switch", "-c", user_branch]);
    repos.delete_worktree(&f.root, path).await.unwrap();
    assert!(!path.exists());
    assert_eq!(
        std::fs::read_to_string(f.root.join("cleanup-context"))
            .unwrap()
            .trim(),
        user_branch
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("remove-context"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec![user_branch, wt.name.as_str()]
    );
    assert_eq!(
        git(&f.root, &["branch", "--list", user_branch]),
        user_branch
    );
    assert_eq!(git(&f.root, &["branch", "--show-current"]), "main");
}

#[tokio::test]
async fn lifecycle_commands_must_actually_create_and_remove_the_selected_worktree() {
    let (f, repos) = workflow_repository();
    let store = repos.worktree_settings();
    let mut settings = WorktreeSettings {
        dependencies: WorktreeDependencyMode::Skip,
        create_command: "true".into(),
        ..Default::default()
    };
    store.save(&f.root, None, Some(settings.clone())).unwrap();
    let error = repos.create_worktree(&f.root, "main").await.unwrap_err();
    assert!(error.to_string().contains("create a linked worktree"));
    assert!(
        store
            .snapshot(&f.root, None, vec![])
            .unwrap()
            .worktrees
            .is_empty(),
        "A command that creates nothing must not leave a phantom worktree in settings"
    );
    settings.create_command = "exit 7".into();
    store.save(&f.root, None, Some(settings.clone())).unwrap();
    assert!(
        repos
            .create_worktree(&f.root, "main")
            .await
            .unwrap_err()
            .to_string()
            .contains("status 7")
    );
    settings.create_command.clear();
    settings.remove_command = "true".into();
    store.save(&f.root, None, Some(settings.clone())).unwrap();
    let wt = repos.create_worktree(&f.root, "main").await.unwrap();
    let error = repos
        .delete_worktree(&f.root, Path::new(&wt.path))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("still exists"));
    assert!(Path::new(&wt.path).exists());
    assert!(git(&f.root, &["branch", "--list", &wt.branch]).contains(&wt.branch));
    settings.remove_command.clear();
    store.save(&f.root, None, Some(settings)).unwrap();
    repos
        .delete_worktree(&f.root, Path::new(&wt.path))
        .await
        .unwrap();
    assert!(!Path::new(&wt.path).exists());
}

#[test]
fn workflow_context_variables_cannot_be_overridden_by_extra_variables() {
    for key in [
        "branchname",
        "basebranch",
        "worktreename",
        "worktreepath",
        "projectroot",
        "ZERON_BRANCH_NAME",
    ] {
        let mut config = WorktreeSettings::default();
        config.variables.insert(key.into(), "override".into());
        assert!(normalize(config).is_err(), "{key}");
    }
}

#[tokio::test]
async fn shutdown_cancels_cleanup_without_removing_the_worktree() {
    let f = Fixture::new();
    f.store
        .save(
            &f.root,
            None,
            Some(WorktreeSettings {
                dependencies: WorktreeDependencyMode::Skip,
                cleanup_command: "echo $$ > cleanup.pid; exec sleep 60".into(),
                ..Default::default()
            }),
        )
        .unwrap();
    let store = f.store.clone();
    let root = f.root.clone();
    let checkout = f.a.clone();
    let cleanup = tokio::spawn(async move { store.cleanup(&root, &checkout).await });
    let pid = wait_pid(&f.a.join("cleanup.pid")).await;
    f.store.shutdown();
    let result = tokio::time::timeout(Duration::from_secs(5), cleanup)
        .await
        .unwrap()
        .unwrap();
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    assert_dead(pid).await;
    assert!(f.a.is_dir());
    assert_eq!(f.phase(&f.a), WorktreeSetupPhase::Interrupted);
}
