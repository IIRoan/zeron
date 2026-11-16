//! The Windows VM of the chat's project (`zeron_engine::windows_vm`,
//! `docs/windows-vm.md`): start it, run commands in it, see and drive its
//! screen. The person connects a project to the VM and chooses the ports
//! that cross; no tool here changes that.
use anyhow::{Context, ensure};
use serde_json::{Value, json};
use zeron_rpc::methods;

use crate::{ToolDef, Zeron};

pub const STATUS: &str = "vm_status";
pub const DIAGNOSTICS: &str = "vm_diagnostics";
pub const START: &str = "vm_start";
pub const STOP: &str = "vm_stop";
pub const EXEC: &str = "vm_exec";
pub const LAUNCH: &str = "vm_launch";
pub const SCREENSHOT: &str = "vm_screenshot";
pub const INPUT: &str = "vm_input";
pub const NAMES: [&str; 8] = [
    STATUS,
    DIAGNOSTICS,
    START,
    STOP,
    EXEC,
    LAUNCH,
    SCREENSHOT,
    INPUT,
];

pub fn catalog() -> Vec<ToolDef> {
    let none = || json!({"type":"object","properties":{},"additionalProperties":false});
    vec![
        ToolDef {
            name: STATUS,
            description: "The Windows VM this chat's project is connected to: `vm.state` is stopped, starting or running; `vm.toVm` are ports of this machine the VM reaches on its own localhost (the project's dev servers), `vm.fromVm` VM ports this machine reaches on its localhost; `vm.tunnelError` says why ports are not connected. Fails when the project is not connected to a VM: the person connects it in Project settings → Environment.",
            input_schema: none(),
        },
        ToolDef {
            name: DIAGNOSTICS,
            description: "The running VM's health in one reading: Windows version, uptime, logged-on user, CPU load, memory and disk free, display mode, the biggest processes, listening ports with their process, automatic services that are stopped, and the last hour's errors from the Application and System event logs. Start here when something in the VM misbehaves; use vm_exec for anything more specific (a log file, one process, one service).",
            input_schema: none(),
        },
        ToolDef {
            name: START,
            description: "Start the project's Windows VM if it is not running, wait until Windows answers, and connect the project's ports. Usually under a minute; the first start after a disk snapshot can take several. Safe to call when it already runs. Check `vm.tunnelError` in the result.",
            input_schema: none(),
        },
        ToolDef {
            name: STOP,
            description: "Shut Windows down and stop the VM (it can take a few minutes). Other chats of the project lose it too: stop it only when the Windows work is done.",
            input_schema: none(),
        },
        ToolDef {
            name: EXEC,
            description: "Run a PowerShell command in the VM as its administrator and return exitCode, stdout and stderr (the last 100 KB of each). The session has no desktop: a program with a window started here is invisible, so use vm_launch for those. The VM has its own disk; the project's files are not there unless something copied them. `localhost` there reaches only the ports in `vm.toVm`.",
            input_schema: json!({
                "type":"object",
                "properties":{
                    "command":{"type":"string","description":"PowerShell, e.g. `Get-Content C:\\\\logs\\\\app.log -Tail 50` or `cmd /c dir`."},
                    "cwd":{"type":"string","description":"Windows folder to run it in."},
                    "timeoutSeconds":{"type":"integer","minimum":1,"maximum":600,"description":"Default 120. A command still running then keeps running in the VM."}
                },
                "required":["command"],
                "additionalProperties":false
            }),
        },
        ToolDef {
            name: LAUNCH,
            description: "Start a program on the VM's logged-in desktop (where its windows show and vm_screenshot sees them), without waiting for it. For a command line with arguments, quote paths as cmd.exe expects.",
            input_schema: json!({
                "type":"object",
                "properties":{"command":{"type":"string","description":"A command line, e.g. `notepad.exe C:\\\\notes.txt`."}},
                "required":["command"],
                "additionalProperties":false
            }),
        },
        ToolDef {
            name: SCREENSHOT,
            description: "The VM's screen as a PNG image, exactly as displayed (whatever desktop Windows currently shows). Coordinates for vm_input are pixels of this image.",
            input_schema: none(),
        },
        ToolDef {
            name: INPUT,
            description: "Pointer and keyboard input on the VM's screen, as a person at it. `click`, `doubleclick`, `rightclick` and `move` take x and y in screenshot pixels; `type` takes text (US keyboard characters); `key` takes combinations such as `ctrl+s`, `alt+f4`, `enter`, `win`. Take a screenshot before to find the target and after to see the effect.",
            input_schema: json!({
                "type":"object",
                "properties":{
                    "input":{"type":"string","enum":["click","doubleclick","rightclick","move","type","key"]},
                    "x":{"type":"integer","minimum":0},
                    "y":{"type":"integer","minimum":0},
                    "text":{"type":"string"},
                    "keys":{"type":"array","items":{"type":"string"},"description":"One combination per entry, sent in order."}
                },
                "required":["input"],
                "additionalProperties":false
            }),
        },
    ]
}

