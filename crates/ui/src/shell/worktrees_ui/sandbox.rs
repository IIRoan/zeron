//! The project's sandbox (`zeron sandbox`) on the Environment page, for
//! project defaults: its devcontainer config, ports and read-only files.
//! Applying builds the image and starts the container, which can take
//! minutes; the engine finishes it even if this page closes.
use super::*;

#[derive(serde::Deserialize, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct View {
    #[serde(default)]
    available: bool,
    #[serde(default)]
    pub(super) sandbox: Option<Status>,
    #[serde(default)]
    suggested_config: Option<String>,
    #[serde(default)]
    pub(super) build: Build,
}

/// The project's image build: whether one runs now, and the end of its log
/// (the steps Zeron takes, `▸ …`, and the devcontainer CLI's output).
#[derive(serde::Deserialize, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct Build {
    #[serde(default)]
    pub(super) running: bool,
    #[serde(default)]
    log: String,
}

impl Build {
    /// The log's last line, which says what the build is doing.
    pub(super) fn latest(&self) -> Option<&str> {
        self.log
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
    }

    fn failed(&self) -> bool {
        self.latest()
            .is_some_and(|line| line.starts_with("▸ Failed"))
    }
}

/// The build log window ([`Shell::render_sandbox_log`]).
pub(in crate::shell) struct LogDialog {
    focus: gpui::FocusHandle,
    focus_pending: bool,
    scroll: gpui::ScrollHandle,
}

/// A sandbox build Zeron follows once a second until it ends, wherever the
/// person goes: the sidebar shows it ([`Shell::render_sandbox_build_strip`])
/// and the log window reads it. A failed one stays until its log is seen.
pub(in crate::shell) struct Watch {
    space: String,
    /// The project's name, for the sidebar.
    name: String,
    build: Build,
    /// `SetProjectSandbox` hasn't answered yet.
    requesting: bool,
    failed: bool,
    _poll: Task<()>,
}

/// Lines of the log the window shows, its end.
const LOG_LINES: usize = 600;

#[derive(serde::Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct Status {
    pub(super) config: String,
    #[serde(default)]
    pub(super) host_ports: Vec<u16>,
    #[serde(default)]
    pub(super) publish_ports: Vec<u16>,
    #[serde(default)]
    read_only: Vec<String>,
    #[serde(default)]
    share_github: bool,
    #[serde(default)]
    sign_commits: bool,
    #[serde(default)]
    pub(super) container: String,
    #[serde(default)]
    pub(super) state: String,
    #[serde(default)]
    pub(super) image_built: bool,
}

pub(super) struct Panel {
    space: String,
    view: Option<View>,
    config: Entity<ComposerInput>,
    host_ports: Entity<ComposerInput>,
    publish_ports: Entity<ComposerInput>,
    read_only: Entity<ComposerInput>,
    share_github: bool,
    sign_commits: bool,
    busy: Option<&'static str>,
    error: Option<String>,
}

impl Panel {
    pub(super) fn is_on(&self) -> bool {
        self.view.as_ref().is_some_and(|v| v.sandbox.is_some())
    }
}

