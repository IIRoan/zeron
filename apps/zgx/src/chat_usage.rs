//! `chat.usage`, for the chat list's usage card (Linux fork:
//! plugins-fork/linux-workbench/screens/chat_usage.luau).
//!
//!   chat.usage { ids, terminals?, project?, checkout_shared?, window? }  ->  { cpu,
//!     memory, read, written, processes, threads, cores,
//!     groups = { { name, shared, cpu, memory, read, written, processes } },
//!     top = { { name, count, cpu, memory } } }
//!
//! A process is a group's by its `ZERON_ORIGIN` marker (`chat:<id>`,
//! `terminal:<id>`) or its nearest marked ancestor's; the Windows VM's by
//! winvm's pid files. Shared groups (the project's databases and VM, a
//! checkout's services other chats also use) are listed but kept out of the
//! totals and `top`. `cpu` is percent of one core; `memory` is Pss, or
//! `podman stats`' figure for a container. Nil off Linux.

#[cfg(target_os = "linux")]
mod groups;
#[cfg(target_os = "linux")]
mod scan;

use zgx_script::ScriptHost;

pub fn install(host: &ScriptHost) {
    host.async_action("chat.usage", |arg| async move {
        #[cfg(target_os = "linux")]
        {
            linux::usage(arg).await
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = arg;
            Ok(zgx_script::Value::Nil)
        }
    });
}

#[cfg(target_os = "linux")]
mod linux {
    use super::groups::{group, services, vm};
    use super::scan::{self, Amount, Group};
    use crate::engine;
    use serde_json::json;
    use zgx_script::Value;

    fn texts(arg: &Value, key: &str) -> Vec<String> {
        arg.field(key).map_or(&[][..], Value::list).iter().map(crate::auth::text).filter(|s| !s.is_empty()).collect()
    }

    async fn groups(arg: &Value) -> Vec<Group> {
        let ids = texts(arg, "ids");
        let mut groups = vec![group("This chat", false, ids.iter().map(|id| format!("chat:{id}")).collect(), vec![])];
        let terminals = texts(arg, "terminals");
        if !terminals.is_empty() {
            groups.push(group("Terminals", false, terminals.iter().map(|id| format!("terminal:{id}")).collect(), vec![]));
        }
        let project = arg.field("project").map(crate::auth::text).filter(|p| !p.is_empty());
        if let (Some(project), Some(chat)) = (project, ids.first()) {
            if let Ok(snapshot) = engine::get().call("GetProjectTerminals", json!({ "spaceId": project, "chatId": chat })).await {
                groups.extend(services(&snapshot, matches!(arg.field("checkout_shared"), Some(Value::Bool(true)))));
            }
            if let Ok(reply) = engine::get().call("GetProjectWindowsVm", json!({ "spaceId": project, "brief": true })).await
                && !reply["settings"].is_null()
            {
                groups.extend(vm());
            }
        }
        groups
    }

    fn amount(fields: &mut Vec<(&'static str, Value)>, a: &Amount) {
        fields.extend([
            ("cpu", Value::Num(a.cpu)),
            ("memory", Value::Num(a.memory as f64)),
            ("read", Value::Num(a.read)),
            ("written", Value::Num(a.written)),
            ("processes", Value::Num(a.processes as f64)),
        ]);
    }

    pub async fn usage(arg: Value) -> Result<Value, String> {
        let window = arg.field("window").and_then(Value::as_f64).unwrap_or(400.0).clamp(50.0, 5000.0);
        let groups = groups(&arg).await;
        // (measure sleeps through the window: off this thread.)
        let (tx, mut rx) = zgx::task::latest();
        std::thread::spawn(move || tx.send(scan::measure(&groups, std::time::Duration::from_millis(window as u64))));
        let usage = rx.recv().await.ok_or("failed")?;
        let mut fields = vec![("threads", Value::Num(usage.threads as f64)), ("cores", Value::Num(usage.cores as f64))];
        amount(&mut fields, &usage.total);
        let list = |items: Vec<Value>| Value::List(items.into());
        fields.push((
            "groups",
            list(usage
                .groups
                .iter()
                .map(|(g, a)| {
                    let mut f = vec![("name", Value::from(g.name.as_str())), ("shared", Value::Bool(g.shared))];
                    amount(&mut f, a);
                    Value::record(f)
                })
                .collect()),
        ));
        fields.push((
            "top",
            list(usage
                .top
                .iter()
                .map(|t| {
                    let mut f = vec![("name", Value::from(t.name.as_str())), ("count", Value::Num(t.count as f64))];
                    amount(&mut f, &t.amount);
                    Value::record(f)
                })
                .collect()),
        ));
        Ok(Value::record(fields))
    }
}
