//! Device-private environment defaults and per-checkout overrides.
use super::project_terminals_ui::{DiscardPrompt, ProjectContext, ServiceFields, UnsavedForm};
use super::*;
use crate::settings::widgets::{self, SelectOption, SelectState};
use std::hash::{Hash as _, Hasher as _};
use zeron_proto::{
    ProjectTerminalService, WorktreeDependencyMode as Deps, WorktreeEnvFile,
    WorktreeEnvMode as Env, WorktreeServiceMode as Services, WorktreeSettings,
    WorktreeSettingsSnapshot, WorktreeSetupPhase as Phase,
};

mod environment;
mod removal;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SettingsPage {
    #[default]
    General,
    Environment,
    Workflow,
    Services,
    Advanced,
}

impl SettingsPage {
    const ALL: [Self; 5] = [
        Self::General,
        Self::Environment,
        Self::Workflow,
        Self::Services,
        Self::Advanced,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Environment => "environment",
            Self::Workflow => "workflow",
            Self::Services => "services",
            Self::Advanced => "advanced",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Environment => "Environment",
            Self::Workflow => "Workflow",
            Self::Services => "Services",
            Self::Advanced => "Advanced",
        }
    }
}

#[derive(Default)]
pub(super) struct WorktreeSettingsUi {
    pub(super) editor: popover::Popup<Editor>,
    poll: Option<Task<()>>,
    epoch: u64,
    dependencies: SelectState,
    scope: SelectState,
    services: SelectState,
    directory: SelectState,
    env_modes: std::collections::HashMap<String, SelectState>,
    lifecycle_visible: bool,
    page: SettingsPage,
    navigation_focus: Option<[gpui::FocusHandle; 5]>,
    removal: popover::Popup<removal::Removal>,
    pub(super) discard: popover::Popup<DiscardPrompt>,
}

pub(super) struct Editor {
    project: ProjectContext,
    checkout: Option<String>,
    snapshot: Option<WorktreeSettingsSnapshot>,
    fields: Option<Fields>,
    drafts: std::collections::HashMap<Option<String>, Fields>,
    scroll: gpui::ScrollHandle,
    error: Option<String>,
    /// Kept apart from `error` so a successful poll can clear it without
    /// erasing a save or setup failure the user has not seen yet.
    poll_error: Option<String>,
    busy: bool,
    output: Option<String>,
}

impl Editor {
    fn new(project: ProjectContext) -> Self {
        Self {
            project,
            checkout: None,
            snapshot: None,
            fields: None,
            drafts: Default::default(),
            scroll: gpui::ScrollHandle::new(),
            error: None,
            poll_error: None,
            busy: false,
            output: None,
        }
    }

    fn page_identity(&self, page: SettingsPage) -> u64 {
        let mut identity = std::hash::DefaultHasher::new();
        (
            &self.project.space,
            &self.checkout,
            page as u8,
            self.fields.is_some(),
        )
            .hash(&mut identity);
        identity.finish()
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.fields
            .iter()
            .chain(self.drafts.values())
            .any(|fields| fields.is_dirty(cx))
    }
}

struct Fields {
    settings: WorktreeSettings,
    /// Project defaults only. Saved separately from app services, which this
    /// page never edits.
    databases: Vec<ServiceFields>,
    /// What the inputs produced when they were last loaded or saved.
    saved: Option<WorktreeSettings>,
    saved_databases: Vec<ProjectTerminalService>,
    prefix: Entity<ComposerInput>,
    profile: Entity<ComposerInput>,
    paths: Entity<ComposerInput>,
    install: Entity<ComposerInput>,
    create: Entity<ComposerInput>,
    setup: Entity<ComposerInput>,
    activate: Entity<ComposerInput>,
    cleanup: Entity<ComposerInput>,
    remove: Entity<ComposerInput>,
    variables: Entity<ComposerInput>,
    timeout: Entity<ComposerInput>,
    new_file: Entity<ComposerInput>,
    _events: Vec<Subscription>,
}

impl Fields {
    fn is_dirty(&self, cx: &App) -> bool {
        self.settings(cx).ok() != self.saved
            || self.database_drafts(cx) != self.saved_databases
            || !self.new_file.read(cx).text().trim().is_empty()
    }

    fn database_drafts(&self, cx: &App) -> Vec<ProjectTerminalService> {
        self.databases.iter().map(|d| d.service(cx)).collect()
    }

    /// The databases to save, checked field by field so the message can say
    /// which one needs attention.
    fn databases(&self, cx: &App) -> Result<Vec<ProjectTerminalService>, String> {
        let mut databases = self.database_drafts(cx);
        for (database, fields) in databases.iter_mut().zip(&self.databases) {
            let name = database.name.trim().to_string();
            let label = if name.is_empty() {
                "this database".to_string()
            } else {
                format!("“{name}”")
            };
            if name.is_empty() {
                return Err("Give every database a name.".into());
            }
            if database.command.trim().is_empty() {
                return Err(format!("Enter the command that starts {label}."));
            }
            let readiness = database.setup_prerequisite.as_mut().expect("database");
            if readiness.command.trim().is_empty() {
                return Err(format!(
                    "Enter a ready check for {label}, such as pg_isready."
                ));
            }
            readiness.timeout_seconds = fields
                .readiness_timeout
                .read(cx)
                .text()
                .trim()
                .parse()
                .ok()
                .filter(|t| (1..=zeron_proto::MAX_WORKTREE_COMMAND_TIMEOUT_SECONDS).contains(t))
                .ok_or_else(|| format!("Wait between 1 and 7200 seconds for {label}."))?;
        }
        Ok(databases)
    }

    fn settings(&self, cx: &App) -> Result<WorktreeSettings, String> {
        let mut settings = self.settings.clone();
        settings.branch_prefix = self.prefix.read(cx).text().into();
        settings.env_profile = self.profile.read(cx).text().into();
        settings.dependency_paths = self
            .paths
            .read(cx)
            .text()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        settings.install_command = self.install.read(cx).text().into();
        settings.create_command = self.create.read(cx).text().into();
        settings.setup_command = self.setup.read(cx).text().into();
        settings.activate_command = self.activate.read(cx).text().into();
        settings.cleanup_command = self.cleanup.read(cx).text().into();
        settings.remove_command = self.remove.read(cx).text().into();
        settings.command_timeout_seconds = self
            .timeout
            .read(cx)
            .text()
            .trim()
            .parse()
            .map_err(|_| "Enter a timeout in seconds.".to_string())?;
        settings.variables.clear();
        for line in self
            .variables
            .read(cx)
            .text()
            .lines()
            .filter(|l| !l.trim().is_empty())
        {
            let (name, value) = line
                .split_once('=')
                .ok_or_else(|| "Use NAME=value, one environment variable per line.".to_string())?;
            if settings
                .variables
                .insert(name.trim().into(), value.into())
                .is_some()
            {
                return Err("Enter each environment variable only once.".into());
            }
        }
        Ok(settings)
    }
}

impl Shell {
    fn reset_worktree_selects(&mut self) {
        self.worktree_settings.env_modes.clear();
        self.worktree_settings.dependencies = SelectState::default();
        self.worktree_settings.scope = SelectState::default();
        self.worktree_settings.services = SelectState::default();
        self.worktree_settings.directory = SelectState::default();
    }

    pub(super) fn dismiss_worktree_select(&mut self, cx: &mut Context<Self>) -> bool {
        let env_mode = self
            .worktree_settings
            .env_modes
            .iter()
            .find(|(_, state)| state.is_open())
            .map(|(path, _)| path.clone());
        if let Some(path) = env_mode {
            widgets::close_select(
                self,
                move |s| {
                    s.worktree_settings
                        .env_modes
                        .entry(path.clone())
                        .or_default()
                },
                cx,
            );
        } else if self.worktree_settings.scope.is_open() {
            widgets::close_select(self, |s| &mut s.worktree_settings.scope, cx);
        } else if self.worktree_settings.dependencies.is_open() {
            widgets::close_select(self, |s| &mut s.worktree_settings.dependencies, cx);
        } else if self.worktree_settings.services.is_open() {
            widgets::close_select(self, |s| &mut s.worktree_settings.services, cx);
        } else if self.worktree_settings.directory.is_open() {
            widgets::close_select(self, |s| &mut s.worktree_settings.directory, cx);
        } else {
            return false;
        }
        true
    }

    pub(super) fn open_worktree_settings(&mut self, space: String, cx: &mut Context<Self>) {
        let Some(project) = self.terminal_project_context(Some(&space), cx) else {
            return;
        };
        self.worktree_settings.epoch += 1;
        self.reset_worktree_selects();
        self.worktree_settings.lifecycle_visible = false;
        self.worktree_settings.page = SettingsPage::General;
        self.worktree_settings.removal = popover::Popup::default();
        self.worktree_settings.discard = popover::Popup::default();
        self.worktree_settings.editor.open(Editor::new(project));
        self.worktree_settings.poll = None;
        self.poll_worktree_settings(cx);
        cx.notify();
    }

    pub(super) fn show_project_database_settings(&mut self, space: String, cx: &mut Context<Self>) {
        if self
            .worktree_settings
            .editor
            .as_open()
            .is_some_and(|e| e.project.space == space)
        {
            self.select_worktree_settings(None, cx);
            self.select_worktree_settings_page(SettingsPage::Services, cx);
            cx.notify();
        } else {
            self.open_worktree_settings(space, cx);
            self.select_worktree_settings_page(SettingsPage::Services, cx);
        }
    }

    pub(super) fn open_active_worktree_settings(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.terminal_project_context(None, cx) else {
            return;
        };
        let main = self
            .terminal_project_context(Some(&active.space), cx)
            .map(|p| p.cwd);
        self.open_worktree_settings(active.space, cx);
        if main.as_deref() != Some(&active.cwd) {
            self.select_worktree_settings(Some(active.cwd), cx);
        }
    }

    pub(super) fn close_worktree_settings(&mut self, cx: &mut Context<Self>) {
        self.worktree_settings.epoch += 1;
        popover::close_popup(self, cx, |this| &mut this.worktree_settings.editor);
        self.worktree_settings.poll = None;
        popover::close_popup(self, cx, |this| &mut this.worktree_settings.removal);
        popover::close_popup(self, cx, |this| &mut this.worktree_settings.discard);
        self.reset_worktree_selects();
        cx.notify();
    }

