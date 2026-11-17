//! (Linux fork) Agents driving the browser: the engine's `browser_*` tools,
//! hosted as the Linux desktop app hosted them
//! (crates/ui/src/shell/browser_control.rs). The window lists its browser
//! tabs to the engine (`UpdateBrowserHost`), takes each request the engine
//! routes to it (`AttachBrowserHost`, by tab and chat, only for a tab whose
//! agent control is on), hands it to the scripts that hold the pages
//! (ui/screens/panel/browser_agents.luau) and sends their answer back
//! (`ReplyBrowserHost`), the list sent again first so that the next call can
//! name a tab this one opened. A page that doesn't answer in time is said
//! to have timed out, as the desktop app said it.
//!
//! ```text
//! shared  browser_agents.requests   [{ id, chat, tab, command, automation? }]
//! action  browser_agents.tabs(list) [{ tab, chat, title, url?, loading, enabled }]
//! action  browser_agents.reply      { id, value?, error? }
//! ```
use std::{cell::RefCell, rc::Rc, time::Duration};

use serde_json::{Value as Json, json};
use zgx_desktop::{TaskSpawner, WindowContext};
use zgx_script::{ScriptHost, Shared, Value};

use crate::engine::rpc::{Rpc, Watch};
use crate::plugins::{json_value, value_json};

/// How long a page has to answer (the engine gives up at 20 s).
const ANSWER_WITHIN: Duration = Duration::from_secs(18);
/// The desktop app's page script: snapshot, click's point, fill, select, scroll.
const AUTOMATION: &str = include_str!("browser_automation.js");

struct Host {
    /// This window's id with the engine; a tab's is `<id>/<tab>`.
    id: String,
    revision: u64,
    tabs: Vec<Json>,
    rpc: Option<Rpc>,
    attach: Option<Watch>,
    /// Requests handed to the scripts, not yet answered.
    waiting: Vec<Json>,
    requests: Shared,
    tasks: TaskSpawner,
}

type Me = Rc<RefCell<Host>>;

