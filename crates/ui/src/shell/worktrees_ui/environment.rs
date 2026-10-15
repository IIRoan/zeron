use super::*;

impl Shell {
    pub(super) fn render_environment_settings(
        &self,
        fields: &Fields,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut files = widgets::section_card(theme).mt_0();
        let modes = [Env::Copy, Env::Link, Env::Follow, Env::Skip];
        let closed = SelectState::default();

        for (index, rule) in fields.settings.env_files.iter().enumerate() {
            let path = rule.path.clone();
            let state_path = path.clone();
            let selected_path = path.clone();
            let remove_path = path.clone();
            let mode = widgets::select(
                format!("env-mode-{index}"),
                format!("Mode for {path}"),
                theme,
                move |shell: &mut Shell| {
                    shell
                        .worktree_settings
                        .env_modes
                        .entry(state_path.clone())
                        .or_default()
                },
            )
            .menu_layer(3)
            .options(
                [
                    SelectOption::new(env_label(Env::Copy)).detail("Independent copy"),
                    SelectOption::new(env_label(Env::Link)).detail("Shared file"),
                    SelectOption::new(env_label(Env::Follow))
                        .detail("Latest edits across worktrees"),
                    SelectOption::new(env_label(Env::Skip)).detail("Handled by workflow"),
                ],
                modes
                    .iter()
                    .position(|mode| *mode == rule.mode)
                    .unwrap_or(0),
            )
            .width(98.0)
            .menu_width(320.0)
            .on_select(move |shell, index, _, cx| {
                if let Some(fields) = shell
                    .worktree_settings
                    .editor
                    .open_mut()
                    .and_then(|editor| editor.fields.as_mut())
                    && let Some(rule) = fields
                        .settings
                        .env_files
                        .iter_mut()
                        .find(|rule| rule.path == selected_path)
                {
                    rule.mode = modes[index];
                }
                cx.notify();
            })
            .render(
                self.worktree_settings
                    .env_modes
                    .get(&path)
                    .unwrap_or(&closed),
                cx,
            );
            files = files.child(
                widgets::card_row(theme, index == 0)
                    .gap(px(10.0))
                    .child(
                        widgets::row_title(theme, SharedString::from(path.clone()))
                            .flex_1()
                            .min_w(px(96.0))
                            .truncate(),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(mode)
                            .child(
                                widgets::action(
                                    theme,
                                    widgets::ActionTone::Quiet,
                                    format!("remove-env-{index}"),
                                    window,
                                )
                                .w(px(28.0))
                                .h(px(28.0))
                                .min_h(px(28.0))
                                .p_0()
                                .justify_center()
                                .tab_index(0)
                                .role(gpui::Role::Button)
                                .aria_label(format!("Remove {path}"))
                                .child(
                                    icon(icons::CLOSE)
                                        .size(px(12.0))
                                        .text_color(theme.text_muted),
                                )
                                .on_click(cx.listener(
                                    move |shell, _, _, cx| {
                                        if let Some(fields) = shell
                                            .worktree_settings
                                            .editor
                                            .open_mut()
                                            .and_then(|editor| editor.fields.as_mut())
                                        {
                                            fields
                                                .settings
                                                .env_files
                                                .retain(|rule| rule.path != remove_path);
                                        }
                                        shell.worktree_settings.env_modes.remove(&remove_path);
                                        cx.notify();
                                    },
                                )),
                            ),
                    ),
            );
        }

        files = files.child(
            widgets::card_row(theme, fields.settings.env_files.is_empty())
                .gap(px(8.0))
                .child(field(theme, &fields.new_file).flex_1().min_w(px(100.0)))
                .child(
                    widgets::action(theme, widgets::ActionTone::Quiet, "add-env-file", window)
                        .tab_index(0)
                        .role(gpui::Role::Button)
                        .child("Add file")
                        .on_click(cx.listener(|shell, _, _, cx| {
                            if let Some(fields) = shell
                                .worktree_settings
                                .editor
                                .open_mut()
                                .and_then(|editor| editor.fields.as_mut())
                            {
                                let path = fields.new_file.read(cx).text().trim().to_string();
                                if !path.is_empty()
                                    && !fields
                                        .settings
                                        .env_files
                                        .iter()
                                        .any(|rule| rule.path == path)
                                {
                                    fields.settings.env_files.push(WorktreeEnvFile {
                                        path,
                                        mode: Env::Copy,
                                    });
                                    fields
                                        .new_file
                                        .update(cx, |input, cx| input.set_text("", cx));
                                }
                            }
                            cx.notify();
                        })),
                ),
        );

        let has_detected_files = self
            .worktree_settings
            .editor
            .as_open()
            .and_then(|editor| editor.snapshot.as_ref())
            .is_some_and(|snapshot| {
                snapshot.detected_env_files.iter().any(|path| {
                    !fields
                        .settings
                        .env_files
                        .iter()
                        .any(|rule| &rule.path == path)
                })
            });
        if has_detected_files {
            files = files.child(
                widgets::card_row(theme, false)
                    .min_h(px(48.0))
                    .py(px(8.0))
                    .child(
                        widgets::action(
                            theme,
                            widgets::ActionTone::Quiet,
                            "detect-env-files",
                            window,
                        )
                        .px_0()
                        .tab_index(0)
                        .role(gpui::Role::Button)
                        .child("Add detected files")
                        .on_click(cx.listener(|shell, _, _, cx| {
                            if let Some(editor) = shell.worktree_settings.editor.open_mut()
                                && let (Some(snapshot), Some(fields)) =
                                    (&editor.snapshot, &mut editor.fields)
                            {
                                for path in &snapshot.detected_env_files {
                                    if !fields
                                        .settings
                                        .env_files
                                        .iter()
                                        .any(|rule| &rule.path == path)
                                    {
                                        fields.settings.env_files.push(WorktreeEnvFile {
                                            path: path.clone(),
                                            mode: Env::Copy,
                                        });
                                    }
                                }
                            }
                            cx.notify();
                        })),
                    ),
            );
        }

        files = files.child(
            widgets::card_row(theme, false)
                .child(environment_row_label(
                    theme,
                    "Follow profile",
                    "Files set to Follow share edits with this profile.",
                ))
                .child(
                    field(theme, &fields.profile)
                        .id("project-settings-follow-profile")
                        .debug_selector(|| "project-settings-follow-profile".into())
                        .w(px(190.0))
                        .max_w_full(),
                ),
        );
        let dependency_modes = [Deps::Copy, Deps::Install, Deps::Link, Deps::Skip];
        let dependency_description = match fields.settings.dependencies {
            Deps::Copy => "Each worktree gets an independent copy.",
            Deps::Install => "Install from each worktree’s lockfile.",
            Deps::Link => "Package changes affect every worktree.",
            Deps::Skip => "Your setup command prepares dependencies.",
        };
        let mut dependencies = widgets::section_card(theme).mt_0().child(
            widgets::card_row(theme, true)
                .child(environment_row_label(
                    theme,
                    "Preparation",
                    dependency_description,
                ))
                .child(
                    widgets::select(
                        "worktree-dependency-mode",
                        "Dependencies",
                        theme,
                        |shell: &mut Shell| &mut shell.worktree_settings.dependencies,
                    )
                    .menu_layer(3)
                    .options(
                        [
                            SelectOption::new("Copy").detail("Independent folders"),
                            SelectOption::new("Install").detail("Use each worktree’s lockfile"),
                            SelectOption::new("Link").detail("Share the project’s folders"),
                            SelectOption::new("Skip").detail("Handled by workflow"),
                        ],
                        dependency_modes
                            .iter()
                            .position(|mode| *mode == fields.settings.dependencies)
                            .unwrap_or(0),
                    )
                    .width(146.0)
                    .menu_width(320.0)
                    .on_select(move |shell, index, _, cx| {
                        if let Some(fields) = shell
                            .worktree_settings
                            .editor
                            .open_mut()
                            .and_then(|editor| editor.fields.as_mut())
                        {
                            fields.settings.dependencies = dependency_modes[index];
                        }
                        cx.notify();
                    })
                    .render(&self.worktree_settings.dependencies, cx),
                ),
        );
        if fields.settings.dependencies == Deps::Install {
            dependencies = dependencies.child(
                widgets::card_row(theme, false)
                    .child(environment_row_label(
                        theme,
                        "Install command",
                        "Leave empty to detect it from the project.",
                    ))
                    .child(field(theme, &fields.install).w(px(240.0)).max_w_full()),
            );
        }
        if matches!(fields.settings.dependencies, Deps::Copy | Deps::Link) {
            dependencies = dependencies.child(
                widgets::card_row(theme, false)
                    .child(environment_row_label(
                        theme,
                        "Folders",
                        "Separate paths with commas.",
                    ))
                    .child(field(theme, &fields.paths).w(px(240.0)).max_w_full()),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap(px(24.0))
            .child(widgets::section(theme, "Environment files", files).mt_0())
            .child(widgets::section(theme, "Dependencies", dependencies).mt_0())
    }
}

fn environment_row_label(theme: &Theme, title: &'static str, detail: &str) -> gpui::Div {
    div()
        .flex_1()
        .min_w(px(130.0))
        .child(widgets::row_title(theme, title))
        .child(note(theme, detail).mt(px(3.0)))
}
