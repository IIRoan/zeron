use super::*;
use gpui::{AppContext, Bounds, TestAppContext, VisualTestContext};

/// Exercise the production form and its real callbacks without booting an
/// engine or depending on the desktop shell's connection gate.
struct SettingsHost {
    shell: Entity<Shell>,
    _directory: tempfile::TempDir,
    _changed: Subscription,
}

impl Render for SettingsHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.shell.update(cx, |shell, cx| {
            let viewport = window.viewport_size();
            let settings = shell.render_worktree_settings(viewport, window, cx);
            let discard =
                shell.render_discard_prompt(UnsavedForm::WorktreeSettings, viewport, window, cx);
            div()
                .size_full()
                .track_focus(&shell.shortcut_focus)
                .capture_key_down(cx.listener(Shell::on_key_down_capture))
                .on_key_down(cx.listener(Shell::on_key_down))
                .children(settings)
                .children(discard)
        })
    }
}

fn setup(cx: &mut TestAppContext) -> (Entity<Shell>, &mut VisualTestContext) {
    let directory = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        motion::set_reduced_motion(cx, true);
    });
    let (host, cx) = cx.add_window_view(|_, cx| {
        let shell = cx.new(|cx| {
            let state = cx.new(|_| AppState::new());
            let mut shell = Shell::new(
                state,
                EngineBootConfig {
                    data_dir: directory.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: zeron_proto::HarnessId::Mock,
                },
                cx,
            );
            shell.state.update(cx, |state, _| {
                state.local_device_id = Some("local".into());
                state.spaces = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "project", "deviceId": "local", "path": "/project",
                        "createdAt": "2026-01-01T00:00:00Z",
                    }))
                    .unwrap(),
                ];
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "local", "name": "This device", "platform": "linux",
                        "capabilities": [zeron_proto::capabilities::PROJECT_DATABASES_V1],
                    }))
                    .unwrap(),
                ];
            });
            let settings = WorktreeSettings {
                env_files: vec![WorktreeEnvFile {
                    path: "apps/backend/config/development/.env.local".into(),
                    mode: Env::Copy,
                }],
                dependency_paths: vec!["node_modules".into(), "apps/web/node_modules".into()],
                ..Default::default()
            };
            let mut editor = Editor::new(ProjectContext {
                space: "project".into(),
                device: "local".into(),
                name: "Project with a deliberately long workspace name".into(),
                target: None,
                chat: None,
                cwd: "/project".into(),
                checkout_override: None,
            });
            editor.fields = Some(shell.worktree_fields(settings.clone(), vec![], cx));
            editor.snapshot = Some(WorktreeSettingsSnapshot {
                defaults: settings.clone(),
                settings,
                checkout: None,
                has_overrides: false,
                detected_env_files: vec!["apps/backend/.env.local".into()],
                detected_dependency_paths: vec!["node_modules".into()],
                detected_install_command: "bun install --frozen-lockfile".into(),
                detected_workflow: None,
                worktrees: vec![zeron_proto::WorktreeEnvironmentEntry {
                    path: "/project-worktree".into(),
                    branch: "feature/a-deliberately-long-worktree-branch-name".into(),
                    has_overrides: false,
                    managed: true,
                    state: Default::default(),
                }],
                databases: vec![],
                app_services: vec![],
            });
            shell.worktree_settings.editor.open(editor);
            shell
        });
        let changed = cx.observe(&shell, |_, _, cx| cx.notify());
        SettingsHost {
            shell,
            _directory: directory,
            _changed: changed,
        }
    });
    let shell = host.read_with(cx, |host, _| host.shell.clone());
    redraw(cx);
    (shell, cx)
}

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
}

fn settle_popup_exit(cx: &mut VisualTestContext) {
    // Popups retain their exit frame using wall time, and the reap task uses
    // the test executor's timer. Let both finish before clicking the trigger
    // again, just as an ordinary user does between menu selections.
    let duration = motion::MENU_OUT
        .total()
        .mul_f32(motion::speed_scale())
        .saturating_add(std::time::Duration::from_millis(25));
    std::thread::sleep(duration);
    cx.executor().advance_clock(duration);
    redraw(cx);
}

fn reveal_in_settings(
    shell: &Entity<Shell>,
    cx: &mut VisualTestContext,
    selector: &'static str,
) -> Bounds<Pixels> {
    let item = bounds(cx, selector);
    let scroll = bounds(cx, "worktree-environment-scroll");
    let delta = if item.bottom() > scroll.bottom() - px(8.0) {
        scroll.bottom() - px(8.0) - item.bottom()
    } else if item.top() < scroll.top() + px(8.0) {
        scroll.top() + px(8.0) - item.top()
    } else {
        px(0.0)
    };
    if delta != px(0.0) {
        shell.update(cx, |shell, cx| {
            let scroll = &shell.worktree_settings.editor.as_open().unwrap().scroll;
            let offset = scroll.offset();
            scroll.set_offset(gpui::point(offset.x, offset.y + delta));
            cx.notify();
        });
        redraw(cx);
    }
    let item = bounds(cx, selector);
    contains(bounds(cx, "worktree-environment-scroll"), item, selector);
    item
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing project settings element: {selector}"))
}

