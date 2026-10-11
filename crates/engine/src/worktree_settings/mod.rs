//! Worktree environment lifecycle. Profiles and secrets remain on this device;
//! preparing a checkout never blocks the UI thread or a registry watch.
pub(crate) mod commands;
mod files;

use crate::EngineError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
use zeron_proto::*;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Checkout {
    settings: Option<WorktreeSettings>,
    /// The checkout's own app services, kept apart from `settings` so that
    /// customizing services never freezes its inherited environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    services: Option<ProjectTerminalConfig>,
    branch: Option<String>,
    prefix: String,
    prepared_hash: String,
    state: WorktreeSetupState,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Project {
    defaults: WorktreeSettings,
    active: Option<String>,
    checkouts: BTreeMap<String, Checkout>,
    /// Project services that older versions stored in `defaults`. The project
    /// service profile adopts them before this is cleared.
    #[serde(skip_serializing_if = "Option::is_none")]
    legacy_services: Option<ProjectTerminalConfig>,
}

type Discovered = (Vec<String>, Vec<String>);

struct Inner {
    directory: PathBuf,
    projects: Mutex<Option<BTreeMap<String, Project>>>,
    operations: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    cancellation: Mutex<HashMap<String, CancellationToken>>,
    discovery: Mutex<HashMap<String, (std::time::Instant, Discovered)>>,
    /// Checkouts whose removal has started. A setup queued behind the removal
    /// must not recreate files in a folder that is about to disappear.
    removing: Mutex<HashSet<String>>,
    stopping: std::sync::atomic::AtomicBool,
    project_services: Mutex<Option<(crate::project_terminals::ProjectTerminals, crate::Terminals)>>,
}

#[derive(Clone)]
pub struct WorktreeSettingsStore {
    inner: Arc<Inner>,
}

impl WorktreeSettingsStore {
    pub fn new(directory: &Path) -> Self {
        Self {
            inner: Arc::new(Inner {
                directory: directory.join("worktree-environments"),
                projects: Mutex::new(None),
                operations: Mutex::new(HashMap::new()),
                cancellation: Mutex::new(HashMap::new()),
                discovery: Mutex::new(HashMap::new()),
                removing: Mutex::new(HashSet::new()),
                stopping: std::sync::atomic::AtomicBool::new(false),
                project_services: Mutex::new(None),
            }),
        }
    }

    pub fn bind_project_services(
        &self,
        services: crate::project_terminals::ProjectTerminals,
        terminals: crate::Terminals,
    ) {
        *self
            .inner
            .project_services
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((services, terminals));
    }

    fn with<T>(
        &self,
        update: bool,
        f: impl FnOnce(&mut BTreeMap<String, Project>) -> Result<T, EngineError>,
    ) -> Result<T, EngineError> {
        let mut guard = self
            .inner
            .projects
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let mut projects: BTreeMap<String, Project> =
                match std::fs::read(self.inner.directory.join("settings.json")) {
                    Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                        EngineError::Other(
                            "Worktree settings could not be read; the existing file was preserved."
                                .into(),
                        )
                    })?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                    Err(e) => return Err(e.into()),
                };
            if migrate_services(&mut projects) {
                // Keep the file as it was before the format changed.
                let path = self.inner.directory.join("settings.json");
                let backup = path.with_file_name(format!(
                    "settings.json.before-service-migration-{}",
                    chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
                ));
                files::private_write(&backup, &std::fs::read(&path)?, false)?;
                tracing::info!(backup = %backup.display(), "moved service lists out of worktree settings");
            }
            for project in projects.values_mut() {
                for checkout in project.checkouts.values_mut() {
                    if checkout.state.phase == WorktreeSetupPhase::Preparing {
                        checkout.state.phase = WorktreeSetupPhase::Interrupted;
                        checkout.state.error = Some(
                            "Setup was interrupted. Retry it before starting services.".into(),
                        );
                    }
                }
            }
            *guard = Some(projects);
        }
        let projects = guard.as_mut().unwrap();
        let mut next = projects.clone();
        let result = f(&mut next)?;
        if update {
            files::private_directory(&self.inner.directory)?;
            let mut file = tempfile::NamedTempFile::new_in(&self.inner.directory)?;
            file.write_all(
                &serde_json::to_vec_pretty(&next).map_err(|e| EngineError::Other(e.to_string()))?,
            )?;
            file.as_file().sync_all()?;
            file.persist(self.inner.directory.join("settings.json"))
                .map_err(|e| e.error)?;
            *projects = next;
        }
        Ok(result)
    }

    pub fn defaults(&self, root: &Path) -> Result<WorktreeSettings, EngineError> {
        let key = canonical(root)?;
        if let Some(settings) = self.with(false, |p| Ok(p.get(&key).map(|p| p.defaults.clone())))? {
            return Ok(settings);
        }
        let detected = self.detect(root)?;
        let settings = WorktreeSettings {
            env_files: detected
                .0
                .into_iter()
                .map(|path| WorktreeEnvFile {
                    path,
                    mode: WorktreeEnvMode::Follow,
                })
                .collect(),
            dependencies: if detected.1.is_empty() {
                WorktreeDependencyMode::Install
            } else {
                WorktreeDependencyMode::Copy
            },
            dependency_paths: if detected.1.is_empty() {
                vec!["node_modules".into()]
            } else {
                detected.1
            },
            ..WorktreeSettings::default()
        };
        self.with(true, |p| {
            Ok(p.entry(key)
                .or_insert_with(|| Project {
                    defaults: settings,
                    ..Project::default()
                })
                .defaults
                .clone())
        })
    }

    pub fn effective(&self, root: &Path, checkout: &Path) -> Result<WorktreeSettings, EngineError> {
        let key = canonical(root)?;
        let cwd = canonical(checkout)?;
        let defaults = self.defaults(root)?;
        self.with(false, |projects| {
            let checkout = projects.get(&key).and_then(|p| p.checkouts.get(&cwd));
            let mut settings = checkout
                .and_then(|c| c.settings.clone())
                .unwrap_or(defaults.clone());
            settings.services = checkout.and_then(|c| c.services.clone());
            // A project's concurrency policy must be consistent across all its checkouts.
            settings.service_mode = defaults.service_mode;
            Ok(settings)
        })
    }

    /// Sets or clears (None) a checkout's own app services. Its environment
    /// settings, override or inherited, are unchanged.
    pub fn save_services(
        &self,
        root: &Path,
        checkout: &Path,
        services: Option<ProjectTerminalConfig>,
    ) -> Result<(), EngineError> {
        let key = canonical(root)?;
        let cwd = canonical(checkout)?;
        if key == cwd {
            return Err(EngineError::Other(
                "The main project folder uses the project's services.".into(),
            ));
        }
        let services = services
            .map(crate::project_terminals::normalize_config)
            .transpose()?;
        let defaults = self.defaults(root)?;
        self.with(true, |projects| {
            projects
                .entry(key)
                .or_insert_with(|| Project {
                    defaults,
                    ..Project::default()
                })
                .checkouts
                .entry(cwd)
                .or_default()
                .services = services;
            Ok(())
        })
    }

    pub(crate) fn legacy_services(
        &self,
        root: &Path,
    ) -> Result<Option<ProjectTerminalConfig>, EngineError> {
        let key = canonical(root)?;
        self.with(false, |p| {
            Ok(p.get(&key).and_then(|p| p.legacy_services.clone()))
        })
    }

    pub(crate) fn clear_legacy_services(&self, root: &Path) -> Result<(), EngineError> {
        let key = canonical(root)?;
        self.with(true, |p| {
            if let Some(p) = p.get_mut(&key) {
                p.legacy_services = None;
            }
            Ok(())
        })
    }

    pub fn save(
        &self,
        root: &Path,
        checkout: Option<&Path>,
        settings: Option<WorktreeSettings>,
    ) -> Result<(), EngineError> {
        let key = canonical(root)?;
        let cwd = checkout.map(canonical).transpose()?;
        // Services are saved separately (see `save_services`); a settings page
        // that still carries a copy of them can never overwrite them.
        let settings = settings
            .map(|settings| {
                normalize(WorktreeSettings {
                    services: None,
                    ..settings
                })
            })
            .transpose()?;
        if cwd.is_none() && settings.is_none() {
            return Err(EngineError::Other(
                "Project defaults cannot be removed.".into(),
            ));
        }
        let defaults = self.defaults(root)?;
        self.with(true, |projects| {
            let project = projects.entry(key).or_insert_with(|| Project { defaults, ..Project::default() });
            if let Some(cwd) = cwd { project.checkouts.entry(cwd).or_default().settings = settings; }
            else { project.defaults = settings.unwrap(); }
            if project.defaults.service_mode == WorktreeServiceMode::Parallel &&
                (project.defaults.env_files.iter().any(|f| f.mode == WorktreeEnvMode::Follow) || project.checkouts.values().filter_map(|c| c.settings.as_ref()).any(|s| s.env_files.iter().any(|f| f.mode == WorktreeEnvMode::Follow))) {
                return Err(EngineError::Other("Env files that follow your conversation require one active service group. Choose independent copies or shared links for parallel services.".into()));
            }
            Ok(())
        })
    }

    pub fn remember(
        &self,
        root: &Path,
        checkout: &Path,
        branch: &str,
        prefix: &str,
    ) -> Result<(), EngineError> {
        let key = canonical(root)?;
        let cwd = canonical(checkout)?;
        let defaults = self.defaults(root)?;
        self.with(true, |projects| {
            let c = projects
                .entry(key)
                .or_insert_with(|| Project {
                    defaults,
                    ..Project::default()
                })
                .checkouts
                .entry(cwd)
                .or_default();
            c.branch = Some(branch.into());
            c.prefix = prefix.into();
            Ok(())
        })
    }

    pub fn owned_branch(
        &self,
        root: &Path,
        checkout: &Path,
    ) -> Result<Option<(String, String)>, EngineError> {
        let key = canonical(root)?;
        let cwd = checkout
            .canonicalize()
            .unwrap_or_else(|_| checkout.into())
            .to_string_lossy()
            .into_owned();
        self.with(false, |p| {
            Ok(p.get(&key)
                .and_then(|p| p.checkouts.get(&cwd))
                .and_then(|c| c.branch.clone().map(|b| (b, c.prefix.clone()))))
        })
    }

    pub fn forget(&self, root: &Path, checkout: &Path) -> Result<(), EngineError> {
        let key = canonical(root)?;
        let cwd = checkout
            .canonicalize()
            .unwrap_or_else(|_| checkout.into())
            .to_string_lossy()
            .into_owned();
        self.with(true, |p| {
            if let Some(p) = p.get_mut(&key) {
                p.checkouts.remove(&cwd);
                if p.active.as_deref() == Some(&cwd) {
                    p.active = None;
                }
            }
            Ok(())
        })
    }

    /// Ends a removal started by [`Self::cleanup`], whether or not it succeeded,
    /// so an explicit setup can run again in a checkout that was kept.
    pub fn finish_removal(&self, checkout: &Path) {
        let cwd = checkout
            .canonicalize()
            .unwrap_or_else(|_| checkout.into())
            .to_string_lossy()
            .into_owned();
        self.inner
            .removing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&cwd);
    }

    pub fn snapshot(
        &self,
        root: &Path,
        checkout: Option<&Path>,
        mut worktrees: Vec<(String, String)>,
    ) -> Result<WorktreeSettingsSnapshot, EngineError> {
        let defaults = self.defaults(root)?;
        let settings = match checkout {
            Some(c) => self.effective(root, c)?,
            None => defaults.clone(),
        };
        let key = canonical(root)?;
        let cwd = checkout.map(canonical).transpose()?;
        let detected = self.detect(root)?;
        // Folders deleted outside Zeron leave entries behind. A checkout that
        // is still being created has no folder yet and keeps its progress.
        let stale = |path: &String, c: &Checkout| {
            c.state.phase != WorktreeSetupPhase::Preparing && !Path::new(path).is_dir()
        };
        if self.with(false, |p| {
            Ok(p.get(&key)
                .is_some_and(|p| p.checkouts.iter().any(|(path, c)| stale(path, c))))
        })? {
            self.with(true, |p| {
                if let Some(p) = p.get_mut(&key) {
                    p.checkouts.retain(|path, c| !stale(path, c));
                }
                Ok(())
            })?;
        }
        self.with(false, |projects| {
            let project = projects.get(&key);
            let has_overrides = cwd
                .as_ref()
                .and_then(|c| project.and_then(|p| p.checkouts.get(c)))
                .is_some_and(|c| c.settings.is_some());
            if let Some(p) = project {
                for (path, c) in &p.checkouts {
                    if !worktrees.iter().any(|(p, _)| p == path) {
                        worktrees.push((path.clone(), c.branch.clone().unwrap_or_default()));
                    }
                }
            }
            Ok(WorktreeSettingsSnapshot {
                defaults,
                settings,
                checkout: cwd,
                has_overrides,
                detected_env_files: detected.0,
                detected_dependency_paths: detected.1,
                detected_install_command: detect_install(root),
                detected_workflow: root
                    .join("tools/worktrees/src/commands/codex-sync.ts")
                    .is_file()
                    .then(|| "muute".into()),
                databases: Vec::new(),
                app_services: Vec::new(),
                worktrees: worktrees
                    .into_iter()
                    .map(|(path, branch)| {
                        let c = project.and_then(|p| p.checkouts.get(&path));
                        WorktreeEnvironmentEntry {
                            path,
                            branch,
                            has_overrides: c
                                .is_some_and(|c| c.settings.is_some() || c.services.is_some()),
                            managed: c.is_some_and(|c| c.branch.is_some()),
                            state: c.map(|c| c.state.clone()).unwrap_or_default(),
                        }
                    })
                    .collect(),
            })
        })
    }

    fn detect(&self, root: &Path) -> Result<(Vec<String>, Vec<String>), EngineError> {
        let key = canonical(root)?;
        if let Some((when, files)) = self
            .inner
            .discovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            && when.elapsed() < std::time::Duration::from_secs(60)
        {
            return Ok(files.clone());
        }
        let detected = files::detect(root);
        self.inner
            .discovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, (std::time::Instant::now(), detected.clone()));
        Ok(detected)
    }

    fn operation(&self, root: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.inner
            .operations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(root.into())
            .or_default()
            .clone()
    }

    pub fn shutdown(&self) {
        self.inner
            .stopping
            .store(true, std::sync::atomic::Ordering::Release);
        for token in self
            .inner
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            token.cancel();
        }
    }

    fn ensure_running(&self) -> Result<(), EngineError> {
        if self
            .inner
            .stopping
            .load(std::sync::atomic::Ordering::Acquire)
        {
            Err(EngineError::Other(
                "The engine is stopping; environment setup was cancelled.".into(),
            ))
        } else {
            Ok(())
        }
    }

    fn setup_token(&self, cwd: &str) -> CancellationToken {
        let token = CancellationToken::new();
        let mut tokens = self
            .inner
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self
            .inner
            .stopping
            .load(std::sync::atomic::Ordering::Acquire)
        {
            token.cancel();
        }
        tokens.insert(cwd.into(), token.clone());
        token
    }

    pub fn cancel(&self, checkout: &Path) -> Result<(), EngineError> {
        let cwd = canonical(checkout)?;
        if let Some(token) = self
            .inner
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&cwd)
        {
            token.cancel();
        }
        Ok(())
    }

    fn state(
        &self,
        root: &str,
        checkout: &str,
        phase: WorktreeSetupPhase,
        step: &str,
        error: Option<String>,
    ) -> Result<(), EngineError> {
        let log_available = self.log_path(root, checkout).is_file();
        self.with(true, |p| {
            let checkout = p
                .get_mut(root)
                .ok_or_else(|| {
                    EngineError::Other("Project environment was not initialized.".into())
                })?
                .checkouts
                .entry(checkout.into())
                .or_default();
            checkout.state.phase = phase;
            checkout.state.step = step.into();
            checkout.state.error = error;
            checkout.state.log_available = log_available;
            Ok(())
        })
    }

    fn log_path(&self, root: &str, checkout: &str) -> PathBuf {
        self.inner
            .directory
            .join("logs")
            .join(hash(root))
            .join(format!("{}.log", hash(checkout)))
    }

    pub fn log(&self, root: &Path, checkout: &Path) -> Result<String, EngineError> {
        let path = self.log_path(&canonical(root)?, &canonical(checkout)?);
        match std::fs::read(path) {
            Ok(bytes) => {
                Ok(String::from_utf8_lossy(&bytes[..bytes.len().min(256 * 1024)]).into_owned())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("No setup output yet.".into()),
            Err(e) => Err(e.into()),
        }
    }

    /// Idempotent for an unchanged successful configuration; retry explicitly
    /// reruns custom setup but never overwrites independent env copies.
    pub async fn prepare(
        &self,
        root: &Path,
        checkout: &Path,
        retry: bool,
    ) -> Result<(), EngineError> {
        let root_key = canonical(root)?;
        let cwd = canonical(checkout)?;
        if root_key == cwd {
            return Ok(());
        }
        let operation = self.operation(&root_key);
        let _guard = operation.lock().await;
        self.capture_active(root)?;
        self.prepare_locked(root, checkout, retry).await
    }

    /// Agent dispatch prepares only worktrees Zeron created. A worktree made
    /// outside Zeron keeps its files until the user starts a service or
    /// prepares it explicitly.
    pub async fn prepare_managed(&self, root: &Path, checkout: &Path) -> Result<(), EngineError> {
        if self.owned_branch(root, checkout)?.is_none() {
            return Ok(());
        }
        self.prepare(root, checkout, false).await
    }

    pub fn is_prepared(&self, root: &Path, checkout: &Path) -> Result<bool, EngineError> {
        let key = canonical(root)?;
        let cwd = canonical(checkout)?;
        if key == cwd {
            return Ok(true);
        }
        let digest = preparation_hash(&self.effective(root, checkout)?)?;
        self.with(false, |p| {
            Ok(p.get(&key)
                .and_then(|p| p.checkouts.get(&cwd))
                .is_some_and(|c| {
                    c.state.phase == WorktreeSetupPhase::Ready && c.prepared_hash == digest
                }))
        })
    }

    async fn prepare_locked(
        &self,
        root: &Path,
        checkout: &Path,
        retry: bool,
    ) -> Result<(), EngineError> {
        self.ensure_running()?;
        let root_key = canonical(root)?;
        let cwd = canonical(checkout)?;
        if self
            .inner
            .removing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&cwd)
        {
            return Err(EngineError::Other(
                "This worktree is being removed; its environment was not prepared.".into(),
            ));
        }
        let settings = self.effective(root, checkout)?;
        let digest = preparation_hash(&settings)?;
        let already_prepared = !retry
            && self.with(false, |p| {
                Ok(p.get(&root_key)
                    .and_then(|p| p.checkouts.get(&cwd))
                    .is_some_and(|c| c.prepared_hash == digest))
            })?;
        let project_services = self
            .inner
            .project_services
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if already_prepared
            && !project_services
                .as_ref()
                .map(|(services, _)| services.has_setup_services(root))
                .transpose()?
                .unwrap_or(false)
        {
            self.state(
                &root_key,
                &cwd,
                WorktreeSetupPhase::Ready,
                "Environment ready",
                None,
            )?;
            return Ok(());
        }
        let defaults = self.defaults(root)?;
        self.with(true, |p| {
            p.entry(root_key.clone())
                .or_insert_with(|| Project {
                    defaults: defaults.clone(),
                    ..Project::default()
                })
                .checkouts
                .entry(cwd.clone())
                .or_default();
            if !already_prepared {
                p.get_mut(&root_key)
                    .unwrap()
                    .checkouts
                    .get_mut(&cwd)
                    .unwrap()
                    .prepared_hash
                    .clear();
            }
            Ok(())
        })?;
        let token = self.setup_token(&cwd);
        let mut guard = SetupGuard {
            store: self.clone(),
            root: root_key.clone(),
            checkout: cwd.clone(),
            token: token.clone(),
            complete: false,
        };
        self.state(
            &root_key,
            &cwd,
            WorktreeSetupPhase::Preparing,
            "Waiting for project services",
            None,
        )?;
        let log = self.log_path(&root_key, &cwd);
        files::private_directory(log.parent().unwrap())?;
        if !already_prepared {
            files::private_write(&log, b"", false)?;
        }
        let this = self.clone();
        let source = root.to_path_buf();
        let destination = checkout.to_path_buf();
        let config = settings.clone();
        let cancelled = token.clone();
        let result = async {
            if let Some((services, terminals)) = project_services {
                services
                    .ensure_setup_services(&terminals, root, &defaults, &log, &token)
                    .await?;
            }
            if already_prepared {
                return Ok(());
            }
            self.state(
                &root_key,
                &cwd,
                WorktreeSetupPhase::Preparing,
                "Preparing environment files",
                None,
            )?;
            tokio::task::spawn_blocking(move || {
                this.prepare_files(&source, &destination, &config, &cancelled)
            })
            .await
            .map_err(|e| EngineError::Other(e.to_string()))??;
            if settings.dependencies == WorktreeDependencyMode::Install
                && checkout.join("package.json").is_file()
            {
                self.state(
                    &root_key,
                    &cwd,
                    WorktreeSetupPhase::Preparing,
                    "Installing dependencies",
                    None,
                )?;
                let command = if settings.install_command.trim().is_empty() {
                    detect_install(checkout)
                } else {
                    settings.install_command.clone()
                };
                commands::run(root, checkout, &settings, &command, &log, &token, false).await?;
            }
            if !settings.setup_command.trim().is_empty() {
                self.state(
                    &root_key,
                    &cwd,
                    WorktreeSetupPhase::Preparing,
                    "Running setup workflow",
                    None,
                )?;
                commands::run(
                    root,
                    checkout,
                    &settings,
                    &settings.setup_command,
                    &log,
                    &token,
                    settings.commands_in_project,
                )
                .await?;
            }
            files::check_cancel(&token)?;
            Ok::<_, EngineError>(())
        }
        .await;
        match &result {
            Ok(()) => {
                self.with(true, |p| {
                    let c = p
                        .get_mut(&root_key)
                        .unwrap()
                        .checkouts
                        .get_mut(&cwd)
                        .unwrap();
                    c.prepared_hash = digest;
                    c.state = WorktreeSetupState {
                        phase: WorktreeSetupPhase::Ready,
                        step: "Environment ready".into(),
                        error: None,
                        log_available: true,
                    };
                    Ok(())
                })?;
            }
            Err(error) => {
                self.state(
                    &root_key,
                    &cwd,
                    if token.is_cancelled() {
                        WorktreeSetupPhase::Interrupted
                    } else {
                        WorktreeSetupPhase::Failed
                    },
                    "Setup needs attention",
                    Some(error.to_string()),
                )?;
            }
        }
        guard.complete = true;
        self.inner
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&cwd);
        result
    }

    fn prepare_files(
        &self,
        root: &Path,
        checkout: &Path,
        settings: &WorktreeSettings,
        token: &CancellationToken,
    ) -> Result<(), EngineError> {
        for rule in &settings.env_files {
            files::check_cancel(token)?;
            match rule.mode {
                WorktreeEnvMode::Copy => files::copy_env(root, checkout, &rule.path)?,
                WorktreeEnvMode::Link => files::link(root, checkout, &rule.path)?,
                WorktreeEnvMode::Follow => {
                    self.follow_file(root, checkout, settings, &rule.path)?
                }
                WorktreeEnvMode::Skip => {}
            }
        }
        if matches!(
            settings.dependencies,
            WorktreeDependencyMode::Copy | WorktreeDependencyMode::Link
        ) {
            self.state(
                &canonical(root)?,
                &canonical(checkout)?,
                WorktreeSetupPhase::Preparing,
                if settings.dependencies == WorktreeDependencyMode::Copy {
                    "Copying node_modules"
                } else {
                    "Linking node_modules"
                },
                None,
            )?;
        }
        for path in &settings.dependency_paths {
            files::check_cancel(token)?;
            match settings.dependencies {
                WorktreeDependencyMode::Install => {
                    // Do not run an installer through a previously shared
                    // node_modules link and mutate the project's dependencies.
                    files::destination(checkout, path)?;
                }
                WorktreeDependencyMode::Link => files::link(root, checkout, path)?,
                WorktreeDependencyMode::Copy => {
                    files::copy_dependencies(root, checkout, path, token)?
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn vault_path(
        &self,
        root: &Path,
        settings: &WorktreeSettings,
        path: &str,
    ) -> Result<PathBuf, EngineError> {
        Ok(self
            .inner
            .directory
            .join("env-profiles")
            .join(hash(&canonical(root)?))
            .join(hash(&settings.env_profile))
            .join(hash(path)))
    }

    fn capture_active(&self, root: &Path) -> Result<(), EngineError> {
        let key = canonical(root)?;
        self.ensure_running()?;
        let previous = self.active(&key)?;
        if Path::new(&previous).is_dir() {
            let settings = self.effective(root, Path::new(&previous))?;
            self.capture_follow(root, Path::new(&previous), &settings)?;
        }
        Ok(())
    }

    /// Until another checkout is activated, the project folder holds the
    /// followed env files, so its edits must be captured like any other.
    fn active(&self, key: &str) -> Result<String, EngineError> {
        self.with(false, |p| {
            Ok(p.get(key)
                .and_then(|p| p.active.clone())
                .unwrap_or_else(|| key.to_string()))
        })
    }

    /// A followed file that is missing or unreadable in the outgoing checkout
    /// keeps the last captured copy; it must not block switching or removal.
    fn capture_follow(
        &self,
        root: &Path,
        checkout: &Path,
        settings: &WorktreeSettings,
    ) -> Result<(), EngineError> {
        for rule in settings
            .env_files
            .iter()
            .filter(|r| r.mode == WorktreeEnvMode::Follow)
        {
            let bytes = files::source(checkout, &rule.path).and_then(|file| files::read_env(&file));
            match bytes {
                Ok(bytes) => files::private_write(
                    &self.vault_path(root, settings, &rule.path)?,
                    &bytes,
                    true,
                )?,
                Err(error) => tracing::warn!(
                    checkout = %checkout.display(),
                    path = %rule.path,
                    %error,
                    "followed env file was not captured"
                ),
            }
        }
        Ok(())
    }

    fn follow_file(
        &self,
        root: &Path,
        checkout: &Path,
        settings: &WorktreeSettings,
        path: &str,
    ) -> Result<(), EngineError> {
        let vault = self.vault_path(root, settings, path)?;
        if !vault.exists() {
            let source = files::source(root, path)?;
            files::private_write(&vault, &files::read_env(&source)?, false)?;
        }
        let target = files::destination(checkout, path)?;
        if target.is_file() {
            let existing = files::read_env(&target)?;
            let next = files::read_env(&vault)?;
            if existing == next {
                return Ok(());
            }
            // A follow-mode handoff retains an edited destination before replacing it.
            let backup = self
                .inner
                .directory
                .join("env-backups")
                .join(uuid::Uuid::new_v4().to_string())
                .join(hash(path));
            files::private_write(&backup, &existing, false)?;
            files::private_write(&backup.parent().unwrap().join("metadata.json"),&serde_json::to_vec_pretty(&serde_json::json!({"project":root,"checkout":checkout,"path":path,"profile":settings.env_profile,"createdAt":chrono::Utc::now()})).map_err(|e|EngineError::Other(e.to_string()))?,false)?;
        }
        files::private_write(&target, &files::read_env(&vault)?, true)
    }

    /// Called only by foreground selection or an explicit Start, after service
    /// teardown. Background polls have no environment side effects.
    pub async fn activate(&self, root: &Path, checkout: &Path) -> Result<(), EngineError> {
        let key = canonical(root)?;
        let cwd = canonical(checkout)?;
        let operation = self.operation(&key);
        let _guard = operation.lock().await;
        let previous = Some(self.active(&key)?);
        self.capture_active(root)?;
        if cwd != key {
            self.prepare_locked(root, checkout, false).await?;
        }
        let settings = self.effective(root, checkout)?;
        let defaults = self.defaults(root)?;
        self.with(true, |p| {
            p.entry(key.clone()).or_insert_with(|| Project {
                defaults,
                ..Project::default()
            });
            Ok(())
        })?;
        if previous.as_deref() != Some(&cwd) {
            for rule in settings
                .env_files
                .iter()
                .filter(|r| r.mode == WorktreeEnvMode::Follow)
            {
                self.follow_file(root, checkout, &settings, &rule.path)?;
            }
            if !settings.activate_command.trim().is_empty() {
                let token = self.setup_token(&cwd);
                let mut guard = SetupGuard {
                    store: self.clone(),
                    root: key.clone(),
                    checkout: cwd.clone(),
                    token: token.clone(),
                    complete: false,
                };
                self.state(
                    &key,
                    &cwd,
                    WorktreeSetupPhase::Preparing,
                    "Activating worktree",
                    None,
                )?;
                let log = self.log_path(&key, &cwd);
                let result = commands::run(
                    root,
                    checkout,
                    &settings,
                    &settings.activate_command,
                    &log,
                    &token,
                    settings.commands_in_project,
                )
                .await;
                guard.complete = true;
                self.inner
                    .cancellation
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&cwd);
                if let Err(error) = result {
                    self.state(
                        &key,
                        &cwd,
                        if token.is_cancelled() {
                            WorktreeSetupPhase::Interrupted
                        } else {
                            WorktreeSetupPhase::Failed
                        },
                        "Activation needs attention",
                        Some(error.to_string()),
                    )?;
                    return Err(error);
                }
                self.state(
                    &key,
                    &cwd,
                    WorktreeSetupPhase::Ready,
                    "Environment ready",
                    None,
                )?;
            }
            self.with(true, |p| {
                p.get_mut(&key).unwrap().active = Some(cwd.clone());
                Ok(())
            })?;
        }
        Ok(())
    }

    pub async fn cleanup(&self, root: &Path, checkout: &Path) -> Result<(), EngineError> {
        let key = canonical(root)?;
        let cwd = canonical(checkout)?;
        if key == cwd {
            return Err(EngineError::Other(
                "The project folder cannot be removed as a worktree.".into(),
            ));
        }
        self.inner
            .removing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cwd.clone());
        self.cancel(checkout)?;
        let operation = self.operation(&key);
        let _guard = operation.lock().await;
        let settings = self.effective(root, checkout)?;
        if self.active(&key)? == cwd {
            self.capture_follow(root, checkout, &settings)?;
            // The project folder becomes the active checkout again, so the
            // followed files return there instead of being overwritten by a
            // stale project copy on the next switch.
            let project = self.effective(root, root)?;
            for rule in project
                .env_files
                .iter()
                .filter(|r| r.mode == WorktreeEnvMode::Follow)
            {
                if let Err(error) = self.follow_file(root, root, &project, &rule.path) {
                    tracing::warn!(path = %rule.path, %error, "followed env file was not restored to the project");
                }
            }
            self.with(true, |p| {
                if let Some(p) = p.get_mut(&key) {
                    p.active = None;
                }
                Ok(())
            })?;
        }
        if !settings.cleanup_command.trim().is_empty() {
            self.run_tracked_workflow(
                root,
                checkout,
                &settings,
                &settings.cleanup_command,
                None,
                settings.commands_in_project,
                "Cleaning up worktree resources",
            )
            .await?;
        }
        Ok(())
    }

    /// Create/remove commands replace only the Git lifecycle step. Environment
    /// preparation and cleanup remain separate and always run in their order.
    pub(crate) async fn run_lifecycle(
        &self,
        root: &Path,
        checkout: &Path,
        settings: &WorktreeSettings,
        command: &str,
        refs: Option<(&str, &str)>,
    ) -> Result<(), EngineError> {
        let operation = self.operation(&canonical(root)?);
        let _lock = operation.lock().await;
        self.run_tracked_workflow(
            root,
            checkout,
            settings,
            command,
            refs,
            true,
            if refs.is_some() {
                "Creating worktree"
            } else {
                "Removing worktree"
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_tracked_workflow(
        &self,
        root: &Path,
        checkout: &Path,
        settings: &WorktreeSettings,
        command: &str,
        refs: Option<(&str, &str)>,
        in_project: bool,
        step: &str,
    ) -> Result<(), EngineError> {
        let key = canonical(root)?;
        let cwd = checkout
            .canonicalize()
            .or_else(|_| {
                Ok::<_, std::io::Error>(
                    checkout
                        .parent()
                        .unwrap_or(root)
                        .canonicalize()?
                        .join(checkout.file_name().unwrap_or_default()),
                )
            })?
            .to_string_lossy()
            .into_owned();
        self.ensure_running()?;
        self.defaults(root)?;
        let token = self.setup_token(&cwd);
        let mut guard = SetupGuard {
            store: self.clone(),
            root: key.clone(),
            checkout: cwd.clone(),
            token: token.clone(),
            complete: false,
        };
        let log = self.log_path(&key, &cwd);
        self.state(&key, &cwd, WorktreeSetupPhase::Preparing, step, None)?;
        let result = commands::run_with_refs(
            root, checkout, settings, command, &log, &token, in_project, refs,
        )
        .await;
        self.state(
            &key,
            &cwd,
            if result.is_ok() {
                WorktreeSetupPhase::NotPrepared
            } else if token.is_cancelled() {
                WorktreeSetupPhase::Interrupted
            } else {
                WorktreeSetupPhase::Failed
            },
            if result.is_ok() {
                "Workflow complete"
            } else {
                "Workflow needs attention"
            },
            result.as_ref().err().map(ToString::to_string),
        )?;
        guard.complete = true;
        self.inner
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&cwd);
        if refs.is_some() && !checkout.exists() {
            self.forget(root, Path::new(&cwd))?;
        }
        result
    }
}

struct SetupGuard {
    store: WorktreeSettingsStore,
    root: String,
    checkout: String,
    token: CancellationToken,
    complete: bool,
}
impl Drop for SetupGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.token.cancel();
            let _ = self.store.state(
                &self.root,
                &self.checkout,
                WorktreeSetupPhase::Interrupted,
                "Setup interrupted",
                Some("Setup was cancelled. Retry to finish preparing this worktree.".into()),
            );
            self.store
                .inner
                .cancellation
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.checkout);
        }
    }
}

