//! The Windows VM over RPC (`crate::windows_vm`, `docs/windows-vm.md`).
//!
//! Two surfaces. The app's project settings connect a project to the VM and
//! choose the ports that cross (`*ProjectWindowsVm`): never for agents. A
//! chat uses the VM its project is connected to (`ChatWindowsVm`): start and
//! stop it, run commands in it, see and drive its screen.
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use zeron_rpc::{Caller, RpcError, RpcReply, methods, parse_params};

use super::EngineRpc;
use crate::windows_vm::{self, WindowsVm};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectParams {
    space_id: String,
    /// `GetProjectWindowsVm`: only whether the project is connected and
    /// how, without asking the VM for its state.
    #[serde(default)]
    brief: bool,
    /// `SetProjectWindowsVm`: connect the project, or disconnect it.
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    to_vm: Vec<u16>,
    #[serde(default)]
    from_vm: Vec<u16>,
    #[serde(default)]
    viewer: bool,
    #[serde(default)]
    start_with: Vec<String>,
    /// Absent: on.
    #[serde(default)]
    auto_ports: Option<bool>,
    /// `ControlProjectWindowsVm`: `start`, `stop`, `view`, `diagnostics`,
    /// `screenshot`, `exec` or `launch`.
    #[serde(default)]
    action: Option<String>,
    /// `exec`: PowerShell. `launch`: a command line.
    #[serde(default)]
    command: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChatParams {
    chat_id: String,
    /// `status`, `diagnostics`, `start`, `stop`, `exec`, `launch`,
    /// `screenshot` or `input`.
    action: String,
    /// `exec`: PowerShell. `launch`: a command line.
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    /// `input`: `click`, `doubleclick`, `rightclick`, `move`, `type` or `key`.
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    x: Option<u32>,
    #[serde(default)]
    y: Option<u32>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    keys: Vec<String>,
}

fn failed(error: crate::EngineError) -> RpcError {
    RpcError::Failed(error.to_string())
}

/// `winvm`'s hints name its own commands; a chat has tools instead.
fn for_chat(error: crate::EngineError) -> RpcError {
    RpcError::Failed(error.to_string().replace("(winvm up)", "(call vm_start)"))
}

/// The VM's state for a reply; an error there (no `winvm`, no VM built) is
/// part of the answer, not a failed request.
async fn vm_state() -> Value {
    match windows_vm::status().await {
        Ok(status) => status,
        Err(error) => json!({"state": "unavailable", "error": error.to_string()}),
    }
}

impl EngineRpc {
    /// The app's view of one project's Windows VM: whether it is connected
    /// and how, and the VM's state. Connecting it, and starting or stopping
    /// the VM from here, is the person's.
    pub(super) async fn project_windows_vm(
        &self,
        method: &str,
        params: Value,
    ) -> Result<RpcReply, RpcError> {
        if matches!(zeron_rpc::caller(), Caller::Agent { .. }) {
            return Err(RpcError::Failed(
                "A project's Windows VM is set up in Zeron's project settings".into(),
            ));
        }
        let p: ProjectParams = parse_params(params)?;
        let space = self.local_project_action_space(&p.space_id)?;
        let root = std::path::PathBuf::from(&space.path);
        if method == methods::SET_PROJECT_WINDOWS_VM {
            if p.enabled {
                windows_vm::enable(WindowsVm {
                    root: root.clone(),
                    to_vm: p.to_vm,
                    from_vm: p.from_vm,
                    viewer: p.viewer,
                    start_with: p.start_with,
                    auto_ports: p.auto_ports.unwrap_or(true),
                })
                .map_err(failed)?;
            } else {
                windows_vm::disable(&root).map_err(failed)?;
            }
        }
        let settings = windows_vm::for_root(&root).map_err(failed)?;
        // What the action answered: the diagnostics, a command's output, the
        // screen. The VM's own state follows in `vm` (not with the screen).
        let mut result = Value::Null;
        if method == methods::CONTROL_PROJECT_WINDOWS_VM {
            let vm = settings.as_ref().ok_or_else(|| {
                RpcError::Failed("Connect this project to the Windows VM first".into())
            })?;
            let command = || {
                p.command
                    .as_deref()
                    .filter(|command| !command.trim().is_empty())
                    .ok_or_else(|| RpcError::BadParams("command is required".into()))
            };
            match p.action.as_deref() {
                Some("start") => {
                    windows_vm::start(vm).await.map_err(failed)?;
                }
                Some("stop") => windows_vm::stop().await.map_err(failed)?,
                Some("view") => windows_vm::view().await.map_err(failed)?,
                Some("diagnostics") => {
                    result = windows_vm::diagnostics().await.map_err(failed)?;
                }
                Some("screenshot") => {
                    let (png, _) = windows_vm::screenshot().await.map_err(failed)?;
                    result = json!({
                        "mimeType": "image/png",
                        "data": base64::engine::general_purpose::STANDARD.encode(png),
                    });
                }
                Some("exec") => {
                    result = windows_vm::exec(command()?, None, 120)
                        .await
                        .map_err(failed)?;
                }
                Some("launch") => {
                    windows_vm::launch(command()?).await.map_err(failed)?;
                    result = json!({"launched": true});
                }
                _ => {
                    return Err(RpcError::BadParams(
                        "action: start, stop, view, diagnostics, screenshot, exec or launch".into(),
                    ));
                }
            }
        }
        // Not with the screen: the panel takes that picture every few
        // seconds and reads the VM's state on its own schedule.
        let vm = match p.brief || p.action.as_deref() == Some("screenshot") {
            true => Value::Null,
            false => vm_state().await,
        };
        Ok(RpcReply::Value(json!({
            "available": windows_vm::available(),
            "settings": settings,
            "vm": vm,
            "result": result,
        })))
    }

    /// The calling chat's use of its project's Windows VM. Reading its state
    /// and screen is any chat's; changing anything there (starting it,
    /// commands, input) needs a chat that may run commands at all.
    pub(super) async fn chat_windows_vm(&self, params: Value) -> Result<RpcReply, RpcError> {
        let p: ChatParams = parse_params(params)?;
        if let Caller::Agent { chat } = zeron_rpc::caller()
            && chat.as_deref() != Some(&p.chat_id)
        {
            return Err(RpcError::Failed(
                "Windows VM tools act for the calling chat only".into(),
            ));
        }
        let chat = self
            .workspace
            .chat(&p.chat_id)
            .map_err(|e| RpcError::Failed(e.to_string()))?
            .ok_or_else(|| RpcError::Failed("Chat not found".into()))?;
        if chat.device_id != self.doc_host.device_id() {
            return Err(RpcError::Failed(
                "Windows VM tools require a chat on this device".into(),
            ));
        }
        let space = chat
            .space_id
            .as_deref()
            .map(|id| self.local_project_action_space(id))
            .transpose()?
            .ok_or_else(|| RpcError::Failed("This chat has no project".into()))?;
        let vm = windows_vm::for_root(std::path::Path::new(&space.path))
            .map_err(failed)?
            .ok_or_else(|| {
                RpcError::Failed(
                    "This project is not connected to a Windows VM. The person connects it in Project settings → Environment → Windows VM, or with zeron vm enable.".into(),
                )
            })?;
        if !matches!(p.action.as_str(), "status" | "diagnostics" | "screenshot") {
            self.require_full_access(&p.chat_id, "Using the Windows VM")?;
        }
        let text = |value: Option<String>, what: &str| {
            value
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| RpcError::BadParams(format!("{what} is required")))
        };
        let reply = match p.action.as_str() {
            "status" => json!({"project": vm, "vm": vm_state().await}),
            "diagnostics" => windows_vm::diagnostics().await.map_err(for_chat)?,
            "start" => {
                json!({"project": vm, "vm": windows_vm::start(&vm).await.map_err(for_chat)?})
            }
            "stop" => {
                windows_vm::stop().await.map_err(for_chat)?;
                json!({"vm": vm_state().await})
            }
            "exec" => {
                let command = text(p.command, "command")?;
                let timeout = p.timeout_seconds.unwrap_or(120).clamp(1, 600);
                windows_vm::exec(&command, p.cwd.as_deref(), timeout)
                    .await
                    .map_err(for_chat)?
            }
            "launch" => {
                windows_vm::launch(&text(p.command, "command")?)
                    .await
                    .map_err(for_chat)?;
                json!({"launched": true})
            }
            "screenshot" => {
                let (png, _) = windows_vm::screenshot().await.map_err(for_chat)?;
                json!({
                    "mimeType": "image/png",
                    "data": base64::engine::general_purpose::STANDARD.encode(png),
                })
            }
            "input" => {
                let kind = text(p.input, "input")?;
                let values = match kind.as_str() {
                    "click" | "doubleclick" | "rightclick" | "move" => match (p.x, p.y) {
                        (Some(x), Some(y)) => vec![x.to_string(), y.to_string()],
                        _ => return Err(RpcError::BadParams(format!("{kind} needs x and y"))),
                    },
                    "type" => vec![text(p.text, "text")?],
                    "key" if !p.keys.is_empty() => p.keys,
                    "key" => return Err(RpcError::BadParams("key needs keys".into())),
                    _ => {
                        return Err(RpcError::BadParams(
                            "input: click, doubleclick, rightclick, move, type or key".into(),
                        ));
                    }
                };
                windows_vm::input(&kind, &values).await.map_err(for_chat)?;
                json!({"sent": true})
            }
            _ => {
                return Err(RpcError::BadParams(
                    "action: status, diagnostics, start, stop, exec, launch, screenshot or input"
                        .into(),
                ));
            }
        };
        Ok(RpcReply::Value(reply))
    }
}