    pub(super) fn worktree_settings_dirty(&self, cx: &App) -> bool {
        self.worktree_settings
            .editor
            .as_open()
            .is_some_and(|editor| editor.is_dirty(cx))
    }

    fn worktree_fields(
        &self,
        settings: WorktreeSettings,
        databases: Vec<ProjectTerminalService>,
        cx: &mut Context<Self>,
    ) -> Fields {
        let input = |text: String, placeholder: &str, cx: &mut Context<Self>| {
            let field = cx.new(|cx| ComposerInput::new(placeholder.to_string(), cx));
            field.update(cx, |input, cx| input.set_text(text, cx));
            field
        };
        let prefix = input(settings.branch_prefix.clone(), "feature/", cx);
        let profile = input(settings.env_profile.clone(), "shared", cx);
        let paths = input(
            settings.dependency_paths.join(", "),
            "node_modules, apps/frontend/node_modules",
            cx,
        );
        let install = input(settings.install_command.clone(), "Detect from lockfile", cx);
        let create = input(
            settings.create_command.clone(),
            "Leave blank to use git worktree add",
            cx,
        );
        let setup = input(settings.setup_command.clone(), "Optional setup command", cx);
        let activate = input(
            settings.activate_command.clone(),
            "Optional command when entering this checkout",
            cx,
        );
        let cleanup = input(
            settings.cleanup_command.clone(),
            "Optional command before deleting a worktree",
            cx,
        );
        let remove = input(
            settings.remove_command.clone(),
            "Leave blank to use git worktree remove",
            cx,
        );
        let variables = input(
            settings
                .variables
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("\n"),
            "NAME=value, one per line",
            cx,
        );
        let timeout = input(settings.command_timeout_seconds.to_string(), "900", cx);
        let new_file = input(
            String::new(),
            "Relative path, e.g. apps/backend/.env.local",
            cx,
        );
        let events = [
            &prefix, &profile, &paths, &install, &create, &setup, &activate, &cleanup, &remove,
            &variables, &timeout, &new_file,
        ]
        .into_iter()
        .map(|field| {
            cx.subscribe(field, |_: &mut Shell, _, _: &ComposerInputEvent, cx| {
                cx.notify()
            })
        })
        .collect();
        let databases = databases
            .into_iter()
            .filter(|s| s.is_database())
            .map(|s| self.service_fields(s, false, cx))
            .collect();
        let mut fields = Fields {
            databases,
            settings,
            saved: None,
            saved_databases: Vec::new(),
            prefix,
            profile,
            paths,
            install,
            create,
            setup,
            activate,
            cleanup,
            remove,
            variables,
            timeout,
            new_file,
            _events: events,
        };
        // Read the baseline back through the inputs so formatting differences
        // (such as how dependency paths are joined) never count as edits.
        fields.saved = fields.settings(cx).ok();
        fields.saved_databases = fields.database_drafts(cx);
        fields
    }

    /// Whether the engine owning this project saves databases apart from app
    /// services. An older engine left running after an update cannot, and
    /// would turn a database into an ordinary service that replaces the
    /// project's other services.
    fn databases_supported(&self, cx: &App) -> bool {
        self.worktree_settings
            .editor
            .as_open()
            .is_some_and(|editor| {
                self.state.read(cx).device_supports(
                    &editor.project.device,
                    zeron_proto::capabilities::PROJECT_DATABASES_V1,
                )
            })
    }

    /// What Save sends: this page's settings, plus the databases on the
    /// Project defaults page. App services are never part of it.
    fn worktree_save_payload(
        &self,
        cx: &App,
    ) -> Result<(WorktreeSettings, Option<Vec<ProjectTerminalService>>), String> {
        let editor = self
            .worktree_settings
            .editor
            .as_open()
            .ok_or("Settings are closed.")?;
        let fields = editor
            .fields
            .as_ref()
            .ok_or("Settings are still loading.")?;
        let settings = fields.settings(cx)?;
        let databases = (editor.checkout.is_none() && self.databases_supported(cx))
            .then(|| fields.databases(cx))
            .transpose()?;
        Ok((settings, databases))
    }

    fn worktree_params(&self) -> Option<serde_json::Value> {
        let editor = self.worktree_settings.editor.as_open()?;
        Some(
            editor
                .project
                .params(serde_json::json!({"checkoutPath":editor.checkout})),
        )
    }

    pub(super) fn refresh_worktree_settings(&mut self, cx: &mut Context<Self>) {
        if self.worktree_settings.editor.is_open() {
            self.poll_worktree_settings(cx);
        }
    }

    fn poll_worktree_settings(&mut self, cx: &mut Context<Self>) {
        let Some(params) = self.worktree_params() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let epoch = self.worktree_settings.epoch;
        self.worktree_settings.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let result = engine
                    .client()
                    .call_as::<WorktreeSettingsSnapshot>(
                        methods::GET_WORKTREE_SETTINGS,
                        params.clone(),
                    )
                    .await;
                let keep = this
                    .update(cx, |shell, cx| {
                        if shell.worktree_settings.epoch != epoch
                            || !shell.worktree_settings.editor.is_open()
                        {
                            return false;
                        }
                        let result = result.map_err(|error| error.to_string());
                        if shell.accept_worktree_settings_poll(result, cx) {
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        }));
    }

