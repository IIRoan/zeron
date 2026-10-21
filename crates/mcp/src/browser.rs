//! Chat-scoped browser tools. Mutations are never replayed on reconnect.
use crate::{ToolDef, Zeron};
use anyhow::{Context, bail};
use serde_json::{Value, json};
use zeron_proto::browser::{BrowserAction, BrowserRequest};
use zeron_rpc::methods;

pub fn catalog() -> Vec<ToolDef> {
    let specs = [
        (
            "browser_list_tabs",
            "List this chat's browser tabs. Enable the bot button on a tab to allow control. Tabs in other chats are unavailable.",
            "",
            json!({}),
        ),
        (
            "browser_new_tab",
            "Open a background tab in this chat. browserId selects an existing tab whose agent-control toggle is enabled; its consent applies to the new tab.",
            "new_tab",
            json!({"url":{"type":"string"}}),
        ),
        (
            "browser_close_tab",
            "Close a tab in this chat.",
            "close",
            json!({}),
        ),
        (
            "browser_navigate",
            "Navigate a tab to an explicit http or https URL. Then use browser_wait before interacting.",
            "navigate",
            json!({"url":{"type":"string"}}),
        ),
        (
            "browser_back",
            "Navigate back in a tab's history.",
            "back",
            json!({}),
        ),
        (
            "browser_forward",
            "Navigate forward in a tab's history.",
            "forward",
            json!({}),
        ),
        ("browser_reload", "Reload a tab.", "reload", json!({})),
        (
            "browser_snapshot",
            "Inspect a page's visible DOM roles, names, field values and element refs. Password values are omitted. Refs expire on the next snapshot, navigation, replacement or target changes. Cross-origin frames must be opened separately.",
            "snapshot",
            json!({}),
        ),
        (
            "browser_screenshot",
            "Capture the tab's current viewport as a PNG image, returned as MCP image content.",
            "screenshot",
            json!({}),
        ),
        (
            "browser_click",
            "Click a visible, enabled, unobscured element using native pointer input. ref must come from this tab's latest snapshot.",
            "click",
            json!({"ref":{"type":"string"}}),
        ),
        (
            "browser_fill",
            "Replace a text field's value and emit input/change events. Use a current snapshot ref.",
            "fill",
            json!({"ref":{"type":"string"},"value":{"type":"string"}}),
        ),
        (
            "browser_type",
            "Insert text into the focused editable element using the browser's editing command.",
            "type",
            json!({"text":{"type":"string"}}),
        ),
        (
            "browser_keypress",
            "Send a native key press: Enter, Tab, Escape, Backspace, Delete, arrow keys, Home, End, PageUp, PageDown, Space, or one character.",
            "keypress",
            json!({"key":{"type":"string"}}),
        ),
        (
            "browser_scroll",
            "Scroll the page by x/y CSS pixels.",
            "scroll",
            json!({"x":{"type":"number"},"y":{"type":"number"}}),
        ),
        (
            "browser_select",
            "Select an enabled option by its value in a select element. Use a current snapshot ref.",
            "select",
            json!({"ref":{"type":"string"},"value":{"type":"string"}}),
        ),
        (
            "browser_evaluate",
            "Evaluate synchronous JavaScript in the page and return bounded JSON. Use this for assertions. Return JSON values, not promises or DOM objects.",
            "evaluate",
            json!({"script":{"type":"string"}}),
        ),
        (
            "browser_wait",
            "Wait up to 30 seconds for page text after loading. Returns a fresh snapshot; previous refs expire.",
            "wait",
            json!({"text":{"type":"string"}}),
        ),
    ];
    specs.into_iter().map(|(name, description, action, extra)| {
        let mut properties = extra.as_object().unwrap().clone();
        if !action.is_empty() { properties.insert("browserId".into(), json!({"type":"string"})); }
        let required: Vec<_> = properties.keys().cloned().collect();
        ToolDef { name, description, input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}) }
    }).collect()
}

