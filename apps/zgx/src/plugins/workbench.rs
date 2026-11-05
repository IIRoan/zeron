//! Project tools use the fork's existing engine implementations. A plugin
//! receives only these named operations, in a chat/project scope resolved
//! from the host's records. No RPC token or unrestricted engine call is lent.
use serde_json::{Value as Json, json};

use super::Item;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Git,
    Services,
    ServiceTerminal,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub kind: Kind,
    pub method: String,
    pub params: Json,
    pub device: String,
    pub service: Option<String>,
}

fn kind(method: &str) -> Result<Kind, String> {
    Ok(match method {
        "GetProjectTerminals" | "SaveProjectTerminals" | "ControlProjectTerminals"
        | "GetWorktreeSettings" | "SaveWorktreeSettings" | "GetWorktreeSetupLog"
        | "PrepareWorktree" | "CancelWorktreeSetup" | "DeleteWorktree" => Kind::Services,
        "ProjectServiceTerminal" => Kind::ServiceTerminal,
        "HighlightGitDiff" | "GetCheckoutChanges" | "GetCheckoutChangeDiff" | "GetCheckoutDiff"
        | "GetCheckoutFileDiffText" | "GetCheckoutGitDetails" | "GetCheckoutBranches"
        | "GetCheckoutStashes" | "GetCheckoutConflictMarkers" | "SetCheckoutStaged"
        | "CommitCheckoutStaged" | "RunCheckoutGitAction" | "PreviewCheckoutDiscard"
        | "DiscardCheckoutChanges" | "RestoreCheckoutDiscard" | "ListGitHistory"
        | "SearchGitHistory" | "ListRefs" => Kind::Git,
        _ => return Err("That operation isn't available to workbench plugins".into()),
    })
}

pub fn shell_allowed(item: &Item) -> bool {
    item.may("commands")
        && item.json["commands"].as_array().into_iter().flatten().any(|p| p == "sh")
}

fn permitted(item: &Item, kind: Kind) -> bool {
    let program = if kind == Kind::Git { "git" } else { "sh" };
    item.may("sessions.read") && item.may("files.read") && item.may("commands")
        && item.json["commands"].as_array().into_iter().flatten().any(|p| p == program)
}

fn row<'a>(records: &'a Json, list: &str, id: &str) -> Result<&'a Json, String> {
    records[list].as_array().into_iter().flatten().find(|r| r["id"] == id)
        .ok_or_else(|| format!("That {list} entry is no longer available"))
}

fn nonempty<'a>(value: &'a Json, key: &str) -> Result<&'a str, String> {
    value[key].as_str().filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("This project has no {key}"))
}

