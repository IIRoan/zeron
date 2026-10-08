//! Named development services, configured privately on the project's device.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTerminalService {
    pub id: String,
    pub name: String,
    pub command: String,
    /// Relative to the active session's checkout, including the main checkout.
    pub directory: String,
    #[serde(default)]
    pub restart_on_failure: bool,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTerminalRun {
    pub service_id: String,
    pub status: ProjectTerminalStatus,
    pub exit_code: Option<i32>,
    pub restarts: u32,
    pub message: Option<String>,
    pub terminal: Option<crate::TerminalSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTerminalsSnapshot {
    pub config: ProjectTerminalConfig,
    pub runs: Vec<ProjectTerminalRun>,
    /// The one checkout currently owning this project's service processes.
    #[serde(default)]
    pub checkout: Option<String>,
}
