//! One shell per surface tab. The terminal panel keeps its PTYs and per-chat
//! selection; Shell owns the single visible tab strip.
use super::*;

#[derive(Clone, PartialEq)]
pub(super) struct TabsState {
    chat: String,
    tabs: Vec<(u64, SharedString, bool)>,
    active: Option<u64>,
}

impl Shell {
    pub(super) fn open_terminal_sidebar(&mut self, cx: &mut Context<Self>) {
        self.close_files_panel(cx);
        self.set_surfaces_open(true, cx);
        let panel = self.terminal_panel(cx);
        self.sync_terminal_sidebar_tabs(&panel, cx);
        let active = panel.read(cx).active_tab_key(cx);
        if let Some(tab) = active {
            self.set_right_active(RightSurface::Terminal(tab), cx);
        } else {
            self.add_terminal_surface(cx);
        }
    }

    /// Setup and project actions may attach to a chat which is not selected.
    /// Register that captured chat's tab without creating an ordinary shell.
    pub(super) fn show_shell_tab_for_chat(&mut self, chat: &str, tab: u64, cx: &mut Context<Self>) {
        let surface = RightSurface::Terminal(tab);
        push_unique_right_surface(self.right_tabs.entry(chat.to_owned()).or_default(), surface);
        if self.panel_key(cx) == chat {
            self.close_files_panel(cx);
            self.set_surfaces_open(true, cx);
            self.set_right_active(surface, cx);
        } else {
            self.panels.update(chat, |panel| {
                panel.changes_open = true;
                panel.right_active = surface;
            });
        }
    }

