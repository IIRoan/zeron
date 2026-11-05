//! A narrow service surface: the chat selects its project and checkout, never
//! caller-supplied paths, commands or profile edits.
use serde::Deserialize;
use serde_json::{Value, json};
use zeron_proto::{ProjectTerminalStatus, ProjectTerminalsSnapshot};
use zeron_rpc::{Caller, RpcError, methods, parse_params};

use super::{EngineRpc, ProjectTerminalsParams};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadParams {
    chat_id: String,
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
        let (chat_id, service_id, action) = if method == methods::CONTROL_CHAT_SERVICE {
            let p: ControlParams = parse_params(params)?;
            if p.service_id.trim().is_empty() {
                return Err(RpcError::BadParams("nonempty serviceId required".into()));
            }
            let action = match p.action {
                Action::Start => "start",
                Action::Restart => "restart",
                Action::Stop => "stop",
            };
            (p.chat_id, Some(p.service_id), Some(action.to_owned()))
        } else {
            let p: ReadParams = parse_params(params)?;
            (p.chat_id, None, None)
        };
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
            service_id,
            action,
            config: None,
            checkout_path: None,
            use_project_services: false,
        })
    }
}

pub(super) fn snapshot(space_id: &str, snapshot: ProjectTerminalsSnapshot) -> Value {
    let services: Vec<_> = snapshot
        .config
        .services
        .iter()
        .map(|service| {
            let run = snapshot.runs.iter().find(|r| r.service_id == service.id);
            json!({
                "id": service.id,
                "name": service.name,
                "kind": if service.is_database() { "database" } else { "service" },
                "scope": if service.is_database() { "project" } else { "checkout" },
                "status": run.map(|r| r.status).unwrap_or(ProjectTerminalStatus::Stopped),
                "readiness": run.and_then(|r| r.readiness),
                "exitCode": run.and_then(|r| r.exit_code),
                "restarts": run.map(|r| r.restarts).unwrap_or(0),
                "message": run.and_then(|r| r.message.as_deref()),
            })
        })
        .collect();
    json!({"projectId":space_id,"checkout":snapshot.checkout,"services":services})
}
