//! Named development services, configured privately on the project's device.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTerminalService {
    pub id: String,
    pub name: String,
    pub command: String,
    /// Relative to the active checkout, or the main folder for startup services.
    pub directory: String,
    #[serde(default)]
    pub restart_on_failure: bool,
    /// Present on shared databases: project infrastructure that runs in the
    /// main folder and must pass this check before setup or app services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_prerequisite: Option<ProjectServiceReadiness>,
}

impl ProjectTerminalService {
    pub fn is_database(&self) -> bool {
        self.setup_prerequisite.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectServiceReadiness {
    /// Retried in the main project folder until it exits successfully.
    pub command: String,
    #[serde(default = "default_readiness_timeout")]
    pub timeout_seconds: u64,
}

fn default_readiness_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTerminalConfig {
    pub services: Vec<ProjectTerminalService>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectTerminalStatus {
    Running,
    Restarting,
    Stopped,
    Exited,
    Failed,
    Interrupted,
}

/// Result of a database's readiness check for its current process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServiceReadinessState {
    Checking,
    Ready,
    NotReady,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTerminalRun {
    pub service_id: String,
    pub status: ProjectTerminalStatus,
    pub exit_code: Option<i32>,
    pub restarts: u32,
    pub message: Option<String>,
    pub terminal: Option<crate::TerminalSession>,
    /// Databases only; cleared whenever the process stops or restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<ServiceReadinessState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTerminalsSnapshot {
    pub config: ProjectTerminalConfig,
    pub runs: Vec<ProjectTerminalRun>,
    /// The one checkout currently owning this project's service processes.
    #[serde(default)]
    pub checkout: Option<String>,
}