pub async fn call(zeron: &Zeron, name: &str, args: Value) -> anyhow::Result<Value> {
    let chat_id = zeron
        .origin()
        .chat_id
        .clone()
        .context("Windows VM tools require a Zeron chat")?;
    let args = args
        .as_object()
        .context("Windows VM arguments must be an object")?
        .clone();
    let allowed: &[&str] = match name {
        EXEC => &["command", "cwd", "timeoutSeconds"],
        LAUNCH => &["command"],
        INPUT => &["input", "x", "y", "text", "keys"],
        _ => &[],
    };
    let unknown: Vec<_> = args
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .collect();
    ensure!(unknown.is_empty(), "unknown arguments: {unknown:?}");
    let info: zeron_proto::EngineInfo =
        serde_json::from_value(zeron.call(methods::ENGINE_INFO, json!({})).await?)?;
    ensure!(
        info.supports(zeron_proto::capabilities::WINDOWS_VM_V1),
        "Update the Zeron daemon to use the Windows VM tools"
    );
    let mut request = Value::Object(args);
    request["chatId"] = json!(chat_id);
    request["action"] = json!(name.trim_start_matches("vm_"));
    match name {
        STATUS | DIAGNOSTICS | SCREENSHOT => zeron.call(methods::CHAT_WINDOWS_VM, request).await,
        // A lost connection must never repeat a command, a click or a shutdown.
        _ => zeron.call_once(methods::CHAT_WINDOWS_VM, request).await,
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
            Ok(RpcReply::Value(json!({"vm": {"state": "running"}})))
        }
    }

    #[tokio::test]
    async fn each_tool_sends_its_action_for_the_calling_chat_only() {
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
        assert!(NAMES.iter().all(|name| !standalone.has(name)));
        let agent = tools(Some("agent"));
        assert!(NAMES.iter().all(|name| agent.has(name)));
        for (name, args) in [
            (STATUS, json!({"chatId": "other"})),
            (START, json!({"toVm": [22]})),
            (EXEC, json!({"command": "dir", "project": "/elsewhere"})),
            (SCREENSHOT, json!({"action": "stop"})),
        ] {
            assert!(agent.call(name, args).await.is_err(), "{name}");
        }
        assert!(engine.sent.lock().unwrap().is_empty());
        agent.call(STATUS, json!({})).await.unwrap();
        agent
            .call(EXEC, json!({"command": "dir", "timeoutSeconds": 5}))
            .await
            .unwrap();
        agent
            .call(INPUT, json!({"input": "click", "x": 10, "y": 20}))
            .await
            .unwrap();
        let sent = engine.sent.lock().unwrap();
        assert_eq!(
            sent[0],
            (
                methods::CHAT_WINDOWS_VM.into(),
                json!({"chatId": "agent", "action": "status"})
            )
        );
        assert_eq!(
            sent[1].1,
            json!({"chatId": "agent", "action": "exec", "command": "dir", "timeoutSeconds": 5})
        );
        assert_eq!(
            sent[2].1,
            json!({"chatId": "agent", "action": "input", "input": "click", "x": 10, "y": 20})
        );
    }
}