fn contains(outer: Bounds<Pixels>, inner: Bounds<Pixels>, label: &str) {
    let tolerance = px(1.0);
    assert!(
        inner.left() >= outer.left() - tolerance
            && inner.top() >= outer.top() - tolerance
            && inner.right() <= outer.right() + tolerance
            && inner.bottom() <= outer.bottom() + tolerance,
        "{label} escaped its container: {inner:?} outside {outer:?}",
    );
}

fn assert_modal_dropdown_geometry(
    cx: &mut VisualTestContext,
    trigger: Bounds<Pixels>,
    minimum_width: Pixels,
) {
    let viewport =
        cx.update(|window, _| Bounds::new(gpui::point(px(0.0), px(0.0)), window.viewport_size()));
    let menu = bounds(cx, "contained-menu-scroll");
    contains(viewport, menu, "modal dropdown");
    assert!(
        menu.size.width >= minimum_width,
        "modal dropdown should use the window's available width: {menu:?}"
    );
    assert!(
        menu.left() <= trigger.center().x && menu.right() >= trigger.center().x,
        "modal dropdown should remain horizontally anchored to its trigger: {menu:?} vs {trigger:?}"
    );
    assert!(
        menu.top() >= trigger.bottom() + px(4.0) || menu.bottom() <= trigger.top() - px(4.0),
        "modal dropdown should open beside its trigger without covering it: {menu:?} vs {trigger:?}"
    );
}

const PAGES: [(SettingsPage, &str); 5] = [
    (SettingsPage::General, "general"),
    (SettingsPage::Environment, "environment"),
    (SettingsPage::Workflow, "workflow"),
    (SettingsPage::Services, "services"),
    (SettingsPage::Advanced, "advanced"),
];

fn overview_selector(page: SettingsPage) -> &'static str {
    match page {
        SettingsPage::General => panic!("the overview has no navigation row"),
        SettingsPage::Environment => "project-settings-open-environment",
        SettingsPage::Workflow => "project-settings-open-workflow",
        SettingsPage::Services => "project-settings-open-services",
        SettingsPage::Advanced => "project-settings-open-advanced",
    }
}

fn open_page(shell: &Entity<Shell>, cx: &mut VisualTestContext, page: SettingsPage) {
    let current = shell.read_with(cx, |shell, _| shell.worktree_settings.page);
    if current != SettingsPage::General {
        let back = bounds(cx, "project-settings-back");
        cx.simulate_click(back.center(), gpui::Modifiers::default());
        redraw(cx);
    }
    if page != SettingsPage::General {
        let row = reveal_in_settings(shell, cx, overview_selector(page));
        cx.simulate_click(row.center(), gpui::Modifiers::default());
        redraw(cx);
    }
}

fn page_selector(page: SettingsPage) -> &'static str {
    match page {
        SettingsPage::General => "project-settings-page-general",
        SettingsPage::Environment => "project-settings-page-environment",
        SettingsPage::Workflow => "project-settings-page-workflow",
        SettingsPage::Services => "project-settings-page-services",
        SettingsPage::Advanced => "project-settings-page-advanced",
    }
}

#[gpui::test]
fn project_settings_pages_fit_wide_and_compact_windows(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    for (width, height) in [
        (1100.0, 800.0),
        (800.0, 700.0),
        (600.0, 650.0),
        (390.0, 650.0),
    ] {
        cx.simulate_resize(gpui::size(px(width), px(height)));
        redraw(cx);
        for (page, slug) in PAGES {
            open_page(&shell, cx, page);
            shell.read_with(cx, |shell, _| {
                assert!(shell.worktree_settings.page == page, "{slug} did not open")
            });

            let viewport = Bounds::new(
                gpui::point(px(0.0), px(0.0)),
                gpui::size(px(width), px(height)),
            );
            let card = bounds(cx, "project-settings-card");
            contains(viewport, card, "settings dialog");
            for selector in [
                "project-settings-header",
                "project-settings-navigation",
                "project-settings-body",
                "project-settings-footer",
            ] {
                contains(card, bounds(cx, selector), selector);
            }
            if page == SettingsPage::General {
                assert!(cx.debug_bounds("project-settings-back").is_none());
                for (destination, _) in PAGES.into_iter().skip(1) {
                    let selector = overview_selector(destination);
                    let row = bounds(cx, selector);
                    contains(bounds(cx, page_selector(page)), row, selector);
                    assert!(row.size.height >= px(44.0), "{selector} is squeezed");
                    assert!(
                        row.size.width >= px(250.0),
                        "overview navigation should remain usable at width {width}"
                    );
                }
            } else {
                let back = bounds(cx, "project-settings-back");
                contains(bounds(cx, "project-settings-header"), back, "back button");
                assert!(back.size.width >= px(24.0));
                assert!(back.size.height >= px(24.0));
                for (destination, _) in PAGES.into_iter().skip(1) {
                    assert!(cx.debug_bounds(overview_selector(destination)).is_none());
                }
            }
            let footer = bounds(cx, "project-settings-footer");
            for selector in ["close-worktree-settings", "save-worktree-settings"] {
                let button = bounds(cx, selector);
                contains(footer, button, selector);
                assert!(button.size.width >= px(40.0), "{selector} is squeezed");
                assert!(button.size.height >= px(24.0), "{selector} is squeezed");
            }
            let scroll = bounds(cx, "worktree-environment-scroll");
            let body = bounds(cx, "project-settings-body");
            let navigation = bounds(cx, "project-settings-navigation");
            assert!(
                navigation.size.height < px(80.0),
                "scope selection should remain compact at width {width}"
            );
            assert!(
                navigation.bottom() <= scroll.top() + px(1.0),
                "scope selection should sit above the form at width {width}"
            );
            assert!(
                f32::from(scroll.size.width - body.size.width).abs() <= 2.0,
                "scope selection must leave the full content width at width {width}"
            );
            assert!(
                scroll.size.height > px(80.0),
                "settings lost their scrolling area at {width}×{height}"
            );
            let page_bounds = bounds(cx, page_selector(page));
            assert!(page_bounds.left() >= scroll.left());
            assert!(
                page_bounds.right() <= scroll.right() + px(1.0),
                "{slug} overflows at width {width}"
            );
            for (other, _) in PAGES.into_iter().filter(|(other, _)| *other != page) {
                assert!(cx.debug_bounds(page_selector(other)).is_none());
            }
        }
    }
}