fn joined<T: ToString>(items: &[T]) -> String {
    items
        .iter()
        .map(T::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn list(text: &str) -> Vec<&str> {
    text.split([',', '\n', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Ports and `a-b` ranges, as the engine stores them (one by one).
fn ports(text: &str, what: &str) -> Result<Vec<u16>, String> {
    zeron_proto::sandbox::parse_ports(text).map_err(|e| format!("{what}: {e}"))
}

/// A row with a switch for one of the panel's settings, applied with the rest.
fn switch_row(
    theme: &Theme,
    title: &'static str,
    detail: &'static str,
    on: bool,
    id: &'static str,
    flip: fn(&mut Panel),
    cx: &mut Context<Shell>,
) -> gpui::Div {
    widgets::card_row(theme, false)
        .gap(px(12.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(widgets::row_title(theme, title))
                .child(note(theme, detail).mt(px(2.0))),
        )
        .child(
            widgets::toggle_switch(theme, on, id)
                .id(id)
                .cursor_pointer()
                .tab_index(0)
                .role(gpui::Role::Switch)
                .aria_label(title)
                .aria_toggled(if on {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(panel) = this.worktree_settings.sandbox.as_mut() {
                        flip(panel);
                        cx.notify();
                    }
                })),
        )
}

/// The inputs as `SetProjectSandbox` takes them, into `params`.
fn settings(panel: &Panel, params: &mut serde_json::Value, cx: &App) -> Result<(), String> {
    let config = panel.config.read(cx).text().trim().to_string();
    if !config.starts_with('/') {
        return Err("Devcontainer config: give its full path".into());
    }
    params["config"] = config.into();
    params["hostPorts"] = ports(&panel.host_ports.read(cx).text(), "Host ports")?.into();
    params["publishPorts"] = ports(&panel.publish_ports.read(cx).text(), "Published ports")?.into();
    params["readOnly"] = list(&panel.read_only.read(cx).text()).into();
    params["shareGithub"] = panel.share_github.into();
    params["signCommits"] = panel.sign_commits.into();
    Ok(())
}

impl Shell {
    /// Loads the open project's sandbox for the Environment page.
    pub(super) fn load_project_sandbox(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.worktree_settings.editor.as_open() else {
            return;
        };
        let space = editor.project.space.clone();
        let params = editor.project.params(serde_json::json!({}));
        let input = |placeholder: &str, cx: &mut Context<Self>| {
            cx.new(|cx| ComposerInput::new(placeholder.to_string(), cx))
        };
        self.worktree_settings.sandbox = Some(Panel {
            space,
            view: None,
            config: input("/path/to/.devcontainer/devcontainer.json", cx),
            host_ports: input("5432, 6379", cx),
            publish_ports: input("Automatic", cx),
            read_only: input("compose.yaml, bunfig.toml", cx),
            share_github: false,
            sign_commits: false,
            busy: None,
            error: None,
        });
        self.project_sandbox_request(methods::GET_PROJECT_SANDBOX, params, cx);
    }

    /// Turns the sandbox on with the page's inputs (rebuilding the image), or
    /// off.
    fn apply_project_sandbox(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let Some(editor) = self.worktree_settings.editor.as_open() else {
            return;
        };
        let Some(panel) = self.worktree_settings.sandbox.as_mut() else {
            return;
        };
        let mut params = editor
            .project
            .params(serde_json::json!({ "enabled": enabled }));
        if enabled && let Err(error) = settings(panel, &mut params, cx) {
            panel.error = Some(error);
            cx.notify();
            return;
        }
        panel.busy = Some(if enabled {
            "Building the image and starting the container…"
        } else {
            "Turning the sandbox off…"
        });
        panel.error = None;
        self.project_sandbox_request(methods::SET_PROJECT_SANDBOX, params, cx);
        if enabled {
            self.watch_project_sandbox_build(true, cx);
        }
        cx.notify();
    }

    /// Builds the project's image again with the settings it has and starts
    /// its container anew, for the Environment panel: followed like a build
    /// the settings page asked for (the sidebar strip, the log window).
    /// `done` hears how it ended.
    pub(super) fn rebuild_sandbox(
        &mut self,
        project: ProjectContext,
        sandbox: &Status,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<(), String>, &mut Context<Self>) + 'static,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = project.params(serde_json::json!({
            "enabled": true,
            "config": sandbox.config,
            "hostPorts": sandbox.host_ports,
            "publishPorts": sandbox.publish_ports,
            "readOnly": sandbox.read_only,
            "shareGithub": sandbox.share_github,
            "signCommits": sandbox.sign_commits,
        }));
        let space = project.space.clone();
        self.watch_sandbox_build(
            project.space.clone(),
            project.name.clone(),
            project.params(serde_json::json!({})),
            true,
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<View>(methods::SET_PROJECT_SANDBOX, params)
                .await;
            this.update(cx, |shell, cx| {
                if let Some(watch) = shell
                    .worktree_settings
                    .sandbox_build
                    .as_mut()
                    .filter(|w| w.space == space)
                {
                    watch.requesting = false;
                    if let Ok(view) = &result {
                        watch.build = view.build.clone();
                    }
                }
                done(shell, result.map(|_| ()).map_err(|e| e.to_string()), cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Follows the open project's sandbox build ([`Watch`]): one this page
    /// just asked for (`requesting`), or one already running when it opened.
    fn watch_project_sandbox_build(&mut self, requesting: bool, cx: &mut Context<Self>) {
        let Some(editor) = self.worktree_settings.editor.as_open() else {
            return;
        };
        let (space, name) = (editor.project.space.clone(), editor.project.name.clone());
        let params = editor.project.params(serde_json::json!({}));
        self.watch_sandbox_build(space, name, params, requesting, cx);
    }

    /// Follows the sandbox build of the project `space` (named `name`),
    /// asking with `params`.
    fn watch_sandbox_build(
        &mut self,
        space: String,
        name: String,
        params: serde_json::Value,
        requesting: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(watch) = self
            .worktree_settings
            .sandbox_build
            .as_mut()
            .filter(|w| w.space == space && !w.failed)
        {
            watch.requesting |= requesting;
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let polled = space.clone();
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let fresh = engine
                    .client()
                    .call_as::<View>(methods::GET_PROJECT_SANDBOX, params.clone())
                    .await
                    .ok();
                let more = this.update(cx, |shell, cx| {
                    shell.sandbox_build_polled(&polled, fresh, &params, cx)
                });
                if !more.unwrap_or(false) {
                    break;
                }
            }
        });
        self.worktree_settings.sandbox_build = Some(Watch {
            space,
            name,
            build: Build::default(),
            requesting,
            failed: false,
            _poll: poll,
        });
    }

    /// A poll of the watched build. Whether to keep polling.
    fn sandbox_build_polled(
        &mut self,
        space: &str,
        fresh: Option<View>,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> bool {
        let settings = &mut self.worktree_settings;
        let Some(watch) = settings.sandbox_build.as_mut().filter(|w| w.space == space) else {
            return false;
        };
        if let Some(fresh) = &fresh
            && watch.build != fresh.build
        {
            // The log window follows the end, unless scrolled up.
            if let Some(dialog) = settings.sandbox_log.get() {
                let scroll = &dialog.scroll;
                if scroll.max_offset().y - (-scroll.offset().y) < px(4.0) {
                    scroll.scroll_to_bottom();
                }
            }
            watch.build = fresh.build.clone();
            if let Some(view) = settings
                .sandbox
                .as_mut()
                .filter(|p| p.space == space)
                .and_then(|p| p.view.as_mut())
            {
                view.build = fresh.build.clone();
            }
        }
        let running = fresh.as_ref().is_some_and(|v| v.build.running);
        let more = watch.requesting || running;
        if !more {
            if watch.build.failed() {
                watch.failed = true;
            } else {
                settings.sandbox_build = None;
            }
            // How it ended, on the page too (a build it didn't start).
            if settings.sandbox.as_ref().is_some_and(|p| p.space == space) {
                self.project_sandbox_request(methods::GET_PROJECT_SANDBOX, params.clone(), cx);
            }
        }
        cx.notify();
        more
    }

    fn project_sandbox_request(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(space) = self
            .worktree_settings
            .sandbox
            .as_ref()
            .map(|p| p.space.clone())
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = engine.client().call_as::<View>(method, params).await;
            this.update(cx, |shell, cx| {
                // The build answered: its watch stops once the log says so.
                if method == methods::SET_PROJECT_SANDBOX
                    && let Some(watch) = shell
                        .worktree_settings
                        .sandbox_build
                        .as_mut()
                        .filter(|w| w.space == space)
                {
                    watch.requesting = false;
                    if let Ok(view) = &result {
                        watch.build = view.build.clone();
                    }
                }
                let Some(panel) = shell
                    .worktree_settings
                    .sandbox
                    .as_mut()
                    .filter(|p| p.space == space)
                else {
                    return;
                };
                panel.busy = None;
                match result {
                    Ok(view) => {
                        let (config, host, publish, read_only) = match &view.sandbox {
                            Some(s) => (
                                s.config.clone(),
                                joined(&zeron_proto::sandbox::port_ranges(&s.host_ports)),
                                joined(&zeron_proto::sandbox::port_ranges(&s.publish_ports)),
                                joined(&s.read_only),
                            ),
                            None => (
                                view.suggested_config.clone().unwrap_or_default(),
                                String::new(),
                                String::new(),
                                String::new(),
                            ),
                        };
                        // A first load fills the inputs; after that, only
                        // what the engine applied replaces what was typed.
                        if panel.view.is_none() || method == methods::SET_PROJECT_SANDBOX {
                            for (input, text) in [
                                (&panel.config, config),
                                (&panel.host_ports, host),
                                (&panel.publish_ports, publish),
                                (&panel.read_only, read_only),
                            ] {
                                input.update(cx, |input, cx| input.set_text(text, cx));
                            }
                            panel.share_github =
                                view.sandbox.as_ref().is_some_and(|s| s.share_github);
                            panel.sign_commits =
                                view.sandbox.as_ref().is_some_and(|s| s.sign_commits);
                        }
                        let running = view.build.running;
                        panel.view = Some(view);
                        if running {
                            shell.watch_project_sandbox_build(false, cx);
                        }
                    }
                    Err(error) => panel.error = Some(error.to_string()),
                }
                shell.reveal_environment_groups();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The Sandbox group, once loaded on a device that can run one.
    pub(super) fn render_project_sandbox(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let panel = self.worktree_settings.sandbox.as_ref()?;
        let view = panel.view.as_ref()?;
        if !view.available && view.sandbox.is_none() {
            return None;
        }
        // A build this page started, or one already running when it opened.
        let watch = self
            .worktree_settings
            .sandbox_build
            .as_ref()
            .filter(|w| w.space == panel.space);
        let build = watch.map_or(&view.build, |w| &w.build);
        let busy = panel.busy.is_some() || build.running || watch.is_some_and(|w| w.requesting);
        let status = match &view.sandbox {
            Some(s) => format!(
                "On · container {}{}. Agents, services and terminals of this project and its worktrees run in it, with this machine's Claude Code and Codex logins.",
                if s.state.is_empty() {
                    "unknown"
                } else {
                    &s.state
                },
                if s.image_built {
                    ""
                } else {
                    " · image not built"
                },
            ),
            None => "Off · agents run on this machine.".to_string(),
        };
        let mut actions = div().flex().flex_wrap().gap(px(8.0)).mt(px(12.0)).child(
            popover::btn_primary(
                theme,
                if view.sandbox.is_some() {
                    "Apply and rebuild"
                } else {
                    "Turn on"
                },
                "project-sandbox-apply",
            )
            .id("project-sandbox-apply")
            .when(busy, |b| b.opacity(0.5).cursor_default())
            .when(!busy, |b| {
                b.on_click(cx.listener(|this, _, _, cx| this.apply_project_sandbox(true, cx)))
            }),
        );
        if !build.log.trim().is_empty() {
            actions = actions.child(
                popover::btn_ghost(theme, "Show log", "project-sandbox-log")
                    .id("project-sandbox-log")
                    .on_click(cx.listener(|this, _, _, cx| this.open_sandbox_log(cx))),
            );
        }
        if view.sandbox.is_some() {
            actions = actions.child(
                popover::btn_ghost(theme, "Turn off", "project-sandbox-off")
                    .id("project-sandbox-off")
                    .when(busy, |b| b.opacity(0.5).cursor_default())
                    .when(!busy, |b| {
                        b.on_click(
                            cx.listener(|this, _, _, cx| this.apply_project_sandbox(false, cx)),
                        )
                    }),
            );
        }
        let mut group = div()
            .debug_selector(|| "project-sandbox".into())
            .child(section(theme, "Sandbox"))
            .child(
                widgets::section_card(theme)
                    .mt_0()
                    .child(
                        widgets::card_row(theme, true)
                            .flex_col()
                            .items_stretch()
                            .child(widgets::row_title(theme, "Run this project in a container"))
                            .child(note(theme, &status)),
                    )
                    .child(command_row(
                        theme,
                        false,
                        "Devcontainer config",
                        "The project's own .devcontainer/devcontainer.json (its folder becomes read-only for agents), or one outside every project. Its image needs the project's toolchain; without Claude Code or Codex of its own, agents there use this machine's.",
                        &panel.config,
                    ))
                    .child(command_row(
                        theme,
                        false,
                        "Host ports",
                        "Ports on this machine the container reaches on its localhost, such as databases.",
                        &panel.host_ports,
                    ))
                    .child(command_row(
                        theme,
                        false,
                        "Published ports",
                        "Leave empty to publish every port a server in the container opens, on this machine's localhost only. List ports or ranges (3300-3399) to publish just those.",
                        &panel.publish_ports,
                    ))
                    .child(command_row(
                        theme,
                        false,
                        "Read-only files",
                        "Paths in the project this machine reads, such as a database's compose file, and any compose file the config names outside its folder.",
                        &panel.read_only,
                    ))
                    .child(switch_row(
                        theme,
                        "Share GitHub login",
                        "Agents there get this machine's gh login, so gh and git over HTTPS reach GitHub. They can then do anything that account can, including push. Takes effect when you apply.",
                        panel.share_github,
                        "project-sandbox-github",
                        |panel| panel.share_github = !panel.share_github,
                        cx,
                    ))
                    .child(switch_row(
                        theme,
                        "Sign commits",
                        "Agents' commits and tags there are signed with this machine's signing key (git's user.signingkey), which never goes in: only git commits and tags are signed. Takes effect when you apply.",
                        panel.sign_commits,
                        "project-sandbox-sign",
                        |panel| panel.sign_commits = !panel.sign_commits,
                        cx,
                    )),
            )
            .child(actions);
        if busy {
            group = group.child(note(
                theme,
                panel
                    .busy
                    .unwrap_or("Building the image and starting the container…"),
            ));
            // What the build is doing now: its log's last line.
            if let Some(latest) = build.latest() {
                group = group.child(
                    div()
                        .mt(px(4.0))
                        .font_family(theme.font_mono.clone())
                        .text_size(px(11.0))
                        .text_color(theme.text_muted)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(SharedString::from(latest.to_owned())),
                );
            }
        }
        if let Some(error) = &panel.error {
            group = group.child(
                div()
                    .mt(px(6.0))
                    .text_color(theme.danger)
                    .text_size(px(12.0))
                    .child(SharedString::from(error.clone())),
            );
        }
        Some(group.child(note(
            theme,
            "Applying builds the image, which can take minutes. It keeps going if you close this page.",
        )))
    }
}

impl Shell {
    /// The build the log window and the sidebar show: the watched one, else
    /// the open page's last.
    fn shown_sandbox_build(&self) -> Option<&Build> {
        let settings = &self.worktree_settings;
        settings
            .sandbox_build
            .as_ref()
            .map(|w| &w.build)
            .or_else(|| {
                settings
                    .sandbox
                    .as_ref()
                    .and_then(|p| p.view.as_ref())
                    .map(|v| &v.build)
            })
            // The Environment panel's reading, with no settings page open.
            .or_else(|| settings.environment.sandbox().map(|v| &v.build))
    }

    pub(super) fn open_sandbox_log(&mut self, cx: &mut Context<Self>) {
        let scroll = gpui::ScrollHandle::new();
        scroll.scroll_to_bottom();
        self.worktree_settings.sandbox_log.open(LogDialog {
            focus: cx.focus_handle(),
            focus_pending: true,
            scroll,
        });
        cx.notify();
    }

    /// A failed build's sidebar strip goes once its log was seen.
    fn close_sandbox_log(&mut self, cx: &mut Context<Self>) {
        if self
            .worktree_settings
            .sandbox_build
            .as_ref()
            .is_some_and(|w| w.failed)
        {
            self.worktree_settings.sandbox_build = None;
        }
        popover::close_popup(self, cx, |this| &mut this.worktree_settings.sandbox_log);
    }

    /// In the sidebar while a sandbox build runs, or after one failed: the
    /// project, what the build is doing, and its log on a click.
    pub(in crate::shell) fn render_sandbox_build_strip(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let watch = self.worktree_settings.sandbox_build.as_ref()?;
        let (tone, bg, bg_hover) = if watch.failed {
            (
                theme.danger,
                theme.danger.opacity(0.14),
                theme.danger.opacity(0.22),
            )
        } else {
            (theme.accent, theme.accent_wash, theme.accent.opacity(0.16))
        };
        let title = if watch.failed {
            format!("Sandbox build failed · {}", watch.name)
        } else {
            format!("Rebuilding sandbox · {}", watch.name)
        };
        let latest = (!watch.failed)
            .then(|| watch.build.latest())
            .flatten()
            .map(|line| line.trim_start_matches("▸ ").to_owned());
        let spinner = (!watch.failed).then(|| {
            crate::loaders::mini_glyph_spinner(
                "sandbox-build-spinner",
                2.0,
                theme.glyph,
                self.sidebar_pane.entity_id(),
                cx,
            )
            .into_any_element()
        });
        Some(
            div()
                .id("sandbox-build-strip")
                .debug_selector(|| "sandbox-build-strip".into())
                .role(gpui::Role::Button)
                .aria_label(format!("{title}. Show its log"))
                .tab_index(0)
                .mx(px(Theme::SPACE_SM))
                .mb(px(4.0))
                .px(px(Theme::SPACE_SM))
                .py(px(6.0))
                .rounded(px(Theme::CONTROL_RADIUS))
                .cursor_pointer()
                .bg(motion::hover_blend("sandbox-build-strip", bg, bg_hover))
                .on_hover(motion::hover_listener("sandbox-build-strip"))
                .on_click(cx.listener(|this, _, _, cx| this.open_sandbox_log(cx)))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(tone)
                        .children(spinner)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(title),
                        ),
                )
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(SharedString::from(latest.unwrap_or_else(|| {
                            if watch.failed {
                                "Show the log".into()
                            } else {
                                "Starting…".into()
                            }
                        }))),
                )
                .into_any_element(),
        )
    }

    /// The build log window: the log's end, followed while the build runs.
    pub(in crate::shell) fn render_sandbox_log(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if let Some(dialog) = self.worktree_settings.sandbox_log.open_mut()
            && std::mem::take(&mut dialog.focus_pending)
        {
            window.focus(&dialog.focus, cx);
        }
        let dialog = self.worktree_settings.sandbox_log.get()?;
        let build = self.shown_sandbox_build()?;
        let theme = Theme::of(cx).for_popup();
        let state = if build.running {
            "Building…"
        } else if build.failed() {
            "Failed"
        } else {
            "Finished"
        };
        let lines: Vec<&str> = build.log.lines().collect();
        let shown = lines[lines.len().saturating_sub(LOG_LINES)..].join("\n");
        let card = popover::dialog_card(&theme)
            .w(px(760.0))
            .max_w((viewport.width - px(40.0)).max(px(200.0)))
            .track_focus(&dialog.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if matches!(
                    popover::classify_key(
                        event.keystroke.key.as_str(),
                        event.keystroke.modifiers.platform,
                        event.keystroke.modifiers.control,
                    ),
                    popover::MenuKey::Escape
                ) {
                    cx.stop_propagation();
                    this.close_sandbox_log(cx);
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_sandbox_log(cx)))
            .child(popover::dialog_title(&theme, "Sandbox build log"))
            .child(
                div()
                    .mt(px(4.0))
                    .text_size(px(12.0))
                    .text_color(if build.failed() {
                        theme.danger
                    } else {
                        theme.text_muted
                    })
                    .child(state),
            )
            .child(
                div()
                    .mt(px(12.0))
                    .rounded(px(8.0))
                    .bg(theme.bg.opacity(0.45))
                    .border_1()
                    .border_color(theme.border.opacity(0.6))
                    .px(px(10.0))
                    .py(px(8.0))
                    .child(
                        div()
                            .id("sandbox-build-log")
                            .max_h((viewport.height * 0.6).max(px(160.0)))
                            .overflow_y_scroll()
                            .track_scroll(&dialog.scroll)
                            .font_family(theme.font_mono.clone())
                            .text_size(px(11.0))
                            .line_height(px(16.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(if shown.is_empty() {
                                "No output yet".to_owned()
                            } else {
                                shown
                            })),
                    ),
            )
            .child(
                div().mt(px(16.0)).flex().justify_end().child(
                    popover::btn_ghost(&theme, "Close", "sandbox-log-close")
                        .id("sandbox-log-close")
                        .role(gpui::Role::Button)
                        .on_click(cx.listener(|this, _, _, cx| this.close_sandbox_log(cx))),
                ),
            );
        Some(popover::modal(
            "sandbox-build-log-dialog",
            viewport,
            card.into_any_element(),
            self.worktree_settings.sandbox_log.closing_since(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_builds_latest_step_is_its_last_line() {
        let build = Build {
            running: false,
            log: "▸ Building the image\nSTEP 2/9: RUN true\n\n".into(),
        };
        assert_eq!(build.latest(), Some("STEP 2/9: RUN true"));
        assert!(!build.failed());
        let failed = Build {
            log: format!("{}▸ Failed: devcontainer build failed\n", build.log),
            ..build
        };
        assert!(failed.failed());
    }

    #[test]
    fn ports_and_paths_read_from_any_separator() {
        assert_eq!(
            ports("5432, 6379\n1025 8025 3300-3301", "x"),
            Ok(vec![5432, 6379, 1025, 8025, 3300, 3301])
        );
        assert!(
            ports("5432, http", "Host ports")
                .unwrap_err()
                .contains("http")
        );
        assert!(ports("0", "x").is_err());
        assert_eq!(
            list(" compose.yaml,\nbunfig.toml ,"),
            vec!["compose.yaml", "bunfig.toml"]
        );
    }
}
