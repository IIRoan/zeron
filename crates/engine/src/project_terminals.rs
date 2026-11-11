//! Private project service profiles over the existing PTY engine. Services are
//! keyed by project + stable service id, not by chat or the viewing window.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use zeron_proto::{
    ProjectTerminalConfig, ProjectTerminalRun, ProjectTerminalService,
    ProjectTerminalStatus as Status, ProjectTerminalsSnapshot, ServiceReadinessState as Readiness,
    TerminalEvent,
};

use crate::{EngineError, Terminals};

mod startup;

const MAX_SERVICES: usize = 12;
const MAX_RESTARTS: u32 = 3;
const RESTART_BUDGET_RESET: Duration = Duration::from_secs(60);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    root: String,
    config: ProjectTerminalConfig,
    #[serde(default)]
    checkout: Option<String>,
    #[serde(default)]
    runs: Vec<ProjectTerminalRun>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    environment: HashMap<String, String>,
}

struct LiveRun {
    token: CancellationToken,
    terminals: Terminals,
    terminal_id: String,
}

struct Inner {
    path: PathBuf,
    projects: Mutex<BTreeMap<String, Project>>,
    live: Mutex<HashMap<(String, String), LiveRun>>,
    operations: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    managed_operations: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    startups: Mutex<HashMap<String, startup::Startup>>,
    readiness_checks: Mutex<HashMap<String, startup::ReadinessCheck>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for run in self
            .live
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            run.token.cancel();
            let _ = run.terminals.close(&run.terminal_id);
        }
    }
}

#[derive(Clone)]
pub struct ProjectTerminals {
    inner: Arc<Inner>,
}

impl ProjectTerminals {
    pub fn open(directory: &Path) -> Result<Self, EngineError> {
        let path = directory.join("project-terminals.json");
        let mut projects: BTreeMap<String, Project> = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(projects) => projects,
                Err(error) => {
                    // A damaged file must not keep the engine from starting.
                    // Set it aside for recovery rather than overwriting it.
                    let aside = path.with_file_name(format!(
                        "project-terminals.json.invalid-{}",
                        chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
                    ));
                    std::fs::rename(&path, &aside)?;
                    tracing::warn!(%error, preserved = %aside.display(), "project terminal profiles were unreadable; starting with none");
                    BTreeMap::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };
        // Worktree service groups outlive folders deleted outside Zeron.
        projects.retain(|_, project| {
            project.parent.is_none()
                || project
                    .checkout
                    .as_deref()
                    .is_some_and(|checkout| Path::new(checkout).is_dir())
        });
        for project in projects.values_mut() {
            for run in &mut project.runs {
                if matches!(run.status, Status::Running | Status::Restarting) {
                    run.status = Status::Interrupted;
                    run.message = Some(
                        "The engine stopped while this service was running. Start it to resume."
                            .into(),
                    );
                }
                run.terminal = None;
            }
        }
        let store = Self {
            inner: Arc::new(Inner {
                path,
                projects: Mutex::new(projects),
                live: Mutex::new(HashMap::new()),
                operations: Mutex::new(HashMap::new()),
                managed_operations: Mutex::new(HashMap::new()),
                startups: Mutex::new(HashMap::new()),
                readiness_checks: Mutex::new(HashMap::new()),
            }),
        };
        store.persist()?;
        Ok(store)
    }

    fn persist(&self) -> Result<(), EngineError> {
        let projects = self
            .inner
            .projects
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.write(&projects)
    }

    fn write(&self, projects: &BTreeMap<String, Project>) -> Result<(), EngineError> {
        let directory = self.inner.path.parent().expect("profile directory");
        std::fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        // NamedTempFile is private (0600 on Unix); profile commands may contain secrets.
        file.write_all(
            &serde_json::to_vec_pretty(projects).map_err(|e| EngineError::Other(e.to_string()))?,
        )?;
        file.as_file().sync_all()?;
        file.persist(&self.inner.path).map_err(|e| e.error)?;
        Ok(())
    }

    pub fn snapshot(
        &self,
        space: &str,
        root: &Path,
    ) -> Result<ProjectTerminalsSnapshot, EngineError> {
        let root = canonical_root(root)?;
        let projects = self
            .inner
            .projects
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(project) = projects.get(space) else {
            return Ok(ProjectTerminalsSnapshot {
                startups: self.startup_progress(space),
                config: ProjectTerminalConfig::default(),
                runs: Vec::new(),
                checkout: None,
            });
        };
        check_root(project, &root)?;
        Ok(ProjectTerminalsSnapshot {
            startups: self.startup_progress(space),
            config: project.config.clone(),
            runs: project.runs.clone(),
            checkout: project.checkout.clone(),
        })
    }