    /// Applies one poll result and reports whether anything visible changed.
    fn accept_worktree_settings_poll(
        &mut self,
        result: Result<WorktreeSettingsSnapshot, String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(editor) = self.worktree_settings.editor.as_open() else {
            return false;
        };
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let error = Some(error);
                let Some(editor) = self.worktree_settings.editor.open_mut() else {
                    return false;
                };
                if editor.poll_error == error {
                    return false;
                }
                editor.poll_error = error;
                return true;
            }
        };
        let fields = editor.fields.is_none().then(|| {
            self.worktree_fields(snapshot.settings.clone(), snapshot.databases.clone(), cx)
        });
        let Some(editor) = self.worktree_settings.editor.open_mut() else {
            return false;
        };
        let mut changed = editor.poll_error.take().is_some();
        if let Some(fields) = fields {
            self.worktree_settings.lifecycle_visible = !fields.settings.create_command.is_empty()
                || !fields.settings.remove_command.is_empty();
            editor.fields = Some(fields);
            changed = true;
        }
        if editor.snapshot.as_ref() != Some(&snapshot) {
            editor.snapshot = Some(snapshot);
            changed = true;
        }
        changed
    }

    fn select_worktree_settings_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        if !self.worktree_settings.editor.is_open() || self.worktree_settings.page == page {
            return;
        }
        self.worktree_settings.page = page;
        self.reset_worktree_selects();
        if let Some(editor) = self.worktree_settings.editor.open_mut() {
            editor.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        }
        cx.notify();
    }

    fn navigate_worktree_settings(
        &mut self,
        page: SettingsPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = self.worktree_settings.page;
        self.select_worktree_settings_page(page, cx);
        if let Some(focus) = &self.worktree_settings.navigation_focus {
            window.focus(
                &focus[if page == SettingsPage::General {
                    previous as usize
                } else {
                    SettingsPage::General as usize
                }],
                cx,
            );
        }
    }

    fn select_worktree_settings(&mut self, checkout: Option<String>, cx: &mut Context<Self>) {
        let Some(editor) = self.worktree_settings.editor.open_mut() else {
            return;
        };
        if editor.busy || editor.checkout == checkout {
            return;
        }
        if let Some(fields) = editor.fields.take() {
            editor.drafts.insert(editor.checkout.clone(), fields);
        }
        editor.fields = editor.drafts.remove(&checkout);
        self.worktree_settings.lifecycle_visible = editor.fields.as_ref().is_some_and(|f| {
            !f.settings.create_command.is_empty() || !f.settings.remove_command.is_empty()
        });
        editor.checkout = checkout;
        editor.output = None;
        editor.error = None;
        editor.poll_error = None;
        editor.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        self.reset_worktree_selects();
        self.worktree_settings.epoch += 1;
        self.worktree_settings.poll = None;
        self.poll_worktree_settings(cx);
        cx.notify();
    }

    fn worktree_request(
        &mut self,
        method: &'static str,
        settings: Option<WorktreeSettings>,
        databases: Option<Vec<ProjectTerminalService>>,
        cx: &mut Context<Self>,
    ) {
        let Some(mut params) = self.worktree_params() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let resetting = method == methods::SAVE_WORKTREE_SETTINGS && settings.is_none();
        let saving = (method == methods::SAVE_WORKTREE_SETTINGS)
            .then(|| settings.clone().map(|s| (s, databases.clone())))
            .flatten();
        if method == methods::SAVE_WORKTREE_SETTINGS {
            params["settings"] = serde_json::to_value(settings).unwrap_or_default();
            if let Some(databases) = &databases {
                params["databases"] = serde_json::to_value(databases).unwrap_or_default();
            }
        }
        let epoch = self.worktree_settings.epoch;
        if let Some(editor) = self.worktree_settings.editor.open_mut() {
            editor.busy = true;
            editor.error = None;
        }
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<WorktreeSettingsSnapshot>(method, params)
                .await;
            this.update(cx, |shell, cx| {
                if epoch != shell.worktree_settings.epoch {
                    return;
                }
                let Some(editor) = shell.worktree_settings.editor.open_mut() else {
                    return;
                };
                editor.busy = false;
                match result {
                    Ok(snapshot) => {
                        let (settings, databases) =
                            (snapshot.settings.clone(), snapshot.databases.clone());
                        editor.snapshot = Some(snapshot);
                        // Reload what the engine saved (it trims and fills in
                        // defaults) unless the user kept typing meanwhile.
                        let unchanged = saving.as_ref().is_some_and(|(sent, sent_databases)| {
                            editor.fields.as_ref().is_some_and(|fields| {
                                fields.settings(cx).ok().as_ref() == Some(sent)
                                    && sent_databases.as_ref().is_none_or(|sent| {
                                        fields.databases(cx).ok().as_ref() == Some(sent)
                                    })
                            })
                        });
                        if resetting || unchanged {
                            let fields = shell.worktree_fields(settings, databases, cx);
                            if let Some(editor) = shell.worktree_settings.editor.open_mut() {
                                editor.fields = Some(fields);
                            }
                        } else if let Some((sent, sent_databases)) = saving
                            && let Some(fields) = editor.fields.as_mut()
                        {
                            fields.saved = Some(sent);
                            if let Some(sent) = sent_databases {
                                fields.saved_databases = sent;
                            }
                        }
                    }
                    Err(error) => editor.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Drops a worktree's own app services so it uses the project's again.
    fn use_project_services(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.worktree_settings.editor.open_mut() else {
            return;
        };
        let Some(checkout) = editor.checkout.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let mut context = editor.project.clone();
        context.checkout_override = Some(checkout);
        editor.busy = true;
        editor.error = None;
        let epoch = self.worktree_settings.epoch;
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SAVE_PROJECT_TERMINALS,
                    context.params(serde_json::json!({"useProjectServices": true})),
                )
                .await;
            this.update(cx, |shell, cx| {
                if epoch != shell.worktree_settings.epoch {
                    return;
                }
                if let Some(editor) = shell.worktree_settings.editor.open_mut() {
                    editor.busy = false;
                    if let Err(error) = result {
                        editor.error = Some(error.to_string());
                    }
                }
                shell.poll_worktree_settings(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn show_worktree_log(&mut self, cx: &mut Context<Self>) {
        let Some(params) = self.worktree_params() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let epoch = self.worktree_settings.epoch;
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::GET_WORKTREE_SETUP_LOG, params)
                .await;
            this.update(cx, |shell, cx| {
                if epoch != shell.worktree_settings.epoch {
                    return;
                };
                if let Some(editor) = shell.worktree_settings.editor.open_mut() {
                    match result {
                        Ok(value) => {
                            editor.output = Some(
                                value["output"]
                                    .as_str()
                                    .unwrap_or("No output yet.")
                                    .chars()
                                    .rev()
                                    .take(16_384)
                                    .collect::<String>()
                                    .chars()
                                    .rev()
                                    .collect(),
                            )
                        }
                        Err(error) => editor.error = Some(error.to_string()),
                    };
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply_muute_preset(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.worktree_settings.editor.open_mut() else {
            return;
        };
        let Some(fields) = editor.fields.as_mut() else {
            return;
        };
        fields.settings.env_files.clear();
        fields.settings.dependencies = Deps::Skip;
        fields.settings.commands_in_project = true;
        for input in [&fields.create, &fields.remove, &fields.activate] {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
        fields
            .install
            .update(cx, |input, cx| input.set_text("", cx));
        fields.setup.update(cx,|input,cx|input.set_text("bun run worktrees codex-sync --mode setup --source \"$ZERON_PROJECT_ROOT\" --worktree \"$ZERON_CHECKOUT_ROOT\"",cx));
        fields.cleanup.update(cx,|input,cx|input.set_text("bun run worktrees codex-sync --mode cleanup --source \"$ZERON_PROJECT_ROOT\" --worktree \"$ZERON_CHECKOUT_ROOT\"",cx));
        cx.notify();
    }

    pub(super) fn render_worktree_settings(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.worktree_settings.editor.get()?;
        let navigation_focus = self
            .worktree_settings
            .navigation_focus
            .get_or_insert_with(|| {
                std::array::from_fn(|_| cx.focus_handle().tab_index(0).tab_stop(true))
            })
            .clone();
        let editor = self.worktree_settings.editor.get()?;
        let theme = Theme::of(cx).for_popup();
        let busy = editor.busy;
        let has_scopes = editor.checkout.is_some()
            || editor.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot
                    .worktrees
                    .iter()
                    .any(|tree| tree.path != editor.project.cwd)
            });
        let page = self.worktree_settings.page;
        let compact = viewport.width < px(560.0);
        let padding = px(if compact { 16.0 } else { 24.0 });
        let card_width = px(680.0).min((viewport.width - px(32.0)).max(px(0.0)));
        let mut scopes = vec![None];
        let mut scope_options = vec![SelectOption::new("Project defaults")];
        if let Some(snapshot) = &editor.snapshot {
            for tree in snapshot
                .worktrees
                .iter()
                .filter(|tree| tree.path != editor.project.cwd)
            {
                let label = if tree.branch.is_empty() {
                    std::path::Path::new(&tree.path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                } else {
                    tree.branch.clone()
                };
                scopes.push(Some(tree.path.clone()));
                scope_options.push(SelectOption::new(label));
            }
        }
        if let Some(checkout) = &editor.checkout
            && !scopes.iter().any(|scope| scope.as_ref() == Some(checkout))
        {
            scopes.push(Some(checkout.clone()));
            scope_options.push(SelectOption::new(
                std::path::Path::new(checkout)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            ));
        }
        let selected_scope = scopes
            .iter()
            .position(|scope| scope == &editor.checkout)
            .unwrap_or(0);
        let navigation = div()
            .id("project-settings-navigation")
            .debug_selector(|| "project-settings-navigation".into())
            .min_w_0()
            .max_w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .child(widgets::row_title(&theme, "Settings for"))
            .child(
                widgets::select(
                    "project-settings-scope",
                    "Settings for",
                    &theme,
                    |shell: &mut Shell| &mut shell.worktree_settings.scope,
                )
                .menu_layer(3)
                .options(scope_options, selected_scope)
                .width(220.0)
                .on_select(move |shell, index, _, cx| {
                    if let Some(scope) = scopes.get(index) {
                        shell.select_worktree_settings(scope.clone(), cx);
                    }
                })
                .render(&self.worktree_settings.scope, cx),
            );
        let mut content = div()
            .id("worktree-environment-scroll")
            .debug_selector(|| "worktree-environment-scroll".into())
            .w_full()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .px(padding)
            .pt(px(4.0))
            .pb(px(16.0))
            .overflow_y_scroll()
            .track_scroll(&editor.scroll)
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation());
        if let Some(snapshot) = &editor.snapshot {
            if page == SettingsPage::General
                && let Some(path) = &editor.checkout
                && let Some(tree) = snapshot.worktrees.iter().find(|t| &t.path == path)
            {
                content = content.child(section(&theme, "Environment status"));
                if tree.state.phase == Phase::Preparing {
                    content = content.child(loaders::worktree_setup_indicator(
                        "worktree-environment-progress",
                        &theme,
                        if tree.state.step.is_empty() {
                            "Preparing the environment"
                        } else {
                            &tree.state.step
                        },
                        cx.entity_id(),
                        cx,
                    ));
                } else {
                    content = content.child(note(
                        &theme,
                        if tree.state.step.is_empty() {
                            "Not prepared yet"
                        } else {
                            &tree.state.step
                        },
                    ));
                }
                if let Some(error) = &tree.state.error {
                    content = content.child(
                        div()
                            .mt(px(6.0))
                            .text_color(theme.danger)
                            .text_size(px(12.0))
                            .child(SharedString::from(error.clone())),
                    );
                }
                let preparing = tree.state.phase == Phase::Preparing;
                let mut actions = div().flex().flex_wrap().gap(px(8.0)).mt(px(10.0));
                actions = actions.child(
                    popover::btn_ghost(
                        &theme,
                        if preparing {
                            "Cancel setup"
                        } else if tree.state.phase == Phase::Ready {
                            "Run setup again"
                        } else {
                            "Prepare worktree"
                        },
                        "prepare-worktree",
                    )
                    .id("prepare-worktree")
                    .when(!busy || preparing, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| {
                            this.worktree_request(
                                if preparing {
                                    methods::CANCEL_WORKTREE_SETUP
                                } else {
                                    methods::PREPARE_WORKTREE
                                },
                                None,
                                None,
                                cx,
                            )
                        }))
                    }),
                );
                if tree.state.log_available {
                    actions = actions.child(
                        popover::btn_ghost(&theme, "View output", "worktree-output")
                            .id("worktree-output")
                            .on_click(cx.listener(|this, _, _, cx| this.show_worktree_log(cx))),
                    );
                }
                if snapshot.has_overrides {
                    actions = actions.child(
                        popover::btn_ghost(&theme, "Use project defaults", "reset-worktree")
                            .id("reset-worktree")
                            .when(!busy, |b| {
                                b.on_click(cx.listener(|this, _, _, cx| {
                                    this.worktree_request(
                                        methods::SAVE_WORKTREE_SETTINGS,
                                        None,
                                        None,
                                        cx,
                                    )
                                }))
                            }),
                    );
                }
                content=content.child(actions).child(note(&theme,"Preparation stops this worktree’s services. Save settings before running setup."));
                content = content.child(
                    popover::btn_ghost(&theme, "Remove worktree…", "remove-worktree")
                        .id("remove-worktree")
                        .debug_selector(|| "remove-worktree".into())
                        .mt(px(10.0))
                        .text_color(theme.danger)
                        .when(busy || preparing, |b| b.opacity(0.5).cursor_default())
                        .when(!busy && !preparing, |b| {
                            b.on_click(cx.listener(|this, _, window, cx| {
                                this.begin_worktree_removal(window, cx);
                            }))
                        }),
                );
            }
        }
        if let Some(fields) = &editor.fields {
            let mut panel = div()
                .id(SharedString::from(format!(
                    "project-settings-page-{}",
                    page.key()
                )))
                .debug_selector(move || format!("project-settings-page-{}", page.key()))
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(24.0));
            if page == SettingsPage::General {
                let branch = widgets::card_row(&theme, false)
                    .id("project-settings-branch-row")
                    .debug_selector(|| "project-settings-branch-row".into())
                    .child(
                        div()
                            .min_w(px(160.0))
                            .max_w_full()
                            .flex_1()
                            .child(widgets::row_title(&theme, "Branch prefix"))
                            .child(note(
                                &theme,
                                if editor.checkout.is_none() {
                                    "Added to new branches. Leave blank for none."
                                } else {
                                    "Set in project defaults for new worktrees."
                                },
                            )),
                    )
                    .when(editor.checkout.is_none(), |row| {
                        row.child(field(&theme, &fields.prefix).w(px(200.0)).max_w_full())
                    });
                panel =
                    panel.child(
                        widgets::section_card(&theme)
                            .mt_0()
                            .id("project-settings-general-block")
                            .debug_selector(|| "project-settings-general-block".into())
                            .child(widgets::card_row(&theme, true).child(
                                self.render_project_icon_controls(&editor.project.space, cx),
                            ))
                            .child(branch),
                    );
                let mut sections = widgets::section_card(&theme).mt_0();
                for (index, destination) in SettingsPage::ALL.into_iter().skip(1).enumerate() {
                    let id =
                        SharedString::from(format!("project-settings-open-{}", destination.key()));
                    let focus = navigation_focus[destination as usize].clone();
                    let summary = settings_summary(destination, editor, fields, cx);
                    sections = sections.child(
                        widgets::card_row(&theme, index == 0).py_0().child(
                            div()
                                .id(id.clone())
                                .debug_selector({
                                    let id = id.clone();
                                    move || id.to_string()
                                })
                                .track_focus(&focus)
                                .tab_index(0)
                                .role(gpui::Role::Button)
                                .aria_label(format!("{} settings", destination.title()))
                                .w_full()
                                .min_w_0()
                                .min_h(px(60.0))
                                .py(px(10.0))
                                .flex()
                                .items_center()
                                .gap(px(12.0))
                                .cursor_pointer()
                                .group(SharedString::from(format!("{id}-group")))
                                .border_1()
                                .border_color(gpui::transparent_black())
                                .focus_visible(|s| s.rounded(px(6.0)).border_color(theme.accent))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(widgets::row_title(&theme, destination.title()))
                                        .child(note(&theme, &summary).mt(px(2.0))),
                                )
                                .child(
                                    icon(icons::ALT_ARROW_RIGHT)
                                        .size(px(16.0))
                                        .flex_none()
                                        .text_color(theme.text_faint)
                                        .group_hover(
                                            SharedString::from(format!("{id}-group")),
                                            |s| s.text_color(theme.text),
                                        ),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.navigate_worktree_settings(destination, window, cx)
                                }))
                                .on_key_down(cx.listener(
                                    move |this, event: &gpui::KeyDownEvent, window, cx| {
                                        if !event.is_held
                                            && matches!(
                                                event.keystroke.key.as_str(),
                                                "enter" | "space"
                                            )
                                        {
                                            this.navigate_worktree_settings(
                                                destination,
                                                window,
                                                cx,
                                            );
                                            cx.stop_propagation();
                                        }
                                    },
                                )),
                        ),
                    );
                }
                panel = panel.child(sections);
            }
            if page == SettingsPage::Environment {
                panel = panel.child(self.render_environment_settings(fields, &theme, window, cx));
            }
            if page == SettingsPage::Workflow {
                if editor
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.detected_workflow.as_deref() == Some("muute"))
                {
                    panel = panel.child(widgets::section_card(&theme).mt_0().child(
                        widgets::card_row(&theme, true)
                            .child(div().flex_1().min_w_0()
                                .child(widgets::row_title(&theme, "Muute workflow"))
                                .child(note(&theme, "Prepare environment files, ports and databases with codex-sync.")))
                            .child(widgets::action(&theme, widgets::ActionTone::Outlined, "muute-worktree-preset", window)
                                .child("Apply preset").role(gpui::Role::Button).aria_label("Apply Muute worktree preset")
                                .on_click(cx.listener(|this, _, _, cx| this.apply_muute_preset(cx))))));
                }
                let group = widgets::section_card(&theme)
                    .mt_0()
                    .child(command_row(
                        &theme,
                        true,
                        "Setup",
                        "After creation, before agents and services start",
                        &fields.setup,
                    ))
                    .child(command_row(
                        &theme,
                        false,
                        "Activation",
                        "When switching to this worktree",
                        &fields.activate,
                    ))
                    .child(command_row(
                        &theme,
                        false,
                        "Cleanup",
                        "Before removing this worktree",
                        &fields.cleanup,
                    ))
                    .child(
                        widgets::card_row(&theme, false)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(130.0))
                                    .child(widgets::row_title(&theme, "Run commands in")),
                            )
                            .child(
                                widgets::select(
                                    "worktree-command-directory",
                                    "Command folder",
                                    &theme,
                                    |s: &mut Shell| &mut s.worktree_settings.directory,
                                )
                                .menu_layer(3)
                                .options(
                                    [
                                        SelectOption::new("Worktree folder"),
                                        SelectOption::new("Main project folder"),
                                    ],
                                    usize::from(fields.settings.commands_in_project),
                                )
                                .width(220.0)
                                .on_select(|this, index, _, cx| {
                                    if let Some(f) = this
                                        .worktree_settings
                                        .editor
                                        .open_mut()
                                        .and_then(|e| e.fields.as_mut())
                                    {
                                        f.settings.commands_in_project = index == 1;
                                    }
                                    cx.notify();
                                })
                                .render(&self.worktree_settings.directory, cx),
                            ),
                    );
                panel = panel.child(group);
                let expanded = self.worktree_settings.lifecycle_visible;
                let mut custom = widgets::section_card(&theme).mt_0().child(
                    widgets::card_row(&theme, true).py_0().child(
                        choice(
                            &theme,
                            "worktree-lifecycle-options",
                            "Custom Git commands",
                            false,
                            window,
                        )
                        .w_full()
                        .min_h(px(48.0))
                        .px_0()
                        .justify_between()
                        .aria_expanded(expanded)
                        .child(
                            icons::disclosure_chevron(
                                "project-settings-git-chevron",
                                expanded,
                                window,
                                cx,
                            )
                            .size(px(14.0))
                            .text_color(theme.text_muted),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.worktree_settings.lifecycle_visible =
                                !this.worktree_settings.lifecycle_visible;
                            cx.notify();
                        }))
                        .on_key_down(cx.listener(
                            |this, event: &gpui::KeyDownEvent, _, cx| {
                                if !event.is_held
                                    && matches!(event.keystroke.key.as_str(), "enter" | "space")
                                {
                                    this.worktree_settings.lifecycle_visible =
                                        !this.worktree_settings.lifecycle_visible;
                                    cx.notify();
                                    cx.stop_propagation();
                                }
                            },
                        )),
                    ),
                );
                if expanded {
                    if editor.checkout.is_none() {
                        custom = custom.child(command_row(&theme, false, "Create worktree", "Leave blank to use Git. The command must create the linked checkout at \"$worktreepath\" on \"$branchname\" from \"$basebranch\".", &fields.create));
                    }
                    custom = custom.child(command_row(&theme, false, "Remove worktree", "Leave blank to use Git. Runs after cleanup, in the main project folder, and must remove \"$worktreepath\".", &fields.remove));
                }
                panel = panel.child(custom).child(
                    div().id("project-settings-command-variables").px(px(8.0)).child(note(&theme, "Command variables ⓘ"))
                        .tooltip(widgets::text_tooltip("Use double quotes around variables: \"$branchname\" (Git branch), \"$worktreename\" (folder name), \"$worktreepath\" (worktree path), \"$projectroot\" (main project path).")));
            }
            if page == SettingsPage::Services {
                panel = panel
                    .child(
                        widgets::section(
                            &theme,
                            "Shared databases",
                            self.render_database_section(editor, fields, &theme, busy, window, cx),
                        )
                        .mt_0(),
                    )
                    .child(
                        widgets::section(
                            &theme,
                            "App services",
                            self.render_app_service_section(
                                editor, fields, &theme, busy, window, cx,
                            ),
                        )
                        .mt_0(),
                    );
            }
            if page == SettingsPage::Advanced {
                panel = panel.child(widgets::section_card(&theme).mt_0()
                    .child(widgets::card_row(&theme, true)
                        .child(div().flex_1().min_w(px(140.0))
                            .child(widgets::row_title(&theme, "Command timeout"))
                            .child(note(&theme, "Maximum time for each workflow command")))
                        .child(div().flex().items_center().gap(px(8.0))
                            .child(field(&theme, &fields.timeout).w(px(80.0)))
                            .child(note(&theme, "seconds").mt_0())))
                    .child(command_row(&theme, false, "Environment variables", "NAME=value, one per line. Available to commands, services and worktree terminals.", &fields.variables)))
                    .child(note(&theme, "Settings and environment backups are stored on this device.").px(px(8.0)));
            }
            content = content.child(panel);
        } else {
            content = content.child(note(&theme, "Loading worktree settings…"));
        }
        if let Some(output) = &editor.output
            && page == SettingsPage::General
        {
            content = content.child(section(&theme, "Setup output")).child(
                div()
                    .font_family("monospace")
                    .text_size(px(11.0))
                    .p(px(12.0))
                    .rounded(px(8.0))
                    .bg(theme.wash(0.04))
                    .child(SharedString::from(output.clone())),
            );
        }
        let dirty = editor
            .fields
            .as_ref()
            .is_some_and(|fields| fields.is_dirty(cx));
        let other_dirty = editor.drafts.values().any(|fields| fields.is_dirty(cx));
        let mut footer = div()
            .id("project-settings-footer")
            .debug_selector(|| "project-settings-footer".into())
            .flex_none()
            .px(padding)
            .py(px(16.0));
        if let Some(error) = editor.error.as_ref().or(editor.poll_error.as_ref()) {
            footer = footer.child(
                div()
                    .mb(px(10.0))
                    .text_size(px(12.0))
                    .text_color(theme.danger)
                    .child(SharedString::from(error.clone())),
            );
        }
        footer = footer.child(
            div()
                .flex()
                .when(compact, |el| el.flex_col().items_end())
                .when(!compact, |el| el.items_center())
                .gap(px(12.0))
                .child(
                    div()
                        .id("project-settings-status")
                        .debug_selector(|| "project-settings-status".into())
                        .when(compact, |el| el.w_full())
                        .when(!compact, |el| el.flex_1())
                        .min_w_0()
                        .text_size(px(11.0))
                        .text_color(theme.text_muted)
                        .when(dirty && !busy, |el| el.text_color(theme.warning))
                        .child(if busy {
                            "Working…"
                        } else if dirty {
                            "Unsaved changes"
                        } else if other_dirty {
                            "Other settings have unsaved changes"
                        } else {
                            "Saved"
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Close", "close-worktree-settings")
                                .id("close-worktree-settings")
                                .debug_selector(|| "close-worktree-settings".into())
                                .role(gpui::Role::Button)
                                .aria_label("Close")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.request_close_form(
                                        UnsavedForm::WorktreeSettings,
                                        window,
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            popover::btn_primary(
                                &theme,
                                if busy { "Working…" } else { "Save settings" },
                                "save-worktree-settings",
                            )
                            .id("save-worktree-settings")
                            .debug_selector(|| "save-worktree-settings".into())
                            .role(gpui::Role::Button)
                            .aria_label("Save settings")
                            .when(busy || !dirty || editor.fields.is_none(), |b| {
                                b.opacity(0.5).cursor_default()
                            })
                            .when(
                                !busy && dirty && editor.fields.is_some(),
                                |b| {
                                    b.on_click(cx.listener(|this, _, _, cx| {
                                        match this.worktree_save_payload(cx) {
                                            Ok((settings, databases)) => this.worktree_request(
                                                methods::SAVE_WORKTREE_SETTINGS,
                                                Some(settings),
                                                databases,
                                                cx,
                                            ),
                                            Err(error) => {
                                                if let Some(editor) =
                                                    this.worktree_settings.editor.open_mut()
                                                {
                                                    editor.error = Some(error);
                                                }
                                                cx.notify();
                                            }
                                        }
                                    }))
                                },
                            ),
                        ),
                ),
        );
        let mut metadata = vec![
            div()
                .truncate()
                .child(SharedString::from(editor.project.name.clone()))
                .into_any_element(),
        ];
        if editor.checkout.is_some() {
            metadata.push(
                div()
                    .child(
                        if editor.snapshot.as_ref().is_some_and(|s| s.has_overrides) {
                            "Customized"
                        } else {
                            "Using project defaults"
                        },
                    )
                    .into_any_element(),
            );
        }
        let header = div()
            .id("project-settings-header")
            .debug_selector(|| "project-settings-header".into())
            .flex_none()
            .px(padding)
            .pt(px(24.0))
            .pb(px(20.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .when(page != SettingsPage::General, |el| {
                        el.child(
                            widgets::action(
                                &theme,
                                widgets::ActionTone::Quiet,
                                "project-settings-back",
                                window,
                            )
                            .id("project-settings-back")
                            .debug_selector(|| "project-settings-back".into())
                            .track_focus(&navigation_focus[SettingsPage::General as usize])
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .aria_label("Back to project settings")
                            .size(px(32.0))
                            .p_0()
                            .justify_center()
                            .focus_visible(|s| s.border_2().border_color(theme.accent))
                            .child(
                                icon(icons::ALT_ARROW_LEFT)
                                    .size(px(18.0))
                                    .text_color(theme.text_muted),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.navigate_worktree_settings(SettingsPage::General, window, cx)
                            }))
                            .on_key_down(cx.listener(
                                |this, event: &gpui::KeyDownEvent, window, cx| {
                                    if !event.is_held
                                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                                    {
                                        this.navigate_worktree_settings(
                                            SettingsPage::General,
                                            window,
                                            cx,
                                        );
                                        cx.stop_propagation();
                                    }
                                },
                            )),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                widgets::page_header(
                                    &theme,
                                    if page == SettingsPage::General {
                                        "Project settings"
                                    } else {
                                        page.title()
                                    },
                                    None,
                                )
                                .px_0(),
                            )
                            .child(widgets::meta_line(&theme, metadata)),
                    ),
            )
            .when(has_scopes, |el| {
                el.child(div().mt(px(20.0)).child(navigation))
            });
        // Keep the modal entrance and exit on the shared deferred layer; only
        // content replacements get a separate, first-frame-deferred fade.
        let shown = editor.page_identity(page);
        let t = motion::deferred_swap_entrance(
            window,
            "project-settings-page-transition",
            shown,
            motion::FADE_QUICK,
            motion::reduced_motion(cx),
        );
        let outlet = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .opacity(t)
            .child(
                crate::edge_fade::edge_faded(16.0, true, true, content)
                    .fade_overflow_y(&editor.scroll),
            );
        let height = px(640.0).min((viewport.height - px(48.0)).max(px(0.0)));
        let card = popover::dialog_card(&theme)
            .id("project-settings-card")
            .debug_selector(|| "project-settings-card".into())
            .w(card_width)
            .h(height)
            .p_0()
            .bg(theme.surface_dialog)
            .border_color(theme.border)
            .overflow_hidden()
            .child(header)
            .child(
                div()
                    .id("project-settings-body")
                    .debug_selector(|| "project-settings-body".into())
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex_col()
                    .child(outlet),
            )
            .child(footer);
        motion::keyed_fade_frames(window);
        Some(popover::modal(
            "worktree-settings",
            viewport,
            card.into_any_element(),
            self.worktree_settings.editor.closing_since(),
        ))
    }
}