#[gpui::test]
fn project_settings_general_controls_share_the_full_form_width(cx: &mut TestAppContext) {
    let (_, cx) = setup(cx);
    for (width, height) in [(1100.0, 800.0), (600.0, 650.0), (390.0, 650.0)] {
        cx.simulate_resize(gpui::size(px(width), px(height)));
        redraw(cx);
        let page = bounds(cx, page_selector(SettingsPage::General));
        let block = bounds(cx, "project-settings-general-block");
        let branch = bounds(cx, "project-settings-branch-row");
        contains(page, block, "general settings block");
        contains(block, branch, "branch prefix row");
        assert!(
            f32::from(page.size.width - block.size.width).abs() <= 2.0,
            "general controls should fill their content area at width {width}"
        );
        assert!(
            branch.size.width >= block.size.width * 0.75,
            "the branch control should occupy a full row at width {width}"
        );
        assert!(
            (branch.left() - block.left() - (block.right() - branch.right())).abs() <= px(1.0),
            "the branch row should have matching horizontal insets at width {width}"
        );
        contains(
            block,
            bounds(cx, "project-icon-choose"),
            "project icon chooser",
        );
    }
}

#[gpui::test]
fn project_settings_navigation_keeps_the_dialog_and_header_in_place(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    for (width, height, expected_height) in [(1100.0, 800.0, 640.0), (390.0, 650.0, 602.0)] {
        cx.simulate_resize(gpui::size(px(width), px(height)));
        redraw(cx);
        let header = bounds(cx, "project-settings-header");
        let navigation = bounds(cx, "project-settings-navigation");
        let footer = bounds(cx, "project-settings-footer");
        for (page, _) in PAGES {
            open_page(&shell, cx, page);
            let card = bounds(cx, "project-settings-card");
            assert!(
                (card.size.height - px(expected_height)).abs() <= px(1.0),
                "navigation should keep a stable dialog height at width {width}: {card:?}"
            );
            for (selector, original) in [
                ("project-settings-header", header),
                ("project-settings-navigation", navigation),
                ("project-settings-footer", footer),
            ] {
                let current = bounds(cx, selector);
                assert!(
                    (current.top() - original.top()).abs() <= px(1.0)
                        && (current.size.height - original.size.height).abs() <= px(1.0),
                    "{selector} moved while navigating at width {width}: {current:?} vs {original:?}"
                );
            }
        }
        open_page(&shell, cx, SettingsPage::General);
    }
}

#[gpui::test]
fn project_settings_scope_menu_switches_checkout_without_losing_the_defaults_draft(
    cx: &mut TestAppContext,
) {
    let (shell, cx) = setup(cx);
    cx.simulate_resize(gpui::size(px(390.0), px(650.0)));
    let prefix = shell.update(cx, |shell, cx| {
        let prefix = shell
            .worktree_settings
            .editor
            .open_mut()
            .unwrap()
            .fields
            .as_ref()
            .unwrap()
            .prefix
            .clone();
        prefix.update(cx, |input, cx| input.set_text("feature/draft/", cx));
        prefix.entity_id()
    });
    redraw(cx);

    for (option, checkout) in [
        ("project-settings-scope-option-1", Some("/project-worktree")),
        ("project-settings-scope-option-0", None),
    ] {
        let trigger = bounds(cx, "project-settings-scope");
        cx.simulate_click(trigger.center(), gpui::Modifiers::default());
        redraw(cx);
        assert_modal_dropdown_geometry(cx, trigger, px(200.0));
        let option = bounds(cx, option);
        cx.simulate_click(option.center(), gpui::Modifiers::default());
        redraw(cx);
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                shell
                    .worktree_settings
                    .editor
                    .as_open()
                    .unwrap()
                    .checkout
                    .as_deref(),
                checkout,
                "the scope menu must select its worktree"
            );
        });
    }
    shell.read_with(cx, |shell, cx| {
        let fields = shell
            .worktree_settings
            .editor
            .as_open()
            .unwrap()
            .fields
            .as_ref()
            .unwrap();
        assert_eq!(fields.prefix.entity_id(), prefix);
        assert_eq!(fields.prefix.read(cx).text(), "feature/draft/");
        assert!(shell.worktree_settings_dirty(cx));
    });
}

