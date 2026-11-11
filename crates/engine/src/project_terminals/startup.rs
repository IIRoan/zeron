use super::*;
use zeron_proto::ProjectServiceStartup;

pub(super) struct ReadinessCheck {
    space: String,
    service: String,
    pub token: CancellationToken,
}

pub(super) struct ReadinessGuard {
    store: ProjectTerminals,
    key: String,
}

impl Drop for ReadinessGuard {
    fn drop(&mut self) {
        self.store
            .inner
            .readiness_checks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

pub(super) struct Startup {
    pub space: String,
    pub active: bool,
    pub checkout: String,
    pub group: String,
    pub token: CancellationToken,
    /// What is still to start: the requested services and all they need.
    pub services: HashSet<String>,
    pub shared: HashSet<String>,
    /// Each requested service with everything it needs.
    requests: Vec<(String, HashSet<String>)>,
    /// Services stopped since: the requests that need one are dropped, the
    /// others carry on.
    stopped: HashSet<String>,
    pub progress: BTreeMap<(String, String), ProjectServiceStartup>,
}

impl Startup {
    /// Requests `roots`, each shown as `message`, except those a stopped
    /// service is part of.
    fn request(&mut self, config: &ProjectTerminalConfig, roots: &[String], message: &str) {
        self.requests = roots
            .iter()
            .map(|root| {
                let needs = order(config, std::slice::from_ref(root)).unwrap_or_default();
                (root.clone(), needs.into_iter().collect())
            })
            .collect();
        self.progress = roots
            .iter()
            .map(|id| {
                let owner = if self.shared.contains(id) {
                    self.space.clone()
                } else {
                    self.group.clone()
                };
                (
                    (owner, id.clone()),
                    ProjectServiceStartup {
                        service_id: id.clone(),
                        message: message.into(),
                    },
                )
            })
            .collect();
        self.withdraw();
    }

    /// Drops the requests a stopped service is part of. False when none is
    /// left.
    fn withdraw(&mut self) -> bool {
        let Self {
            requests,
            stopped,
            services,
            progress,
            ..
        } = self;
        requests.retain(|(_, needs)| needs.is_disjoint(stopped));
        *services = requests
            .iter()
            .flat_map(|(_, needs)| needs.iter().cloned())
            .collect();
        progress.retain(|(_, id), _| requests.iter().any(|(root, _)| root == id));
        !requests.is_empty()
    }
}

pub(super) struct Guard {
    store: ProjectTerminals,
    key: String,
}

impl Guard {
    pub fn activate(&self, config: &ProjectTerminalConfig, roots: &[String]) {
        if let Some(startup) = self
            .store
            .inner
            .startups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&self.key)
        {
            startup.active = true;
            startup.request(config, roots, "Starting…");
        }
    }

    /// Whether `id` is still to start: not stopped since this startup began,
    /// nor only needed by a service that was.
    pub fn wants(&self, id: &str) -> bool {
        self.store
            .inner
            .startups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.key)
            .is_some_and(|startup| startup.services.contains(id))
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(startup) = self
            .store
            .inner
            .startups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key)
        {
            startup.token.cancel();
        }
    }
}

/// Resolve only the requested services and their transitive dependencies.
/// Missing fields retain the old database prerequisite policy for saved apps.
pub(super) fn order(
    config: &ProjectTerminalConfig,
    roots: &[String],
) -> Result<Vec<String>, EngineError> {
    fn visit(
        id: &str,
        config: &ProjectTerminalConfig,
        visiting: &mut HashSet<String>,
        done: &mut HashSet<String>,
        result: &mut Vec<String>,
    ) -> Result<(), EngineError> {
        if done.contains(id) {
            return Ok(());
        }
        let service = config.services.iter().find(|s| s.id == id).ok_or_else(|| {
            EngineError::Other(format!(
                "Unknown startup dependency: {id}. Update the service's startup requirements."
            ))
        })?;
        if !visiting.insert(id.into()) {
            return Err(EngineError::Other(format!(
                "Startup dependencies contain a cycle at {}.",
                service.name
            )));
        }
        let dependencies = dependencies(config, service);
        for dependency in dependencies {
            if service.is_database()
                && config
                    .services
                    .iter()
                    .any(|s| s.id == dependency && !s.is_database())
            {
                return Err(EngineError::Other(format!(
                    "Shared database {} cannot depend on a worktree app service.",
                    service.name
                )));
            }
            visit(&dependency, config, visiting, done, result)?;
        }
        visiting.remove(id);
        done.insert(id.into());
        result.push(id.into());
        Ok(())
    }
    let mut result = Vec::new();
    let mut done = HashSet::new();
    for id in roots {
        visit(id, config, &mut HashSet::new(), &mut done, &mut result)?;
    }
    Ok(result)
}

