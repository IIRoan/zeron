use super::*;
use zeron_proto::{
    ProjectTerminalConfig, ProjectTerminalService, ProjectTerminalStatus as Status,
    ProjectTerminalsSnapshot, ServiceReadinessState as Readiness,
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

pub(super) struct ServiceFields {
    pub(super) id: String,
    pub(super) name: Entity<ComposerInput>,
    pub(super) command: Entity<ComposerInput>,
    pub(super) directory: Entity<ComposerInput>,
    pub(super) restart: bool,
    pub(super) prerequisite: bool,
    pub(super) readiness: Entity<ComposerInput>,
    pub(super) readiness_timeout: Entity<ComposerInput>,
    /// A suggested service the project has not saved yet. It is only saved
    /// once the user changes it from `original`.
    seeded: bool,
    original: ProjectTerminalService,
    _events: Vec<Subscription>,
}

impl ServiceFields {
    pub(super) fn service(&self, cx: &App) -> ProjectTerminalService {
        ProjectTerminalService {
            id: self.id.clone(),
            name: self.name.read(cx).text().to_string(),
            command: self.command.read(cx).text().to_string(),
            directory: self.directory.read(cx).text().to_string(),
            restart_on_failure: self.restart,
            setup_prerequisite: self
                .prerequisite
                .then(|| zeron_proto::ProjectServiceReadiness {
                    command: self.readiness.read(cx).text().to_string(),
                    timeout_seconds: self.readiness_timeout.read(cx).text().trim().parse().unwrap_or(0),
                }),
        }
    }

    fn untouched_seed(&self, cx: &App) -> bool {
        self.seeded && self.service(cx) == self.original
    }

    fn missing_command(&self, cx: &App) -> bool {
        !self.untouched_seed(cx) && self.command.read(cx).text().trim().is_empty()
    }
}

pub(super) struct Editor {
    project: ProjectContext,
    /// App services only. Databases are edited in Project settings.
    services: Vec<ServiceFields>,
    /// Shown read-only so the list matches the Services panel.
    databases: Vec<ProjectTerminalService>,
    selected: Option<String>,
    initialized: bool,
    scroll: gpui::ScrollHandle,
    /// The services as loaded, to detect unsaved edits.
    saved: Vec<ProjectTerminalService>,
    /// Set by a rejected save, so empty commands are flagged only after the
    /// user tried to save them.
    show_missing: bool,
    /// Set when a save closes the editor, so it still reads "Saving…" while it
    /// animates out.
    closed_by_save: bool,
}

impl Editor {
    fn new(project: ProjectContext) -> Self {
        Self {
            project,
            services: Vec::new(),
            databases: Vec::new(),
            selected: None,
            initialized: false,
            scroll: gpui::ScrollHandle::new(),
            saved: Vec::new(),
            show_missing: false,
            closed_by_save: false,
        }
    }

    fn config(&self, cx: &App) -> Vec<ProjectTerminalService> {
        self.services
            .iter()
            .filter(|fields| !fields.untouched_seed(cx))
            .map(|fields| fields.service(cx))
            .collect()
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.initialized && self.config(cx) != self.saved
    }

    /// The databases are sent back unchanged: an engine with
    /// `project-databases-v1` ignores them, and an older engine that still
    /// saves whole lists keeps them instead of dropping them.
    fn save_payload(&self, cx: &App) -> ProjectTerminalConfig {
        ProjectTerminalConfig {
            services: self
                .databases
                .iter()
                .cloned()
                .chain(self.config(cx))
                .collect(),
        }
    }
}

/// Asks before an editor with unsaved edits is closed.
pub(super) struct DiscardPrompt {
    buttons: [FocusHandle; 2],
    previous_focus: Option<FocusHandle>,
    focus_pending: bool,
    _key_interceptor: Subscription,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnsavedForm {
    Services,
    WorktreeSettings,
}

#[derive(Default)]
pub(super) struct ProjectTerminalUi {
    pub drawer: bool,
    pub mode: TerminalMode,
    pub editor: popover::Popup<Editor>,
    pub(super) discard: popover::Popup<DiscardPrompt>,
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
    /// Scope of the checkout services were last activated for.
    last_activation: Option<String>,
    handoff_error: Option<String>,
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

    /// Only a foreground change of the selected checkout can move services.
    /// Refocusing the window, background status polls and the project
    /// settings editor never claim a checkout again.
    pub(super) fn follow_project_terminal_checkout(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if !window.is_window_active()
            || !matches!(self.route, Route::Chat)
            || self.project_terminals.handoff.is_some()
            || self.project_terminals.busy
        {
            return;
        }
        let Some(context) = self.terminal_project_context(None, cx) else {
            return;
        };
        let scope = context.scope();
        if self.project_terminals.last_activation.as_ref() == Some(&scope) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.project_terminals.last_activation = Some(scope);
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

    pub(super) fn open_service_settings_context(
        &mut self,
        context: ProjectContext,
        cx: &mut Context<Self>,
    ) {
        self.project_terminals.editor.open(Editor::new(context));
        self.project_terminals.discard = popover::Popup::default();
        self.project_terminals.poll = None;
        self.ensure_project_terminals(cx);
        cx.notify();
    }

    pub(super) fn ensure_project_terminals(&mut self, cx: &mut Context<Self>) {
        if !self.project_terminals.drawer && !self.project_terminals.editor.is_open() {
            self.project_terminals.poll = None;
            return;
        }
        let context = self
            .project_terminals
            .editor
            .as_open()
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
        // Keep polling during Start/Stop and handoffs: a database can take a
        // while to become ready, and its progress should stay visible.
        if self.project_terminals.poll.is_some() {
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
                                    shell.accept_project_terminal_snapshot(snapshot, cx);
                                }
                                Err(error) => {
                                    let error = Some(error.to_string());
                                    if shell.project_terminals.error != error {
                                        shell.project_terminals.error = error;
                                        cx.notify();
                                    }
                                }
                            }
                        }
                        shell.project_terminals.drawer || shell.project_terminals.editor.is_open()
                    })
                    .unwrap_or(false);
                if !keep_polling {
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        }));
    }

    pub(super) fn service_fields(
        &self,
        service: ProjectTerminalService,
        seeded: bool,
        cx: &mut Context<Self>,
    ) -> ServiceFields {
        let original = service.clone();
        let prerequisite = service.setup_prerequisite.is_some();
        let [name, command, directory] = if prerequisite {
            [
                "Database name",
                "Start command, e.g. docker compose up postgres",
                "Folder in the main project, e.g. .",
            ]
        } else {
            [
                "Service name",
                "Command to run",
                "Directory relative to agent checkout",
            ]
        }
        .map(|placeholder| cx.new(|cx| ComposerInput::new(placeholder, cx)));
        let readiness =
            cx.new(|cx| ComposerInput::new("Ready check, e.g. pg_isready -h localhost", cx));
        let timeout_seconds = service
            .setup_prerequisite
            .as_ref()
            .map_or(120, |r| r.timeout_seconds);
        let readiness_timeout = cx.new(|cx| ComposerInput::new("Seconds", cx));
        readiness_timeout.update(cx, |input, cx| input.set_text(timeout_seconds.to_string(), cx));
        readiness.update(cx, |input, cx| {
            input.set_text(
                service
                    .setup_prerequisite
                    .as_ref()
                    .map(|r| r.command.clone())
                    .unwrap_or_default(),
                cx,
            )
        });
        name.update(cx, |input, cx| input.set_text(service.name, cx));
        command.update(cx, |input, cx| input.set_text(service.command, cx));
        directory.update(cx, |input, cx| input.set_text(service.directory, cx));
        let events = [&name, &command, &directory, &readiness, &readiness_timeout]
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
            prerequisite,
            readiness,
            readiness_timeout,
            seeded,
            original,
            _events: events,
        }
    }

    /// Returns whether the snapshot changed anything on screen.
    fn accept_project_terminal_snapshot(
        &mut self,
        snapshot: ProjectTerminalsSnapshot,
        cx: &mut Context<Self>,
    ) -> bool {
        // Status polls repeat every second; an unchanged result must not
        // re-render the whole shell.
        let mut changed = self.project_terminals.error.take().is_some()
            || self.project_terminals.snapshot.as_ref() != Some(&snapshot);
        let (databases, apps): (Vec<_>, Vec<_>) = snapshot
            .config
            .services
            .iter()
            .cloned()
            .partition(|s| s.is_database());
        if let Some(editor) = self.project_terminals.editor.open_mut()
            && editor.initialized
            && editor.databases != databases
        {
            editor.databases = databases.clone();
            changed = true;
        }
        if self
            .project_terminals
            .editor
            .as_open()
            .is_some_and(|editor| !editor.initialized)
        {
            changed = true;
            let seeded = apps.is_empty();
            let services = if seeded {
                ["Backend", "Frontend", "Cloudflared"]
                    .into_iter()
                    .map(|name| ProjectTerminalService {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: name.into(),
                        command: String::new(),
                        directory: ".".into(),
                        restart_on_failure: false,
                        setup_prerequisite: None,
                    })
                    .collect()
            } else {
                apps
            };
            let fields = services
                .into_iter()
                .map(|service| self.service_fields(service, seeded, cx))
                .collect();
            if let Some(editor) = self.project_terminals.editor.open_mut() {
                editor.services = fields;
                editor.databases = databases;
                editor.selected = editor.services.first().map(|service| service.id.clone());
                editor.initialized = true;
                editor.saved = editor.config(cx);
            }
        }
        let Some(context) = self.project_terminals.context.clone() else {
            if changed {
                cx.notify();
            }
            return changed;
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
                    changed = true;
                }
            }
            for service in &snapshot.config.services {
                let map_key = (scope.clone(), service.id.clone());
                let entry = self
                    .project_terminals
                    .tabs
                    .entry(map_key)
                    .or_insert_with(|| {
                        changed = true;
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
                        changed = true;
                    }
                }
            }
            let selected = self
                .project_terminals
                .selected
                .entry(scope.clone())
                .or_insert_with(|| {
                    changed = true;
                    snapshot
                        .config
                        .services
                        .first()
                        .map(|s| s.id.clone())
                        .unwrap_or_default()
                });
            if !snapshot.config.services.iter().any(|s| &s.id == selected) {
                let first = snapshot
                    .config
                    .services
                    .first()
                    .map(|s| s.id.clone())
                    .unwrap_or_default();
                changed |= *selected != first;
                *selected = first;
            }
            if let Some((tab, _)) = self.project_terminals.tabs.get(&(scope, selected.clone())) {
                panel.update(cx, |panel, cx| panel.select_tab_by_key(*tab, cx));
            }
        }
        self.project_terminals.snapshot = Some(snapshot);
        if changed {
            cx.notify();
        }
        changed
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
                            if let Some(editor) = shell.project_terminals.editor.open_mut() {
                                editor.closed_by_save = true;
                            }
                            popover::close_popup(shell, cx, |this| {
                                &mut this.project_terminals.editor
                            });
                            popover::close_popup(shell, cx, |this| {
                                &mut this.project_terminals.discard
                            });
                            // Project settings, if open underneath, lists them.
                            shell.refresh_worktree_settings(cx);
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
        let Some(editor) = self.project_terminals.editor.open_mut() else {
            return;
        };
        if let Some(missing) = editor
            .services
            .iter()
            .find(|fields| fields.missing_command(cx))
            .map(|fields| fields.id.clone())
        {
            editor.selected = Some(missing);
            editor.show_missing = true;
            cx.notify();
            return;
        }
        let config = editor.save_payload(cx);
        let params = editor
            .project
            .params(serde_json::json!({ "config": config }));
        self.request_project_terminals(methods::SAVE_PROJECT_TERMINALS, params, cx);
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
        let checking = self
            .project_terminals
            .snapshot
            .as_ref()
            .filter(|_| self.project_terminals.context.as_ref() == project.as_ref())
            .and_then(|snapshot| {
                snapshot.config.services.iter().find(|service| {
                    service_readiness(snapshot, &service.id) == Some(Readiness::Checking)
                })
            })
            .map(|service| service.name.clone());
        let description = if let Some(name) = checking {
            format!("Waiting for {name} to be ready…")
        } else if self.project_terminals.handoff.is_some() {
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
                .unwrap_or(if action == "stop-worktree" {
                    "app services"
                } else {
                    "all services"
                });
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
        let selected = self
            .project_terminals
            .context
            .as_ref()
            .and_then(|context| self.project_terminals.selected.get(&context.scope()));
        let selected_service = snapshot
            .and_then(|snapshot| {
                snapshot
                    .config
                    .services
                    .iter()
                    .find(|service| Some(&service.id) == selected)
            })
            .cloned();
        // One split control, like the project-action pill: a direct primary
        // action and an attached menu for restart, bulk actions and settings.
        let mut actions = div()
            .id("service-controls")
            .flex_none()
            .flex()
            .items_center()
            .h(px(24.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.wash(0.035));
        if let (Some(snapshot), Some(service)) = (snapshot, selected_service.as_ref()) {
            let status = service_status(snapshot, &service.id);
            let (action, glyph, label) = if service_is_active(status) {
                ("stop", icons::STOP, "Stop")
            } else {
                ("start", icons::ACTION_PLAY, "Start")
            };
            let id = service.id.clone();
            let hint = format!("{label} {}", service.name);
            actions = actions
                .child(
                    popover::btn_ghost(&theme, "", format!("service-{action}"))
                        .id(SharedString::from(format!("service-{action}")))
                        .h(px(22.0))
                        .px(px(7.0))
                        .py_0()
                        .rounded(px(0.0))
                        .rounded_l(px(6.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(5.0))
                        .text_size(px(11.5))
                        .role(gpui::Role::Button)
                        .aria_label(hint.clone())
                        .tooltip(crate::settings::widgets::text_tooltip(hint))
                        .child(icon(glyph).size(px(13.0)).text_color(theme.text_muted))
                        .child(label)
                        .when(busy, |el| el.opacity(0.4).cursor_default())
                        .when(!busy, |el| {
                            el.on_click(cx.listener(move |this, _, _, cx| {
                                this.control_project_terminal(Some(id.clone()), action, cx);
                            }))
                        }),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(1.0))
                        .h(px(14.0))
                        .bg(theme.text.opacity(0.14)),
                );
        }
        let mut more = popover::btn_ghost(&theme, "", "project-services-more")
            .id("services-more")
            .size(px(22.0))
            .p_0()
            .rounded(px(0.0))
            .rounded_r(px(6.0))
            .when(selected_service.is_none(), |el| el.rounded_l(px(6.0)))
            .flex_none()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .role(gpui::Role::Button)
            .aria_label("Service actions")
            .tooltip(crate::settings::widgets::text_tooltip(
                "Service actions and settings",
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
                .w(px(250.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .on_mouse_down_out(
                    cx.listener(|this, _, _, cx| this.close_project_terminal_actions(cx)),
                )
                .on_click(|_, _, cx| cx.stop_propagation());
            let closing = self.project_terminals.actions_menu.is_closing();
            if let Some(service) = &selected_service {
                let id = service.id.clone();
                let label = format!("Restart {}", service.name);
                menu = menu
                    .child(
                        popover::menu_row(&popup_theme, false, "services-menu-restart-selected")
                            .id("service-restart")
                            .role(gpui::Role::Button)
                            .aria_label(label.clone())
                            .child(
                                icon(icons::RESTART)
                                    .size(px(14.0))
                                    .text_color(popup_theme.text_muted),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(label)),
                            )
                            .when(busy || closing, |el| el.opacity(0.4).cursor_default())
                            .when(!busy && !closing, |el| {
                                el.on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_project_terminal_actions(cx);
                                    this.control_project_terminal(Some(id.clone()), "restart", cx);
                                }))
                            }),
                    )
                    .child(popover::menu_separator());
            }
            // With shared databases, the everyday actions leave them running;
            // only the last one stops them too.
            let databases =
                snapshot.is_some_and(|s| s.config.services.iter().any(|s| s.is_database()));
            let apps_running = snapshot.is_some_and(|s| {
                s.config.services.iter().any(|service| {
                    !service.is_database() && service_is_active(service_status(s, &service.id))
                })
            });
            let actions: &[(&'static str, &'static str, &'static str, bool)] = if databases {
                &[
                    (
                        "Start all services",
                        "start",
                        icons::ACTION_PLAY,
                        configured,
                    ),
                    (
                        "Restart app services",
                        "restart",
                        icons::RESTART,
                        configured,
                    ),
                    (
                        "Stop app services",
                        "stop-worktree",
                        icons::STOP,
                        apps_running,
                    ),
                    (
                        "Stop all, including databases",
                        "stop",
                        icons::STOP,
                        running,
                    ),
                ]
            } else {
                &[
                    (
                        "Start all services",
                        "start",
                        icons::ACTION_PLAY,
                        configured,
                    ),
                    (
                        "Restart all services",
                        "restart",
                        icons::RESTART,
                        configured,
                    ),
                    ("Stop all services", "stop", icons::STOP, running),
                ]
            };
            for &(label, action, glyph, enabled) in actions {
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
            menu = menu
                .child(popover::menu_separator())
                .child(
                    popover::menu_row(&popup_theme, false, "services-menu-edit")
                        .id("services-edit")
                        .role(gpui::Role::Button)
                        .aria_label("Edit app services")
                        .child(
                            icon(icons::SETTINGS_MINIMALISTIC)
                                .size(px(14.0))
                                .text_color(popup_theme.text_muted),
                        )
                        .child("Edit app services…")
                        .when(closing, |el| el.opacity(0.4).cursor_default())
                        .when(!closing, |el| {
                            el.on_click(cx.listener(|this, _, _, cx| {
                                this.close_project_terminal_actions(cx);
                                if let Some(context) = this.terminal_project_context(None, cx) {
                                    this.open_project_terminal_settings(context.space, cx);
                                }
                            }))
                        }),
                )
                .child(
                    popover::menu_row(&popup_theme, false, "services-menu-worktree-settings")
                        .id("services-worktree-settings")
                        .role(gpui::Role::Button)
                        .aria_label("Project settings")
                        .tooltip(crate::settings::widgets::text_tooltip(
                            "Shared databases, worktree setup and environment",
                        ))
                        .child(
                            icon(icons::FOLDER)
                                .size(px(14.0))
                                .text_color(popup_theme.text_muted),
                        )
                        .child("Project settings…")
                        .when(closing, |el| el.opacity(0.4).cursor_default())
                        .when(!closing, |el| {
                            el.on_click(cx.listener(|this, _, _, cx| {
                                this.close_project_terminal_actions(cx);
                                this.open_active_worktree_settings(cx);
                            }))
                        }),
                );
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
                popover::MenuAlign::End,
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
            let mut previous_database = false;
            for service in &snapshot.config.services {
                // A hairline separates shared databases from the app services.
                if previous_database && !service.is_database() {
                    tabs = tabs.child(
                        div()
                            .flex_none()
                            .w(px(1.0))
                            .h(px(14.0))
                            .mx(px(2.0))
                            .bg(theme.text.opacity(0.14)),
                    );
                }
                previous_database = service.is_database();
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
                        .aria_label(format!(
                            "{}{} · {}",
                            service.name,
                            if service.is_database() {
                                " (shared database)"
                            } else {
                                ""
                            },
                            service_state_label(snapshot, service)
                        ))
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
                        .child(status_dot(service_state_color(snapshot, service, &theme)))
                        .when(service.is_database(), |tab| {
                            tab.child(
                                icon(icons::HARD_DRIVE)
                                    .size(px(12.0))
                                    .flex_none()
                                    .text_color(theme.text_muted),
                            )
                        })
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
                if service_readiness(snapshot, &service.id) == Some(Readiness::Checking) {
                    body = body.child(
                        div()
                            .px(px(10.0))
                            .py(px(5.0))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(loaders::mini_mono_spinner(
                                "database-readiness-progress",
                                1.5,
                                theme.warning,
                                cx.entity_id(),
                                cx,
                            ))
                            .child(SharedString::from(format!(
                                "Waiting until {} is ready. Setup and app services start after this check passes.",
                                service.name
                            ))),
                    );
                } else if let Some(message) = run.and_then(|r| r.message.as_ref()) {
                    body = body.child(
                        div()
                            .px(px(10.0))
                            .py(px(5.0))
                            .text_size(px(11.0))
                            .text_color(service_state_color(snapshot, service, &theme))
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let editor = self.project_terminals.editor.get()?;
        let theme = Theme::of(cx).for_popup();
        let compact = viewport.width < px(640.0);
        let busy = self.project_terminals.busy || editor.closed_by_save;
        let initialized = editor.initialized;
        let selected_database = editor
            .databases
            .iter()
            .find(|database| editor.selected.as_deref() == Some(database.id.as_str()));
        let selected = editor
            .services
            .iter()
            .find(|service| editor.selected.as_deref() == Some(service.id.as_str()))
            .or_else(|| editor.services.first())
            .filter(|_| selected_database.is_none());
        let field_animation = SharedString::from(format!(
            "project-service-settings-fields-{}",
            selected
                .map(|s| s.id.as_str())
                .or(selected_database.map(|d| d.id.as_str()))
                .unwrap_or("empty")
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
        if !editor.databases.is_empty() {
            if !compact {
                list = list.child(nav_caption(&theme, "Shared databases"));
            }
            for database in &editor.databases {
                let id = database.id.clone();
                let active = selected_database.is_some_and(|d| d.id == id);
                let key = format!("{}-service-settings-tab-{id}", cx.entity_id());
                let progress =
                    motion::state_t(&key, active, motion::TAB_SLIDE, self.reduced_motion);
                list = list.child(
                    div()
                        .id(SharedString::from(format!("database-settings-tab-{id}")))
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
                        .aria_label(format!("Show {} database", database.name))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(editor) = this.project_terminals.editor.open_mut() {
                                editor.selected = Some(id.clone());
                            }
                            cx.notify();
                        }))
                        .child(
                            icon(icons::HARD_DRIVE)
                                .size(px(14.0))
                                .flex_none()
                                .text_color(theme.text_muted),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(database.name.clone())),
                        ),
                );
            }
            if !compact {
                list = list.child(nav_caption(&theme, "App services"));
            }
        }
        for service in &editor.services {
            let id = service.id.clone();
            let active = selected.is_some_and(|selected| selected.id == id);
            let missing = editor.show_missing && service.missing_command(cx);
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
                    .text_color(if missing {
                        theme.danger
                    } else if active {
                        theme.text
                    } else {
                        theme.text_muted
                    })
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
                        if let Some(editor) = this.project_terminals.editor.open_mut() {
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
            .role(gpui::Role::Button)
            .aria_label("Add service")
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
                            setup_prerequisite: None,
                        },
                        false,
                        cx,
                    );
                    if let Some(editor) = this.project_terminals.editor.open_mut() {
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
        if let Some(database) = selected_database {
            let space = editor.project.space.clone();
            let can_open = !editor.is_dirty(cx) && !busy;
            let readiness = database.setup_prerequisite.as_ref();
            let detail = |label: &'static str, value: String| {
                div().child(field_label(&theme, label)).child(
                    div()
                        .px(px(10.0))
                        .py(px(7.0))
                        .rounded(px(6.0))
                        .bg(theme.wash(0.035))
                        .font_family(theme.font_mono.clone())
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .child(SharedString::from(value)),
                )
            };
            fields = fields
                .child(popover::dialog_title(&theme, &database.name))
                .child(popover::dialog_body(
                    &theme,
                    "A shared database. It runs in the main project folder for every worktree and starts before setup and app services. Start, stop and logs are in the Services panel.",
                ))
                .child(detail("Start command", database.command.clone()))
                .child(detail(
                    "Ready when this command succeeds",
                    readiness.map(|r| r.command.clone()).unwrap_or_default(),
                ))
                .child(
                    div()
                        .flex()
                        .gap(px(12.0))
                        .child(div().flex_1().child(detail(
                            "Run in folder",
                            if database.directory == "." {
                                "Project root".into()
                            } else {
                                database.directory.clone()
                            },
                        )))
                        .child(div().flex_1().child(detail(
                            "Wait up to",
                            format!("{} seconds", readiness.map_or(0, |r| r.timeout_seconds)),
                        ))),
                )
                .child(div().flex().child(
                    crate::settings::widgets::action(
                        &theme,
                        crate::settings::widgets::ActionTone::Outlined,
                        "database-worktree-settings",
                        window,
                    )
                    .child("Edit in Project settings")
                    .role(gpui::Role::Button)
                    .aria_label("Edit in Project settings")
                    .mt(px(16.0))
                    .when(!can_open, |button| {
                        button.opacity(0.4).cursor_default().tooltip(
                            crate::settings::widgets::text_tooltip(
                                "Save or cancel your service edits first.",
                            ),
                        )
                    })
                    .when(can_open, |button| {
                        button.on_click(cx.listener(move |this, _, _, cx| {
                            popover::close_popup(this, cx, |this| {
                                &mut this.project_terminals.editor
                            });
                            this.show_project_database_settings(space.clone(), cx);
                        }))
                    }),
                ));
        } else if let Some(service) = selected {
            {
            let id = service.id.clone();
            let restart_id = id.clone();
            let restart = service.restart;
            let prerequisite = service.prerequisite;
            let missing = editor.show_missing && service.missing_command(cx);
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
                                    if let Some(editor) = this.project_terminals.editor.open_mut() {
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
                    .font_family(theme.font_mono.clone())
                    .when(missing, |field| field.border_color(theme.danger)),
                )
                .when(missing, |fields| {
                    fields.child(
                        div()
                            .mt(px(5.0))
                            .text_size(px(11.0))
                            .text_color(theme.danger)
                            .child("Command required"),
                    )
                })
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
                        .child(if prerequisite { "Relative to the main project folder; shared across worktrees." } else { "Relative to the active checkout. Use . for the checkout root." }),
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
                                        this.project_terminals.editor.open_mut().and_then(|editor| {
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
            }
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
                .on_click(cx.listener(|this, _, window, cx| {
                    this.request_close_form(UnsavedForm::Services, window, cx);
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
                            .role(gpui::Role::Button)
                            .aria_label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.request_close_form(UnsavedForm::Services, window, cx);
                            })),
                    )
                    .child(
                        popover::btn_primary(&theme, if busy { "Saving…" } else { "Save" }, "save-project-services")
                            .id("save-project-services")
                            .role(gpui::Role::Button)
                            .aria_label("Save services")
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
        // Same size as Project settings, which opens this editor on top of
        // itself: it then reads as a sub-page instead of a second dialog.
        let card = popover::dialog_card(&theme)
            .w(px(840.0))
            .max_w((viewport.width - px(40.0)).max(px(200.0)))
            .h(px(680.0).min((viewport.height - px(64.0)).max(px(200.0))))
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
            self.project_terminals.editor.closing_since(),
        ))
    }

    fn discard_prompt(&self, form: UnsavedForm) -> Option<&DiscardPrompt> {
        match form {
            UnsavedForm::Services => self.project_terminals.discard.as_open(),
            UnsavedForm::WorktreeSettings => self.worktree_settings.discard.as_open(),
        }
    }

    fn discard_prompt_slot(&mut self, form: UnsavedForm) -> &mut popover::Popup<DiscardPrompt> {
        match form {
            UnsavedForm::Services => &mut self.project_terminals.discard,
            UnsavedForm::WorktreeSettings => &mut self.worktree_settings.discard,
        }
    }

    fn unsaved_form_dirty(&self, form: UnsavedForm, cx: &App) -> bool {
        match form {
            UnsavedForm::Services => self
                .project_terminals
                .editor
                .as_open()
                .is_some_and(|editor| editor.is_dirty(cx)),
            UnsavedForm::WorktreeSettings => self.worktree_settings_dirty(cx),
        }
    }

    /// Close an editor, or ask first when it holds unsaved edits.
    pub(super) fn request_close_form(
        &mut self,
        form: UnsavedForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.unsaved_form_dirty(form, cx) {
            self.discard_form(form, window, cx);
            return;
        }
        if !self.discard_prompt_slot(form).is_open() {
            let shell = cx.weak_entity();
            let owner = window.window_handle();
            let interceptor = cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != owner {
                    return;
                }
                let _ = shell.update(cx, |shell, cx| {
                    if shell.capture_discard_prompt_key(form, &event.keystroke, window, cx) {
                        cx.stop_propagation();
                    }
                });
            });
            let buttons = std::array::from_fn(|_| cx.focus_handle().tab_stop(true));
            let previous_focus = window.focused(cx);
            window.focus(&buttons[0], cx);
            self.discard_prompt_slot(form).open(DiscardPrompt {
                buttons,
                previous_focus,
                focus_pending: true,
                _key_interceptor: interceptor,
            });
        }
        cx.notify();
    }

    pub(super) fn discard_form(
        &mut self,
        form: UnsavedForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.discard_prompt_slot(form).is_open() {
            popover::close_popup(self, cx, move |this| this.discard_prompt_slot(form));
            window.focus(&self.shortcut_focus, cx);
        }
        match form {
            UnsavedForm::Services => {
                popover::close_popup(self, cx, |this| &mut this.project_terminals.editor)
            }
            UnsavedForm::WorktreeSettings => self.close_worktree_settings(cx),
        }
        cx.notify();
    }

    /// Dismiss the discard prompt and return to the editor. Returns whether
    /// a prompt was open.
    pub(super) fn keep_editing(
        &mut self,
        form: UnsavedForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(prompt) = self.discard_prompt(form) else {
            return false;
        };
        let focus = prompt.previous_focus.clone();
        popover::close_popup(self, cx, move |this| this.discard_prompt_slot(form));
        if let Some(focus) = focus {
            window.focus(&focus, cx);
        }
        true
    }

    fn capture_discard_prompt_key(
        &mut self,
        form: UnsavedForm,
        key: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(prompt) = self.discard_prompt(form) else {
            return false;
        };
        let active = prompt
            .buttons
            .iter()
            .position(|button| button.is_focused(window))
            .unwrap_or(0);
        let plain = !key.modifiers.control && !key.modifiers.alt && !key.modifiers.platform;
        let discard = match key.key.as_str() {
            "tab" if plain => {
                window.focus(&prompt.buttons[(active + 1) % 2], cx);
                None
            }
            "escape" => Some(false),
            "enter" | "space" if plain => Some(active == 1),
            _ => None,
        };
        if let Some(discard) = discard {
            let identity = prompt.buttons[0].clone();
            let shell = cx.weak_entity();
            // Keep the capture guard mounted until this dispatch finishes.
            window.defer(cx, move |window, cx| {
                let _ = shell.update(cx, |shell, cx| {
                    if shell
                        .discard_prompt(form)
                        .is_some_and(|prompt| prompt.buttons[0] == identity)
                    {
                        if discard {
                            shell.discard_form(form, window, cx);
                        } else {
                            shell.keep_editing(form, window, cx);
                        }
                    }
                });
            });
        }
        true
    }

    pub(super) fn render_discard_prompt(
        &mut self,
        form: UnsavedForm,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let popup = self.discard_prompt_slot(form);
        if let Some(prompt) = popup.open_mut()
            && std::mem::take(&mut prompt.focus_pending)
        {
            window.focus(&prompt.buttons[0], cx);
        }
        let closing = popup.closing_since();
        let prompt = popup.get()?;
        let theme = Theme::of(cx).for_popup();
        let (id, body) = match form {
            UnsavedForm::Services => (
                "discard-project-services",
                "Your edits to these services have not been saved.",
            ),
            UnsavedForm::WorktreeSettings => (
                "discard-worktree-settings",
                "Your edits to these project settings have not been saved.",
            ),
        };
        let card = popover::dialog_card(&theme)
            .id(SharedString::from(format!("{id}-card")))
            .role(gpui::Role::Dialog)
            .aria_label("Discard unsaved changes?")
            .w(px(400.0).min(viewport.width - px(32.0)))
            .child(popover::dialog_title(&theme, "Discard unsaved changes?"))
            .child(div().mt(px(6.0)).child(popover::dialog_body(&theme, body)))
            .child(
                div()
                    .mt(px(20.0))
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Keep editing", format!("{id}-keep"))
                            .id(SharedString::from(format!("{id}-keep")))
                            .role(gpui::Role::Button)
                            .track_focus(&prompt.buttons[0])
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .focus_visible(|style| style.border_color(theme.text_muted))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.keep_editing(form, window, cx);
                            })),
                    )
                    .child(
                        popover::btn_danger(&theme, "Discard changes", SharedString::from(format!("{id}-confirm")))
                            .id(SharedString::from(format!("{id}-confirm")))
                            .role(gpui::Role::Button)
                            .track_focus(&prompt.buttons[1])
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .focus_visible(|style| style.border_color(theme.text_muted))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.discard_form(form, window, cx)
                            })),
                    ),
            );
        Some(popover::modal(id, viewport, card.into_any_element(), closing))
    }
}