#[gpui::test]
fn project_settings_env_mode_menu_edits_only_its_file_and_retires_on_navigation(
    cx: &mut TestAppContext,
) {
    let (shell, cx) = setup(cx);
    cx.simulate_resize(gpui::size(px(390.0), px(650.0)));
    const FIRST: &str = "apps/backend/config/development/.env.local";
    const SECOND: &str = "apps/worker/.env.test";
    shell.update(cx, |shell, cx| {
        shell
            .worktree_settings
            .editor
            .open_mut()
            .unwrap()
            .fields
            .as_mut()
            .unwrap()
            .settings
            .env_files
            .push(WorktreeEnvFile {
                path: SECOND.into(),
                mode: Env::Link,
            });
        cx.notify();
    });
    redraw(cx);
    open_page(&shell, cx, SettingsPage::Environment);

    for (option, mode) in [
        ("env-mode-0-option-0", Env::Copy),
        ("env-mode-0-option-1", Env::Link),
        ("env-mode-0-option-2", Env::Follow),
        ("env-mode-0-option-3", Env::Skip),
    ] {
        let trigger = bounds(cx, "env-mode-0");
        cx.simulate_click(trigger.center(), gpui::Modifiers::default());
        redraw(cx);
        assert_modal_dropdown_geometry(cx, trigger, px(260.0));
        shell.read_with(cx, |shell, _| {
            assert!(shell.worktree_settings.env_modes[FIRST].is_open());
        });
        let option = bounds(cx, option);
        cx.simulate_click(option.center(), gpui::Modifiers::default());
        redraw(cx);
        shell.read_with(cx, |shell, _| {
            let editor = shell.worktree_settings.editor.as_open().unwrap();
            let files = &editor.fields.as_ref().unwrap().settings.env_files;
            assert_eq!(files.len(), 2);
            assert_eq!(files[0].path, FIRST);
            assert_eq!(files[0].mode, mode);
            assert_eq!(files[1].path, SECOND);
            assert_eq!(files[1].mode, Env::Link);
            assert!(!shell.worktree_settings.env_modes[FIRST].is_open());
            assert!(!shell.worktree_settings.discard.is_open());
        });
        settle_popup_exit(cx);
    }

    let unchanged = vec![
        WorktreeEnvFile {
            path: FIRST.into(),
            mode: Env::Skip,
        },
        WorktreeEnvFile {
            path: SECOND.into(),
            mode: Env::Link,
        },
    ];
    let trigger = bounds(cx, "env-mode-0");
    cx.simulate_click(trigger.center(), gpui::Modifiers::default());
    redraw(cx);
    cx.simulate_keystrokes("escape");
    redraw(cx);
    shell.read_with(cx, |shell, _| {
        assert!(shell.worktree_settings.editor.is_open());
        assert!(!shell.worktree_settings.discard.is_open());
        assert!(!shell.worktree_settings.env_modes[FIRST].is_open());
        assert_eq!(
            shell
                .worktree_settings
                .editor
                .as_open()
                .unwrap()
                .fields
                .as_ref()
                .unwrap()
                .settings
                .env_files,
            unchanged
        );
    });
    settle_popup_exit(cx);

    let trigger = bounds(cx, "env-mode-0");
    cx.simulate_click(trigger.center(), gpui::Modifiers::default());
    redraw(cx);
    shell.update(cx, |shell, cx| {
        assert!(shell.worktree_settings.env_modes[FIRST].is_open());
        shell.select_worktree_settings_page(SettingsPage::Workflow, cx);
        assert!(shell.worktree_settings.env_modes.is_empty());
        assert_eq!(
            shell
                .worktree_settings
                .editor
                .as_open()
                .unwrap()
                .fields
                .as_ref()
                .unwrap()
                .settings
                .env_files,
            unchanged
        );
    });
    redraw(cx);
    assert!(cx.debug_bounds("env-mode-0-option-0").is_none());

    open_page(&shell, cx, SettingsPage::Environment);
    let trigger = bounds(cx, "env-mode-0");
    cx.simulate_click(trigger.center(), gpui::Modifiers::default());
    redraw(cx);
    shell.update(cx, |shell, cx| {
        assert!(shell.worktree_settings.env_modes[FIRST].is_open());
        shell.select_worktree_settings(Some("/project-worktree".into()), cx);
        assert!(shell.worktree_settings.env_modes.is_empty());
        let editor = shell.worktree_settings.editor.as_open().unwrap();
        assert_eq!(editor.drafts[&None].settings.env_files, unchanged);
        shell.select_worktree_settings(None, cx);
        assert!(shell.worktree_settings.env_modes.is_empty());
        assert_eq!(
            shell
                .worktree_settings
                .editor
                .as_open()
                .unwrap()
                .fields
                .as_ref()
                .unwrap()
                .settings
                .env_files,
            unchanged
        );
    });
    redraw(cx);
    assert!(cx.debug_bounds("env-mode-0-option-0").is_none());
}

