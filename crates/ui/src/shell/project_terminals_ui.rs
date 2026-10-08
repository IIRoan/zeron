use super::*;
use zeron_proto::{
    ProjectTerminalConfig, ProjectTerminalService, ProjectTerminalStatus as Status,
    ProjectTerminalsSnapshot,
};
use zeron_rpc::methods;

pub(super) const TERMINAL_TOOLBAR_HEIGHT: f32 = 32.0;
const SERVICE_BAR_HEIGHT: f32 = 28.0;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum TerminalMode {
    #[default]
    Services,
    Shells,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ProjectContext {
    pub(super) space: String,
    pub(super) device: String,
    pub(super) name: String,
    pub(super) target: Option<String>,
    pub(super) chat: Option<String>,
    pub(super) cwd: String,
    pub(super) checkout_override: Option<String>,
}

impl ProjectContext {
    fn scope(&self) -> String {
        format!("project-services:{}:{}:{}", self.device, self.space, self.cwd)
    }
    pub(super) fn params(&self, mut value: serde_json::Value) -> serde_json::Value {
        value["spaceId"] = self.space.clone().into();
        if let Some(chat) = &self.chat {
            value["chatId"] = chat.clone().into();
        }
        if let Some(path) = &self.checkout_override {value["checkoutPath"] = path.clone().into();}
        if let Some(target) = &self.target {
            value["targetDeviceId"] = target.clone().into();
        }
        value
    }
}

struct ServiceFields {
    id: String,
    name: Entity<ComposerInput>,
    command: Entity<ComposerInput>,
    directory: Entity<ComposerInput>,
    restart: bool,
    _events: Vec<Subscription>,
}

pub(super) struct Editor {
    project: ProjectContext,
    services: Vec<ServiceFields>,
    selected: Option<String>,
    initialized: bool,
    scroll: gpui::ScrollHandle,
}

#[derive(Default)]
pub(super) struct ProjectTerminalUi {
    pub drawer: bool,
    pub mode: TerminalMode,
    pub editor: Option<Editor>,
    pub panel: Option<Entity<TerminalPanel>>,
    context: Option<ProjectContext>,
    snapshot: Option<ProjectTerminalsSnapshot>,
    tabs: std::collections::HashMap<(String, String), (u64, Option<String>)>,
    selected: std::collections::HashMap<String, String>,
    pub actions_menu: popover::Popup<()>,
    bar_width: std::rc::Rc<std::cell::Cell<f32>>,
    output_height: std::rc::Rc<std::cell::Cell<f32>>,
    poll: Option<Task<()>>,
    busy: bool,
    pending_action: Option<&'static str>,
    pending_service: Option<String>,
    content_started: Option<std::time::Instant>,
    operation_error: Option<String>,
    epoch: u64,
    error: Option<String>,
    handoff: Option<Task<()>>,
    last_activation: Option<ProjectContext>,
    handoff_error: Option<String>,
    was_active: bool,
}

impl Shell {
    pub(super) fn terminal_project_context(
        &self,
        space: Option<&str>,
        cx: &App,
    ) -> Option<ProjectContext> {
        let state = self.state.read(cx);
        if space.is_none() && state.selected_chat.is_none() {
            return None;
        }
        let chat = space.is_none().then(|| state.selected_chat_row()).flatten();
        // A selected agent can arrive before its synced row. Wait for it rather
        // than briefly moving running services back to the main checkout.
        if space.is_none() && state.selected_chat.is_some() && chat.is_none() {
            return None;
        }
        let project = space
            .and_then(|id| state.space_row(id))
            .or_else(|| chat.and_then(|chat| state.space_for_chat(chat)))
            .or_else(|| state.selected_space_row())?;
        Some(ProjectContext {
            checkout_override: None,
            space: project.id.clone(),
            device: project.device_id.clone(),
            name: project.display_name().to_string(),
            target: (state.local_device_id.as_deref() != Some(project.device_id.as_str()))
                .then(|| project.device_id.clone()),
            chat: chat
                .filter(|chat| chat.space_id.as_deref() == Some(project.id.as_str()))
                .map(|chat| chat.id.clone()),
            cwd: chat
                .and_then(|chat| chat.cwd.clone())
                .unwrap_or_else(|| project.path.clone()),
        })
    }

    /// Only foreground selection/focus changes can move services. Background
    /// status polls and the project settings editor never claim a checkout.
    pub(super) fn follow_project_terminal_checkout(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let active = window.is_window_active();
        if active && !self.project_terminals.was_active {
            self.project_terminals.last_activation = None;
        }
        self.project_terminals.was_active = active;
        if !active
            || !matches!(self.route, Route::Chat)
            || self.project_terminals.handoff.is_some()
            || self.project_terminals.busy
        {
            return;
        }
        let Some(context) = self.terminal_project_context(None, cx) else {
            return;
        };
        if self.project_terminals.last_activation.as_ref() == Some(&context) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.project_terminals.last_activation = Some(context.clone());
        self.project_terminals.handoff_error = None;
        self.project_terminals.poll = None;
        self.project_terminals.handoff = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<ProjectTerminalsSnapshot>(
                    methods::CONTROL_PROJECT_TERMINALS,
                    context.params(serde_json::json!({ "action": "activate" })),
                )
                .await;
            this.update(cx, |shell, cx| {
                shell.project_terminals.handoff = None;
                if shell.terminal_project_context(None, cx).as_ref() == Some(&context) {
                    match result {
                        Ok(snapshot) => {
                            if shell.project_terminals.context.as_ref() == Some(&context) {
                                shell.accept_project_terminal_snapshot(snapshot, cx);
                            }
                        }
                        Err(error) => {
                            shell.project_terminals.handoff_error =
                                Some(format!("Could not switch project terminals: {error}"));
                            shell.select_terminal_mode(TerminalMode::Services, cx);
                        }
                    }
                }
                // Notify even for an obsolete response: the next render sends
                // only the latest selection, keeping rapid switches ordered.
                cx.notify();
            })
            .ok();
        }));
    }

    pub(super) fn select_terminal_mode(&mut self, mode: TerminalMode, cx: &mut Context<Self>) {
        if !self.session_workspace_visible(cx) {
            return;
        }
        if mode == TerminalMode::Services && self.terminal_project_context(None, cx).is_none() {
            return;
        }
        let was_open = self.terminal_open(cx);
        let from = self.terminal_geometry.get().height;
        if !was_open || self.project_terminals.mode != mode {
            self.project_terminals.content_started = Some(std::time::Instant::now());
        }
        self.project_terminals.mode = mode;
        self.project_terminals.actions_menu = popover::Popup::default();
        self.project_terminals.drawer = mode == TerminalMode::Services;
        let key = self.panel_key(cx);
        self.panels
            .update(&key, |panels| panels.terminal_open = true);
        if mode == TerminalMode::Shells {
            self.composer
                .update(cx, |composer, _| composer.focus_pending = false);
            self.terminal_panel(cx).update(cx, |panel, cx| {
                panel.set_open(true, cx);
                panel.request_focus(cx);
            });
        } else {
            if let Some(panel) = &self.terminal {
                panel.update(cx, |panel, cx| panel.set_open(false, cx));
            }
            self.ensure_project_terminals(cx);
            if let Some(panel) = &self.project_terminals.panel {
                panel.update(cx, |panel, cx| panel.request_focus(cx));
            }
        }
        if !was_open {
            self.animate_terminal_dock(from, cx);
        }
        cx.notify();
    }

    pub(super) fn hide_terminal_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.project_terminals.actions_menu = popover::Popup::default();
        let from = self.terminal_geometry.get().height;
        self.project_terminals.drawer = false;
        let key = self.panel_key(cx);
        self.panels
            .update(&key, |panels| panels.terminal_open = false);
        if let Some(panel) = &self.terminal {
            panel.update(cx, |panel, cx| panel.set_open(false, cx));
        }
        window.focus(&self.composer.focus_handle(cx), cx);
        self.animate_terminal_dock(from, cx);
        cx.notify();
    }

    fn animate_terminal_dock(&mut self, from: f32, cx: &mut Context<Self>) {
        self.terminal_tween_task = None;
        if self.reduced_motion {
            self.terminal_tween = None;
            return;
        }
        self.terminal_tween = Some(WidthTween::new(from, self.terminal_target(cx)));
        self.terminal_tween_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(RESIZE.total().mul_f32(motion::speed_scale()) + Duration::from_millis(30))
                .await;
            this.update(cx, |shell, cx| {
                shell.terminal_tween = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Event-driven, so status polls and editor remounts never replay the fade.
    pub(super) fn terminal_content_opacity(&self) -> f32 {
        let Some(started) = self.project_terminals.content_started else {
            return 1.0;
        };
        if self.reduced_motion {
            return 1.0;
        }
        let raw = self.tween_elapsed(started).as_secs_f32()
            / motion::FADE_QUICK
                .total()
                .mul_f32(motion::speed_scale())
                .as_secs_f32();
        if raw >= 1.0 {
            return 1.0;
        }
        self.motion_active.set(true);
        motion::lerp(0.65, 1.0, motion::FADE_QUICK.progress(raw))
    }

    pub(super) fn open_project_terminal_settings(&mut self, space: String, cx: &mut Context<Self>) {
        self.project_terminals.actions_menu = popover::Popup::default();
        let Some(context) = self.terminal_project_context(Some(&space), cx) else {
            return;
        };
        self.open_service_settings_context(context,cx);
    }

    pub(super) fn open_service_settings_context(&mut self, context: ProjectContext, cx: &mut Context<Self>) {
        self.project_terminals.editor = Some(Editor {
            project: context,
            services: Vec::new(),
            selected: None,
            initialized: false,
            scroll: gpui::ScrollHandle::new(),
        });
        self.project_terminals.poll = None;
        self.ensure_project_terminals(cx);
        cx.notify();
    }

    pub(super) fn ensure_project_terminals(&mut self, cx: &mut Context<Self>) {
        if !self.project_terminals.drawer && self.project_terminals.editor.is_none() {
            self.project_terminals.poll = None;
            return;
        }
        let context = self
            .project_terminals
            .editor
            .as_ref()
            .map(|editor| editor.project.clone())
            .or_else(|| self.terminal_project_context(None, cx));
        let Some(context) = context else {
            return;
        };
        if self.project_terminals.context.as_ref() != Some(&context) {
            self.project_terminals.context = Some(context.clone());
            self.project_terminals.snapshot = None;
            self.project_terminals.error = None;
            self.project_terminals.operation_error = None;
            self.project_terminals.poll = None;
            self.project_terminals.epoch += 1;
        }
        if self.project_terminals.panel.is_none() {
            self.project_terminals.panel =
                Some(cx.new(|cx| TerminalPanel::new_embedded(self.state.clone(), cx)));
        }
        if let Some(panel) = &self.project_terminals.panel {
            panel.update(cx, |panel, cx| {
                panel.set_session_scope(context.scope(), cx);
                if !panel.is_open() {
                    panel.set_open(true, cx);
                }
            });
        }
        if self.project_terminals.poll.is_some()
            || self.project_terminals.busy
            || self.project_terminals.handoff.is_some()
        {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.project_terminals.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(epoch) = this.update(cx, |shell, _| shell.project_terminals.epoch) else {
                    return;
                };
                let result = engine
                    .client()
                    .call_as::<ProjectTerminalsSnapshot>(
                        methods::GET_PROJECT_TERMINALS,
                        context.params(serde_json::json!({})),
                    )
                    .await;
                let keep_polling = this
                    .update(cx, |shell, cx| {
                        if shell.project_terminals.context.as_ref() != Some(&context) {
                            return false;
                        }
                        if shell.project_terminals.epoch == epoch {
                            match result {
                                Ok(snapshot) => {
                                    shell.accept_project_terminal_snapshot(snapshot, cx)
                                }
                                Err(error) => {
                                    shell.project_terminals.error = Some(error.to_string());
                                    cx.notify();
                                }
                            }
                        }
                        shell.project_terminals.drawer || shell.project_terminals.editor.is_some()
                    })
                    .unwrap_or(false);
                if !keep_polling {
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        }));
    }

    fn service_fields(
        &self,
        service: ProjectTerminalService,
        cx: &mut Context<Self>,
    ) -> ServiceFields {
        let name = cx.new(|cx| ComposerInput::new("Service name", cx));
        let command = cx.new(|cx| ComposerInput::new("Command to run", cx));
        let directory = cx.new(|cx| ComposerInput::new("Directory relative to agent checkout", cx));
        name.update(cx, |input, cx| input.set_text(service.name, cx));
        command.update(cx, |input, cx| input.set_text(service.command, cx));
        directory.update(cx, |input, cx| input.set_text(service.directory, cx));
        let events = [&name, &command, &directory]
            .into_iter()
            .map(|input| {
                cx.subscribe(
                    input,
                    |_: &mut Shell, _, _: &crate::composer::ComposerInputEvent, cx| cx.notify(),
                )
            })
            .collect();
        ServiceFields {
            id: service.id,
            name,
            command,
            directory,
            restart: service.restart_on_failure,
            _events: events,
        }
    }

    fn accept_project_terminal_snapshot(
        &mut self,
        snapshot: ProjectTerminalsSnapshot,
        cx: &mut Context<Self>,
    ) {
        self.project_terminals.error = None;
        if self
            .project_terminals
            .editor
            .as_ref()
            .is_some_and(|editor| !editor.initialized)
        {
            let services = if snapshot.config.services.is_empty() {
                ["Backend", "Frontend", "Cloudflared"]
                    .into_iter()
                    .map(|name| ProjectTerminalService {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: name.into(),
                        command: String::new(),
                        directory: ".".into(),
                        restart_on_failure: false,
                    })
                    .collect()
            } else {
                snapshot.config.services.clone()
            };
            let fields = services
                .into_iter()
                .map(|service| self.service_fields(service, cx))
                .collect();
            if let Some(editor) = &mut self.project_terminals.editor {
                editor.services = fields;
                editor.selected = editor.services.first().map(|service| service.id.clone());
                editor.initialized = true;
            }
        }
        let Some(context) = self.project_terminals.context.clone() else {
            return;
        };
        let scope = context.scope();
        if let Some(panel) = self.project_terminals.panel.clone() {
            let removed = self
                .project_terminals
                .tabs
                .keys()
                .filter(|(key_scope, id)| {
                    key_scope == &scope && !snapshot.config.services.iter().any(|s| &s.id == id)
                })
                .cloned()
                .collect::<Vec<_>>();
            for key in removed {
                if let Some((tab, _)) = self.project_terminals.tabs.remove(&key) {
                    panel.update(cx, |panel, cx| panel.remove_service_view(&scope, tab, cx));
                }
            }
            for service in &snapshot.config.services {
                let map_key = (scope.clone(), service.id.clone());
                let entry = self
                    .project_terminals
                    .tabs
                    .entry(map_key)
                    .or_insert_with(|| {
                        let key = panel.update(cx, |panel, cx| {
                            panel.reserve_tab_for_chat(scope.clone(), service.name.clone(), cx)
                        });
                        (key, None)
                    });
                let tab = entry.0;
                panel.update(cx, |panel, cx| {
                    panel.set_service_title(&scope, tab, service.name.clone(), cx)
                });
                if snapshot.runs.iter().any(|run| {
                    run.service_id == service.id
                        && matches!(run.status, Status::Stopped | Status::Interrupted)
                }) {
                    panel.update(cx, |panel, _| panel.detach_service_view(&scope, tab));
                }
                if let Some(session) = snapshot
                    .runs
                    .iter()
                    .find(|run| run.service_id == service.id)
                    .and_then(|run| run.terminal.clone())
                    && entry.1.as_deref() != Some(session.id.as_str())
                    && !matches!(
                        snapshot
                            .runs
                            .iter()
                            .find(|r| r.service_id == service.id)
                            .map(|r| r.status),
                        Some(Status::Stopped | Status::Interrupted)
                    )
                {
                    let id = session.id.clone();
                    if panel.update(cx, |panel, cx| {
                        panel.attach_reserved_session(
                            &scope,
                            tab,
                            session,
                            context.target.clone(),
                            cx,
                        )
                    }) {
                        entry.1 = Some(id);
                    }
                }
            }
            let selected = self
                .project_terminals
                .selected
                .entry(scope.clone())
                .or_insert_with(|| {
                    snapshot
                        .config
                        .services
                        .first()
                        .map(|s| s.id.clone())
                        .unwrap_or_default()
                });
            if !snapshot.config.services.iter().any(|s| &s.id == selected) {
                *selected = snapshot
                    .config
                    .services
                    .first()
                    .map(|s| s.id.clone())
                    .unwrap_or_default();
            }
            if let Some((tab, _)) = self.project_terminals.tabs.get(&(scope, selected.clone())) {
                panel.update(cx, |panel, cx| panel.select_tab_by_key(*tab, cx));
            }
        }
        self.project_terminals.snapshot = Some(snapshot);
        cx.notify();
    }

    fn control_project_terminal(
        &mut self,
        service: Option<String>,
        action: &'static str,
        cx: &mut Context<Self>,
    ) {
        if self.project_terminals.busy || self.project_terminals.handoff.is_some() {
            return;
        }
        let Some(context) = self.project_terminals.context.clone() else {
            return;
        };
        if let Some(service) = &service {
            self.project_terminals
                .selected
                .insert(context.scope(), service.clone());
        }
        self.project_terminals.pending_action = Some(action);
        self.project_terminals.pending_service = service.clone();
        self.request_project_terminals(
            methods::CONTROL_PROJECT_TERMINALS,
            context.params(serde_json::json!({ "serviceId": service, "action": action })),
            cx,
        );
    }

    fn request_project_terminals(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let context = self.project_terminals.context.clone();
        self.project_terminals.busy = true;
        self.project_terminals.operation_error = None;
        self.project_terminals.poll = None;
        self.project_terminals.epoch += 1;
        let epoch = self.project_terminals.epoch;
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<ProjectTerminalsSnapshot>(method, params)
                .await;
            this.update(cx, |shell, cx| {
                // Finish the old request before handing off a newer selection.
                shell.project_terminals.busy = false;
                shell.project_terminals.pending_action = None;
                shell.project_terminals.pending_service = None;
                cx.notify();
                if shell.project_terminals.context != context
                    || shell.project_terminals.epoch != epoch
                {
                    return;
                }
                match result {
                    Ok(snapshot) => {
                        shell.project_terminals.handoff_error = None;
                        if method == methods::SAVE_PROJECT_TERMINALS {
                            if let Some(context) = &context {
                                shell.update_worktree_service_fields(context, snapshot.config.clone());
                            }
                            shell.project_terminals.editor = None;
                        }
                        shell.accept_project_terminal_snapshot(snapshot, cx);
                    }
                    Err(error) => {
                        tracing::warn!(%error, method, "project terminal action failed");
                        shell.project_terminals.operation_error = Some(error.to_string());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn save_project_terminal_settings(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.project_terminals.editor else {
            return;
        };
        let config = ProjectTerminalConfig {
            services: editor
                .services
                .iter()
                .filter(|fields| !fields.command.read(cx).text().trim().is_empty())
                .map(|fields| ProjectTerminalService {
                    id: fields.id.clone(),
                    name: fields.name.read(cx).text().to_string(),
                    command: fields.command.read(cx).text().to_string(),
                    directory: fields.directory.read(cx).text().to_string(),
                    restart_on_failure: fields.restart,
                })
                .collect(),
        };
        self.request_project_terminals(
            methods::SAVE_PROJECT_TERMINALS,
            editor
                .project
                .params(serde_json::json!({ "config": config })),
            cx,
        );
    }

    pub(super) fn render_terminal_toolbar(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let open = self.terminal_open(cx);
        let mode = self.project_terminals.mode;
        let project = self.terminal_project_context(None, cx);
        let mut header = div()
            .id("terminal-toolbar")
            .h(px(TERMINAL_TOOLBAR_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.0))
            .px(px(8.0))
            .bg(crate::terminal::view::terminal_panel_bg(&theme))
            .border_b_1()
            .border_color(theme.border);
        for (choice, id, label, hint) in [
            (
                TerminalMode::Services,
                "terminal-mode-services",
                "Services",
                "Project services · Shared servers and tunnels for the active checkout",
            ),
            (
                TerminalMode::Shells,
                "terminal-mode-shells",
                "Terminals",
                "Shell terminals · Independent terminals for this agent",
            ),
        ] {
            let enabled = choice == TerminalMode::Shells || project.is_some();
            let selected = open && mode == choice;
            let key = format!("{}-{id}", cx.entity_id());
            let progress = motion::state_t(&key, selected, motion::TAB_SLIDE, self.reduced_motion);
            header = header.child(
                popover::btn_ghost(&theme, label, key.clone())
                    .id(id)
                    .flex_none()
                    .text_size(px(12.0))
                    .px(px(9.0))
                    .py(px(4.0))
                    .role(gpui::Role::Tab)
                    .aria_label(hint)
                    .aria_toggled(if selected {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .tooltip(crate::settings::widgets::text_tooltip(hint))
                    .text_color(motion::mix(theme.text_muted, theme.text, progress))
                    .bg(motion::hover_blend(
                        &key,
                        theme.wash(0.08).opacity(progress),
                        theme.element_hover,
                    ))
                    .when(!enabled, |el| el.opacity(0.4).cursor_default())
                    .when(enabled, |el| {
                        el.on_click(cx.listener(move |this, _, _, cx| {
                            this.select_terminal_mode(choice, cx);
                        }))
                    }),
            );
        }
        let description = if self.project_terminals.handoff.is_some() {
            "Moving services to this checkout…".to_string()
        } else if let Some(action) = self.project_terminals.pending_action {
            let target = self
                .project_terminals
                .pending_service
                .as_ref()
                .and_then(|id| {
                    self.project_terminals
                        .snapshot
                        .as_ref()?
                        .config
                        .services
                        .iter()
                        .find(|s| &s.id == id)
                })
                .map(|s| s.name.as_str())
                .unwrap_or("all services");
            format!("{} {target}…", action_label(action))
        } else if mode == TerminalMode::Services {
            let name = project
                .as_ref()
                .map(|p| p.name.as_str())
                .unwrap_or("Select a project");
            match self
                .project_terminals
                .snapshot
                .as_ref()
                .filter(|_| self.project_terminals.context.as_ref() == project.as_ref())
            {
                Some(snapshot) if !snapshot.config.services.is_empty() => {
                    format!("{} · {name}", service_summary(snapshot))
                }
                _ => name.to_string(),
            }
        } else {
            "Agent terminals".into()
        };
        header = header.child(
            div()
                .id("terminal-context")
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .tooltip(crate::settings::widgets::text_tooltip(description.clone()))
                .child(SharedString::from(description)),
        );
        if open && mode == TerminalMode::Services {
            header = header.child(self.render_service_toolbar_actions(window, cx));
            header = header.child(
                service_icon_button(
                    &theme,
                    "services-settings".into(),
                    icons::SETTINGS_MINIMALISTIC,
                    "Worktree environment and services",
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.open_active_worktree_settings(cx);
                })),
            );
        }
        header
            .child(
                service_icon_button(
                    &theme,
                    "terminal-visibility".into(),
                    if open {
                        icons::ALT_ARROW_DOWN
                    } else {
                        icons::ALT_ARROW_UP
                    },
                    if open {
                        "Hide terminal panel · Ctrl+J"
                    } else {
                        "Show terminal panel · Ctrl+J"
                    },
                )
                .on_click(cx.listener(|this, _, window, cx| this.toggle_terminal(window, cx))),
            )
            .into_any_element()
    }

    pub(super) fn close_project_terminal_actions(&mut self, cx: &mut Context<Self>) {
        if self.project_terminals.actions_menu.begin_close() {
            popover::reap_popup(cx, |shell| &mut shell.project_terminals.actions_menu);
            cx.notify();
        }
    }

    fn render_service_toolbar_actions(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let busy = self.project_terminals.busy || self.project_terminals.handoff.is_some();
        let snapshot = self.project_terminals.snapshot.as_ref();
        let configured = snapshot.is_some_and(|s| !s.config.services.is_empty());
        let running = snapshot.is_some_and(|s| {
            s.config
                .services
                .iter()
                .any(|service| service_is_active(service_status(s, &service.id)))
        });
        let mut actions = div().flex_none().flex().items_center().gap(px(2.0));
        let selected = self
            .project_terminals
            .context
            .as_ref()
            .and_then(|context| self.project_terminals.selected.get(&context.scope()));
        if let Some(service) = snapshot.and_then(|s| {
            s.config
                .services
                .iter()
                .find(|service| Some(&service.id) == selected)
        }) {
            let status = service_status(snapshot.unwrap(), &service.id);
            for (action, glyph, label) in [
                if service_is_active(status) {
                    ("stop", icons::STOP, "Stop")
                } else {
                    ("start", icons::ACTION_PLAY, "Start")
                },
                ("restart", icons::RESTART, "Restart"),
            ] {
                let id = service.id.clone();
                actions = actions.child(
                    service_icon_button(
                        &theme,
                        format!("service-{action}"),
                        glyph,
                        format!("{label} {}", service.name),
                    )
                    .when(busy, |el| el.opacity(0.4).cursor_default())
                    .when(!busy, |el| {
                        el.on_click(cx.listener(move |this, _, _, cx| {
                            this.control_project_terminal(Some(id.clone()), action, cx);
                        }))
                    }),
                );
            }
        }
        let mut more = popover::btn_ghost(&theme, "", "project-services-more")
            .id("services-more")
            .size(px(24.0))
            .p_0()
            .flex_none()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .role(gpui::Role::Button)
            .aria_label("All service actions")
            .tooltip(crate::settings::widgets::text_tooltip(
                "All service actions",
            ))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.project_terminals.actions_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                if this.project_terminals.actions_menu.take_press_was_open() {
                    this.close_project_terminal_actions(cx);
                } else {
                    this.project_terminals.actions_menu.open(());
                    cx.notify();
                }
            }));
        if busy {
            more = more.child(loaders::mini_mono_spinner(
                "project-service-progress",
                2.5,
                theme.text_muted,
                cx.entity_id(),
                cx,
            ));
        } else {
            more = more.child(
                icon(icons::VSC_MORE)
                    .size(px(15.0))
                    .text_color(theme.text_muted),
            );
        }
        if self.project_terminals.actions_menu.get().is_some() {
            let popup_theme = theme.for_popup();
            let mut menu = popover::popover_card(&popup_theme)
                .id("service-actions-menu")
                .w(px(220.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .on_mouse_down_out(
                    cx.listener(|this, _, _, cx| this.close_project_terminal_actions(cx)),
                )
                .on_click(|_, _, cx| cx.stop_propagation());
            let closing = self.project_terminals.actions_menu.is_closing();
            for (label, action, glyph, enabled) in [
                (
                    "Start all services",
                    "start",
                    icons::ACTION_PLAY,
                    configured,
                ),
                ("Stop all services", "stop", icons::STOP, running),
                (
                    "Restart all services",
                    "restart",
                    icons::RESTART,
                    configured,
                ),
            ] {
                menu = menu.child(
                    popover::menu_row(&popup_theme, false, format!("services-menu-{action}"))
                        .id(SharedString::from(format!("services-{action}")))
                        .role(gpui::Role::Button)
                        .aria_label(label)
                        .child(
                            icon(glyph)
                                .size(px(14.0))
                                .text_color(popup_theme.text_muted),
                        )
                        .child(label)
                        .when(busy || !enabled || closing, |el| {
                            el.opacity(0.4).cursor_default()
                        })
                        .when(!busy && enabled && !closing, |el| {
                            el.on_click(cx.listener(move |this, _, _, cx| {
                                this.close_project_terminal_actions(cx);
                                this.control_project_terminal(None, action, cx);
                            }))
                        }),
                );
            }
            let viewport = window.viewport_size();
            more = more.child(popover::contained_menu(
                "project-service-actions-popup".into(),
                div().child(menu),
                self.project_terminals.actions_menu.closing_since(),
                24.0,
                gpui::Bounds::new(
                    gpui::point(px(8.0), px(Theme::TITLEBAR_HEIGHT + 8.0)),
                    gpui::size(
                        (viewport.width - px(16.0)).max(px(1.0)),
                        (viewport.height - px(Theme::TITLEBAR_HEIGHT + 16.0)).max(px(1.0)),
                    ),
                ),
            ));
        }
        actions.child(more).into_any_element()
    }

    pub(super) fn render_project_terminal_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let context = self.project_terminals.context.clone();
        let snapshot = self.project_terminals.snapshot.clone();
        let compact = self.project_terminals.bar_width.get() < 520.0;
        let mut tabs = div()
            .id("project-service-tabs")
            .flex()
            .items_center()
            .gap(px(4.0))
            .flex_1()
            .min_w_0()
            .overflow_x_scroll()
            .when(!compact, |tabs| {
                tabs.flex_none()
                    .flex_shrink(1.0)
                    .max_w(px(self.project_terminals.bar_width.get() * 0.55))
            });
        if let (Some(context), Some(snapshot)) = (&context, &snapshot) {
            let scope = context.scope();
            for service in &snapshot.config.services {
                let status = service_status(snapshot, &service.id);
                let active = self.project_terminals.selected.get(&scope) == Some(&service.id);
                let id = service.id.clone();
                let tab_scope = scope.clone();
                let key = format!("{}-service-tab-{id}", cx.entity_id());
                let progress =
                    motion::state_t(&key, active, motion::TAB_SLIDE, self.reduced_motion);
                tabs = tabs.child(
                    popover::btn_ghost(&theme, "", key.clone())
                        .id(SharedString::from(format!("service-tab-{id}")))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .px(px(8.0))
                        .py(px(3.0))
                        .text_size(px(11.0))
                        .flex_none()
                        .role(gpui::Role::Tab)
                        .aria_label(format!("{} · {}", service.name, status_label(status)))
                        .aria_toggled(if active {
                            gpui::Toggled::True
                        } else {
                            gpui::Toggled::False
                        })
                        .tooltip(crate::settings::widgets::text_tooltip(service_tooltip(
                            snapshot, context, service,
                        )))
                        .bg(motion::hover_blend(
                            &key,
                            theme.wash(0.08).opacity(progress),
                            theme.element_hover,
                        ))
                        .text_color(motion::mix(theme.text_muted, theme.text, progress))
                        .child(status_dot(&theme, status))
                        .child(SharedString::from(service.name.clone()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.project_terminals.selected.get(&tab_scope) != Some(&id) {
                                this.project_terminals.content_started =
                                    Some(std::time::Instant::now());
                            }
                            this.project_terminals
                                .selected
                                .insert(tab_scope.clone(), id.clone());
                            if let Some(panel) = &this.project_terminals.panel
                                && let Some((key, _)) = this
                                    .project_terminals
                                    .tabs
                                    .get(&(tab_scope.clone(), id.clone()))
                            {
                                panel.update(cx, |panel, cx| {
                                    panel.select_tab_by_key(*key, cx);
                                    panel.request_focus(cx);
                                });
                            }
                            cx.notify();
                        })),
                );
            }
        }
        let configured = snapshot
            .as_ref()
            .is_some_and(|s| !s.config.services.is_empty());
        let width = self.project_terminals.bar_width.clone();
        let mut tab_bar = div()
            .id("project-service-bar")
            .relative()
            .h(px(SERVICE_BAR_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.0))
            .px(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .child(tabs)
            .child(
                gpui::canvas(
                    move |bounds, window, _| {
                        let measured = f32::from(bounds.size.width);
                        if (width.get() - measured).abs() > 0.5 {
                            width.set(measured);
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            );
        if !compact && let (Some(context), Some(snapshot)) = (&context, &snapshot) {
            let selected = self.project_terminals.selected.get(&context.scope());
            if let Some(service) = snapshot
                .config
                .services
                .iter()
                .find(|s| Some(&s.id) == selected)
            {
                let command = if service.directory.is_empty() || service.directory == "." {
                    service.command.clone()
                } else {
                    format!("{} · {}", service.command, service.directory)
                };
                tab_bar = tab_bar.child(
                    div()
                        .id("selected-service-command")
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .tooltip(crate::settings::widgets::text_tooltip(service_tooltip(
                            snapshot, context, service,
                        )))
                        .child(SharedString::from(command)),
                );
            }
        }
        let mut body = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(crate::terminal::view::terminal_panel_bg(&theme));
        if configured {
            body = body.child(tab_bar);
        }
        if let (Some(context), Some(snapshot)) = (&context, &snapshot) {
            let selected = self.project_terminals.selected.get(&context.scope());
            if let Some(service) = snapshot
                .config
                .services
                .iter()
                .find(|s| Some(&s.id) == selected)
            {
                let run = snapshot.runs.iter().find(|r| r.service_id == service.id);
                let status = service_status(snapshot, &service.id);
                if let Some(message) = run.and_then(|r| r.message.as_ref()) {
                    body = body.child(
                        div()
                            .px(px(10.0))
                            .py(px(5.0))
                            .text_size(px(11.0))
                            .text_color(status_color(status, &theme))
                            .child(SharedString::from(message.clone())),
                    );
                }
            } else if !configured {
                body = body.child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(8.0))
                        .px(px(20.0))
                        .child(
                            icon(icons::TERMINAL)
                                .size(px(24.0))
                                .text_color(theme.text_faint),
                        )
                        .child(
                            div()
                                .text_size(px(13.0))
                                .text_color(theme.text)
                                .child("No project services"),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.text_muted)
                                .child("Add your dev servers and tunnels to run them here."),
                        )
                        .child(
                            popover::btn_ghost(
                                &theme,
                                "Configure services",
                                "configure-empty-services",
                            )
                            .id("configure-empty-services")
                            .text_size(px(12.0))
                            .text_color(theme.text)
                            .bg(theme.element_active)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(context) = this.terminal_project_context(None, cx) {
                                    this.open_project_terminal_settings(context.space, cx);
                                }
                            })),
                        ),
                );
            }
        } else {
            body = body.child(
                div()
                    .p(px(16.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child("Loading project services…"),
            );
        }
        if let Some(error) = self
            .project_terminals
            .operation_error
            .as_ref()
            .or(self.project_terminals.error.as_ref())
            .or(self.project_terminals.handoff_error.as_ref())
        {
            body = body.child(
                div()
                    .mx(px(10.0))
                    .my(px(6.0))
                    .px(px(9.0))
                    .py(px(7.0))
                    .rounded(px(6.0))
                    .bg(theme.danger.opacity(0.06))
                    .text_size(px(12.0))
                    .text_color(theme.danger)
                    .child(SharedString::from(error.clone())),
            );
        }
        if configured && let Some(context) = &context {
            let selected = self.project_terminals.selected.get(&context.scope());
            let has_output = selected
                .and_then(|id| {
                    self.project_terminals
                        .tabs
                        .get(&(context.scope(), id.clone()))
                })
                .is_some_and(|(_, session)| session.is_some());
            if has_output && let Some(panel) = &self.project_terminals.panel {
                body = body.child(service_output_container(
                    panel.clone().into_any_element(),
                    self.project_terminals.output_height.clone(),
                ));
            } else if let Some(service) = snapshot
                .as_ref()
                .and_then(|s| s.config.services.iter().find(|s| Some(&s.id) == selected))
            {
                body = body.child(service_output_container(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px(px(16.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(format!(
                            "Start {} to see its output.",
                            service.name
                        )))
                        .into_any_element(),
                    self.project_terminals.output_height.clone(),
                ));
            }
        }
        body.into_any_element()
    }

    pub(super) fn render_project_terminal_editor(
        &mut self,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let editor = self.project_terminals.editor.as_ref()?;
        let theme = Theme::of(cx).for_popup();
        let compact = viewport.width < px(640.0);
        let busy = self.project_terminals.busy;
        let initialized = editor.initialized;
        let selected = editor
            .services
            .iter()
            .find(|service| editor.selected.as_deref() == Some(service.id.as_str()))
            .or_else(|| editor.services.first());
        let field_animation = SharedString::from(format!(
            "project-service-settings-fields-{}",
            selected.map(|s| s.id.as_str()).unwrap_or("empty")
        ));
        let mut list = div()
            .id("project-service-settings-list")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .when(compact, |list| list.flex_row().overflow_x_scroll())
            .when(!compact, |list| list.flex_col().overflow_y_scroll())
            .gap(px(4.0));
        for service in &editor.services {
            let id = service.id.clone();
            let active = selected.is_some_and(|selected| selected.id == id);
            let name = service.name.read(cx).text().trim().to_string();
            let key = format!("{}-service-settings-tab-{id}", cx.entity_id());
            let progress = motion::state_t(&key, active, motion::TAB_SLIDE, self.reduced_motion);
            list = list.child(
                div()
                    .id(SharedString::from(format!("service-settings-tab-{id}")))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(px(7.0))
                    .text_size(px(12.0))
                    .text_color(if active { theme.text } else { theme.text_muted })
                    .bg(motion::hover_blend(
                        &key,
                        theme.wash(0.08).opacity(progress),
                        theme.element_hover,
                    ))
                    .on_hover(motion::hover_listener(key))
                    .cursor_pointer()
                    .role(gpui::Role::Button)
                    .aria_label(format!(
                        "Edit {} service",
                        if name.is_empty() { "Unnamed" } else { &name }
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(editor) = &mut this.project_terminals.editor {
                            editor.selected = Some(id.clone());
                            editor.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                        }
                        cx.notify();
                    }))
                    .child(
                        icon(icons::TERMINAL)
                            .size(px(14.0))
                            .flex_none()
                            .text_color(theme.text_muted),
                    )
                    .child(div().min_w_0().truncate().child(SharedString::from(
                        if name.is_empty() {
                            "Unnamed service".into()
                        } else {
                            name
                        },
                    ))),
            );
        }
        let can_add = initialized && editor.services.len() < 12;
        let add = popover::btn_ghost(&theme, "Add service", "add-project-service")
            .id("add-project-service")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(7.0))
            .px(px(10.0))
            .child(
                icon(icons::PLUS)
                    .size(px(14.0))
                    .text_color(theme.text_muted),
            )
            .when(!can_add, |button| button.opacity(0.4).cursor_default())
            .when(can_add, |button| {
                button.on_click(cx.listener(|this, _, _, cx| {
                    let fields = this.service_fields(
                        ProjectTerminalService {
                            id: uuid::Uuid::new_v4().to_string(),
                            name: "New service".into(),
                            command: String::new(),
                            directory: ".".into(),
                            restart_on_failure: false,
                        },
                        cx,
                    );
                    if let Some(editor) = &mut this.project_terminals.editor {
                        editor.selected = Some(fields.id.clone());
                        editor.services.push(fields);
                        editor.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                    }
                    cx.notify();
                }))
            });
        let navigation = div()
            .flex_none()
            .flex()
            .gap(px(8.0))
            .p(px(12.0))
            .when(compact, |nav| nav.flex_row().border_b_1())
            .when(!compact, |nav| nav.w(px(168.0)).flex_col().border_r_1())
            .border_color(theme.border)
            .child(list)
            .child(add);
        let mut fields = div()
            .id("project-terminal-settings-scroll")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .px(px(20.0))
            .py(px(16.0))
            .overflow_y_scroll()
            .track_scroll(&editor.scroll)
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation());
        if let Some(service) = selected {
            let id = service.id.clone();
            let restart_id = id.clone();
            let restart = service.restart;
            fields = fields
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(popover::dialog_title(&theme, "Service"))
                        .child(
                            service_icon_button(
                                &theme,
                                format!("remove-service-{id}"),
                                icons::TRASH_BIN_MINIMALISTIC,
                                "Remove service",
                            )
                            .text_color(theme.danger)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if let Some(editor) = &mut this.project_terminals.editor {
                                        let index = editor
                                            .services
                                            .iter()
                                            .position(|s| s.id == id)
                                            .unwrap_or(0);
                                        editor.services.retain(|s| s.id != id);
                                        editor.selected = editor
                                            .services
                                            .get(index.min(editor.services.len().saturating_sub(1)))
                                            .map(|s| s.id.clone());
                                    }
                                    cx.notify();
                                },
                            )),
                        ),
                )
                .child(field_label(&theme, "Name"))
                .child(terminal_setting_field(
                    &theme,
                    service.name.clone().into_any_element(),
                ))
                .child(field_label(&theme, "Command"))
                .child(
                    terminal_setting_field(
                        &theme,
                        div()
                            .h(px(72.0))
                            .overflow_hidden()
                            .child(service.command.clone())
                            .into_any_element(),
                    )
                    .font_family(theme.font_mono.clone()),
                )
                .child(field_label(&theme, "Run in folder"))
                .child(
                    terminal_setting_field(&theme, service.directory.clone().into_any_element())
                        .font_family(theme.font_mono.clone()),
                )
                .child(
                    div()
                        .mt(px(5.0))
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child("Relative to the active checkout. Use . for the checkout root."),
                )
                .child(
                    div()
                        .mt(px(20.0))
                        .pt(px(14.0))
                        .border_t_1()
                        .border_color(theme.border)
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_size(px(12.0)).child("Restart on failure"))
                                .child(
                                    div()
                                        .mt(px(3.0))
                                        .text_size(px(11.0))
                                        .text_color(theme.text_muted)
                                        .child("Restart up to 3 times if the command exits with an error."),
                                ),
                        )
                        .child(
                            crate::settings::widgets::toggle_switch(
                                &theme,
                                restart,
                                format!("restart-service-{restart_id}"),
                            )
                            .id(SharedString::from(format!("restart-service-{restart_id}")))
                            .cursor_pointer()
                            .tab_index(0)
                            .role(gpui::Role::Switch)
                            .aria_label("Restart on failure")
                            .aria_toggled(if restart {
                                gpui::Toggled::True
                            } else {
                                gpui::Toggled::False
                            })
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if let Some(service) =
                                        this.project_terminals.editor.as_mut().and_then(|editor| {
                                            editor.services.iter_mut().find(|s| s.id == restart_id)
                                        })
                                    {
                                        service.restart = !service.restart;
                                    }
                                    cx.notify();
                                },
                            )),
                        ),
                );
        } else {
            fields = fields.child(popover::dialog_body(
                &theme,
                if initialized {
                    "Add a service to configure a dev server or tunnel."
                } else {
                    "Loading service settings…"
                },
            ));
        }
        let header = div()
            .flex_none()
            .px(px(20.0))
            .py(px(16.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(
                icon(icons::TERMINAL)
                    .size(px(20.0))
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(popover::dialog_title(&theme, "Project services"))
                    .child(
                        div()
                            .mt(px(4.0))
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .truncate()
                            .child(SharedString::from(format!(
                                "{} · {}",
                                editor.project.name,
                                if editor.project.checkout_override.is_some() {"This worktree"} else {"Project defaults"}
                            ))),
                    ),
            )
            .child(
                service_icon_button(
                    &theme,
                    "close-project-terminal-settings".into(),
                    icons::CLOSE,
                    "Close service settings",
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.project_terminals.editor = None;
                    cx.notify();
                })),
            );
        let mut footer = div()
            .flex_none()
            .px(px(20.0))
            .py(px(14.0))
            .border_t_1()
            .border_color(theme.border);
        if let Some(error) = self
            .project_terminals
            .operation_error
            .as_ref()
            .or(self.project_terminals.error.as_ref())
        {
            footer = footer.child(
                div()
                    .mb(px(10.0))
                    .text_size(px(12.0))
                    .text_color(theme.danger)
                    .child(SharedString::from(error.clone())),
            );
        }
        footer =
            footer.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child("Saved commands apply on the next start or restart."),
                    )
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "cancel-project-services")
                            .id("cancel-project-services")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.project_terminals.editor = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_primary(&theme, if busy { "Saving…" } else { "Save" })
                            .id("save-project-services")
                            .when(!initialized || busy, |button| {
                                button.opacity(0.5).cursor_default()
                            })
                            .when(initialized && !busy, |button| {
                                button.on_click(cx.listener(|this, _, _, cx| {
                                    this.save_project_terminal_settings(cx)
                                }))
                            }),
                    ),
            );
        let card = popover::dialog_card(&theme)
            .w(px(700.0))
            .max_w((viewport.width - px(40.0)).max(px(200.0)))
            .h(px(600.0).min((viewport.height - px(64.0)).max(px(200.0))))
            .p_0()
            .bg(theme.surface_dialog)
            .border_color(theme.border)
            .overflow_hidden()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .border_t_1()
                    .border_color(theme.border)
                    .when(compact, |body| body.flex_col())
                    .child(navigation)
                    .child(motion::fade_quick(field_animation, fields)),
            )
            .child(footer);
        Some(popover::modal(
            "project-terminal-settings",
            viewport,
            card.into_any_element(),
        ))
    }
}

