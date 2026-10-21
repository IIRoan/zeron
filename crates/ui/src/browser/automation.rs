use super::BrowserSurface;
use futures::{FutureExt, future::BoxFuture};
use serde_json::{Value, json};
use zeron_proto::browser::BrowserAction;

pub(crate) type BrowserWork = BoxFuture<'static, Result<Value, String>>;

pub(crate) fn script(command: &BrowserAction) -> Result<String, String> {
    let encoded = serde_json::to_string(command).map_err(|error| error.to_string())?;
    Ok(include_str!("automation.js").replace("__ZERON_COMMAND__", &encoded))
}

impl BrowserSurface {
    pub(crate) fn set_agent_control(&mut self, enabled: bool, cx: &mut gpui::Context<Self>) {
        self.agent_control = enabled;
        cx.emit(super::BrowserEvent::Changed);
        cx.notify();
    }

    #[cfg(feature = "browser-fixture")]
    pub fn fixture_set_agent_control(&mut self, enabled: bool, cx: &mut gpui::Context<Self>) {
        self.set_agent_control(enabled, cx);
    }

    pub(crate) fn automate_navigation(
        &mut self,
        command: BrowserAction,
        cx: &mut gpui::Context<Self>,
    ) {
        match command {
            BrowserAction::Reload => self.reload(cx),
            BrowserAction::Back => self.history(false),
            BrowserAction::Forward => self.history(true),
            _ => return,
        }
        cx.notify();
    }
    pub(crate) fn automate(&self, command: BrowserAction) -> BrowserWork {
        if !self.agent_control {
            return futures::future::ready(Err(
                "browser_disabled: agent control was turned off".into()
            ))
            .boxed();
        }
        #[cfg(target_os = "linux")]
        if let Some(native) = &self.native {
            return native.automate(command);
        }
        let _ = command;
        futures::future::ready(Err(
            "browser_unavailable: open the page in the Linux desktop browser first".into(),
        ))
        .boxed()
    }
}

pub(crate) fn checked_result(value: Value) -> Result<Value, String> {
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        Err(error.into())
    } else {
        Ok(value)
    }
}

pub(crate) fn success() -> BrowserWork {
    futures::future::ready(Ok(json!({"ok":true}))).boxed()
}