#[gpui::test]
fn project_settings_category_switches_keep_inputs_and_checkout_drafts(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    shell.update(cx, |shell, cx| {
        let editor = shell.worktree_settings.editor.open_mut().unwrap();
        let fields = editor.fields.as_ref().unwrap();
        fields
            .prefix
            .update(cx, |input, cx| input.set_text("feature/draft/", cx));
        fields
            .setup
            .update(cx, |input, cx| input.set_text("bun run prepare", cx));
        fields
            .variables
            .update(cx, |input, cx| input.set_text("APP_PORT=4100", cx));
        let inputs = [
            fields.prefix.entity_id(),
            fields.setup.entity_id(),
            fields.variables.entity_id(),
        ];
        let epoch = shell.worktree_settings.epoch;

        for (page, _) in PAGES.into_iter().skip(1).chain(std::iter::once(PAGES[0])) {
            shell.worktree_settings.dependencies.open(0);
            shell.worktree_settings.services.open(0);
            shell.worktree_settings.directory.open(0);
            shell
                .worktree_settings
                .editor
                .open_mut()
                .unwrap()
                .scroll
                .set_offset(gpui::point(px(0.0), px(-80.0)));
            // A different category closes menus and starts at its first field.
            shell.select_worktree_settings_page(page, cx);
            assert!(!shell.worktree_settings.dependencies.is_open());
            assert!(!shell.worktree_settings.services.is_open());
            assert!(!shell.worktree_settings.directory.is_open());
            let editor = shell.worktree_settings.editor.as_open().unwrap();
            assert_eq!(editor.scroll.offset(), gpui::point(px(0.0), px(0.0)));
            let fields = editor.fields.as_ref().unwrap();
            assert_eq!(
                [
                    fields.prefix.entity_id(),
                    fields.setup.entity_id(),
                    fields.variables.entity_id()
                ],
                inputs
            );
            assert_eq!(fields.prefix.read(cx).text(), "feature/draft/");
            assert_eq!(fields.setup.read(cx).text(), "bun run prepare");
            assert_eq!(fields.variables.read(cx).text(), "APP_PORT=4100");
            assert!(editor.is_dirty(cx));
            assert_eq!(
                shell.worktree_settings.epoch, epoch,
                "category navigation must not restart polling"
            );
        }

        shell.select_worktree_settings(Some("/project-worktree".into()), cx);
        shell.select_worktree_settings_page(SettingsPage::Advanced, cx);
        assert!(
            shell
                .worktree_settings
                .editor
                .as_open()
                .unwrap()
                .drafts
                .contains_key(&None)
        );
        shell.select_worktree_settings(None, cx);
        let fields = shell
            .worktree_settings
            .editor
            .as_open()
            .unwrap()
            .fields
            .as_ref()
            .unwrap();
        assert_eq!(
            [
                fields.prefix.entity_id(),
                fields.setup.entity_id(),
                fields.variables.entity_id()
            ],
            inputs
        );
        assert_eq!(fields.prefix.read(cx).text(), "feature/draft/");
        assert!(shell.worktree_settings_dirty(cx));
    });
}

