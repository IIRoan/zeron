//! Linux workbench chrome: tools on the left, projects on the right.
use super::*;

pub(super) const ACTIVITY_WIDTH: f32 = if cfg!(target_os = "linux") { 44.0 } else { 0.0 };

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Explorer,
    Changes,
    Browser,
    History,
    Views,
}

impl Shell {
    fn select_tool(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        if tool == Tool::Explorer {
            self.open_explorer(window, cx);
            return;
        }
        self.close_files_panel(cx);
        self.set_surfaces_open(true, cx);
        if tool == Tool::Views {
            self.set_right_active(RightSurface::Picker, cx);
            return;
        }
        let existing = self
            .right_surface_rows(cx)
            .into_iter()
            .find_map(|(surface, _, _, _)| {
                let matches = match (tool, surface) {
                    (Tool::Browser, RightSurface::Browser(_)) => true,
                    (Tool::Changes, RightSurface::Diff(id)) => self
                        .diffs
                        .get(&id)
                        .is_some_and(|d| d.read(cx).tab_title().as_ref() == "Source Control"),
                    (Tool::History, RightSurface::Diff(id)) => {
                        self.diffs.get(&id).is_some_and(|d| d.read(cx).is_history())
                    }
                    _ => false,
                };
                matches.then_some(surface)
            });
        if let Some(surface) = existing {
            self.set_right_active(surface, cx);
            self.focus_right_file_editor(surface, window, cx);
        } else {
            match tool {
                Tool::Changes => self.add_diff_surface(window, cx),
                Tool::History => self.add_history_surface(window, cx),
                Tool::Browser => self.add_browser_surface(None, window, cx),
                _ => {}
            }
        }
    }

