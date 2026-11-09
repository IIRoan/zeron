//! The project's sandbox (`zeron sandbox`) on the Environment page, for
//! project defaults: its devcontainer config, ports and read-only files.
//! Applying builds the image and starts the container, which can take
//! minutes; the engine finishes it even if this page closes.
use super::*;

#[derive(serde::Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct View {
    #[serde(default)]
    available: bool,
    #[serde(default)]
    sandbox: Option<Status>,
    #[serde(default)]
    suggested_config: Option<String>,
}

#[derive(serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Status {
    config: String,
    #[serde(default)]
    host_ports: Vec<u16>,
    #[serde(default)]
    publish_ports: Vec<u16>,
    #[serde(default)]
    read_only: Vec<String>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    image_built: bool,
}

pub(super) struct Panel {
    space: String,
    view: Option<View>,
    config: Entity<ComposerInput>,
    host_ports: Entity<ComposerInput>,
    publish_ports: Entity<ComposerInput>,
    read_only: Entity<ComposerInput>,
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
        cx.notify();
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
                        }
                        panel.view = Some(view);
                    }
                    Err(error) => panel.error = Some(error.to_string()),
                }
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
        let busy = panel.busy.is_some();
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
                        "The project's own .devcontainer/devcontainer.json (its folder becomes read-only for agents), or one outside every project. Its image needs the agent CLIs and toolchain.",
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
                    )),
            )
            .child(actions);
        if let Some(busy) = panel.busy {
            group = group.child(note(theme, busy));
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

#[cfg(test)]
mod tests {
    use super::*;

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
