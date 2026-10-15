//! Show index edits immediately, serialize writes and reconcile every reply.
use super::*;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
#[cfg(test)]
mod tests;

pub(super) type StagingHandle = Rc<RefCell<StagingQueue>>;

pub(super) struct StagingQueue {
    target: Target,
    baseline: CheckoutChanges,
    requests: VecDeque<StageRequest>,
    pending: HashMap<String, HashMap<String, bool>>,
}

#[derive(Clone)]
struct StageRequest {
    repository: String,
    paths: Vec<String>,
    staged: bool,
    allow_conflict_markers: bool,
    head: Option<String>,
    branch: Option<String>,
}

struct Batch {
    request: StageRequest,
    count: usize,
}

impl StagingQueue {
    /// Combine adjacent clicks on the same index without reordering writes.
    fn batch(&self) -> Option<Batch> {
        let mut request = self.requests.front()?.clone();
        let count = self
            .requests
            .iter()
            .take_while(|next| {
                next.repository == request.repository
                    && next.staged == request.staged
                    && next.allow_conflict_markers == request.allow_conflict_markers
                    && next.head == request.head
                    && next.branch == request.branch
            })
            .count();
        request.paths = self
            .requests
            .iter()
            .take(count)
            .flat_map(|next| next.paths.iter().cloned())
            .collect();
        request.paths.sort();
        request.paths.dedup();
        Some(Batch { request, count })
    }

    fn projected(&self) -> CheckoutChanges {
        let mut snapshot = self.baseline.clone();
        for request in &self.requests {
            let paths: HashSet<_> = request.paths.iter().map(String::as_str).collect();
            let Some(repo) = snapshot
                .repositories
                .iter_mut()
                .find(|repo| repo.path == request.repository && repo.complete)
            else {
                continue;
            };
            for file in &mut repo.files {
                if paths.contains(file.path.as_str()) && !repo.submodules.contains(&file.path) {
                    project_file(file, request.staged);
                }
            }
            repo.files
                .retain(|file| in_group(file, false) || in_group(file, true));
        }
        snapshot
    }
}

/// Porcelain cannot predict rename pairs, conflict resolution or gitlinks.
/// Keep those rows in place, with pending feedback, until Git reports them.
fn project_file(file: &mut GitFileStatus, staged: bool) {
    use GitFileState::*;
    if file.is_conflicted()
        || file.old_path.is_some()
        || matches!(file.index, Renamed | Copied)
        || matches!(file.worktree, Renamed | Copied)
    {
        return;
    }
    if staged {
        file.index = match file.worktree {
            Untracked | Added => Added,
            Modified if file.index == Added => Added,
            Deleted if file.index == Added => Unchanged,
            Unchanged => return,
            state => state,
        };
        file.worktree = Unchanged;
    } else {
        file.worktree = match (file.index, file.worktree) {
            (Added, Deleted) => Unchanged,
            (Added, _) => Untracked,
            (Unchanged | Untracked, _) => return,
            (_, Deleted) => Deleted,
            (state, Unchanged) => state,
            (_, state) => state,
        };
        file.index = Unchanged;
    }
}

impl SourceControl {
    /// Other Git mutations stay blocked until the real index is reconciled.
    pub(super) fn staging_enabled(&self) -> bool {
        (!self.is_busy() || (self.staging_queue.is_some() && self.pending_commit.is_none()))
            && self.confirmation.is_none()
            && self.load_error.is_none()
    }

    pub(super) fn staging_pending(&self, repository: &str, path: &str) -> Option<bool> {
        self.staging_queue
            .as_ref()?
            .borrow()
            .pending
            .get(repository)?
            .get(path)
            .copied()
    }

