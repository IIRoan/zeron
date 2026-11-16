//! The Linux fork's plugins (plugins-fork/): each loads into the shell as a
//! bundled one does, and the logic they share or keep apart from their
//! views (fork_sdk.luau's ports, the sandbox's and the VM's models) reads
//! the engine's replies as the desktop app did.
use std::path::{Path, PathBuf};

use serde_json::json;
use zgx_script::{ScriptHost, testing::TestApp};

use super::Item;
use super::examples_tests::{World, shell_with};

const FORK_PLUGINS: &[&str] = &["project-services", "zeron-git", "project-sandbox", "windows-vm"];

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins-fork")
}

/// A fork plugin as the bridge lists a bundled one.
fn fork(id: &str) -> Item {
    let dir = dir().join(id);
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("zeron-plugin.json")).unwrap()).unwrap();
    Item {
        json: json!({
            "id": id, "name": manifest["name"], "status": "on", "entry": manifest["entry"],
            "snapshot": dir, "permissions": manifest["permissions"], "commands": manifest["commands"],
            "ui": manifest.get("ui").cloned().unwrap_or(json!({})),
        }),
    }
}

/// What loading `items` into the shell said went wrong: a script that
/// failed as it loaded (the window's error), and each plugin's problems.
fn problems(items: Vec<Item>) -> Vec<(String, String)> {
    let world: World = std::rc::Rc::new(std::cell::RefCell::new(json!({ "viewer": "ada" })));
    let (app, heard) = shell_with(items, world);
    let mut said: Vec<(String, String)> = app.error().map(|e| ("error".to_string(), e)).into_iter().collect();
    said.extend(heard.borrow().iter().filter(|(key, _)| key.ends_with(".problem")).cloned());
    said
}