impl Shell {
    /// Databases are edited only here, on Project defaults. Worktree pages
    /// show them read-only because every worktree shares them.
    fn render_database_section(
        &self,
        editor: &Editor,
        fields: &Fields,
        theme: &Theme,
        busy: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut el = form_block(theme).child(note(
            theme,
            "Run in the main project folder. Ready checks finish before worktree setup; databases stay running across worktree switches.",
        ));
        if editor.checkout.is_some() {
            let databases = editor
                .snapshot
                .as_ref()
                .map(|s| s.databases.as_slice())
                .unwrap_or_default();
            el = el.child(summary_list(
                theme,
                databases
                    .iter()
                    .map(|db| summary_row(theme, icons::HARD_DRIVE, &db.name, db.command.clone())),
                "No shared databases.",
            ));
            return el.child(
                choice(
                    theme,
                    "database-project-defaults",
                    "Edit in Project defaults",
                    false,
                    window,
                )
                .mt(px(6.0))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.select_worktree_settings(None, cx);
                    this.select_worktree_settings_page(SettingsPage::Services, cx);
                })),
            );
        }
        if !self.databases_supported(cx) {
            return el.child(widgets::warning_strip(
                theme,
                "This window is connected to an older Zeron engine that can’t save databases safely. Quit Zeron completely, including other Zeron windows, then open it again.",
            ));
        }
        for database in &fields.databases {
            let id = database.id.clone();
            let name = database.name.read(cx).text().trim().to_string();
            el = el.child(
                div()
                    .mt(px(12.0))
                    .pt(px(12.0))
                    .border_t_1()
                    .border_color(widgets::row_divider(theme))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                icon(icons::HARD_DRIVE)
                                    .size(px(14.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(SharedString::from(if name.is_empty() {
                                        "New database".to_string()
                                    } else {
                                        name
                                    })),
                            )
                            .child(
                                choice(
                                    theme,
                                    format!("remove-database-{id}"),
                                    "Remove",
                                    false,
                                    window,
                                )
                                .text_color(theme.danger)
                                .aria_label("Remove database")
                                .when(!busy, |el| {
                                    el.on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(f) = this
                                            .worktree_settings
                                            .editor
                                            .open_mut()
                                            .and_then(|e| e.fields.as_mut())
                                        {
                                            f.databases.retain(|d| d.id != id);
                                        }
                                        cx.notify();
                                    }))
                                }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(12.0))
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "project-settings-database-name-{}",
                                        database.id
                                    )))
                                    .debug_selector({
                                        let id = database.id.clone();
                                        move || format!("project-settings-database-name-{id}")
                                    })
                                    .flex_1()
                                    .min_w(px(140.0))
                                    .max_w_full()
                                    .child(label(theme, "Name"))
                                    .child(field(theme, &database.name)),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "project-settings-database-directory-{}",
                                        database.id
                                    )))
                                    .debug_selector({
                                        let id = database.id.clone();
                                        move || format!("project-settings-database-directory-{id}")
                                    })
                                    .w(px(170.0))
                                    .max_w_full()
                                    .flex_shrink_0()
                                    .child(label(theme, "Run in folder"))
                                    .child(field(theme, &database.directory)),
                            ),
                    )
                    .child(label(theme, "Start command"))
                    .child(field(theme, &database.command))
                    .child(label(theme, "Ready when this command succeeds"))
                    .child(field(theme, &database.readiness))
                    .child(
                        div()
                            .mt(px(10.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .child("Wait up to")
                            .child(field(theme, &database.readiness_timeout).w(px(80.0)))
                            .child("seconds"),
                    ),
            );
        }
        if fields.databases.is_empty() {
            el = el.child(note(theme, "No shared databases yet."));
        }
        let can_add = fields.databases.len()
            + editor.snapshot.as_ref().map_or(0, |s| s.app_services.len())
            < 12;
        el.child(
            div().flex().mt(px(10.0)).child(
                widgets::action(
                    theme,
                    widgets::ActionTone::Outlined,
                    "add-worktree-database",
                    window,
                )
                .child("Add database")
                .role(gpui::Role::Button)
                .aria_label("Add database")
                .when(!can_add || busy, |el| el.opacity(0.4).cursor_default())
                .when(can_add && !busy, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| {
                        let database = this.service_fields(
                            ProjectTerminalService {
                                id: uuid::Uuid::new_v4().to_string(),
                                name: "Databases".into(),
                                command: String::new(),
                                directory: ".".into(),
                                restart_on_failure: false,
                                setup_prerequisite: Some(zeron_proto::ProjectServiceReadiness {
                                    command: String::new(),
                                    timeout_seconds: 120,
                                }),
                            },
                            false,
                            cx,
                        );
                        if let Some(f) = this
                            .worktree_settings
                            .editor
                            .open_mut()
                            .and_then(|e| e.fields.as_mut())
                        {
                            f.databases.push(database);
                        }
                        cx.notify();
                    }))
                }),
            ),
        )
    }

    /// App services are edited in the services editor; this page only shows
    /// which ones apply and how they follow worktree switches.
    fn render_app_service_section(
        &self,
        editor: &Editor,
        fields: &Fields,
        theme: &Theme,
        busy: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let snapshot = editor.snapshot.as_ref();
        let own =
            editor.checkout.is_some() && snapshot.is_some_and(|s| s.settings.services.is_some());
        let mut el = form_block(theme).child(note(
            theme,
            match (editor.checkout.is_some(), own) {
                (false, _) => "Dev servers start after setup in the active worktree.",
                (true, true) => "This worktree runs its own list of app services.",
                (true, false) => "This worktree uses the project’s app services.",
            },
        ));
        if self.databases_supported(cx) {
            let apps = snapshot
                .map(|s| s.app_services.as_slice())
                .unwrap_or_default();
            el = el.child(summary_list(
                theme,
                apps.iter().map(|app| {
                    let detail = if app.directory.is_empty() || app.directory == "." {
                        app.command.clone()
                    } else {
                        format!("{} · {}", app.command, app.directory)
                    };
                    summary_row(theme, icons::TERMINAL, &app.name, detail)
                }),
                "No app services yet.",
            ));
        }
        let edit_label = match (editor.checkout.is_some(), own) {
            (false, _) => "Edit app services…",
            (true, true) => "Edit this worktree’s services…",
            (true, false) => "Customize for this worktree…",
        };
        let mut actions = div().flex().flex_wrap().gap(px(8.0)).mt(px(10.0)).child(
            widgets::action(
                theme,
                widgets::ActionTone::Outlined,
                "worktree-service-settings",
                window,
            )
            .child(edit_label)
            .role(gpui::Role::Button)
            .aria_label(edit_label)
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(editor) = this.worktree_settings.editor.as_open() {
                    let mut context = editor.project.clone();
                    context.checkout_override = editor.checkout.clone();
                    if let Some(cwd) = &editor.checkout {
                        context.cwd = cwd.clone();
                    }
                    this.open_service_settings_context(context, cx);
                }
            })),
        );
        if own {
            actions = actions.child(
                widgets::action(
                    theme,
                    widgets::ActionTone::Quiet,
                    "inherit-worktree-services",
                    window,
                )
                .child("Use project services")
                .role(gpui::Role::Button)
                .aria_label("Use project services")
                .when(busy, |el| el.opacity(0.4).cursor_default())
                .when(!busy, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| this.use_project_services(cx)))
                }),
            );
        }
        el = el.child(actions);
        if editor.checkout.is_none() {
            let service_modes = [Services::FollowActive, Services::Parallel];
            el = el.child(label(theme, "When you switch worktrees")).child(
                widgets::select(
                    "worktree-service-mode",
                    "Service groups",
                    theme,
                    |s: &mut Shell| &mut s.worktree_settings.services,
                )
                .menu_layer(3)
                .options(
                    [
                        SelectOption::new("Move the services to the active worktree"),
                        SelectOption::new("Keep separate services in each worktree"),
                    ],
                    usize::from(fields.settings.service_mode == Services::Parallel),
                )
                .width(
                    360.0_f32
                        .min(f32::from(window.viewport_size().width) - 98.0)
                        .max(0.0),
                )
                .on_select(move |this, index, _, cx| {
                    if let Some(f) = this
                        .worktree_settings
                        .editor
                        .open_mut()
                        .and_then(|e| e.fields.as_mut())
                    {
                        f.settings.service_mode = service_modes[index];
                    }
                    cx.notify();
                })
                .render(&self.worktree_settings.services, cx),
            );
            el = el.child(note(theme, service_help(fields.settings.service_mode)));
            if fields.settings.service_mode == Services::Parallel
                && fields
                    .settings
                    .env_files
                    .iter()
                    .any(|f| f.mode == Env::Follow)
            {
                el = el.child(note(theme, "Choose Copy or Link for your env files before saving separate service groups. Follow uses one shared active environment."));
            }
        }
        el
    }
}

