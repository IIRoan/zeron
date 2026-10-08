//! Device-private environment defaults and per-checkout overrides.
use super::project_terminals_ui::ProjectContext;
use super::*;
use crate::settings::widgets::{self, SelectOption, SelectState};
use zeron_proto::{
    WorktreeDependencyMode as Deps, WorktreeEnvFile, WorktreeEnvMode as Env,
    WorktreeServiceMode as Services, WorktreeSettings, WorktreeSettingsSnapshot,
    WorktreeSetupPhase as Phase,
};

mod removal;

#[derive(Default)]
pub(super) struct WorktreeSettingsUi {
    pub(super) editor: Option<Editor>,
    poll: Option<Task<()>>,
    epoch: u64,
    dependencies: SelectState,
    services: SelectState,
    directory: SelectState,
    lifecycle_visible: bool,
    removal: Option<removal::Removal>,
}

pub(super) struct Editor {
    project: ProjectContext,
    checkout: Option<String>,
    snapshot: Option<WorktreeSettingsSnapshot>,
    fields: Option<Fields>,
    drafts: std::collections::HashMap<Option<String>, Fields>,
    scroll: gpui::ScrollHandle,
    error: Option<String>,
    busy: bool,
    output: Option<String>,
}

struct Fields {
    settings: WorktreeSettings,
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
        self.worktree_settings.dependencies = SelectState::default();
        self.worktree_settings.services = SelectState::default();
        self.worktree_settings.directory = SelectState::default();
    }

    pub(super) fn dismiss_worktree_select(&mut self, cx: &mut Context<Self>) -> bool {
        if self.worktree_settings.dependencies.is_open() {
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
        self.worktree_settings.removal = None;
        self.worktree_settings.editor = Some(Editor {
            project,
            checkout: None,
            snapshot: None,
            fields: None,
            drafts: Default::default(),
            scroll: gpui::ScrollHandle::new(),
            error: None,
            busy: false,
            output: None,
        });
        self.worktree_settings.poll = None;
        self.poll_worktree_settings(cx);
        cx.notify();
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
        self.worktree_settings.editor = None;
        self.worktree_settings.poll = None;
        self.worktree_settings.removal = None;
        self.reset_worktree_selects();
        cx.notify();
    }

    fn worktree_fields(&self, settings: WorktreeSettings, cx: &mut Context<Self>) -> Fields {
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
        Fields {
            settings,
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
        }
    }

    fn worktree_params(&self) -> Option<serde_json::Value> {
        let editor = self.worktree_settings.editor.as_ref()?;
        Some(
            editor
                .project
                .params(serde_json::json!({"checkoutPath":editor.checkout})),
        )
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
                            || shell.worktree_settings.editor.is_none()
                        {
                            return false;
                        }
                        match result {
                            Ok(snapshot) => {
                                let needs_fields = shell
                                    .worktree_settings
                                    .editor
                                    .as_ref()
                                    .is_some_and(|e| e.fields.is_none());
                                let fields = needs_fields
                                    .then(|| shell.worktree_fields(snapshot.settings.clone(), cx));
                                let editor = shell.worktree_settings.editor.as_mut().unwrap();
                                if let Some(fields) = fields {
                                    shell.worktree_settings.lifecycle_visible =
                                        !fields.settings.create_command.is_empty()
                                            || !fields.settings.remove_command.is_empty();
                                    editor.fields = Some(fields);
                                }
                                editor.snapshot = Some(snapshot);
                            }
                            Err(error) => {
                                shell.worktree_settings.editor.as_mut().unwrap().error =
                                    Some(error.to_string())
                            }
                        }
                        cx.notify();
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

    fn select_worktree_settings(&mut self, checkout: Option<String>, cx: &mut Context<Self>) {
        let Some(editor) = self.worktree_settings.editor.as_mut() else {
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
        cx: &mut Context<Self>,
    ) {
        let Some(mut params) = self.worktree_params() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let resetting = method == methods::SAVE_WORKTREE_SETTINGS && settings.is_none();
        if method == methods::SAVE_WORKTREE_SETTINGS {
            params["settings"] = serde_json::to_value(settings).unwrap();
        }
        let epoch = self.worktree_settings.epoch;
        if let Some(editor) = self.worktree_settings.editor.as_mut() {
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
                let Some(editor) = shell.worktree_settings.editor.as_mut() else {
                    return;
                };
                editor.busy = false;
                match result {
                    Ok(snapshot) => {
                        let settings = snapshot.settings.clone();
                        editor.snapshot = Some(snapshot);
                        if resetting {
                            let fields = shell.worktree_fields(settings, cx);
                            shell.worktree_settings.editor.as_mut().unwrap().fields = Some(fields);
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

    pub(super) fn update_worktree_service_fields(
        &mut self,
        context: &ProjectContext,
        config: zeron_proto::ProjectTerminalConfig,
    ) {
        if let Some(editor) = self.worktree_settings.editor.as_mut()
            && editor.project.space == context.space
            && editor.project.device == context.device
            && editor.checkout == context.checkout_override
            && let Some(fields) = editor.fields.as_mut()
        {
            fields.settings.services = Some(config);
        }
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
                if let Some(editor) = shell.worktree_settings.editor.as_mut() {
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
        let Some(editor) = self.worktree_settings.editor.as_mut() else {
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
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let editor = self.worktree_settings.editor.as_ref()?;
        let theme = Theme::of(cx).for_popup();
        let busy = editor.busy;
        let title = editor
            .checkout
            .as_ref()
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| "Project defaults".into());
        let mut navigation = div()
            .id("worktree-settings-list")
            .w(px(190.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .p(px(12.0))
            .border_r_1()
            .border_color(theme.border)
            .overflow_y_scroll();
        navigation = navigation.child(
            choice(
                &theme,
                "worktree-defaults",
                "Project defaults",
                editor.checkout.is_none(),
            )
            .on_click(cx.listener(|this, _, _, cx| this.select_worktree_settings(None, cx))),
        );
        if let Some(snapshot) = &editor.snapshot {
            navigation = navigation.child(
                div()
                    .mt(px(12.0))
                    .mb(px(4.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_muted)
                    .child("WORKTREES"),
            );
            for tree in snapshot
                .worktrees
                .iter()
                .filter(|t| t.path != editor.project.cwd)
            {
                let path = tree.path.clone();
                let selected = editor.checkout.as_deref() == Some(&path);
                let label = if tree.branch.is_empty() {
                    std::path::Path::new(&path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                } else {
                    tree.branch.clone()
                };
                navigation = navigation.child(
                    choice(&theme, format!("worktree-choice-{path}"), "", selected)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(label)),
                        )
                        .when(tree.state.phase == Phase::Preparing, |row| row.child(
                            loaders::mini_mono_spinner(format!("worktree-preparing-{path}"), 1.5, theme.text_muted, cx.entity_id(), cx),
                        ))
                        .child(
                            div()
                                .ml_auto()
                                .text_size(px(10.0))
                                .text_color(if tree.state.phase == Phase::Failed {
                                    theme.danger
                                } else {
                                    theme.text_muted
                                })
                                .child(phase_label(tree.state.phase)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_worktree_settings(Some(path.clone()), cx)
                        })),
                );
            }
        }
        let mut content = div()
            .id("worktree-environment-scroll")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .p(px(20.0))
            .overflow_y_scroll()
            .track_scroll(&editor.scroll)
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation());
        if let Some(snapshot) = &editor.snapshot {
            content=content.child(popover::dialog_title(&theme,&title)).child(note(&theme,if editor.checkout.is_none(){"Defaults for new worktrees. Existing worktrees inherit them unless customized."}else if snapshot.has_overrides{"This worktree has its own settings. Save applies to this checkout only."}else{"Inheriting project defaults. Save creates an override for this worktree."}));
            if let Some(path) = &editor.checkout
                && let Some(tree) = snapshot.worktrees.iter().find(|t| &t.path == path)
            {
                content = content.child(section(&theme, "Environment status"));
                if tree.state.phase == Phase::Preparing {
                    content = content.child(loaders::worktree_setup_indicator(
                        "worktree-environment-progress", &theme,
                        if tree.state.step.is_empty() { "Preparing the environment" } else { &tree.state.step },
                        cx.entity_id(), cx,
                    ));
                } else {
                    content = content.child(note(&theme, if tree.state.step.is_empty() {
                        "Not prepared yet"
                    } else { &tree.state.step }));
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
                let mut actions = div().flex().gap(px(8.0)).mt(px(10.0));
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
                                    this.worktree_request(methods::SAVE_WORKTREE_SETTINGS, None, cx)
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
            content = content.child(section(&theme, "Branch names"));
            if editor.checkout.is_none() {
                content=content.child(label(&theme,"Prefix for new branches")).child(field(&theme,&fields.prefix)).child(note(&theme,"For example feature/ or iiroan/. Leave blank for no prefix. Existing branches keep their names."));
            } else {
                content=content.child(note(&theme,"The branch prefix is set in project defaults and applies when creating a worktree."));
            }
            content=content.child(section(&theme,"Environment files")).child(note(&theme,"Copy keeps edits separate. Link shares the project file. Follow carries the latest edits when you switch conversations. Skip leaves the file to your workflow."));
            for (index, rule) in fields.settings.env_files.iter().enumerate() {
                let path = rule.path.clone();
                let mode = rule.mode;
                content = content.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .mt(px(7.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.0))
                                .child(SharedString::from(path)),
                        )
                        .child(
                            choice(&theme, format!("env-mode-{index}"), env_label(mode), false)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(f) = this
                                        .worktree_settings
                                        .editor
                                        .as_mut()
                                        .and_then(|e| e.fields.as_mut())
                                        && let Some(rule) = f.settings.env_files.get_mut(index)
                                    {
                                        rule.mode = match rule.mode {
                                            Env::Copy => Env::Link,
                                            Env::Link => Env::Follow,
                                            Env::Follow => Env::Skip,
                                            Env::Skip => Env::Copy,
                                        };
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            choice(&theme, format!("remove-env-{index}"), "Remove", false)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(f) = this
                                        .worktree_settings
                                        .editor
                                        .as_mut()
                                        .and_then(|e| e.fields.as_mut())
                                        && index < f.settings.env_files.len()
                                    {
                                        f.settings.env_files.remove(index);
                                    }
                                    cx.notify();
                                })),
                        ),
                );
            }
            content = content.child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .mt(px(10.0))
                    .child(field(&theme, &fields.new_file).flex_1())
                    .child(
                        choice(&theme, "add-env-file", "Add", false).on_click(cx.listener(
                            |this, _, _, cx| {
                                if let Some(f) = this
                                    .worktree_settings
                                    .editor
                                    .as_mut()
                                    .and_then(|e| e.fields.as_mut())
                                {
                                    let path = f.new_file.read(cx).text().trim().to_string();
                                    if !path.is_empty()
                                        && !f.settings.env_files.iter().any(|r| r.path == path)
                                    {
                                        f.settings.env_files.push(WorktreeEnvFile {
                                            path,
                                            mode: Env::Copy,
                                        });
                                        f.new_file.update(cx, |input, cx| input.set_text("", cx));
                                    }
                                }
                                cx.notify();
                            },
                        )),
                    ),
            );
            content = content.child(
                choice(&theme, "detect-env-files", "Add detected env files", false)
                    .mt(px(8.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(editor) = this.worktree_settings.editor.as_mut()
                            && let (Some(snapshot), Some(fields)) =
                                (&editor.snapshot, &mut editor.fields)
                        {
                            for path in &snapshot.detected_env_files {
                                if !fields.settings.env_files.iter().any(|r| &r.path == path) {
                                    fields.settings.env_files.push(WorktreeEnvFile {
                                        path: path.clone(),
                                        mode: Env::Copy,
                                    });
                                }
                            }
                        }
                        cx.notify();
                    })),
            );
            if fields
                .settings
                .env_files
                .iter()
                .any(|r| r.mode == Env::Follow)
            {
                content=content.child(label(&theme,"Follow profile")).child(field(&theme,&fields.profile)).child(note(&theme,"Worktrees using the same profile share the latest env edits. Different names keep profiles separate. Replaced files are backed up locally."));
            }
            content = content.child(section(&theme, "Dependencies"));
            let dependency_modes = [Deps::Copy, Deps::Install, Deps::Link, Deps::Skip];
            content = content
                .child(
                    widgets::select(
                        "worktree-dependency-mode",
                        "Dependencies",
                        &theme,
                        |s: &mut Shell| &mut s.worktree_settings.dependencies,
                    )
                    .menu_layer(3)
                    .options(
                        [
                            SelectOption::new("Copy node_modules automatically"),
                            SelectOption::new("Install dependencies per worktree"),
                            SelectOption::new("Share node_modules with the project"),
                            SelectOption::new("Let my setup command handle dependencies"),
                        ],
                        dependency_modes
                            .iter()
                            .position(|m| *m == fields.settings.dependencies)
                            .unwrap_or(0),
                    )
                    .width(360.0)
                    .on_select(move |this, index, _, cx| {
                        if let Some(f) = this
                            .worktree_settings
                            .editor
                            .as_mut()
                            .and_then(|e| e.fields.as_mut())
                        {
                            f.settings.dependencies = dependency_modes[index];
                        }
                        cx.notify();
                    })
                    .render(&self.worktree_settings.dependencies, cx),
                )
                .child(note(&theme, dependency_help(fields.settings.dependencies)));
            if fields.settings.dependencies == Deps::Install {
                content = content
                    .child(label(&theme, "Install command (optional override)"))
                    .child(field(&theme, &fields.install));
            }
            if matches!(fields.settings.dependencies, Deps::Copy | Deps::Link) {
                content = content
                    .child(label(&theme, "Dependency folders, separated by commas"))
                    .child(field(&theme, &fields.paths));
            }
            content = content.child(section(&theme, "Services"));
            if editor.checkout.is_none() {
                let service_modes = [Services::FollowActive, Services::Parallel];
                content = content.child(
                    widgets::select(
                        "worktree-service-mode",
                        "Service groups",
                        &theme,
                        |s: &mut Shell| &mut s.worktree_settings.services,
                    )
                    .menu_layer(3)
                    .options(
                        [
                            SelectOption::new("One group follows the active conversation"),
                            SelectOption::new("Keep a separate group in each worktree"),
                        ],
                        usize::from(fields.settings.service_mode == Services::Parallel),
                    )
                    .width(360.0)
                    .on_select(move |this, index, _, cx| {
                        if let Some(f) = this
                            .worktree_settings
                            .editor
                            .as_mut()
                            .and_then(|e| e.fields.as_mut())
                        {
                            f.settings.service_mode = service_modes[index];
                        }
                        cx.notify();
                    })
                    .render(&self.worktree_settings.services, cx),
                );
            }
            content = content.child(note(&theme, service_help(fields.settings.service_mode)));
            if fields.settings.service_mode == Services::Parallel
                && fields
                    .settings
                    .env_files
                    .iter()
                    .any(|f| f.mode == Env::Follow)
            {
                content = content.child(note(&theme, "Choose Copy or Link for your env files before saving separate service groups. Follow uses one shared active environment."));
            }
            if editor.checkout.is_some() {
                content = content.child(note(
                    &theme,
                    "This project-wide choice is set in Project defaults.",
                ));
            }
            content = content.child(
                widgets::text_action(
                    &theme,
                    widgets::ActionTone::Outlined,
                    if editor.checkout.is_some() {
                        "Configure this worktree’s services"
                    } else {
                        "Configure default services"
                    },
                )
                .id("worktree-service-settings")
                .mt(px(8.0))
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(editor) = this.worktree_settings.editor.as_ref() {
                        let mut context = editor.project.clone();
                        context.checkout_override = editor.checkout.clone();
                        if let Some(cwd) = &editor.checkout {
                            context.cwd = cwd.clone();
                        }
                        this.open_service_settings_context(context, cx);
                    }
                })),
            );
            if editor.checkout.is_some() && fields.settings.services.is_some() {
                content = content.child(
                    choice(
                        &theme,
                        "inherit-worktree-services",
                        "Use default services",
                        false,
                    )
                    .mt(px(6.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(f) = this
                            .worktree_settings
                            .editor
                            .as_mut()
                            .and_then(|e| e.fields.as_mut())
                        {
                            f.settings.services = None;
                        }
                        cx.notify();
                    })),
                );
            }
            content = content.child(section(&theme, "Custom workflow"));
            if editor
                .snapshot
                .as_ref()
                .is_some_and(|s| s.detected_workflow.as_deref() == Some("muute"))
            {
                content=content.child(widgets::text_action(&theme, widgets::ActionTone::Outlined, "Use Muute worktree workflow").id("muute-worktree-preset").on_click(cx.listener(|this,_,_,cx|this.apply_muute_preset(cx)))).child(note(&theme,"Zeron creates the checkout; Muute’s codex-sync prepares env files, ports, submodules and database resources, and installs dependencies independently. Cleanup removes its database schema. Apply this preset, then Save settings."));
            }
            content = content
                .child(label(&theme, "Set up the worktree after creation"))
                .child(field(&theme, &fields.setup))
                .child(label(&theme, "Activate when switching to this worktree"))
                .child(field(&theme, &fields.activate))
                .child(label(
                    &theme,
                    "Clean up resources before removing the worktree",
                ))
                .child(field(&theme, &fields.cleanup));
            let in_project = fields.settings.commands_in_project;
            content = content.child(label(&theme, "Run setup, activation and cleanup commands in"))
                .child(widgets::select("worktree-command-directory", "Command folder", &theme,
                    |s: &mut Shell| &mut s.worktree_settings.directory)
                    .menu_layer(3)
                    .options([SelectOption::new("The worktree folder"), SelectOption::new("The main project folder")], usize::from(in_project))
                    .width(300.0)
                    .on_select(|this, index, _, cx| {
                        if let Some(f) = this.worktree_settings.editor.as_mut().and_then(|e| e.fields.as_mut()) {
                            f.settings.commands_in_project = index == 1;
                        }
                        cx.notify();
                    })
                    .render(&self.worktree_settings.directory, cx))
                .child(note(&theme, "Setup finishes before agents or services start. Cleanup releases resources before the worktree is removed."))
                .child(label(&theme, "Variables you can use in workflow commands"))
                .child(note(&theme, "\"$branchname\" = Git branch · \"$worktreename\" = folder name · \"$worktreepath\" = full worktree path · \"$projectroot\" = main project path. Use double quotes around variables."))
                .child(choice(&theme, "worktree-lifecycle-options", if self.worktree_settings.lifecycle_visible {"Hide custom creation and removal commands"} else {"Custom creation and removal commands…"}, false)
                    .mt(px(12.0)).on_click(cx.listener(|this, _, _, cx| {
                        this.worktree_settings.lifecycle_visible = !this.worktree_settings.lifecycle_visible;
                        cx.notify();
                    })));
            if self.worktree_settings.lifecycle_visible {
                if editor.checkout.is_none() {
                    content = content.child(label(&theme, "Create the worktree (optional Git override)"))
                        .child(field(&theme, &fields.create))
                        .child(note(&theme, "Leave blank for Zeron’s Git creation. Your command must create a linked checkout at \"$worktreepath\" on \"$branchname\", starting from \"$basebranch\"."));
                }
                content = content.child(label(&theme, "Remove the worktree (optional Git override)"))
                    .child(field(&theme, &fields.remove))
                    .child(note(&theme, "Leave blank for Zeron’s Git removal. Runs after cleanup and must remove \"$worktreepath\". Creation and removal commands always run in the main project folder."));
            }
            content = content
                .child(label(&theme, "Command timeout in seconds"))
                .child(field(&theme, &fields.timeout))
                .child(label(
                    &theme,
                    "Extra environment variables for commands, services, and worktree terminals",
                ))
                .child(field(&theme, &fields.variables))
                .child(note(
                    &theme,
                    "NAME=value, one per line. These settings and env backups stay on this device.",
                ));
        } else {
            content = content.child(note(&theme, "Loading worktree settings…"));
        }
        if let Some(output) = &editor.output {
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
        let mut footer = div()
            .flex_none()
            .p(px(16.0))
            .border_t_1()
            .border_color(theme.border);
        if let Some(error) = &editor.error {
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
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(11.0))
                        .text_color(theme.text_muted)
                        .child(if busy {
                            "Working… You can keep using the app."
                        } else {
                            "Save applies to this page. Setup runs before agents and services use the worktree."
                        }),
                )
                .child(
                    popover::btn_ghost(&theme, "Close", "close-worktree-settings")
                        .id("close-worktree-settings")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.close_worktree_settings(cx);
                        })),
                )
                .child(
                    popover::btn_primary(&theme, if busy { "Working…" } else { "Save settings" })
                        .id("save-worktree-settings")
                        .when(busy || editor.fields.is_none(), |b| {
                            b.opacity(0.5).cursor_default()
                        })
                        .when(!busy && editor.fields.is_some(), |b| {
                            b.on_click(cx.listener(|this, _, _, cx| {
                                let settings = this
                                    .worktree_settings
                                    .editor
                                    .as_ref()
                                    .unwrap()
                                    .fields
                                    .as_ref()
                                    .unwrap()
                                    .settings(cx);
                                match settings {
                                    Ok(settings) => this.worktree_request(
                                        methods::SAVE_WORKTREE_SETTINGS,
                                        Some(settings),
                                        cx,
                                    ),
                                    Err(error) => {
                                        this.worktree_settings.editor.as_mut().unwrap().error =
                                            Some(error);
                                        cx.notify();
                                    }
                                }
                            }))
                        }),
                ),
        );
        let header = div()
            .flex_none()
            .p(px(20.0))
            .border_b_1()
            .border_color(theme.border)
            .child(popover::dialog_title(&theme, "Project settings"))
            .child(note(&theme, &editor.project.name));
        let card = popover::dialog_card(&theme)
            .w(px(840.0))
            .max_w((viewport.width - px(40.0)).max(px(200.0)))
            .h(px(680.0).min((viewport.height - px(64.0)).max(px(240.0))))
            .p_0()
            .bg(theme.surface_dialog)
            .border_color(theme.border)
            .overflow_hidden()
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(navigation)
                    .child(motion::fade_quick(
                        SharedString::from(format!("worktree-settings-content-{title}")),
                        content,
                    )),
            )
            .child(footer);
        Some(popover::modal(
            "worktree-settings",
            viewport,
            card.into_any_element(),
        ))
    }
}