fn nav_caption(theme: &Theme, label: &'static str) -> impl IntoElement {
    div()
        .flex_none()
        .mt(px(6.0))
        .px(px(10.0))
        .text_size(px(10.5))
        .text_color(theme.text_faint)
        .child(label)
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
    let detail = service_status_detail(
        status,
        snapshot.runs.iter().find(|r| r.service_id == service.id),
    );
    if let Some(readiness) = &service.setup_prerequisite {
        return format!(
            "{} · {}\nShared database · runs in the main project folder for every worktree\n{}\nCommand: {}\nReady check: {} (up to {} s)\nFolder: {}",
            service.name,
            service_state_label(snapshot, service),
            detail,
            service.command,
            readiness.command,
            readiness.timeout_seconds,
            if service.directory == "." {
                "Project root"
            } else {
                &service.directory
            },
        );
    }
    let checkout = snapshot.checkout.as_deref().unwrap_or(&context.cwd);
    format!(
        "{} · {}\n{}\nCommand: {}\nFolder: {}\nCheckout: {}",
        service.name,
        status_label(status),
        detail,
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
        "stop" | "stop-worktree" => "Stopping",
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
    let apps = snapshot.config.services.iter().filter(|s| !s.is_database());
    for service in apps.clone() {
        match service_status(snapshot, &service.id) {
            Status::Running => running += 1,
            Status::Restarting => retrying += 1,
            Status::Failed | Status::Interrupted => failed += 1,
            _ => {}
        }
    }
    let mut parts = Vec::new();
    let databases: Vec<_> = snapshot
        .config
        .services
        .iter()
        .filter(|s| s.is_database())
        .collect();
    if let Some(first) = databases.first() {
        let states: Vec<_> = databases
            .iter()
            .map(|db| service_state_label(snapshot, db))
            .collect();
        let subject = if databases.len() == 1 {
            first.name.clone()
        } else {
            "Databases".into()
        };
        let state = if states.contains(&"Starting…") {
            "starting…"
        } else if states
            .iter()
            .any(|s| matches!(*s, "Not ready" | "Exited with error" | "Needs restart"))
        {
            "not ready"
        } else if states
            .iter()
            .all(|s| matches!(*s, "Ready" | "Running" | "Retrying…"))
        {
            "ready"
        } else {
            "stopped"
        };
        parts.push(format!("{subject} {state}"));
    }
    let apps = apps.count();
    if apps > 0 {
        let mut label = format!("{running}/{apps} running");
        if retrying > 0 {
            label.push_str(&format!(" · {retrying} retrying"));
        }
        if failed > 0 {
            label.push_str(&format!(
                " · {failed} {} attention",
                if failed == 1 { "needs" } else { "need" }
            ));
        }
        parts.push(label);
    }
    parts.join(" · ")
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

fn status_dot(color: gpui::Hsla) -> impl IntoElement {
    div().size(px(6.0)).flex_none().rounded_full().bg(color)
}

fn service_readiness(snapshot: &ProjectTerminalsSnapshot, id: &str) -> Option<Readiness> {
    snapshot
        .runs
        .iter()
        .find(|run| run.service_id == id)
        .and_then(|run| run.readiness)
}

/// A database's readiness matters more than whether its process is alive.
fn service_state_label(
    snapshot: &ProjectTerminalsSnapshot,
    service: &ProjectTerminalService,
) -> &'static str {
    let status = service_status(snapshot, &service.id);
    match service_readiness(snapshot, &service.id) {
        Some(Readiness::Checking) if service_is_active(status) => "Starting…",
        Some(Readiness::Ready) if service_is_active(status) || status == Status::Exited => "Ready",
        Some(Readiness::NotReady) if service_is_active(status) => "Not ready",
        _ => status_label(status),
    }
}

fn service_state_color(
    snapshot: &ProjectTerminalsSnapshot,
    service: &ProjectTerminalService,
    theme: &Theme,
) -> gpui::Hsla {
    match service_state_label(snapshot, service) {
        "Starting…" => theme.warning,
        "Ready" => theme.success,
        "Not ready" => theme.danger,
        _ => status_color(service_status(snapshot, &service.id), theme),
    }
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
            setup_prerequisite: None,
        };
        let run = |id: &str, status| zeron_proto::ProjectTerminalRun {
            service_id: id.into(),
            status,
            exit_code: None,
            restarts: 0,
            message: None,
            terminal: None,
            readiness: None,
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
    fn service_editor_preserves_and_edits_startup_readiness(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let shell = test_shell(cx, dir.path());
        shell
            .update(cx, |shell, _, cx| {
                let service = ProjectTerminalService {
                    id: "databases".into(),
                    name: "Databases".into(),
                    command: "bun run start-db".into(),
                    directory: ".".into(),
                    restart_on_failure: false,
                    setup_prerequisite: Some(zeron_proto::ProjectServiceReadiness {
                        command: "check-database".into(),
                        timeout_seconds: 75,
                    }),
                };
                let mut fields = shell.service_fields(service.clone(), false, cx);
                assert_eq!(fields.service(cx), service);
                fields.readiness.update(cx, |input, cx| {
                    input.set_text("check-database-and-redis", cx)
                });
                let changed = fields.service(cx).setup_prerequisite.unwrap();
                assert_eq!(changed.command, "check-database-and-redis");
                assert_eq!(changed.timeout_seconds, 75);
                fields.readiness_timeout.update(cx, |input, cx| input.set_text("240", cx));
                assert_eq!(fields.service(cx).setup_prerequisite.unwrap().timeout_seconds, 240);
                fields.prerequisite = false;
                assert!(fields.service(cx).setup_prerequisite.is_none());
            })
            .unwrap();
    }

    fn database_service(name: &str) -> ProjectTerminalService {
        ProjectTerminalService {
            id: "db".into(),
            name: name.into(),
            command: "bun run db".into(),
            directory: ".".into(),
            restart_on_failure: false,
            setup_prerequisite: Some(zeron_proto::ProjectServiceReadiness {
                command: "pg_isready".into(),
                timeout_seconds: 120,
            }),
        }
    }

    fn app_service(id: &str, name: &str) -> ProjectTerminalService {
        ProjectTerminalService {
            id: id.into(),
            name: name.into(),
            command: format!("bun run {id}"),
            directory: ".".into(),
            restart_on_failure: false,
            setup_prerequisite: None,
        }
    }

    #[gpui::test]
    fn services_editor_shows_databases_read_only_and_saves_only_app_services(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let shell = test_shell(cx, dir.path());
        shell
            .update(cx, |shell, _, cx| {
                let project = test_project(dir.path());
                shell.project_terminals.context = Some(project.clone());
                shell.project_terminals.editor.open(Editor::new(project.clone()));
                let snapshot = |db: &str| ProjectTerminalsSnapshot {
                    checkout: None,
                    config: ProjectTerminalConfig {
                        services: vec![database_service(db), app_service("backend", "Backend")],
                    },
                    runs: vec![],
                };
                shell.accept_project_terminal_snapshot(snapshot("Databases"), cx);
                let editor = shell.project_terminals.editor.as_open().unwrap();
                assert_eq!(editor.services.len(), 1, "databases are not editable here");
                assert_eq!(editor.services[0].id, "backend");
                assert_eq!(editor.databases, vec![database_service("Databases")]);
                assert!(!editor.is_dirty(cx));
                assert_eq!(editor.config(cx), vec![app_service("backend", "Backend")]);
                let command = editor.services[0].command.clone();
                command.update(cx, |input, cx| input.set_text("bun run dev:api", cx));
                // A poll after the database was renamed in Project settings
                // refreshes the read-only list but keeps the unsaved edit.
                shell.accept_project_terminal_snapshot(snapshot("Postgres"), cx);
                let editor = shell.project_terminals.editor.as_open().unwrap();
                assert_eq!(editor.databases[0].name, "Postgres");
                assert_eq!(editor.services[0].command.entity_id(), command.entity_id());
                assert_eq!(editor.config(cx)[0].command, "bun run dev:api");
                assert!(editor.config(cx).iter().all(|s| !s.is_database()));
                assert!(editor.is_dirty(cx));
                let payload = editor.save_payload(cx).services;
                assert_eq!(
                    payload[0],
                    database_service("Postgres"),
                    "databases go back unchanged"
                );
                assert_eq!(payload[1].command, "bun run dev:api");

                // A project with only a database still suggests app services,
                // and untouched suggestions are never saved.
                shell.project_terminals.editor.open(Editor::new(project));
                shell.accept_project_terminal_snapshot(
                    ProjectTerminalsSnapshot {
                        checkout: None,
                        config: ProjectTerminalConfig {
                            services: vec![database_service("Databases")],
                        },
                        runs: vec![],
                    },
                    cx,
                );
                let editor = shell.project_terminals.editor.as_open().unwrap();
                let names: Vec<_> = editor
                    .services
                    .iter()
                    .map(|s| s.name.read(cx).text().to_string())
                    .collect();
                assert_eq!(names, ["Backend", "Frontend", "Cloudflared"]);
                assert!(editor.config(cx).is_empty());
                assert!(!editor.is_dirty(cx));
            })
            .unwrap();
    }

    #[test]
    fn database_readiness_drives_status_and_summary() {
        let run = |id: &str, readiness| zeron_proto::ProjectTerminalRun {
            service_id: id.into(),
            status: Status::Running,
            exit_code: None,
            restarts: 0,
            message: None,
            terminal: None,
            readiness,
        };
        let database = database_service("Databases");
        let mut snapshot = ProjectTerminalsSnapshot {
            checkout: None,
            config: ProjectTerminalConfig {
                services: vec![
                    database.clone(),
                    app_service("backend", "Backend"),
                    app_service("frontend", "Frontend"),
                ],
            },
            runs: vec![run("db", Some(Readiness::Checking)), run("backend", None)],
        };
        assert_eq!(service_state_label(&snapshot, &database), "Starting…");
        assert_eq!(
            service_summary(&snapshot),
            "Databases starting… · 1/2 running"
        );
        snapshot.runs[0].readiness = Some(Readiness::Ready);
        assert_eq!(service_state_label(&snapshot, &database), "Ready");
        assert_eq!(service_summary(&snapshot), "Databases ready · 1/2 running");
        snapshot.runs[0].readiness = Some(Readiness::NotReady);
        assert_eq!(service_state_label(&snapshot, &database), "Not ready");
        assert_eq!(
            service_summary(&snapshot),
            "Databases not ready · 1/2 running"
        );
        snapshot.runs[0].status = Status::Stopped;
        snapshot.runs[0].readiness = None;
        assert_eq!(service_state_label(&snapshot, &database), "Not running");
        assert_eq!(
            service_summary(&snapshot),
            "Databases stopped · 1/2 running"
        );
        let tooltip = service_tooltip(
            &snapshot,
            &test_project(std::path::Path::new("/p")),
            &database,
        );
        assert!(tooltip.contains("Shared database"));
        assert!(tooltip.contains("Ready check: pg_isready (up to 120 s)"));
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
                shell.project_terminals.editor.open(Editor::new(project));
                let snapshot = ProjectTerminalsSnapshot {
                    checkout: None,
                    config: ProjectTerminalConfig {
                        services: vec![ProjectTerminalService {
                            id: "backend".into(),
                            name: "Backend".into(),
                            command: "bun dev".into(),
                            directory: ".".into(),
                            restart_on_failure: false,
                            setup_prerequisite: None,
                        }],
                    },
                    runs: Vec::new(),
                };
                shell.accept_project_terminal_snapshot(snapshot.clone(), cx);
                let editor = shell.project_terminals.editor.as_open().unwrap();
                let input = editor.services[0].command.clone();
                input.update(cx, |input, cx| input.set_text("bun run dev:server", cx));
                shell.accept_project_terminal_snapshot(snapshot, cx);
                let editor = shell.project_terminals.editor.as_open().unwrap();
                assert_eq!(editor.services[0].command.entity_id(), input.entity_id());
                assert_eq!(input.read(cx).text(), "bun run dev:server");
                assert_eq!(editor.selected.as_deref(), Some("backend"));
                assert!(editor.initialized);
            })
            .unwrap();
    }
    #[gpui::test]
    fn fresh_chat_keeps_checkout_and_shared_service_scope(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let shell = test_shell(cx, dir.path());
        shell.update(cx, |shell, _, cx| {
        shell.state.update(cx, |state, cx| {
            state.local_device_id = Some("local".into());
            state.spaces = ["project", "other"].into_iter().map(|id| {
                serde_json::from_value(serde_json::json!({
                    "id": id, "deviceId": "remote", "path": format!("/{id}"),
                    "gitDetected": true, "createdAt": Utc::now()
                })).unwrap()
            }).collect();
            state.chats = ["claude", "codex"].into_iter().map(|id| {
                serde_json::from_value(serde_json::json!({
                    "id": id, "deviceId": "remote", "spaceId": "project", "cwd": "/worktrees/shared",
                    "branch": "feature", "archived": false, "createdAt": Utc::now(),
                    "harnessSessionId": "old-provider-session",
                    "config": { "harness": "claude-code", "sandbox": "workspace-write" }
                })).unwrap()
            }).collect();
            state.select_chat(Some("claude".into()), cx);
        });
        let before = shell.terminal_project_context(None, cx).unwrap();
        shell.settings.space_filter = Some("other".into());
        shell.new_chat_in_current_checkout(cx);
        let state = shell.state.read(cx);
        assert!(state.selected_chat.is_none());
        assert!(state.transcript.is_empty());
        assert_eq!(state.selected_space.as_deref(), Some("project"));
        assert_eq!(state.effective_device_id().as_deref(), Some("remote"));
        assert_eq!(state.chats.len(), 2);
        assert!(!shell.session_workspace_visible(cx));
        assert!(shell.terminal_project_context(None, cx).is_none());
        assert_eq!(shell.composer.read(cx).pickers().read(cx).checkout_plan(), crate::pickers::CheckoutPlan::ReuseCheckout {
            path: "/worktrees/shared".into(), branch: Some("feature".into())
        });
        shell.state.update(cx, |state, cx| state.select_chat(Some("codex".into()), cx));
        let after = shell.terminal_project_context(None, cx).unwrap();
        assert_eq!(before.scope(), after.scope(), "changing agents in one checkout must reuse its services");
        assert_ne!(before.chat, after.chat);
    }).unwrap();
        cx.run_until_parked();
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

    fn test_project(dir: &std::path::Path) -> ProjectContext {
        ProjectContext {
            space: "project".into(),
            device: "local".into(),
            name: "Project".into(),
            target: None,
            chat: None,
            cwd: dir.to_string_lossy().into_owned(),
            checkout_override: None,
        }
    }

    fn empty_snapshot() -> ProjectTerminalsSnapshot {
        ProjectTerminalsSnapshot {
            checkout: None,
            config: ProjectTerminalConfig {
                services: Vec::new(),
            },
            runs: Vec::new(),
        }
    }

    struct PromptHost {
        shell: Entity<Shell>,
        _observer: Subscription,
    }

    impl Render for PromptHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.shell.update(cx, |shell, cx| {
                let modal = shell.render_discard_prompt(
                    UnsavedForm::Services,
                    window.viewport_size(),
                    window,
                    cx,
                );
                div()
                    .size_full()
                    .track_focus(&shell.shortcut_focus)
                    .capture_key_down(cx.listener(Shell::on_key_down_capture))
                    .on_action(|_: &SaveFile, _, _| {
                        panic!("background save bypassed unsaved-settings prompt")
                    })
                    .children(modal)
            })
        }
    }

    #[gpui::test]
    fn unsaved_settings_prompt_traps_shortcuts_and_keyboard_focus(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let shell_window = test_shell(cx, dir.path());
        let shell = shell_window.update(cx, |_, _, cx| cx.entity()).unwrap();
        cx.update(|cx| cx.bind_keys([gpui::KeyBinding::new("ctrl-s", SaveFile, None)]));
        let (host, cx) = cx.add_window_view(|window, cx| {
            shell.update(cx, |shell, cx| {
                let project = test_project(dir.path());
                shell.project_terminals.context = Some(project.clone());
                shell.project_terminals.editor.open(Editor::new(project));
                shell.accept_project_terminal_snapshot(empty_snapshot(), cx);
                shell.project_terminals.editor.as_open().unwrap().services[0]
                    .command
                    .update(cx, |input, cx| input.set_text("run dev", cx));
                shell.request_close_form(UnsavedForm::Services, window, cx);
            });
            PromptHost {
                _observer: cx.observe(&shell, |_, _, cx| cx.notify()),
                shell,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear());
        cx.simulate_keystrokes("ctrl-s");
        for (keys, button) in [("tab", 1), ("tab", 0), ("shift-tab", 1)] {
            cx.simulate_keystrokes(keys);
            cx.update(|window, cx| {
                assert!(
                    host.read(cx)
                        .shell
                        .read(cx)
                        .project_terminals
                        .discard
                        .as_open()
                        .unwrap()
                        .buttons[button]
                        .is_focused(window)
                );
            });
        }
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        host.read_with(cx, |host, cx| {
            let shell = host.shell.read(cx);
            assert!(!shell.project_terminals.discard.is_open());
            assert!(shell.project_terminals.editor.is_open());
        });
    }

    #[test]
    fn session_workspace_gating_applies_only_on_linux() {
        for (chat_route, chat_selected) in [(false, false), (true, false), (false, true)] {
            assert!(session_workspace_visible_on(
                false,
                chat_route,
                chat_selected
            ));
            assert!(!session_workspace_visible_on(
                true,
                chat_route,
                chat_selected
            ));
        }
        assert!(session_workspace_visible_on(true, true, true));
    }

    #[gpui::test]
    fn shell_terminal_toggle_closes_an_open_services_drawer(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let shell = test_shell(cx, dir.path());
        shell
            .update(cx, |shell, window, cx| {
                shell.state.update(cx, |state, _| {
                    state.local_device_id = Some("local".into());
                    state.spaces = vec![
                        serde_json::from_value(serde_json::json!({
                            "id": "project", "deviceId": "local", "path": dir.path(),
                            "createdAt": Utc::now()
                        }))
                        .unwrap(),
                    ];
                    state.selected_space = Some("project".into());
                    state.selected_chat = Some("session".into());
                    state.chats = vec![
                        serde_json::from_value(serde_json::json!({
                            "id": "session", "deviceId": "local", "spaceId": "project",
                            "cwd": dir.path(), "createdAt": Utc::now(), "archived": false
                        }))
                        .unwrap(),
                    ];
                });
                shell.select_terminal_mode(TerminalMode::Services, cx);
                assert!(shell.project_terminals.drawer);
                assert!(shell.terminal_open(cx));
                shell.toggle_shell_terminal(window, cx);
                assert!(!shell.project_terminals.drawer);
                assert!(!shell.terminal_open(cx));
                assert!(!shell.panels.get(&shell.panel_key(cx)).terminal_open);
            })
            .unwrap();
    }

    #[gpui::test]
    fn services_editor_skips_untouched_suggestions_and_requires_commands(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let shell = test_shell(cx, dir.path());
        shell
            .update(cx, |shell, window, cx| {
                let project = test_project(dir.path());
                shell.project_terminals.context = Some(project.clone());
                shell.project_terminals.editor.open(Editor::new(project.clone()));
                shell.accept_project_terminal_snapshot(empty_snapshot(), cx);
                let editor = shell.project_terminals.editor.as_open().unwrap();
                assert_eq!(editor.services.len(), 3, "a new project suggests services");
                assert!(
                    editor.config(cx).is_empty(),
                    "untouched suggestions are not saved"
                );
                assert!(!editor.is_dirty(cx));
                shell.request_close_form(UnsavedForm::Services, window, cx);
                assert!(!shell.project_terminals.editor.is_open());
                assert!(!shell.project_terminals.discard.is_open());

                shell.project_terminals.editor.open(Editor::new(project));
                shell.accept_project_terminal_snapshot(empty_snapshot(), cx);
                let editor = shell.project_terminals.editor.as_open().unwrap();
                let frontend = editor.services[1].id.clone();
                editor.services[1]
                    .name
                    .update(cx, |input, cx| input.set_text("Web", cx));
                assert!(editor.is_dirty(cx));
                shell.save_project_terminal_settings(cx);
                let editor = shell.project_terminals.editor.as_open().unwrap();
                assert!(
                    editor.show_missing,
                    "an edited service without a command is flagged"
                );
                assert_eq!(editor.selected.as_deref(), Some(frontend.as_str()));
                assert!(
                    !shell.project_terminals.busy,
                    "the save request is not sent"
                );

                shell.request_close_form(UnsavedForm::Services, window, cx);
                assert!(shell.project_terminals.discard.is_open());
                assert!(shell.project_terminals.editor.is_open());
                assert!(shell.capture_escape_surface(window, cx));
                assert!(!shell.project_terminals.discard.is_open());
                assert!(
                    shell.project_terminals.editor.is_open(),
                    "Escape keeps editing"
                );
                shell.request_close_form(UnsavedForm::Services, window, cx);
                shell.discard_form(UnsavedForm::Services, window, cx);
                assert!(!shell.project_terminals.discard.is_open());
                assert!(!shell.project_terminals.editor.is_open());
            })
            .unwrap();
    }

    #[gpui::test]
    fn unchanged_status_polls_do_not_rerender(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let shell = test_shell(cx, dir.path());
        shell
            .update(cx, |shell, _, cx| {
                shell.project_terminals.context = Some(test_project(dir.path()));
                assert!(shell.accept_project_terminal_snapshot(empty_snapshot(), cx));
                assert!(!shell.accept_project_terminal_snapshot(empty_snapshot(), cx));
                shell.project_terminals.error = Some("Temporary status error".into());
                assert!(
                    shell.accept_project_terminal_snapshot(empty_snapshot(), cx),
                    "clearing a poll error is a visible change"
                );
                assert!(shell.project_terminals.error.is_none());
            })
            .unwrap();
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
            "error": self.project_terminals.operation_error.as_ref().or(self.project_terminals.error.as_ref()), "mode": if self.project_terminals.mode == TerminalMode::Services { "services" } else { "shells" }, "editor": self.project_terminals.editor.is_open(),
            "drawer": self.project_terminals.drawer, "handoff": self.project_terminals.handoff.is_some(),
            "handoffError": self.project_terminals.handoff_error })
    }
}
