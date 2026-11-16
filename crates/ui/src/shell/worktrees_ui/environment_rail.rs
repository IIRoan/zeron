//! The workbench's Environment panel and its rail entry: the active
//! project's Windows VM and sandbox, as cards. The VM's card shows its
//! screen (a picture taken again every few seconds while the panel shows),
//! its state, what its processor, memory and disk hold, and the ports that
//! cross; under it a command line into Windows and the rest of a health
//! reading, folded. The sandbox's card shows its container. Setting either
//! up stays in Project settings → Environment. What the panel says is in
//! `environment_model`.
use super::environment_model::{self as model, Diagnostics, Meter, Ran, Section, Tone};
use super::project_terminals_ui::{service_icon_button, status_dot};
use super::*;
use base64::Engine as _;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use widgets::ActionTone;

/// The active project's sandbox and Windows VM, as the engine last told,
/// and what the panel last read or ran in the VM.
#[derive(Default)]
pub(super) struct InUse {
    /// The project this is read for.
    space: Option<String>,
    sandbox: Option<sandbox::View>,
    windows_vm: Option<windows_vm::View>,
    /// Reads them again: every five seconds while the panel shows, every half
    /// minute for the rail (a project is also connected or sandboxed from a
    /// terminal, or by an agent).
    _poll: Option<Task<()>>,
    /// The settings were opened from the panel: keep the sandbox and VM
    /// groups (the end of the page) in view as they load.
    pub(super) reveal: bool,
    /// What a part is doing now, in the words shown for it.
    busy: HashMap<Section, &'static str>,
    /// Why a part's last action failed.
    errors: HashMap<Section, String>,
    diagnostics: Option<Diagnostics>,
    read_at: Option<Instant>,
    /// The VM's screen as last captured, and its width over its height.
    screen: Option<(Arc<gpui::Image>, f32)>,
    screen_hash: u64,
    /// The health lists that are unfolded.
    unfolded: HashSet<&'static str>,
    /// The last command and how it ran.
    ran: Option<(String, Ran)>,
    /// The last program started on the VM's desktop.
    launched: Option<String>,
    command: Option<Entity<ComposerInput>>,
    _command: Option<Subscription>,
    /// Rebuild was pressed once: a second press within a few seconds goes
    /// ahead (it stops what runs in the container).
    rebuild_armed: Option<Instant>,
    rebuilding: bool,
    /// Why the last rebuild asked for here failed.
    sandbox_error: Option<String>,
}

impl InUse {
    /// The project's sandbox as last read, for the build log's window.
    pub(super) fn sandbox(&self) -> Option<&sandbox::View> {
        self.sandbox.as_ref()
    }
}

/// How long a first press of Rebuild waits for the second.
const REBUILD_CONFIRM: std::time::Duration = std::time::Duration::from_secs(6);

/// The cards' shared inset: their text, controls and hairlines start here.
const INSET: f32 = 14.0;

fn tone_color(theme: &Theme, tone: Tone) -> gpui::Hsla {
    match tone {
        Tone::Good => theme.success,
        Tone::Busy => theme.warning,
        Tone::Off => theme.text_muted,
        Tone::Bad => theme.danger,
    }
}

/// What is shown while `action` runs.
fn busy_label(action: &str, vm_running: bool) -> &'static str {
    match action {
        "start" if vm_running => "Reconnecting this project's ports…",
        "start" => "Starting Windows…",
        "stop" => "Shutting Windows down…",
        "view" => "Opening the viewer…",
        "diagnostics" => "Reading Windows' health…",
        "screenshot" => "Capturing the screen…",
        "launch" => "Starting it on the desktop…",
        _ => "Running…",
    }
}

/// A state as a pill: a tinted fill with the state's word.
fn pill(theme: &Theme, tone: Tone, text: impl Into<SharedString>) -> gpui::Div {
    match tone {
        Tone::Good => widgets::badge_active(theme, text),
        Tone::Off => widgets::badge(theme, text),
        Tone::Busy | Tone::Bad => {
            let color = tone_color(theme, tone);
            widgets::badge(theme, text)
                .bg(color.opacity(0.12))
                .text_color(color.opacity(0.95))
        }
    }
}

/// A port or a range of them.
fn chip(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    widgets::badge(theme, text)
        .font_family(theme.font_mono.clone())
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text)
}

fn port_chips(theme: &Theme, ports: &[u16]) -> Vec<gpui::Div> {
    zeron_proto::sandbox::port_ranges(ports)
        .iter()
        .map(|range| chip(theme, range.to_string()))
        .collect()
}

/// A name with what belongs to it beside it: chips, a path, a sentence.
fn fact(theme: &Theme, label: &'static str) -> gpui::Div {
    div()
        .min_h(px(20.0))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(6.0))
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text)
        .child(
            div()
                .w(px(108.0))
                .flex_none()
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(theme.text_muted)
                .child(label),
        )
}