fn choice(
    theme: &Theme,
    id: impl Into<SharedString>,
    name: impl Into<SharedString>,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id.into())
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
        .bg(if selected {
            theme.wash(0.08)
        } else {
            gpui::transparent_black()
        })
        .hover(|s| s.bg(theme.element_hover))
        .cursor_pointer()
        .tab_index(0)
        .role(gpui::Role::Button)
        .child(name.into())
}
fn section(theme: &Theme, text: &'static str) -> impl IntoElement {
    div()
        .mt(px(24.0))
        .mb(px(10.0))
        .text_size(px(13.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text)
        .child(text)
}
fn note(theme: &Theme, text: &str) -> gpui::Div {
    div()
        .mt(px(6.0))
        .text_size(px(12.0))
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
        .bg(theme.wash(0.035))
        .border_color(theme.border)
        .rounded(px(6.0))
}
fn env_label(mode: Env) -> &'static str {
    match mode {
        Env::Copy => "Copy",
        Env::Link => "Link",
        Env::Follow => "Follow",
        Env::Skip => "Skip",
    }
}

fn dependency_help(mode: Deps) -> &'static str {
    match mode {
        Deps::Copy => {
            "Zeron automatically copies the selected node_modules folders from the main project into each new worktree. Each worktree keeps its own copy; existing folders are never overwritten."
        }
        Deps::Install => {
            "Zeron installs dependencies in each worktree using its lockfile. This creates independent node_modules; it does not copy the project’s existing folders."
        }
        Deps::Link => {
            "Zeron links the selected node_modules folders to the main project. Every worktree uses the same dependencies, so installing or changing packages affects all of them."
        }
        Deps::Skip => {
            "Zeron leaves node_modules alone. Your setup command must install, copy or link dependencies before agents and services use the worktree."
        }
    }
}

