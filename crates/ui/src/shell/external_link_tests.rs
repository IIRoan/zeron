//! Exercise painted chat links through Shell and the operating-system opener.
use super::*;
use crate::markdown::render::{LinkAction, LinkActivation, LinkOutcome, LinkTarget};
use gpui::{AppContext, TestAppContext, VisualTestContext};
use zeron_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};

fn setup(cx: &mut TestAppContext) -> (tempfile::TempDir, Entity<Shell>, &mut VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
    // An existing profile must not retain either embedded destination.
    std::fs::write(
        UiSettings::path(dir.path()),
        r#"{"openWebLinksInZeron":true,"pullRequestOpenIn":"native","sidebarWidth":280}"#,
    )
    .unwrap();
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
        settings::init(UiSettings::load(dir.path()), dir.path(), cx);
        motion::set_reduced_motion(cx, true);
    });
    let (shell, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.workspace_scope = Some(WorkspaceScope::Development);
            state.connection = ConnectionStatus::Ready;
            state.local_device_id = Some("local".into());
            state.chats.push(
                serde_json::from_value(serde_json::json!({
                    "id": "a", "title": "Chat", "deviceId": "local", "archived": false,
                    "createdAt": Utc::now(),
                }))
                .unwrap(),
            );
            state.selected_chat = Some("a".into());
            state
        });
        let mut shell = Shell::new(
            state,
            EngineBootConfig {
                data_dir: dir.path().into(),
                ipc_port: 0,
                edge_url: "http://127.0.0.1:1".into(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: zeron_proto::HarnessId::Mock,
            },
            cx,
        );
        shell.debug_gate = Some(GatePhase::Ready);
        shell.splash = SplashPhase::Gone;
        shell.reduced_motion = true;
        shell.active_chat = "a".into();
        shell
    });
    cx.simulate_resize(gpui::size(px(1400.), px(800.)));
    redraw(cx);
    (dir, shell, cx)
}

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.refresh().unwrap();
    cx.update(|window, cx| window.draw(cx).clear());
    cx.run_until_parked();
}

fn message(shell: &Entity<Shell>, text: String, cx: &mut VisualTestContext) {
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.apply_transcript(vec![SessionMessageEntry {
                id: "reply".into(),
                role: MessageRole::Assistant,
                parts: vec![MessagePart::Text {
                    id: "text".into(),
                    text,
                }],
                created_at: 0,
                device_id: "local".into(),
                status: Some(MessageStatus::Complete),
                continuation_of: None,
                duration_ms: None,
            }]);
            cx.notify();
        });
    });
    redraw(cx);
}

fn painted(label: &str) -> Point<Pixels> {
    crate::markdown::render::selection_test_position(label) + gpui::point(px(2.), px(8.))
}

fn assert_chat_unchanged(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    shell.read_with(cx, |shell, cx| {
        assert!(
            shell.browsers.is_empty(),
            "a chat link must not create a Browser tab"
        );
        assert!(shell.right_tabs.get("a").is_none_or(Vec::is_empty));
        assert_eq!(shell.route, Route::Chat);
        assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("a"));
    });
    assert!(cx.debug_bounds("open-pull-requests").is_none());
}

#[gpui::test]
fn painted_markdown_and_bare_links_open_the_default_browser(cx: &mut TestAppContext) {
    let _selection = crate::markdown::selection::test_state_lock();
    let (_dir, shell, cx) = setup(cx);
    let urls = [
        (
            "pull request",
            "https://github.com/acme/repo/pull/123/files?diff=split#discussion_r1",
        ),
        ("repository", "https://github.com/acme/repo"),
        ("issue", "https://github.com/acme/repo/issues/42"),
        (
            "documentation",
            "https://example.com/docs?topic=git#install",
        ),
        ("preview", "http://localhost:3000/"),
    ];
    message(
        &shell,
        urls.iter()
            .map(|(label, url)| format!("[{label}]({url})"))
            .collect::<Vec<_>>()
            .join(", ")
            + ", https://example.org/bare",
        cx,
    );
    for (label, url) in urls
        .into_iter()
        .chain([("https://example.org/bare", "https://example.org/bare")])
    {
        let position = painted(label);
        cx.simulate_mouse_move(position, None, gpui::Modifiers::default());
        redraw(cx);
        cx.simulate_click(position, gpui::Modifiers::default());
        redraw(cx);
        assert_eq!(cx.opened_url().as_deref(), Some(url), "{label}");
        assert_chat_unchanged(&shell, cx);
    }
}