/// How much of something is taken: its name, a bar, the amount.
fn meter(theme: &Theme, meter: &Meter) -> gpui::Div {
    let tone = model::level(meter.fraction);
    let fill = match tone {
        Tone::Good => theme.accent.opacity(0.8),
        tone => tone_color(theme, tone).opacity(0.9),
    };
    div()
        .h(px(18.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .text_size(crate::typography::ui_rems(11.5))
        .child(
            div()
                .w(px(56.0))
                .flex_none()
                .text_color(theme.text_muted)
                .child(meter.label),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(40.0))
                .h(px(4.0))
                .rounded_full()
                .overflow_hidden()
                .bg(theme.wash(0.08))
                .when(meter.fraction > 0.0, |track| {
                    track.child(
                        div()
                            .h_full()
                            .w(gpui::relative(meter.fraction.max(0.015)))
                            .rounded_full()
                            .bg(fill),
                    )
                }),
        )
        .child(
            div()
                .w(px(112.0))
                .flex_none()
                .text_right()
                .text_color(match tone {
                    Tone::Good => theme.text_muted,
                    tone => tone_color(theme, tone),
                })
                .child(SharedString::from(meter.value.clone())),
        )
}

fn card(theme: &Theme) -> gpui::Div {
    widgets::section_card(theme).mt_0()
}

/// A card's name above it, as on the settings pages.
fn heading(theme: &Theme, text: &'static str) -> gpui::Div {
    widgets::section_label(theme, text).mt(px(22.0)).mb(px(8.0))
}

fn hint(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(crate::typography::ui_rems(12.0))
        .line_height(crate::typography::ui_rems(17.0))
        .text_color(theme.text_muted)
        .child(text.into())
}

/// Something is happening: a spinner and what.
fn working(theme: &Theme, text: &'static str, cx: &mut Context<Shell>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(7.0))
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text_muted)
        .child(loaders::mini_mono_spinner(
            text,
            1.5,
            theme.warning,
            cx.entity_id(),
            cx,
        ))
        .child(text)
}