fn summary_list(
    theme: &Theme,
    rows: impl Iterator<Item = gpui::Div>,
    empty: &'static str,
) -> gpui::Div {
    let mut list = div().mt(px(10.0)).py(px(6.0));
    let mut any = false;
    for row in rows {
        any = true;
        list = list.child(row);
    }
    if !any {
        list = list.child(
            div()
                .py(px(4.0))
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child(empty),
        );
    }
    list
}

fn summary_row(theme: &Theme, glyph: &'static str, name: &str, detail: String) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .py(px(4.0))
        .child(
            icon(glyph)
                .size(px(13.0))
                .flex_none()
                .text_color(theme.text_muted),
        )
        .child(
            div()
                .flex_none()
                .max_w(px(220.0))
                .truncate()
                .text_size(px(12.0))
                .text_color(theme.text)
                .child(SharedString::from(name.to_string())),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(theme.font_mono.clone())
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .child(SharedString::from(detail)),
        )
}

fn command_row(
    theme: &Theme,
    first: bool,
    title: &'static str,
    detail: &'static str,
    input: &Entity<ComposerInput>,
) -> gpui::Div {
    widgets::card_row(theme, first)
        .flex_col()
        .items_stretch()
        .gap(px(8.0))
        .child(
            div()
                .child(widgets::row_title(theme, title))
                .child(note(theme, detail).mt(px(2.0))),
        )
        .child(field(theme, input))
}