#[test]
fn every_fork_plugin_loads_into_the_shell() {
    for id in FORK_PLUGINS {
        let said = problems(vec![fork(id)]);
        assert!(said.is_empty(), "{id} reported problems: {said:?}");
    }
    // (One that fails as it loads is told: this test would see it.)
    let broken = std::env::temp_dir().join(format!("zeron-fork-broken-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&broken);
    std::fs::create_dir_all(&broken).unwrap();
    for name in ["zeron-plugin.json", "zeron.luau", "fork_sdk.luau", "model.luau", "settings.luau", "view.luau"] {
        std::fs::copy(dir().join("project-sandbox").join(name), broken.join(name)).unwrap();
    }
    std::fs::write(broken.join("init.luau"), "local zeron = require(\"./zeron\")(\"project-sandbox\")\nerror(\"boom\")\n").unwrap();
    let mut item = fork("project-sandbox");
    item.json["snapshot"] = json!(broken);
    let said = problems(vec![item]);
    assert!(said.iter().any(|(_, words)| words.contains("boom")), "a broken plugin wasn't told: {said:?}");
    let _ = std::fs::remove_dir_all(broken);
}

/// The fork's plugins that use the SDK carry upstream's own unchanged
/// (plugins-sdk/zeron.luau stays upstream's) and the fork's addition beside
/// it (plugins-fork/fork_sdk.luau), byte for byte. Linux Workbench replaces
/// screens and uses neither. Fix with
/// `for d in apps/zgx/plugins-fork/*/; do [ -f "$d/zeron.luau" ] && cp apps/zgx/plugins-sdk/zeron.luau apps/zgx/plugins-fork/fork_sdk.luau "$d"; done`.
#[test]
fn fork_plugins_carry_the_sdk_and_its_fork_addition() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sdk = std::fs::read(root.join("plugins-sdk/zeron.luau")).unwrap();
    let addition = std::fs::read(dir().join("fork_sdk.luau")).unwrap();
    let mut stale = Vec::new();
    for id in FORK_PLUGINS {
        for (name, canonical) in [("zeron.luau", &sdk), ("fork_sdk.luau", &addition)] {
            let copy = dir().join(id).join(name);
            if std::fs::read(&copy).ok().as_ref() != Some(canonical) {
                stale.push(copy.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    assert!(stale.is_empty(), "stale or missing SDK copies in fork plugins: {stale:?}");
}

/// `script` run in a window of its own, the named fork files beside it: it
/// passes by drawing `passed` (Luau `assert`s fail it).
fn run_luau(name: &str, files: &[(&str, &str)], script: &str) {
    let root = std::env::temp_dir().join(format!("zeron-fork-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for (from, to) in files {
        std::fs::copy(dir().join(from), root.join(to)).unwrap();
    }
    let entry = root.join("app.luau");
    std::fs::write(&entry, format!("{script}\nreturn ui.text {{ id = 'passed', 'passed' }}")).unwrap();
    let mut app = TestApp::new(move |runtime| ScriptHost::new(runtime, &entry));
    app.frames(2);
    assert_eq!(app.error(), None, "{name}");
    assert_eq!(app.text("passed"), "passed", "{name}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn fork_sdk_reads_and_writes_ports_as_the_engine_does() {
    run_luau(
        "ports",
        &[("fork_sdk.luau", "fork_sdk.luau")],
        r#"
local z = require("./fork_sdk")({}, "test")
local ports, why = z.ports.parse("5432, 6379\n8000-8002")
assert(why == nil and #ports == 5 and ports[1] == 5432 and ports[3] == 8000 and ports[5] == 8002)
assert(#(z.ports.parse("")) == 0)
assert(select(2, z.ports.parse("0")) == "0 isn't a port or a range of ports")
assert(select(2, z.ports.parse("70000")) == "70000 isn't a port or a range of ports")
assert(select(2, z.ports.parse("80,abc")) == "abc isn't a port or a range of ports")
assert(select(2, z.ports.parse("9-3")) == "9-3 isn't a port range")
assert(z.ports.format({ 3001, 3000, 3002, 5432, 3000 }) == "3000-3002, 5432")
assert(z.ports.format(nil) == "")
local words = z.words("api, web\nworker  db")
assert(#words == 4 and words[1] == "api" and words[4] == "db")
assert(type(z.workbench) == "function")
"#,
    );
}

#[test]
fn the_sandbox_model_reads_its_build_log_as_the_desktop_did() {
    run_luau(
        "sandbox-model",
        &[("project-sandbox/model.luau", "model.luau")],
        r#"
local Model = require("./model")({})
local log = "▸ Checking the devcontainer config\nok\n▸ Building the image (devcontainer build)\nstep 3/9\n"
assert(Model.last_step(log) == "▸ Building the image (devcontainer build)")
assert(Model.latest(log) == "step 3/9")
assert(not Model.failed({ log = log }))
assert(Model.failed({ log = log .. "▸ Failed: the image didn't build\n" }))
assert(not Model.failed(nil) and Model.last_step(nil) == nil)
assert(Model.tail("a\nb\nc\n", 2) == "b\nc")
assert(Model.tail("", 600) == "")
"#,
    );
}

#[test]
fn the_vm_model_says_its_readings_as_the_desktop_did() {
    run_luau(
        "vm-model",
        &[("windows-vm/model.luau", "model.luau")],
        r#"
local Model = require("./model")({})
assert(Model.duration(59) == "0 min")
assert(Model.duration(3 * 3600 + 20 * 60) == "3 h 20 min")
assert(Model.duration(2 * 86400 + 5 * 3600) == "2 d 5 h")
assert(Model.level(50) == "normal" and Model.level(80) == "warning" and Model.level(95) == "critical")
assert(Model.ran({ exitCode = 0 }) == "Finished · exit code 0")
assert(Model.ran({ exitCode = 3 }) == "Failed · exit code 3")
assert(Model.ran({ timedOut = true }) == "Stopped · it ran longer than two minutes")
assert(Model.ran({}) == "Ended without an exit code")
"#,
    );
}

type Calls = std::rc::Rc<std::cell::RefCell<Vec<(String, serde_json::Value)>>>;

/// A fork plugin's tab view alone in a window with the design system: the
/// project "Atlas" on device d1, its project tools answered by `reply`
/// (method, params → the engine's reply) and every call kept in order.
fn mount(
    plugin: &str,
    files: &[&str],
    view: &str,
    reply: impl Fn(&str, &serde_json::Value) -> serde_json::Value + 'static,
) -> (TestApp, Calls) {
    let root = std::env::temp_dir().join(format!("zeron-fork-view-{plugin}-{}-{}", std::process::id(), rand_suffix()));
    std::fs::create_dir_all(&root).unwrap();
    for name in files {
        std::fs::copy(dir().join(plugin).join(name), root.join(name)).unwrap();
    }
    let entry = root.join("app.luau");
    std::fs::write(
        &entry,
        format!(
            r#"
local fake = {{
	context_of = function(kind, id)
		if kind == "project" then
			return {{ id = id, name = "Atlas", path = "/work/atlas", device = "d1" }}
		end
		return {{ id = id }}
	end,
	toast = function(text) ui.invoke("test.toast", text) end,
}}
local zeron = require("./fork_sdk")(fake, "{plugin}")
local Model = require("./model")(zeron)
local Settings = require("./settings")(zeron, Model)
local View = require("./view")(zeron, Model, Settings)
return ui.div {{ w_full = true, h_full = true, View {{ project = "p1" }} }}
"#
        ),
    )
    .unwrap();
    let calls: Calls = Calls::default();
    let kept = calls.clone();
    let reply = std::rc::Rc::new(reply);
    let action = format!("plugin.{plugin}.workbench");
    let ui_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
    let app = TestApp::new(move |runtime| {
        let host = super::lend_design_system(ScriptHost::new(runtime, entry.clone()), &ui_dir.join("app.luau"));
        host.action("test.toast", |_| {});
        let (calls, reply) = (kept.clone(), reply.clone());
        host.async_action(action.clone(), move |asked: zgx_script::Value| {
            let asked = super::value_json(&asked);
            let method = asked["method"].as_str().unwrap_or_default().to_string();
            calls.borrow_mut().push((method.clone(), asked["params"].clone()));
            let answer = reply(&method, &asked["params"]);
            async move { Ok(super::json_value(&answer)) }
        });
        host
    });
    app.ui.scene.borrow_mut().resize(520., 900.);
    app.frames(20);
    (app, calls)
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn shows(app: &mut TestApp, words: &str) -> bool {
    app.texts().iter().any(|t| t.contains(words))
}

const SANDBOX_FILES: &[&str] = &["fork_sdk.luau", "model.luau", "settings.luau", "view.luau"];

fn sandbox_on() -> serde_json::Value {
    json!({
        "available": true,
        "sandbox": {
            "root": "/work/atlas", "config": "/work/atlas/.devcontainer/devcontainer.json",
            "hostPorts": [5432], "publishPorts": [], "readOnly": ["compose.yaml"],
            "shareGithub": true, "signCommits": false,
            "container": "zeron-sandbox-atlas", "state": "running", "imageBuilt": true,
        },
        "build": { "running": false, "log": "▸ Checking the devcontainer config\n▸ Building the image (devcontainer build)\n▸ Starting the container\n▸ Ready\n" },
        "suggestedConfig": null,
    })
}

#[test]
fn the_sandbox_tab_says_off_and_offers_to_set_it_up() {
    let (mut app, calls) = mount("project-sandbox", SANDBOX_FILES, "view", |method, _| match method {
        "GetProjectSandbox" => json!({ "available": true, "sandbox": null, "build": { "running": false, "log": "" },
            "suggestedConfig": "/work/atlas/.devcontainer/devcontainer.json" }),
        _ => json!({}),
    });
    assert_eq!(app.error(), None);
    assert!(app.wait_until(|a| shows(a, "Set up")), "{}", app.tree());
    assert!(shows(&mut app, "Off"));
    assert!(shows(&mut app, "run on this machine"));
    // Project-wide: it asks with the project's context, nothing else.
    assert_eq!(calls.borrow()[0].0, "GetProjectSandbox");
}

#[test]
fn the_sandbox_tab_shows_its_container_and_rebuilds_only_when_asked_twice() {
    let (mut app, calls) = mount("project-sandbox", SANDBOX_FILES, "view", |method, params| match method {
        "GetProjectSandbox" => sandbox_on(),
        "SetProjectSandbox" => {
            assert_eq!(params["enabled"], true);
            sandbox_on()
        }
        _ => json!({}),
    });
    assert_eq!(app.error(), None);
    assert!(app.wait_until(|a| shows(a, "zeron-sandbox-atlas")), "{}", app.tree());
    assert!(shows(&mut app, "Running"));
    assert!(shows(&mut app, "5432"), "the host ports it reaches");
    assert!(shows(&mut app, "Every port a server opens"), "no published ports: automatic");
    assert!(shows(&mut app, "compose.yaml"));
    // Rebuild stops what runs in it: a first press only warns.
    app.click("role:button \"Rebuild\"");
    app.frames(5);
    assert!(shows(&mut app, "Press Rebuild again"), "{}", app.tree());
    assert!(!calls.borrow().iter().any(|(m, _)| m == "SetProjectSandbox"));
    app.click("role:button \"Rebuild now\"");
    assert!(app.wait_until(|_| calls.borrow().iter().any(|(m, _)| m == "SetProjectSandbox")));
    let sent = calls.borrow().iter().find(|(m, _)| m == "SetProjectSandbox").unwrap().1.clone();
    // Built again with what it has: the same config, ports and files.
    assert_eq!(sent["config"], "/work/atlas/.devcontainer/devcontainer.json");
    assert_eq!(sent["hostPorts"], json!([5432]));
    assert_eq!(sent["readOnly"], json!(["compose.yaml"]));
    assert_eq!(sent["shareGithub"], true);
}

#[test]
fn the_sandbox_settings_refuse_a_relative_config_and_send_what_the_form_says() {
    let (mut app, calls) = mount("project-sandbox", SANDBOX_FILES, "view", |method, _| match method {
        "GetProjectSandbox" => json!({ "available": true, "sandbox": null, "build": { "running": false, "log": "" },
            "suggestedConfig": "/work/atlas/.devcontainer/devcontainer.json" }),
        "SetProjectSandbox" => sandbox_on(),
        _ => json!({}),
    });
    assert!(app.wait_until(|a| shows(a, "Set up")));
    app.click("role:button \"Set up\"");
    assert!(app.wait_until(|a| shows(a, "Turn on")), "{}", app.tree());
    // (The sheet slides in from the window's edge: done before it's used.)
    app.frames(90);
    // Filled with the project's own devcontainer, as the engine suggests.
    assert!(shows(&mut app, "/work/atlas/.devcontainer/devcontainer.json"));
    app.fill("role:textbox \"Devcontainer config\"", "devcontainer.json");
    app.click("role:button \"Turn on\"");
    app.frames(5);
    assert!(shows(&mut app, "Devcontainer config: give its full path"), "{}", app.tree());
    assert!(!calls.borrow().iter().any(|(m, _)| m == "SetProjectSandbox"));
    app.fill("role:textbox \"Devcontainer config\"", "/work/atlas/.devcontainer/devcontainer.json");
    app.click("role:button \"Turn on\"");
    assert!(app.wait_until(|_| calls.borrow().iter().any(|(m, _)| m == "SetProjectSandbox")));
    let sent = calls.borrow().iter().find(|(m, _)| m == "SetProjectSandbox").unwrap().1.clone();
    assert_eq!(sent["enabled"], true);
    assert_eq!(sent["config"], "/work/atlas/.devcontainer/devcontainer.json");
    assert_eq!(sent["hostPorts"], json!([]));
    assert_eq!(sent["publishPorts"], json!([]));
    assert_eq!(sent["shareGithub"], false);
}

const VM_FILES: &[&str] = &["fork_sdk.luau", "model.luau", "settings.luau", "view.luau"];

#[test]
fn the_vm_tab_offers_to_connect_a_project_that_has_none() {
    let (mut app, _) = mount("windows-vm", VM_FILES, "view", |method, _| match method {
        "GetProjectWindowsVm" => json!({ "available": true, "settings": null, "vm": null, "result": null }),
        _ => json!({}),
    });
    assert_eq!(app.error(), None);
    assert!(app.wait_until(|a| shows(a, "Connect in settings")), "{}", app.tree());
    assert!(shows(&mut app, "Not connected"));
}

#[test]
fn the_vm_tab_shows_a_running_vm_and_runs_a_command_in_it() {
    let (mut app, calls) = mount("windows-vm", VM_FILES, "view", |method, params| match method {
        "GetProjectWindowsVm" => json!({
            "available": true,
            "settings": { "root": "/work/atlas", "toVm": [3000], "fromVm": [8080], "viewer": true, "startWith": [], "autoPorts": true },
            "vm": { "state": "running", "toVm": [3000, 5173], "fromVm": [8080], "viewer": true },
            "result": null,
        }),
        "ControlProjectWindowsVm" => match params["action"].as_str().unwrap() {
            "screenshot" => json!({ "result": { "src": "host:plugin/windows-vm/windows-vm/d1/p1", "width": 1280, "height": 800 } }),
            "diagnostics" => json!({ "result": {
                "os": "Windows 11 Pro", "uptimeSeconds": 3600, "loggedOnUser": "ada", "cpuLoadPercent": 12,
                "memoryMB": { "total": 8192, "free": 4096 }, "diskGB": { "total": 64, "free": 32 }, "display": "1280x800",
                "topProcesses": [{ "name": "explorer", "pid": 4242, "memoryMB": 120 }], "listeningPorts": ["3389"],
                "stoppedAutomaticServices": [], "recentErrors": [],
            } }),
            "exec" => json!({ "result": { "exitCode": 0, "timedOut": false, "stdout": "ATLAS-VM\n", "stderr": "" } }),
            _ => json!({}),
        },
        _ => json!({}),
    });
    assert_eq!(app.error(), None);
    assert!(app.wait_until(|a| shows(a, "Windows 11 Pro")), "{}", app.tree());
    assert!(shows(&mut app, "Running"));
    assert!(shows(&mut app, "up 1 h 0 min"));
    assert!(shows(&mut app, "ada signed in"));
    assert!(shows(&mut app, "5173"), "the ports connected into Windows");
    assert!(shows(&mut app, "8080"), "the ports from Windows");
    assert!(shows(&mut app, "Health"));
    // Its screen and health were asked for as it read running.
    assert!(calls.borrow().iter().any(|(m, p)| m == "ControlProjectWindowsVm" && p["action"] == "screenshot"));
    app.fill("role:textbox", "hostname");
    app.click("role:button \"Run in PowerShell\"");
    assert!(app.wait_until(|a| shows(a, "ATLAS-VM")), "{}", app.tree());
    assert!(shows(&mut app, "Finished · exit code 0 · hostname"));
    let sent = calls.borrow().iter().find(|(m, p)| m == "ControlProjectWindowsVm" && p["action"] == "exec").unwrap().1.clone();
    assert_eq!(sent["command"], "hostname");
}
