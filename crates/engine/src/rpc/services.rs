//! A narrow service surface: the chat selects its project and checkout, never
//! caller-supplied project or checkout paths. Edits merge one entry at a time.
use serde::Deserialize;
use serde_json::{Value, json};
use zeron_proto::{
    AccessMode, ProjectServiceEdit, ProjectTerminalStatus, ProjectTerminalsSnapshot, SandboxLevel,
};
use zeron_rpc::{Caller, RpcError, RpcReply, methods, parse_params};

use super::{EngineRpc, ProjectTerminalsParams};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadParams {
    chat_id: String,
    #[serde(default)]
    include_config: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditParams {
    chat_id: String,
    edit: ProjectServiceEdit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ControlParams {
    chat_id: String,
    service_id: String,
    action: Action,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Action {
    Start,
    Restart,
    Stop,
}

impl EngineRpc {
    pub(super) fn chat_services_params(
        &self,
        method: &str,
        params: Value,
    ) -> Result<ProjectTerminalsParams, RpcError> {
        let (chat_id, service_id, action, include_config) =
            if method == methods::CONTROL_CHAT_SERVICE {
                let p: ControlParams = parse_params(params)?;
                if p.service_id.trim().is_empty() {
                    return Err(RpcError::BadParams("nonempty serviceId required".into()));
                }
                let action = match p.action {
                    Action::Start => "start",
                    Action::Restart => "restart",
                    Action::Stop => "stop",
                };
                (
                    p.chat_id,
                    Some(p.service_id),
                    Some(action.to_owned()),
                    false,
                )
            } else {
                let p: ReadParams = parse_params(params)?;
                (p.chat_id, None, None, p.include_config)
            };
        let mut target = self.chat_service_target(chat_id)?;
        target.service_id = service_id;
        target.action = action;
        target.include_service_config = include_config;
        Ok(target)
    }

    fn chat_service_target(&self, chat_id: String) -> Result<ProjectTerminalsParams, RpcError> {
        if let Caller::Agent { chat } = zeron_rpc::caller()
            && chat.as_deref() != Some(&chat_id)
        {
            return Err(RpcError::Failed(
                "Service tools can only control the calling chat's project".into(),
            ));
        }
        let chat = self
            .workspace
            .chat(&chat_id)
            .map_err(|e| RpcError::Failed(e.to_string()))?
            .ok_or_else(|| RpcError::Failed("Project chat not found".into()))?;
        if chat.device_id != self.doc_host.device_id() {
            return Err(RpcError::Failed(
                "Service tools require a chat on this device".into(),
            ));
        }
        let space_id = chat.space_id.ok_or_else(|| {
            RpcError::Failed(
                "This chat has no project; choose a project to use service tools".into(),
            )
        })?;
        Ok(ProjectTerminalsParams {
            space_id,
            chat_id: Some(chat_id),
            include_service_config: false,
            service_id: None,
            action: None,
            config: None,
            checkout_path: None,
            use_project_services: false,
        })
    }

    pub(super) async fn edit_chat_service(&self, params: Value) -> Result<RpcReply, RpcError> {
        let p: EditParams = parse_params(params)?;
        let target = self.chat_service_target(p.chat_id.clone())?;
        // Authoring a shell command must not bypass a chat's approval or
        // read-only/plan settings through the unsandboxed service manager.
        if matches!(zeron_rpc::caller(), Caller::Agent { .. }) {
            let chat = self
                .workspace
                .chat(&p.chat_id)
                .map_err(|e| RpcError::Failed(e.to_string()))?
                .ok_or_else(|| RpcError::Failed("Project chat not found".into()))?;
            if chat.config.as_ref().is_some_and(|c| {
                c.access != AccessMode::FullAccess || c.sandbox == SandboxLevel::ReadOnly || c.plan
            }) {
                return Err(RpcError::Failed("Editing service commands requires a chat with full access outside read-only or plan mode.".into()));
            }
        }
        let space = self.local_project_action_space(&target.space_id)?;
        let root = std::path::PathBuf::from(&space.path);
        let checkout = self
            .workspace_files
            .resolve_target(&zeron_proto::WorkspaceTarget {
                space_id: None,
                chat_id: Some(p.chat_id),
                checkout_path: None,
            })
            .await
            .map_err(|e| RpcError::Failed(e.to_string()))?
            .root;
        let profiles = self.project_actions.project_terminals.clone();
        let environments = self.repos.worktree_settings().clone();
        super::adopt_legacy_services(&profiles, &environments, &space.id, &root).await?;
        let (saved, id) = profiles
            .edit_checkout_service(&environments, &space.id, &root, &checkout, p.edit)
            .await
            .map_err(|e| RpcError::Failed(e.to_string()))?;
        let mut result = snapshot(&space.id, saved, true);
        let service = result["services"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"].as_str() == Some(&id))
            .unwrap()
            .clone();
        result.as_object_mut().unwrap().remove("services");
        result["service"] = service;
        result["saved"] = json!(true);
        result["note"] = json!(
            "Configuration saved. Running processes keep their current configuration; use service_restart to apply changes to them."
        );
        Ok(RpcReply::Value(result))
    }
}

pub(super) fn snapshot(
    space_id: &str,
    snapshot: ProjectTerminalsSnapshot,
    include_config: bool,
) -> Value {
    let services: Vec<_> = snapshot
        .config
        .services
        .iter()
        .map(|service| {
            let run = snapshot.runs.iter().find(|r| r.service_id == service.id);
            let mut value = json!({
                "id": service.id,
                "name": service.name,
                "kind": if service.is_database() { "database" } else { "service" },
                "scope": if service.is_database() { "project" } else { "checkout" },
                "status": run.map(|r| r.status).unwrap_or(ProjectTerminalStatus::Stopped),
                "readiness": run.and_then(|r| r.readiness),
                "exitCode": run.and_then(|r| r.exit_code),
                "restarts": run.map(|r| r.restarts).unwrap_or(0),
                "message": run.and_then(|r| r.message.as_deref()),
            });
            if include_config {
                let mut config = json!({"name":service.name,"command":service.command,"directory":service.directory,"restartOnFailure":service.restart_on_failure});
                if let Some(readiness) = &service.setup_prerequisite {
                    config["readinessCommand"] = json!(readiness.command);
                    config["readinessTimeoutSeconds"] = json!(readiness.timeout_seconds);
                }
                value["configuration"] = config;
            }
            value
        })
        .collect();
    json!({"projectId":space_id,"checkout":snapshot.checkout,"services":services})
}