    pub(super) fn open_explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cfg!(target_os = "linux") {
            self.add_files_surface(window, cx);
            return;
        }
        if self.active_chat.is_empty() {
            return;
        }
        self.ensure_files_explorer(window, cx);
        self.close_files_panel(cx);
        self.files_tween = None;
        self.set_surfaces_open(true, cx);
        push_unique_right_surface(
            self.right_tabs.entry(self.panel_key(cx)).or_default(),
            RightSurface::Explorer,
        );
        self.activate_right_surface(RightSurface::Explorer, window, cx);
    }

    pub(super) fn toggle_explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if cfg!(target_os = "linux") {
            if self.right_pane_open(cx) && self.resolved_right_active(cx) == RightSurface::Explorer
            {
                self.close_right_surface(RightSurface::Explorer, window, cx);
            } else {
                self.open_explorer(window, cx);
            }
        } else {
            self.toggle_files_panel(window, cx);
        }
    }

    pub(super) fn render_activity_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let active = self.resolved_right_active(cx);
        let explorer = self.files_panel_open(cx)
            || (self.right_pane_open(cx) && active == RightSurface::Explorer);
        let open = self.right_pane_open(cx);
        let git = self.space_git_detected(cx);
        let buttons = [
            (
                Tool::Explorer,
                "workbench-explorer",
                icons::FILE_TREE,
                "Explorer",
            ),
            (
                Tool::Changes,
                "workbench-changes",
                icons::SOURCE_CONTROL,
                "Source Control",
            ),
            (
                Tool::History,
                "workbench-history",
                icons::HISTORY,
                "History",
            ),
            (Tool::Views, "workbench-views", icons::LIST, "All views"),
        ]
        .into_iter()
        .filter(|(tool, ..)| git || !matches!(tool, Tool::Changes | Tool::History))
        .map(|(tool, id, icon_path, label)| {
            let selected = if tool == Tool::Explorer {
                explorer
            } else {
                open && match (tool, active) {
                    (Tool::Browser, RightSurface::Browser(_))
                    | (Tool::Views, RightSurface::Picker) => true,
                    (Tool::Changes, RightSurface::Diff(id)) => self
                        .diffs
                        .get(&id)
                        .is_some_and(|d| d.read(cx).tab_title().as_ref() == "Source Control"),
                    (Tool::History, RightSurface::Diff(id)) => {
                        self.diffs.get(&id).is_some_and(|d| d.read(cx).is_history())
                    }
                    _ => false,
                }
            };
            div()
                .id(id)
                .role(gpui::Role::Button)
                .aria_label(label)
                .w_full()
                .h(px(44.0))
                .flex()
                .items_center()
                .justify_center()
                .border_l_2()
                .border_color(if selected {
                    theme.accent
                } else {
                    gpui::transparent_black()
                })
                .text_color(if selected {
                    theme.text
                } else {
                    theme.text_muted
                })
                .cursor_pointer()
                .hover(|el| el.bg(theme.glass_hover()))
                .tooltip(settings::widgets::text_tooltip(label))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.select_tool(tool, window, cx)),
                )
                .child(icon(icon_path).size(px(21.0)).text_color(if selected {
                    theme.text
                } else {
                    theme.text_muted
                }))
                .into_any_element()
        })
        .collect::<Vec<_>>();
        div()
            .id("workbench-activity")
            .w(px(ACTIVITY_WIDTH))
            .h_full()
            .flex_none()
            .pt(px(Theme::TITLEBAR_HEIGHT))
            .flex()
            .flex_col()
            .bg(theme.panel_bg())
            .border_r_1()
            .border_color(theme.border)
            .children(buttons)
            .into_any_element()
    }

    pub(super) fn render_workbench_titlebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let in_session = self.session_workspace_visible(cx);
        let files_width = if in_session { self.files_visible_width(cx) } else { 0.0 };
        let tools_width = if in_session { self.right_visible_width(cx) } else { 0.0 };
        let left = if in_session { ACTIVITY_WIDTH + files_width + tools_width } else { ACTIVITY_WIDTH };
        let sidebar = if in_session { self.sidebar_now() } else { 0.0 };
        let title: SharedString = self
            .state
            .read(cx)
            .selected_chat_row()
            .map(|chat| {
                chat.title
                    .clone()
                    .unwrap_or_else(|| "New session".into())
                    .into()
            })
            .unwrap_or_else(|| "Zeron".into());
        let tabs = self.render_right_tab_strip(cx);
        let brand = div()
            .id("workbench-brand")
            .debug_selector(|| "workbench-brand".into())
            .absolute()
            .left_0()
            .top_0()
            .w(px(ACTIVITY_WIDTH))
            .h(px(Theme::TITLEBAR_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .border_b_1()
            .border_r_1()
            .border_color(theme.border)
            .bg(theme.panel_bg())
            .role(gpui::Role::Image)
            .aria_label("Solace")
            .tooltip(settings::widgets::text_tooltip("Solace"))
            .child(
                icon(icons::SOLACE_LOGO)
                    .size(px(26.0))
                    .text_color(theme.text),
            );
        let tools = div()
            .absolute()
            .left(px(ACTIVITY_WIDTH + files_width))
            .top_0()
            .w(px(tools_width))
            .h(px(Theme::TITLEBAR_HEIGHT))
            .flex()
            .items_center()
            .gap(px(4.0))
            .bg(theme.panel_bg())
            .border_r_1()
            .border_color(theme.border)
            .px(px(6.0))
            .overflow_hidden()
            .child(div().flex_1().min_w_0().overflow_hidden().child(tabs))
            .child(header_icon_button(
                "workbench-hide-tools",
                icons::CLOSE,
                "Hide tools",
                &theme,
                cx.listener(|this, _, _, cx| this.set_surfaces_open(false, cx)),
            ));
        let explorer = div()
            .absolute()
            .left(px(ACTIVITY_WIDTH))
            .top_0()
            .w(px(files_width))
            .h(px(Theme::TITLEBAR_HEIGHT))
            .flex()
            .items_center()
            .px(px(12.0))
            .text_size(px(11.0))
            .text_color(theme.text_muted)
            .bg(theme.panel_bg())
            .border_r_1()
            .border_color(theme.border)
            .child("EXPLORER");
        let main = div()
            .absolute()
            .left(px(left))
            .right(px(sidebar.max(self.titlebar_right_pad(0.0))))
            .h(px(Theme::TITLEBAR_HEIGHT))
            .flex()
            .items_center()
            .px(px(14.0))
            .gap(px(8.0))
            .overflow_hidden()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(title),
            );
        self.titlebar_drag_region(
            "workbench-titlebar",
            div().relative().w_full().h(px(Theme::TITLEBAR_HEIGHT)),
            cx,
        )
        .when(files_width > 0.0, |el| el.child(explorer))
        .when(tools_width > 0.0, |el| el.child(tools))
        .child(main)
        .child(brand)
        .into_any_element()
    }

    /// Keep the explorer/tool selection while opening a file in the main editor.
    pub(super) fn open_workbench_file(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = self.panel_key(cx);
        let previous = self.panels.get(&key);
        self.add_file_surface(path.clone(), window, cx);
        if let Some(id) = self
            .file_surface_keys
            .get(&(key.clone(), self.active_chat.clone(), path))
            .copied()
        {
            self.workbench_files.insert(id);
            self.main_file = Some((self.active_chat.clone(), id));
            self.main_diff = None;
            self.main_diff_sub = None;
            self.panels.update(&key, |p| *p = previous);
            self.right_tween = None;
            self.focus_right_file_editor(RightSurface::File(id), window, cx);
            cx.notify();
        }
    }

    pub(super) fn render_workbench_file(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (chat, id) = self.main_file.clone()?;
        if self.state.read(cx).selected_chat.as_ref() != Some(&chat)
            || !self.file_surfaces.contains_key(&id)
        {
            self.main_file = None;
            return None;
        }
        let file = self.file_surfaces.get(&id)?.clone();
        file.update(cx, |file, cx| file.ensure_loaded(cx));
        let theme = Theme::of(cx).clone();
        let path = self.file_surface_paths.get(&id)?.clone();
        let dirty = file.read(cx).has_unsaved_changes();
        let header = div()
            .h(px(36.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(12.0))
            .text_color(theme.text)
            .child(
                icon(icons::FILE_CODE)
                    .size(px(15.0))
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .id("main-editor-tab")
                    .debug_selector(|| "main-editor-tab".into())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(path))
                    .when_some(
                        self.workspace_path_for_surface(RightSurface::File(id), cx),
                        |el, payload| {
                            el.on_drag(payload, |payload, _, _, cx| {
                                cx.stop_propagation();
                                crate::files::workspace_path_drag_ghost(payload, cx)
                            })
                        },
                    ),
            )
            .when(dirty, |el| el.child("●"))
            .child(header_icon_button(
                "close-main-editor",
                icons::CLOSE,
                "Close editor",
                &theme,
                cx.listener(move |this, _, window, cx| {
                    this.close_right_surface(RightSurface::File(id), window, cx)
                }),
            ));
        Some(
            div()
                .id("main-file-editor")
                .track_focus(&self.navigation_focus.main)
                .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                    this.capture_navigation_focus(false, false, window, cx);
                }))
                .flex_1()
                .min_w_0()
                .h_full()
                .pt(px(Theme::TITLEBAR_HEIGHT))
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(header)
                .child(div().flex_1().min_h_0().child(file))
                .into_any_element(),
        )
    }
}
