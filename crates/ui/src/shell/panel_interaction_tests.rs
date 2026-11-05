//! The panel lifecycle through Shell's actions and actual GPUI layout.
use super::*;
use gpui::{AppContext, Bounds, TestAppContext, VisualTestContext};

fn setup(cx: &mut TestAppContext) -> (tempfile::TempDir, Entity<Shell>, &mut VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        let mut config = UiSettings::default();
        config.sidebar_width = 300.0;
        config.files_panel_width = 377.0;
        // The session card shifts the composer while it fits; these panels
        // measure the composer centered in its column.
        config.session_info_hidden = true;
        settings::init(config, dir.path(), cx);
    });
    let (shell, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.no_project = true;
            state.selected_chat = Some("a".into());
            state.chats = ["a", "b"]
                .into_iter()
                .map(|id| {
                    serde_json::from_value(serde_json::json!({
                        "id": id, "title": id, "deviceId": "local", "archived": false,
                        "createdAt": Utc::now(),
                    }))
                    .unwrap()
                })
                .collect();
            state
        });
        let mut shell = Shell::new(
            state,
            EngineBootConfig {
                data_dir: dir.path().into(),
                ipc_port: 0,
                edge_url: String::new(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: zeron_proto::HarnessId::Mock,
            },
            cx,
        );
        shell.debug_gate = Some(GatePhase::Ready);
        shell.splash = SplashPhase::Gone;
        shell.active_chat = "a".into();
        shell.sidebar_opened_at = 1;
        shell.panel_open_sequence = 1;
        shell.test_time = Some(std::time::Instant::now());
        shell
    });
    cx.simulate_resize(window(1600.0));
    (dir, shell, cx)
}

fn change(
    shell: &Entity<Shell>,
    cx: &mut VisualTestContext,
    action: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>),
) {
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            action(shell, window, cx);
            cx.notify();
        })
    });
}

enum Panel {
    Sidebar,
    Files,
    Right,
}

