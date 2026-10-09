//! Isolated process: this test changes SHELL/PATH and warms the shell PATH cache.
#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use zeron_engine::Terminals;
use zeron_proto::TerminalEvent;

async fn exit_code(terminals: &Terminals, id: &str) -> i32 {
    let mut events = terminals.subscribe(id, None).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = events.recv().await {
            if let TerminalEvent::Exit { exit_code, .. } = event {
                return exit_code;
            }
        }
        panic!("service ended without an exit event");
    })
    .await
    .expect("service exits")
}

#[tokio::test]
async fn terminals_use_interactive_login_path_and_respect_explicit_overrides() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("interactive-bin");
    std::fs::create_dir(&bin).unwrap();
    let tool = bin.join("zeron-shell-only-tool");
    std::fs::write(&tool, "#!/bin/sh\nprintf 'resolved-through-shell'\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();
    let shell = root.path().join("shell-fixture");
    // Emulate Homebrew initialization restricted to interactive shells. A
    // non-interactive service has to inherit the captured interactive PATH.
    std::fs::write(
        &shell,
        format!(
            "#!/bin/sh\nfor flag in \"$@\"; do\n  if [ \"$flag\" = -i ]; then\n    export PATH=\"{}:$PATH\"\n  fi\ndone\nexec /bin/sh \"$@\"\n",
            bin.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
    // SAFETY: a single current-thread test in a dedicated integration binary.
    unsafe {
        std::env::set_var("SHELL", &shell);
        std::env::set_var("HOME", root.path());
        std::env::set_var("PATH", "/usr/bin:/bin");
        std::env::remove_var("ZERON_NO_LOGIN_SHELL");
    }
    let terminals = Terminals::new();
    let cwd = root.path().to_str().unwrap();
    let service = terminals
        .open_service(
            cwd,
            &HashMap::new(),
            "zeron-shell-only-tool > service-output",
        )
        .unwrap();
    assert_eq!(exit_code(&terminals, &service.id).await, 0);
    assert_eq!(
        std::fs::read_to_string(root.path().join("service-output")).unwrap(),
        "resolved-through-shell"
    );
    terminals.close(&service.id).unwrap();

    let explicit = HashMap::from([("PATH".into(), "/usr/bin:/bin".into())]);
    let service = terminals
        .open_service(cwd, &explicit, "zeron-shell-only-tool")
        .unwrap();
    assert_eq!(exit_code(&terminals, &service.id).await, 127);
    terminals.close(&service.id).unwrap();

    let terminal = terminals
        .open_with_shell(cwd, 80, 24, Some("/bin/sh"))
        .unwrap();
    terminals
        .write(&terminal.id, "zeron-shell-only-tool > terminal-output\r")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if std::fs::read_to_string(root.path().join("terminal-output"))
                .is_ok_and(|s| s == "resolved-through-shell")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("ordinary terminal inherits the login PATH too");
    terminals.close(&terminal.id).unwrap();
}
