//! Project sandboxes (`zeron_engine::sandbox`, `docs/sandboxes.md`): see them,
//! and set one up or remove it, the way `zeron sandbox` does. The engine
//! refuses changes from a chat inside any sandbox, and from one without full
//! access.
use anyhow::{Context, ensure};
use serde_json::{Value, json};
use zeron_rpc::methods;

use crate::{ToolDef, Zeron};

pub const STATUS: &str = "sandbox_status";
pub const ENABLE: &str = "sandbox_enable";
pub const DISABLE: &str = "sandbox_disable";

pub fn catalog() -> Vec<ToolDef> {
    let project = json!({"type":"string","description":"Project id, path, or name on this device. Defaults to this chat's project."});
    let ports = |what: &str| json!({"type":"array","items":{"type":"integer","minimum":1,"maximum":65535},"description":what});
    vec![
        ToolDef {
            name: STATUS,
            description: "Project sandboxes on this device: each sandboxed project (its folder, worktrees folder, devcontainer config, host and published ports, read-only paths), its container's state (running, exited, not created), whether its image is built, and its last build log. `chat.inSandbox` says whether this chat runs inside one.",
            input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
        },
        ToolDef {
            name: ENABLE,
            description: "Run a project's agents (Claude Code, Codex), app services, worktree setup commands, project actions and terminals in a container built from a devcontainer config (Linux, rootless podman, devcontainer CLI). Its worktrees are included; shared databases stay on this machine. Builds the image and starts the container: it can take minutes. If the call times out, check sandbox_status: the build carries on. Calling it again for a sandboxed project replaces its settings. Use the project's own devcontainer config (its folder is then mounted read-only; list the compose file or Dockerfile it names elsewhere in the project in readOnly), or write one outside every project. Its image must already hold the agent CLIs and the project's toolchain: Zeron runs no lifecycle commands (onCreateCommand and the like). Afterwards the person logs the agents in once inside the container, from a Zeron terminal of that project. Refused from a chat inside a sandbox, and without full access outside read-only/plan mode.",
            input_schema: json!({
                "type":"object",
                "properties":{
                    "project": project,
                    "config":{"type":"string","description":"Absolute path of the devcontainer.json: in this project's main folder, or outside every sandboxed project."},
                    "hostPorts": ports("Ports on this machine the container reaches on its localhost (databases)."),
                    "publishPorts": ports("Container ports this machine reaches on its localhost (dev servers)."),
                    "readOnly":{"type":"array","items":{"type":"string"},"description":"Paths relative to the project that this machine reads (a database's compose file, a bunfig.toml its checks load), mounted read-only so agents can't change them. .git config and hooks always are."}
                },
                "required":["config"],
                "additionalProperties":false
            }),
        },
        ToolDef {
            name: DISABLE,
            description: "Run a sandboxed project on this machine again and remove its container (its image and home volume, with the agents' logins, are kept). Refused from a chat inside a sandbox, and without full access outside read-only/plan mode.",
            input_schema: json!({"type":"object","properties":{"project": project},"additionalProperties":false}),
        },
    ]
}

pub async fn call(zeron: &Zeron, name: &str, args: Value) -> anyhow::Result<Value> {
    let chat_id = zeron
        .origin()
        .chat_id
        .clone()
        .context("sandbox tools require a Zeron chat")?;
    let mut args = args
        .as_object()
        .context("sandbox arguments must be an object")?
        .clone();
    let mut request = json!({"chatId": chat_id});
    if name != STATUS {
        if let Some(project) = args.remove("project") {
            let key = project
                .as_str()
                .context("project is a project's id, path or name")?;
            request["spaceId"] = json!(zeron.resolve_space(key).await?.id);
        }
        request["enabled"] = json!(name == ENABLE);
        for key in ["config", "hostPorts", "publishPorts", "readOnly"] {
            if let Some(value) = args.remove(key) {
                ensure!(name == ENABLE, "{name} takes only a project");
                request[key] = value;
            }
        }
    }
    ensure!(
        args.is_empty(),
        "unknown arguments: {:?}",
        args.keys().collect::<Vec<_>>()
    );
    let info: zeron_proto::EngineInfo =
        serde_json::from_value(zeron.call(methods::ENGINE_INFO, json!({})).await?)?;
    ensure!(
        info.supports(zeron_proto::capabilities::PROJECT_SANDBOX_V1),
        "Update the Zeron daemon to enable sandbox tools"
    );
    match name {
        STATUS => zeron.call(methods::GET_SANDBOXES, request).await,
        // A lost connection must never repeat a build or a removal.
        _ => zeron.call_once(methods::SET_SANDBOX, request).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Origin, Tools};
    use std::sync::{Arc, Mutex};
    use zeron_rpc::{RpcError, RpcReply, RpcService};

    #[derive(Default)]
    struct Engine {
        sent: Mutex<Vec<(String, Value)>>,
    }

    #[async_trait::async_trait]
    impl RpcService for Engine {
        async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
            if method == methods::ENGINE_INFO {
                return Ok(RpcReply::Value(json!({
                    "deviceId": "local", "workspaceScope": "local",
                    "capabilities": zeron_proto::capabilities::current()
                })));
            }
            self.sent.lock().unwrap().push((method.into(), params));
            Ok(RpcReply::Value(json!({"projects": []})))
        }
    }

    #[tokio::test]
    async fn chat_scoped_requests_carry_only_what_each_tool_takes() {
        let engine = Arc::new(Engine::default());
        let tools = |chat: Option<&str>| {
            Tools::new(Arc::new(Zeron::with_client(
                zeron_rpc::memory_client(engine.clone()),
                Origin {
                    chat_id: chat.map(str::to_owned),
                    ..Default::default()
                },
            )))
        };
        let standalone = tools(None);
        assert!(!standalone.has(STATUS) && !standalone.has(ENABLE));
        let agent = tools(Some("agent"));
        assert!(agent.has(STATUS) && agent.has(ENABLE) && agent.has(DISABLE));
        for (name, args) in [
            (DISABLE, json!({"config": "/c/devcontainer.json"})),
            (STATUS, json!({"chatId": "other"})),
            (ENABLE, json!({"config": "/c", "privileged": true})),
        ] {
            assert!(agent.call(name, args).await.is_err(), "{name}");
        }
        assert!(engine.sent.lock().unwrap().is_empty());
        agent.call(STATUS, json!({})).await.unwrap();
        agent
            .call(
                ENABLE,
                json!({"config": "/c/devcontainer.json", "hostPorts": [5432]}),
            )
            .await
            .unwrap();
        agent.call(DISABLE, json!({})).await.unwrap();
        let sent = engine.sent.lock().unwrap();
        assert_eq!(
            sent[0],
            (methods::GET_SANDBOXES.into(), json!({"chatId": "agent"}))
        );
        assert_eq!(
            sent[1],
            (
                methods::SET_SANDBOX.into(),
                json!({"chatId": "agent", "enabled": true, "config": "/c/devcontainer.json", "hostPorts": [5432]})
            )
        );
        assert_eq!(
            sent[2],
            (
                methods::SET_SANDBOX.into(),
                json!({"chatId": "agent", "enabled": false})
            )
        );
    }
}
