use super::*;
use zeron_proto::RepositoryGitDetails;
use zeron_rpc::RpcError;

impl SourceControl {
    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() || self.refreshing || self.confirmation.is_some() {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let repository = self.active_repository.clone();
        self.mutation_epoch += 1;
        let epoch = self.mutation_epoch;
        self.refreshing = true;
        self.refresh_feedback = None;
        self.error = None;
        self.details_task = None;
        self.details_key = None;
        cx.notify();
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let started = std::time::Instant::now();
            let (snapshot, details) = futures::join!(
                engine.client().call_as::<CheckoutChanges>(methods::GET_CHECKOUT_CHANGES,
                    serde_json::json!({ "cwd": target.cwd, "targetDeviceId": target.device })),
                engine.client().call_as::<RepositoryGitDetails>(methods::GET_CHECKOUT_GIT_DETAILS,
                    serde_json::json!({ "cwd": target.cwd, "targetDeviceId": target.device, "repository": repository })),
            );
            // A quick local read should still have a visible acknowledgement.
            cx.background_executor().timer(Duration::from_millis(600).saturating_sub(started.elapsed())).await;
            let applied = this.update(cx, |view, cx| {
                view.complete_refresh(&target, epoch, &repository, snapshot, details, cx)
            }).unwrap_or(false);
            if !applied { return; }
            cx.background_executor().timer(Duration::from_millis(1000)).await;
            this.update(cx, |view, cx| {
                if view.target.as_ref() == Some(&target) {
                    view.refresh_feedback = None;
                    cx.notify();
                }
            }).ok();
        }));
    }

    pub(super) fn complete_refresh(
        &mut self,
        target: &Target,
        epoch: u64,
        repository: &str,
        snapshot: Result<CheckoutChanges, RpcError>,
        details: Result<RepositoryGitDetails, RpcError>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.target.as_ref() != Some(target) {
            return false;
        }
        self.refreshing = false;
        // A commit or staging operation may have finished during this read.
        if self.mutation_epoch != epoch || self.is_busy() {
            self.refresh_feedback = None;
            cx.notify();
            return false;
        }
        let success = snapshot.is_ok() && details.is_ok();
        match snapshot {
            Ok(snapshot) => {
                self.load_error = None;
                self.apply_snapshot(snapshot, cx);
                if self.active_repository == repository {
                    match details {
                        Ok(details) => {
                            self.details_key = Some((
                                target.clone(),
                                repository.into(),
                                self.git_state().cloned(),
                            ));
                            self.git_details = Some(details);
                        }
                        Err(error) => {
                            self.error =
                                Some(format!("Unable to refresh Git details: {error}").into())
                        }
                    }
                }
            }
            Err(error) => {
                self.load_error = Some(format!("Unable to refresh changes: {error}").into())
            }
        }
        self.refresh_feedback = Some(success);
        cx.notify();
        true
    }
}
