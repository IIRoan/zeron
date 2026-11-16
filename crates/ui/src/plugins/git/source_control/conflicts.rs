use super::*;

impl SourceControl {
    pub(super) fn check_conflicts_before_staging(
        &mut self,
        repository: String,
        paths: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() || self.confirmation.is_some() {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.notify_error(&repository, "Repository is unavailable", cx);
            return;
        };
        let head = self.git_state().and_then(|state| state.head.clone());
        let branch = self.git_state().and_then(|state| state.branch.clone());
        self.busy = true;
        self.mutation_epoch += 1;
        let epoch = self.mutation_epoch;
        let notification = self.begin_notification(&repository, "Checking merge conflicts…", cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = engine.client().call_as::<Vec<String>>(methods::GET_CHECKOUT_CONFLICT_MARKERS,
                serde_json::json!({"cwd": target.cwd, "targetDeviceId": target.device, "repository": repository, "paths": paths})).await;
            if let Some(notification) = notification {
                match &result {
                    Ok(_) => notification.dismiss(cx),
                    Err(error) => notification.finish(Err(format!("Unable to check conflicts: {error}").into()), cx),
                }
            }
            this.update(cx, |view, cx| {
                if view.target.as_ref() != Some(&target) || view.mutation_epoch != epoch { return; }
                view.busy = false;
                match result {
                    Ok(markers) if markers.is_empty() => view.set_staged_checked(repository, paths, true, false, cx),
                    Ok(markers) => view.confirmation = Some(Confirmation {
                        target, repository, head, branch,
                        title: "Stage files with conflict markers?".into(),
                        body: format!("Merge conflict markers remain in {} selected file{}. Resolve them first, or stage the current contents anyway.", markers.len(), if markers.len() == 1 { "" } else { "s" }),
                        command: git::Command::StageConflicts(paths),
                    }),
                    Err(error) => view.error = Some(format!("Unable to check conflicts: {error}").into()),
                }
                cx.notify();
            }).ok();
        }).detach();
    }
}
