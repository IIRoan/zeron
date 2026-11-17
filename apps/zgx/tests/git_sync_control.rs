//! The Git header's sync affordance uses the selected repository's state.
//! Mock actions verify dispatch without pulling or pushing a real branch.
use std::path::PathBuf;
use zgx_script::{ScriptHost, testing::TestApp};

fn mount() -> (tempfile::TempDir, TestApp) {
    let root = tempfile::tempdir().unwrap();
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ui = project.join("ui");
    std::fs::copy(project.join("plugins-fork/zeron-git/sync_counts.luau"), root.path().join("sync_counts.luau")).unwrap();
    let themes = root.path().join("ds/themes");
    std::fs::create_dir_all(&themes).unwrap();
    for theme in std::fs::read_dir(ui.join("ds/themes")).unwrap().flatten() {
        std::fs::copy(theme.path(), themes.join(theme.file_name())).unwrap();
    }
    let entry = root.path().join("app.luau");
    std::fs::write(&entry, r#"
require("@ds").setup()
local Sync = require("./sync_counts")
local repo=ui.state("test.repo",{complete=true,git={head="abc",branch="main",upstream="origin/main",ahead=3,behind=2}})
local details=ui.state("test.details",{state={head="abc",branch="main"},publishRemote="origin",remotes={{name="origin"}}})
local busy=ui.state("test.busy",false)
local action=ui.state("test.action","none")
local w={repo=function() return repo:get() end,details=details,busy=busy,
    verified=ui.state("test.verified",true),conflict_open=ui.state("test.conflicts",false),
    action=function(a) action:set(a.kind..":"..(a.remote or "")) end}
local m={workbench=w,acting=ui.state("test.acting",nil)}
for name,git in {
    unpublished={head="abc",branch="main"},
    current={head="abc",branch="main",upstream="origin/main",ahead=0,behind=0},
    detached={head="abc"}, empty={branch="main"},
    unknown={head="abc",branch="main",upstream="origin/main"},
} do
    ui.command {id=name,title=name,run=function() repo:set({complete=true,git=git});action:set("none") end}
end
ui.command {id="loading",title="Loading",run=function() repo:set(nil);action:set("none") end}
ui.command {id="busy",title="Busy",run=function() busy:set(true);action:set("none") end}
ui.command {id="stale",title="Stale",run=function() details:set({state={head="old",branch="other"},publishRemote="wrong"}) end}
ui.command {id="no-auto",title="No default remote",run=function() details:set({state={head="abc",branch="main"},remotes={{name="origin"}}}) end}
return ui.column {Sync(m),ui.text {id="last-action",function() return action:get() end}}
"#).unwrap();
    let mut app = TestApp::new(move |runtime| ScriptHost::new(runtime, entry.clone())
        .package("ds", ui.join("ds")).i18n(ui.join("locales"), "en"));
    app.ui.scene.borrow_mut().resize(400., 180.);
    app.frames(30);
    assert_eq!(app.error(), None);
    (root, app)
}

#[test]
fn tracked_branches_show_number_first_counts_and_dispatch_sync() {
    let (_dir, mut app) = mount();
    assert_eq!(app.text("#git-incoming-count >> role:text"), "2");
    assert_eq!(app.text("#git-outgoing-count >> role:text"), "3");
    assert!(app.tree().contains("Sync Changes (2 incoming, 3 outgoing)"));
    app.click("#git-sync");app.frames(5);
    assert_eq!(app.text("last-action"), "sync:");
}

#[test]
fn unpublished_branches_offer_publish_and_reject_stale_remote_metadata() {
    let (_dir, mut app) = mount();
    app.host.run_command("unpublished");app.frames(30);
    assert!(app.tree().contains("Publish Branch"));
    app.click("#git-sync");app.frames(5);
    assert_eq!(app.text("last-action"), "publish:origin");
    app.host.run_command("stale");app.frames(5);
    assert!(app.tree().contains("disabled"));
    app.host.run_command("no-auto");app.frames(5);
    assert!(app.tree().contains("disabled"), "honor the engine's decision not to select a publish remote");
}

#[test]
fn busy_and_unavailable_states_never_offer_a_git_mutation() {
    let (_dir, mut app) = mount();
    for command in ["current", "detached", "empty", "unknown", "loading", "unpublished", "busy"] {
        app.host.run_command(command);app.frames(30);
        assert_eq!(app.error(), None, "transition {command}");
        if command != "unpublished" {
            assert!(app.tree().contains("disabled"), "{command}: {}", app.tree());
        }
        assert_eq!(app.text("last-action"), "none");
    }
}