pub async fn call(zeron: &Zeron, name: &str, args: Value) -> anyhow::Result<Value> {
    let chat_id = zeron
        .origin()
        .chat_id
        .as_ref()
        .context("browser tools require a Zeron chat")?;
    let info: zeron_proto::EngineInfo =
        serde_json::from_value(zeron.call(methods::ENGINE_INFO, json!({})).await?)?;
    if !info.supports(zeron_proto::capabilities::BROWSER_CONTROL_V1) {
        bail!("Update the Zeron daemon to enable browser tools");
    }
    if name == "browser_list_tabs" {
        anyhow::ensure!(
            args.as_object().is_some_and(|args| args.is_empty()),
            "browser_list_tabs takes no arguments"
        );
        return zeron
            .call(methods::LIST_BROWSER_TABS, json!({"chatId":chat_id}))
            .await;
    }
    let action = match name {
        "browser_new_tab" => "new_tab",
        "browser_close_tab" => "close",
        "browser_navigate" => "navigate",
        "browser_back" => "back",
        "browser_forward" => "forward",
        "browser_reload" => "reload",
        "browser_snapshot" | "browser_wait" => "snapshot",
        "browser_screenshot" => "screenshot",
        "browser_click" => "click",
        "browser_fill" => "fill",
        "browser_type" => "type",
        "browser_keypress" => "keypress",
        "browser_scroll" => "scroll",
        "browser_select" => "select",
        "browser_evaluate" => "evaluate",
        _ => bail!("unknown browser tool"),
    };
    let mut args = args
        .as_object()
        .context("browser arguments must be an object")?
        .clone();
    let browser_id = args
        .remove("browserId")
        .and_then(|id| id.as_str().map(str::to_owned))
        .context("browserId required")?;
    let wait_text = if name == "browser_wait" {
        Some(
            args.remove("text")
                .and_then(|text| text.as_str().map(str::to_owned))
                .filter(|text| !text.is_empty())
                .context("nonempty text required")?,
        )
    } else {
        None
    };
    anyhow::ensure!(
        !args.contains_key("action"),
        "action is selected by the tool name"
    );
    args.insert("action".into(), action.into());
    let command: BrowserAction = serde_json::from_value(Value::Object(args))?;
    let request = serde_json::to_value(BrowserRequest {
        chat_id: chat_id.clone(),
        browser_id,
        command,
    })?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let call = zeron.call_once(methods::CONTROL_BROWSER, request.clone());
        let result = if wait_text.is_some() {
            tokio::time::timeout_at(deadline, call)
                .await
                .context("browser_timeout: text did not appear")?
        } else {
            call.await
        };
        let Some(text) = &wait_text else {
            return result;
        };
        match result {
            Ok(value) if snapshot_text(&value).contains(text) => return Ok(value),
            Ok(_) => {}
            Err(error) if error.to_string().contains("browser_loading") => {}
            Err(error) => return Err(error),
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("browser_timeout: text did not appear");
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

fn snapshot_text(value: &Value) -> String {
    let mut text = value["title"].as_str().unwrap_or_default().to_owned();
    if let Some(elements) = value["elements"].as_array() {
        for element in elements {
            for key in ["name", "value"] {
                text.push('\n');
                text.push_str(element[key].as_str().unwrap_or_default());
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Origin, Tools};
    use std::sync::{Arc, Mutex};
    use zeron_rpc::{RpcError, RpcReply, RpcService};

    #[derive(Default)]
    struct BrowserService {
        calls: Mutex<Vec<Value>>,
    }
    #[async_trait::async_trait]
    impl RpcService for BrowserService {
        async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
            match method {
                methods::ENGINE_INFO => RpcReply::value(
                    &json!({"deviceId":"device","workspaceScope":"local","capabilities":[zeron_proto::capabilities::BROWSER_CONTROL_V1]}),
                ),
                methods::LIST_BROWSER_TABS => {
                    assert_eq!(params["chatId"], "origin");
                    RpcReply::value(&json!([{ "id":"host/1", "chatId":"origin", "enabled":true }]))
                }
                methods::CONTROL_BROWSER => {
                    let request: BrowserRequest = serde_json::from_value(params.clone()).unwrap();
                    assert_eq!(request.chat_id, "origin");
                    self.calls.lock().unwrap().push(params);
                    RpcReply::value(&json!({"title":"Page","elements":[{"name":"Done \"now\""}]}))
                }
                _ => Err(RpcError::UnknownMethod(method.into())),
            }
        }
    }

    #[tokio::test]
    async fn tools_bind_to_the_origin_chat_and_validate_arguments_before_dispatch() {
        let service = Arc::new(BrowserService::default());
        let zeron = Arc::new(Zeron::with_client(
            zeron_rpc::memory_client(service.clone()),
            Origin {
                chat_id: Some("origin".into()),
                device_id: None,
            },
        ));
        let tools = Tools::new(zeron);
        assert!(tools.has("browser_snapshot"));
        assert_eq!(
            tools.call("browser_list_tabs", json!({})).await.unwrap()[0]["id"],
            "host/1"
        );
        tools
            .call("browser_click", json!({"browserId":"host/1","ref":"fresh"}))
            .await
            .unwrap();
        tools
            .call(
                "browser_wait",
                json!({"browserId":"host/1","text":"Done \"now\""}),
            )
            .await
            .unwrap();
        assert!(
            tools
                .call(
                    "browser_click",
                    json!({"browserId":"host/1","ref":"fresh","chatId":"other"})
                )
                .await
                .is_err()
        );
        assert!(
            tools
                .call("browser_click", json!({"browserId":"host/1"}))
                .await
                .is_err()
        );
        assert_eq!(service.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn browser_tools_are_not_available_without_an_origin_chat() {
        let zeron = Arc::new(Zeron::with_client(
            zeron_rpc::memory_client(Arc::new(BrowserService::default())),
            Origin::default(),
        ));
        let tools = Tools::new(zeron);
        assert!(!tools.has("browser_snapshot"));
        assert!(
            tools
                .call("browser_list_tabs", json!({}))
                .await
                .unwrap_err()
                .contains("require a Zeron chat")
        );
    }
}