pub(crate) fn dependencies(
    config: &ProjectTerminalConfig,
    service: &ProjectTerminalService,
) -> Vec<String> {
    service.depends_on.clone().unwrap_or_else(|| {
        if service.is_database() {
            Vec::new()
        } else {
            config
                .services
                .iter()
                .filter(|s| s.is_database())
                .map(|s| s.id.clone())
                .collect()
        }
    })
}

pub(super) fn validate(config: &ProjectTerminalConfig) -> Result<(), EngineError> {
    order(
        config,
        &config
            .services
            .iter()
            .map(|s| s.id.clone())
            .collect::<Vec<_>>(),
    )?;
    Ok(())
}

impl ProjectTerminals {
    pub(super) fn readiness_check(
        &self,
        space: &str,
        service: &str,
        token: CancellationToken,
    ) -> ReadinessGuard {
        let key = uuid::Uuid::new_v4().to_string();
        self.inner
            .readiness_checks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                key.clone(),
                ReadinessCheck {
                    space: space.into(),
                    service: service.into(),
                    token,
                },
            );
        ReadinessGuard {
            store: self.clone(),
            key,
        }
    }
    pub(super) fn startup_progress(&self, owner: &str) -> Vec<ProjectServiceStartup> {
        self.inner
            .startups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .flat_map(|startup| {
                startup
                    .progress
                    .iter()
                    .filter(|((group, _), _)| group == owner)
                    .map(|(_, progress)| progress.clone())
            })
            .collect()
    }

    pub(super) fn begin_startup(
        &self,
        space: &str,
        checkout: &Path,
        config: &ProjectTerminalConfig,
        roots: &[String],
        token: CancellationToken,
    ) -> Result<Guard, EngineError> {
        let mut startup = Startup {
            space: space.into(),
            active: false,
            checkout: canonical_root(checkout)?,
            group: Self::group_key(space, checkout)?,
            token,
            services: HashSet::new(),
            shared: config
                .services
                .iter()
                .filter(|s| s.is_database())
                .map(|s| s.id.clone())
                .collect(),
            requests: Vec::new(),
            stopped: HashSet::new(),
            progress: BTreeMap::new(),
        };
        startup.request(config, roots, "Waiting for another service operation…");
        let key = uuid::Uuid::new_v4().to_string();
        self.inner
            .startups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), startup);
        Ok(Guard {
            store: self.clone(),
            key,
        })
    }

    pub(super) fn startup_message(
        &self,
        space: &str,
        waiting_for: Option<&ProjectTerminalService>,
        message: &str,
    ) {
        for startup in self
            .inner
            .startups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values_mut()
            .filter(|s| s.space == space && s.active)
        {
            for progress in startup.progress.values_mut() {
                progress.message =
                    if waiting_for.is_some_and(|service| service.id == progress.service_id) {
                        "Starting…".into()
                    } else {
                        message.into()
                    };
            }
        }
    }

    pub(super) fn startup_finished(&self, space: &str, id: &str) {
        for startup in self
            .inner
            .startups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values_mut()
            .filter(|s| s.space == space && s.active)
        {
            startup.progress.retain(|(_, service), _| service != id);
        }
    }

    /// Cancellation must happen before waiting for the project's control lock.
    /// True when an active startup was cancelled as a whole: it then gives up
    /// that lock. One that only lost a service keeps starting the others.
    pub(super) fn cancel_startup(
        &self,
        space: &str,
        checkout: &Path,
        service: Option<&str>,
        worktree_only: bool,
    ) -> Result<bool, EngineError> {
        let checkout = canonical_root(checkout)?;
        if !worktree_only {
            for check in self
                .inner
                .readiness_checks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|check| {
                    check.space == space && service.is_none_or(|id| id == check.service)
                })
            {
                check.token.cancel();
            }
        }
        let mut active = false;
        for startup in self
            .inner
            .startups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values_mut()
            .filter(|s| s.space == space)
        {
            let same_checkout = startup.checkout == checkout;
            if service.is_none_or(|id| startup.services.contains(id))
                && (same_checkout
                    || (!worktree_only
                        && service.map_or_else(
                            || {
                                startup
                                    .services
                                    .iter()
                                    .any(|id| startup.shared.contains(id))
                            },
                            |id| startup.shared.contains(id),
                        )))
            {
                // Stopping one service leaves the rest of its batch starting,
                // unless they need it.
                if let Some(id) = service {
                    startup.stopped.insert(id.into());
                    if startup.withdraw() && !startup.token.is_cancelled() {
                        continue;
                    }
                }
                startup.token.cancel();
                active |= startup.active;
                if !startup.active {
                    startup.progress.clear();
                }
            }
        }
        Ok(active)
    }

    pub(super) fn has_active_startup(&self, space: &str) -> bool {
        self.inner
            .startups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .any(|s| s.space == space && s.active)
    }
}
