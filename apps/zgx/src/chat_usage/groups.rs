//! Services, databases and the Windows VM as groups to measure. serde_json
//! and libc only, so it tests apart from the app.
use super::scan::Group;
use serde_json::Value as Json;

pub fn group(name: &str, shared: bool, origins: Vec<String>, pids: Vec<u32>) -> Group {
    Group { name: name.into(), shared, origins, pids }
}

/// The services and databases running for the chat's checkout, by their
/// runs' terminals. Databases are always shared, services when
/// `checkout_shared`.
pub fn services(snapshot: &Json, checkout_shared: bool) -> Vec<Group> {
    let config = snapshot["config"]["services"].as_array().cloned().unwrap_or_default();
    snapshot["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|run| {
            let terminal = run["terminal"]["id"].as_str().filter(|id| !id.is_empty())?;
            let service = config.iter().find(|s| s["id"] == run["serviceId"]);
            let name = service.and_then(|s| s["name"].as_str()).or(run["serviceId"].as_str()).unwrap_or("Service");
            // (A database is a service with a setup prerequisite: one run
            // for all the project's checkouts.)
            let database = service.is_some_and(|s| !s["setupPrerequisite"].is_null());
            Some(group(name, database || checkout_shared, vec![format!("terminal:{terminal}")], vec![]))
        })
        .collect()
}

/// The Windows VM's processes from winvm's pid files, each only while it's
/// still winvm's (a stale pid may be another process's by now).
pub fn vm() -> Option<Group> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| format!("/run/user/{}", unsafe { libc::getuid() }).into())
        .join("winvm");
    let alive = |name: &str| -> Option<u32> {
        let pid: u32 = std::fs::read_to_string(dir.join(format!("{name}.pid"))).ok()?.trim().parse().ok()?;
        let args = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        args.windows(5).any(|w| w == b"winvm").then_some(pid)
    };
    let qemu = alive("qemu")?;
    let mut pids = vec![qemu];
    pids.extend(["swtpm", "passt", "tunnel", "viewer"].into_iter().filter_map(alive));
    Some(group("Windows VM", true, vec![], pids))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn services_and_the_project_s_databases_are_their_runs_terminals() {
        let snapshot = json!({
            "config": { "services": [
                { "id": "api", "name": "Backend" },
                { "id": "db", "name": "Databases", "setupPrerequisite": { "command": "true" } },
            ] },
            "runs": [
                { "serviceId": "db", "status": "running", "terminal": { "id": "t-db" } },
                { "serviceId": "api", "status": "running", "terminal": { "id": "t-api" } },
                { "serviceId": "api", "status": "stopped" },
            ],
        });
        assert_eq!(
            services(&snapshot, false),
            [
                group("Databases", true, vec!["terminal:t-db".into()], vec![]),
                group("Backend", false, vec!["terminal:t-api".into()], vec![]),
            ]
        );
        // Another chat in the same checkout: its services are that chat's too.
        assert!(services(&snapshot, true).iter().all(|g| g.shared));
    }
}
