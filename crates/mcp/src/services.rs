//! Configured project services, using the same manager as the desktop.
use anyhow::{Context, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use zeron_rpc::methods;

use crate::{ToolDef, Zeron};

pub const NAMES: &[&str] = &[
    "service_list",
    "service_status",
    "service_start",
    "service_restart",
    "service_stop",
    "service_add",
    "service_update",
];

pub fn catalog() -> Vec<ToolDef> {
    let mut tools: Vec<_> = [
        ("service_list", "List configured services and databases for this chat's project and checkout, with status and readiness. Set includeConfig:true to inspect commands, directories and readiness checks before editing. Use service_add to create entries and service_update with a listed id to edit them. Databases are shared across project checkouts."),
        ("service_status", "Get one configured service or database's current process status, readiness, exit code and restart count. Use serviceId from service_list. Status reads do not start anything."),
        ("service_start", "Start a configured service or database using Zeron's service manager. Already running processes are reused. Setup and database readiness checks run as in the desktop. Use service_status to check the result. Databases are shared across this project's checkouts."),
        ("service_restart", "Restart a configured service or database using Zeron's service manager. Use serviceId from service_list. Restarting a shared database affects all project checkouts; app services follow the project's checkout mode."),
        ("service_stop", "Stop a configured service or database using Zeron's service manager. Use serviceId from service_list. Stopping a shared database affects all project checkouts."),
    ]
    .into_iter()
    .map(|(name, description)| ToolDef {
        name,
        description,
        input_schema: if name == "service_list" {
            json!({"type":"object","properties":{"includeConfig":{"type":"boolean","default":false}},"additionalProperties":false})
        } else {
            json!({"type":"object","properties":{"serviceId":{"type":"string","minLength":1,"description":"Exact id from service_list."}},"required":["serviceId"],"additionalProperties":false})
        },
    })
    .collect();
    let fields = json!({
        "name":{"type":"string","minLength":1,"maxLength":80},
        "command":{"type":"string","minLength":1,"maxLength":16384},
        "directory":{"type":"string","description":"Relative folder inside the checkout; defaults to . (project root for databases)."},
        "restartOnFailure":{"type":"boolean"},
        "readinessCommand":{"type":"string","minLength":1,"description":"Required for a database: a command that exits successfully when ready, run in the main project folder."},
        "readinessTimeoutSeconds":{"type":"integer","minimum":1,"maximum":7200,"description":"Database readiness timeout; defaults to 120 seconds."}
    });
    let mut add = fields.clone();
    add["kind"] = json!({"type":"string","enum":["service","database"],"default":"service"});
    tools.push(ToolDef {
        name:"service_add",
        description:"Add a service or shared database to this chat's list. Requires name and command; databases also require readinessCommand. App services are saved for this checkout (project defaults when in the main folder); databases are shared by the project. Returns its generated id. Saving does not start anything; use service_start. Requires full access outside read-only/plan mode.",
        input_schema:json!({"type":"object","properties":add,"required":["name","command"],"additionalProperties":false}),
    });
    let mut update = fields;
    update["serviceId"] =
        json!({"type":"string","minLength":1,"description":"Exact id from service_list."});
    tools.push(ToolDef {
        name:"service_update",
        description:"Edit one listed service or database. Pass serviceId plus only the fields to change; omitted fields and other entries/settings are preserved. Inspect service_list with includeConfig:true first. App edits in a worktree create a checkout override; main-folder edits change project defaults. Databases remain shared and require a readiness check. Saving keeps running processes unchanged; use service_restart to apply it. Requires full access outside read-only/plan mode.",
        input_schema:json!({"type":"object","properties":update,"required":["serviceId"],"minProperties":2,"additionalProperties":false}),
    });
    tools
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    include_config: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServiceArgs {
    service_id: String,
}

pub async fn call(zeron: &Zeron, name: &str, args: Value) -> anyhow::Result<Value> {
    let chat_id = zeron
        .origin()
        .chat_id
        .as_ref()
        .context("service tools require a Zeron chat")?;
    let editing = matches!(name, "service_add" | "service_update");
    let include_config = if name == "service_list" {
        serde_json::from_value::<ListArgs>(args.clone())?.include_config
    } else {
        false
    };
    let service_id = if name != "service_list" && !editing {
        let args: ServiceArgs = serde_json::from_value(args.clone())?;
        ensure!(
            !args.service_id.trim().is_empty(),
            "nonempty serviceId required"
        );
        Some(args.service_id)
    } else {
        None
    };
    let edit = if editing {
        Some(edit_request(name, args)?)
    } else {
        None
    };
    let info: zeron_proto::EngineInfo =
        serde_json::from_value(zeron.call(methods::ENGINE_INFO, json!({})).await?)?;
    ensure!(
        info.supports(zeron_proto::capabilities::PROJECT_SERVICE_CONTROL_V1),
        "Update the Zeron daemon to enable service tools"
    );
    if editing || include_config {
        ensure!(
            info.supports(zeron_proto::capabilities::PROJECT_SERVICE_EDIT_V1),
            "Restart or update the Zeron daemon to enable service configuration tools"
        );
    }
    let mut request = json!({"chatId":chat_id});
    if let Some(edit) = edit {
        request["edit"] = edit;
        return zeron.call_once(methods::EDIT_CHAT_SERVICE, request).await;
    }
    if include_config {
        request["includeConfig"] = json!(true);
    }
    let mut snapshot = match name {
        "service_list" | "service_status" => {
            zeron.call(methods::GET_CHAT_SERVICES, request).await?
        }
        "service_start" | "service_restart" | "service_stop" => {
            request["serviceId"] = json!(service_id);
            request["action"] = json!(name.strip_prefix("service_").unwrap());
            // A disconnected reply must never cause a second restart.
            zeron
                .call_once(methods::CONTROL_CHAT_SERVICE, request)
                .await?
        }
        _ => anyhow::bail!("unknown service tool"),
    };
    if let Some(id) = service_id {
        let service = snapshot["services"]
            .as_array()
            .and_then(|services| services.iter().find(|s| s["id"].as_str() == Some(&id)))
            .cloned()
            .with_context(|| format!("Unknown service id: {id}; use service_list"))?;
        snapshot
            .as_object_mut()
            .context("invalid service snapshot")?
            .remove("services");
        snapshot["service"] = service;
    }
    Ok(snapshot)
}

fn edit_request(name: &str, args: Value) -> anyhow::Result<Value> {
    let mut fields = args
        .as_object()
        .context("service arguments must be an object")?
        .clone();
    let value = if name == "service_add" {
        ensure!(
            !fields.contains_key("op"),
            "operation is selected by the tool name"
        );
        fields.insert("op".into(), json!("add"));
        Value::Object(fields)
    } else {
        let id = fields.remove("serviceId").context("serviceId required")?;
        ensure!(
            id.as_str().is_some_and(|id| !id.trim().is_empty()),
            "nonempty serviceId required"
        );
        ensure!(
            !fields.is_empty() && fields.values().all(|value| !value.is_null()),
            "Provide at least one non-null field to update"
        );
        json!({"op":"update","serviceId":id,"changes":fields})
    };
    let edit: zeron_proto::ProjectServiceEdit = serde_json::from_value(value)?;
    Ok(serde_json::to_value(edit)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Origin, Tools};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use zeron_rpc::{RpcError, RpcReply, RpcService};

    #[derive(Default)]
    struct Service {
        controls: AtomicUsize,
        legacy: bool,
        control_only: bool,
    }

    #[async_trait::async_trait]
    impl RpcService for Service {
        async fn handle(&self, method: &str, _: Value) -> Result<RpcReply, RpcError> {
            match method {
                methods::ENGINE_INFO => Ok(RpcReply::Value(
                    json!({"deviceId":"local","workspaceScope":"local","capabilities":if self.legacy { vec![] } else { zeron_proto::capabilities::current().into_iter().filter(|cap| !self.control_only || cap != zeron_proto::capabilities::PROJECT_SERVICE_EDIT_V1).collect() }}),
                )),
                methods::GET_CHAT_SERVICES => Ok(RpcReply::Value(
                    json!({"projectId":"project","checkout":"/project","services":[{"id":"api","status":"stopped"}]}),
                )),
                methods::CONTROL_CHAT_SERVICE => {
                    self.controls.fetch_add(1, Ordering::SeqCst);
                    Err(RpcError::Closed)
                }
                methods::EDIT_CHAT_SERVICE => {
                    self.controls.fetch_add(1, Ordering::SeqCst);
                    Err(RpcError::Closed)
                }
                _ => panic!("unexpected {method}"),
            }
        }
    }

    fn tools(service: Arc<Service>, chat: Option<&str>) -> Tools {
        Tools::new(Arc::new(Zeron::with_client(
            zeron_rpc::memory_client(service),
            Origin {
                chat_id: chat.map(str::to_owned),
                ..Default::default()
            },
        )))
    }

    #[tokio::test]
    async fn service_tools_are_chat_scoped_and_validate_arguments() {
        let service = Arc::new(Service::default());
        let standalone = tools(service.clone(), None);
        for name in NAMES {
            assert!(!standalone.has(name));
        }
        assert!(
            standalone
                .call("service_list", json!({}))
                .await
                .unwrap_err()
                .contains("require a Zeron chat")
        );
        let tools = tools(service.clone(), Some("agent"));
        for name in NAMES {
            assert!(tools.has(name));
        }
        for (name, args) in [
            ("service_list", json!({"chatId":"other"})),
            ("service_start", json!({})),
            ("service_start", json!({"serviceId":" "})),
            (
                "service_restart",
                json!({"serviceId":"api","action":"stop"}),
            ),
            (
                "service_stop",
                json!({"serviceId":"api","checkoutPath":"/other"}),
            ),
        ] {
            assert!(tools.call(name, args).await.is_err(), "{name}");
        }
        assert_eq!(service.controls.load(Ordering::SeqCst), 0);
        let status = tools
            .call("service_status", json!({"serviceId":"api"}))
            .await
            .unwrap();
        assert_eq!(status["service"]["status"], "stopped");
        assert!(status.get("services").is_none());
        assert!(
            tools
                .call("service_status", json!({"serviceId":"missing"}))
                .await
                .unwrap_err()
                .contains("Unknown service")
        );
    }

    #[tokio::test]
    async fn disconnected_mutations_are_not_replayed_and_old_daemons_are_explained() {
        let service = Arc::new(Service::default());
        let tools = tools(service.clone(), Some("agent"));
        assert!(
            tools
                .call("service_restart", json!({"serviceId":"api"}))
                .await
                .is_err()
        );
        assert_eq!(service.controls.load(Ordering::SeqCst), 1);
        let old = super::tests::tools(
            Arc::new(Service {
                legacy: true,
                ..Default::default()
            }),
            Some("agent"),
        );
        assert!(
            old.call("service_list", json!({}))
                .await
                .unwrap_err()
                .contains("Update the Zeron daemon")
        );
    }

    #[tokio::test]
    async fn configuration_edits_validate_fields_and_are_not_replayed() {
        let service = Arc::new(Service::default());
        let tools = tools(service.clone(), Some("agent"));
        for (name, args) in [
            ("service_add", json!({"name":"api"})),
            (
                "service_add",
                json!({"name":"api","command":"true","chatId":"other"}),
            ),
            (
                "service_add",
                json!({"name":"api","command":"true","op":"update"}),
            ),
            ("service_update", json!({"serviceId":"api"})),
            ("service_update", json!({"serviceId":"api","command":null})),
            (
                "service_update",
                json!({"serviceId":"api","kind":"database"}),
            ),
            ("service_list", json!({"includeConfig":"yes"})),
        ] {
            assert!(tools.call(name, args).await.is_err(), "{name}");
        }
        assert_eq!(service.controls.load(Ordering::SeqCst), 0);
        for (name, args) in [
            ("service_add", json!({"name":"api","command":"true"})),
            (
                "service_update",
                json!({"serviceId":"api","command":"false"}),
            ),
        ] {
            assert!(tools.call(name, args).await.is_err());
        }
        assert_eq!(service.controls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn older_control_only_daemons_keep_status_and_explain_missing_edit_support() {
        let service = Arc::new(Service {
            control_only: true,
            ..Default::default()
        });
        let tools = tools(service.clone(), Some("agent"));
        tools.call("service_list", json!({})).await.unwrap();
        for (name, args) in [
            ("service_list", json!({"includeConfig":true})),
            ("service_add", json!({"name":"New","command":"true"})),
            (
                "service_update",
                json!({"serviceId":"api","name":"Changed"}),
            ),
        ] {
            assert!(
                tools
                    .call(name, args)
                    .await
                    .unwrap_err()
                    .contains("Restart or update")
            );
        }
        assert_eq!(service.controls.load(Ordering::SeqCst), 0);
    }
}
