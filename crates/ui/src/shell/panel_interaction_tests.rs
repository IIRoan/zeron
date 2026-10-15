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