fn field_label(theme: &Theme, label: &'static str) -> impl IntoElement {
    div()
        .mt(px(14.0))
        .mb(px(6.0))
        .text_size(px(12.0))
        .text_color(theme.text_muted)
        .child(label)
}

fn terminal_setting_field(theme: &Theme, input: AnyElement) -> gpui::Div {
    popover::dialog_field(input)
        .bg(theme.wash(0.035))
        .border_color(theme.border)
}

fn service_icon_button(
    theme: &Theme,
    id: String,
    glyph: &'static str,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    let label = label.into();
    popover::btn_ghost(theme, "", id.clone())
        .id(SharedString::from(id))
        .size(px(24.0))
        .p_0()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .role(gpui::Role::Button)
        .aria_label(label.clone())
        .tooltip(crate::settings::widgets::text_tooltip(label))
        .child(icon(glyph).size(px(15.0)).text_color(theme.text_muted))
}

fn service_tooltip(
    snapshot: &ProjectTerminalsSnapshot,
    context: &ProjectContext,
    service: &ProjectTerminalService,
) -> String {
    let status = service_status(snapshot, &service.id);
    let checkout = snapshot.checkout.as_deref().unwrap_or(&context.cwd);
    format!(
        "{} · {}\n{}\nCommand: {}\nFolder: {}\nCheckout: {}",
        service.name,
        status_label(status),
        service_status_detail(
            status,
            snapshot.runs.iter().find(|r| r.service_id == service.id)
        ),
        service.command,
        service_folder_label(&service.directory, checkout),
        checkout
    )
}