#[gpui::test]
fn project_settings_clean_checkout_cannot_save_another_checkouts_draft(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    open_page(&shell, cx, SettingsPage::Environment);
    let defaults_prefix = shell.update(cx, |shell, cx| {
        let editor = shell.worktree_settings.editor.open_mut().unwrap();
        let prefix = editor.fields.as_ref().unwrap().prefix.clone();
        prefix.update(cx, |input, cx| {
            input.set_text("feature/defaults-draft/", cx)
        });
        let mut snapshot = editor.snapshot.clone().unwrap();
        snapshot.checkout = Some("/project-worktree".into());
        snapshot.has_overrides = false;
        shell.select_worktree_settings(Some("/project-worktree".into()), cx);
        assert!(shell.accept_worktree_settings_poll(Ok(snapshot), cx));
        let editor = shell.worktree_settings.editor.as_open().unwrap();
        assert!(!editor.fields.as_ref().unwrap().is_dirty(cx));
        assert!(editor.is_dirty(cx));
        prefix.entity_id()
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (out, mut requests) = tokio::sync::mpsc::channel(16);
    let (_replies, inbound) = tokio::sync::mpsc::channel(16);
    let engine = {
        let _guard = runtime.enter();
        crate::state::EngineHandle::from_test_client(zeron_rpc::RpcClient::new(out, inbound))
    };
    shell.update(cx, |shell, cx| {
        shell
            .state
            .update(cx, |state, _| state.set_test_engine(engine));
    });
    redraw(cx);
    let save = bounds(cx, "save-worktree-settings");
    cx.simulate_click(save.center(), gpui::Modifiers::default());
    redraw(cx);
    assert!(
        matches!(
            requests.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "Save must not dispatch a request for an unchanged checkout"
    );
    shell.read_with(cx, |shell, cx| {
        let editor = shell.worktree_settings.editor.as_open().unwrap();
        assert!(!editor.busy);
        assert!(editor.error.is_none());
        assert!(!editor.fields.as_ref().unwrap().is_dirty(cx));
        assert!(shell.worktree_settings_dirty(cx));
    });

    let back = bounds(cx, "project-settings-back");
    cx.simulate_click(back.center(), gpui::Modifiers::default());
    redraw(cx);
    shell.read_with(cx, |shell, cx| {
        assert!(shell.worktree_settings.page == SettingsPage::General);
        let editor = shell.worktree_settings.editor.as_open().unwrap();
        let defaults = &editor.drafts[&None];
        assert_eq!(defaults.prefix.entity_id(), defaults_prefix);
        assert_eq!(defaults.prefix.read(cx).text(), "feature/defaults-draft/");
        assert!(defaults.is_dirty(cx));
        assert!(editor.is_dirty(cx));
    });
    let close = bounds(cx, "close-worktree-settings");
    cx.simulate_click(close.center(), gpui::Modifiers::default());
    redraw(cx);
    shell.read_with(cx, |shell, _| {
        assert!(shell.worktree_settings.editor.is_open());
        assert!(shell.worktree_settings.discard.is_open());
    });
}

#[gpui::test]
fn project_settings_blank_follow_profile_remains_editable_after_the_last_follow_rule_changes(
    cx: &mut TestAppContext,
) {
    let (shell, cx) = setup(cx);
    cx.simulate_resize(gpui::size(px(390.0), px(650.0)));
    let profile = shell.update(cx, |shell, cx| {
        let fields = shell
            .worktree_settings
            .editor
            .open_mut()
            .unwrap()
            .fields
            .as_mut()
            .unwrap();
        fields.settings.env_files[0].mode = Env::Follow;
        fields
            .profile
            .update(cx, |input, cx| input.set_text("", cx));
        let profile = fields.profile.clone();
        cx.notify();
        profile
    });
    redraw(cx);
    open_page(&shell, cx, SettingsPage::Environment);
    assert!(cx.debug_bounds("project-settings-follow-profile").is_some());
    let trigger = bounds(cx, "env-mode-0");
    cx.simulate_click(trigger.center(), gpui::Modifiers::default());
    redraw(cx);
    let copy = bounds(cx, "env-mode-0-option-0");
    cx.simulate_click(copy.center(), gpui::Modifiers::default());
    redraw(cx);
    let profile_bounds = bounds(cx, "project-settings-follow-profile");
    contains(
        bounds(cx, page_selector(SettingsPage::Environment)),
        profile_bounds,
        "blank Follow profile input",
    );
    let focus = profile.read_with(cx, |input, cx| input.focus_handle(cx));
    cx.update(|window, cx| window.focus(&focus, cx));
    cx.simulate_input("s");
    redraw(cx);
    assert!(cx.debug_bounds("project-settings-follow-profile").is_some());
    cx.update(|window, _| assert!(focus.is_focused(window)));
    cx.simulate_input("hared");
    redraw(cx);
    assert!(cx.debug_bounds("project-settings-follow-profile").is_some());
    shell.read_with(cx, |shell, cx| {
        let fields = shell
            .worktree_settings
            .editor
            .as_open()
            .unwrap()
            .fields
            .as_ref()
            .unwrap();
        assert!(
            fields
                .settings
                .env_files
                .iter()
                .all(|file| file.mode != Env::Follow)
        );
        assert_eq!(fields.settings.env_files[0].mode, Env::Copy);
        assert_eq!(fields.profile.entity_id(), profile.entity_id());
        assert_eq!(fields.profile.read(cx).text(), "shared");
        assert_eq!(fields.settings(cx).unwrap().env_profile, "shared");
    });
}

#[gpui::test]
fn project_settings_busy_save_and_dirty_close_keep_the_form_safe(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    shell.update(cx, |shell, cx| {
        let editor = shell.worktree_settings.editor.open_mut().unwrap();
        editor.busy = true;
        editor
            .fields
            .as_ref()
            .unwrap()
            .timeout
            .update(cx, |input, cx| input.set_text("invalid timeout", cx));
        cx.notify();
    });
    redraw(cx);
    let save = bounds(cx, "save-worktree-settings").center();
    cx.simulate_click(save, gpui::Modifiers::default());
    shell.read_with(cx, |shell, _| {
        let editor = shell.worktree_settings.editor.as_open().unwrap();
        assert!(editor.busy);
        assert!(
            editor.error.is_none(),
            "a disabled save must not validate or dispatch again"
        );
    });

    let close = bounds(cx, "close-worktree-settings").center();
    cx.simulate_click(close, gpui::Modifiers::default());
    shell.read_with(cx, |shell, _| {
        assert!(shell.worktree_settings.editor.is_open());
        assert!(shell.worktree_settings.discard.is_open());
    });
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            assert!(shell.keep_editing(UnsavedForm::WorktreeSettings, window, cx));
            assert_eq!(
                shell
                    .worktree_settings
                    .editor
                    .as_open()
                    .unwrap()
                    .fields
                    .as_ref()
                    .unwrap()
                    .timeout
                    .read(cx)
                    .text(),
                "invalid timeout"
            );
            shell.discard_form(UnsavedForm::WorktreeSettings, window, cx);
            assert!(!shell.worktree_settings.editor.is_open());
            assert!(shell.worktree_settings.editor.is_closing());
            assert!(
                shell.worktree_settings.editor.get().is_some(),
                "the shared popup lifecycle retains the exit frame"
            );
        });
    });
}

