//! The calling chat's project worktree settings, as the Worktree settings page
//! edits them. The engine picks the project and checkout from the chat.
use anyhow::{Context, ensure};
use serde_json::{Value, json};
use zeron_rpc::methods;

use crate::{ToolDef, Zeron};

pub const GET: &str = "worktree_settings_get";
pub const UPDATE: &str = "worktree_settings_update";

pub fn catalog() -> Vec<ToolDef> {
    let command = |what: &str| json!({"type":"string","maxLength":16384,"description":what});
    let fields = json!({
        "scope":{"type":"string","enum":["project","checkout"],"description":"project: the defaults every worktree without its own settings uses. checkout: this chat's worktree only (creates its own settings from what it uses now). Not available in the main folder."},
        "reset":{"type":"boolean","description":"With scope checkout and no other fields: drop this worktree's own settings so it uses the project defaults again."},
        "branchPrefix":{"type":"string","maxLength":80,"description":"Prefix for new worktree branches, e.g. zeron/. Blank for none."},
        "envFiles":{"type":"array","maxItems":128,"description":"Env files (relative paths) a new worktree gets. Replaces the whole list. copy: an independent copy; link: a link to the main folder's file; follow: one private copy (envProfile) that follows the active conversation, which needs serviceMode followActive; skip.","items":{"type":"object","properties":{"path":{"type":"string"},"mode":{"type":"string","enum":["copy","link","follow","skip"]}},"required":["path"],"additionalProperties":false}},
        "envProfile":{"type":"string","minLength":1,"maxLength":80,"description":"Name of the private profile that follow-mode env files share."},
        "dependencies":{"type":"string","enum":["install","link","copy","skip"],"description":"How a new worktree gets its dependency folders: run installCommand, link or copy the main folder's, or skip."},
        "dependencyPaths":{"type":"array","maxItems":64,"items":{"type":"string"},"description":"Relative node_modules folders. Replaces the whole list."},
        "installCommand":command("Dependency install command. Empty detects Bun, pnpm, Yarn or npm from the lockfile."),
        "variables":{"type":"object","additionalProperties":{"type":"string"},"description":"Environment variables for workflow commands and the worktree's terminals. Replaces the whole map. ZERON_ and CODEX_ names are reserved."},
        "createCommand":command("Replaces `git worktree add`; runs in the project root. Empty uses Git."),
        "setupCommand":command("Runs when a worktree is set up (after it is created, and when it is prepared again)."),
        "activateCommand":command("Runs when a worktree becomes the active one."),
        "cleanupCommand":command("Runs before a worktree is removed."),
        "removeCommand":command("Replaces `git worktree remove`; runs after cleanup in the project root. Empty uses Git."),
        "commandsInProject":{"type":"boolean","description":"Run the setup, activate and cleanup commands in the main project folder instead of the worktree."},
        "commandTimeoutSeconds":{"type":"integer","minimum":1,"maximum":zeron_proto::MAX_WORKTREE_COMMAND_TIMEOUT_SECONDS,"description":"Time limit for each workflow command."},
        "serviceMode":{"type":"string","enum":["followActive","parallel"],"description":"Project scope only. followActive: one worktree runs app services at a time, following the active conversation. parallel: every worktree runs its own."}
    });
    vec![
        ToolDef {
            name: GET,
            description: "Worktree settings for this chat's project: the project defaults (`defaults`), what this chat's checkout uses (`settings`, and `hasOverrides` when it has its own), what Zeron detected (env files, dependency folders, install command), each worktree's setup state, and the folder new worktrees go in (`worktreeFolder`). `mainFolder` is true when this chat runs in the project's main folder. Services and databases are listed by service_list.",
            input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
        },
        ToolDef {
            name: UPDATE,
            description: "Change worktree settings for this chat's project: pass scope plus only the fields to change. Other fields and the services list stay as they are. Lists and maps you pass replace the saved ones, so read worktree_settings_get first. Saving runs nothing; Zeron uses the settings the next time it creates, prepares, activates or removes a worktree. Services and databases are edited with service_add and service_update. Requires full access outside read-only/plan mode, because workflow commands run outside the agent sandbox.",
            input_schema: json!({"type":"object","properties":fields,"required":["scope"],"minProperties":2,"additionalProperties":false}),
        },
    ]
}