fn service_output_container(
    content: AnyElement,
    height: std::rc::Rc<std::cell::Cell<f32>>,
) -> impl IntoElement {
    div()
        .flex_1()
        .min_h_0()
        .relative()
        .flex()
        .flex_col()
        .child(content)
        .child(
            gpui::canvas(
                move |bounds, _, _| height.set(f32::from(bounds.size.height)),
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
}

fn status_label(status: Status) -> &'static str {
    match status {
        Status::Running => "Running",
        Status::Restarting => "Retrying…",
        Status::Stopped => "Not running",
        Status::Exited => "Finished",
        Status::Failed => "Exited with error",
        Status::Interrupted => "Needs restart",
    }
}

fn action_label(action: &str) -> &'static str {
    match action {
        "stop" => "Stopping",
        "restart" => "Restarting",
        _ => "Starting",
    }
}

fn service_is_active(status: Status) -> bool {
    matches!(status, Status::Running | Status::Restarting)
}

fn service_status(snapshot: &ProjectTerminalsSnapshot, id: &str) -> Status {
    snapshot
        .runs
        .iter()
        .find(|run| run.service_id == id)
        .map(|run| run.status)
        .unwrap_or(Status::Stopped)
}

fn service_summary(snapshot: &ProjectTerminalsSnapshot) -> String {
    let mut running = 0;
    let mut retrying = 0;
    let mut failed = 0;
    for service in &snapshot.config.services {
        match service_status(snapshot, &service.id) {
            Status::Running => running += 1,
            Status::Restarting => retrying += 1,
            Status::Failed | Status::Interrupted => failed += 1,
            _ => {}
        }
    }
    let mut label = format!("{running}/{} running", snapshot.config.services.len());
    if retrying > 0 {
        label.push_str(&format!(" · {retrying} retrying"));
    }
    if failed > 0 {
        label.push_str(&format!(
            " · {failed} {} attention",
            if failed == 1 { "needs" } else { "need" }
        ));
    }
    label
}

