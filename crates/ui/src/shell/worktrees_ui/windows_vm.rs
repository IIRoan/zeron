//! The project's Windows VM (`zeron vm`, `docs/windows-vm.md`) on the
//! Environment page, for project defaults: whether the project is connected
//! to the VM on this machine, the ports that cross, and the VM itself
//! (start, stop, viewer). Connecting starts nothing; starting can take a
//! minute, and the engine finishes it even if this page closes.
use super::*;

#[derive(serde::Deserialize, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct View {
    #[serde(default)]
    pub(super) available: bool,
    #[serde(default)]
    pub(super) settings: Option<Settings>,
    /// Absent in a brief reply, which doesn't ask the VM.
    #[serde(default, deserialize_with = "or_default")]
    pub(super) vm: Vm,
    /// What a `ControlProjectWindowsVm` action answered: the diagnostics, a
    /// command's output, the screen.
    #[serde(default)]
    pub(super) result: serde_json::Value,
}

fn or_default<'de, D: serde::Deserializer<'de>>(reply: D) -> Result<Vm, D::Error> {
    Ok(<Option<Vm> as serde::Deserialize>::deserialize(reply)?.unwrap_or_default())
}

#[derive(serde::Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct Settings {
    #[serde(default)]
    to_vm: Vec<u16>,
    #[serde(default)]
    pub(super) from_vm: Vec<u16>,
    #[serde(default)]
    viewer: bool,
    #[serde(default)]
    pub(super) start_with: Vec<String>,
    #[serde(default)]
    pub(super) auto_ports: bool,
}

/// `winvm status`, or why there is none.
#[derive(serde::Deserialize, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct Vm {
    #[serde(default)]
    pub(super) state: String,
    #[serde(default)]
    pub(super) error: Option<String>,
    #[serde(default)]
    pub(super) tunnel_error: Option<String>,
    /// The ports connected now.
    #[serde(default)]
    pub(super) to_vm: Vec<u16>,
}

pub(super) struct Panel {
    space: String,
    view: Option<View>,
    to_vm: Entity<ComposerInput>,
    from_vm: Entity<ComposerInput>,
    start_with: Entity<ComposerInput>,
    viewer: bool,
    viewer_focus: gpui::FocusHandle,
    auto_ports: bool,
    auto_ports_focus: gpui::FocusHandle,
    busy: Option<&'static str>,
    error: Option<String>,
}