    pub fn save(
        &self,
        space: &str,
        root: &Path,
        config: ProjectTerminalConfig,
    ) -> Result<ProjectTerminalsSnapshot, EngineError> {
        let operation = self.operation(space);
        let _guard = operation.lock().unwrap_or_else(|e| e.into_inner());
        let root_path = root;
        let root = canonical_root(root)?;
        let config = normalize_config(config)?;
        let removed = {
            let projects = self
                .inner
                .projects
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(project) = projects.get(space) {
                check_root(project, &root)?;
                project
                    .config
                    .services
                    .iter()
                    .filter(|service| !config.services.iter().any(|s| s.id == service.id))
                    .map(|s| s.id.clone())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        };
        // A failed stop must keep the profile available for a retry.
        for service in removed {
            self.stop(space, &service)?;
        }
        {
            let mut projects = self
                .inner
                .projects
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut next = projects.clone();
            let project = next.entry(space.to_string()).or_insert(Project {
                parent: None,
                environment: HashMap::new(),
                root,
                checkout: None,
                config: ProjectTerminalConfig::default(),
                runs: Vec::new(),
            });
            project.config = config;
            project.runs.retain(|run| {
                project
                    .config
                    .services
                    .iter()
                    .any(|s| s.id == run.service_id)
            });
            self.write(&next)?;
            *projects = next;
        }
        self.snapshot(space, root_path)
    }

    /// Preflight every selected directory before starting any service. The live
    /// registry lock also makes repeated/concurrent Start requests idempotent.
    pub fn control(
        &self,
        terminals: &Terminals,
        space: &str,
        root: &Path,
        service_id: Option<&str>,
        action: &str,
    ) -> Result<ProjectTerminalsSnapshot, EngineError> {
        self.control_in_checkout(terminals, space, root, None, service_id, action)
    }

    fn operation(&self, space: &str) -> Arc<Mutex<()>> {
        self.inner
            .operations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(space.into())
            .or_default()
            .clone()
    }

    /// The RPC layer verifies `checkout` against this project's chat and Git
    /// worktree list. Serialize the entire handoff, including process teardown.
    pub fn control_in_checkout(
        &self,
        terminals: &Terminals,
        space: &str,
        root: &Path,
        checkout: Option<&Path>,
        service_id: Option<&str>,
        action: &str,
    ) -> Result<ProjectTerminalsSnapshot, EngineError> {
        let operation = self.operation(space);
        let _guard = operation.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(action, "start" | "stop" | "restart" | "activate") {
            return Err(EngineError::Other("Unknown terminal action".into()));
        }
        let snapshot = self.snapshot(space, root)?;
        let checkout = if action == "stop" {
            None
        } else {
            Some(canonical_root(checkout.unwrap_or_else(|| {
                snapshot.checkout.as_deref().map(Path::new).unwrap_or(root)
            }))?)
        };
        let changed = checkout.as_ref().is_some_and(|checkout| {
            snapshot
                .checkout
                .as_deref()
                .unwrap_or_else(|| root.to_str().unwrap_or_default())
                != checkout
        });
        let running = snapshot
            .runs
            .iter()
            .filter(|run| matches!(run.status, Status::Running | Status::Restarting))
            .map(|run| run.service_id.clone())
            .collect::<HashSet<_>>();
        let services = snapshot
            .config
            .services
            .into_iter()
            .filter(|s| {
                (changed && running.contains(&s.id))
                    || (action != "activate" && service_id.is_none_or(|id| id == s.id))
            })
            .collect::<Vec<_>>();
        if services.is_empty() && action != "activate" {
            return Err(EngineError::Other(
                "No saved terminal services selected".into(),
            ));
        }
        if changed {
            for id in &running {
                self.stop(space, id)?;
            }
        }
        if let Some(checkout) = &checkout {
            let mut projects = self
                .inner
                .projects
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(project) = projects.get_mut(space) {
                project.checkout = Some(checkout.clone());
                self.write(&projects)?;
            }
        }
        let directories = if action != "stop" {
            services
                .iter()
                .map(|s| {
                    service_directory(
                        Path::new(checkout.as_deref().expect("start checkout")),
                        &s.directory,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        if action == "stop" || (action == "restart" && !changed) {
            for service in &services {
                self.stop(space, &service.id)?;
            }
        }
        if action != "stop" {
            for (service, directory) in services.into_iter().zip(directories) {
                self.start(
                    terminals,
                    space,
                    root,
                    Path::new(checkout.as_deref().expect("start checkout")),
                    service,
                    directory,
                )?;
            }
        }
        self.snapshot(space, root)
    }

    fn start(
        &self,
        terminals: &Terminals,
        space: &str,
        root: &Path,
        checkout: &Path,
        service: ProjectTerminalService,
        directory: PathBuf,
    ) -> Result<(), EngineError> {
        let key = (space.to_string(), service.id.clone());
        // The first lookup can wait for the user's shell; never under `live`.
        warm_login_path();
        let mut live = self.inner.live.lock().unwrap_or_else(|e| e.into_inner());
        if live.contains_key(&key) {
            return Ok(());
        }
        let current = self.snapshot(space, root)?;
        if !current
            .config
            .services
            .iter()
            .any(|saved| saved == &service)
        {
            return Err(EngineError::Other(
                "The terminal profile changed while starting. Try Start again.".into(),
            ));
        }
        // Retire exited buffers when restarting manually; running buffers are
        // released only by Stop. This avoids exhausting the shared PTY limit.
        if let Some(old) = current
            .runs
            .into_iter()
            .find(|r| r.service_id == service.id)
            && let Some(terminal) = old.terminal
        {
            let _ = terminals.close(&terminal.id);
        }
        let mut environment = self.inner.projects.lock().unwrap_or_else(|e| e.into_inner()).get(space).map(|p| p.environment.clone()).unwrap_or_default();
        environment.extend([
            ("ZERON_PROJECT_ROOT".into(), canonical_root(root)?),
            ("ZERON_CHECKOUT_ROOT".into(), canonical_root(checkout)?),
        ]);
        // Shared databases stay on the host in a sandboxed project.
        let open = match service.is_database() {
            true => Terminals::open_host_service,
            false => Terminals::open_service,
        };
        let session = match open(
            terminals,
            &directory.to_string_lossy(),
            &environment,
            &service.command,
        ) {
            Ok(session) => session,
            Err(error) => {
                self.update(
                    space,
                    ProjectTerminalRun {
                        service_id: service.id,
                        status: Status::Failed,
                        exit_code: None,
                        restarts: 0,
                        message: Some(error.to_string()),
                        terminal: None,
                        readiness: None,
                    },
                );
                return Err(error);
            }
        };
        let token = CancellationToken::new();
        live.insert(
            key.clone(),
            LiveRun {
                token: token.clone(),
                terminals: terminals.clone(),
                terminal_id: session.id.clone(),
            },
        );
        self.update(
            space,
            ProjectTerminalRun {
                service_id: service.id.clone(),
                status: Status::Running,
                exit_code: None,
                restarts: 0,
                message: None,
                terminal: Some(session.clone()),
                readiness: None,
            },
        );
        let weak = Arc::downgrade(&self.inner);
        let terminals = terminals.clone();
        tokio::spawn(supervise(
            weak,
            terminals,
            key,
            service,
            directory,
            environment,
            session,
            token,
        ));
        Ok(())
    }

    fn stop(&self, space: &str, service: &str) -> Result<(), EngineError> {
        let key: (String, String) = (space.into(), service.into());
        // Cancelling under the lock keeps the supervisor from restarting the
        // process. The wait happens without it, so other services can still
        // start, exit and stop during the grace period.
        let run = {
            let mut live = self.inner.live.lock().unwrap_or_else(|e| e.into_inner());
            let run = live.remove(&key);
            if let Some(run) = &run {
                run.token.cancel();
            }
            run
        };
        if let Some(run) = run
            && let Err(error) = run.terminals.close_service_and_wait(&run.terminal_id)
        {
            // Keep the process registered so Stop can retry.
            self.inner
                .live
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(key)
                .or_insert(run);
            return Err(error);
        }
        let mut projects = self
            .inner
            .projects
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(project) = projects.get_mut(space)
            && let Some(run) = project.runs.iter_mut().find(|r| r.service_id == service)
        {
            run.status = Status::Stopped;
            run.message = None;
            run.readiness = None;
        }
        if let Err(error) = self.write(&projects) {
            tracing::warn!(%error, "could not persist terminal stop");
        }
        Ok(())
    }

    fn update(&self, space: &str, run: ProjectTerminalRun) {
        let mut projects = self
            .inner
            .projects
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(project) = projects.get_mut(space) {
            if !project
                .config
                .services
                .iter()
                .any(|s| s.id == run.service_id)
            {
                return;
            }
            if let Some(existing) = project
                .runs
                .iter_mut()
                .find(|r| r.service_id == run.service_id)
            {
                *existing = run;
            } else {
                project.runs.push(run);
            }
        }
        if let Err(error) = self.write(&projects) {
            tracing::warn!(%error, "could not persist terminal state");
        }
    }

    fn set_readiness(
        &self,
        space: &str,
        service: &str,
        readiness: Option<Readiness>,
        message: Option<String>,
    ) {
        let mut projects = self
            .inner
            .projects
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(run) = projects
            .get_mut(space)
            .and_then(|p| p.runs.iter_mut().find(|r| r.service_id == service))
        {
            if run.readiness == readiness && run.message == message {
                return;
            }
            run.readiness = readiness;
            run.message = message;
        }
        if let Err(error) = self.write(&projects) {
            tracing::warn!(%error, "could not persist database readiness");
        }
    }

    /// Cancel supervision before the engine closes its PTYs, so shutdown can
    /// never be mistaken for a crash that should restart a service.
    pub fn shutdown(&self) {
        for check in self.inner.readiness_checks.lock().unwrap_or_else(|e| e.into_inner()).values() {
            check.token.cancel();
        }
        for startup in self.inner.startups.lock().unwrap_or_else(|e| e.into_inner()).values() {
            startup.token.cancel();
        }
        let keys = self
            .inner
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        // Each stop can take the full SIGTERM grace period; overlap them so
        // shutdown waits for the slowest service, not the sum of all of them.
        std::thread::scope(|scope| {
            for (space, service) in &keys {
                scope.spawn(move || {
                    if let Err(error) = self.stop(space, service) {
                        tracing::warn!(%error, "could not stop project service during shutdown");
                    }
                });
            }
        });
    }

    fn group_key(space: &str, checkout: &Path) -> Result<String, EngineError> {
        use sha2::{Digest, Sha256};
        let path = canonical_root(checkout)?;
        let hash: String = Sha256::digest(path.as_bytes())
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect();
        Ok(format!("worktree-services:{space}:{hash}"))
    }

    pub fn checkout_snapshot(
        &self,
        space: &str,
        root: &Path,
        checkout: &Path,
        settings: &zeron_proto::WorktreeSettings,
    ) -> Result<ProjectTerminalsSnapshot, EngineError> {
        let mut snapshot = self.snapshot(&Self::group_key(space, checkout)?, root)?;
        let defaults = self.snapshot(space, root)?;
        snapshot.startups.extend(defaults.startups.clone());
        snapshot.config = settings
            .services
            .clone()
            .unwrap_or(defaults.config.clone());
        snapshot.config.services
            .retain(|service| service.setup_prerequisite.is_none());
        snapshot.runs.retain(|run| {
            snapshot
                .config
                .services
                .iter()
                .any(|service| service.id == run.service_id)
        });
        for service in defaults
            .config
            .services
            .into_iter()
            .filter(|service| service.setup_prerequisite.is_some())
            .rev()
        {
            snapshot.config.services.insert(0, service.clone());
            if let Some(run) = defaults
                .runs
                .iter()
                .find(|run| run.service_id == service.id)
            {
                snapshot.runs.push(run.clone());
            }
        }
        snapshot.checkout = Some(canonical_root(checkout)?);
        Ok(snapshot)
    }

    pub fn save_checkout(
        &self,
        space: &str,
        root: &Path,
        checkout: &Path,
        mut config: ProjectTerminalConfig,
    ) -> Result<(), EngineError> {
        let key = Self::group_key(space, checkout)?;
        config
            .services
            .retain(|service| service.setup_prerequisite.is_none());
        self.save(&key, root, config)?;
        let mut projects = self
            .inner
            .projects
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let group = projects.get_mut(&key).unwrap();
        group.parent = Some(space.into());
        group.checkout = Some(canonical_root(checkout)?);
        self.write(&projects)
    }

    fn reconcile_configs(
        &self,
        environments: &crate::worktree_settings::WorktreeSettingsStore,
        space: &str,
        root: &Path,
        config: ProjectTerminalConfig,
    ) -> Result<(), EngineError> {
        self.save(space, root, config.clone())?;
        let groups = {
            let projects = self
                .inner
                .projects
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            projects
                .iter()
                .filter(|(_, p)| p.parent.as_deref() == Some(space))
                .filter_map(|(id, p)| {
                    p.checkout
                        .as_ref()
                        .map(|cwd| (id.clone(), PathBuf::from(cwd)))
                })
                .collect::<Vec<_>>()
        };
        for (id, cwd) in groups {
            if cwd.is_dir() {
                let settings = environments.effective(root, &cwd)?;
                self.save(
                    &id,
                    root,
                    ProjectTerminalConfig {
                        services: settings
                            .services
                            .unwrap_or_else(|| config.clone())
                            .services
                            .into_iter()
                            .filter(|service| service.setup_prerequisite.is_none())
                            .collect(),
                    },
                )?;
            }
        }
        Ok(())
    }

    pub async fn save_defaults(
        &self,
        environments: &crate::worktree_settings::WorktreeSettingsStore,
        space: &str,
        root: &Path,
        config: ProjectTerminalConfig,
    ) -> Result<(), EngineError> {
        let operation = self.managed_operation(space);
        let _guard = operation.lock().await;
        let this = self.clone();
        let environments = environments.clone();
        let space = space.to_string();
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let current = this.snapshot(&space, &root)?.config;
            let config = with_apps(&current, config);
            startup::validate(&config)?;
            this.reconcile_configs(&environments, &space, &root, config)
        })
        .await
        .map_err(|e| EngineError::Other(e.to_string()))?
    }

    /// Replaces the project's databases and keeps its app services. Removed
    /// databases are stopped.
    fn save_databases(
        &self,
        space: &str,
        root: &Path,
        databases: Vec<ProjectTerminalService>,
    ) -> Result<(), EngineError> {
        let current = self.snapshot(space, root)?.config;
        let config = with_databases(&current, databases)?;
        startup::validate(&config)?;
        self.save(space, root, config)?;
        Ok(())
    }

    /// Sets (or with None, clears) a worktree's own app services. Databases
    /// always come from the project and are never stored per checkout.
    pub async fn save_checkout_services(
        &self,
        environments: &crate::worktree_settings::WorktreeSettingsStore,
        space: &str,
        root: &Path,
        checkout: &Path,
        config: Option<ProjectTerminalConfig>,
    ) -> Result<(), EngineError> {
        let operation = self.managed_operation(space);
        let _guard = operation.lock().await;
        let this = self.clone();
        let environments = environments.clone();
        let space = space.to_string();
        let root = root.to_path_buf();
        let checkout = checkout.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let project = this.snapshot(&space, &root)?.config;
            startup::validate(&with_apps(&project, config.clone().unwrap_or_else(|| project.clone())))?;
            let apps = config.map(|config| ProjectTerminalConfig {
                services: with_apps(&project, config)
                    .services
                    .into_iter()
                    .filter(|s| !s.is_database())
                    .collect(),
            });
            environments.save_services(&root, &checkout, apps)?;
            let settings = environments.effective(&root, &checkout)?;
            this.save_checkout(
                &space,
                &root,
                &checkout,
                settings.services.unwrap_or(project),
            )
        })
        .await
        .map_err(|e| EngineError::Other(e.to_string()))?
    }

    /// Older versions also kept the project's services in worktree settings.
    /// Adopt that copy only when the profile has no app services of its own,
    /// then drop it so there is a single list.
    pub fn adopt_legacy_services(
        &self,
        environments: &crate::worktree_settings::WorktreeSettingsStore,
        space: &str,
        root: &Path,
    ) -> Result<(), EngineError> {
        let Some(legacy) = environments.legacy_services(root)? else {
            return Ok(());
        };
        let current = self.snapshot(space, root)?.config;
        if !current.services.iter().any(|s| !s.is_database())
            && legacy.services.iter().any(|s| !s.is_database())
        {
            self.reconcile_configs(environments, space, root, with_apps(&current, legacy))?;
            tracing::info!(space, "adopted project services from worktree settings");
        }
        environments.clear_legacy_services(root)
    }

    fn managed_operation(&self, space: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.inner
            .managed_operations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(space.into())
            .or_default()
            .clone()
    }

    /// Merge one agent edit while holding the same project lock as the UI.
    /// Saving never starts or restarts a process. Worktree apps become an
    /// override; databases always update the project's shared list.
    pub async fn edit_checkout_service(
        &self,
        environments: &crate::worktree_settings::WorktreeSettingsStore,
        space: &str,
        root: &Path,
        checkout: &Path,
        edit: zeron_proto::ProjectServiceEdit,
    ) -> Result<(ProjectTerminalsSnapshot, String), EngineError> {
        let operation = self.managed_operation(space);
        let _guard = operation.lock().await;
        let this = self.clone();
        let environments = environments.clone();
        let space = space.to_owned();
        let root = root.to_path_buf();
        let checkout = checkout.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let settings = environments.effective(&root, &checkout)?;
            let mut config = this
                .checkout_snapshot(&space, &root, &checkout, &settings)?
                .config;
            let service = edited_service(&config, edit)?;
            let id = service.id.clone();
            if let Some(existing) = config.services.iter_mut().find(|s| s.id == id) {
                *existing = service.clone();
            } else {
                config.services.push(service.clone());
            }
            // Validate the complete visible list before touching either store.
            let config = normalize_config(config)?;
            startup::validate(&config)?;
            if service.is_database() {
                let mut project = this.snapshot(&space, &root)?.config;
                let normalized = config.services.iter().find(|s| s.id == id).unwrap().clone();
                if let Some(existing) = project.services.iter_mut().find(|s| s.id == id) {
                    *existing = normalized;
                } else {
                    project.services.push(normalized);
                }
                this.save(&space, &root, normalize_config(project)?)?;
            } else if root.canonicalize()? == checkout.canonicalize()? {
                this.reconcile_configs(&environments, &space, &root, config)?;
            } else {
                let apps = ProjectTerminalConfig {
                    services: config
                        .services
                        .into_iter()
                        .filter(|s| !s.is_database())
                        .collect(),
                };
                environments.save_services(&root, &checkout, Some(apps.clone()))?;
                this.save_checkout(&space, &root, &checkout, apps)?;
            }
            let snapshot = this.checkout_snapshot(
                &space,
                &root,
                &checkout,
                &environments.effective(&root, &checkout)?,
            )?;
            Ok((snapshot, id))
        })
        .await
        .map_err(|e| EngineError::Other(e.to_string()))?
    }

    /// Saves a Project settings page. Its service lists are ignored: app
    /// services are saved only by the services editor, and `databases`, when
    /// given, replaces only the project's databases.
    pub async fn save_environment(
        &self,
        environments: &crate::worktree_settings::WorktreeSettingsStore,
        space: &str,
        root: &Path,
        checkout: Option<&Path>,
        settings: Option<zeron_proto::WorktreeSettings>,
        databases: Option<Vec<ProjectTerminalService>>,
    ) -> Result<(), EngineError> {
        if checkout.is_some() && databases.is_some() {
            return Err(EngineError::Other(
                "Databases are shared by the whole project. Edit them in Project defaults.".into(),
            ));
        }
        let operation = self.managed_operation(space);
        let _guard = operation.lock().await;
        let this = self.clone();
        let environments = environments.clone();
        let space = space.to_string();
        let root = root.to_path_buf();
        let checkout = checkout.map(Path::to_path_buf);
        tokio::task::spawn_blocking(move || {
            // Validate both parts before writing either.
            if let Some(databases) = &databases {
                let config = normalize_config(with_databases(
                    &this.snapshot(&space, &root)?.config,
                    databases.clone(),
                )?)?;
                startup::validate(&config)?;
            }
            environments.save(&root, checkout.as_deref(), settings)?;
            match databases {
                Some(databases) => this.save_databases(&space, &root, databases),
                None => Ok(()),
            }
        })
        .await
        .map_err(|e| EngineError::Other(e.to_string()))?
    }

    /// Applies an agent's change to the latest settings (the project
    /// defaults, or what `checkout` uses now) under the UI's project lock,
    /// so concurrent edits never drop each other's fields. `None` from
    /// `edit` removes the checkout's override.
    pub async fn edit_environment(
        &self,
        environments: &crate::worktree_settings::WorktreeSettingsStore,
        space: &str,
        root: &Path,
        checkout: Option<&Path>,
        edit: impl FnOnce(
            zeron_proto::WorktreeSettings,
        ) -> Result<Option<zeron_proto::WorktreeSettings>, EngineError>
        + Send
        + 'static,
    ) -> Result<(), EngineError> {
        let operation = self.managed_operation(space);
        let _guard = operation.lock().await;
        let environments = environments.clone();
        let root = root.to_path_buf();
        let checkout = checkout.map(Path::to_path_buf);
        tokio::task::spawn_blocking(move || {
            let current = match &checkout {
                Some(checkout) => environments.effective(&root, checkout)?,
                None => environments.defaults(&root)?,
            };
            environments.save(&root, checkout.as_deref(), edit(current)?)
        })
        .await
        .map_err(|e| EngineError::Other(e.to_string()))?
    }

    pub(crate) fn has_setup_services(&self, root: &Path) -> Result<bool, EngineError> {
        let root = canonical_root(root)?;
        Ok(self
            .inner
            .projects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .any(|p| {
                p.root == root
                    && p.parent.is_none()
                    && p.config
                        .services
                        .iter()
                        .any(|s| s.setup_prerequisite.is_some())
            }))
    }

    /// Infrastructure belongs to the project's main folder, outside checkout
    /// handoffs. Status polling never calls this method.
    pub(crate) async fn ensure_setup_services_for(
        &self,
        terminals: &Terminals,
        root: &Path,
        settings: &zeron_proto::WorktreeSettings,
        log: &Path,
        token: &CancellationToken,
        selected: Option<&HashSet<String>>,
    ) -> Result<(), EngineError> {
        let source = canonical_root(root)?;
        let mut services = {
            let projects = self
                .inner
                .projects
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            projects
                .iter()
                .filter(|(_, p)| p.root == source && p.parent.is_none())
                .flat_map(|(space, p)| {
                    p.config
                        .services
                        .iter()
                        .filter(|s| {
                            s.is_database() && selected.is_none_or(|ids| ids.contains(&s.id))
                        })
                        .cloned()
                        .map(|s| (space.clone(), s))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let config = ProjectTerminalConfig {
            services: services
                .iter()
                .map(|(_, service)| service.clone())
                .collect(),
        };
        let ordered = startup::order(
            &config,
            &config
                .services
                .iter()
                .map(|service| service.id.clone())
                .collect::<Vec<_>>(),
        )?;
        services.sort_by_key(|(_, service)| ordered.iter().position(|id| id == &service.id));
        for (space, service) in services {
            let _check = self.readiness_check(&space, &service.id, token.clone());
            if token.is_cancelled() {
                return Err(EngineError::Other("Worktree setup was cancelled.".into()));
            }
            self.startup_message(
                &space,
                Some(&service),
                &format!("Waiting for {}", service.name),
            );
            {
                let mut projects = self
                    .inner
                    .projects
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(project) = projects.get_mut(&space) {
                    project.environment = settings.variables.clone().into_iter().collect();
                }
            }
            let this = self.clone();
            let terminals = terminals.clone();
            let root_path = root.to_path_buf();
            let id = service.id.clone();
            let owner = space.clone();
            tokio::task::spawn_blocking(move || {
                this.control_in_checkout(
                    &terminals,
                    &owner,
                    &root_path,
                    Some(&root_path),
                    Some(&id),
                    "start",
                )
            })
            .await
            .map_err(|e| EngineError::Other(e.to_string()))??;
            let readiness = service.setup_prerequisite.as_ref().unwrap();
            let deadline =
                tokio::time::Instant::now() + Duration::from_secs(readiness.timeout_seconds);
            // Re-checking a ready database must not flicker its status.
            let was_ready = self
                .snapshot(&space, root)?
                .runs
                .iter()
                .any(|r| r.service_id == service.id && r.readiness == Some(Readiness::Ready));
            if !was_ready {
                self.set_readiness(&space, &service.id, Some(Readiness::Checking), None);
            }
            let cancelled = || {
                if !was_ready {
                    self.set_readiness(&space, &service.id, None, None);
                }
                Err(EngineError::Other("Worktree setup was cancelled.".into()))
            };
            loop {
                if token.is_cancelled() {
                    return cancelled();
                }
                let snapshot = self.snapshot(&space, root)?;
                if !snapshot
                    .config
                    .services
                    .iter()
                    .any(|saved| saved == &service)
                {
                    return Err(EngineError::Other(
                        "Project services changed during startup.".into(),
                    ));
                }
                let run = snapshot
                    .runs
                    .iter()
                    .find(|run| run.service_id == service.id)
                    .ok_or_else(|| {
                        EngineError::Other(format!(
                            "{} was removed while waiting for readiness.",
                            service.name
                        ))
                    })?;
                if !matches!(
                    run.status,
                    Status::Running | Status::Restarting | Status::Exited
                ) || (run.status == Status::Exited && run.exit_code != Some(0))
                {
                    return Err(EngineError::Other(format!(
                        "{} stopped before it was ready. Open its service output for details.",
                        service.name
                    )));
                }
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    let message = format!(
                        "{} was not ready after {} seconds. Check its output and the readiness command.",
                        service.name, readiness.timeout_seconds
                    );
                    self.set_readiness(
                        &space,
                        &service.id,
                        Some(Readiness::NotReady),
                        Some(message.clone()),
                    );
                    return Err(EngineError::Other(message));
                }
                let mut check_settings = settings.clone();
                check_settings.command_timeout_seconds = remaining.as_secs().clamp(1, 5);
                if crate::worktree_settings::commands::run_on_host(
                    root,
                    &check_settings,
                    &readiness.command,
                    log,
                    token,
                )
                .await
                .is_ok()
                {
                    // A successful check must not hide an exited or stopped
                    // infrastructure process (for example a stale ready file).
                    let after = self.snapshot(&space, root)?;
                    if after.runs.iter().any(|r| {
                        r.service_id == service.id
                            && (r.status == Status::Running
                                || (r.status == Status::Exited && r.exit_code == Some(0)))
                    }) {
                        self.set_readiness(&space, &service.id, Some(Readiness::Ready), None);
                        break;
                    }
                }
                tokio::select! {
                    _ = token.cancelled() => return cancelled(),
                    _ = tokio::time::sleep_until((tokio::time::Instant::now() + Duration::from_millis(250)).min(deadline)) => {},
                }
            }
        }
        Ok(())
    }

    /// All windows serialize a project's handoff. Parallel groups are keyed by
    /// the physical checkout: two conversations in one worktree reuse its PTYs.
    #[allow(clippy::too_many_arguments)]
    pub async fn control_checkout(
        &self,
        terminals: &Terminals,
        environments: &crate::worktree_settings::WorktreeSettingsStore,
        space: &str,
        root: &Path,
        checkout: &Path,
        service_id: Option<&str>,
        action: &str,
    ) -> Result<ProjectTerminalsSnapshot, EngineError> {
        if !matches!(
            action,
            "start" | "stop" | "restart" | "activate" | "stop-worktree"
        ) {
            return Err(EngineError::Other("Unknown terminal action".into()));
        }
        if action == "stop-worktree" && service_id.is_some() {
            return Err(EngineError::Other(
                "Stop worktree services does not take a service id.".into(),
            ));
        }
        let operation = self.managed_operation(space);
        let settings = environments.effective(root, checkout)?;
        let config = self
            .checkout_snapshot(space, root, checkout, &settings)?
            .config;
        if let Some(id) = service_id
            && !config.services.iter().any(|service| service.id == id)
        {
            return Err(EngineError::Other(format!("Unknown service id: {id}")));
        }
        let stopping = matches!(action, "stop" | "stop-worktree");
        if stopping {
            let cancelled_active =
                self.cancel_startup(space, checkout, service_id, action == "stop-worktree")?;
            // A queued start can be cancelled while an unrelated startup keeps
            // the project lock. Stop its existing process without joining that queue.
            if let Some(id) = service_id
                && !cancelled_active
                && self.has_active_startup(space)
            {
                let owner = if config
                    .services
                    .iter()
                    .any(|s| s.id == id && s.is_database())
                {
                    space.to_string()
                } else {
                    Self::group_key(space, checkout)?
                };
                if self
                    .snapshot(&owner, root)?
                    .config
                    .services
                    .iter()
                    .any(|s| s.id == id)
                {
                    let this = self.clone();
                    let terminals = terminals.clone();
                    let root = root.to_path_buf();
                    let id = id.to_string();
                    tokio::task::spawn_blocking(move || {
                        this.control_in_checkout(&terminals, &owner, &root, None, Some(&id), "stop")
                    })
                    .await
                    .map_err(|e| EngineError::Other(e.to_string()))??;
                }
                return self.checkout_snapshot(space, root, checkout, &settings);
            }
        }
        let resolve_roots =
            || -> Result<Vec<String>, EngineError> {
                let mut roots: Vec<String> = if stopping {
                    Vec::new()
                } else if let Some(id) = service_id {
                    vec![id.into()]
                } else if action == "activate" {
                    let key = Self::group_key(space, checkout)?;
                    let projects = self
                        .inner
                        .projects
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let running: HashSet<_> = projects
                        .iter()
                        .filter(|(id, project)| {
                            (*id == space || project.parent.as_deref() == Some(space))
                                && (settings.service_mode
                                    == zeron_proto::WorktreeServiceMode::FollowActive
                                    || **id == key)
                        })
                        .flat_map(|(_, project)| project.runs.iter())
                        .filter(|run| matches!(run.status, Status::Running | Status::Restarting))
                        .map(|run| run.service_id.as_str())
                        .collect();
                    config
                        .services
                        .iter()
                        .filter(|service| {
                            !service.is_database() && running.contains(service.id.as_str())
                        })
                        .map(|service| service.id.clone())
                        .collect()
                } else {
                    config
                        .services
                        .iter()
                        .filter(|service| action != "restart" || !service.is_database())
                        .map(|service| service.id.clone())
                        .collect()
                };
                // Old profiles activated their shared databases on checkout selection.
                if action == "activate"
                    && config
                        .services
                        .iter()
                        .all(|service| service.depends_on.is_none())
                {
                    roots.extend(
                        config
                            .services
                            .iter()
                            .filter(|service| service.is_database())
                            .map(|service| service.id.clone()),
                    );
                }
                if !stopping
                    && !service_id.is_some_and(|id| {
                        config
                            .services
                            .iter()
                            .any(|s| s.id == id && s.is_database())
                    })
                {
                    let key = Self::group_key(space, checkout)?;
                    let needs_prepare = !environments.is_prepared(root, checkout)?;
                    let variables: HashMap<_, _> = settings.variables.clone().into_iter().collect();
                    let projects = self
                        .inner
                        .projects
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    for (_, project) in projects.iter().filter(|(id, p)| {
                        (*id == space || p.parent.as_deref() == Some(space))
                            && (*id == space
                                || (settings.service_mode
                                    == zeron_proto::WorktreeServiceMode::FollowActive
                                    && **id != key)
                                || (**id == key && (needs_prepare || p.environment != variables)))
                    }) {
                        for run in project.runs.iter().filter(|run| {
                            matches!(run.status, Status::Running | Status::Restarting)
                        }) {
                            if config
                                .services
                                .iter()
                                .any(|s| s.id == run.service_id && !s.is_database())
                                && !roots.contains(&run.service_id)
                            {
                                roots.push(run.service_id.clone());
                            }
                        }
                    }
                }
                Ok(roots)
            };
        let mut roots = resolve_roots()?;
        let mut order = startup::order(&config, &roots)?;
        let token = CancellationToken::new();
        let startup = if stopping {
            None
        } else {
            Some(self.begin_startup(space, checkout, &config, &roots, &order, token.clone())?)
        };
        let _guard = if stopping {
            operation.lock().await
        } else {
            tokio::select! {
                biased;
                _ = token.cancelled() => {
                    drop(startup);
                    return self.checkout_snapshot(space, root, checkout, &settings);
                }
                guard = operation.lock() => guard,
            }
        };
        let current_settings = environments.effective(root, checkout)?;
        if !stopping
            && (current_settings != settings
                || self
                    .checkout_snapshot(space, root, checkout, &current_settings)?
                    .config
                    != config)
        {
            return Err(EngineError::Other(
                "Service settings changed while startup was queued. Try Start again.".into(),
            ));
        }
        roots = resolve_roots()?;
        order = startup::order(&config, &roots)?;
        if let Some(startup) = &startup {
            startup.activate(&roots, &order);
        }
        let result = self
            .control_checkout_impl(
                terminals,
                environments,
                space,
                root,
                checkout,
                service_id,
                action,
                &order,
                &token,
            )
            .await;
        if let Err(error) = &result
            && action != "activate"
        {
            let group = Self::group_key(space, checkout)?;
            for id in &roots {
                let owner = if config
                    .services
                    .iter()
                    .any(|service| service.id == *id && service.is_database())
                {
                    space
                } else {
                    &group
                };
                let previous = self
                    .snapshot(owner, root)?
                    .runs
                    .into_iter()
                    .find(|run| run.service_id == *id);
                if previous
                    .as_ref()
                    .is_none_or(|run| !matches!(run.status, Status::Running | Status::Restarting))
                {
                    self.update(
                        owner,
                        ProjectTerminalRun {
                            service_id: id.clone(),
                            status: if token.is_cancelled() {
                                Status::Stopped
                            } else {
                                Status::Failed
                            },
                            exit_code: None,
                            restarts: previous.as_ref().map_or(0, |run| run.restarts),
                            message: (!token.is_cancelled()).then(|| error.to_string()),
                            terminal: previous.and_then(|run| run.terminal),
                            readiness: None,
                        },
                    );
                }
            }
        }
        let cancelled = token.is_cancelled();
        drop(startup);
        if cancelled || result.is_ok() {
            self.checkout_snapshot(space, root, checkout, &settings)
        } else {
            result
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn control_checkout_impl(
        &self,
        terminals: &Terminals,
        environments: &crate::worktree_settings::WorktreeSettingsStore,
        space: &str,
        root: &Path,
        checkout: &Path,
        service_id: Option<&str>,
        action: &str,
        order: &[String],
        token: &CancellationToken,
    ) -> Result<ProjectTerminalsSnapshot, EngineError> {
        let worktree_only = action == "stop-worktree";
        if worktree_only && service_id.is_some() {
            return Err(EngineError::Other(
                "Stop worktree services does not take a service id.".into(),
            ));
        }
        let action = if worktree_only { "stop" } else { action };
        if !matches!(action, "start" | "stop" | "restart" | "activate") {
            return Err(EngineError::Other("Unknown terminal action".into()));
        }
        let settings = environments.effective(root, checkout)?;
        let config = self
            .checkout_snapshot(space, root, checkout, &settings)?
            .config;
        // Reject stale ids before preparing a checkout or handing off processes.
        if let Some(id) = service_id
            && !config.services.iter().any(|service| service.id == id)
        {
            return Err(EngineError::Other(format!("Unknown service id: {id}")));
        }
        let shared: HashSet<_> = config
            .services
            .iter()
            .filter(|s| s.setup_prerequisite.is_some())
            .map(|s| s.id.clone())
            .collect();
        if action != "activate" && service_id.is_some_and(|id| shared.contains(id)) {
            let this = self.clone();
            let startup_terminals = terminals.clone();
            let owner = space.to_string();
            let source = root.to_path_buf();
            let id = service_id.map(str::to_string);
            let operation = if action == "restart" { "stop" } else { action }.to_string();
            if action != "start" {
                tokio::task::spawn_blocking(move || {
                    this.control_in_checkout(
                        &startup_terminals,
                        &owner,
                        &source,
                        Some(&source),
                        id.as_deref(),
                        &operation,
                    )
                })
                .await
                .map_err(|e| EngineError::Other(e.to_string()))??;
            }
            if action != "stop" {
                let log = self.readiness_log(root)?;
                self.ensure_setup_services_for(
                    terminals,
                    root,
                    &environments.defaults(root)?,
                    &log,
                    token,
                    Some(&order.iter().cloned().collect()),
                )
                .await?;
            }
            return self.checkout_snapshot(space, root, checkout, &settings);
        }
        if !worktree_only && !shared.is_empty() && action == "stop" && service_id.is_none() {
            let this = self.clone();
            let terminals = terminals.clone();
            let owner = space.to_string();
            let source = root.to_path_buf();
            tokio::task::spawn_blocking(move || {
                this.control_in_checkout(&terminals, &owner, &source, None, None, "stop")
            })
            .await
            .map_err(|e| EngineError::Other(e.to_string()))??;
        }
        let key = Self::group_key(space, checkout)?;
        let needs_prepare = !environments.is_prepared(root, checkout)?;
        let variables: HashMap<_, _> = settings.variables.clone().into_iter().collect();
        let groups = {
            let projects = self
                .inner
                .projects
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            projects
                .iter()
                .filter(|(id, p)| {
                    (*id == space || p.parent.as_deref() == Some(space))
                        && action != "stop"
                        && (*id == space
                            || (settings.service_mode
                                == zeron_proto::WorktreeServiceMode::FollowActive
                                && **id != key)
                            || (**id == key && (needs_prepare || p.environment != variables)))
                })
                .map(|(id, p)| {
                    (
                        id.clone(),
                        p.runs
                            .iter()
                            .filter(|r| matches!(r.status, Status::Running | Status::Restarting))
                            .filter(|r| !shared.contains(&r.service_id))
                            .map(|r| r.service_id.clone())
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        for (group, services) in groups {
            if !services.is_empty() {
                let this = self.clone();
                let root = root.to_path_buf();
                let terminals = terminals.clone();
                tokio::task::spawn_blocking(move || {
                    for id in services {
                        this.control_in_checkout(
                            &terminals,
                            &group,
                            &root,
                            None,
                            Some(&id),
                            "stop",
                        )?;
                    }
                    Ok::<_, EngineError>(())
                })
                .await
                .map_err(|e| EngineError::Other(e.to_string()))??;
            }
        }
        // Configuration changes are applied to this group's next process start.
        self.save_checkout(space, root, checkout, config.clone())?;
        {
            let mut projects = self
                .inner
                .projects
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            projects.get_mut(&key).unwrap().environment =
                settings.variables.clone().into_iter().collect();
            self.write(&projects)?;
        }
        if action != "activate" && config.services.is_empty() {
            return Err(EngineError::Other(
                "No saved terminal services selected".into(),
            ));
        }
        if action == "stop" {
            if config.services.iter().any(|s| !s.is_database()) {
                let this = self.clone();
                let terminals = terminals.clone();
                let source = root.to_path_buf();
                let service = service_id.map(str::to_string);
                tokio::task::spawn_blocking(move || {
                    this.control_in_checkout(
                        &terminals,
                        &key,
                        &source,
                        None,
                        service.as_deref(),
                        "stop",
                    )
                })
                .await
                .map_err(|e| EngineError::Other(e.to_string()))??;
            }
            return self.checkout_snapshot(space, root, checkout, &settings);
        }
        check_startup_cancelled(token)?;
        let prerequisites: HashSet<_> = order
            .iter()
            .filter(|id| shared.contains(*id))
            .cloned()
            .collect();
        if root.canonicalize()? == checkout.canonicalize()? {
            let log = self.readiness_log(root)?;
            self.ensure_setup_services_for(
                terminals,
                root,
                &environments.defaults(root)?,
                &log,
                token,
                Some(&prerequisites),
            )
            .await?;
        }
        self.startup_message(space, None, "Preparing worktree…");
        environments
            .activate_for_services(root, checkout, Some(&prerequisites), Some(token))
            .await?;
        check_startup_cancelled(token)?;
        for service in config
            .services
            .iter()
            .filter(|s| !s.is_database() && order.contains(&s.id))
        {
            service_directory(checkout, &service.directory)?;
        }
        for id in order {
            check_startup_cancelled(token)?;
            let service = config
                .services
                .iter()
                .find(|s| s.id == *id)
                .expect("validated dependency");
            if service.is_database() {
                self.startup_finished(space, id);
                continue;
            }
            self.startup_message(
                space,
                Some(service),
                &format!("Waiting for {}", service.name),
            );
            let this = self.clone();
            let terminals = terminals.clone();
            let source = root.to_path_buf();
            let destination = checkout.to_path_buf();
            let group = key.clone();
            let id = id.clone();
            // Dependencies are reused. Only the requested services restart.
            let action = if action == "restart" && service_id.is_none_or(|selected| selected == id)
            {
                "restart"
            } else {
                "start"
            };
            tokio::task::spawn_blocking(move || {
                this.control_in_checkout(
                    &terminals,
                    &group,
                    &source,
                    Some(&destination),
                    Some(&id),
                    action,
                )
            })
            .await
            .map_err(|e| EngineError::Other(e.to_string()))??;
            self.startup_finished(space, &service.id);
        }
        self.checkout_snapshot(space, root, checkout, &settings)
    }

    fn readiness_log(&self, root: &Path) -> Result<PathBuf, EngineError> {
        use sha2::{Digest, Sha256};
        let hash = format!("{:x}", Sha256::digest(canonical_root(root)?.as_bytes()));
        Ok(self
            .inner
            .path
            .parent()
            .unwrap()
            .join("service-readiness")
            .join(format!("{hash}.log")))
    }

    pub async fn prepare_checkout(
        &self,
        environments: &crate::worktree_settings::WorktreeSettingsStore,
        space: &str,
        root: &Path,
        checkout: &Path,
    ) -> Result<(), EngineError> {
        let operation = self.managed_operation(space);
        let _guard = operation.lock().await;
        let this = self.clone();
        let source = root.to_path_buf();
        let cwd = checkout.to_path_buf();
        tokio::task::spawn_blocking(move || this.stop_checkout(&source, &cwd))
            .await
            .map_err(|e| EngineError::Other(e.to_string()))??;
        environments.prepare(root, checkout, true).await
    }

    pub fn stop_checkout(&self, root: &Path, checkout: &Path) -> Result<(), EngineError> {
        let root = canonical_root(root)?;
        // A repeated removal (or a checkout removed outside Zeron) still needs
        // to stop any owned process and retire its saved service state.
        let checkout = match canonical_root(checkout) {
            Ok(path) => path,
            Err(_) if checkout.is_absolute() && !checkout.exists() => {
                checkout.to_string_lossy().into_owned()
            }
            Err(error) => return Err(error),
        };
        let groups = {
            let projects = self
                .inner
                .projects
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            projects
                .iter()
                .filter(|(_, p)| p.root == root && p.checkout.as_deref() == Some(&checkout))
                .map(|(id, p)| {
                    (
                        id.clone(),
                        p.runs
                            .iter()
                            .filter(|run| !p.config.services.iter().any(|s| s.id == run.service_id && s.setup_prerequisite.is_some()))
                            .map(|r| r.service_id.clone())
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        for (group, services) in groups {
            let operation = self.operation(&group);
            let _guard = operation.lock().unwrap_or_else(|e| e.into_inner());
            for id in services {
                self.stop(&group, &id)?;
            }
        }
        Ok(())
    }


}

#[allow(clippy::too_many_arguments)]
async fn supervise(
    weak: Weak<Inner>,
    terminals: Terminals,
    key: (String, String),
    service: ProjectTerminalService,
    directory: PathBuf,
    environment: HashMap<String, String>,
    mut session: zeron_proto::TerminalSession,
    token: CancellationToken,
) {
    let mut restarts = 0;
    loop {
        let started = std::time::Instant::now();
        let mut stream = match terminals.subscribe(&session.id, None) {
            Ok(stream) => stream,
            Err(_) => return,
        };
        let code = loop {
            tokio::select! {
                _ = token.cancelled() => return,
                event = stream.recv() => match event {
                    Some(TerminalEvent::Exit { exit_code, .. }) => break exit_code,
                    None => break -1,
                    _ => {},
                }
            }
        };
        // The limit stops crash loops. A service that stayed up for a while
        // before failing gets its full budget again.
        if started.elapsed() >= RESTART_BUDGET_RESET {
            restarts = 0;
        }
        let Some(inner) = weak.upgrade() else { return };
        let store = ProjectTerminals { inner };
        // Serialize exit/restart with Stop and Start. A cancelled old watcher
        // can never overwrite the state of a newly started process.
        {
            let mut live = store.inner.live.lock().unwrap_or_else(|e| e.into_inner());
            if token.is_cancelled() {
                return;
            }
            let retry = code != 0 && service.restart_on_failure && restarts < MAX_RESTARTS;
            store.update(
                &key.0,
                ProjectTerminalRun {
                    service_id: key.1.clone(),
                    status: if retry {
                        Status::Restarting
                    } else if code == 0 {
                        Status::Exited
                    } else {
                        Status::Failed
                    },
                    exit_code: Some(code),
                    restarts,
                    message: (code != 0).then(|| {
                        if retry {
                            "Service exited; restarting shortly…".into()
                        } else if restarts == MAX_RESTARTS {
                            "Restart limit reached (3 attempts). Check the terminal output.".into()
                        } else {
                            format!("Service exited with status {code}. Check the terminal output.")
                        }
                    }),
                    terminal: Some(session.clone()),
                    readiness: None,
                },
            );
            if !retry {
                live.remove(&key);
                return;
            }
        }
        drop(store);
        tokio::select! {
            _ = token.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(1 << restarts)) => {},
        }
        let Some(inner) = weak.upgrade() else { return };
        let store = ProjectTerminals { inner };
        restarts += 1;
        let failed = session.clone();
        let respawn = {
            let terminals = terminals.clone();
            let key = key.clone();
            let directory = directory.clone();
            let environment = environment.clone();
            let command = service.command.clone();
            let token = token.clone();
            let open = match service.is_database() {
                true => Terminals::open_host_service,
                false => Terminals::open_service,
            };
            // Spawning a shell blocks; keep it off the async workers.
            tokio::task::spawn_blocking(move || {
                warm_login_path();
                let mut live = store.inner.live.lock().unwrap_or_else(|e| e.into_inner());
                if token.is_cancelled() {
                    return None;
                }
                match open(
                    &terminals,
                    &directory.to_string_lossy(),
                    &environment,
                    &command,
                ) {
                    Ok(next) => {
                        let _ = terminals.close(&failed.id);
                        if let Some(run) = live.get_mut(&key) {
                            run.terminal_id = next.id.clone();
                        }
                        store.update(
                            &key.0,
                            ProjectTerminalRun {
                                service_id: key.1.clone(),
                                status: Status::Running,
                                exit_code: None,
                                restarts,
                                message: None,
                                terminal: Some(next.clone()),
                                readiness: None,
                            },
                        );
                        Some(next)
                    }
                    Err(error) => {
                        live.remove(&key);
                        // Keep the failed run's terminal so its output explains
                        // the failure.
                        store.update(
                            &key.0,
                            ProjectTerminalRun {
                                service_id: key.1.clone(),
                                status: Status::Failed,
                                exit_code: Some(code),
                                restarts,
                                message: Some(error.to_string()),
                                terminal: Some(failed),
                                readiness: None,
                            },
                        );
                        None
                    }
                }
            })
        };
        match respawn.await {
            Ok(Some(next)) => session = next,
            _ => return,
        }
    }
}

/// Resolve the cached login-shell PATH before taking `live`, so the first
/// service start never waits for the user's shell while holding the registry.
fn warm_login_path() {
    let _ = zeron_harness::composed_login_shell_path();
}

fn check_startup_cancelled(token: &CancellationToken) -> Result<(), EngineError> {
    if token.is_cancelled() {
        Err(EngineError::Other("Service startup was cancelled.".into()))
    } else {
        Ok(())
    }
}

fn canonical_root(root: &Path) -> Result<String, EngineError> {
    let path = std::fs::canonicalize(root)
        .map_err(|_| EngineError::Other("Project directory is unavailable".into()))?;
    if !path.is_dir() {
        return Err(EngineError::Other(
            "Project directory is unavailable".into(),
        ));
    }
    Ok(path.to_string_lossy().into_owned())
}

fn check_root(project: &Project, root: &str) -> Result<(), EngineError> {
    if project.root != root {
        return Err(EngineError::Other(
            "Project terminal profile belongs to another directory".into(),
        ));
    }
    Ok(())
}

/// Apply a single-entry change to the latest saved configuration.
fn edited_service(
    config: &ProjectTerminalConfig,
    edit: zeron_proto::ProjectServiceEdit,
) -> Result<ProjectTerminalService, EngineError> {
    use zeron_proto::{
        ProjectServiceEdit as Edit, ProjectServiceKind as Kind, ProjectServiceReadiness,
    };
    match edit {
        Edit::Add {
            kind,
            name,
            command,
            directory,
            restart_on_failure,
            depends_on,
            readiness_command,
            readiness_timeout_seconds,
        } => {
            let setup_prerequisite = match kind {
                Kind::Database => Some(ProjectServiceReadiness {
                    command: readiness_command.ok_or_else(|| {
                        EngineError::Other("Databases require a readinessCommand.".into())
                    })?,
                    timeout_seconds: readiness_timeout_seconds.unwrap_or(120),
                }),
                Kind::Service => {
                    if readiness_command.is_some() || readiness_timeout_seconds.is_some() {
                        return Err(EngineError::Other(
                            "Readiness checks apply to databases; use kind: database.".into(),
                        ));
                    }
                    None
                }
            };
            Ok(ProjectTerminalService {
                depends_on: Some(depends_on.unwrap_or_default()),
                id: uuid::Uuid::new_v4().to_string(),
                name,
                command,
                directory,
                restart_on_failure,
                setup_prerequisite,
            })
        }
        Edit::Update {
            service_id,
            changes,
        } => {
            let mut service = config
                .services
                .iter()
                .find(|s| s.id == service_id)
                .cloned()
                .ok_or_else(|| EngineError::Other(format!("Unknown service id: {service_id}")))?;
            if changes.name.is_none()
                && changes.command.is_none()
                && changes.directory.is_none()
                && changes.restart_on_failure.is_none()
                && changes.depends_on.is_none()
                && changes.readiness_command.is_none()
                && changes.readiness_timeout_seconds.is_none()
            {
                return Err(EngineError::Other(
                    "Provide at least one service field to update.".into(),
                ));
            }
            if let Some(value) = changes.name {
                service.name = value;
            }
            if let Some(value) = changes.command {
                service.command = value;
            }
            if let Some(value) = changes.directory {
                service.directory = value;
            }
            if let Some(value) = changes.restart_on_failure {
                service.restart_on_failure = value;
            }
            if let Some(value) = changes.depends_on {
                service.depends_on = Some(value);
            }
            if changes.readiness_command.is_some() || changes.readiness_timeout_seconds.is_some() {
                let readiness = service.setup_prerequisite.as_mut().ok_or_else(|| {
                    EngineError::Other(
                        "Readiness checks apply to databases; add a database instead.".into(),
                    )
                })?;
                if let Some(value) = changes.readiness_command {
                    readiness.command = value;
                }
                if let Some(value) = changes.readiness_timeout_seconds {
                    readiness.timeout_seconds = value;
                }
            }
            Ok(service)
        }
    }
}

/// The project's saved databases plus `apps` from an app-service editor.
/// Entries matching a saved database by id never replace or remove it.
fn with_apps(
    current: &ProjectTerminalConfig,
    apps: ProjectTerminalConfig,
) -> ProjectTerminalConfig {
    let mut services: Vec<_> = current
        .services
        .iter()
        .filter(|s| s.is_database())
        .cloned()
        .collect();
    let apps = apps
        .services
        .into_iter()
        .filter(|app| !app.is_database() && !services.iter().any(|db| db.id == app.id))
        .collect::<Vec<_>>();
    services.extend(apps);
    ProjectTerminalConfig { services }
}

/// `databases` followed by the project's saved app services.
fn with_databases(
    current: &ProjectTerminalConfig,
    mut databases: Vec<ProjectTerminalService>,
) -> Result<ProjectTerminalConfig, EngineError> {
    if let Some(database) = databases.iter().find(|s| !s.is_database()) {
        return Err(EngineError::Other(format!(
            "Add a readiness check for the database “{}”.",
            database.name.trim()
        )));
    }
    databases.extend(
        current
            .services
            .iter()
            .filter(|s| !s.is_database())
            .cloned(),
    );
    Ok(ProjectTerminalConfig {
        services: databases,
    })
}

pub fn normalize_config(
    mut config: ProjectTerminalConfig,
) -> Result<ProjectTerminalConfig, EngineError> {
    let invalid = |message: String| Err(EngineError::Other(message));
    if config.services.len() > MAX_SERVICES {
        return invalid("A project supports at most 12 services and databases.".into());
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for service in &mut config.services {
        service.name = service.name.trim().to_string();
        service.command = service.command.trim().to_string();
        service.directory = service.directory.trim().to_string();
        if service.directory.is_empty() {
            service.directory = ".".into();
        }
        let name = &service.name;
        if service.id.is_empty() || service.id.len() > 96 || !ids.insert(service.id.clone()) {
            return invalid(format!(
                "“{name}” has a missing or duplicate id. Remove it and add it again."
            ));
        }
        if name.is_empty() || name.chars().count() > 80 {
            return invalid(
                "Give every service and database a name of up to 80 characters.".into(),
            );
        }
        if !names.insert(name.to_lowercase()) {
            return invalid(format!(
                "“{name}” is used twice. Give each service and database its own name."
            ));
        }
        if service.command.is_empty() {
            return invalid(format!("Enter a command for “{name}”."));
        }
        if service.command.len() > 16 * 1024 || service.command.contains('\0') {
            return invalid(format!("The command for “{name}” is too long or invalid."));
        }
        if service.directory.len() > 4096
            || service.directory.contains('\0')
            || Path::new(&service.directory).components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return invalid(format!(
                "“{name}” must run in a folder inside the project, such as . or apps/api."
            ));
        }
        if let Some(readiness) = &mut service.setup_prerequisite {
            readiness.command = readiness.command.trim().to_string();
            if readiness.command.is_empty()
                || readiness.command.len() > 16 * 1024
                || readiness.command.contains('\0')
            {
                return invalid(format!("Enter a readiness check for “{name}”."));
            }
            if !(1..=zeron_proto::MAX_WORKTREE_COMMAND_TIMEOUT_SECONDS)
                .contains(&readiness.timeout_seconds)
            {
                return invalid(format!(
                    "The readiness timeout for “{name}” must be between 1 and 7200 seconds."
                ));
            }
        }
        if let Some(dependencies) = &service.depends_on {
            let mut seen = HashSet::new();
            if dependencies.len() > MAX_SERVICES || dependencies.iter().any(|id|
                id.trim().is_empty() || id.len() > 96 || id == &service.id || !seen.insert(id)) {
                return invalid(format!("Choose each startup requirement for “{name}” once, without the service itself."));
            }
        }
    }
    Ok(config)
}

fn service_directory(root: &Path, relative: &str) -> Result<PathBuf, EngineError> {
    let root = std::fs::canonicalize(root)?;
    let path = std::fs::canonicalize(root.join(relative))
        .map_err(|_| EngineError::Other(format!("Service directory is unavailable: {relative}")))?;
    if !path.is_dir() || !path.starts_with(&root) {
        return Err(EngineError::Other(format!(
            "Service directory must stay inside the project: {relative}"
        )));
    }
    Ok(path)
}