    pub(super) fn stage_paths_enabled(
        &self,
        repository: &str,
        paths: &[String],
        staged: bool,
    ) -> bool {
        let paths: HashSet<_> = paths.iter().map(String::as_str).collect();
        self.staging_enabled()
            && self.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.repositories.iter().any(|repo| {
                    repo.path == repository
                        && repo.complete
                        && repo.files.iter().any(|file| {
                            paths.contains(file.path.as_str())
                                && self.staging_pending(repository, &file.path).is_none()
                                && (!staged || !file.is_conflicted() || !self.is_busy())
                        })
                })
            })
    }

    fn queue_is_current(&self, queue: &StagingHandle) -> bool {
        self.staging_queue
            .as_ref()
            .is_some_and(|current| Rc::ptr_eq(current, queue))
            && self.target.as_ref() == Some(&queue.borrow().target)
    }

    fn show_staging(
        &mut self,
        queue: &StagingHandle,
        request: &StageRequest,
        staged: bool,
        cx: &mut Context<Self>,
    ) {
        let selection = self.selected_files.clone();
        let anchor = self.selection_anchor.clone();
        let snapshot = queue.borrow().projected();
        self.apply_snapshot(snapshot, cx);
        if !selection.is_empty() || anchor.is_some() {
            self.follow_staging(
                &request.repository,
                &request.paths,
                staged,
                selection,
                anchor,
            );
        }
        cx.emit(SourceControlEvent::Updated);
        cx.notify();
    }

    pub(super) fn set_staged_checked(
        &mut self,
        repository: String,
        mut paths: Vec<String>,
        staged: bool,
        allow_conflict_markers: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.staging_enabled() {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.notify_error(&repository, "Repository is unavailable", cx);
            return;
        };
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let Some(repo) = snapshot
            .repositories
            .iter()
            .find(|repo| repo.path == repository && repo.complete)
        else {
            return;
        };
        let files: HashSet<_> = repo.files.iter().map(|file| file.path.as_str()).collect();
        paths.retain(|path| {
            self.staging_pending(&repository, path).is_none() && files.contains(path.as_str())
        });
        paths.sort();
        paths.dedup();
        if paths.is_empty() {
            return;
        }
        let request = StageRequest {
            repository,
            paths,
            staged,
            allow_conflict_markers,
            head: repo.git.as_ref().and_then(|state| state.head.clone()),
            branch: repo.git.as_ref().and_then(|state| state.branch.clone()),
        };
        let start = self.staging_queue.is_none();
        let queue = self
            .staging_queue
            .get_or_insert_with(|| {
                Rc::new(RefCell::new(StagingQueue {
                    target,
                    baseline: snapshot.clone(),
                    requests: VecDeque::new(),
                    pending: HashMap::new(),
                }))
            })
            .clone();
        {
            let mut pending = queue.borrow_mut();
            pending
                .pending
                .entry(request.repository.clone())
                .or_default()
                .extend(request.paths.iter().cloned().map(|path| (path, staged)));
            pending.requests.push_back(request.clone());
        }
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.operation_label = Some(
            if staged {
                "Staging changes…"
            } else {
                "Unstaging changes…"
            }
            .into(),
        );
        self.mutation_epoch += 1;
        self.details_task = None;
        self.details_key = None;
        self.show_staging(&queue, &request, staged, cx);
        if !start {
            return;
        }
        // Own the queue independently: accepted clicks still finish if the user
        // changes checkout or closes the source-control surface.
        cx.spawn(async move |this, cx| {
            loop {
                let Some(batch) = queue.borrow().batch() else { break; };
                let request = &batch.request;
                let target = queue.borrow().target.clone();
                let conflicts: Vec<_> = queue.borrow().baseline.repositories.iter()
                    .filter(|repo| repo.path == request.repository).flat_map(|repo| &repo.files)
                    .filter(|file| request.staged && file.is_conflicted() && request.paths.contains(&file.path))
                    .map(|file| file.path.clone()).collect();
                let notification = this.update(cx, |view, cx| {
                    if view.queue_is_current(&queue) {
                        let label = if request.staged { "Staging changes…" } else { "Unstaging changes…" };
                        view.operation_label = Some(label.into());
                        cx.notify();
                        view.begin_notification(&request.repository, label, cx)
                    } else { None }
                }).ok().flatten();
                let notified = notification.is_some();
                let response = engine.client().call_as::<CheckoutChanges>(methods::SET_CHECKOUT_STAGED,
                    serde_json::json!({ "cwd": target.cwd, "targetDeviceId": target.device, "repository": request.repository,
                        "paths": request.paths, "staged": request.staged, "allowConflictMarkers": request.allow_conflict_markers,
                        "expectedHead": request.head, "expectedBranch": request.branch })).await;
                let outcome = response.as_ref().map(|snapshot| {
                    let notice = staging_notice(snapshot, &request.repository, &request.paths, request.staged);
                    let resolved = snapshot.repositories.iter().find(|repo| repo.path == request.repository && repo.complete)
                        .map_or(0, |repo| conflicts.iter().filter(|path| !repo.files.iter().any(|file| &file.path == *path && file.is_conflicted())).count());
                    if resolved == 0 { return notice; }
                    let resolved = format!("Resolved {resolved} merge conflict{}", if resolved == 1 { "" } else { "s" });
                    if notice == "No file changes staged" { resolved.into() } else { format!("{resolved}. {notice}").into() }
                }).map_err(|error| SharedString::from(format!("Unable to {} changes: {error}", if request.staged { "stage" } else { "unstage" })));
                {
                    let mut pending = queue.borrow_mut();
                    pending.requests.drain(..batch.count);
                    if let Some(paths) = pending.pending.get_mut(&request.repository) {
                        for path in &request.paths { paths.remove(path); }
                        if paths.is_empty() { pending.pending.remove(&request.repository); }
                    }
                    if let Ok(snapshot) = &response { pending.baseline = snapshot.clone(); }
                }
                this.update(cx, |view, cx| {
                    if !view.queue_is_current(&queue) { return; }
                    view.show_staging(&queue, request, if response.is_ok() { request.staged } else { !request.staged }, cx);
                    match &outcome {
                        Ok(message) if notified => view.notice = Some(message.clone()),
                        Ok(message) => view.notify_success(&request.repository, message.clone(), cx),
                        Err(error) => view.error = Some(error.clone()),
                    }
                    cx.notify();
                }).ok();
                // Git may have written the index before a transport/list failure.
                // Read real status before accepting another write or enabling commit.
                if response.is_err() {
                    let refreshed = engine.client().call_as::<CheckoutChanges>(methods::GET_CHECKOUT_CHANGES,
                        serde_json::json!({ "cwd": target.cwd, "targetDeviceId": target.device })).await;
                    {
                        let mut pending = queue.borrow_mut();
                        match &refreshed {
                            Ok(snapshot) => pending.baseline = snapshot.clone(),
                            Err(_) => { pending.requests.clear(); pending.pending.clear(); }
                        }
                    }
                    this.update(cx, |view, cx| {
                        if !view.queue_is_current(&queue) { return; }
                        if let Err(error) = &refreshed {
                            view.load_error = Some(format!("Unable to verify the index after staging failed: {error}").into());
                        }
                        view.show_staging(&queue, request, !request.staged, cx);
                    }).ok();
                }
                if let Some(notification) = notification { notification.finish(outcome, cx); }
            }
            this.update(cx, |view, cx| {
                if !view.queue_is_current(&queue) { return; }
                view.staging_queue = None;
                view.busy = false;
                view.operation_label = None;
                view.details_key = None;
                view.apply_snapshot(queue.borrow().baseline.clone(), cx);
                cx.notify();
            }).ok();
        }).detach();
    }

    #[cfg(feature = "source-control-fixture")]
    pub(super) fn fixture_pending_staging(&self) -> serde_json::Value {
        self.staging_queue
            .as_ref()
            .map_or(serde_json::Value::Null, |queue| {
                serde_json::json!(queue.borrow().requests.iter().map(|request| serde_json::json!({
                "repository": request.repository, "paths": request.paths, "staged": request.staged
            })).collect::<Vec<_>>())
            })
    }
}