fn settings_summary(page: SettingsPage, editor: &Editor, fields: &Fields, cx: &App) -> String {
    match page {
        SettingsPage::General => String::new(),
        SettingsPage::Environment => {
            let files = match fields.settings.env_files.len() {
                0 => "No env files".to_string(),
                1 => fields.settings.env_files[0].path.clone(),
                count => format!("{count} env files"),
            };
            let dependencies = match fields.settings.dependencies {
                Deps::Copy => "Copy dependencies",
                Deps::Install => "Install dependencies",
                Deps::Link => "Shared dependencies",
                Deps::Skip => "Custom dependency setup",
            };
            format!("{files} · {dependencies}")
        }
        SettingsPage::Workflow => {
            let mut commands = Vec::new();
            for (input, name) in [
                (&fields.setup, "Setup"),
                (&fields.activate, "Activation"),
                (&fields.cleanup, "Cleanup"),
            ] {
                if !input.read(cx).text().trim().is_empty() {
                    commands.push(name);
                }
            }
            if commands.is_empty() {
                "No workflow commands".into()
            } else {
                commands.join(" · ")
            }
        }
        SettingsPage::Services => {
            let databases = if editor.checkout.is_none() {
                fields.databases.len()
            } else {
                editor.snapshot.as_ref().map_or(0, |s| s.databases.len())
            };
            let services = editor.snapshot.as_ref().map_or(0, |s| s.app_services.len());
            format!(
                "{databases} {} · {services} app {}",
                if databases == 1 {
                    "database"
                } else {
                    "databases"
                },
                if services == 1 { "service" } else { "services" }
            )
        }
        SettingsPage::Advanced => {
            let variables = fields
                .variables
                .read(cx)
                .text()
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count();
            format!(
                "{}s timeout · {variables} {}",
                fields.timeout.read(cx).text(),
                if variables == 1 {
                    "variable"
                } else {
                    "variables"
                }
            )
        }
    }
}

fn choice(
    theme: &Theme,
    id: impl Into<SharedString>,
    name: impl Into<SharedString>,
    selected: bool,
    window: &mut Window,
) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    let name = name.into();
    div()
        .id(id.clone())
        .min_w_0()
        .max_w_full()
        .flex()
        .items_center()
        .gap(px(7.0))
        .px(px(10.0))
        .py(px(7.0))
        .rounded(px(6.0))
        .text_size(px(12.0))
        .text_color(if selected {
            theme.text
        } else {
            theme.text_muted
        })
        .hover_bg(
            SharedString::from(format!("choice-wash-{id}")),
            window,
            if selected {
                theme.wash(0.08)
            } else {
                gpui::transparent_black()
            },
            theme.element_hover,
        )
        .cursor_pointer()
        .tab_index(0)
        .role(gpui::Role::Button)
        .when(!name.is_empty(), |el| el.aria_label(name.clone()))
        .child(name)
}
fn section(theme: &Theme, text: &'static str) -> impl IntoElement {
    widgets::section_label(theme, text).mt(px(24.0)).mb(px(8.0))
}
fn form_block(theme: &Theme) -> gpui::Div {
    widgets::section_card(theme)
        .mt_0()
        .px(px(16.0))
        .pt(px(4.0))
        .pb(px(16.0))
}

fn note(theme: &Theme, text: &str) -> gpui::Div {
    div()
        .mt(px(6.0))
        .text_size(crate::typography::ui_rems(12.0))
        .line_height(crate::typography::ui_rems(16.0))
        .text_color(theme.text_muted)
        .child(SharedString::from(text.to_string()))
}
fn label(theme: &Theme, text: &'static str) -> impl IntoElement {
    div()
        .mt(px(12.0))
        .mb(px(5.0))
        .text_size(px(12.0))
        .text_color(theme.text_muted)
        .child(text)
}
fn field(theme: &Theme, input: &Entity<ComposerInput>) -> gpui::Div {
    popover::dialog_field(input.clone().into_any_element())
        .bg(widgets::select_fill(theme, false))
        .border_color(gpui::transparent_black())
        .rounded(px(8.0))
}
fn env_label(mode: Env) -> &'static str {
    match mode {
        Env::Copy => "Copy",
        Env::Link => "Link",
        Env::Follow => "Follow",
        Env::Skip => "Skip",
    }
}

fn service_help(mode: Services) -> &'static str {
    match mode {
        Services::FollowActive => {
            "One set of app services runs at a time. Switching to a conversation in another worktree stops them, prepares that worktree, then restarts the ones that were running there. Shared databases keep running."
        }
        Services::Parallel => {
            "Each worktree keeps its own app services running. Give each worktree different ports through env files or your setup command. Shared databases are still shared."
        }
    }
}

#[cfg(feature = "source-control-fixture")]
impl Shell {
    pub fn fixture_open_worktree_settings(&mut self, cx: &mut Context<Self>) {
        self.open_worktree_settings("fixture-space".into(), cx);
    }
    pub fn fixture_worktree_settings_state(&self, cx: &App) -> serde_json::Value {
        self.worktree_settings
            .editor
            .as_open()
            .map(|e| {
                serde_json::json!({
                    "checkout":e.checkout,"busy":e.busy,
                    "error":e.error.as_ref().or(e.poll_error.as_ref()),
                    "worktrees":e.snapshot.as_ref().map(|s|&s.worktrees),
                    "settings":e.fields.as_ref().and_then(|f|f.settings(cx).ok()),
                    "hasOverrides":e.snapshot.as_ref().map(|s|s.has_overrides),
                    "workflow":e.snapshot.as_ref().and_then(|s|s.detected_workflow.clone()),
                    "dependencyMenuOpen":self.worktree_settings.dependencies.is_open(),
                    "serviceMenuOpen":self.worktree_settings.services.is_open(),
                    "directoryMenuOpen":self.worktree_settings.directory.is_open(),
                    "removalOpen": self.worktree_settings.removal.is_open()
                })
            })
            .unwrap_or(serde_json::Value::Null)
    }
}

