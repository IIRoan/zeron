//! Native host adapter for bundled plugin contributions and lifecycle.
use super::*;
use crate::plugins::{self, Contribution, PluginId};

impl Shell {
    pub(super) fn git_plugin_enabled(&self, cx: &App) -> bool {
        plugins::enabled(PluginId::Git, cx)
    }

    pub(super) fn services_plugin_enabled(&self, cx: &App) -> bool {
        plugins::contributes(Contribution::ProjectServices, cx)
    }

    pub(super) fn git_tools_available(&self, cx: &App) -> bool {
        plugins::contributes(Contribution::SourceControl, cx) && self.space_git_detected(cx)
    }

    /// SettingsStore is shared by every window. Hide contributions and pause
    /// their UI work without cancelling accepted engine mutations or processes.
    pub(super) fn sync_plugins(&mut self, cx: &mut Context<Self>) {
        let preferences = settings::plugin_preferences(cx);
        if self.plugins_snapshot == preferences {
            return;
        }
        self.plugins_snapshot = preferences;
        self.composer.update(cx, |composer, cx| composer.refresh_plugin_commands(cx));
        if !self.git_plugin_enabled(cx) {
            self.suspend_git_info();
            for changes in self.diffs.values() {
                changes.update(cx, |changes, cx| changes.suspend_git_plugin(cx));
            }
            if let Some((_, _, changes)) = &self.main_diff {
                changes.update(cx, |changes, cx| changes.suspend_git_plugin(cx));
            }
        }
        if !self.services_plugin_enabled(cx) {
            self.suspend_services_plugin(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[gpui::test]
    fn store_changes_hide_cached_git_tabs_in_every_window_and_reenable_them(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(Default::default(), dir.path(), cx);
        });
        let windows: Vec<_> = (0..2).map(|_| cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(state, EngineBootConfig {
                data_dir: dir.path().into(), ipc_port: 0, edge_url: String::new(),
                edge_token: None, org_id: None, workos_client_id: None,
                default_harness: zeron_proto::HarnessId::Mock,
            }, cx)
        })).collect();
        for window in &windows {
            window.update(cx, |shell, window, cx| {
                shell.add_diff_surface(window, cx);
                shell.add_history_surface(window, cx);
                assert_eq!(shell.right_surface_rows(cx).len(), 2);
            }).unwrap();
        }
        cx.update(|cx| { settings::update(SavePolicy::Immediate, cx, |settings| {
            settings.plugins.set_enabled(PluginId::Git, false);
        }); });
        cx.run_until_parked();
        for window in &windows {
            window.update(cx, |shell, window, cx| {
                assert!(!shell.plugins_snapshot.enabled(PluginId::Git), "store observer reached this window");
                assert!(shell.right_surface_rows(cx).is_empty());
                assert_eq!(shell.resolved_right_active(cx), RightSurface::Picker);
                shell.add_diff_surface(window, cx);
                shell.add_history_surface(window, cx);
                assert_eq!(shell.diffs.len(), 2, "no new Git surfaces while disabled");
                assert!(shell.services_plugin_enabled(cx));
            }).unwrap();
        }
        cx.update(|cx| { settings::update(SavePolicy::Immediate, cx, |settings| {
            settings.plugins.set_enabled(PluginId::Git, true);
        }); });
        cx.run_until_parked();
        for window in &windows {
            window.update(cx, |shell, _, cx| {
                assert_eq!(shell.right_surface_rows(cx).len(), 2, "cached views are restored");
            }).unwrap();
        }
    }
}

#[cfg(feature = "source-control-fixture")]
impl Shell {
    pub fn fixture_plugins_settings(&mut self, cx: &mut Context<Self>) {
        self.open_settings(SettingsSection::Plugins, cx);
    }

    pub fn fixture_plugin_chat(&mut self, cx: &mut Context<Self>) {
        self.set_route(Route::Chat, cx);
        cx.notify();
    }
}