#[gpui::test]
fn web_context_menu_has_only_external_open_and_copy(cx: &mut TestAppContext) {
    let _selection = crate::markdown::selection::test_state_lock();
    let (_dir, shell, cx) = setup(cx);
    let url = "https://github.com/acme/repo/pull/123";
    message(&shell, format!("[review]({url})"), cx);
    let position = painted("review");
    cx.simulate_mouse_down(position, MouseButton::Right, gpui::Modifiers::default());
    cx.simulate_mouse_up(position, MouseButton::Right, gpui::Modifiers::default());
    redraw(cx);
    assert!(cx.debug_bounds("link-menu-open-zeron").is_none());
    assert!(cx.debug_bounds("link-menu-default-destination").is_none());
    assert!(cx.debug_bounds("link-menu-copy-address").is_some());
    let open = cx.debug_bounds("link-menu-open-external").unwrap().center();
    cx.simulate_click(open, gpui::Modifiers::default());
    redraw(cx);
    assert_eq!(cx.opened_url().as_deref(), Some(url));
    assert_chat_unchanged(&shell, cx);
}

#[gpui::test]
fn legacy_internal_actions_leave_chat_and_stale_links_remain_rejected(cx: &mut TestAppContext) {
    let (_dir, shell, cx) = setup(cx);
    shell.update_in(cx, |shell, window, cx| {
        for action in [
            LinkAction::Primary,
            LinkAction::Internal,
            LinkAction::External,
        ] {
            let mut activation = LinkActivation {
                target: LinkTarget::new("PR", "https://github.com/acme/repo/pull/123"),
                action,
                source_session: Some("a".into()),
            };
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::External("https://github.com/acme/repo/pull/123".into())
            );
            activation.source_session = Some("another-session".into());
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Rejected
            );
            activation.source_session = None;
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Rejected
            );
            activation.source_session = Some("a".into());
            shell.state.update(cx, |state, _| {
                state.selected_chat = Some("switch-in-progress".into())
            });
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Rejected
            );
            shell
                .state
                .update(cx, |state, _| state.selected_chat = Some("a".into()));
        }
        assert!(shell.browsers.is_empty());
    });
}

#[gpui::test]
fn side_chat_links_leave_without_replacing_either_conversation(cx: &mut TestAppContext) {
    let (_dir, shell, cx) = setup(cx);
    shell.update_in(cx, |shell, window, cx| {
        let chat = serde_json::from_value(serde_json::json!({
            "id": "side", "parentChatId": "a", "deviceId": "local",
            "archived": false, "createdAt": Utc::now(),
        }))
        .unwrap();
        shell.open_side_chat(chat, shell.panel_key(cx), cx);
        let tabs = shell.right_tabs.get("a").cloned().unwrap();
        let activation = LinkActivation {
            target: LinkTarget::new("PR", "https://github.com/acme/repo/pull/9"),
            action: LinkAction::Primary,
            source_session: Some("side".into()),
        };
        assert_eq!(
            shell.activate_session_link(&activation, window, cx),
            LinkOutcome::External("https://github.com/acme/repo/pull/9".into())
        );
        assert_eq!(shell.right_tabs.get("a"), Some(&tabs));
        assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("a"));
        assert!(shell.browsers.is_empty());
        shell.close_right_surface(tabs[0], window, cx);
        assert_eq!(
            shell.activate_session_link(&activation, window, cx),
            LinkOutcome::Rejected
        );
    });
}
