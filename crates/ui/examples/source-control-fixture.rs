//! Drive the real Linux Shell against disposable Git repos. Commands in
//! <output>/command.json dispatch native GPUI input; screenshots capture the window.
use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowOptions, px, size};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use zeron_ui::*;

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn commit(root: &Path) {
    git(root, &["add", "-A"]);
    git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "fixture"],
    );
}
async fn pause(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

#[cfg(target_os = "linux")]
fn screenshot(path: &Path) -> anyhow::Result<()> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        let output = std::process::Command::new("niri")
            .args(["msg", "-j", "windows"])
            .output()?;
        let windows: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let window = windows
            .as_array()
            .and_then(|windows| {
                windows
                    .iter()
                    .find(|w| w["title"] == "Zeron Source Control Fixture")
            })
            .ok_or_else(|| anyhow::anyhow!("Fixture window not found"))?;
        let path = path
            .parent()
            .unwrap()
            .canonicalize()?
            .join(path.file_name().unwrap());
        let status = std::process::Command::new("niri")
            .args([
                "msg",
                "action",
                "screenshot-window",
                "--id",
                &window["id"].to_string(),
                "--path",
            ])
            .arg(path)
            .status()?;
        anyhow::ensure!(status.success(), "Window screenshot failed");
        return Ok(());
    }
    use x11rb::{
        connection::Connection,
        protocol::xproto::{ConnectionExt, ImageFormat},
    };
    let (conn, screen) = x11rb::connect(None)?;
    let screen = &conn.setup().roots[screen];
    let result = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            screen.root,
            0,
            0,
            screen.width_in_pixels,
            screen.height_in_pixels,
            u32::MAX,
        )?
        .reply()?;
    let mut pixels = result.data;
    anyhow::ensure!(
        pixels.len() == screen.width_in_pixels as usize * screen.height_in_pixels as usize * 4,
        "Expected 32-bit X11 pixels"
    );
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    image::RgbaImage::from_raw(
        screen.width_in_pixels as u32,
        screen.height_in_pixels as u32,
        pixels,
    )
    .unwrap()
    .save(path)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let output = PathBuf::from(std::env::args().nth(1).expect("output directory"));
    std::fs::create_dir_all(&output)?;
    let temp = tempfile::tempdir()?;
    let repo = temp.path().join("source-control-demo");
    let child = temp.path().join("module-origin");
    std::fs::create_dir_all(repo.join("src"))?;
    std::fs::create_dir_all(&child)?;
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&child, &["init", "-q"]);
    std::fs::write(
        child.join("module.rs"),
        "pub fn message() -> &'static str {\n    \"original\"\n}\n",
    )?;
    commit(&child);
    git(
        &repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            child.to_str().unwrap(),
            "apps/device-apps",
        ],
    );
    std::fs::write(
        repo.join("src/main.rs"),
        "fn main() {\n    println!(\"original\");\n}\n",
    )?;
    std::fs::write(repo.join("src/staged.rs"), "pub const VALUE: u32 = 1;\n")?;
    std::fs::write(repo.join(".gitignore"), "# Dependencies\nnode_modules\n\n# Build output\ntarget\n")?;
    let typescript = "type Participant = { id: string; active: boolean };\n\nexport function assignMembers(members: Participant[]) {\n    const message = \"original\";\n    const groupRouter = { assign: (member: Participant) => member.id };\n    return members.filter((member) => member.active).map(groupRouter.assign);\n}\n";
    std::fs::write(repo.join("src/example.ts"), typescript)?;
    commit(&repo);
    for (i, root) in [&repo, &repo.join("apps/device-apps")].into_iter().enumerate() {
        git(root, &["config", "user.name", "Fixture"]);
        git(root, &["config", "user.email", "fixture@example.com"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        git(
            root,
            &[
                "config",
                "core.hooksPath",
                temp.path()
                    .join(format!("fixture-hooks-{i}"))
                    .to_str()
                    .unwrap(),
            ],
        );
    }
    // A local bare origin gives the native harness real publish/push/pull and
    // ahead/behind behavior without contacting any hosted repository.
    let origin = temp.path().join("origin.git");
    std::fs::create_dir_all(&origin)?;
    git(&origin, &["init", "-q", "--bare", "-b", "main"]);
    git(&repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&repo, &["push", "-qu", "origin", "main"]);
    std::fs::write(repo.join("local-history.txt"), "local history\n")?;
    commit(&repo);
    std::fs::write(
        repo.join("src/example.ts"),
        typescript.replace("original", "updated participants"),
    )?;
    std::fs::write(
        repo.join("src/main.rs"),
        "fn main() {\n    println!(\"staged version\");\n}\n",
    )?;
    git(&repo, &["add", "src/main.rs"]);
    std::fs::write(
        repo.join("src/main.rs"),
        "fn main() {\n    println!(\"working version\");\n}\n",
    )?;
    std::fs::write(repo.join("src/staged.rs"), "pub const VALUE: u32 = 2;\n")?;
    git(&repo, &["add", "src/staged.rs"]);
    std::fs::write(repo.join("src/new.rs"), "pub fn added() {}\n")?;
    std::fs::write(
        repo.join("apps/device-apps/module.rs"),
        "pub fn message() -> &'static str {\n    \"submodule change\"\n}\n",
    )?;
    let runtime = tokio::runtime::Runtime::new()?;
    let core = runtime.block_on(async {
        zeron_engine::EngineCore::assemble(
            &temp.path().join("engine"),
            Arc::new(zeron_engine::default_registry()),
            zeron_proto::HarnessId::ClaudeCode,
            None,
        )
    })?;
    core.workspace.create_space(
        "fixture-space",
        &core.device_id,
        repo.to_str().unwrap(),
        None,
        true,
    )?;
    core.workspace.create_chat(
        "source-control-fixture",
        Some("fixture-space"),
        Some(&core.device_id),
        None,
        None,
    )?;
    core.workspace
        .set_chat_cwd("source-control-fixture", repo.to_str().unwrap())?;
    core.workspace
        .rename_chat("source-control-fixture", "Test source control")?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let _ipc = runtime.block_on(zeron_engine::serve_ipc(port, core.rpc_service()))?;
    let data = temp.path().join("ui");
    std::fs::create_dir(&data)?;
    let boot = EngineBootConfig {
        data_dir: data.clone(),
        ipc_port: port,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: zeron_proto::HarnessId::ClaudeCode,
    };
    let handle = runtime.block_on(state::EngineHandle::bootstrap(boot.clone()))?;
    let device = core.device_id.clone();
    let chats = core.workspace.read_chats()?;
    let spaces = core.workspace.read_spaces()?;
    std::fs::write(
        output.join("repository.txt"),
        repo.to_string_lossy().as_bytes(),
    )?;
    gpui_platform::application().with_assets(icons::Assets).run(move |cx| {
        gpui_tokio::init(cx); gpui_base::init(cx);
        gpui::profiler::set_frame_trace_enabled(true);
        let prefs = settings::UiSettings::default(); settings::init(prefs.clone(), data.clone(), cx);
        let fonts = typography::register_fonts(cx);
        typography::init(prefs.ui_font_family.clone(), prefs.ui_font_size, prefs.terminal_font_family.clone(), prefs.terminal_font_size, prefs.code_font_family.clone(), prefs.code_font_size, fonts, cx);
        theme_library::init(data, cx);
        appearance::init(appearance::AppearanceMode::Dark, prefs.theme_selection, prefs.accent, prefs.surface, cx);
        history::init(prefs.git_history_columns, prefs.git_history_column_widths, prefs.git_history_column_order, prefs.git_history_author_display, cx);
        composer::init(cx, prefs.composer_send_behavior); terminal::panel::init(cx); app_menus::init(cx);
        let state = cx.new(|_| {
            let mut s = state::AppState::new(); s.fixture_attachment_engine(handle);
            s.connection = zeron_proto::view::ConnectionStatus::Ready; s.workspace_scope = Some(zeron_proto::WorkspaceScope::Local);
            s.local_device_id = Some(device.clone()); s.chats = chats; s.spaces = spaces;
            s.selected_space = Some("fixture-space".into()); s.selected_chat = Some("source-control-fixture".into()); s.auto_selected = true; s.chats_synced = true; s.spaces_synced = true; s
        });
        let window = cx.open_window(WindowOptions { titlebar: Some(gpui::TitlebarOptions { title: Some("Zeron Source Control Fixture".into()), ..Default::default() }), window_bounds: Some(WindowBounds::Windowed(Bounds::new(gpui::point(px(0.0), px(0.0)), size(px(1440.0), px(900.0))))), ..Default::default() }, |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx))).unwrap();
        window.update(cx, |_, window, _| window.set_window_title("Zeron Source Control Fixture")).unwrap();
        state.update(cx, |_, cx| cx.notify());
        cx.activate(true);
        cx.spawn(async move |cx| {
            pause(cx, 1500).await;
            window.update(cx, |view, window, cx| view.fixture_open_source_control(window, cx)).unwrap();
            pause(cx, 2500).await;
            screenshot(&output.join("initial.png")).unwrap();
            std::fs::write(output.join("ready"), "ready").unwrap();
            let mut frame_timings = gpui::profiler::FrameTimingCollector::new();
            loop {
                pause(cx, 100).await;
                let path = output.join("command.json");
                let Ok(bytes) = std::fs::read(&path) else { continue; };
                let command: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                std::fs::remove_file(path).unwrap();
                let action = command["action"].as_str().unwrap();
                let command_start = std::time::Instant::now();
                if action == "quit" {
                    gpui::AnyWindowHandle::from(window).update(cx, |_, w, _| w.blur()).unwrap();
                    pause(cx, 100).await;
                    cx.update(|cx| cx.quit());
                    break;
                }
                gpui::AnyWindowHandle::from(window).update(cx, |_, w, cx| {
                    if command["forceDraw"].as_bool().unwrap_or(true) {
                        w.draw(cx).clear();
                    }
                    match action {
                        "mouse-down" | "mouse-up" | "mouse-move" => {
                            let position = gpui::point(px(command["x"].as_f64().unwrap() as f32), px(command["y"].as_f64().unwrap() as f32));
                            match action {
                                "mouse-down" => w.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent { position, button: gpui::MouseButton::Left, click_count: 1, ..Default::default() }), cx),
                                "mouse-up" => w.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent { position, button: gpui::MouseButton::Left, click_count: 1, ..Default::default() }), cx),
                                _ => w.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent { position, ..Default::default() }), cx),
                            };
                        }
                        "click" => {
                            let position = gpui::point(px(command["x"].as_f64().unwrap() as f32), px(command["y"].as_f64().unwrap() as f32));
                            let modifiers = gpui::Modifiers { shift: command["shift"].as_bool().unwrap_or(false), control: command["control"].as_bool().unwrap_or(false), ..Default::default() };
                            w.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent { position, modifiers, ..Default::default() }), cx);
                            w.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent { position, modifiers, button: gpui::MouseButton::Left, click_count: 1, ..Default::default() }), cx);
                            w.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent { position, modifiers, button: gpui::MouseButton::Left, click_count: 1, ..Default::default() }), cx);
                        }
                        "scroll" => {
                            let position = gpui::point(px(command["x"].as_f64().unwrap() as f32), px(command["y"].as_f64().unwrap() as f32));
                            w.dispatch_event(gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent { position, delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(command["deltaY"].as_f64().unwrap() as f32))), ..Default::default() }), cx);
                        }
                        "light" => appearance::set_mode(appearance::AppearanceMode::Light, cx),
                        "dark" => appearance::set_mode(appearance::AppearanceMode::Dark, cx),
                        "key" => {
                            for key in command["keys"].as_array().unwrap() {
                                w.dispatch_keystroke(gpui::Keystroke::parse(key.as_str().unwrap()).unwrap(), cx);
                                if command["forceDraw"].as_bool().unwrap_or(true) {
                                    w.draw(cx).clear();
                                }
                            }
                        }
                        "drag" => {
                            let start = gpui::point(px(command["x"].as_f64().unwrap() as f32), px(command["y"].as_f64().unwrap() as f32));
                            let end = gpui::point(px(command["toX"].as_f64().unwrap() as f32), px(command["toY"].as_f64().unwrap() as f32));
                            w.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent { position: start, button: gpui::MouseButton::Left, click_count: 1, ..Default::default() }), cx);
                            w.draw(cx).clear();
                            w.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent { position: end, pressed_button: Some(gpui::MouseButton::Left), ..Default::default() }), cx);
                            w.draw(cx).clear();
                            // The first move starts the drag; the next move reaches its handler.
                            w.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent { position: end, pressed_button: Some(gpui::MouseButton::Left), ..Default::default() }), cx);
                            w.draw(cx).clear();
                            w.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent { position: end, button: gpui::MouseButton::Left, click_count: 1, ..Default::default() }), cx);
                        }
                        "resize" => w.resize(size(px(command["width"].as_f64().unwrap() as f32), px(900.0))),
                        "capture" => (),
                        _ => panic!("Unknown fixture command"),
                    }
                }).unwrap();
                pause(cx, command["waitMs"].as_u64().unwrap_or(2400)).await;
                let name = command["name"].as_str().unwrap_or("latest");
                if command["screenshot"].as_bool().unwrap_or(true) {
                    screenshot(&output.join(format!("{name}.png"))).unwrap();
                }
                let workbench = window.update(cx, |s, _, cx| s.fixture_workbench_state(cx)).unwrap();
                let frame_ms: Vec<_> = frame_timings.collect_unseen().iter().map(|frame| frame.draw_duration().as_secs_f64() * 1000.0).collect();
                std::fs::write(output.join(format!("{name}.json")), serde_json::to_vec(&serde_json::json!({ "workbench": workbench, "performance": { "commandMs": command_start.elapsed().as_secs_f64() * 1000.0, "drawMs": frame_ms }, "status": git(&repo, &["status", "--porcelain=v1"]), "head": git(&repo, &["rev-parse", "HEAD"]), "headMessage": git(&repo, &["log", "-1", "--format=%B"]), "committedMain": git(&repo, &["show", "HEAD:src/main.rs"]), "typescript": std::fs::read_to_string(repo.join("src/example.ts")).unwrap(), "submoduleStatus": git(&repo.join("apps/device-apps"), &["status", "--porcelain=v1"]), "submoduleHead": git(&repo.join("apps/device-apps"), &["rev-parse", "HEAD"]), "submoduleMessage": git(&repo.join("apps/device-apps"), &["log", "-1", "--format=%B"]) })).unwrap()).unwrap();
            }
        }).detach();
    });
    Ok(())
}