/// Authorize first, then overwrite all routing fields with the host's
/// current scope. A captured scope rejects a worktree/device switch even
/// while a popup from the previous scope is still open.
pub fn prepare(item: &Item, records: &Json, asked: &Json) -> Result<Request, String> {
    let method = nonempty(asked, "method")?;
    let kind = kind(method)?;
    if !permitted(item, kind) {
        return Err("This plugin wasn't allowed to use these project tools".into());
    }
    let session = asked["session"].as_str().filter(|s| !s.is_empty());
    let chat = session.map(|id| row(records, "chats", id)).transpose()?;
    let project_id = chat.and_then(|c| c["project"].as_str())
        .or_else(|| asked["project"].as_str().filter(|s| !s.is_empty()));
    if chat.is_some() && asked["project"].as_str().is_some_and(|id| Some(id) != project_id) {
        return Err("The chat belongs to another project".into());
    }
    let project = project_id.map(|id| row(records, "projects", id)).transpose()?;
    if kind != Kind::Git && project.is_none() {
        return Err("Choose a project to manage its services".into());
    }
    let source = chat.or(project).ok_or("Choose a chat or project first")?;
    let device = nonempty(source, "device")?;
    if project.is_some_and(|p| p["device"].as_str() != Some(device)) {
        return Err("The chat belongs to another project's device".into());
    }
    let cwd = chat.and_then(|c| c["cwd"].as_str()).filter(|s| !s.is_empty())
        .or_else(|| project.and_then(|p| p["path"].as_str()).filter(|s| !s.is_empty()))
        .ok_or("This chat has no checkout")?;
    for (key, now) in [("cwd", cwd), ("device", device), ("project", project_id.unwrap_or(""))] {
        if asked["scope"][key].as_str().is_some_and(|was| was != now) {
            return Err("The project or checkout changed; refresh and try again".into());
        }
    }
    let mut params = match &asked["params"] {
        Json::Null => json!({}),
        Json::Array(items) if items.is_empty() => json!({}),
        Json::Object(_) => asked["params"].clone(),
        _ => return Err("Project tool parameters must be an object".into()),
    };
    let object = params.as_object_mut().expect("checked object");
    let selected_checkout = object.get("checkoutPath").and_then(Json::as_str).map(str::to_owned);
    for key in ["chatId", "spaceId", "cwd", "repoPath", "checkoutPath", "targetDeviceId"] {
        object.remove(key);
    }
    object.insert("cwd".into(), json!(cwd));
    object.insert("targetDeviceId".into(), json!(device));
    if let Some(id) = session { object.insert("chatId".into(), json!(id)); }
    if let Some(id) = project_id { object.insert("spaceId".into(), json!(id)); }
    // Project settings address defaults. Only a chat in a different
    // checkout addresses an override; passing the main folder here would
    // create a main-folder override instead of updating project defaults.
    if matches!(method, "GetWorktreeSettings" | "SaveWorktreeSettings" | "GetWorktreeSetupLog" | "PrepareWorktree" | "CancelWorktreeSetup") {
        if let Some(selected) = selected_checkout.filter(|s| !s.is_empty()) {
            if session.is_some() && selected != cwd {
                return Err("Choose project settings to edit another checkout".into());
            }
            // The engine resolves this against the selected project's registered
            // checkouts, rejecting paths owned by another project.
            object.insert("checkoutPath".into(), json!(selected));
        } else if project.is_some_and(|p| p["path"].as_str() != Some(cwd)) {
            object.insert("checkoutPath".into(), json!(cwd));
        }
    }
    if method == "DeleteWorktree" {
        let root = project.and_then(|p| p["path"].as_str()).ok_or("Choose a project first")?;
        let path = object.get("worktreePath").and_then(Json::as_str).filter(|p| !p.is_empty()).ok_or("Choose a worktree first")?;
        if path == root { return Err("The main project folder cannot be removed".into()); }
        object.insert("repoPath".into(), json!(root));
        object.insert("deleteBranch".into(), json!(false));
        object.insert("archiveSessions".into(), json!(true));
    }
    if session.is_none() && matches!(method,"GetProjectTerminals"|"SaveProjectTerminals"|"ControlProjectTerminals") {
        if let Some(selected)=asked["params"]["checkoutPath"].as_str().filter(|p| !p.is_empty()) {
            // As above, the engine checks that the checkout belongs to this project.
            object.insert("checkoutPath".into(),json!(selected));
        }
    }
    if object.get("settings").is_some_and(Json::is_object)
        && object["settings"]["variables"].as_array().is_some_and(Vec::is_empty) {
        object.get_mut("settings").unwrap()["variables"] = json!({});
    }
    if matches!(method, "ListGitHistory" | "SearchGitHistory" | "ListRefs") {
        object.insert("repoPath".into(), json!(cwd));
    }
    let service = params["serviceId"].as_str().map(str::to_owned);
    if kind == Kind::ServiceTerminal && service.as_ref().is_none_or(|id| id.is_empty()) {
        return Err("Choose a service to show its output".into());
    }
    Ok(Request { kind, method: if kind == Kind::ServiceTerminal { "GetProjectTerminals".into() } else { method.into() },
                 params, device: device.into(), service })
}