impl Panel {
    fn toggle(self, shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
        match self {
            Self::Sidebar => shell.toggle_sidebar(cx),
            Self::Files => shell.toggle_files_panel(window, cx),
            Self::Right => shell.toggle_right_pane(cx),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Frame {
    // Sidebar, Files, surface host, visible conversation.
    widths: [f32; 4],
    composer: Bounds<Pixels>,
    strip: Option<Bounds<Pixels>>,
}

fn draw(shell: &Entity<Shell>, cx: &mut VisualTestContext, millis: u64) -> Frame {
    shell.update(cx, |shell, cx| {
        *shell.test_time.as_mut().unwrap() +=
            Duration::from_millis(millis).mul_f32(motion::speed_scale());
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear());
    let mut width = |id| cx.debug_bounds(id).map_or(0.0, |b| f32::from(b.size.width));
    Frame {
        widths: [
            width("sidebar-column"),
            width("files-panel"),
            width("right-panel"),
            width("conversation-column"),
        ],
        composer: shell.read_with(cx, |s, cx| {
            s.composer.read(cx).surface_bounds().get().unwrap()
        }),
        strip: cx.debug_bounds("right-titlebar-controls"),
    }
}

#[track_caller]
fn near(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= 0.5,
        "expected {expected}px, got {actual}px"
    );
}

/// A window whose columns measure `width`: Linux adds its activity bar.
fn window(width: f32) -> gpui::Size<Pixels> {
    gpui::size(px(width + ACTIVITY_WIDTH), px(900.0))
}

/// The Linux workbench mirrors upstream's columns: the activity bar, Files
/// and the surface host lead, the conversation follows, the sidebar trails.
fn conversation_left(frame: &Frame) -> f32 {
    if cfg!(target_os = "linux") {
        ACTIVITY_WIDTH + frame.widths[1] + frame.widths[2]
    } else {
        frame.widths[0]
    }
}

/// The surface host's leading edge, where its titlebar strip starts.
fn surface_left(frame: &Frame) -> f32 {
    if cfg!(target_os = "linux") {
        ACTIVITY_WIDTH + frame.widths[1]
    } else {
        frame.widths[0] + frame.widths[3]
    }
}

/// Upstream's trailing titlebar strip follows the surface host beneath it.
/// The Linux workbench titlebar lays the surface tabs out itself.
#[track_caller]
fn strip_follows_surface(frame: &Frame) {
    if !cfg!(target_os = "linux") {
        near(f32::from(frame.strip.unwrap().left()), surface_left(frame));
    }
}

/// The seam between the conversation and the surface host, and the pointer
/// direction that widens the surface.
fn surface_seam(frame: &Frame) -> (f32, f32) {
    if cfg!(target_os = "linux") {
        (conversation_left(frame), 1.0)
    } else {
        (surface_left(frame), -1.0)
    }
}

/// The new-session canvas: upstream keeps the sidebar beside it; the Linux
/// canvas shows the conversation alone across the whole window.
fn canvas() -> [f32; 4] {
    if cfg!(target_os = "linux") {
        [0.0, 0.0, 0.0, 1600.0 + ACTIVITY_WIDTH]
    } else {
        [300.0, 0.0, 0.0, 1300.0]
    }
}

/// A point `inset` px into the window from the sidebar's outer edge.
fn from_sidebar_edge(inset: f32) -> Point<Pixels> {
    let x = if cfg!(target_os = "linux") {
        1600.0 + ACTIVITY_WIDTH - inset
    } else {
        inset
    };
    gpui::point(px(x), px(400.0))
}

#[track_caller]
fn expect(frame: Frame, widths: [f32; 4]) {
    for (actual, expected) in frame.widths.into_iter().zip(widths) {
        near(actual, expected);
    }
}

#[track_caller]
fn continuous(before: Frame, after: Frame) {
    expect(after, before.widths);
    near(
        f32::from(after.composer.left()),
        f32::from(before.composer.left()),
    );
    near(
        f32::from(after.composer.size.width),
        f32::from(before.composer.size.width),
    );
    if let (Some(before), Some(after)) = (before.strip, after.strip) {
        near(f32::from(after.left()), f32::from(before.left()));
    }
}

fn settled(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Frame {
    // A closing column may make room for a hidden neighbour to return.
    draw(shell, cx, 600);
    draw(shell, cx, 600)
}

fn select(shell: &mut Shell, chat: Option<&str>, cx: &mut Context<Shell>) {
    let state = shell.state.clone();
    state.update(cx, |s, cx| s.select_chat(chat.map(str::to_owned), cx));
    shell.on_state_changed(&state, cx);
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn workbench_never_shows_the_session_card_or_reserves_its_space(cx: &mut TestAppContext) {
    let (dir, shell, cx) = setup(cx);
    for width in [1600.0, 1200.0, 900.0, 700.0, 1600.0] {
        cx.simulate_resize(window(width));
        change(&shell, cx, |shell, _, cx| {
            shell.settings.session_info_hidden = true;
            shell.session_info_overlay = false;
            shell.schedule_save(cx);
        });
        let baseline = settled(&shell, cx);

        // A preference written by an earlier release and an open overlay
        // must both leave the workbench's conversation unobstructed.
        change(&shell, cx, |shell, _, cx| {
            shell.settings.session_info_hidden = false;
            shell.session_info_overlay = true;
            shell.schedule_save(cx);
        });
        cx.update(|_, cx| settings::flush(cx));
        assert!(!UiSettings::load(dir.path()).session_info_hidden);
        let frame = settled(&shell, cx);
        assert!(cx.debug_bounds("session-info").is_none(), "width={width}");
        expect(frame, baseline.widths);
        near(
            f32::from(frame.composer.size.width),
            f32::from(baseline.composer.size.width),
        );
        near(
            f32::from(frame.composer.left()),
            conversation_left(&frame)
                + (frame.widths[3] - f32::from(frame.composer.size.width)) / 2.0,
        );
        shell.update(cx, |shell, cx| {
            let (_, reveal, transcript_reserve) = shell.session_info_frame(true, cx);
            assert_eq!(reveal, 0.0);
            assert_eq!(transcript_reserve, 0.0);
        });
    }
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn workbench_width_survives_restart_toggles_and_window_resize(cx: &mut TestAppContext) {
    let (dir, shell, cx) = setup(cx);
    change(&shell, cx, |s, window, cx| s.open_explorer(window, cx));
    let open = settled(&shell, cx);
    let (seam_x, widen) = surface_seam(&open);
    let seam = gpui::point(px(seam_x), px(400.0));
    let dragged = gpui::point(px(seam_x + (430.0 - open.widths[2]) * widen), px(400.0));
    cx.simulate_mouse_down(seam, MouseButton::Left, Default::default());
    for _ in 0..2 {
        cx.simulate_mouse_move(dragged, Some(MouseButton::Left), Default::default());
    }
    cx.simulate_mouse_up(dragged, MouseButton::Left, Default::default());
    near(settled(&shell, cx).widths[2], 430.0);
    cx.update(|_, cx| settings::flush(cx));
    let loaded = UiSettings::load(dir.path());
    near(loaded.right_pane_width, 430.0);

    // Rebuild the shell from disk, as a fresh process does, with the same chat.
    let state = shell.read_with(cx, |s, _| s.state.clone());
    drop(shell);
    let shell = cx.update(|window, cx| {
        settings::init(loaded, dir.path(), cx);
        window.replace_root(cx, |_, cx| {
            let mut shell = Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: String::new(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: zeron_proto::HarnessId::Mock,
                },
                cx,
            );
            shell.debug_gate = Some(GatePhase::Ready);
            shell.splash = SplashPhase::Gone;
            shell.active_chat = "a".into();
            shell.sidebar_opened_at = 1;
            shell.panel_open_sequence = 1;
            shell.test_time = Some(std::time::Instant::now());
            shell
        })
    });
    settled(&shell, cx);
    change(&shell, cx, |s, window, cx| s.open_explorer(window, cx));
    near(settled(&shell, cx).widths[2], 430.0);
    change(&shell, cx, |s, _, cx| s.toggle_right_pane(cx));
    near(settled(&shell, cx).widths[2], 0.0);
    change(&shell, cx, |s, window, cx| s.open_explorer(window, cx));
    near(settled(&shell, cx).widths[2], 430.0);

    for panel in [Panel::Files, Panel::Files, Panel::Sidebar, Panel::Sidebar] {
        change(&shell, cx, |s, window, cx| panel.toggle(s, window, cx));
        near(settled(&shell, cx).widths[2], 430.0);
    }
    // A narrow window constrains only the painted width, retaining the choice.
    cx.simulate_resize(window(1000.0));
    near(settled(&shell, cx).widths[2], 400.0);
    near(
        shell.read_with(cx, |s, _| s.settings.right_pane_width),
        430.0,
    );
    cx.simulate_resize(window(1600.0));
    near(settled(&shell, cx).widths[2], 430.0);
    cx.update(|_, cx| settings::flush(cx));
    near(UiSettings::load(dir.path()).right_pane_width, 430.0);
}

#[gpui::test]
fn panels_share_space_and_keep_rendered_geometry_through_their_lifecycle(cx: &mut TestAppContext) {
    let (_dir, shell, cx) = setup(cx);
    let initial = settled(&shell, cx);
    expect(initial, [300.0, 0.0, 0.0, 1300.0]);

    // Each column responds gently, with no click jump or overshoot. Measure
    // the rendered widths, not a second implementation of the easing curve.
    for (panel, column, target) in [
        (
            Panel::Right,
            2,
            if cfg!(target_os = "linux") {
                520.0
            } else {
                650.0
            },
        ),
        (Panel::Files, 1, 377.0),
        (Panel::Sidebar, 0, 0.0),
    ] {
        let before = draw(&shell, cx, 0);
        change(&shell, cx, |s, window, cx| panel.toggle(s, window, cx));
        continuous(before, draw(&shell, cx, 0));
        let source = before.widths[column];
        let first = draw(&shell, cx, 16);
        let progress = (first.widths[column] - source) / (target - source);
        assert!(
            progress > 0.0 && progress < 0.1,
            "column {column} lurches: {progress}"
        );
        let mut previous = first.widths[column];
        for _ in 0..15 {
            let frame = draw(&shell, cx, 16);
            let width = frame.widths[column];
            assert!(width >= source.min(target) - 0.5 && width <= source.max(target) + 0.5);
            assert!((width - previous) * (target - source) >= -0.5);
            // The visible composer follows its column throughout panel motion.
            let composer_width = f32::from(frame.composer.size.width);
            near(
                composer_width,
                (frame.widths[3] - 2.0 * Theme::SPACE_LG)
                    .min(f32::from(initial.composer.size.width)),
            );
            near(
                f32::from(frame.composer.left()),
                conversation_left(&frame) + (frame.widths[3] - composer_width) / 2.0,
            );
            if frame.widths[2] > 128.0 {
                strip_follows_surface(&frame);
            }
            previous = width;
        }
        near(previous, target);
    }
    let wide = settled(&shell, cx);
    expect(
        wide,
        if cfg!(target_os = "linux") {
            [0.0, 377.0, 520.0, 703.0]
        } else {
            [0.0, 377.0, 611.5, 611.5]
        },
    );
    strip_follows_surface(&wide);
    // A collapsed sidebar leaves the left edge to the window's own resize.
    let edge = from_sidebar_edge;
    cx.simulate_mouse_down(edge(3.0), MouseButton::Left, Default::default());
    for x in [40.0, 80.0] {
        cx.simulate_mouse_move(edge(x), Some(MouseButton::Left), Default::default());
    }
    cx.simulate_mouse_up(edge(80.0), MouseButton::Left, Default::default());
    expect(settled(&shell, cx), wide.widths);

    // Mid-flight reversal, then a different panel interrupting the return.
    change(&shell, cx, |s, _, cx| s.toggle_right_pane(cx));
    let closing = draw(&shell, cx, 75);
    change(&shell, cx, |s, _, cx| s.toggle_right_pane(cx));
    continuous(closing, draw(&shell, cx, 0));
    let reopening = draw(&shell, cx, 40);
    change(&shell, cx, |s, _, cx| s.toggle_sidebar(cx));
    continuous(reopening, draw(&shell, cx, 0));
    let shared = settled(&shell, cx);
    expect(
        shared,
        if cfg!(target_os = "linux") {
            [300.0, 377.0, 520.0, 403.0]
        } else {
            [300.0, 377.0, 461.5, 461.5]
        },
    );

    // An explicit seam reset restores the shared allocation after a drag.
    let (seam_x, widen) = surface_seam(&shared);
    let seam = gpui::point(px(seam_x), px(400.0));
    let dragged = gpui::point(px(seam_x + (500.0 - shared.widths[2]) * widen), px(400.0));
    cx.simulate_mouse_down(seam, MouseButton::Left, Default::default());
    for _ in 0..2 {
        cx.simulate_mouse_move(dragged, Some(MouseButton::Left), Default::default());
    }
    cx.simulate_mouse_up(dragged, MouseButton::Left, Default::default());
    expect(draw(&shell, cx, 0), [300.0, 377.0, 500.0, 423.0]);
    expect(draw(&shell, cx, 300), [300.0, 377.0, 500.0, 423.0]);
    // Visiting a chat with other panels open is not a toggle.
    change(&shell, cx, |s, _, cx| {
        s.panels.update("b", |p| p.changes_open = true);
        select(s, Some("b"), cx);
    });
    expect(settled(&shell, cx), [300.0, 0.0, 500.0, 800.0]);
    change(&shell, cx, |s, _, cx| {
        s.panels.update("b", |p| p.changes_open = false);
        select(s, Some("a"), cx);
    });
    expect(settled(&shell, cx), [300.0, 377.0, 500.0, 423.0]);
    change(&shell, cx, |s, _, cx| {
        s.reset_panel_widths(PaneResizeKind::Right, cx)
    });
    expect(settled(&shell, cx), [300.0, 377.0, 461.5, 461.5]);
    change(&shell, cx, |s, _, cx| {
        s.reset_panel_widths(PaneResizeKind::Files, cx)
    });
    expect(settled(&shell, cx), [300.0, 286.0, 507.0, 507.0]);

    // Narrowing hides the oldest open column without closing it. Asking
    // for it promotes it; closing a neighbour makes room for its return.
    cx.simulate_resize(window(1000.0));
    let narrow = draw(&shell, cx, 0);
    let narrow_widths = if cfg!(target_os = "linux") {
        [300.0, 0.0, 400.0, 300.0]
    } else {
        [300.0, 0.0, 360.0, 340.0]
    };
    expect(narrow, narrow_widths);
    assert!(shell.read_with(cx, |s, cx| s.files_panel_open(cx)));
    change(&shell, cx, |s, window, cx| s.toggle_files_panel(window, cx));
    expect(settled(&shell, cx), [300.0, 286.0, 0.0, 414.0]);
    change(&shell, cx, |s, window, cx| s.toggle_files_panel(window, cx));
    expect(settled(&shell, cx), narrow_widths);
    cx.simulate_resize(window(1600.0));
    change(&shell, cx, |s, window, cx| s.toggle_files_panel(window, cx));
    let ordinary = settled(&shell, cx);
    expect(ordinary, [300.0, 286.0, 507.0, 507.0]);
    // The sidebar's seam follows the pointer, not a tween behind it.
    // At a sidebar width of `x`.
    let sidebar_seam = from_sidebar_edge;
    cx.simulate_mouse_down(sidebar_seam(300.0), MouseButton::Left, Default::default());
    // The first move starts the drag; the following ones move the seam.
    for x in [310.0, 320.0, 340.0] {
        cx.simulate_mouse_move(sidebar_seam(x), Some(MouseButton::Left), Default::default());
        let width = draw(&shell, cx, 16).widths[0];
        if x > 310.0 {
            near(width, x);
        }
    }
    cx.simulate_mouse_move(
        sidebar_seam(300.0),
        Some(MouseButton::Left),
        Default::default(),
    );
    cx.simulate_mouse_up(sidebar_seam(300.0), MouseButton::Left, Default::default());
    // A squeezed sidebar lays its rows and menus out within its column.
    cx.simulate_resize(window(1130.0));
    near(draw(&shell, cx, 0).widths[0], 250.0);
    near(shell.read_with(cx, |s, _| s.sidebar_content_width()), 250.0);
    cx.simulate_resize(window(1600.0));
    expect(settled(&shell, cx), ordinary.widths);

    // Fullscreen clips the conversation, retaining its composer layout.
    change(&shell, cx, |s, _, cx| s.toggle_right_pane_expand(cx));
    for millis in [0, 75, 180] {
        let frame = draw(&shell, cx, millis);
        near(
            f32::from(frame.composer.size.width),
            f32::from(ordinary.composer.size.width),
        );
    }
    expect(draw(&shell, cx, 0), [300.0, 286.0, 1014.0, 0.0]);
    change(&shell, cx, |s, _, cx| select(s, Some("b"), cx));
    expect(settled(&shell, cx), [300.0, 0.0, 0.0, 1300.0]);
    assert!(!shell.read_with(cx, |s, _| s.right_pane_expanded));
    change(&shell, cx, |s, _, cx| select(s, Some("a"), cx));
    expect(draw(&shell, cx, 0), [300.0, 286.0, 1014.0, 0.0]);
    change(&shell, cx, |s, _, cx| s.toggle_right_pane_expand(cx));
    expect(settled(&shell, cx), ordinary.widths);

    // Navigation interrupts a close and lands directly on the destination.
    change(&shell, cx, |s, _, cx| s.toggle_right_pane(cx));
    draw(&shell, cx, 75);
    change(&shell, cx, |s, _, cx| select(s, None, cx));
    expect(draw(&shell, cx, 0), canvas());
    expect(settled(&shell, cx), canvas());
    change(&shell, cx, |s, _, cx| select(s, Some("a"), cx));
    expect(settled(&shell, cx), [300.0, 286.0, 0.0, 1014.0]);
    change(&shell, cx, |s, _, cx| {
        s.open_settings(SettingsSection::Appearance, cx)
    });
    draw(&shell, cx, 0);
    assert!(cx.debug_bounds("conversation-column").is_none());
    change(&shell, cx, |s, _, cx| s.close_settings(cx));
    expect(draw(&shell, cx, 0), [300.0, 286.0, 0.0, 1014.0]);

    // Window resizing and reduced motion apply directly, without a lagging tween.
    for width in [1024.0, 800.0, 600.0, 800.0, 1600.0] {
        cx.simulate_resize(window(width));
        let frame = draw(&shell, cx, 0);
        near(frame.widths.iter().sum(), width);
        expect(draw(&shell, cx, 300), frame.widths);
    }
    // A side chat's composer keeps its column's layout under the pane's mask.
    change(&shell, cx, |s, _, cx| {
        let chat = serde_json::from_value(serde_json::json!({
            "id": "side", "parentChatId": "a", "deviceId": "local",
            "archived": false, "createdAt": Utc::now(),
        }))
        .unwrap();
        s.open_side_chat(chat, s.panel_key(cx), cx);
    });
    settled(&shell, cx);
    let side_composer = |cx: &mut VisualTestContext| {
        shell.read_with(cx, |s, cx| {
            let tab = s.side_chats.values().next().unwrap();
            tab.composer.read(cx).surface_bounds().get().unwrap()
        })
    };
    let resting = side_composer(cx);
    for _ in 0..2 {
        change(&shell, cx, |s, _, cx| s.toggle_right_pane(cx));
        draw(&shell, cx, 75);
        let moving = side_composer(cx);
        near(f32::from(moving.size.width), f32::from(resting.size.width));
        near(
            f32::from(moving.size.height),
            f32::from(resting.size.height),
        );
        settled(&shell, cx);
    }
    change(&shell, cx, |s, _, cx| s.toggle_right_pane(cx));
    settled(&shell, cx);
    cx.update(|_, cx| motion::set_reduced_motion(cx, true));
    for panel in [
        Panel::Sidebar,
        Panel::Right,
        Panel::Files,
        Panel::Files,
        Panel::Right,
        Panel::Sidebar,
    ] {
        change(&shell, cx, |s, window, cx| panel.toggle(s, window, cx));
        let landed = draw(&shell, cx, 0);
        near(landed.widths.iter().sum(), 1600.0);
        expect(settled(&shell, cx), landed.widths);
    }
}

/// Side-chat tabs belong to an engine connection; connectionless shell
/// updates intentionally discard them. Keep the RPC transport alive in tests.
fn connect_rename_engine(
    shell: &Entity<Shell>,
    cx: &mut VisualTestContext,
) -> (
    tokio::runtime::Runtime,
    tokio::sync::mpsc::Receiver<String>,
    tokio::sync::mpsc::Sender<String>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (out, requests) = tokio::sync::mpsc::channel(256);
    let (replies, inbound) = tokio::sync::mpsc::channel(256);
    let engine = {
        let _guard = runtime.enter();
        crate::state::EngineHandle::from_test_client(zeron_rpc::RpcClient::new(out, inbound))
    };
    shell.update(cx, |shell, cx| {
        shell
            .state
            .update(cx, |state, _| state.set_test_engine(engine));
    });
    (runtime, requests, replies)
}

#[gpui::test]
fn side_chat_menu_rename_keeps_the_editor_visible_without_opening_files(cx: &mut TestAppContext) {
    let (_dir, shell, cx) = setup(cx);
    let (runtime, _requests, _replies) = connect_rename_engine(&shell, cx);
    let _guard = runtime.enter();
    change(&shell, cx, |shell, _, cx| {
        shell.state.update(cx, |state, cx| {
            state.chats.push(
                serde_json::from_value(serde_json::json!({
                    "id": "side", "title": "Side chat", "parentChatId": "a",
                    "deviceId": "local", "archived": false, "createdAt": Utc::now(),
                }))
                .unwrap(),
            );
            cx.notify();
        });
        shell.chat_menu.open(ChatMenuState {
            chat_id: "side".into(),
            tab: None,
            position: gpui::point(px(800.0), px(200.0)),
            page: ChatMenuPage::Root,
        });
    });
    settled(&shell, cx);
    assert!(shell.read_with(cx, |shell, cx| !shell.files_panel_open(cx)));

    let rename = cx.debug_bounds("chat-menu-rename").unwrap();
    cx.simulate_click(rename.center(), Default::default());
    settled(&shell, cx);

    shell.read_with(cx, |shell, cx| {
        assert!(
            !shell.files_panel_open(cx),
            "Rename must not open the explorer"
        );
        assert_eq!(shell.session_info_visible(cx), !cfg!(target_os = "linux"));
        assert_eq!(shell.chat_rename.as_ref().unwrap().chat_id, "side");
    });
    assert_side_chat_rename_visible(&shell, cx, "session-lanes-chat-side-title-editor");
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn side_chat_tab_rename_keeps_focus_and_handles_save_cancel_and_blur(cx: &mut TestAppContext) {
    let (_dir, shell, cx) = setup(cx);
    let (runtime, mut requests, _replies) = connect_rename_engine(&shell, cx);
    let _guard = runtime.enter();
    let renames = |requests: &mut tokio::sync::mpsc::Receiver<String>| {
        std::iter::from_fn(|| requests.try_recv().ok())
            .map(|frame| serde_json::from_str::<serde_json::Value>(&frame).unwrap())
            .filter(|frame| {
                frame["method"] == methods::MUTATE && frame["params"]["op"] == "renameChat"
            })
            .map(|frame| frame["params"].clone())
            .collect::<Vec<_>>()
    };
    cx.update(|window, cx| {
        window.activate_window();
        apply_keymap(
            cx,
            &KeymapConfig::default(),
            ComposerSendBehavior::default(),
        );
    });
    change(&shell, cx, |shell, _, cx| {
        shell.state.update(cx, |state, cx| {
            state.chats.push(
                serde_json::from_value(serde_json::json!({
                    "id": "side", "title": "Side chat", "parentChatId": "a",
                    "deviceId": "local", "archived": false, "createdAt": Utc::now(),
                }))
                .unwrap(),
            );
            cx.notify();
        });
        shell.open_rename_chat("side".into(), cx);
    });
    settled(&shell, cx);
    let field = assert_side_chat_rename_visible(&shell, cx, "session-lanes-chat-side-title-editor");
    cx.simulate_click(field.center(), Default::default());
    settled(&shell, cx);
    cx.update(|window, cx| {
        shell.read_with(cx, |shell, cx| {
            assert!(
                shell
                    .chat_rename
                    .as_ref()
                    .unwrap()
                    .input
                    .focus_handle(cx)
                    .is_focused(window)
            );
        });
    });
    cx.simulate_input("Cancelled");
    cx.simulate_keystrokes("escape");
    settled(&shell, cx);
    shell.read_with(cx, |shell, _| {
        assert!(shell.chat_rename.is_none());
    });
    assert!(
        renames(&mut requests).is_empty(),
        "Escape must not send a rename"
    );

    for blur in [false, true] {
        change(&shell, cx, |shell, _, cx| {
            shell.open_rename_chat("side".into(), cx);
        });
        settled(&shell, cx);
        cx.simulate_input("Renamed side chat");
        cx.update(|window, cx| {
            shell.read_with(cx, |shell, cx| {
                let input = &shell.chat_rename.as_ref().unwrap().input;
                assert!(
                    input.focus_handle(cx).is_focused(window),
                    "rename must have focus (blur={blur})"
                );
                assert_eq!(input.read(cx).text(), "Renamed side chat");
            });
        });
        if blur {
            cx.update(|window, _| window.blur());
        } else {
            cx.simulate_keystrokes("enter");
        }
        settled(&shell, cx);
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| {
            assert!(
                shell.chat_rename.is_none(),
                "rename must close (blur={blur})"
            );
        });
        assert_eq!(
            renames(&mut requests),
            vec![serde_json::json!({
                "op": "renameChat", "chatId": "side", "title": "Renamed side chat",
            })],
            "the title must be sent (blur={blur})"
        );
        shell.read_with(cx, |shell, _| {
            assert!(shell.chat_rename.is_none());
            assert_eq!(shell.side_chats.len(), 1, "rename reuses its existing tab");
        });
    }
}

#[track_caller]
fn assert_side_chat_rename_visible(
    shell: &Entity<Shell>,
    cx: &mut VisualTestContext,
    lane_selector: &'static str,
) -> Bounds<Pixels> {
    // A newly opened surface mounts after the panel's deferred content reveal.
    cx.run_until_parked();
    draw(shell, cx, 100);
    let (field, viewport) = if cfg!(target_os = "linux") {
        (
            cx.debug_bounds("side-chat-tab-title-editor").unwrap(),
            shell.read_with(cx, |shell, _| shell.right_tab_scroll.bounds()),
        )
    } else {
        (
            cx.debug_bounds(lane_selector).unwrap(),
            cx.debug_bounds("session-lanes").unwrap(),
        )
    };
    assert!(
        field.left() >= viewport.left()
            && field.right() <= viewport.right()
            && field.top() >= viewport.top()
            && field.bottom() <= viewport.bottom(),
        "the rename editor {field:?} is outside its viewport {viewport:?}"
    );
    field
}

#[gpui::test]
fn side_chat_rename_leaves_takeover_to_show_its_editor(cx: &mut TestAppContext) {
    let (_dir, shell, cx) = setup(cx);
    let (runtime, _requests, _replies) = connect_rename_engine(&shell, cx);
    let _guard = runtime.enter();
    change(&shell, cx, |shell, _, cx| {
        shell.state.update(cx, |state, cx| {
            state.chats.push(
                serde_json::from_value(serde_json::json!({
                    "id": "side", "title": "Side chat", "parentChatId": "a",
                    "deviceId": "local", "archived": false, "createdAt": Utc::now(),
                }))
                .unwrap(),
            );
            cx.notify();
        });
        shell.toggle_right_pane(cx);
    });
    settled(&shell, cx);
    change(&shell, cx, |s, _, cx| s.toggle_right_pane_expand(cx));
    near(settled(&shell, cx).widths[3], 0.0);

    change(&shell, cx, |s, _, cx| s.open_rename_chat("side".into(), cx));
    let frame = settled(&shell, cx);
    assert!(!shell.read_with(cx, |s, _| s.right_pane_expanded));
    assert!(frame.widths[3] > 0.0, "the transcript column must return");
    assert_side_chat_rename_visible(&shell, cx, "session-lanes-chat-side-title-editor");
}

fn check_side_chat_rename_scroll(cx: &mut TestAppContext, width: f32, reduced_motion: bool) {
    let (_dir, shell, cx) = setup(cx);
    let (runtime, _requests, _replies) = connect_rename_engine(&shell, cx);
    let _guard = runtime.enter();
    cx.simulate_resize(gpui::size(px(width), px(900.0)));
    cx.update(|_, cx| motion::set_reduced_motion(cx, reduced_motion));
    change(&shell, cx, |shell, _, cx| {
        shell.state.update(cx, |state, cx| {
            let now = Utc::now();
            for index in 0..40 {
                state.chats.push(
                    serde_json::from_value(serde_json::json!({
                        "id": format!("side-{index}"), "title": format!("Side {index}"),
                        "parentChatId": "a", "deviceId": "local", "archived": false,
                        "createdAt": now - chrono::Duration::minutes(index),
                    }))
                    .unwrap(),
                );
            }
            cx.notify();
        });
        if cfg!(target_os = "linux") {
            // The Linux editor lives in a tab. Fill the strip so both ends
            // really are outside its horizontal viewport before revealing them.
            let chats: Vec<_> = shell
                .state
                .read(cx)
                .chats
                .iter()
                .filter(|chat| chat.parent_chat_id.as_deref() == Some("a"))
                .cloned()
                .collect();
            for chat in chats {
                shell.open_side_chat(chat, shell.panel_key(cx), cx);
            }
        }
    });
    settled(&shell, cx);
    assert!(cx.debug_bounds("session-lanes").is_none());

    // Reveal a row/tab outside the viewport, then the one at the other end.
    // Both directions must leave the whole editor visible.
    for (chat, selector) in [
        ("side-39", "session-lanes-chat-side-39-title-editor"),
        ("side-0", "session-lanes-chat-side-0-title-editor"),
    ] {
        change(&shell, cx, |shell, _, cx| {
            shell.open_rename_chat(chat.into(), cx);
        });
        settled(&shell, cx);
        for _ in 0..4 {
            cx.run_until_parked();
            draw(&shell, cx, 100);
        }
        assert_side_chat_rename_visible(&shell, cx, selector);
        cx.update(|window, cx| {
            shell.read_with(cx, |shell, cx| {
                let rename = shell.chat_rename.as_ref().unwrap();
                assert_eq!(rename.chat_id, chat);
                assert!(rename.input.focus_handle(cx).is_focused(window));
            });
        });
    }
}

#[gpui::test]
fn side_chat_rename_scrolls_into_view_in_a_wide_window(cx: &mut TestAppContext) {
    check_side_chat_rename_scroll(cx, 1600.0, false);
}

#[gpui::test]
fn side_chat_rename_scrolls_into_view_in_a_narrow_window_without_motion(cx: &mut TestAppContext) {
    check_side_chat_rename_scroll(cx, 1000.0, true);
}

// The Linux workbench has no session card. Its absence and layout are covered
// by workbench_never_shows_the_session_card_or_reserves_its_space above.
#[cfg(not(target_os = "linux"))]
#[gpui::test]
fn the_session_card_lands_with_a_chat_switch_and_eases_on_its_toggle(cx: &mut TestAppContext) {
    let (_dir, shell, cx) = setup(cx);
    change(&shell, cx, |s, _, _| {
        s.settings.session_info_hidden = false;
        // b's surface host leaves its transcript no room for the card.
        s.panels.update("b", |p| p.changes_open = true);
    });
    settled(&shell, cx);
    let card = cx
        .debug_bounds("session-info")
        .expect("a's wide column shows the card");
    // Where the card's reveal is easing to, if it eases at all.
    let easing_to = |cx: &mut VisualTestContext| {
        shell.read_with(cx, |s, _| s.session_info_tween.map(|tween| tween.to))
    };

    // The panels snap to b's; the card must not fade out over b's transcript.
    change(&shell, cx, |s, _, cx| select(s, Some("b"), cx));
    draw(&shell, cx, 0);
    assert!(cx.debug_bounds("session-info").is_none());
    assert_eq!(easing_to(cx), None);

    // Back on a, it is at rest from the first frame.
    change(&shell, cx, |s, _, cx| select(s, Some("a"), cx));
    draw(&shell, cx, 0);
    let landed = cx
        .debug_bounds("session-info")
        .expect("a shows its card from the first frame");
    near(f32::from(landed.left()), f32::from(card.left()));
    assert_eq!(easing_to(cx), None);

    // Its own toggle still eases it out, then back in.
    for (reveal, shown) in [(0.0, false), (1.0, true)] {
        change(&shell, cx, |s, _, cx| s.toggle_session_info(cx));
        draw(&shell, cx, 0);
        assert_eq!(easing_to(cx), Some(reveal));
        settled(&shell, cx);
        assert_eq!(cx.debug_bounds("session-info").is_some(), shown);
    }
}