/// The main window hosts its tabs (Linux, as the desktop app did).
pub fn install(cx: &mut WindowContext, host: &ScriptHost) {
    if !cfg!(target_os = "linux") {
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let me: Me = Rc::new(RefCell::new(Host {
        id: format!("zgx-{}-{nanos:x}", std::process::id()),
        revision: 0,
        tabs: Vec::new(),
        rpc: None,
        attach: None,
        waiting: Vec::new(),
        requests: host.shared("browser_agents.requests", json_value(&json!([]))),
        tasks: cx.tasks.clone(),
    }));
    let tabs = me.clone();
    host.action("browser_agents.tabs", move |list| heard_tabs(&tabs, &list));
    let replies = me.clone();
    host.action("browser_agents.reply", move |answer| answered(&replies, &answer));
    let weak = Rc::downgrade(&me);
    crate::engine::get().on_connect(move |rpc| {
        let Some(me) = weak.upgrade() else { return };
        let id = me.borrow().id.clone();
        let (on_item, on_end) = (Rc::downgrade(&me), Rc::downgrade(&me));
        let rpc_for_items = rpc.clone();
        let watch = rpc.watch_ending(
            "AttachBrowserHost",
            json!({ "hostId": id }),
            move |item| {
                if let Some(me) = on_item.upgrade() {
                    heard_request(&me, &rpc_for_items, item);
                }
            },
            move |_| {
                if let Some(me) = on_end.upgrade() {
                    let mut host = me.borrow_mut();
                    host.rpc = None;
                    host.waiting.clear();
                    host.requests.set(json_value(&json!([])));
                }
            },
        );
        me.borrow_mut().attach = Some(watch);
    });
}

/// The scripts' list of the window's tabs: sent when it changed.
fn heard_tabs(me: &Me, list: &Value) {
    let mut host = me.borrow_mut();
    let mut tabs: Vec<Json> = match value_json(list) {
        Json::Array(items) => items
            .iter()
            .filter_map(|t| {
                let (tab, chat) = (t["tab"].as_str()?, t["chat"].as_str()?);
                Some(json!({
                    "id": format!("{}/{tab}", host.id),
                    "chatId": chat,
                    "title": t["title"].as_str().unwrap_or("").chars().take(2000).collect::<String>(),
                    "url": t["url"].as_str(),
                    "loading": t["loading"].as_bool().unwrap_or(false),
                    "enabled": t["enabled"].as_bool().unwrap_or(false),
                }))
            })
            .collect(),
        _ => return,
    };
    tabs.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    if tabs == host.tabs {
        return;
    }
    host.tabs = tabs;
    host.revision += 1;
    drop(host);
    publish(me);
}

/// The list to the engine, as it is when this runs.
fn publish(me: &Me) {
    let host = me.borrow();
    let Some(rpc) = host.rpc.clone() else { return };
    let params = json!({ "hostId": host.id, "revision": host.revision, "tabs": host.tabs });
    host.tasks.spawn(async move {
        let _ = rpc.call("UpdateBrowserHost", params).await;
    });
}

/// What the engine sent: that it's ready (the list goes), or a request.
fn heard_request(me: &Me, rpc: &Rpc, item: Json) {
    if item["ready"] == true {
        let mut host = me.borrow_mut();
        host.rpc = Some(rpc.clone());
        host.revision += 1;
        drop(host);
        publish(me);
        return;
    }
    let (Some(request_id), request) = (item["requestId"].as_str().map(str::to_owned), &item["request"]) else {
        return;
    };
    let host_id = me.borrow().id.clone();
    let Some(tab) = request["browserId"]
        .as_str()
        .and_then(|b| b.strip_prefix(&format!("{host_id}/")))
        .map(str::to_owned)
    else {
        send_reply(me, request_id, Err("browser_not_found: tab belongs to another desktop".into()));
        return;
    };
    let mut command = request.clone();
    if let Some(fields) = command.as_object_mut() {
        fields.remove("chatId");
        fields.remove("browserId");
    }
    let action = command["action"].as_str().unwrap_or("").to_owned();
    let mut entry = json!({
        "id": request_id,
        "chat": request["chatId"],
        "tab": tab,
        "command": command,
    });
    if matches!(action.as_str(), "snapshot" | "click" | "fill" | "select" | "scroll") {
        entry["automation"] = AUTOMATION.replace("__ZERON_COMMAND__", &command.to_string()).into();
    }
    {
        let mut host = me.borrow_mut();
        host.waiting.push(entry);
        let list = Json::Array(host.waiting.clone());
        host.requests.set(json_value(&list));
    }
    // A page that never answers (a process gone, a dialog) still gets one.
    let weak = Rc::downgrade(me);
    let tasks = me.borrow().tasks.clone();
    tasks.spawn(async move {
        zgx::timer::sleep(ANSWER_WITHIN).await;
        if let Some(me) = weak.upgrade()
            && take(&me, &request_id).is_some()
        {
            send_reply(&me, request_id, Err("browser_timeout: page did not respond".into()));
        }
    });
}

/// The request `id`, if it still waits: no longer.
fn take(me: &Me, id: &str) -> Option<Json> {
    let mut host = me.borrow_mut();
    let at = host.waiting.iter().position(|r| r["id"] == id)?;
    let request = host.waiting.remove(at);
    let list = Json::Array(host.waiting.clone());
    host.requests.set(json_value(&list));
    Some(request)
}

/// The scripts' answer to a request.
fn answered(me: &Me, answer: &Value) {
    let answer = value_json(answer);
    let Some(id) = answer["id"].as_str() else { return };
    let Some(request) = take(me, id) else { return };
    let result = match answer["error"].as_str() {
        Some(error) => Err(error.to_owned()),
        None => {
            let value = answer.get("value").cloned().unwrap_or(Json::Null);
            // A tab an agent opened, by the id the next call names it by.
            if request["command"]["action"] == "new_tab"
                && let Some(tab) = value["tab"].as_str()
            {
                let host_id = me.borrow().id.clone();
                Ok(json!({ "browserId": format!("{host_id}/{tab}"), "loading": true }))
            } else {
                Ok(value)
            }
        }
    };
    send_reply(me, id.to_owned(), result);
}

/// The answer to the engine, the tab list sent again first (as it is then).
fn send_reply(me: &Me, request_id: String, mut result: Result<Json, String>) {
    let weak = Rc::downgrade(me);
    let tasks = me.borrow().tasks.clone();
    tasks.spawn(async move {
        // (After this turn: the scripts' list follows what they just did.)
        zgx::timer::sleep(Duration::from_millis(30)).await;
        let Some(me) = weak.upgrade() else { return };
        let (rpc, host_id, inventory) = {
            let host = me.borrow();
            let Some(rpc) = host.rpc.clone() else { return };
            let inventory = json!({ "hostId": host.id, "revision": host.revision, "tabs": host.tabs });
            (rpc, host.id.clone(), inventory)
        };
        if let Err(error) = rpc.call("UpdateBrowserHost", inventory).await {
            result = Err(format!("browser_no_host: {error}"));
        }
        let result = match result {
            Ok(value) => json!({ "Ok": value }),
            Err(error) => json!({ "Err": error }),
        };
        let _ = rpc
            .call("ReplyBrowserHost", json!({ "hostId": host_id, "requestId": request_id, "result": result }))
            .await;
    });
}