/// Older versions kept service lists inside worktree settings. Checkout lists
/// become checkout service overrides; the project list waits for adoption by
/// the project service profile. Returns whether anything moved.
fn migrate_services(projects: &mut BTreeMap<String, Project>) -> bool {
    let mut moved = false;
    for project in projects.values_mut() {
        if let Some(services) = project.defaults.services.take() {
            moved = true;
            project.legacy_services.get_or_insert(services);
        }
        for checkout in project.checkouts.values_mut() {
            if let Some(services) = checkout.settings.as_mut().and_then(|s| s.services.take()) {
                moved = true;
                checkout.services.get_or_insert(services);
            }
        }
    }
    moved
}

fn canonical(path: &Path) -> Result<String, EngineError> {
    Ok(std::fs::canonicalize(path)
        .map_err(|_| EngineError::Other("Project or worktree folder is unavailable.".into()))?
        .to_string_lossy()
        .into_owned())
}
fn hash(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}

fn preparation_hash(settings: &WorktreeSettings) -> Result<String, EngineError> {
    let mut preparation = settings.clone();
    preparation.branch_prefix.clear();
    preparation.services = None;
    preparation.service_mode = WorktreeServiceMode::FollowActive;
    preparation.activate_command.clear();
    preparation.cleanup_command.clear();
    preparation.create_command.clear();
    preparation.remove_command.clear();
    // The default keeps digests of checkouts prepared before the timeout was
    // excluded unchanged in the common case.
    preparation.command_timeout_seconds = WorktreeSettings::default().command_timeout_seconds;
    Ok(hash(
        &serde_json::to_string(&preparation).map_err(|e| EngineError::Other(e.to_string()))?,
    ))
}