fn service_help(mode: Services) -> &'static str {
    match mode {
        Services::FollowActive => {
            "Run one Backend, Frontend and other configured services for this project. Switching to a conversation in another worktree stops the old group, prepares the new worktree, then restarts the services you had running there. Conversations in the same worktree share the group."
        }
        Services::Parallel => {
            "Each worktree keeps its own Backend, Frontend and other configured services. Switching conversations leaves the other groups running. Give each worktree different ports through env files or your setup command."
        }
    }
}
fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::NotPrepared => "Not set up",
        Phase::Preparing => "Setup…",
        Phase::Ready => "Ready",
        Phase::Failed => "Failed",
        Phase::Interrupted => "Interrupted",
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
            .as_ref()
            .map(|e| {
                serde_json::json!({
                    "checkout":e.checkout,"busy":e.busy,"error":e.error,
                    "worktrees":e.snapshot.as_ref().map(|s|&s.worktrees),
                    "settings":e.fields.as_ref().and_then(|f|f.settings(cx).ok()),
                    "hasOverrides":e.snapshot.as_ref().map(|s|s.has_overrides),
                    "workflow":e.snapshot.as_ref().and_then(|s|s.detected_workflow.clone()),
                    "dependencyMenuOpen":self.worktree_settings.dependencies.is_open(),
                    "serviceMenuOpen":self.worktree_settings.services.is_open(),
                    "directoryMenuOpen":self.worktree_settings.directory.is_open(),
                    "removalOpen": self.worktree_settings.removal.is_some()
                })
            })
            .unwrap_or(serde_json::Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};
    fn shell(cx: &mut TestAppContext, path: &PathBuf) -> gpui::WindowHandle<Shell> {
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
                    data_dir: path.clone(),
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

    #[gpui::test]
    fn saved_services_update_only_the_matching_project_settings_draft(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let window = shell(cx, &dir.path().to_path_buf());
        window.update(cx, |shell, _, cx| {
            let project = ProjectContext {
                space: "project".into(), device: "local".into(), name: "Project".into(),
                target: None, chat: None, cwd: "/project".into(), checkout_override: None,
            };
            let old = zeron_proto::ProjectTerminalConfig { services: vec![
                zeron_proto::ProjectTerminalService {
                    id: "backend".into(), name: "Backend".into(), command: "old command".into(),
                    directory: ".".into(), restart_on_failure: false,
                },
            ] };
            let mut next = old.clone();
            next.services[0].command = "new command".into();
            for checkout in [None, Some("/worktree".to_string())] {
                let settings = WorktreeSettings { services: Some(old.clone()), ..Default::default() };
                shell.worktree_settings.editor = Some(Editor {
                    project: project.clone(), checkout: checkout.clone(), snapshot: None,
                    fields: Some(shell.worktree_fields(settings, cx)), drafts: Default::default(),
                    scroll: gpui::ScrollHandle::new(), error: None, busy: false, output: None,
                });
                let mut context = project.clone();
                context.checkout_override = checkout;
                let mut other = context.clone();
                other.space = "another-project".into();
                shell.update_worktree_service_fields(&other, next.clone());
                let saved = |shell: &Shell| shell.worktree_settings.editor.as_ref().unwrap()
                    .fields.as_ref().unwrap().settings(cx).unwrap().services.unwrap();
                assert_eq!(saved(shell), old);
                shell.update_worktree_service_fields(&context, next.clone());
                assert_eq!(saved(shell), next, "A later settings Save must keep the newly saved commands");
            }
        }).unwrap();
    }

    #[gpui::test]
    fn removal_requires_a_worktree_and_cancel_keeps_its_settings_open(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let window = shell(cx, &dir.path().to_path_buf());
        window.update(cx, |shell, window, cx| {
            let defaults = WorktreeSettings::default();
            shell.worktree_settings.editor = Some(Editor {
                project: ProjectContext {
                    space:"project".into(), device:"local".into(), name:"Project".into(),
                    target:None, chat:None, cwd:"/project".into(), checkout_override:None,
                },
                checkout:None,
                snapshot:Some(WorktreeSettingsSnapshot {
                    defaults:defaults.clone(), settings:defaults.clone(), checkout:None,
                    has_overrides:false, detected_env_files:vec![], detected_dependency_paths:vec![],
                    detected_install_command:String::new(), detected_workflow:None,
                    worktrees:vec![zeron_proto::WorktreeEnvironmentEntry {
                        path:"/project-worktree".into(), branch:"feature/work".into(),
                        has_overrides:false, managed:true, state:Default::default(),
                    }],
                }),
                fields:Some(shell.worktree_fields(defaults, cx)), drafts:Default::default(),
                scroll:gpui::ScrollHandle::new(), error:None, busy:false, output:None,
            });
            shell.begin_worktree_removal(window, cx);
            assert!(shell.worktree_settings.removal.is_none(), "Project defaults cannot be removed");
            shell.select_worktree_settings(Some("/project-worktree".into()), cx);
            shell.begin_worktree_removal(window, cx);
            assert!(shell.worktree_settings.removal.is_some());
            assert!(shell.capture_escape_surface(window, cx));
            assert!(shell.worktree_settings.removal.is_none());
            assert_eq!(shell.worktree_settings.editor.as_ref().unwrap().checkout.as_deref(), Some("/project-worktree"));
            shell.state.update(cx, |state, cx| {
                state.chats = vec![serde_json::from_value(serde_json::json!({
                    "id":"worker","deviceId":"local","cwd":"/project-worktree",
                    "spaceId":"project","archived":false,"createdAt":Utc::now(),
                })).unwrap()];
                state.begin_pending_send("worker", "message", Utc::now());
                cx.notify();
            });
            shell.begin_worktree_removal(window, cx);
            assert!(shell.worktree_settings.removal.is_none());
            assert!(shell.worktree_settings.editor.as_ref().unwrap().error.as_ref().unwrap().contains("Stop agents"));
        }).unwrap();
    }
    #[gpui::test]
    fn switching_settings_pages_preserves_drafts_and_closing_invalidates_late_requests(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let window = shell(cx, &dir.path().to_path_buf());
        window
            .update(cx, |shell, _, cx| {
                let fields = shell.worktree_fields(WorktreeSettings::default(), cx);
                fields
                    .prefix
                    .update(cx, |input, cx| input.set_text("iiroan/", cx));
                shell.worktree_settings.editor = Some(Editor {
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
                    busy: false,
                    output: None,
                });
                shell.select_worktree_settings(Some("/worktree".into()), cx);
                shell.select_worktree_settings(None, cx);
                assert_eq!(
                    shell
                        .worktree_settings
                        .editor
                        .as_ref()
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
                assert!(shell.worktree_settings.editor.is_some());
                assert!(!shell.worktree_settings.services.is_open());
                let old = shell.worktree_settings.epoch;
                shell.close_worktree_settings(cx);
                assert!(shell.worktree_settings.epoch > old);
                assert!(shell.worktree_settings.editor.is_none());
            })
            .unwrap();
    }
    #[gpui::test]
    fn muute_preset_fills_commands_without_executing_and_variables_keep_equals(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let window = shell(cx, &dir.path().to_path_buf());
        window
            .update(cx, |shell, _, cx| {
                let fields = shell.worktree_fields(WorktreeSettings::default(), cx);
                fields.variables.update(cx, |input, cx| {
                    input.set_text("APP_VALUE=a=b\nAPP_PORT=4100", cx)
                });
                for input in [&fields.create, &fields.remove, &fields.activate] {
                    input.update(cx, |input, cx| input.set_text("old command", cx));
                }
                shell.worktree_settings.editor = Some(Editor {
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
                    busy: false,
                    output: None,
                });
                shell.apply_muute_preset(cx);
                let editor = shell.worktree_settings.editor.as_ref().unwrap();
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