    /// Observe only changes to tabs, titles, exit status and selection. Output
    /// frames stay inside TerminalPanel instead of rebuilding the whole shell.
    pub(super) fn sync_terminal_sidebar_tabs(
        &mut self,
        panel: &Entity<TerminalPanel>,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = TabsState {
            chat: self.state.read(cx).panel_session_key(),
            tabs: panel.read(cx).tab_summaries(cx),
            active: panel.read(cx).active_tab_key(cx),
        };
        if self.terminal_sidebar_state.as_ref() == Some(&state) {
            return false;
        }
        self.terminal_sidebar_state = Some(state.clone());
        let tabs = self.right_tabs.entry(state.chat.clone()).or_default();
        let mut removed = Vec::new();
        tabs.retain(|surface| {
            let keep = match surface {
                RightSurface::Terminal(key) => state.tabs.iter().any(|(id, ..)| id == key),
                _ => true,
            };
            if !keep {
                removed.push(*surface);
            }
            keep
        });
        for (key, ..) in &state.tabs {
            push_unique_right_surface(tabs, RightSurface::Terminal(*key));
        }
        let removed_any = !removed.is_empty();
        for surface in removed {
            self.panels.forget_right_surface(&state.chat, surface);
        }
        if matches!(
            self.panels.get(&state.chat).right_active,
            RightSurface::Terminal(_)
        ) {
            let next = state.active.map(RightSurface::Terminal).unwrap_or_else(|| {
                self.panels.right_surface_fallback(
                    &state.chat,
                    self.right_tabs
                        .get(&state.chat)
                        .into_iter()
                        .flatten()
                        .copied()
                        .filter(|surface| self.is_tool_surface(*surface)),
                )
            });
            if self.panels.get(&state.chat).right_active != next {
                if self.panel_key(cx) == state.chat && self.right_pane_open(cx) {
                    self.set_right_active(next, cx);
                } else {
                    self.panels
                        .update(&state.chat, |panel| panel.right_active = next);
                }
            }
        }
        if state.tabs.is_empty() && removed_any {
            self.close_empty_right_pane(&state.chat, cx);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    fn setup(cx: &mut TestAppContext, directory: &std::path::Path) -> gpui::WindowHandle<Shell> {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            let mut shell = Shell::new(
                state,
                EngineBootConfig {
                    data_dir: directory.into(),
                    ipc_port: 0,
                    edge_url: String::new(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: zeron_proto::HarnessId::Mock,
                },
                cx,
            );
            shell.active_chat = "alpha".into();
            shell.viewport_width = 1440.0;
            shell.state.update(cx, |state, _| {
                state.local_device_id = Some("local".into());
                state.selected_chat = Some("alpha".into());
                state.selected_space = Some("project".into());
                state.spaces =
                    vec![serde_json::from_value(serde_json::json!({
                    "id":"project", "deviceId":"local", "path":directory, "createdAt":Utc::now()
                })).unwrap()];
                state.chats = ["alpha", "beta"]
                    .into_iter()
                    .map(|id| {
                        serde_json::from_value(serde_json::json!({
                            "id":id, "deviceId":"local", "spaceId":"project", "cwd":directory,
                            "createdAt":Utc::now(), "archived":false
                        }))
                        .unwrap()
                    })
                    .collect();
            });
            shell
        })
    }

    #[gpui::test]
    fn individual_terminal_surfaces_hide_without_closing_and_close_only_the_target(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let window = setup(cx, directory.path());
        window
            .update(cx, |shell, window, cx| {
                let panel = shell.terminal_panel(cx);
                let (first, second) = panel.update(cx, |panel, cx| {
                    let first = panel.reserve_tab_for_chat("alpha".into(), "Terminal 1", cx);
                    let second = panel.reserve_tab_for_chat("alpha".into(), "Terminal 2", cx);
                    (first, second)
                });
                shell.open_shell_panel(cx);
                let rows = shell.right_surface_rows(cx);
                assert_eq!(
                    rows.iter()
                        .map(|(id, title, ..)| (*id, title.as_ref()))
                        .collect::<Vec<_>>(),
                    vec![
                        (RightSurface::Terminal(first), "Terminal 1"),
                        (RightSurface::Terminal(second), "Terminal 2")
                    ]
                );
                shell.set_right_active(RightSurface::Terminal(first), cx);
                shell.hide_terminal_panel(window, cx);
                assert!(!panel.read(cx).is_open());
                assert_eq!(panel.read(cx).tab_summaries(cx).len(), 2);
                assert_eq!(shell.right_surface_rows(cx).len(), 2);
                shell.open_shell_panel(cx);
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Terminal(first)
                );
                shell.close_right_surface(RightSurface::Terminal(second), window, cx);
                assert_eq!(
                    panel
                        .read(cx)
                        .tab_summaries(cx)
                        .iter()
                        .map(|(id, ..)| *id)
                        .collect::<Vec<_>>(),
                    vec![first]
                );
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Terminal(first)
                );
                shell.close_right_surface(RightSurface::Terminal(first), window, cx);
                assert!(!shell.right_pane_open(cx));
                assert!(!panel.read(cx).is_open());
            })
            .unwrap();
        cx.run_until_parked();
    }

    #[gpui::test]
    fn new_tabs_sync_while_hidden_and_background_action_tabs_keep_their_chat(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let window = setup(cx, directory.path());
        let panel = window
            .update(cx, |shell, window, cx| {
                let panel = shell.terminal_panel(cx);
                panel.update(cx, |panel, cx| {
                    panel.reserve_tab_for_chat("alpha".into(), "Terminal 1", cx)
                });
                shell.open_shell_panel(cx);
                shell.hide_terminal_panel(window, cx);
                panel
            })
            .unwrap();
        cx.run_until_parked();
        let second = panel.update(cx, |panel, cx| {
            panel.reserve_tab_for_chat("alpha".into(), "Terminal 2", cx)
        });
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert!(!shell.right_pane_open(cx));
                assert!(!panel.read(cx).is_open());
                assert_eq!(shell.right_surface_rows(cx).len(), 2);
                shell.open_shell_panel(cx);
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Terminal(second)
                );
                let background = panel.update(cx, |panel, cx| {
                    panel.reserve_tab_for_chat("beta".into(), "Setup", cx)
                });
                shell.show_shell_tab_for_chat("beta", background, cx);
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Terminal(second)
                );
                assert_eq!(
                    shell.panels.get("beta").right_active,
                    RightSurface::Terminal(background)
                );
                assert_eq!(
                    shell.right_tabs["beta"],
                    vec![RightSurface::Terminal(background)]
                );
                assert_eq!(
                    panel.read(cx).tab_summaries(cx).len(),
                    2,
                    "a background setup creates no extra shell in this chat"
                );
            })
            .unwrap();
        cx.run_until_parked();
    }
}
