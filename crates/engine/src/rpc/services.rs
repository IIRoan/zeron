//! A narrow service and worktree-settings surface: the chat selects its
//! project and checkout, never caller-supplied project or checkout paths.
//! Service edits merge one entry at a time; worktree edits only named fields.
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeReadParams {
    chat_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeEditParams {
    chat_id: String,
    scope: WorktreeScope,
    #[serde(default)]
    changes: serde_json::Map<String, Value>,
    #[serde(default)]
    reset: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum WorktreeScope {
    Project,
    Checkout,
}

/// The worktree settings page's fields an agent may change. Services and
/// databases are the service tools' (`EditChatService`).
const EDITABLE_WORKTREE_FIELDS: &[&str] = &[
    "branchPrefix",
    "envFiles",
    "envProfile",
    "dependencies",
    "dependencyPaths",
    "installCommand",
    "variables",
    "createCommand",
    "setupCommand",
    "activateCommand",
    "cleanupCommand",
    "removeCommand",
    "commandsInProject",
    "commandTimeoutSeconds",
    "serviceMode",
];

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
                "Project tools can only act on the calling chat's project".into(),
            ));
        }
        let chat = self
            .workspace
            .chat(&chat_id)
            .map_err(|e| RpcError::Failed(e.to_string()))?
            .ok_or_else(|| RpcError::Failed("Project chat not found".into()))?;
        if chat.device_id != self.doc_host.device_id() {
            return Err(RpcError::Failed(
                "Project tools require a chat on this device".into(),
            ));
        }
        let space_id = chat.space_id.ok_or_else(|| {
            RpcError::Failed(
                "This chat has no project; choose a project to use project tools".into(),
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
        self.require_full_access(&p.chat_id, "Editing service commands")?;
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

    /// Authoring a shell command must not bypass a chat's approval or
    /// read-only/plan settings through Zeron's unsandboxed runners.
    fn require_full_access(&self, chat_id: &str, what: &str) -> Result<(), RpcError> {
        if !matches!(zeron_rpc::caller(), Caller::Agent { .. }) {
            return Ok(());
        }
        let chat = self
            .workspace
            .chat(chat_id)
            .map_err(|e| RpcError::Failed(e.to_string()))?
            .ok_or_else(|| RpcError::Failed("Project chat not found".into()))?;
        if chat.config.as_ref().is_some_and(|c| {
            c.access != AccessMode::FullAccess || c.sandbox == SandboxLevel::ReadOnly || c.plan
        }) {
            return Err(RpcError::Failed(format!(
                "{what} requires a chat with full access outside read-only or plan mode."
            )));
        }
        Ok(())
    }

    /// The calling chat's worktree settings: the project defaults and, from
    /// a worktree, what that checkout uses. An edit merges only the fields
    /// it names into the latest saved settings of its scope.
    pub(super) async fn chat_worktree_settings(
        &self,
        method: &str,
        params: Value,
    ) -> Result<RpcReply, RpcError> {
        let (chat_id, edit) = if method == methods::EDIT_CHAT_WORKTREE_SETTINGS {
            let p: WorktreeEditParams = parse_params(params)?;
            (p.chat_id.clone(), Some(p))
        } else {
            let p: WorktreeReadParams = parse_params(params)?;
            (p.chat_id, None)
        };
        let target = self.chat_service_target(chat_id.clone())?;
        if let Some(edit) = &edit {
            // Exactly one of: changes, or reset.
            if edit.reset != edit.changes.is_empty() {
                return Err(RpcError::BadParams(
                    "Pass the changes to make, or reset alone".into(),
                ));
            }
            if let Some(key) = edit
                .changes
                .keys()
                .find(|key| !EDITABLE_WORKTREE_FIELDS.contains(&key.as_str()))
            {
                return Err(RpcError::BadParams(if key == "services" {
                    "Services and databases are edited with the service tools".into()
                } else {
                    format!("Unknown worktree setting: {key}")
                }));
            }
            // Workflow commands run unsandboxed when Zeron creates, prepares
            // or removes a worktree.
            self.require_full_access(&chat_id, "Editing worktree settings")?;
        }
        let space = self.local_project_action_space(&target.space_id)?;
        let root = std::path::PathBuf::from(&space.path);
        let resolved = self
            .workspace_files
            .resolve_target(&zeron_proto::WorkspaceTarget {
                space_id: None,
                chat_id: Some(chat_id),
                checkout_path: None,
            })
            .await
            .map_err(|e| RpcError::Failed(e.to_string()))?
            .root;
        let main = root.canonicalize().ok() == resolved.canonicalize().ok();
        let checkout = (!main).then_some(resolved);
        let profiles = self.project_actions.project_terminals.clone();
        let environments = self.repos.worktree_settings().clone();
        super::adopt_legacy_services(&profiles, &environments, &space.id, &root).await?;
        let saved = edit.is_some();
        if let Some(WorktreeEditParams {
            scope,
            changes,
            reset,
            ..
        }) = edit
        {
            let scoped = match scope {
                WorktreeScope::Project if reset => {
                    return Err(RpcError::Failed(
                        "Project defaults can't be reset; pass the changes to make".into(),
                    ));
                }
                WorktreeScope::Project => None,
                WorktreeScope::Checkout if changes.contains_key("serviceMode") => {
                    return Err(RpcError::Failed(
                        "serviceMode applies to the whole project: use scope project".into(),
                    ));
                }
                WorktreeScope::Checkout => Some(checkout.clone().ok_or_else(|| {
                    RpcError::Failed(
                        "This chat is in the project's main folder, which uses the project defaults: use scope project".into(),
                    )
                })?),
            };
            profiles
                .edit_environment(
                    &environments,
                    &space.id,
                    &root,
                    scoped.as_deref(),
                    move |current| {
                        if reset {
                            return Ok(None);
                        }
                        let mut value = serde_json::to_value(current)
                            .map_err(|e| crate::EngineError::Other(e.to_string()))?;
                        for (key, change) in changes {
                            value[key.as_str()] = change;
                        }
                        serde_json::from_value(value).map(Some).map_err(|e| {
                            crate::EngineError::Other(format!("Invalid worktree settings: {e}"))
                        })
                    },
                )
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?;
        }
        let snapshot = self
            .repos
            .worktree_settings_snapshot(&root, checkout.as_deref())
            .await
            .map_err(|e| RpcError::Failed(e.to_string()))?;
        let mut result =
            serde_json::to_value(snapshot).map_err(|e| RpcError::Failed(e.to_string()))?;
        // Services and databases are the service tools' to show.
        if let Some(map) = result.as_object_mut() {
            map.remove("databases");
            map.remove("appServices");
            for page in ["defaults", "settings"] {
                if let Some(settings) = map.get_mut(page).and_then(Value::as_object_mut) {
                    settings.remove("services");
                }
            }
        }
        result["projectId"] = json!(space.id);
        result["mainFolder"] = json!(main);
        result["worktreeFolder"] =
            json!(self.repos.worktree_location_settings().effective_directory);
        if saved {
            result["saved"] = json!(true);
            result["note"] = json!(
                "Saved. Nothing ran; Zeron uses these settings the next time it creates, prepares, activates or removes a worktree."
            );
        }
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
