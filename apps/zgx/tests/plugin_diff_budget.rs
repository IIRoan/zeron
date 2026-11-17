//! Regression for the installed Source Control plugin's 50 ms callback
//! budget. Keep large native results lazy and fallback work cancellable.
use std::{path::PathBuf, time::Duration};
use zgx_script::{Limits, ScriptHost, testing::TestApp};

fn mount(script: &str, shared: bool) -> (tempfile::TempDir, TestApp) {
    let root = tempfile::tempdir().unwrap();
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ui = project.join("ui");
    let source = if shared { ui.join("ds/git.luau") } else { project.join("plugins-fork/zeron-git/ui/git.luau") };
    let source = std::env::var_os("ZERON_BUDGET_TEST_GIT").filter(|_| !shared).map(PathBuf::from).unwrap_or(source);
    let text = std::fs::read_to_string(source).unwrap();
    // Shared modules normally resolve @self inside the ds package.
    std::fs::write(root.path().join("git.luau"), if shared { text.replace("@self/", "@ds/") } else { text }).unwrap();
    let themes = root.path().join("ds/themes");
    std::fs::create_dir_all(&themes).unwrap();
    for theme in std::fs::read_dir(ui.join("ds/themes")).unwrap().flatten() {
        std::fs::copy(theme.path(), themes.join(theme.file_name())).unwrap();
    }
    std::fs::copy(project.join("plugins-fork/project-sandbox/model.luau"), root.path().join("sandbox.luau")).unwrap();
    for name in ["git_changes.luau", "context_menu.luau"] {
        std::fs::copy(project.join("plugins-fork/zeron-git/ui").join(name), root.path().join(name)).unwrap();
    }
    let entry = root.path().join("app.luau");
    std::fs::write(&entry, format!("require(\"@ds\").setup()\nlocal git=require(\"./git\")\nlocal test_view=nil\n{script}\nreturn test_view or ui.text {{'ready'}}")).unwrap();
    let mut app = TestApp::new(move |runtime| ScriptHost::new(runtime, &entry)
        .package("ds", ui.join("ds"))
        .i18n(ui.join("locales"), "en")
        .limits(Limits { call: Duration::from_millis(50), memory: 32 * 1024 * 1024, ..Limits::default() }));
    app.frames(5);
    assert_eq!(app.error(), None);
    (root, app)
}

