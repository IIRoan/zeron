//! Bundled native plugins for the GPUI app.
//!
//! The host owns navigation and persistence; plugins own their implementation
//! and declare the surfaces they contribute. Engine Git/worktree operations
//! remain available to agents when their desktop UI is disabled.

use std::collections::BTreeMap;

pub mod git;
pub mod services;
pub mod windows_vm;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginId {
    Git,
    Services,
    WindowsVm,
}

impl PluginId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Services => "project-services",
            Self::WindowsVm => "windows-vm",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contribution {
    SourceControl,
    GitHistory,
    GitCommands,
    ProjectServices,
    WindowsVm,
}

pub struct Manifest {
    pub id: PluginId,
    pub name: &'static str,
    pub description: &'static str,
    pub contributions: &'static [Contribution],
}

pub const BUNDLED: [&Manifest; 3] = [&git::MANIFEST, &services::MANIFEST, &windows_vm::MANIFEST];

/// Missing entries enable bundled plugins for existing installations. Unknown
/// IDs survive round trips so a temporarily unavailable plugin keeps its choice.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Preferences(pub BTreeMap<String, bool>);

impl Preferences {
    pub fn enabled(&self, id: PluginId) -> bool {
        self.0.get(id.as_str()).copied().unwrap_or(true)
    }

    pub fn set_enabled(&mut self, id: PluginId, enabled: bool) {
        self.0.insert(id.as_str().to_owned(), enabled);
    }

    pub fn contributes(&self, contribution: Contribution) -> bool {
        BUNDLED
            .iter()
            .any(|plugin| self.enabled(plugin.id) && plugin.contributions.contains(&contribution))
    }
}

pub fn enabled(id: PluginId, cx: &gpui::App) -> bool {
    crate::settings::plugin_enabled(id, cx)
}

pub fn contributes(contribution: Contribution, cx: &gpui::App) -> bool {
    BUNDLED.iter().any(|plugin| {
        enabled(plugin.id, cx) && plugin.contributions.contains(&contribution)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_settings_enable_both_bundled_plugins() {
        let settings: crate::settings::UiSettings = serde_json::from_str("{}").unwrap();
        for plugin in BUNDLED {
            assert!(settings.plugins.enabled(plugin.id));
        }
    }

    #[test]
    fn preferences_round_trip_and_contributions_are_independent() {
        let mut prefs: Preferences = serde_json::from_str(r#"{"future-plugin":false}"#).unwrap();
        prefs.set_enabled(PluginId::Git, false);
        let prefs: Preferences =
            serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
        assert!(!prefs.contributes(Contribution::SourceControl));
        assert!(!prefs.contributes(Contribution::GitHistory));
        assert!(!prefs.contributes(Contribution::GitCommands));
        assert!(prefs.contributes(Contribution::ProjectServices));
        assert_eq!(prefs.0.get("future-plugin"), Some(&false));
    }

    #[test]
    fn stale_geometry_save_does_not_reenable_a_plugin() {
        let base = crate::settings::UiSettings::default();
        let mut geometry = base.clone();
        geometry.sidebar_width = 300.0;
        let mut current = base.clone();
        current.plugins.set_enabled(PluginId::Services, false);
        let merged = crate::settings::UiSettings::merge_changes(&base, &geometry, current);
        assert!(!merged.plugins.enabled(PluginId::Services));
        assert_eq!(merged.sidebar_width, 300.0);
    }
}