fn service_status_detail(status: Status, run: Option<&zeron_proto::ProjectTerminalRun>) -> String {
    if let Some(message) = run.and_then(|run| run.message.as_ref()) {
        return message.clone();
    }
    match status {
        Status::Restarting => format!(
            "Restarting after a crash · attempt {} of 3",
            run.map(|r| r.restarts + 1).unwrap_or(1).min(3)
        ),
        Status::Exited => "Command finished successfully (exit code 0).".into(),
        Status::Failed => run
            .and_then(|r| r.exit_code)
            .map(|code| format!("Command exited with code {code}. Check the output below."))
            .unwrap_or_else(|| "Could not start the command. Check the output below.".into()),
        Status::Interrupted => "The app closed unexpectedly. Start this service to resume.".into(),
        Status::Stopped => "Start this service to run its configured command.".into(),
        Status::Running => "Command is running in the checkout shown below.".into(),
    }
}

fn service_folder_label(directory: &str, checkout: &str) -> String {
    let checkout_name = std::path::Path::new(checkout)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(checkout);
    let directory = if directory.is_empty() || directory == "." {
        "Checkout root"
    } else {
        directory
    };
    format!("{directory} · {checkout_name}")
}

fn status_dot(theme: &Theme, status: Status) -> impl IntoElement {
    div()
        .size(px(6.0))
        .flex_none()
        .rounded_full()
        .bg(status_color(status, theme))
}