#[cfg(test)]
mod layout_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};
    fn shell(cx: &mut TestAppContext, path: &std::path::Path) -> gpui::WindowHandle<Shell> {
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
                    data_dir: path.to_path_buf(),
                    ipc_port: 0,
                    edge_url: String::new(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: zeron_proto::HarnessId::Mock,
                },
                cx,
            )
        })
    }

    fn database(id: &str, name: &str) -> zeron_proto::ProjectTerminalService {
        zeron_proto::ProjectTerminalService {
            id: id.into(),
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

    fn supports_databases(shell: &mut Shell, supported: bool, cx: &mut Context<Shell>) {
        shell.state.update(cx, |state, _| {
            state.devices = vec![
                serde_json::from_value(serde_json::json!({
                    "id": "local", "name": "local", "platform": "linux",
                    "capabilities": if supported {
                        vec![zeron_proto::capabilities::PROJECT_DATABASES_V1]
                    } else {
                        vec![]
                    },
                }))
                .unwrap(),
            ];
        });
    }

    #[gpui::test]
    fn database_drafts_save_apart_from_app_services(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let window = shell(cx, dir.path());
        window
            .update(cx, |shell, _, cx| {
                supports_databases(shell, true, cx);
                let app = zeron_proto::ProjectTerminalService {
                    setup_prerequisite: None,
                    ..database("backend", "Backend")
                };
                // A page that still carries a copy of the app services (as the
                // previous build did) must not send them anywhere new.
                let settings = WorktreeSettings {
                    setup_command: "bun run migrate".into(),
                    services: Some(zeron_proto::ProjectTerminalConfig {
                        services: vec![app.clone()],
                    }),
                    ..Default::default()
                };
                let mut editor = Editor::new(test_project());
                editor.fields =
                    Some(shell.worktree_fields(settings, vec![database("db", "Databases")], cx));
                shell.worktree_settings.editor.open(editor);
                assert!(
                    !shell.worktree_settings_dirty(cx),
                    "loaded values are not edits"
                );
                let fields = shell
                    .worktree_settings
                    .editor
                    .as_open()
                    .unwrap()
                    .fields
                    .as_ref()
                    .unwrap();
                fields.databases[0]
                    .readiness_timeout
                    .update(cx, |input, cx| input.set_text("180", cx));
                assert!(
                    shell.worktree_settings_dirty(cx),
                    "database edits count as unsaved"
                );
                let (settings, databases) = shell.worktree_save_payload(cx).unwrap();
                assert_eq!(settings.setup_command, "bun run migrate");
                let databases = databases.unwrap();
                assert_eq!(databases.len(), 1);
                assert_eq!(
                    databases[0]
                        .setup_prerequisite
                        .as_ref()
                        .unwrap()
                        .timeout_seconds,
                    180
                );
                assert!(databases.iter().all(|d| d.is_database()));

                let fields = shell
                    .worktree_settings
                    .editor
                    .as_open()
                    .unwrap()
                    .fields
                    .as_ref()
                    .unwrap();
                fields.databases[0]
                    .readiness_timeout
                    .update(cx, |input, cx| input.set_text("soon", cx));
                assert_eq!(
                    shell.worktree_save_payload(cx).unwrap_err(),
                    "Wait between 1 and 7200 seconds for “Databases”."
                );
                let fields = shell
                    .worktree_settings
                    .editor
                    .as_open()
                    .unwrap()
                    .fields
                    .as_ref()
                    .unwrap();
                fields.databases[0]
                    .readiness
                    .update(cx, |input, cx| input.set_text(" ", cx));
                assert!(
                    shell
                        .worktree_save_payload(cx)
                        .unwrap_err()
                        .contains("ready check")
                );

                // Deleting is explicit: an empty list, never a missing one.
                shell
                    .worktree_settings
                    .editor
                    .open_mut()
                    .unwrap()
                    .fields
                    .as_mut()
                    .unwrap()
                    .databases
                    .clear();
                assert_eq!(shell.worktree_save_payload(cx).unwrap().1, Some(vec![]));

                // Worktree pages and engines without database support never
                // send databases, so they cannot replace anything.
                shell.worktree_settings.editor.open_mut().unwrap().checkout =
                    Some("/worktree".into());
                assert_eq!(shell.worktree_save_payload(cx).unwrap().1, None);
                shell.worktree_settings.editor.open_mut().unwrap().checkout = None;
                supports_databases(shell, false, cx);
                assert_eq!(shell.worktree_save_payload(cx).unwrap().1, None);
            })
            .unwrap();
    }

    #[gpui::test]
    fn polling_and_page_switches_keep_unsaved_database_drafts(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let window = shell(cx, dir.path());
        window
            .update(cx, |shell, _, cx| {
                supports_databases(shell, true, cx);
                let mut snapshot = WorktreeSettingsSnapshot {
                    defaults: WorktreeSettings::default(),
                    settings: WorktreeSettings::default(),
                    checkout: None,
                    has_overrides: false,
                    detected_env_files: vec![],
                    detected_dependency_paths: vec![],
                    detected_install_command: String::new(),
                    detected_workflow: None,
                    worktrees: vec![],
                    databases: vec![database("db", "Databases")],
                    app_services: vec![],
                };
                shell
                    .worktree_settings
                    .editor
                    .open(Editor::new(test_project()));
                assert!(shell.accept_worktree_settings_poll(Ok(snapshot.clone()), cx));
                let name = shell
                    .worktree_settings
                    .editor
                    .as_open()
                    .unwrap()
                    .fields
                    .as_ref()
                    .unwrap()
                    .databases[0]
                    .name
                    .clone();
                name.update(cx, |input, cx| input.set_text("Postgres", cx));
                // The engine reports a different saved list meanwhile.
                snapshot.databases = vec![database("other", "Redis")];
                shell.accept_worktree_settings_poll(Ok(snapshot), cx);
                shell.select_worktree_settings(Some("/worktree".into()), cx);
                shell.select_worktree_settings(None, cx);
                let fields = shell
                    .worktree_settings
                    .editor
                    .as_open()
                    .unwrap()
                    .fields
                    .as_ref()
                    .unwrap();
                assert_eq!(fields.databases.len(), 1);
                assert_eq!(fields.databases[0].name.entity_id(), name.entity_id());
                assert_eq!(fields.databases[0].name.read(cx).text(), "Postgres");
                assert!(shell.worktree_settings_dirty(cx));
            })
            .unwrap();
    }

    #[gpui::test]
    fn removal_requires_a_worktree_and_cancel_keeps_its_settings_open(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let window = shell(cx, dir.path());
        window
            .update(cx, |shell, window, cx| {
                let defaults = WorktreeSettings::default();
                shell.worktree_settings.editor.open(Editor {
                    project: ProjectContext {
                        space: "project".into(),
                        device: "local".into(),
                        name: "Project".into(),
                        target: None,
                        chat: None,
                        cwd: "/project".into(),
                        checkout_override: None,
                    },
                    checkout: None,
                    snapshot: Some(WorktreeSettingsSnapshot {
                        defaults: defaults.clone(),
                        settings: defaults.clone(),
                        checkout: None,
                        has_overrides: false,
                        detected_env_files: vec![],
                        detected_dependency_paths: vec![],
                        detected_install_command: String::new(),
                        detected_workflow: None,
                        worktrees: vec![zeron_proto::WorktreeEnvironmentEntry {
                            path: "/project-worktree".into(),
                            branch: "feature/work".into(),
                            has_overrides: false,
                            managed: true,
                            state: Default::default(),
                        }],
                        databases: vec![],
                        app_services: vec![],
                    }),
                    fields: Some(shell.worktree_fields(defaults, vec![], cx)),
                    drafts: Default::default(),
                    scroll: gpui::ScrollHandle::new(),
                    error: None,
                    poll_error: None,
                    busy: false,
                    output: None,
                });
                shell.begin_worktree_removal(window, cx);
                assert!(
                    !shell.worktree_settings.removal.is_open(),
                    "Project defaults cannot be removed"
                );
                shell.select_worktree_settings(Some("/project-worktree".into()), cx);
                shell.begin_worktree_removal(window, cx);
                assert!(shell.worktree_settings.removal.is_open());
                assert!(shell.capture_escape_surface(window, cx));
                assert!(!shell.worktree_settings.removal.is_open());
                assert_eq!(
                    shell
                        .worktree_settings
                        .editor
                        .as_open()
                        .unwrap()
                        .checkout
                        .as_deref(),
                    Some("/project-worktree")
                );
                shell.state.update(cx, |state, cx| {
                    state.chats = vec![
                        serde_json::from_value(serde_json::json!({
                            "id":"worker","deviceId":"local","cwd":"/project-worktree",
                            "spaceId":"project","archived":false,"createdAt":Utc::now(),
                        }))
                        .unwrap(),
                    ];
                    state.begin_pending_send("worker", "message", Utc::now());
                    cx.notify();
                });
                shell.begin_worktree_removal(window, cx);
                assert!(!shell.worktree_settings.removal.is_open());
                assert!(
                    shell
                        .worktree_settings
                        .editor
                        .as_open()
                        .unwrap()
                        .error
                        .as_ref()
                        .unwrap()
                        .contains("Stop agents")
                );
            })
            .unwrap();
    }
    #[cfg(unix)]
    #[gpui::test]
    fn removal_finds_unsaved_files_through_symlinks_and_relative_paths(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("project/src")).unwrap();
        std::fs::create_dir_all(real.join("project-worktree")).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let worktree = real.join("project-worktree").to_string_lossy().into_owned();
        let window = shell(cx, dir.path());
        window
            .update(cx, |shell, window, cx| {
                let defaults = WorktreeSettings::default();
                let mut editor = Editor::new(ProjectContext {
                    cwd: real.join("project").to_string_lossy().into_owned(),
                    ..test_project()
                });
                editor.checkout = Some(worktree.clone());
                editor.snapshot = Some(WorktreeSettingsSnapshot {
                    defaults: defaults.clone(),
                    settings: defaults,
                    checkout: Some(worktree.clone()),
                    has_overrides: false,
                    detected_env_files: vec![],
                    detected_dependency_paths: vec![],
                    detected_install_command: String::new(),
                    detected_workflow: None,
                    worktrees: vec![zeron_proto::WorktreeEnvironmentEntry {
                        path: worktree.clone(),
                        branch: "feature/work".into(),
                        has_overrides: false,
                        managed: true,
                        state: Default::default(),
                    }],
                    databases: vec![],
                    app_services: vec![],
                });
                shell.worktree_settings.editor.open(editor);
                shell.state.update(cx, |state, _| {
                    state.local_device_id = Some("local".into());
                    state.chats =
                        vec![serde_json::from_value(serde_json::json!({
                        "id": "main", "deviceId": "local", "spaceId": "project",
                        "cwd": link.join("project"), "archived": false, "createdAt": Utc::now(),
                    }))
                    .unwrap()];
                });
                shell.active_chat = "main".into();
                fn open_dirty(
                    shell: &mut Shell,
                    path: &str,
                    window: &mut Window,
                    cx: &mut Context<Shell>,
                ) {
                    let owner = ("main".to_string(), shell.state.clone());
                    shell.add_file_surface_at(owner, path.into(), None, window, cx);
                    let id = shell.file_surface_seq;
                    shell.file_surfaces[&id]
                        .update(cx, |files, _| files.seed_pending_exit_test_document(true));
                }
                open_dirty(shell, "src/lib.rs", window, cx);
                shell.begin_worktree_removal(window, cx);
                assert!(
                    shell.worktree_settings.removal.is_open(),
                    "unsaved files elsewhere in the project do not block removal"
                );
                shell.dismiss_worktree_removal(window, cx);
                open_dirty(shell, "../project-worktree/.env", window, cx);
                shell.begin_worktree_removal(window, cx);
                assert!(!shell.worktree_settings.removal.is_open());
                let editor = shell.worktree_settings.editor.as_open().unwrap();
                assert!(editor.error.as_deref().unwrap().contains("unsaved files"));
            })
            .unwrap();
    }

    #[gpui::test]
    fn switching_settings_pages_preserves_drafts_and_closing_invalidates_late_requests(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let window = shell(cx, dir.path());
        window
            .update(cx, |shell, _, cx| {
                let fields = shell.worktree_fields(WorktreeSettings::default(), vec![], cx);
                fields
                    .prefix
                    .update(cx, |input, cx| input.set_text("iiroan/", cx));
                shell.worktree_settings.editor.open(Editor {
                    project: ProjectContext {
                        space: "project".into(),
                        device: "local".into(),
                        name: "Project".into(),
                        target: None,
                        chat: None,
                        cwd: "/project".into(),
                        checkout_override: None,
                    },
                    checkout: None,
                    snapshot: None,
                    fields: Some(fields),
                    drafts: Default::default(),
                    scroll: gpui::ScrollHandle::new(),
                    error: None,
                    poll_error: None,
                    busy: false,
                    output: None,
                });
                shell.select_worktree_settings(Some("/worktree".into()), cx);
                shell.select_worktree_settings(None, cx);
                assert_eq!(
                    shell
                        .worktree_settings
                        .editor
                        .as_open()
                        .unwrap()
                        .fields
                        .as_ref()
                        .unwrap()
                        .prefix
                        .read(cx)
                        .text(),
                    "iiroan/"
                );
                shell.worktree_settings.services.open(0);
                assert!(shell.dismiss_worktree_select(cx));
                assert!(shell.worktree_settings.editor.is_open());
                assert!(!shell.worktree_settings.services.is_open());
                let old = shell.worktree_settings.epoch;
                shell.close_worktree_settings(cx);
                assert!(shell.worktree_settings.epoch > old);
                assert!(!shell.worktree_settings.editor.is_open());
            })
            .unwrap();
    }
    fn test_project() -> ProjectContext {
        ProjectContext {
            space: "project".into(),
            device: "local".into(),
            name: "Project".into(),
            target: None,
            chat: None,
            cwd: "/project".into(),
            checkout_override: None,
        }
    }

    #[gpui::test]
    fn closing_project_settings_asks_only_when_edits_are_unsaved(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let window = shell(cx, dir.path());
        window
            .update(cx, |shell, window, cx| {
                let settings = WorktreeSettings {
                    dependency_paths: vec!["node_modules".into(), "apps/web/node_modules".into()],
                    variables: [("APP_VALUE".to_string(), "a=b".to_string())].into(),
                    ..Default::default()
                };
                let mut editor = Editor::new(test_project());
                editor.fields = Some(shell.worktree_fields(settings.clone(), vec![], cx));
                shell.worktree_settings.editor.open(editor);
                assert!(
                    !shell.worktree_settings_dirty(cx),
                    "loaded values are not edits"
                );
                shell.request_close_form(UnsavedForm::WorktreeSettings, window, cx);
                assert!(!shell.worktree_settings.editor.is_open());
                assert!(!shell.worktree_settings.discard.is_open());

                let mut editor = Editor::new(test_project());
                editor.fields = Some(shell.worktree_fields(settings, vec![], cx));
                shell.worktree_settings.editor.open(editor);
                let prefix = shell
                    .worktree_settings
                    .editor
                    .as_open()
                    .unwrap()
                    .fields
                    .as_ref();
                let prefix = prefix.unwrap().prefix.clone();
                prefix.update(cx, |input, cx| input.set_text("iiroan/", cx));
                shell.select_worktree_settings(Some("/worktree".into()), cx);
                assert!(
                    shell.worktree_settings_dirty(cx),
                    "drafts on other pages count"
                );
                shell.request_close_form(UnsavedForm::WorktreeSettings, window, cx);
                assert!(shell.worktree_settings.discard.is_open());
                assert!(shell.worktree_settings.editor.is_open());
                assert!(shell.capture_escape_surface(window, cx));
                assert!(!shell.worktree_settings.discard.is_open());
                assert!(
                    shell.worktree_settings.editor.is_open(),
                    "Escape keeps editing"
                );
                shell.request_close_form(UnsavedForm::WorktreeSettings, window, cx);
                shell.discard_form(UnsavedForm::WorktreeSettings, window, cx);
                assert!(!shell.worktree_settings.discard.is_open());
                assert!(!shell.worktree_settings.editor.is_open());
            })
            .unwrap();
    }

    #[gpui::test]
    fn settings_polls_rerender_only_on_change_and_keep_save_errors(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let window = shell(cx, dir.path());
        window
            .update(cx, |shell, _, cx| {
                let defaults = WorktreeSettings::default();
                let snapshot = WorktreeSettingsSnapshot {
                    defaults: defaults.clone(),
                    settings: defaults,
                    checkout: None,
                    has_overrides: false,
                    detected_env_files: vec![],
                    detected_dependency_paths: vec![],
                    detected_install_command: String::new(),
                    detected_workflow: None,
                    worktrees: vec![],
                    databases: vec![],
                    app_services: vec![],
                };
                shell
                    .worktree_settings
                    .editor
                    .open(Editor::new(test_project()));
                assert!(shell.accept_worktree_settings_poll(Ok(snapshot.clone()), cx));
                assert!(
                    shell
                        .worktree_settings
                        .editor
                        .as_open()
                        .unwrap()
                        .fields
                        .is_some()
                );
                assert!(!shell.accept_worktree_settings_poll(Ok(snapshot.clone()), cx));

                shell.worktree_settings.editor.open_mut().unwrap().error =
                    Some("Save failed".into());
                assert!(shell.accept_worktree_settings_poll(Err("Engine offline".into()), cx));
                assert!(!shell.accept_worktree_settings_poll(Err("Engine offline".into()), cx));
                assert!(shell.accept_worktree_settings_poll(Ok(snapshot), cx));
                let editor = shell.worktree_settings.editor.as_open().unwrap();
                assert!(
                    editor.poll_error.is_none(),
                    "a successful poll clears its own error"
                );
                assert_eq!(editor.error.as_deref(), Some("Save failed"));
            })
            .unwrap();
    }

    #[gpui::test]
    fn muute_preset_fills_commands_without_executing_and_variables_keep_equals(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let window = shell(cx, dir.path());
        window
            .update(cx, |shell, _, cx| {
                let fields = shell.worktree_fields(WorktreeSettings::default(), vec![], cx);
                fields.variables.update(cx, |input, cx| {
                    input.set_text("APP_VALUE=a=b\nAPP_PORT=4100", cx)
                });
                for input in [&fields.create, &fields.remove, &fields.activate] {
                    input.update(cx, |input, cx| input.set_text("old command", cx));
                }
                shell.worktree_settings.editor.open(Editor {
                    project: ProjectContext {
                        space: "project".into(),
                        device: "local".into(),
                        name: "Project".into(),
                        target: None,
                        chat: None,
                        cwd: "/project".into(),
                        checkout_override: None,
                    },
                    checkout: None,
                    snapshot: None,
                    fields: Some(fields),
                    drafts: Default::default(),
                    scroll: gpui::ScrollHandle::new(),
                    error: None,
                    poll_error: None,
                    busy: false,
                    output: None,
                });
                shell.apply_muute_preset(cx);
                let editor = shell.worktree_settings.editor.as_open().unwrap();
                let settings = editor.fields.as_ref().unwrap().settings(cx).unwrap();
                assert!(settings.create_command.is_empty());
                assert!(settings.remove_command.is_empty());
                assert!(settings.activate_command.is_empty());
                assert_eq!(settings.dependencies, Deps::Skip);
                assert!(settings.commands_in_project);
                assert!(settings.env_files.is_empty());
                assert!(settings.setup_command.contains("codex-sync --mode setup"));
                assert!(settings.cleanup_command.contains("--mode cleanup"));
                assert_eq!(settings.variables["APP_VALUE"], "a=b");
                assert!(!editor.busy);
            })
            .unwrap();
    }
}