/// Lines of fixed-width text: listeners, log lines, a command's output.
fn lines(theme: &Theme, text: impl IntoIterator<Item = String>) -> gpui::Div {
    div()
        .px(px(10.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .bg(theme.wash(0.04))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .font_family(theme.font_mono.clone())
        .text_size(crate::typography::ui_rems(11.5))
        .line_height(crate::typography::ui_rems(16.0))
        .text_color(theme.text)
        .children(text.into_iter().map(SharedString::from))
}

/// The part of a card where its screen would be, when there is none.
fn stage(theme: &Theme) -> gpui::Div {
    div()
        .w_full()
        .h(px(148.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(10.0))
        .bg(theme.wash(0.03))
        .border_b_1()
        .border_color(widgets::row_divider(theme))
}

impl Shell {
    /// What the active project uses, for the rail entry's label; `None` for
    /// neither. Keeps the project's state read.
    pub(in crate::shell) fn environment_in_use(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<&'static str> {
        let space = self.terminal_project_context(None, cx)?.space;
        if self.worktree_settings.environment.space.as_deref() != Some(&space) {
            if let Some((old, _)) = self.worktree_settings.environment.screen.take() {
                // After this render: the window is not the app's to reach now.
                cx.defer(move |cx| gpui::ImageSource::Image(old).evict(None, cx));
            }
            self.worktree_settings.environment = InUse::default();
            self.watch_environment(cx);
        }
        let vm = self.windows_vm_plugin_enabled(cx);
        let state = &self.worktree_settings.environment;
        model::label(
            state.sandbox.as_ref().is_some_and(|v| v.sandbox.is_some()),
            vm && state
                .windows_vm
                .as_ref()
                .is_some_and(|v| v.settings.is_some()),
        )
    }

    fn environment_panel_visible(&self, cx: &App) -> bool {
        self.right_pane_shown(cx) && self.resolved_right_active(cx) == RightSurface::Environment
    }

    /// Reads the active project's sandbox and VM now, and keeps reading.
    /// What is shown stays until the answer arrives.
    pub(in crate::shell) fn watch_environment(&mut self, cx: &mut Context<Self>) {
        let Some(mut context) = self.terminal_project_context(None, cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        (context.chat, context.checkout_override) = (None, None);
        let project = context.space.clone();
        self.worktree_settings.environment.space = Some(project.clone());
        self.worktree_settings.environment._poll = Some(cx.spawn(async move |this, cx| {
            let mut idle = 0;
            loop {
                // The panel shows the VM itself; the rail only what is set up.
                let Ok(watched) = this.update(cx, |shell, cx| shell.environment_panel_visible(cx))
                else {
                    return;
                };
                if !watched && idle > 0 {
                    idle = (idle + 1) % 6;
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(5))
                        .await;
                    continue;
                }
                idle = usize::from(!watched);
                let params = context.params(serde_json::json!({ "brief": !watched }));
                let client = engine.client();
                let sandbox = client
                    .call_as::<sandbox::View>(methods::GET_PROJECT_SANDBOX, params.clone())
                    .await
                    .ok();
                let mut vm = client
                    .call_as::<windows_vm::View>(methods::GET_PROJECT_WINDOWS_VM, params)
                    .await
                    .ok();
                let current = this.update(cx, |shell, cx| {
                    let state = &mut shell.worktree_settings.environment;
                    if state.space.as_deref() != Some(&project) {
                        return false;
                    }
                    if !watched && let (Some(new), Some(old)) = (&mut vm, &state.windows_vm) {
                        new.vm = old.vm.clone();
                    }
                    if state.sandbox != sandbox || state.windows_vm != vm {
                        state.sandbox = sandbox;
                        state.windows_vm = vm;
                        cx.notify();
                    }
                    if watched {
                        shell.refresh_environment_readings(cx);
                    }
                    true
                });
                if !current.unwrap_or(false) {
                    return;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(5))
                    .await;
            }
        }));
    }

    /// While the panel shows a running VM: its screen again, and a first
    /// health reading (later ones are asked for; they cost Windows more).
    fn refresh_environment_readings(&mut self, cx: &mut Context<Self>) {
        if !self.environment_vm_running() || !self.windows_vm_plugin_enabled(cx) {
            return;
        }
        self.environment_vm_action("screenshot", None, cx);
        let state = &self.worktree_settings.environment;
        if state.diagnostics.is_none() && !state.errors.contains_key(&Section::Diagnostics) {
            self.environment_vm_action("diagnostics", None, cx);
        }
    }

    pub(in crate::shell) fn open_environment_panel(&mut self, cx: &mut Context<Self>) {
        if !self.session_workspace_visible(cx) {
            return;
        }
        self.close_files_panel(cx);
        self.set_surfaces_open(true, cx);
        push_unique_right_surface(
            self.right_tabs.entry(self.panel_key(cx)).or_default(),
            RightSurface::Environment,
        );
        self.set_right_active(RightSurface::Environment, cx);
    }

    fn environment_vm_running(&self) -> bool {
        self.worktree_settings
            .environment
            .windows_vm
            .as_ref()
            .is_some_and(|view| view.settings.is_some() && view.vm.state == "running")
    }

    /// Runs one thing in or on the VM for the active project and keeps what
    /// it answered for its section: `start`, `stop`, `view`, `diagnostics`,
    /// `screenshot`, `exec` and `launch` (the last two with a command).
    fn environment_vm_action(
        &mut self,
        action: &'static str,
        command: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let section = Section::of(action);
        let Some(mut context) = self.terminal_project_context(None, cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let running = self.environment_vm_running();
        let state = &mut self.worktree_settings.environment;
        if state.busy.contains_key(&section) {
            return;
        }
        (context.chat, context.checkout_override) = (None, None);
        state.busy.insert(section, busy_label(action, running));
        state.errors.remove(&section);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let params =
                context.params(serde_json::json!({ "action": action, "command": command }));
            let reply = engine
                .client()
                .call_as::<windows_vm::View>(methods::CONTROL_PROJECT_WINDOWS_VM, params)
                .await;
            this.update(cx, |shell, cx| {
                if shell.worktree_settings.environment.space.as_deref() != Some(&context.space) {
                    return;
                }
                shell.worktree_settings.environment.busy.remove(&section);
                let answered = reply
                    .map_err(|error| error.to_string())
                    .and_then(|mut view| {
                        let result = std::mem::take(&mut view.result);
                        let state = &mut shell.worktree_settings.environment;
                        // Like a brief reply, the screen comes without the
                        // VM's state: what is shown stays.
                        if action == "screenshot"
                            && let Some(old) = &state.windows_vm
                        {
                            view.vm = old.vm.clone();
                        }
                        state.windows_vm = Some(view);
                        shell.keep_environment_result(action, command, result, cx)
                    });
                if let Err(error) = answered {
                    shell
                        .worktree_settings
                        .environment
                        .errors
                        .insert(section, error);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Keeps what an action answered where its section shows it.
    fn keep_environment_result(
        &mut self,
        action: &str,
        command: Option<String>,
        result: serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let state = &mut self.worktree_settings.environment;
        match action {
            "diagnostics" => {
                state.diagnostics = Some(Diagnostics::read(result)?);
                state.read_at = Some(Instant::now());
            }
            "screenshot" => {
                let data = result["data"].as_str().unwrap_or_default();
                // The same picture again (an idle desktop): keep the one shown.
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                data.hash(&mut hasher);
                if state.screen.is_some() && state.screen_hash == hasher.finish() {
                    return Ok(());
                }
                state.screen_hash = hasher.finish();
                let png = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .ok()
                    .filter(|png| !png.is_empty())
                    .ok_or("The VM answered without a picture.")?;
                let (width, height) =
                    model::png_size(&png).ok_or("The VM's picture is not a PNG.")?;
                let image = Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png));
                if let Some((old, _)) = state.screen.replace((image, width as f32 / height as f32))
                {
                    gpui::ImageSource::Image(old).evict(None, cx);
                }
            }
            "exec" => {
                state.ran = Some((command.unwrap_or_default(), Ran::read(result)?));
                state.launched = None;
            }
            "launch" => {
                state.launched = command;
                state.ran = None;
            }
            _ => {}
        }
        Ok(())
    }

    /// The command line of the "Run a command" section, made on first use.
    fn environment_command_input(&mut self, cx: &mut Context<Self>) -> Entity<ComposerInput> {
        if let Some(input) = &self.worktree_settings.environment.command {
            return input.clone();
        }
        let input = cx.new(|cx| {
            ComposerInput::new("hostname", cx)
                .with_single_line()
                .with_text_metrics(12.0, 17.0)
        });
        let state = &mut self.worktree_settings.environment;
        state._command = Some(cx.subscribe(&input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.run_environment_command("exec", cx);
            }
        }));
        state.command = Some(input.clone());
        input
    }

    /// Runs what is typed: `exec` in PowerShell, `launch` on the desktop.
    fn run_environment_command(&mut self, action: &'static str, cx: &mut Context<Self>) {
        let Some(input) = &self.worktree_settings.environment.command else {
            return;
        };
        let command = input.read(cx).text().trim().to_string();
        if !command.is_empty() {
            self.environment_vm_action(action, Some(command), cx);
        }
    }

    /// Rebuilds the sandbox's image and container with the settings it has.
    /// The first press asks; the second, soon after, goes ahead.
    fn rebuild_environment_sandbox(&mut self, cx: &mut Context<Self>) {
        let Some(mut project) = self.terminal_project_context(None, cx) else {
            return;
        };
        let state = &mut self.worktree_settings.environment;
        let Some(sandbox) = state.sandbox.as_ref().and_then(|view| view.sandbox.clone()) else {
            return;
        };
        if state.rebuilding {
            return;
        }
        if !state
            .rebuild_armed
            .is_some_and(|at| at.elapsed() < REBUILD_CONFIRM)
        {
            state.rebuild_armed = Some(Instant::now());
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(REBUILD_CONFIRM).await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
            cx.notify();
            return;
        }
        state.rebuild_armed = None;
        state.rebuilding = true;
        state.sandbox_error = None;
        (project.chat, project.checkout_override) = (None, None);
        self.rebuild_sandbox(project, &sandbox, cx, |shell, result, cx| {
            let state = &mut shell.worktree_settings.environment;
            state.rebuilding = false;
            state.sandbox_error = result.err();
            shell.watch_environment(cx);
        });
        cx.notify();
    }

    /// A labelled button of a card. `enabled` false draws it dimmed and
    /// inert, while what it would do is already running.
    #[allow(clippy::too_many_arguments)]
    fn environment_button(
        &self,
        theme: &Theme,
        tone: ActionTone,
        id: &'static str,
        glyph: Option<&'static str>,
        label: &'static str,
        enabled: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let ink = match tone {
            ActionTone::Solid => theme.on_solid,
            ActionTone::Quiet => theme.text_muted,
            _ => theme.text,
        };
        widgets::action_button(theme, tone, id)
            .min_h(px(28.0))
            .flex_none()
            .children(glyph.map(|glyph| icon(glyph).size(px(13.0)).text_color(ink)))
            .child(label)
            .id(id)
            .debug_selector(move || id.into())
            .role(gpui::Role::Button)
            .aria_label(label)
            .tab_index(0)
            .when(!enabled, |button| button.opacity(0.5).cursor_default())
            .when(enabled, |button| {
                button.on_click(cx.listener(move |this, _, _, cx| action(this, cx)))
            })
    }

    /// The Windows VM's card: its screen, what it is and how loaded, the
    /// ports that cross, and what can be done with it.
    fn environment_vm_card(
        &self,
        theme: &Theme,
        view: &windows_vm::View,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let environment = &self.worktree_settings.environment;
        let connection = view.settings.as_ref();
        let (tone, word) = model::vm_state(connection.is_some(), &view.vm.state);
        let busy = environment.busy.get(&Section::Vm).copied();
        let idle = busy.is_none();
        let state = view.vm.state.as_str();
        let reading = environment.diagnostics.as_ref().filter(|_| state == "running");
        let mut card = card(theme).debug_selector(|| "environment-vm".into());

        // Its screen, or what stands in for it.
        if connection.is_some() {
            card = card.child(match (state, environment.screen.clone()) {
                ("running", Some((image, ratio))) => div()
                    .id("environment-screen")
                    .debug_selector(|| "environment-screen".into())
                    .relative()
                    .w_full()
                    // A tall guest screen is letterboxed, not a page long.
                    .aspect_ratio(ratio.max(1.6))
                    .bg(gpui::black().opacity(0.35))
                    .border_b_1()
                    .border_color(gpui::white().opacity(0.1))
                    .cursor_pointer()
                    .role(gpui::Role::Button)
                    .aria_label("The VM's screen. Opens the viewer window.")
                    .tooltip(settings::widgets::text_tooltip(
                        "A picture of the screen, taken again every few seconds. Click for the live viewer.",
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.environment_vm_action("view", None, cx)
                    }))
                    .child(
                        gpui::img(image)
                            .size_full()
                            .object_fit(gpui::ObjectFit::Contain),
                    )
                    .into_any_element(),
                ("running", None) => stage(theme)
                    .child(match environment.errors.get(&Section::Screen) {
                        Some(error) => hint(theme, error.clone()).px(px(INSET)),
                        None => working(theme, "Capturing the screen…", cx),
                    })
                    .into_any_element(),
                ("starting", _) => stage(theme)
                    .child(working(theme, "Windows is starting…", cx))
                    .into_any_element(),
                ("stopped", _) => stage(theme)
                    .child(
                        icon(icons::MONITOR)
                            .size(px(22.0))
                            .text_color(theme.text_faint),
                    )
                    .child(hint(theme, "Windows is off"))
                    .child(self.environment_button(
                        theme,
                        ActionTone::Solid,
                        "environment-vm-start",
                        Some(icons::ACTION_PLAY),
                        "Start VM",
                        idle,
                        cx,
                        |this, cx| this.environment_vm_action("start", None, cx),
                    ))
                    .into_any_element(),
                _ => stage(theme)
                    .child(
                        icon(icons::DANGER_TRIANGLE)
                            .size(px(20.0))
                            .text_color(theme.text_faint),
                    )
                    .child(
                        hint(
                            theme,
                            view.vm
                                .error
                                .clone()
                                .unwrap_or_else(|| "The VM can't be used on this machine yet.".into()),
                        )
                        .px(px(INSET)),
                    )
                    .into_any_element(),
            });
        }

        // What it is.
        let about: Vec<AnyElement> = match (connection, reading) {
            (None, _) => vec!["Not this project's yet".into_any_element()],
            (Some(_), Some(reading)) => reading
                .headline()
                .into_iter()
                .map(|part| SharedString::from(part).into_any_element())
                .collect(),
            (Some(_), None) => vec!["This project's Windows machine".into_any_element()],
        };
        card = card.child(
            div()
                .px(px(INSET))
                .pt(px(12.0))
                .pb(px(12.0))
                .flex()
                .items_start()
                .gap(px(10.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(widgets::row_title(theme, "Windows VM"))
                        .child(widgets::meta_line(theme, about)),
                )
                .child(pill(theme, tone, word).mt(px(1.0))),
        );
        let Some(connection) = connection else {
            return card.child(
                div()
                    .px(px(INSET))
                    .pb(px(INSET))
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(10.0))
                    .child(hint(
                        theme,
                        "Connected, this project's agents can use the VM, and every port its servers listen on is the same localhost port in Windows.",
                    ))
                    .child(self.environment_button(
                        theme,
                        ActionTone::Filled,
                        "environment-vm-connect",
                        None,
                        "Connect in Settings",
                        true,
                        cx,
                        |this, cx| this.show_project_environment_settings(cx),
                    )),
            );
        };

        // How loaded it is.
        if let Some(reading) = reading {
            card = card.child(
                div()
                    .px(px(INSET))
                    .pb(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .children(reading.meters().iter().map(|one| meter(theme, one))),
            );
        }

        // What crosses.
        if matches!(state, "running" | "starting") {
            let mut crossing = div()
                .px(px(INSET))
                .pb(px(12.0))
                .flex()
                .flex_col()
                .gap(px(6.0));
            let into = fact(theme, "Ports in Windows");
            crossing = crossing.child(if view.vm.to_vm.is_empty() {
                into.child(
                    div()
                        .text_color(theme.text_muted)
                        .child(if connection.auto_ports {
                            "None yet · this project's servers connect by themselves"
                        } else {
                            "None · list them in Settings"
                        }),
                )
            } else {
                into.children(port_chips(theme, &view.vm.to_vm))
            });
            if !connection.from_vm.is_empty() {
                crossing = crossing.child(
                    fact(theme, "Ports from Windows")
                        .children(port_chips(theme, &connection.from_vm)),
                );
            }
            card = card.child(crossing);
        }
        if let Some(problem) = view.vm.tunnel_error.as_ref().filter(|_| state == "running") {
            card = card.child(
                widgets::warning_strip(theme, problem.clone())
                    .mt_0()
                    .mx(px(INSET))
                    .mb(px(12.0)),
            );
        }
        if let Some(error) = environment.errors.get(&Section::Vm) {
            card = card.child(
                widgets::error_strip(theme, error.clone())
                    .mt_0()
                    .mx(px(INSET))
                    .mb(px(12.0)),
            );
        }

        // What can be done with it.
        if matches!(state, "running" | "starting") {
            let mut actions = div()
                .px(px(INSET))
                .py(px(10.0))
                .border_t_1()
                .border_color(widgets::row_divider(theme))
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.0))
                .child(self.environment_button(
                    theme,
                    ActionTone::Filled,
                    "environment-vm-view",
                    Some(icons::EYE),
                    "Open viewer",
                    idle,
                    cx,
                    |this, cx| this.environment_vm_action("view", None, cx),
                ))
                .child(self.environment_button(
                    theme,
                    ActionTone::Quiet,
                    "environment-vm-ports",
                    Some(icons::REFRESH),
                    "Reconnect ports",
                    idle,
                    cx,
                    |this, cx| this.environment_vm_action("start", None, cx),
                ));
            actions = match busy {
                Some(busy) => actions.child(working(theme, busy, cx).flex_1().justify_end()),
                None => actions.child(div().flex_1()).child(self.environment_button(
                    theme,
                    ActionTone::Quiet,
                    "environment-vm-stop",
                    Some(icons::STOP),
                    "Shut down",
                    true,
                    cx,
                    |this, cx| this.environment_vm_action("stop", None, cx),
                )),
            };
            card = card.child(actions);
        } else if let Some(busy) = busy {
            card = card.child(
                working(theme, busy, cx)
                    .px(px(INSET))
                    .pb(px(12.0)),
            );
        }
        card
    }

    /// A command line into Windows, and what the last command printed.
    fn environment_command_card(&mut self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let input = self.environment_command_input(cx);
        let environment = &self.worktree_settings.environment;
        let busy = environment.busy.get(&Section::Command).copied();
        let mut body = div()
            .p(px(INSET))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                popover::dialog_field(input.into_any_element())
                    .text_size(crate::typography::ui_rems(12.0)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.0))
                    .child(self.environment_button(
                        theme,
                        ActionTone::Filled,
                        "environment-command-run",
                        Some(icons::RETURN),
                        "Run in PowerShell",
                        busy.is_none(),
                        cx,
                        |this, cx| this.run_environment_command("exec", cx),
                    ))
                    .child(self.environment_button(
                        theme,
                        ActionTone::Quiet,
                        "environment-command-launch",
                        Some(icons::WINDOW_MAXIMIZE),
                        "Start on the desktop",
                        busy.is_none(),
                        cx,
                        |this, cx| this.run_environment_command("launch", cx),
                    ))
                    .children(busy.map(|busy| working(theme, busy, cx))),
            );
        if let Some(error) = environment.errors.get(&Section::Command) {
            body = body.child(widgets::error_strip(theme, error.clone()).mt_0());
        }
        if let Some((command, ran)) = &environment.ran {
            let (tone, outcome) = ran.outcome();
            let output = ran.output();
            let copy = output.clone();
            body = body
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(status_dot(tone_color(theme, tone)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::typography::ui_rems(11.5))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(format!("{outcome} · {command}"))),
                        )
                        .child(
                            service_icon_button(
                                theme,
                                "environment-command-copy".into(),
                                icons::COPY,
                                "Copy the output",
                            )
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                            })),
                        ),
                )
                .child(
                    lines(theme, output.lines().map(str::to_owned))
                        .debug_selector(|| "environment-command-output".into()),
                );
        } else if let Some(program) = &environment.launched {
            body = body.child(hint(
                theme,
                format!("Started on the Windows desktop: {program}. Its windows show on the screen above."),
            ));
        }
        card(theme)
            .debug_selector(|| "environment-command".into())
            .child(body)
    }

    /// The rest of a health reading, each list folded until asked for.
    fn environment_health_card(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let environment = &self.worktree_settings.environment;
        let mut card = card(theme).debug_selector(|| "environment-health".into());
        if let Some(error) = environment.errors.get(&Section::Diagnostics) {
            card = card.child(
                widgets::error_strip(theme, error.clone())
                    .mt_0()
                    .m(px(INSET)),
            );
        }
        let Some(reading) = &environment.diagnostics else {
            return card.child(div().p(px(INSET)).child(
                match environment.busy.get(&Section::Diagnostics) {
                    Some(busy) => working(theme, busy, cx),
                    None => hint(theme, "No reading yet."),
                },
            ));
        };
        let processes = reading.processes();
        let lists: [(&'static str, &'static str, usize, AnyElement); 4] = [
            (
                "environment-health-processes",
                "Biggest processes",
                processes.len(),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .children(processes.into_iter().map(|(name, memory)| {
                        div()
                            .flex()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.text)
                                    .child(SharedString::from(name)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(memory)),
                            )
                    }))
                    .into_any_element(),
            ),
            (
                "environment-health-ports",
                "Listening ports",
                reading.listening_ports.len(),
                lines(theme, reading.listening_ports.iter().cloned()).into_any_element(),
            ),
            (
                "environment-health-services",
                "Automatic services that are stopped",
                reading.stopped_automatic_services.len(),
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.0))
                    .children(
                        reading
                            .stopped_automatic_services
                            .iter()
                            .map(|name| chip(theme, name.clone())),
                    )
                    .into_any_element(),
            ),
            (
                "environment-health-errors",
                "Errors in the last hour",
                reading.recent_errors.len(),
                lines(theme, reading.recent_errors.iter().cloned()).into_any_element(),
            ),
        ];
        card = card.child(
            fact(theme, "Display")
                .px(px(INSET))
                .py(px(10.0))
                .child(SharedString::from(reading.display.clone())),
        );
        for (id, title, count, list) in lists {
            let open = count > 0 && environment.unfolded.contains(id);
            card = card.child(
                div()
                    .id(id)
                    .debug_selector(move || id.into())
                    .mx(px(INSET))
                    .py(px(10.0))
                    .border_t_1()
                    .border_color(widgets::row_divider(theme))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(crate::typography::ui_rems(12.5))
                    .text_color(theme.text)
                    .role(gpui::Role::Button)
                    .aria_label(title)
                    .aria_expanded(open)
                    .when(count > 0, |row| {
                        row.tab_index(0).cursor_pointer().on_click(cx.listener(
                            move |this, _, _, cx| {
                                let unfolded = &mut this.worktree_settings.environment.unfolded;
                                if !unfolded.remove(id) {
                                    unfolded.insert(id);
                                }
                                cx.notify();
                            },
                        ))
                    })
                    .child(
                        icon(if open {
                            icons::ALT_ARROW_DOWN
                        } else {
                            icons::ALT_ARROW_RIGHT
                        })
                        .size(px(12.0))
                        .text_color(if count > 0 {
                            theme.text_muted
                        } else {
                            theme.text_faint
                        }),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .child(widgets::badge(
                        theme,
                        if count == 0 {
                            "None".to_string()
                        } else {
                            count.to_string()
                        },
                    )),
            );
            if open {
                card = card.child(div().px(px(INSET)).pb(px(12.0)).child(list));
            }
        }
        card
    }

    /// The sandbox's card: its container, what it reaches, and a way in.
    fn environment_sandbox_card(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let environment = &self.worktree_settings.environment;
        let view = environment.sandbox.as_ref();
        let armed = environment
            .rebuild_armed
            .is_some_and(|at| at.elapsed() < REBUILD_CONFIRM);
        let card = card(theme).debug_selector(|| "environment-sandbox".into());
        let top = |about: Vec<AnyElement>, state: gpui::Div| {
            div()
                .px(px(INSET))
                .pt(px(12.0))
                .pb(px(12.0))
                .flex()
                .items_start()
                .gap(px(10.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(widgets::row_title(theme, "Container"))
                        .child(widgets::meta_line(theme, about)),
                )
                .child(state.mt(px(1.0)))
        };
        let footer = || {
            div()
                .px(px(INSET))
                .py(px(10.0))
                .border_t_1()
                .border_color(widgets::row_divider(theme))
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.0))
        };
        let Some(sandbox) = view.and_then(|view| view.sandbox.as_ref()) else {
            return card
                .child(top(
                    vec!["Agents, services and terminals run on this machine".into_any_element()],
                    pill(theme, Tone::Off, "Off"),
                ))
                .child(footer().child(self.environment_button(
                    theme,
                    ActionTone::Filled,
                    "environment-sandbox-setup",
                    None,
                    "Set up in Settings",
                    true,
                    cx,
                    |this, cx| this.show_project_environment_settings(cx),
                )));
        };
        let building = environment.rebuilding || view.is_some_and(|view| view.build.running);
        let (tone, word) = model::sandbox_state(&sandbox.state, building);
        let mut about = vec![
            div()
                .font_family(theme.font_mono.clone())
                .child(SharedString::from(sandbox.container.clone()))
                .into_any_element(),
        ];
        if !sandbox.image_built {
            about.push("image not built".into_any_element());
        }
        let config = sandbox.config.clone();
        let mut facts = div()
            .px(px(INSET))
            .pb(px(12.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                fact(theme, "Config").flex_nowrap().child(
                    div()
                        .id("environment-sandbox-config")
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .tooltip(settings::widgets::text_tooltip(config.clone()))
                        .child(SharedString::from(config)),
                ),
            );
        let reaches = fact(theme, "Reaches here");
        facts = facts.child(if sandbox.host_ports.is_empty() {
            reaches.child(div().text_color(theme.text_muted).child("No port of this machine"))
        } else {
            reaches.children(port_chips(theme, &sandbox.host_ports))
        });
        let published = fact(theme, "Published here");
        facts = facts.child(if sandbox.publish_ports.is_empty() {
            published.child(div().text_color(theme.text_muted).child("Every port a server opens"))
        } else {
            published.children(port_chips(theme, &sandbox.publish_ports))
        });
        if let Some(step) = view.filter(|_| building).and_then(|view| view.build.latest()) {
            facts = facts.child(
                working(theme, "Building", cx)
                    .child(div().min_w_0().truncate().child(SharedString::from(
                        step.trim_start_matches('▸').trim().to_string(),
                    ))),
            );
        }
        if armed {
            facts = facts.child(
                widgets::warning_strip(
                    theme,
                    "Rebuilding stops the agents, services and terminals running in the container, and takes minutes. Press Rebuild now to go ahead.",
                )
                .mt_0(),
            );
        }
        if let Some(error) = &environment.sandbox_error {
            facts = facts.child(widgets::error_strip(theme, error.clone()).mt_0());
        }
        card.child(top(about, pill(theme, tone, word)))
            .child(facts)
            .child(
                footer()
                    .child(self.environment_button(
                        theme,
                        ActionTone::Filled,
                        "environment-sandbox-terminal",
                        Some(icons::TERMINAL),
                        "Open terminal",
                        true,
                        cx,
                        |this, cx| this.open_shell_panel(cx),
                    ))
                    .child(self.environment_button(
                        theme,
                        if armed {
                            ActionTone::Outlined
                        } else {
                            ActionTone::Quiet
                        },
                        "environment-sandbox-rebuild",
                        Some(icons::RESTART),
                        if armed { "Rebuild now" } else { "Rebuild" },
                        !building,
                        cx,
                        |this, cx| this.rebuild_environment_sandbox(cx),
                    ))
                    .child(self.environment_button(
                        theme,
                        ActionTone::Quiet,
                        "environment-sandbox-log",
                        Some(icons::DOCUMENT),
                        "Show log",
                        true,
                        cx,
                        |this, cx| this.open_sandbox_log(cx),
                    )),
            )
    }

    /// The Environment panel: the active project's Windows VM and sandbox.
    pub(in crate::shell) fn render_environment_panel(
        &mut self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let theme = &theme;
        let project = self.terminal_project_context(None, cx);
        let environment = &self.worktree_settings.environment;
        let vm = environment
            .windows_vm
            .clone()
            .filter(|view| view.settings.is_some() || view.available)
            .filter(|_| self.windows_vm_plugin_enabled(cx));
        let running = self.environment_vm_running();
        let read = environment
            .read_at
            .map(|at| format!("Read {}", model::ago(at.elapsed().as_secs())));
        let reading = environment.busy.contains_key(&Section::Diagnostics);
        let mut column = div()
            .w_full()
            .max_w(px(720.0))
            .mx_auto()
            .px(px(12.0))
            .pt(px(12.0))
            .pb(px(24.0))
            .flex()
            .flex_col();
        if let Some(view) = &vm {
            column = column.child(self.environment_vm_card(theme, view, cx));
            if running {
                column = column
                    .child(heading(theme, "Run in Windows"))
                    .child(self.environment_command_card(theme, cx))
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .child(heading(theme, "Health").flex_1())
                            .child(
                                div()
                                    .mb(px(4.0))
                                    .flex()
                                    .items_center()
                                    .gap(px(4.0))
                                    .text_size(crate::typography::ui_rems(11.5))
                                    .text_color(theme.text_faint)
                                    .children(read.map(SharedString::from))
                                    .child(
                                        service_icon_button(
                                            theme,
                                            "environment-health-read".into(),
                                            icons::REFRESH,
                                            "Read Windows' health again",
                                        )
                                        .debug_selector(|| "environment-health-read".into())
                                        .tab_index(0)
                                        .when(reading, |button| {
                                            button.opacity(0.4).cursor_default()
                                        })
                                        .when(!reading, |button| {
                                            button.on_click(cx.listener(|this, _, _, cx| {
                                                this.environment_vm_action("diagnostics", None, cx)
                                            }))
                                        }),
                                    ),
                            ),
                    )
                    .child(self.environment_health_card(theme, cx));
            }
            column = column.child(heading(theme, "Sandbox"));
        }
        column = column.child(self.environment_sandbox_card(theme, cx));
        div()
            .id("environment-panel")
            .debug_selector(|| "environment-panel".into())
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.panel_bg())
            .child(
                crate::surface_chrome::toolbar(theme)
                    .child(
                        div()
                            .id("environment-project-context")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .tooltip(settings::widgets::text_tooltip(
                                project
                                    .as_ref()
                                    .map(|project| project.cwd.clone())
                                    .unwrap_or_default(),
                            ))
                            .child(SharedString::from(
                                project.map(|project| project.name).unwrap_or_default(),
                            )),
                    )
                    .child(
                        service_icon_button(
                            theme,
                            "environment-settings".into(),
                            icons::SETTINGS,
                            "Set up this project's sandbox and Windows VM",
                        )
                        .debug_selector(|| "environment-settings".into())
                        .tab_index(0)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_project_environment_settings(cx)
                        })),
                    ),
            )
            .child(
                div()
                    .id("environment-body")
                    .debug_selector(|| "environment-body".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(column),
            )
            .into_any_element()
    }

    /// Opens the active project's settings on the Environment page (project
    /// defaults): the sandbox and the Windows VM.
    fn show_project_environment_settings(&mut self, cx: &mut Context<Self>) {
        let Some(space) = self.terminal_project_context(None, cx).map(|p| p.space) else {
            return;
        };
        let open = |shell: &Self| {
            shell
                .worktree_settings
                .editor
                .as_open()
                .is_some_and(|editor| editor.project.space == space)
        };
        if open(self) {
            self.select_worktree_settings(None, cx);
        } else {
            self.open_worktree_settings(space.clone(), cx);
        }
        if open(self) {
            self.select_worktree_settings_page(SettingsPage::Environment, cx);
            self.worktree_settings.environment.reveal = true;
            self.reveal_environment_groups();
        }
    }

    /// After the panel opened the settings: scrolls to the sandbox and VM
    /// groups, again as each of them loads below what is already there.
    pub(super) fn reveal_environment_groups(&mut self) {
        if self.worktree_settings.environment.reveal
            && self.worktree_settings.page == SettingsPage::Environment
            && let Some(editor) = self.worktree_settings.editor.as_open()
        {
            editor.scroll.scroll_to_bottom();
        }
    }
}