/// Only a configured service in the resolved project may lend a terminal.
pub fn terminal(snapshot: &Json, service: &str) -> Result<String, String> {
    if !snapshot["config"]["services"].as_array().into_iter().flatten().any(|s| s["id"] == service) {
        return Err("That service is no longer configured".into());
    }
    snapshot["runs"].as_array().into_iter().flatten().find(|r| r["serviceId"] == service)
        .and_then(|r| r["terminal"]["id"].as_str()).filter(|id| !id.is_empty()).map(str::to_owned)
        .ok_or_else(|| "Start this service to show its output".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(program: &str) -> Item {
        Item { json: json!({"id":"test","status":"on","permissions":["commands","sessions.read","files.read"],"commands":[program]}) }
    }
    fn records() -> Json {
        json!({"chats":[{"id":"c","project":"p","device":"remote","cwd":"/work/tree"}],
               "projects":[{"id":"p","device":"remote","path":"/work/main"}]})
    }
    #[test]
    fn routing_comes_from_host_records_and_keeps_git_preconditions() {
        let r=prepare(&item("git"),&records(),&json!({"method":"RunCheckoutGitAction","session":"c",
            "params":{"cwd":"/private","targetDeviceId":"other","spaceId":"other","chatId":"other","expectedHead":"abc","expectedBranch":"topic","action":{"kind":"fetch"}}})).unwrap();
        assert_eq!(r.params["cwd"],"/work/tree"); assert_eq!(r.params["targetDeviceId"],"remote");
        assert_eq!(r.params["spaceId"],"p"); assert_eq!(r.params["chatId"],"c");
        assert_eq!(r.params["expectedHead"],"abc"); assert_eq!(r.params["expectedBranch"],"topic");
    }
    #[test]
    fn git_grant_cannot_control_services_or_call_admin_methods() {
        for method in ["ControlProjectTerminals","SignOut","ReadWorkspaceFile","Mutate","PluginAllow"] {
            assert!(prepare(&item("git"),&records(),&json!({"method":method,"session":"c"})).is_err(),"{method}");
        }
    }
    #[test]
    fn missing_permission_or_disabled_plugin_is_refused() {
        for permission in ["commands","sessions.read","files.read"] {
            let mut p=item("git");
            p.json["permissions"].as_array_mut().unwrap().retain(|v| v!=permission);
            assert!(prepare(&p,&records(),&json!({"method":"GetCheckoutChanges","session":"c"})).is_err());
        }
        let mut p=item("sh"); p.json["status"]=json!("off");
        assert!(prepare(&p,&records(),&json!({"method":"GetProjectTerminals","session":"c"})).is_err());
    }
    #[test]
    fn stale_scope_and_unknown_or_mismatched_project_are_refused() {
        for ask in [json!({"method":"GetProjectTerminals","session":"missing"}),
                    json!({"method":"GetProjectTerminals","session":"c","project":"other"}),
                    json!({"method":"GetProjectTerminals","session":"c","scope":{"cwd":"/old"}})] {
            assert!(prepare(&item("sh"),&records(),&ask).is_err());
        }
        let mut r=records(); r["projects"][0]["device"]=json!("other");
        assert!(prepare(&item("sh"),&r,&json!({"method":"GetProjectTerminals","session":"c"})).is_err());
    }
    #[test]
    fn project_only_services_are_bound_to_the_main_folder() {
        let r=prepare(&item("sh"),&records(),&json!({"method":"GetWorktreeSettings","project":"p"})).unwrap();
        assert_eq!(r.params["spaceId"],"p"); assert_eq!(r.params["cwd"],"/work/main");
        assert!(r.params.get("checkoutPath").is_none()); assert!(r.params.get("chatId").is_none());
    }
    #[test]
    fn chat_services_use_the_chat_checkout_without_a_space_override() {
        let r=prepare(&item("sh"),&records(),&json!({"method":"GetProjectTerminals","session":"c","params":[]})).unwrap();
        assert_eq!(r.params["chatId"],"c");
        assert!(r.params.get("checkoutPath").is_none());
    }
    #[test]
    fn worktree_settings_preserve_default_and_override_scopes_and_empty_maps() {
        let r=prepare(&item("sh"),&records(),&json!({"method":"SaveWorktreeSettings","session":"c","params":{"settings":{"variables":[]}}})).unwrap();
        assert_eq!(r.params["checkoutPath"],"/work/tree");assert_eq!(r.params["settings"]["variables"],json!({}));
        let mut main=records();main["chats"][0]["cwd"]=json!("/work/main");
        let r=prepare(&item("sh"),&main,&json!({"method":"SaveWorktreeSettings","session":"c"})).unwrap();
        assert!(r.params.get("checkoutPath").is_none());
    }
    #[test]
    fn output_cannot_attach_to_an_unrelated_terminal() {
        let snapshot=json!({"config":{"services":[{"id":"api"}]},"runs":[{"serviceId":"api","terminal":{"id":"pty-1"}},{"serviceId":"private","terminal":{"id":"pty-private"}}]});
        assert_eq!(terminal(&snapshot,"api").unwrap(),"pty-1");
        assert!(terminal(&snapshot,"private").is_err());
    }

    #[test]
    fn project_settings_select_checkouts_without_retargeting_chat_actions() {
        let ask=json!({"method":"GetWorktreeSettings","project":"p","params":{"checkoutPath":"/work/other"}});
        assert_eq!(prepare(&item("sh"),&records(),&ask).unwrap().params["checkoutPath"],"/work/other");
        let mut chat=ask;chat["session"]=json!("c");
        assert!(prepare(&item("sh"),&records(),&chat).is_err());
    }

    #[test]
    fn worktree_removal_keeps_the_branch_and_binds_the_project_root() {
        let ask=json!({"method":"DeleteWorktree","project":"p","params":{"repoPath":"/private","worktreePath":"/work/linked","deleteBranch":true}});
        let params=prepare(&item("sh"),&records(),&ask).unwrap().params;
        assert_eq!(params["repoPath"],"/work/main");assert_eq!(params["deleteBranch"],false);assert_eq!(params["archiveSessions"],true);
        let mut main=ask;main["params"]["worktreePath"]=json!("/work/main");
        assert!(prepare(&item("sh"),&records(),&main).is_err());
        assert!(prepare(&item("git"),&records(),&main).is_err());
    }

    fn run_fork_script(name: &str) {
        use zgx_script::{ScriptHost, testing::TestApp};
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins-fork/zeron-git");
        let root = std::env::temp_dir().join(format!("zeron-fork-{name}-{}",std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for name in ["selection.luau","staging.luau","workbench.luau"] {
            std::fs::copy(source.join(name),root.join(name)).unwrap();
        }
        let script = std::fs::read_to_string(source.join("tests").join(format!("{name}.luau"))).unwrap();
        let entry = root.join("app.luau");
        std::fs::write(&entry,format!("{script}\nreturn ui.text {{ id='passed', 'passed' }}")).unwrap();
        let mut app = TestApp::new(move |runtime| ScriptHost::new(runtime,&entry));
        app.frames(2);
        assert_eq!(app.error(),None);
        assert_eq!(app.text("passed"),"passed");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn fork_selection_ranges_mixed_sides_and_staging_follow_focus() {
        run_fork_script("selection");
    }
    #[test]
    fn fork_staging_projects_add_delete_and_partial_files_but_not_gitlinks() {
        run_fork_script("staging");
    }
    #[test]
    fn fork_staging_queue_reconciles_replies_and_failed_batches() {
        run_fork_script("queue");
    }
}
