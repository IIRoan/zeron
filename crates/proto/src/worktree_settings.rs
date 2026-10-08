//! Device-private project defaults and checkout-specific environment settings.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_WORKTREE_COMMAND_TIMEOUT_SECONDS: u64 = 7200;
/// A worktree operation can execute several workflows in sequence. Keep the
/// host reply and UI progress budgets in sync with the per-command limit.
pub const WORKTREE_OPERATION_TIMEOUT_SECONDS: u64 =
    4 * MAX_WORKTREE_COMMAND_TIMEOUT_SECONDS + 120;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorktreeEnvMode {
    #[default]
    Copy,
    Link,
    Follow,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeEnvFile {
    pub path: String,
    #[serde(default)]
    pub mode: WorktreeEnvMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorktreeDependencyMode {
    #[default]
    Install,
    Link,
    Copy,
    Skip,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorktreeServiceMode {
    #[default]
    FollowActive,
    Parallel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorktreeSettings {
    pub branch_prefix: String,
    pub env_files: Vec<WorktreeEnvFile>,
    /// Follow-mode files share a private profile with this name.
    pub env_profile: String,
    pub dependencies: WorktreeDependencyMode,
    pub dependency_paths: Vec<String>,
    /// Empty means detect Bun, pnpm, Yarn, or npm from the checkout's lockfile.
    pub install_command: String,
    pub variables: BTreeMap<String, String>,
    /// Optional replacement for Git worktree creation, run in the project root.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub create_command: String,
    pub setup_command: String,
    pub activate_command: String,
    pub cleanup_command: String,
    /// Optional replacement for Git worktree removal, after cleanup, in the project root.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub remove_command: String,
    pub commands_in_project: bool,
    pub command_timeout_seconds: u64,
    pub service_mode: WorktreeServiceMode,
    /// None inherits project services; Some(empty) disables them for this checkout.
    pub services: Option<crate::ProjectTerminalConfig>,
}

impl Default for WorktreeSettings {
    fn default() -> Self {
        Self {
            branch_prefix: "zeron/".into(),
            env_files: Vec::new(),
            env_profile: "shared".into(),
            dependencies: WorktreeDependencyMode::Install,
            dependency_paths: vec!["node_modules".into()],
            install_command: String::new(),
            variables: BTreeMap::new(),
            create_command: String::new(),
            setup_command: String::new(),
            activate_command: String::new(),
            cleanup_command: String::new(),
            remove_command: String::new(),
            commands_in_project: false,
            command_timeout_seconds: 900,
            service_mode: WorktreeServiceMode::FollowActive,
            services: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorktreeSetupPhase {
    #[default]
    NotPrepared,
    Preparing,
    Ready,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSetupState {
    pub phase: WorktreeSetupPhase,
    pub step: String,
    pub error: Option<String>,
    pub log_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeEnvironmentEntry {
    pub path: String,
    pub branch: String,
    pub has_overrides: bool,
    pub managed: bool,
    pub state: WorktreeSetupState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSettingsSnapshot {
    pub defaults: WorktreeSettings,
    pub settings: WorktreeSettings,
    pub checkout: Option<String>,
    pub has_overrides: bool,
    pub detected_env_files: Vec<String>,
    pub detected_dependency_paths: Vec<String>,
    pub detected_install_command: String,
    pub detected_workflow: Option<String>,
    pub worktrees: Vec<WorktreeEnvironmentEntry>,
}
