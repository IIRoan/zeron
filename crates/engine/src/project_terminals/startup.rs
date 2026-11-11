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
    pub services: HashSet<String>,
    pub shared: HashSet<String>,
    pub progress: BTreeMap<(String, String), ProjectServiceStartup>,
}

pub(super) struct Guard {
    store: ProjectTerminals,
    key: String,
}

impl Guard {
    pub fn activate(&self, roots: &[String], order: &[String]) {
        if let Some(startup) = self
            .store
            .inner
            .startups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&self.key)
        {
            startup.active = true;
            startup.services = order.iter().cloned().collect();
            startup.progress = roots
                .iter()
                .map(|id| {
                    let owner = if startup.shared.contains(id) {
                        startup.space.clone()
                    } else {
                        startup.group.clone()
                    };
                    (
                        (owner, id.clone()),
                        ProjectServiceStartup {
                            service_id: id.clone(),
                            message: "Starting…".into(),
                        },
                    )
                })
                .collect();
        }
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

pub(super) fn dependencies(
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
        order: &[String],
        token: CancellationToken,
    ) -> Result<Guard, EngineError> {
        let group = Self::group_key(space, checkout)?;
        let shared: HashSet<_> = config
            .services
            .iter()
            .filter(|s| s.is_database())
            .map(|s| s.id.clone())
            .collect();
        let progress = roots
            .iter()
            .map(|id| {
                let owner = if shared.contains(id) {
                    space.to_string()
                } else {
                    group.clone()
                };
                (
                    (owner, id.clone()),
                    ProjectServiceStartup {
                        service_id: id.clone(),
                        message: "Waiting for another service operation…".into(),
                    },
                )
            })
            .collect();
        let key = uuid::Uuid::new_v4().to_string();
        self.inner
            .startups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                key.clone(),
                Startup {
                    space: space.into(),
                    active: false,
                    checkout: canonical_root(checkout)?,
                    group,
                    token,
                    services: order.iter().cloned().collect(),
                    shared,
                    progress,
                },
            );
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