fn status_color(status: Status, theme: &Theme) -> gpui::Hsla {
    match status {
        Status::Running => theme.success,
        Status::Restarting => theme.warning,
        Status::Failed | Status::Interrupted => theme.danger,
        _ => theme.text_muted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[test]
    fn summary_counts_configured_services_and_distinguishes_recovery_from_running() {
        let service = |id: &str| ProjectTerminalService {
            id: id.into(),
            name: id.into(),
            command: "sleep 600".into(),
            directory: ".".into(),
            restart_on_failure: false,
        };
        let run = |id: &str, status| zeron_proto::ProjectTerminalRun {
            service_id: id.into(),
            status,
            exit_code: None,
            restarts: 0,
            message: None,
            terminal: None,
        };
        let snapshot = ProjectTerminalsSnapshot {
            checkout: None,
            config: ProjectTerminalConfig {
                services: vec![
                    service("backend"),
                    service("frontend"),
                    service("tunnel"),
                    service("worker"),
                ],
            },
            runs: vec![
                run("backend", Status::Running),
                run("frontend", Status::Restarting),
                run("tunnel", Status::Failed),
                run("removed", Status::Running),
            ],
        };
        assert_eq!(
            service_summary(&snapshot),
            "1/4 running · 1 retrying · 1 needs attention"
        );
        assert_eq!(service_status(&snapshot, "worker"), Status::Stopped);
        let failed = zeron_proto::ProjectTerminalRun {
            exit_code: Some(127),
            ..snapshot.runs[2].clone()
        };
        assert!(service_status_detail(Status::Failed, Some(&failed)).contains("code 127"));
        assert_eq!(
            service_folder_label(".", "/project/worktrees/agent-a"),
            "Checkout root · agent-a"
        );
        assert_eq!(
            service_folder_label("apps/backend", "/project/worktrees/agent-a"),
            "apps/backend · agent-a"
        );
    }

    #[gpui::test]
    fn reduced_motion_snaps_dock_and_content_without_replaying_on_status_poll(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let shell = test_shell(cx, dir.path());
        shell
            .update(cx, |shell, window, cx| {
                shell.reduced_motion = true;
                shell.state.update(cx, |state, _| {
                    state.selected_chat = Some("session".into());
                });
                shell.project_terminals.content_started = Some(std::time::Instant::now());
                let started = shell.project_terminals.content_started;
                shell.accept_project_terminal_snapshot(
                    ProjectTerminalsSnapshot {
                        checkout: None,
                        config: ProjectTerminalConfig::default(),
                        runs: vec![],
                    },
                    cx,
                );
                assert_eq!(shell.project_terminals.content_started, started);
                assert_eq!(shell.terminal_content_opacity(), 1.0);
                shell.hide_terminal_panel(window, cx);
                assert!(shell.terminal_tween.is_none());
                assert!(!shell.terminal_open(cx));
                assert_eq!(shell.terminal_target(cx), TERMINAL_TOOLBAR_HEIGHT);
            })
            .unwrap();
    }

    fn test_shell(cx: &mut TestAppContext, path: &std::path::Path) -> gpui::WindowHandle<Shell> {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: path.into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: zeron_proto::HarnessId::Mock,
                },
                cx,
            )
        })
    }

    #[gpui::test]
    fn status_polling_cannot_replace_unsaved_service_settings(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let shell = test_shell(cx, dir.path());
        shell
            .update(cx, |shell, _, cx| {
                let project = ProjectContext {
                    space: "project".into(),
                    device: "local".into(),
                    name: "Project".into(),
                    target: None,
                    chat: None,
                    cwd: dir.path().to_string_lossy().into_owned(),
                    checkout_override: None,
                };
                shell.project_terminals.context = Some(project.clone());
                shell.project_terminals.editor = Some(Editor {
                    project,
                    services: Vec::new(),
                    selected: None,
                    initialized: false,
                    scroll: gpui::ScrollHandle::new(),
                });
                let snapshot = ProjectTerminalsSnapshot {
                    checkout: None,
                    config: ProjectTerminalConfig {
                        services: vec![ProjectTerminalService {
                            id: "backend".into(),
                            name: "Backend".into(),
                            command: "bun dev".into(),
                            directory: ".".into(),
                            restart_on_failure: false,
                        }],
                    },
                    runs: Vec::new(),
                };
                shell.accept_project_terminal_snapshot(snapshot.clone(), cx);
                let editor = shell.project_terminals.editor.as_ref().unwrap();
                let input = editor.services[0].command.clone();
                input.update(cx, |input, cx| input.set_text("bun run dev:server", cx));
                shell.accept_project_terminal_snapshot(snapshot, cx);
                let editor = shell.project_terminals.editor.as_ref().unwrap();
                assert_eq!(editor.services[0].command.entity_id(), input.entity_id());
                assert_eq!(input.read(cx).text(), "bun run dev:server");
                assert_eq!(editor.selected.as_deref(), Some("backend"));
                assert!(editor.initialized);
            })
            .unwrap();
    }
    #[gpui::test]
    fn new_canvas_hides_session_chrome_and_does_not_activate_project_services(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let shell = test_shell(cx, dir.path());
        shell.update(cx, |shell, window, cx| {
            shell.active_chat = "session".into();
            shell.state.update(cx, |state, _| {
                state.selected_chat = Some("session".into());
                state.selected_space = Some("project".into());
                state.spaces = vec![serde_json::from_value(serde_json::json!({
                    "id":"project", "deviceId":"local", "path":"/project", "createdAt":Utc::now()
                })).unwrap()];
            });
            shell.panels.update("session", |panels| {
                panels.terminal_open = true;
                panels.changes_open = true;
            });
            shell.project_terminals.drawer = true;
            shell.settings.sidebar_collapsed = false;
            assert!(shell.session_workspace_visible(cx));
            assert!(shell.terminal_target(cx) > 0.0);
            shell.state.update(cx, |state, _| state.selected_chat = None);
            assert!(!shell.session_workspace_visible(cx));
            assert!(!shell.right_pane_open(cx));
            assert!(!shell.terminal_open(cx));
            assert_eq!(shell.terminal_target(cx), 0.0);
            assert!(shell.terminal_project_context(None, cx).is_none());
            assert!(shell.terminal_project_context(Some("project"), cx).is_some());
            shell.select_terminal_mode(TerminalMode::Shells, cx);
            shell.toggle_terminal(window, cx);
            assert!(shell.panels.get("session").terminal_open);
            assert!(!shell.settings.sidebar_collapsed);
            shell.state.update(cx, |state, _| state.selected_chat = Some("session".into()));
            assert!(shell.session_workspace_visible(cx));
            assert!(shell.right_pane_open(cx));
            assert!(shell.terminal_open(cx));
        }).unwrap();
    }

    #[gpui::test]
    fn agent_controls_share_project_scope_and_selection_waits_for_pending_mutations(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let shell = test_shell(cx, dir.path());
        shell
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |state, _| {
                    state.local_device_id = Some("local".into());
                    state.spaces = vec![serde_json::from_value(serde_json::json!({
                    "id":"project","deviceId":"local","path":"/project","createdAt":Utc::now()
                })).unwrap()];
                    state.chats = ["a", "b"]
                        .into_iter()
                        .map(|id| {
                            serde_json::from_value(serde_json::json!({
                    "id":id,"deviceId":"local","spaceId":"project","cwd":format!("/worktrees/{id}"),
                    "archived":false,"createdAt":Utc::now()
                })).unwrap()
                        })
                        .collect();
                    state.selected_chat = Some("a".into());
                    state.selected_space = Some("project".into());
                });
                let a = shell.terminal_project_context(None, cx).unwrap();
                shell.project_terminals.context = Some(a.clone());
                shell.project_terminals.drawer = true;
                shell.project_terminals.busy = true;
                shell
                    .state
                    .update(cx, |state, _| state.selected_chat = Some("b".into()));
                let b = shell.terminal_project_context(None, cx).unwrap();
                assert_ne!(a.scope(), b.scope(), "each physical checkout retains its own service views");
                assert_eq!(a.cwd, "/worktrees/a");
                assert_eq!(b.cwd, "/worktrees/b");
                assert_eq!(b.params(serde_json::json!({}))["chatId"], "b");
                shell.ensure_project_terminals(cx);
                assert!(
                    shell.project_terminals.busy,
                    "a late old-agent Start must finish before the new handoff"
                );
                assert_eq!(
                    shell
                        .project_terminals
                        .context
                        .as_ref()
                        .unwrap()
                        .chat
                        .as_deref(),
                    Some("b")
                );
                let settings = shell.terminal_project_context(Some("project"), cx).unwrap();
                assert!(
                    settings.chat.is_none(),
                    "opening settings cannot select an agent checkout"
                );
                shell.state.update(cx, |state, _| {
                    state.selected_chat = Some("not-synced-yet".into())
                });
                assert!(shell.terminal_project_context(None, cx).is_none());
            })
            .unwrap();
    }

    #[gpui::test]
    fn switching_terminal_categories_preserves_tabs_and_hide_remembers_the_category(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let shell = test_shell(cx, dir.path());
        shell.update(cx, |shell, window, cx| {
            shell.state.update(cx, |state, _| {
                state.local_device_id = Some("local".into());
                state.spaces = vec![serde_json::from_value(serde_json::json!({
                    "id": "project", "deviceId": "local", "path": dir.path(), "createdAt": Utc::now()
                })).unwrap()];
                state.selected_space = Some("project".into());
                state.selected_chat = Some("session".into());
                state.chats = vec![serde_json::from_value(serde_json::json!({
                    "id":"session", "deviceId":"local", "spaceId":"project",
                    "cwd":dir.path(), "createdAt":Utc::now(), "archived":false
                })).unwrap()];
            });
            shell.select_terminal_mode(TerminalMode::Services, cx);
            let service_panel = shell.project_terminals.panel.clone().unwrap();
            let scope = shell.project_terminals.context.as_ref().unwrap().scope();
            let service_tab = service_panel.update(cx, |panel, cx| {
                panel.reserve_tab_for_chat(scope.clone(), "Backend", cx)
            });
            // This unit fixture has no engine connection. Seed an existing
            // shell tab; native coverage verifies creation of a real PTY.
            let shell_key = shell.state.read(cx).panel_session_key();
            let shell_panel = shell.terminal_panel(cx);
            shell_panel.update(cx, |panel, cx| {
                panel.reserve_tab_for_chat(shell_key, "Terminal 1", cx)
            });
            shell.select_terminal_mode(TerminalMode::Shells, cx);
            let shell_tabs = shell_panel.read(cx).tab_summaries(cx);
            assert_eq!(shell_tabs.len(), 1);
            assert!(!shell.composer.read(cx).focus_pending);
            assert!(!shell.project_terminals.drawer);
            shell.select_terminal_mode(TerminalMode::Services, cx);
            assert!(!shell_panel.read(cx).is_open());
            assert_eq!(shell_panel.read(cx).tab_summaries(cx), shell_tabs);
            assert_eq!(service_panel.read(cx).tab_summaries(cx)[0].0, service_tab);
            shell.hide_terminal_panel(window, cx);
            assert!(!shell.terminal_open(cx));
            assert_eq!(shell.terminal_target(cx), TERMINAL_TOOLBAR_HEIGHT);
            shell.toggle_terminal(window, cx);
            assert!(shell.project_terminals.drawer);
            shell.select_terminal_mode(TerminalMode::Shells, cx);
            shell.hide_terminal_panel(window, cx);
            shell.toggle_terminal(window, cx);
            assert!(shell_panel.read(cx).is_open());
            assert!(!shell.project_terminals.drawer);
            assert_eq!(shell_panel.read(cx).tab_summaries(cx), shell_tabs);
            assert!(shell.right_terminal.is_none());
        }).unwrap();
    }

    #[gpui::test]
    fn successful_status_polls_preserve_a_failed_stop_message(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let shell = test_shell(cx, dir.path());
        shell
            .update(cx, |shell, _, cx| {
                shell.project_terminals.context = Some(ProjectContext {
                    space: "project".into(),
                    device: "local".into(),
                    name: "Project".into(),
                    target: None,
                    chat: None,
                    cwd: dir.path().to_string_lossy().into_owned(),
                    checkout_override: None,
                });
                shell.project_terminals.operation_error = Some("Backend could not stop".into());
                shell.project_terminals.error = Some("Temporary status error".into());
                shell.accept_project_terminal_snapshot(
                    ProjectTerminalsSnapshot {
                        checkout: None,
                        config: ProjectTerminalConfig {
                            services: Vec::new(),
                        },
                        runs: Vec::new(),
                    },
                    cx,
                );
                assert_eq!(
                    shell.project_terminals.operation_error.as_deref(),
                    Some("Backend could not stop")
                );
                assert!(shell.project_terminals.error.is_none());
            })
            .unwrap();
    }
}