pub fn detect_install(root: &Path) -> String {
    if root.join("bun.lock").exists() || root.join("bun.lockb").exists() {
        "bun install --frozen-lockfile"
    } else if root.join("pnpm-lock.yaml").exists() {
        "pnpm install --frozen-lockfile"
    } else if root.join("yarn.lock").exists() {
        "yarn install --frozen-lockfile"
    } else if root.join("package-lock.json").exists() {
        "npm ci"
    } else {
        // Without a lockfile, a worktree install must not create one that
        // shows up as an unrelated change in the branch.
        "npm install --no-package-lock"
    }
    .into()
}

pub fn normalize(mut settings: WorktreeSettings) -> Result<WorktreeSettings, EngineError> {
    settings.branch_prefix = settings
        .branch_prefix
        .trim()
        .trim_end_matches('/')
        .to_string();
    if !settings.branch_prefix.is_empty() {
        settings.branch_prefix.push('/');
    }
    let prefix = &settings.branch_prefix;
    if prefix.len() > 80
        || prefix.starts_with('/')
        || prefix.starts_with('-')
        || prefix.contains("..")
        || prefix.contains("@{")
        || prefix.contains("//")
        || prefix
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c))
        || prefix
            .split('/')
            .any(|c| c.starts_with('.') || c.ends_with('.') || c.ends_with(".lock"))
    {
        return Err(EngineError::Other("Enter a valid Git branch prefix, such as iiroan/ or feature/. Leave it blank for no prefix.".into()));
    }
    if settings.env_files.len() > 128
        || settings.dependency_paths.len() > 64
        || settings.variables.len() > 128
        || settings.env_profile.trim().is_empty()
        || settings.env_profile.len() > 80
        || !(1..=zeron_proto::MAX_WORKTREE_COMMAND_TIMEOUT_SECONDS)
            .contains(&settings.command_timeout_seconds)
    {
        return Err(EngineError::Other(
            "Worktree settings exceed a supported limit. Command timeouts must be 1–7200 seconds."
                .into(),
        ));
    }
    let mut paths = std::collections::HashSet::new();
    for rule in &mut settings.env_files {
        rule.path = rule.path.trim().into();
        files::relative(&rule.path)?;
        if !paths.insert(rule.path.clone()) {
            return Err(EngineError::Other(
                "Choose each environment file only once.".into(),
            ));
        }
    }
    for path in &mut settings.dependency_paths {
        *path = path.trim().into();
        files::relative(path)?;
        if !path.split('/').any(|part| part == "node_modules") {
            return Err(EngineError::Other(
                "Dependency folders must be node_modules directories.".into(),
            ));
        }
    }
    for command in [
        &settings.create_command,
        &settings.install_command,
        &settings.setup_command,
        &settings.activate_command,
        &settings.cleanup_command,
        &settings.remove_command,
    ] {
        if command.len() > 16384 || command.contains('\0') {
            return Err(EngineError::Other(
                "A workflow command is too long or invalid.".into(),
            ));
        }
    }
    for (key, value) in &settings.variables {
        if key.is_empty()
            || key.len() > 128
            || !key
                .chars()
                .enumerate()
                .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
            || key.starts_with("ZERON_")
            || key.starts_with("CODEX_")
            || matches!(
                key.as_str(),
                "branchname" | "basebranch" | "worktreename" | "projectroot" | "worktreepath"
            )
            || value.len() > 65536
            || value.contains('\0')
        {
            return Err(EngineError::Other("Environment variables need a valid name. Workflow context names, ZERON_ and CODEX_ names are reserved.".into()));
        }
    }
    if settings.services.is_some() {
        settings.services = Some(crate::project_terminals::normalize_config(
            settings.services.unwrap(),
        )?);
    }
    Ok(settings)
}