#[test]
fn native_diff_paint_is_lazy_preserves_utf8_tabs_and_cancels_fallback() {
    let (_root, mut app) = mount(r#"
local lines, data = {}, {new={},added={}}
local text=string.rep("const value = 'café';\t",10)
for i=1,10000 do
    lines[i]={kind="+",new=i,text=text}
    data.new[tostring(i)]={{1,#text,"$zeron.syntax.type"}}
end
ui.command {id="paint",title="Paint",run=function()
    git.highlight(lines,"a.ts") -- queued fallback must yield to native data
    git.paint(lines,data)
    local colors=git.highlight(lines,"a.ts")
    assert(next(colors)==nil,"native results must not paint invisible rows")
    assert(colors[10000][1].color=="$zeron.syntax.type")
    local count=0;for _ in colors do count+=1 end
    assert(count==1,"only the requested line is colored")
end}
ui.command {id="check",title="Check",run=function()
    assert(git.highlight(lines,"a.ts")[10000][1].color=="$zeron.syntax.type","late fallback replaced native syntax")
    local unicode={{kind="+",new=1,text="é\tz"}}
    git.paint(unicode,{new={["1"]={{1,2,"$zeron.syntax.string"},{4,4,"$zeron.syntax.keyword"}}},added={["1"]={{3,3}}}})
    local c=git.highlight(unicode,"a.ts")[1]
    assert(c[1].text=="é" and c[2].text=="   " and c[3].text=="z")
    assert(c[2].bg=="$zeron.tone.success.edge")
    local huge={{kind="+",new=1,text=string.rep("x",20000)}}
    git.paint(huge,{new={}})
    assert(git.highlight(huge,"a.ts")[1]==nil,"pathological lines fall back to unchanged plain text")
end}
"#, false);
    assert!(app.host.run_command("paint"));
    app.frames(20);
    assert_eq!(app.error(), None);
    assert!(app.host.run_command("check"));
    app.frames(5);
    assert_eq!(app.error(), None);
}

#[test]
fn large_fallback_diffs_complete_under_the_plugin_call_budget() {
    for shared in [false, true] {
        let (_root, mut app) = mount(r#"
local lines={}
for i=1,2000 do lines[i]={kind="+",new=i,text="const value = 'hello';"} end
local out
ui.command {id="start",title="Start",run=function() out=git.highlight(lines,"a.ts") end}
ui.command {id="check",title="Check",run=function() assert(out[2000]~=nil,"fallback never completed") end}
"#, shared);
        assert!(app.host.run_command("start"));
        assert!(app.wait_until(|app| {
            app.host.states().iter().any(|(k,v)| k.ends_with("paint_version") && v.as_f64().is_some_and(|n| n > 0.))
        }));
        assert!(app.host.run_command("check"));
        app.frames(5);
        assert_eq!(app.error(), None, "shared {shared}");
    }
}

#[test]
fn sandbox_log_tails_keep_blank_lines_and_unicode_without_splitting_the_log() {
    let (_root, mut app) = mount(r#"
local Model=require("./sandbox")({})
local log=string.rep("old build output\n",100000).."é\n\nlast\n"
ui.command {id="tail",title="Tail",run=function()
    assert(Model.tail(log,3)=="é\n\nlast")
    assert(Model.tail("a\nb\nc\n",2)=="b\nc")
    assert(Model.tail("\n\n",2)=="\n")
    assert(Model.tail("",600)=="" and Model.tail(log,0)=="")
end}
"#, false);
    assert!(app.host.run_command("tail"));
    app.frames(5);
    assert_eq!(app.error(), None);
}

#[test]
fn native_diff_paint_alone_cannot_exhaust_a_callback_on_a_large_document() {
    let (_root, mut app) = mount(r#"
local lines,data={}, {new={},added={}}
local text=string.rep("const value = 'hello';",20)
for i=1,10000 do lines[i]={kind="+",new=i,text=text};data.new[tostring(i)]={{1,#text,"$zeron.syntax.string"}} end
ui.command {id="paint-only",title="Paint",run=function() git.paint(lines,data) end}
"#, false);
    assert!(app.host.run_command("paint-only"));
    app.frames(5);
    assert_eq!(app.error(), None);
}

#[test]
fn a_large_native_diff_opens_and_scrolls_in_the_virtualized_view() {
    let (_root, mut app) = mount(r#"
local Changes=require("./git_changes")
local lines,data={}, {new={}}
for i=1,10000 do
    lines[i]={kind="+",new=i,text="const value = 'hello';"}
    data.new[tostring(i)]={{1,5,"$zeron.syntax.keyword"},{15,21,"$zeron.syntax.string"}}
end
local shown=ui.state("stress.shown",false)
local View=ui.component(function(): Node
    return ui.presence(function() return shown:get() end,function(on: boolean): Node
        return if on then Changes.list {id="stress-diff",bare=true,files={Changes.file("a.ts","added",10000,0)},lines=function() return lines end} else ui.div {}
    end)
end)
ui.command {id="open",title="Open",run=function() git.paint(lines,data);shown:set(true) end}
test_view=ui.div {w=520,h=600,View{}}
"#, false);
    assert!(app.host.run_command("open"));
    app.frames(20);
    assert_eq!(app.error(), None);
    assert!(app.find("stress-diff").is_some());
    let list=app.target("stress-diff").unwrap();
    zgx_script::driver::wheel(&mut app.ui,list,0.,5000.,None);
    app.frames(10);
    assert_eq!(app.error(), None);
}