pub async fn call(zeron: &Zeron, name: &str, args: Value) -> anyhow::Result<Value> {
    let chat_id = zeron
        .origin()
        .chat_id
        .clone()
        .context("worktree settings tools require a Zeron chat")?;
    let mut args = args
        .as_object()
        .context("worktree settings arguments must be an object")?
        .clone();
    let (method, request) = if name == UPDATE {
        let scope = args.remove("scope").context("scope is required")?;
        let reset = args.remove("reset").unwrap_or(json!(false));
        ensure!(reset.is_boolean(), "reset must be true or false");
        ensure!(
            args.values().all(|value| !value.is_null()),
            "Pass a value for each field to change"
        );
        (
            methods::EDIT_CHAT_WORKTREE_SETTINGS,
            json!({"chatId":chat_id,"scope":scope,"reset":reset,"changes":args}),
        )
    } else {
        ensure!(args.is_empty(), "worktree_settings_get takes no arguments");
        (
            methods::GET_CHAT_WORKTREE_SETTINGS,
            json!({"chatId":chat_id}),
        )
    };
    let info: zeron_proto::EngineInfo =
        serde_json::from_value(zeron.call(methods::ENGINE_INFO, json!({})).await?)?;
    ensure!(
        info.supports(zeron_proto::capabilities::CHAT_WORKTREE_SETTINGS_V1),
        "Update the Zeron daemon to enable worktree settings tools"
    );
    if name == UPDATE {
        // A disconnected reply must never save twice.
        zeron.call_once(method, request).await
    } else {
        zeron.call(method, request).await
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
        legacy: bool,
        sent: Mutex<Vec<(String, Value)>>,
    }

    #[async_trait::async_trait]
    impl RpcService for Engine {
        async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
            if method == methods::ENGINE_INFO {
                let capabilities = if self.legacy {
                    vec![]
                } else {
                    zeron_proto::capabilities::current()
                };
                return Ok(RpcReply::Value(
                    json!({"deviceId":"local","workspaceScope":"local","capabilities":capabilities}),
                ));
            }
            self.sent.lock().unwrap().push((method.into(), params));
            Ok(RpcReply::Value(json!({"saved":true})))
        }
    }

    fn tools(engine: Arc<Engine>, chat: Option<&str>) -> Tools {
        Tools::new(Arc::new(Zeron::with_client(
            zeron_rpc::memory_client(engine),
            Origin {
                chat_id: chat.map(str::to_owned),
                ..Default::default()
            },
        )))
    }

    #[tokio::test]
    async fn chat_scoped_requests_carry_only_named_changes() {
        let engine = Arc::new(Engine::default());
        let standalone = tools(engine.clone(), None);
        assert!(!standalone.has(GET) && !standalone.has(UPDATE));
        assert!(
            standalone
                .call(GET, json!({}))
                .await
                .unwrap_err()
                .contains("require a Zeron chat")
        );

        let agent = tools(engine.clone(), Some("agent"));
        assert!(agent.has(GET) && agent.has(UPDATE));
        for args in [
            json!({"setupCommand":"bun install"}),
            json!({"scope":"project","setupCommand":null}),
            json!({"scope":"project","reset":"yes"}),
        ] {
            assert!(agent.call(UPDATE, args).await.is_err());
        }
        assert!(agent.call(GET, json!({"chatId":"other"})).await.is_err());
        assert!(engine.sent.lock().unwrap().is_empty());

        agent.call(GET, json!({})).await.unwrap();
        agent
            .call(
                UPDATE,
                json!({"scope":"checkout","setupCommand":"bun install"}),
            )
            .await
            .unwrap();
        let sent = engine.sent.lock().unwrap();
        assert_eq!(
            sent[0],
            (
                methods::GET_CHAT_WORKTREE_SETTINGS.into(),
                json!({"chatId":"agent"})
            )
        );
        assert_eq!(
            sent[1],
            (
                methods::EDIT_CHAT_WORKTREE_SETTINGS.into(),
                json!({"chatId":"agent","scope":"checkout","reset":false,"changes":{"setupCommand":"bun install"}})
            )
        );
    }

    #[tokio::test]
    async fn older_daemons_are_explained() {
        let engine = Arc::new(Engine {
            legacy: true,
            ..Default::default()
        });
        let agent = tools(engine.clone(), Some("agent"));
        assert!(
            agent
                .call(GET, json!({}))
                .await
                .unwrap_err()
                .contains("Update the Zeron daemon")
        );
        assert!(engine.sent.lock().unwrap().is_empty());
    }
}
