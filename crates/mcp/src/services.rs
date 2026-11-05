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
];

pub fn catalog() -> Vec<ToolDef> {
    [
        ("service_list", "List configured services and databases for this chat's project and checkout, with process status and database readiness. Databases are shared by all project checkouts. Use the returned ids with the service tools."),
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
            json!({"type":"object","properties":{},"additionalProperties":false})
        } else {
            json!({"type":"object","properties":{"serviceId":{"type":"string","minLength":1,"description":"Exact id from service_list."}},"required":["serviceId"],"additionalProperties":false})
        },
    })
    .collect()
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
    let service_id = if name == "service_list" {
        ensure!(
            args.as_object().is_some_and(|a| a.is_empty()),
            "service_list takes no arguments"
        );
        None
    } else {
        let args: ServiceArgs = serde_json::from_value(args)?;
        ensure!(
            !args.service_id.trim().is_empty(),
            "nonempty serviceId required"
        );
        Some(args.service_id)
    };
    let info: zeron_proto::EngineInfo =
        serde_json::from_value(zeron.call(methods::ENGINE_INFO, json!({})).await?)?;
    ensure!(
        info.supports(zeron_proto::capabilities::PROJECT_SERVICE_CONTROL_V1),
        "Update the Zeron daemon to enable service tools"
    );
    let mut request = json!({"chatId":chat_id});
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
    }

    #[async_trait::async_trait]
    impl RpcService for Service {
        async fn handle(&self, method: &str, _: Value) -> Result<RpcReply, RpcError> {
            match method {
                methods::ENGINE_INFO => Ok(RpcReply::Value(
                    json!({"deviceId":"local","workspaceScope":"local","capabilities":if self.legacy { vec![] } else { zeron_proto::capabilities::current() }}),
                )),
                methods::GET_CHAT_SERVICES => Ok(RpcReply::Value(
                    json!({"projectId":"project","checkout":"/project","services":[{"id":"api","status":"stopped"}]}),
                )),
                methods::CONTROL_CHAT_SERVICE => {
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
}
