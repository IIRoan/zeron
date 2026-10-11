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
    ProjectTerminalStatus as Status, ProjectTerminalsSnapshot, TerminalEvent,
};

use crate::{EngineError, Terminals};

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
                config: ProjectTerminalConfig::default(),
                runs: Vec::new(),
                checkout: None,
            });
        };
        check_root(project, &root)?;
        Ok(ProjectTerminalsSnapshot {
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
        let session = match terminals.open_service(
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

    /// Cancel supervision before the engine closes its PTYs, so shutdown can
    /// never be mistaken for a crash that should restart a service.
    pub fn shutdown(&self) {
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
        let operation = self
            .inner
            .managed_operations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(space.into())
            .or_default()
            .clone();
        let _guard = operation.lock().await;
        let this = self.clone();
        let environments = environments.clone();
        let space = space.to_string();
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let mut defaults = environments.defaults(&root)?;
            if defaults.services.is_some() {
                defaults.services = Some(config.clone());
                environments.save(&root, None, Some(defaults))?;
            }
            this.reconcile_configs(&environments, &space, &root, config)
        })
        .await
        .map_err(|e| EngineError::Other(e.to_string()))?
    }

    pub async fn save_environment(
        &self,
        environments: &crate::worktree_settings::WorktreeSettingsStore,
        space: &str,
        root: &Path,
        checkout: Option<&Path>,
        settings: Option<zeron_proto::WorktreeSettings>,
    ) -> Result<(), EngineError> {
        let operation = self
            .inner
            .managed_operations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(space.into())
            .or_default()
            .clone();
        let _guard = operation.lock().await;
        let this = self.clone();
        let environments = environments.clone();
        let space = space.to_string();
        let root = root.to_path_buf();
        let checkout = checkout.map(Path::to_path_buf);
        tokio::task::spawn_blocking(move || {
            if checkout.is_some()
                && let Some(services) = settings.as_ref().and_then(|s| s.services.as_ref())
            {
                let defaults = this.snapshot(&space, &root)?.config;
                let shared = |config: &ProjectTerminalConfig| {
                    config
                        .services
                        .iter()
                        .filter(|s| s.setup_prerequisite.is_some())
                        .cloned()
                        .collect::<Vec<_>>()
                };
                if shared(services) != shared(&defaults) {
                    return Err(EngineError::Other(
                        "Shared startup services must be edited in Project defaults.".into(),
                    ));
                }
            }
            environments.save(&root, checkout.as_deref(), settings)?;
            let config = this.snapshot(&space, &root)?.config;
            if let Some(cwd) = checkout {
                let settings = environments.effective(&root, &cwd)?;
                this.save_checkout(&space, &root, &cwd, settings.services.unwrap_or(config))
            } else {
                let defaults = environments.defaults(&root)?;
                this.reconcile_configs(
                    &environments,
                    &space,
                    &root,
                    defaults.services.unwrap_or(config),
                )
            }
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
    pub(crate) async fn ensure_setup_services(
        &self,
        terminals: &Terminals,
        root: &Path,
        settings: &zeron_proto::WorktreeSettings,
        log: &Path,
        token: &CancellationToken,
    ) -> Result<(), EngineError> {
        let source = canonical_root(root)?;
        let services = {
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
                        .filter(|s| s.setup_prerequisite.is_some())
                        .cloned()
                        .map(|s| (space.clone(), s))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        for (space, service) in services {
            if token.is_cancelled() {
                return Err(EngineError::Other("Worktree setup was cancelled.".into()));
            }
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
            loop {
                if token.is_cancelled() {
                    return Err(EngineError::Other("Worktree setup was cancelled.".into()));
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
                    return Err(EngineError::Other(format!(
                        "{} was not ready after {} seconds. Open its service and setup output for details.",
                        service.name, readiness.timeout_seconds
                    )));
                }
                let mut check_settings = settings.clone();
                check_settings.command_timeout_seconds = remaining.as_secs().clamp(1, 5);
                if crate::worktree_settings::commands::run(
                    root,
                    root,
                    &check_settings,
                    &readiness.command,
                    log,
                    token,
                    true,
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
                        break;
                    }
                }
                tokio::select! {
                    _ = token.cancelled() => return Err(EngineError::Other("Worktree setup was cancelled.".into())),
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
        let worktree_only = action == "stop-worktree";
        if worktree_only && service_id.is_some() {
            return Err(EngineError::Other("Stop worktree services does not take a service id.".into()));
        }
        let action = if worktree_only { "stop" } else { action };
        if !matches!(action, "start" | "stop" | "restart" | "activate") {
            return Err(EngineError::Other("Unknown terminal action".into()));
        }
        let operation = self
            .inner
            .managed_operations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(space.into())
            .or_default()
            .clone();
        let _guard = operation.lock().await;
        let settings = environments.effective(root, checkout)?;
        let config = self
            .checkout_snapshot(space, root, checkout, &settings)?
            .config;
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
            let operation = action.to_string();
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
            if action != "stop" {
                let log = self.readiness_log(root)?;
                self.ensure_setup_services(
                    terminals,
                    root,
                    &environments.defaults(root)?,
                    &log,
                    &CancellationToken::new(),
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
        let mut restart = HashSet::new();
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
            restart.extend(services.iter().cloned());
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
        if action != "stop" {
            if !shared.is_empty()
                && root.canonicalize()? == checkout.canonicalize()?
            {
                let log = self.readiness_log(root)?;
                self.ensure_setup_services(
                    terminals,
                    root,
                    &environments.defaults(root)?,
                    &log,
                    &CancellationToken::new(),
                )
                .await?;
            }
            environments.activate(root, checkout).await?;
        }
        if action != "stop" {
            // Validate the entire handoff before starting any replacement process.
            for service in config.services.iter().filter(|s| {
                s.setup_prerequisite.is_none()
                    && (restart.contains(&s.id)
                        || (action != "activate" && service_id.is_none_or(|id| id == s.id)))
            }) {
                service_directory(checkout, &service.directory)?;
            }
            for id in restart
                .into_iter()
                .filter(|id| config.services.iter().any(|s| &s.id == id))
            {
                let this = self.clone();
                let terminals = terminals.clone();
                let root = root.to_path_buf();
                let checkout = checkout.to_path_buf();
                let key = key.clone();
                tokio::task::spawn_blocking(move || {
                    this.control_in_checkout(
                        &terminals,
                        &key,
                        &root,
                        Some(&checkout),
                        Some(&id),
                        "start",
                    )
                })
                .await
                .map_err(|e| EngineError::Other(e.to_string()))??;
            }
        }
        let this = self.clone();
        let terminals = terminals.clone();
        let source = root.to_path_buf();
        let destination = checkout.to_path_buf();
        let key = key.clone();
        let service = service_id.map(str::to_string);
        let action = action.to_string();
        if action != "activate" && config.services.is_empty() {
            return Err(EngineError::Other(
                "No saved terminal services selected".into(),
            ));
        }
        if !config
            .services
            .iter()
            .any(|s| s.setup_prerequisite.is_none())
            && service_id.is_none()
        {
            return self.checkout_snapshot(space, root, checkout, &settings);
        }
        tokio::task::spawn_blocking(move || {
            this.control_in_checkout(
                &terminals,
                &key,
                &source,
                Some(&destination),
                service.as_deref(),
                &action,
            )
        })
        .await
        .map_err(|e| EngineError::Other(e.to_string()))??;
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
        let operation = self
            .inner
            .managed_operations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(space.into())
            .or_default()
            .clone();
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
            // Spawning a shell blocks; keep it off the async workers.
            tokio::task::spawn_blocking(move || {
                warm_login_path();
                let mut live = store.inner.live.lock().unwrap_or_else(|e| e.into_inner());
                if token.is_cancelled() {
                    return None;
                }
                match terminals.open_service(&directory.to_string_lossy(), &environment, &command) {
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

pub fn normalize_config(
    mut config: ProjectTerminalConfig,
) -> Result<ProjectTerminalConfig, EngineError> {
    if config.services.len() > MAX_SERVICES {
        return Err(EngineError::Other(
            "A project supports at most 12 named services".into(),
        ));
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for service in &mut config.services {
        if let Some(readiness) = &mut service.setup_prerequisite {
            readiness.command = readiness.command.trim().to_string();
            if readiness.command.is_empty()
                || readiness.command.len() > 16 * 1024
                || readiness.command.contains('\0')
                || !(1..=zeron_proto::MAX_WORKTREE_COMMAND_TIMEOUT_SECONDS)
                    .contains(&readiness.timeout_seconds)
            {
                return Err(EngineError::Other("Startup services need a readiness command and a timeout between 1 and 7200 seconds.".into()));
            }
        }
        service.name = service.name.trim().to_string();
        service.command = service.command.trim().to_string();
        service.directory = service.directory.trim().to_string();
        if service.directory.is_empty() {
            service.directory = ".".into();
        }
        if service.id.is_empty()
            || service.id.len() > 96
            || !ids.insert(service.id.clone())
            || service.name.is_empty()
            || service.name.chars().count() > 80
            || !names.insert(service.name.to_lowercase())
            || service.command.is_empty()
            || service.command.len() > 16 * 1024
            || service.command.contains('\0')
            || service.directory.len() > 4096
            || service.directory.contains('\0')
            || Path::new(&service.directory).components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(EngineError::Other("Services need unique names and ids, a command, and a relative directory inside the project".into()));
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