pub(super) fn joined(ports: &[u16]) -> String {
    zeron_proto::sandbox::port_ranges(ports)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn ports(text: &str, what: &str) -> Result<Vec<u16>, String> {
    zeron_proto::sandbox::parse_ports(text).map_err(|e| format!("{what}: {e}"))
}

/// What the group's first row says about the project and the VM.
pub(super) fn status(view: &View) -> String {
    if view.settings.is_none() {
        return "Off · this project's agents have no Windows VM.".into();
    }
    let vm = &view.vm;
    let mut text = match vm.state.as_str() {
        "running" if vm.to_vm.is_empty() => {
            "Connected · the VM is running. No port is connected yet.".to_string()
        }
        "running" => format!(
            "Connected · the VM is running. Ports connected: {}.",
            joined(&vm.to_vm)
        ),
        "starting" => "Connected · the VM is starting; Windows is not answering yet.".to_string(),
        "stopped" => "Connected · the VM is stopped. Start it here, or an agent starts it when it needs Windows.".to_string(),
        _ => "Connected · the VM can't be used on this machine yet.".to_string(),
    };
    if let Some(error) = vm.error.as_ref().or(vm.tunnel_error.as_ref()) {
        text.push(' ');
        text.push_str(error);
    }
    text
}

impl Shell {
    /// Loads the open project's Windows VM for the Environment page.
    pub(super) fn load_project_windows_vm(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.worktree_settings.editor.as_open() else {
            return;
        };
        let space = editor.project.space.clone();
        let params = editor.project.params(serde_json::json!({}));
        let input = |placeholder: &str, cx: &mut Context<Self>| {
            cx.new(|cx| ComposerInput::new(placeholder.to_string(), cx))
        };
        self.worktree_settings.windows_vm = Some(Panel {
            space,
            view: None,
            to_vm: input("3000, 3001", cx),
            from_vm: input("None", cx),
            start_with: input("None", cx),
            viewer: true,
            viewer_focus: cx.focus_handle(),
            auto_ports: true,
            auto_ports_focus: cx.focus_handle(),
            busy: None,
            error: None,
        });
        self.project_windows_vm_request(methods::GET_PROJECT_WINDOWS_VM, params, cx);
    }

    /// Connects the project with the page's inputs, or disconnects it.
    fn apply_project_windows_vm(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let Some(editor) = self.worktree_settings.editor.as_open() else {
            return;
        };
        let Some(panel) = self.worktree_settings.windows_vm.as_mut() else {
            return;
        };
        let mut params = editor
            .project
            .params(serde_json::json!({ "enabled": enabled }));
        if enabled {
            let read = (
                ports(&panel.to_vm.read(cx).text(), "Ports into the VM"),
                ports(&panel.from_vm.read(cx).text(), "Ports from the VM"),
            );
            match read {
                (Ok(to_vm), Ok(from_vm)) => {
                    params["toVm"] = to_vm.into();
                    params["fromVm"] = from_vm.into();
                    params["viewer"] = panel.viewer.into();
                    params["autoPorts"] = panel.auto_ports.into();
                    params["startWith"] = panel
                        .start_with
                        .read(cx)
                        .text()
                        .split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .collect::<Vec<_>>()
                        .into();
                }
                (Err(error), _) | (_, Err(error)) => {
                    panel.error = Some(error);
                    cx.notify();
                    return;
                }
            }
        }
        panel.busy = Some(if enabled {
            "Saving…"
        } else {
            "Disconnecting…"
        });
        panel.error = None;
        self.project_windows_vm_request(methods::SET_PROJECT_WINDOWS_VM, params, cx);
        cx.notify();
    }

    /// Starts or stops the VM, or opens its viewer window.
    fn control_project_windows_vm(&mut self, action: &'static str, cx: &mut Context<Self>) {
        let Some(editor) = self.worktree_settings.editor.as_open() else {
            return;
        };
        let Some(panel) = self.worktree_settings.windows_vm.as_mut() else {
            return;
        };
        let params = editor
            .project
            .params(serde_json::json!({ "action": action }));
        panel.busy = Some(match action {
            "start" => "Connecting this project's ports (starting the VM first if it is stopped)…",
            "stop" => "Shutting Windows down…",
            _ => "Opening the viewer…",
        });
        panel.error = None;
        self.project_windows_vm_request(methods::CONTROL_PROJECT_WINDOWS_VM, params, cx);
        cx.notify();
    }

    fn project_windows_vm_request(
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
            .windows_vm
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
                    .windows_vm
                    .as_mut()
                    .filter(|p| p.space == space)
                else {
                    return;
                };
                panel.busy = None;
                match result {
                    Ok(view) => {
                        // A first load fills the inputs; after that, only
                        // what the engine saved replaces what was typed.
                        if panel.view.is_none() || method == methods::SET_PROJECT_WINDOWS_VM {
                            if let Some(settings) = &view.settings {
                                panel.viewer = settings.viewer;
                                panel.auto_ports = settings.auto_ports;
                                for (input, ports) in [
                                    (&panel.to_vm, &settings.to_vm),
                                    (&panel.from_vm, &settings.from_vm),
                                ] {
                                    input.update(cx, |input, cx| input.set_text(joined(ports), cx));
                                }
                                panel.start_with.update(cx, |input, cx| {
                                    input.set_text(settings.start_with.join(", "), cx)
                                });
                            }
                        }
                        panel.view = Some(view);
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

    /// The Windows VM group, once loaded on a device that can run one.
    pub(super) fn render_project_windows_vm(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        if !self.windows_vm_plugin_enabled(cx) {
            return None;
        }
        let panel = self.worktree_settings.windows_vm.as_ref()?;
        let view = panel.view.as_ref()?;
        let connected = view.settings.is_some();
        if !view.available && !connected {
            return None;
        }
        let busy = panel.busy.is_some();
        let button = |element: gpui::Stateful<gpui::Div>,
                      action: fn(&mut Self, &mut Context<Self>),
                      cx: &mut Context<Self>| {
            element
                .when(busy, |b| b.opacity(0.5).cursor_default())
                .when(!busy, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| action(this, cx)))
                })
        };
        let mut actions = div()
            .flex()
            .flex_wrap()
            .gap(px(8.0))
            .mt(px(12.0))
            .child(button(
                popover::btn_primary(
                    theme,
                    if connected { "Apply" } else { "Connect" },
                    "project-windows-vm-apply",
                )
                .id("project-windows-vm-apply"),
                |this, cx| this.apply_project_windows_vm(true, cx),
                cx,
            ));
        if connected {
            actions = match view.vm.state.as_str() {
                "running" | "starting" => actions
                    .child(button(
                        popover::btn_ghost(theme, "Reconnect ports", "project-windows-vm-ports")
                            .id("project-windows-vm-ports"),
                        |this, cx| this.control_project_windows_vm("start", cx),
                        cx,
                    ))
                    .child(button(
                        popover::btn_ghost(theme, "Open viewer", "project-windows-vm-view")
                            .id("project-windows-vm-view"),
                        |this, cx| this.control_project_windows_vm("view", cx),
                        cx,
                    ))
                    .child(button(
                        popover::btn_ghost(theme, "Stop VM", "project-windows-vm-stop")
                            .id("project-windows-vm-stop"),
                        |this, cx| this.control_project_windows_vm("stop", cx),
                        cx,
                    )),
                _ => actions.child(button(
                    popover::btn_ghost(theme, "Start VM", "project-windows-vm-start")
                        .id("project-windows-vm-start"),
                    |this, cx| this.control_project_windows_vm("start", cx),
                    cx,
                )),
            };
            actions = actions.child(button(
                popover::btn_ghost(theme, "Disconnect", "project-windows-vm-off")
                    .id("project-windows-vm-off"),
                |this, cx| this.apply_project_windows_vm(false, cx),
                cx,
            ));
        }
        let switch = |id: &'static str,
                      focus: &gpui::FocusHandle,
                      on: bool,
                      label: &'static str,
                      flip: fn(&mut Panel),
                      cx: &mut Context<Self>| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .track_focus(focus)
                .role(gpui::Role::CheckBox)
                .aria_label(label)
                .flex()
                .items_center()
                .gap(px(10.0))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(panel) = this.worktree_settings.windows_vm.as_mut() {
                        flip(panel);
                    }
                    cx.notify();
                }))
                .child(widgets::toggle_switch(theme, on, id))
                .child(widgets::row_title(theme, label))
        };
        let auto_ports = switch(
            "project-windows-vm-auto-ports",
            &panel.auto_ports_focus,
            panel.auto_ports,
            "Connect the ports of this project's servers automatically",
            |panel| panel.auto_ports = !panel.auto_ports,
            cx,
        );
        let viewer = switch(
            "project-windows-vm-viewer",
            &panel.viewer_focus,
            panel.viewer,
            "Open the viewer window when the VM starts",
            |panel| panel.viewer = !panel.viewer,
            cx,
        );
        let mut group = div()
            .debug_selector(|| "project-windows-vm".into())
            .child(section(theme, "Windows VM"))
            .child(
                widgets::section_card(theme)
                    .mt_0()
                    .child(
                        widgets::card_row(theme, true)
                            .flex_col()
                            .items_stretch()
                            .child(widgets::row_title(theme, "Give this project a Windows machine"))
                            .child(note(theme, &status(view))),
                    )
                    .child(
                        widgets::card_row(theme, false)
                            .flex_col()
                            .items_stretch()
                            .gap(px(6.0))
                            .child(auto_ports)
                            .child(note(
                                theme,
                                "Every port a server of this project listens on, in any worktree and in its sandbox, is the same localhost port in Windows while it listens. Nothing to list per worktree.",
                            )),
                    )
                    .child(command_row(
                        theme,
                        false,
                        "More ports into the VM",
                        "Ports on this machine the VM always reaches on its own localhost, as ports or ranges, besides the automatic ones. With automatic ports off, these are the only ones. The VM reaches nothing else here.",
                        &panel.to_vm,
                    ))
                    .child(command_row(
                        theme,
                        false,
                        "Ports from the VM",
                        "VM ports this machine reaches on its localhost, such as a debugger. For a sandboxed project to reach them too, add them to the sandbox's host ports.",
                        &panel.from_vm,
                    ))
                    .child(command_row(
                        theme,
                        false,
                        "Start with services",
                        "Names of this project's services, separated by commas. Starting one of them starts the VM and connects its ports. Leave empty to start the VM only from here or by an agent.",
                        &panel.start_with,
                    ))
                    .child(widgets::card_row(theme, false).child(viewer)),
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
            "Agents in this project can start the VM, run commands in it and see its screen. Automatic ports follow the project's servers within a few seconds; other port changes apply when the VM starts for this project, or with Reconnect ports.",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_says_what_the_person_can_do_next() {
        let mut view = View::default();
        assert!(status(&view).starts_with("Off"));
        view.settings = Some(Settings {
            to_vm: vec![3000],
            from_vm: vec![],
            viewer: true,
            start_with: vec![],
            auto_ports: true,
        });
        view.vm.state = "stopped".into();
        assert!(status(&view).contains("Start it here"));
        view.vm.state = "running".into();
        view.vm.tunnel_error = Some("port 3000 is in use".into());
        assert!(status(&view).ends_with("port 3000 is in use"));
        view.vm = Vm {
            state: "unavailable".into(),
            error: Some("winvm is missing".into()),
            tunnel_error: None,
            to_vm: vec![],
        };
        assert!(
            status(&view).contains("can't be used") && status(&view).ends_with("winvm is missing")
        );
        assert_eq!(joined(&[3000, 3001, 3002, 9222]), "3000-3002, 9222");
    }
}