#[gpui::test]
fn project_settings_overview_and_back_support_keyboard_navigation(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    cx.simulate_resize(gpui::size(px(1100.0), px(800.0)));
    redraw(cx);
    let first_row = shell.read_with(cx, |shell, _| {
        shell.worktree_settings.navigation_focus.as_ref().unwrap()
            [SettingsPage::Environment as usize]
            .clone()
    });
    cx.update(|window, cx| window.focus(&first_row, cx));
    for (key, page) in [
        ("tab", SettingsPage::Workflow),
        ("tab", SettingsPage::Services),
        ("tab", SettingsPage::Advanced),
        ("shift-tab", SettingsPage::Services),
        ("shift-tab", SettingsPage::Workflow),
        ("shift-tab", SettingsPage::Environment),
    ] {
        cx.simulate_keystrokes(key);
        redraw(cx);
        cx.update(|window, cx| {
            shell.read_with(cx, |shell, _| {
                assert!(shell.worktree_settings.page == SettingsPage::General);
                assert!(
                    shell.worktree_settings.navigation_focus.as_ref().unwrap()[page as usize]
                        .is_focused(window),
                    "{key} should traverse to {}: actual {:?}, row handles {:?}",
                    page.key(),
                    window.focused(cx),
                    shell.worktree_settings.navigation_focus.as_ref().unwrap()
                );
            });
        });
    }
    cx.simulate_resize(gpui::size(px(390.0), px(650.0)));
    redraw(cx);
    for ((page, _), key) in PAGES
        .into_iter()
        .skip(1)
        .zip(["enter", "space", "enter", "space"])
    {
        reveal_in_settings(&shell, cx, overview_selector(page));
        let focus = shell.read_with(cx, |shell, _| {
            shell.worktree_settings.navigation_focus.as_ref().unwrap()[page as usize].clone()
        });
        cx.update(|window, cx| window.focus(&focus, cx));
        cx.simulate_keystrokes(key);
        redraw(cx);
        cx.update(|window, cx| {
            shell.read_with(cx, |shell, _| {
                assert!(
                    shell.worktree_settings.page == page,
                    "{key} did not open the section"
                );
                assert!(
                    shell.worktree_settings.navigation_focus.as_ref().unwrap()
                        [SettingsPage::General as usize]
                        .is_focused(window)
                );
            });
        });
        assert!(cx.debug_bounds(page_selector(page)).is_some());
        cx.simulate_keystrokes(key);
        redraw(cx);
        cx.update(|window, cx| {
            shell.read_with(cx, |shell, _| {
                assert!(shell.worktree_settings.page == SettingsPage::General);
                assert!(
                    shell.worktree_settings.navigation_focus.as_ref().unwrap()[page as usize]
                        .is_focused(window),
                    "back should restore the originating row"
                );
            });
        });
        assert!(cx.debug_bounds(overview_selector(page)).is_some());
        assert!(cx.debug_bounds("project-settings-back").is_none());
    }

    let input_focus = shell.read_with(cx, |shell, cx| {
        shell
            .worktree_settings
            .editor
            .as_open()
            .unwrap()
            .fields
            .as_ref()
            .unwrap()
            .prefix
            .focus_handle(cx)
    });
    cx.update(|window, cx| window.focus(&input_focus, cx));
    cx.simulate_keystrokes("right");
    shell.read_with(cx, |shell, _| {
        assert!(shell.worktree_settings.page == SettingsPage::General);
    });
}

#[gpui::test]
fn project_settings_without_worktrees_uses_the_full_content_width(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    shell.update(cx, |shell, cx| {
        shell
            .worktree_settings
            .editor
            .open_mut()
            .unwrap()
            .snapshot
            .as_mut()
            .unwrap()
            .worktrees
            .clear();
        cx.notify();
    });
    for (width, height) in [(1100.0, 800.0), (390.0, 650.0)] {
        cx.simulate_resize(gpui::size(px(width), px(height)));
        redraw(cx);
        assert!(
            cx.debug_bounds("project-settings-navigation").is_none(),
            "empty scope navigation should not take up space"
        );
        let card = bounds(cx, "project-settings-card");
        let body = bounds(cx, "project-settings-body");
        let scroll = bounds(cx, "worktree-environment-scroll");
        contains(card, body, "settings content");
        assert!(
            f32::from(scroll.size.width - body.size.width).abs() <= 2.0,
            "the form should use the full content width"
        );
        let footer = bounds(cx, "project-settings-footer");
        for selector in ["close-worktree-settings", "save-worktree-settings"] {
            contains(footer, bounds(cx, selector), selector);
        }
    }
}

