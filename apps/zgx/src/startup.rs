//! The Linux beta's first frame, before its layout plugins are ready.
//! Keep the shell behind a native loading view and evaluate it once, with
//! the engine's approved plugins. After four seconds without them, it
//! opens the default interface.
use std::{cell::{Cell, RefCell}, rc::Rc, sync::Arc, time::{Duration, Instant}};

use zgx::{compose::{View, column, svg, switch, text}, reactive::Signal, scene::Color,
    semantics::{Live, Role}, style::Styled, svg::SvgData, text_layout::FontFamily};
use zgx_desktop::WindowContext;
use zgx_script::ScriptHost;

const PATIENCE: Duration = Duration::from_secs(4);
const WATCH: Duration = Duration::from_millis(250);

pub(crate) struct Startup {
    host: ScriptHost,
    ready: Signal<bool>,
    attempted: Cell<bool>,
    start_watcher: RefCell<Option<Box<dyn FnOnce()>>>,
    since: Instant,
}

impl Startup {
    pub(crate) fn new(cx: &WindowContext, host: &ScriptHost) -> Rc<Self> {
        let (watching, tasks) = (host.clone(), cx.tasks.clone());
        let startup = Rc::new(Self { host: host.clone(), ready: cx.ui.runtime.signal(false),
            attempted: Cell::new(false), since: Instant::now(),
            start_watcher: RefCell::new(Some(Box::new(move || tasks.spawn(watching.watch(WATCH))))) });
        let weak = Rc::downgrade(&startup);
        cx.tasks.spawn(async move {
            zgx::timer::sleep(PATIENCE).await;
            if let Some(startup) = weak.upgrade() { startup.finish("not waiting longer"); }
        });
        startup
    }

    pub(crate) fn pending(&self) -> bool { !self.ready.with_untracked(|ready| *ready) }
    pub(crate) fn attempted(&self) -> bool { self.attempted.get() }
    pub(crate) fn loading(&self) { self.attempted.set(true); }

    pub(crate) fn finish(&self, why: &str) {
        if !self.pending() { return; }
        // No plugin list (timeout, disconnected engine, or setup unfinished):
        // load the default interface once so its recovery controls are usable.
        if !self.attempted.replace(true) {
            if let Err(error) = self.host.reload() {
                eprintln!("zeron-zgx: startup fallback: {error}");
            }
        }
        self.ready.set(true);
        // Starting a file watcher before the first load would itself trigger
        // the stock interface's evaluation while plugins were still arriving.
        if let Some(start) = self.start_watcher.borrow_mut().take() { start(); }
        eprintln!("zeron-zgx: startup: ready after {} ms, {why}", self.since.elapsed().as_millis());
    }

    pub(crate) fn view(&self, content: View, runtime: &zgx::reactive::Runtime) -> View {
        let ready = self.ready.clone();
        let themes = zgx::theme::Themes::of(runtime);
        let mut content = Some(content);
        switch(move || ready.get(), move |ready, _| {
            if ready { content.take().expect("startup only finishes once") }
            else {
                let themes = themes.clone();
                switch(move || { themes.current(); themes.kind().is_dark() },
                    |dark, _| splash(dark))
            }
        }).w_full().h_full()
    }
}

fn splash(dark: bool) -> View {
    let (bg, ink, muted) = if dark {
        (Color(24, 24, 24, 255), Color(237, 237, 239, 255), Color(157, 157, 160, 255))
    } else {
        (Color(250, 250, 250, 255), Color(27, 27, 29, 255), Color(110, 110, 114, 255))
    };
    let mark = SvgData::new(&include_bytes!("../../../crates/ui/assets/icons/solace-logo.svg")[..])
        .expect("Solace's logo").tinted(ink);
    column().id("startup-splash").w_full().h_full().bg(bg)
        .items_center().justify_center().gap(16.)
        .role(Role::Status).live(Live::Polite).accessible_label("Opening Zeron Beta")
        .child(svg("Solace", Arc::new(mark)).size(48., 48.))
        .child(column().items_center().gap(6.)
            .child(text("Zeron Beta").font_family(FontFamily::from("Geist"))
                .text_size(20.).line_height(28.).font_weight(500).text_color(ink))
            .child(text("Opening your workspace…").font_family(FontFamily::from("Geist"))
                .text_size(13.).line_height(20.).text_color(muted)))
}