#[cfg(feature = "source-control-fixture")]
impl Shell {
    pub fn fixture_open_project_terminals(&mut self, cx: &mut Context<Self>) {
        self.select_terminal_mode(TerminalMode::Services, cx);
    }
    pub fn fixture_select_project_terminal_agent(&mut self, chat: String, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.select_chat(Some(chat), cx));
        self.route = Route::Chat;
        cx.notify();
    }
    pub fn fixture_project_terminal_state(&self, cx: &App) -> serde_json::Value {
        let text = |panel: &Entity<TerminalPanel>| {
            panel
                .read(cx)
                .active_grid_snapshot(cx)
                .map(|grid| {
                    grid.lines
                        .into_iter()
                        .map(|line| line.into_iter().map(|cell| cell.ch).collect::<String>())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        serde_json::json!({ "snapshot": self.project_terminals.snapshot, "busy": self.project_terminals.busy,
            "serviceSummary": self.project_terminals.snapshot.as_ref().map(service_summary),
            "dockHeight": self.terminal_geometry.get().height, "dockAnimating": self.terminal_tween.is_some(),
            "outputHeight": self.project_terminals.output_height.get(), "serviceBarWidth": self.project_terminals.bar_width.get(),
            "actionsMenu": self.project_terminals.actions_menu.is_open(),
            "selectedService": self.project_terminals.context.as_ref().and_then(|c| self.project_terminals.selected.get(&c.scope())),
            "contentOpacity": self.terminal_content_opacity(), "reducedMotion": self.reduced_motion,
            "shellOutput": self.terminal.as_ref().map(text).unwrap_or_default(),
            "serviceOutput": self.project_terminals.panel.as_ref().map(text).unwrap_or_default(),
            "panelOpen": self.terminal_open(cx), "shellSessions": self.terminal.as_ref().map(|panel| panel.read(cx).fixture_session_ids(cx)).unwrap_or_default(),
            "shellTabs": self.terminal.as_ref().map(|panel| panel.read(cx).tab_summaries(cx)).unwrap_or_default(),
            "error": self.project_terminals.operation_error.as_ref().or(self.project_terminals.error.as_ref()), "mode": if self.project_terminals.mode == TerminalMode::Services { "services" } else { "shells" }, "editor": self.project_terminals.editor.is_some(),
            "drawer": self.project_terminals.drawer, "handoff": self.project_terminals.handoff.is_some(),
            "handoffError": self.project_terminals.handoff_error })
    }
}