#[gpui::test]
fn project_settings_database_fields_remain_usable_in_compact_windows(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    shell.update(cx, |shell, cx| {
        let database = ProjectTerminalService {
            id: "db".into(),
            name: "Development PostgreSQL database".into(),
            command: "podman compose up database".into(),
            directory: "apps/backend".into(),
            restart_on_failure: false,
            setup_prerequisite: Some(zeron_proto::ProjectServiceReadiness {
                command: "pg_isready".into(),
                timeout_seconds: 120,
            }),
        };
        let settings = shell
            .worktree_settings
            .editor
            .as_open()
            .unwrap()
            .snapshot
            .as_ref()
            .unwrap()
            .settings
            .clone();
        let fields = shell.worktree_fields(settings, vec![database], cx);
        shell.worktree_settings.editor.open_mut().unwrap().fields = Some(fields);
        shell.select_worktree_settings_page(SettingsPage::Services, cx);
    });
    cx.simulate_resize(gpui::size(px(390.0), px(650.0)));
    redraw(cx);
    let panel = bounds(cx, page_selector(SettingsPage::Services));
    let name = bounds(cx, "project-settings-database-name-db");
    let directory = bounds(cx, "project-settings-database-directory-db");
    for (label, field) in [("database name", name), ("database directory", directory)] {
        assert!(
            field.size.width >= px(100.0),
            "{label} is squeezed to an unusable width"
        );
        assert!(field.left() >= panel.left());
        assert!(field.right() <= panel.right() + px(1.0));
    }
    assert!(
        name.bottom() <= directory.top(),
        "database fields should stack when the row cannot fit"
    );
}

#[gpui::test]
fn project_settings_exit_keeps_the_current_animation_identity(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    shell.update(cx, |shell, cx| {
        let editor = shell.worktree_settings.editor.open_mut().unwrap();
        let shown = editor.page_identity(SettingsPage::General);
        assert_ne!(shown, editor.page_identity(SettingsPage::Environment));
        editor.checkout = Some("/project-worktree".into());
        assert_ne!(shown, editor.page_identity(SettingsPage::General));
        editor.checkout = None;
        let fields = editor.fields.take();
        assert_ne!(shown, editor.page_identity(SettingsPage::General));
        editor.fields = fields;
        let snapshot = editor.snapshot.clone().unwrap();
        assert!(!shell.accept_worktree_settings_poll(Ok(snapshot), cx));

        let editor = shell.worktree_settings.editor.open_mut().unwrap();
        editor
            .fields
            .as_ref()
            .unwrap()
            .prefix
            .update(cx, |input, cx| input.set_text("feature/draft/", cx));
        assert!(editor.is_dirty(cx));
        assert_eq!(
            shown,
            editor.page_identity(SettingsPage::General),
            "editing should not replay the page entrance"
        );
        let epoch = shell.worktree_settings.epoch;
        shell.close_worktree_settings(cx);
        assert!(shell.worktree_settings.epoch > epoch);
        assert!(shell.worktree_settings.editor.is_closing());
        let exiting = shell.worktree_settings.editor.get().unwrap();
        assert_eq!(
            shown,
            exiting.page_identity(SettingsPage::General),
            "invalidating requests must not hide the page during the modal exit"
        );
    });
}

#[gpui::test]
fn closing_clean_project_settings_restores_focus_before_reopening(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    open_page(&shell, cx, SettingsPage::Environment);
    let old_navigation_focus = shell.read_with(cx, |shell, cx| {
        assert!(!shell.worktree_settings_dirty(cx));
        shell.worktree_settings.navigation_focus.as_ref().unwrap()[SettingsPage::General as usize]
            .clone()
    });
    cx.update(|window, _| assert!(old_navigation_focus.is_focused(window)));
    let close = bounds(cx, "close-worktree-settings");
    cx.simulate_click(close.center(), gpui::Modifiers::default());
    cx.update(|window, cx| {
        shell.read_with(cx, |shell, _| {
            assert!(shell.worktree_settings.editor.is_closing());
            assert!(!shell.worktree_settings.discard.is_open());
            assert!(shell.shortcut_focus.is_focused(window));
            assert!(!old_navigation_focus.is_focused(window));
        });
    });

    shell.update(cx, |shell, cx| {
        shell.open_worktree_settings("project".into(), cx)
    });
    redraw(cx);
    cx.update(|window, cx| {
        shell.read_with(cx, |shell, _| {
            assert!(shell.worktree_settings.editor.is_open());
            assert!(shell.worktree_settings.page == SettingsPage::General);
            assert!(shell.shortcut_focus.is_focused(window));
            assert!(!old_navigation_focus.is_focused(window));
        });
    });
    cx.simulate_keystrokes("enter");
    shell.read_with(cx, |shell, _| {
        assert!(
            shell.worktree_settings.page == SettingsPage::General,
            "an old navigation target must not receive keys after reopening"
        );
    });
}