/// Compile source text only; execution and grants still come exclusively
/// from the engine's verified snapshots. The compiler compares exact bytes,
/// so edits or a disabled plugin cannot activate cached code.
pub(crate) fn precompile(ui: &std::path::Path) {
    let Some(resources) = ui.parent() else { return };
    let plugins = resources.join("plugins-builtin");
    let ds = ui.join("ds");
    for relative in ["linux-workbench/screens/shell.luau", "linux-workbench/screens/page.luau",
        "linux-workbench/screens/sidebar.luau", "zeron-git/init.luau", "project-services/init.luau",
        "project-sandbox/init.luau", "windows-vm/init.luau"] {
        let entry = plugins.join(relative);
        if entry.is_file() { zgx_script::compile_ahead(&entry, &[("ds", &ds), ("app", ui)]); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zgx_script::{Value, testing::TestApp};

    fn mount() -> (tempfile::TempDir, TestApp, Rc<Startup>, Rc<Cell<usize>>) {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("app.luau");
        std::fs::write(&entry, r#"
local runs=ui.shared("test.runs",0)
runs:set(runs:peek()+1)
if ui.shared("test.fail",false):peek() then error("startup fixture failed") end
return ui.text {id="workspace", "Ready workspace"}
"#).unwrap();
        let made: Rc<RefCell<Option<(Rc<Startup>, zgx::reactive::Runtime)>>> = Rc::default();
        let kept = made.clone();
        let showing = made.clone();
        let watchers = Rc::new(Cell::new(0));
        let watched = watchers.clone();
        let mut app = TestApp::wrapped(move |runtime| {
            let host = ScriptHost::new(runtime, entry);
            let startup = Rc::new(Startup { host: host.clone(), ready: runtime.signal(false),
                attempted: Cell::new(false), since: Instant::now(),
                start_watcher: RefCell::new(Some(Box::new(move || watched.set(watched.get()+1)))) });
            *kept.borrow_mut() = Some((startup, runtime.clone()));
            host
        }, move |scripts| {
            let held = showing.borrow();
            let (startup, runtime) = held.as_ref().unwrap();
            startup.view(scripts, runtime)
        });
        app.frames(5);
        app.host.shared("test.runs", 0.).set(Value::from(0.));
        // The fixture's initial evaluation sets up TestApp; production
        // defers that evaluation in serve_with_startup.
        let startup = made.borrow().as_ref().unwrap().0.clone();
        (dir, app, startup, watchers)
    }

    #[test]
    fn splash_hides_the_stock_shell_until_the_plugin_layout_has_loaded() {
        let (_dir, mut app, startup, watchers) = mount();
        assert!(app.find("#startup-splash").is_some());
        assert!(app.find("#workspace").is_none());
        assert!(app.tree().contains("Opening Zeron Beta"));
        startup.loading();
        app.host.reload().unwrap();
        startup.finish("plugins in");
        app.frames(5);
        assert!(app.find("#startup-splash").is_none());
        assert!(app.find("#workspace").is_some());
        assert_eq!(app.host.shared("test.runs", 0.).signal().with_untracked(Clone::clone), Value::from(1.));
        assert_eq!(watchers.get(), 1);
        startup.finish("plugins in again");
        assert_eq!(watchers.get(), 1);
        assert_eq!(app.host.shared("test.runs", 0.).signal().with_untracked(Clone::clone), Value::from(1.));
    }

    #[test]
    fn missing_engine_and_unfinished_setup_load_a_usable_fallback_once() {
        for reason in ["not waiting longer", "setup isn't finished", "Zeron's own plugins didn't load"] {
            let (_dir, mut app, startup, watchers) = mount();
            startup.finish(reason);
            app.frames(5);
            assert!(!startup.pending());
            assert!(app.find("#workspace").is_some());
            assert_eq!(app.host.shared("test.runs", 0.).signal().with_untracked(Clone::clone), Value::from(1.));
            assert_eq!(watchers.get(), 1);
        }
    }

    #[test]
    fn a_failed_fallback_releases_the_splash_and_keeps_the_script_error() {
        let (_dir, mut app, startup, watchers) = mount();
        app.host.shared("test.fail", false).set(Value::Bool(true));
        startup.finish("not waiting longer");
        app.frames(5);
        assert!(!startup.pending());
        assert!(app.find("#startup-splash").is_none());
        assert!(app.error().unwrap().contains("startup fixture failed"));
        assert_eq!(watchers.get(), 1);
    }

    #[test]
    fn readiness_is_scoped_to_each_main_window() {
        let (_one, mut first, ready, _) = mount();
        let (_two, mut second, waiting, _) = mount();
        ready.finish("plugins in");
        first.frames(5);
        second.frames(5);
        assert!(!ready.pending());
        assert!(waiting.pending());
        assert!(first.find("#workspace").is_some());
        assert!(second.find("#startup-splash").is_some());
    }
}
