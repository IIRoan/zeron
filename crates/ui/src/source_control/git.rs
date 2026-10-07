use super::*;
use zeron_proto::{
    CheckoutDiscardPreview, CheckoutDiscardResult, RepositoryGitAction as Action,
    RepositoryGitActionResult, RepositoryGitDetails, RepositoryGitState,
};
mod dialog;
pub(super) use dialog::ConfirmationDialog;

#[derive(IntoElement)]
struct GitDropdown {
    id: SharedString,
    card: gpui::Stateful<gpui::Div>,
    body: AnyElement,
    closing: Option<std::time::Instant>,
    trigger_height: f32,
    bounds: selection::MenuBounds,
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext, VisualTestContext};

    struct MenuHost(Entity<SourceControl>);
    impl Render for MenuHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                div()
                    .absolute()
                    .left(px(350.0))
                    .top(px(40.0))
                    .w(px(52.0))
                    .h(px(24.0))
                    .child(self.0.update(cx, |view, cx| view.render_header_actions(cx))),
            )
        }
    }

    fn menu_setup(
        cx: &mut TestAppContext,
        height: f32,
    ) -> (Entity<SourceControl>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::composer::init(cx, Default::default());
        });
        let (host, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| AppState::new());
            MenuHost(cx.new(|cx| {
                let mut view = SourceControl::new(state, cx);
                view.target = Some(Target {
                    chat: "chat".into(),
                    cwd: "/checkout".into(),
                    device: None,
                });
                view.snapshot = Some(CheckoutChanges {
                    repositories: vec![zeron_proto::RepositoryChanges {
                        path: String::new(),
                        name: "test".into(),
                        complete: true,
                        files: Vec::new(),
                        submodules: Vec::new(),
                        git: Some(RepositoryGitState {
                            head: None,
                            branch: Some("main".into()),
                            upstream: None,
                            ahead: None,
                            behind: None,
                            operation: None,
                            conflicts: 0,
                        }),
                    }],
                });
                view.actions_menu.open(());
                view.git_panel = Some(GitPanel::Actions);
                view
            }))
        });
        let view = host.read_with(cx, |host, _| host.0.clone());
        cx.simulate_resize(gpui::size(px(640.0), px(height)));
        cx.update(|window, cx| window.draw(cx).clear());
        (view, cx)
    }

    #[gpui::test]
    fn git_menu_shows_its_last_command_when_the_window_has_room(cx: &mut TestAppContext) {
        let (_, cx) = menu_setup(cx, 900.0);
        let scroll = cx.debug_bounds("git-menu-scroll").unwrap();
        let last = cx.debug_bounds("git-menu-refresh").unwrap();
        assert!(scroll.size.height > px(320.0));
        assert!(last.top() >= scroll.top() && last.bottom() <= scroll.bottom());
        assert!(scroll.bottom() <= px(900.0));
    }

    #[gpui::test]
    fn short_git_menu_scrolls_to_and_activates_its_last_command(cx: &mut TestAppContext) {
        let (view, cx) = menu_setup(cx, 280.0);
        let scroll = cx.debug_bounds("git-menu-scroll").unwrap();
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: scroll.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-10_000.0))),
            ..Default::default()
        });
        cx.run_until_parked();
        let last = cx.debug_bounds("git-menu-refresh").unwrap();
        assert!(last.top() >= scroll.top() && last.bottom() <= scroll.bottom());
        assert!(last.bottom() <= px(280.0));
        cx.simulate_click(last.center(), gpui::Modifiers::default());
        view.read_with(cx, |view, _| assert!(!view.actions_menu.is_open()));
    }
}

impl RenderOnce for GitDropdown {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let viewport = window.viewport_size();
        let top = px(Theme::TITLEBAR_HEIGHT + 8.0).min(viewport.height);
        let limits = gpui::Bounds::new(
            gpui::point(px(8.0), top),
            gpui::size(
                (viewport.width - px(16.0)).max(px(1.0)),
                (viewport.height - top - px(8.0)).max(px(1.0)),
            ),
        );
        let height = (f32::from(viewport.height)
            - 2.0 * Theme::TITLEBAR_HEIGHT
            - self.trigger_height
            - 24.0)
            .max(1.0)
            .min(600.0)
            .min(f32::from(limits.size.height));
        let scroll_id: SharedString = format!("{}-options", self.id).into();
        let scroll = window.with_global_id(scroll_id.clone().into(), |id, window| {
            window.with_element_state(id, |previous: Option<gpui::ScrollHandle>, _| {
                let scroll = previous.unwrap_or_default();
                (scroll.clone(), scroll)
            })
        });
        let rows = div()
            .id(scroll_id)
            .debug_selector(|| "git-menu-scroll".into())
            .max_h(px((height - 2.0 * popover::CARD_INSET - 2.0).max(1.0)))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .child(self.body);
        let marker = self.bounds;
        let content = self
            .card
            .relative()
            .child(
                gpui::canvas(
                    move |bounds, _, _| marker.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(crate::edge_fade::edge_faded(10.0, true, true, rows).fade_overflow_y(&scroll));
        popover::contained_menu_with_height(
            self.id,
            div().child(content),
            self.closing,
            self.trigger_height,
            limits,
            height,
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum GitPanel {
    Actions,
    Branches,
    Stashes,
    Remotes,
    History,
}
#[derive(Clone)]
pub(super) enum GitForm {
    CreateBranch,
    RenameBranch,
    Stash(bool),
    AddRemote,
}
#[derive(Clone)]
enum Command {
    Git(Action),
    Discard(CheckoutDiscardPreview),
}
#[derive(Clone)]
pub(super) struct Confirmation {
    target: Target,
    repository: String,
    head: Option<String>,
    branch: Option<String>,
    title: String,
    body: String,
    command: Command,
}
#[derive(Clone)]
pub(super) struct UndoDiscard {
    target: Target,
    repository: String,
    result: CheckoutDiscardResult,
}

fn button(
    id: impl Into<String>,
    label: impl Into<SharedString>,
    enabled: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let label = label.into();
    div()
        .id(SharedString::from(id.into()))
        .h(px(26.0))
        .flex_none()
        .px(px(7.0))
        .flex()
        .items_center()
        .gap(px(5.0))
        .rounded(px(3.0))
        .text_size(px(13.0))
        .text_color(theme.text)
        .role(gpui::Role::Button)
        .aria_label(label.clone())
        .child(label)
        .when(enabled, |el| {
            el.cursor_pointer()
                .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text))
        })
        .when(!enabled, |el| el.opacity(0.4))
}

impl SourceControl {
    pub(super) fn git_state(&self) -> Option<&RepositoryGitState> {
        self.snapshot
            .as_ref()?
            .repositories
            .iter()
            .find(|r| r.path == self.active_repository)?
            .git
            .as_ref()
    }
    pub(super) fn git_enabled(&self) -> bool {
        !self.is_busy()
            && self.confirmation.is_none()
            && self.load_error.is_none()
            && self.git_state().is_some()
    }

    pub(super) fn ensure_git_details(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() || self.refreshing {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        if self.snapshot.is_none() {
            return;
        }
        let repository = self.active_repository.clone();
        let key = (
            target.clone(),
            repository.clone(),
            self.git_state().cloned(),
        );
        if self.details_key.as_ref() == Some(&key) {
            return;
        }
        self.details_key = Some(key.clone());
        self.details_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call_as::<RepositoryGitDetails>(methods::GET_CHECKOUT_GIT_DETAILS,
                serde_json::json!({ "cwd": target.cwd, "targetDeviceId": target.device, "repository": repository })).await;
            this.update(cx, |view, cx| {
                if view.details_key.as_ref() != Some(&key) { return; }
                match result { Ok(details) => view.git_details = Some(details), Err(error) => view.error = Some(format!("Unable to load Git details: {error}").into()) }
                cx.notify();
            }).ok();
        }));
    }

    pub(super) fn run_git(&mut self, action: Action, cx: &mut Context<Self>) {
        if !self.git_enabled() {
            return;
        }
        let head = self.git_state().and_then(|s| s.head.clone());
        let branch = self.git_state().and_then(|s| s.branch.clone());
        let label = if matches!(action, Action::Sync) {
            "Syncing changes…"
        } else {
            "Running Git action…"
        };
        self.dispatch(
            methods::RUN_CHECKOUT_GIT_ACTION,
            serde_json::json!({ "action": action, "expectedHead": head, "expectedBranch": branch }),
            label,
            cx,
        );
    }

    /// Keep submitted operations alive when a tool tab is closed. Report the
    /// operation result separately from a later metadata refresh failure.
    pub(super) fn dispatch(
        &mut self,
        method: &'static str,
        mut params: serde_json::Value,
        label: &'static str,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let repository = self.active_repository.clone();
        params["cwd"] = target.cwd.clone().into();
        params["repository"] = repository.clone().into();
        params["targetDeviceId"] = serde_json::to_value(&target.device).unwrap();
        let message = self.commit_input.read(cx).text().to_owned();
        self.close_actions_menu(cx);
        self.close_commit_menu(cx);
        self.busy = true;
        self.operation_label = Some(label.into());
        self.error = None;
        self.notice = None;
        self.git_panel = None;
        let submitted_form = self.git_form.take();
        self.details_task = None;
        self.details_key = None;
        self.mutation_epoch += 1;
        let epoch = self.mutation_epoch;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = engine.client().call(method, params).await;
            this.update(cx, |view, cx| {
                if view.target.as_ref() != Some(&target) {
                    return;
                }
                match &result {
                    Ok(value) => {
                        if method == methods::DISCARD_CHECKOUT_CHANGES {
                            if let Ok(result) =
                                serde_json::from_value::<CheckoutDiscardResult>(value.clone())
                            {
                                view.notice =
                                    Some(format!("Discarded {} file(s)", result.file_count).into());
                                view.undo_discard = Some(UndoDiscard {
                                    target: target.clone(),
                                    repository: repository.clone(),
                                    result,
                                });
                            }
                        } else if method == methods::RESTORE_CHECKOUT_DISCARD {
                            view.undo_discard = None;
                            view.notice = Some("Discarded files restored".into());
                        } else if let Ok(result) =
                            serde_json::from_value::<RepositoryGitActionResult>(value.clone())
                        {
                            view.notice = Some(result.notice.into());
                            if let Some(text) = result.commit_message
                                && view.active_repository == repository
                                && view.commit_input.read(cx).text() == message
                            {
                                view.commit_input
                                    .update(cx, |input, cx| input.set_text(text, cx));
                                view.remember_draft(cx);
                            }
                        }
                    }
                    Err(error) => {
                        view.error = Some(format!("{error}").into());
                        if view.active_repository == repository {
                            view.git_form = submitted_form.clone();
                            if view.git_form.is_some() {
                                view.actions_menu.open(());
                            }
                        }
                    }
                }
                cx.notify();
            })
            .ok();
            // A rejected pull/merge can still leave conflicts to display.
            let refreshed = engine
                .client()
                .call_as::<CheckoutChanges>(
                    methods::GET_CHECKOUT_CHANGES,
                    serde_json::json!({ "cwd": target.cwd, "targetDeviceId": target.device }),
                )
                .await;
            this.update(cx, |view, cx| {
                if view.target.as_ref() != Some(&target) || view.mutation_epoch != epoch {
                    return;
                }
                view.busy = false;
                view.operation_label = None;
                view.details_key = None;
                match refreshed {
                    Ok(snapshot) => {
                        view.load_error = None;
                        view.apply_snapshot(snapshot, cx);
                    }
                    Err(error) => {
                        view.load_error = Some(format!("Unable to refresh changes: {error}").into())
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn request_discard(
        &mut self,
        repository: String,
        paths: Vec<String>,
        include_staged: bool,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() || self.confirmation.is_some() {
            return;
        }
        self.select_repository(repository.clone(), cx);
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.close_actions_menu(cx);
        self.close_commit_menu(cx);
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.mutation_epoch += 1;
        self.git_panel = None;
        self.git_form = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = engine.client().call_as::<CheckoutDiscardPreview>(methods::PREVIEW_CHECKOUT_DISCARD,
                serde_json::json!({ "cwd": target.cwd, "targetDeviceId": target.device, "repository": repository, "paths": paths, "includeStaged": include_staged })).await;
            this.update(cx, |view, cx| {
                if view.target.as_ref() != Some(&target) { return; }
                view.busy = false;
                match result {
                    Ok(preview) => {
                        let body = if include_staged { "Staged and unstaged edits will be restored to the last commit." } else { "Unstaged edits will be restored to the index. Staged changes and submodule contents will be kept." };
                        view.confirmation = Some(Confirmation { target, repository, head: None, branch: None,
                            title: if preview.file_count == 1 { format!("Discard {}?", preview.paths[0]) } else { format!("Discard changes in {} files?", preview.file_count) },
                            body: format!("{body} Untracked files will be removed. You can undo this discard until those files, staging, or the branch change."), command: Command::Discard(preview) });
                    }
                    Err(error) => view.error = Some(format!("Unable to discard: {error}").into()),
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    pub(super) fn discard_all(&mut self, include_staged: bool, cx: &mut Context<Self>) {
        let Some(repo) = self.snapshot.as_ref().and_then(|s| {
            s.repositories
                .iter()
                .find(|r| r.path == self.active_repository && r.complete)
        }) else {
            return;
        };
        // Gitlinks belong to the parent index, but their worktrees are separate
        // repositories. Never clean submodule contents from a parent group.
        let paths = repo
            .files
            .iter()
            .filter(|f| {
                !repo.submodules.contains(&f.path) && (include_staged || in_group(f, false))
            })
            .map(|f| f.path.clone())
            .collect();
        self.request_discard(self.active_repository.clone(), paths, include_staged, cx);
    }

    pub(super) fn confirm_git(
        &mut self,
        action: Action,
        title: String,
        body: String,
        cx: &mut Context<Self>,
    ) {
        if !self.git_enabled() {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        self.close_actions_menu(cx);
        self.close_commit_menu(cx);
        self.confirmation = Some(Confirmation {
            target,
            repository: self.active_repository.clone(),
            head: self.git_state().and_then(|s| s.head.clone()),
            branch: self.git_state().and_then(|s| s.branch.clone()),
            title,
            body,
            command: Command::Git(action),
        });
        self.git_panel = None;
        cx.notify();
    }

    pub(super) fn accept_confirmation(&mut self, cx: &mut Context<Self>) {
        let Some(confirmation) = self.confirmation.take() else {
            return;
        };
        if self.target.as_ref() != Some(&confirmation.target)
            || self.active_repository != confirmation.repository
        {
            cx.notify();
            return;
        }
        match confirmation.command {
            Command::Discard(preview) => self.dispatch(
                methods::DISCARD_CHECKOUT_CHANGES,
                serde_json::json!({ "preview": preview }),
                "Discarding changes…",
                cx,
            ),
            Command::Git(action) => self.dispatch(
                methods::RUN_CHECKOUT_GIT_ACTION,
                serde_json::json!({ "action": action, "expectedHead": confirmation.head, "expectedBranch": confirmation.branch }),
                "Running Git action…",
                cx,
            ),
        }
    }

    pub(super) fn undo_discard(&mut self, cx: &mut Context<Self>) {
        let Some(undo) = self.undo_discard.clone() else {
            return;
        };
        if self.target.as_ref() != Some(&undo.target) || self.active_repository != undo.repository {
            return;
        }
        self.dispatch(methods::RESTORE_CHECKOUT_DISCARD, serde_json::json!({ "recoveryId": undo.result.recovery_id, "checksum": undo.result.checksum }), "Restoring discarded files…", cx);
    }

    pub(super) fn open_git_panel(&mut self, panel: GitPanel, cx: &mut Context<Self>) {
        if !self.git_enabled() {
            return;
        }
        self.git_panel = Some(panel);
        self.actions_menu.open(());
        self.git_form = None;
        self.details_key = None;
        cx.notify();
    }
    pub(super) fn start_form(
        &mut self,
        form: GitForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.git_enabled() {
            return;
        }
        let text = if matches!(form, GitForm::RenameBranch) {
            self.git_state()
                .and_then(|s| s.branch.clone())
                .unwrap_or_default()
        } else if matches!(form, GitForm::AddRemote) {
            "origin".into()
        } else {
            String::new()
        };
        self.form_input
            .update(cx, |input, cx| input.set_text(text, cx));
        self.form_second
            .update(cx, |input, cx| input.set_text("", cx));
        self.git_form = Some(form);
        self.git_panel = None;
        window.focus(
            &gpui::Focusable::focus_handle(self.form_input.read(cx), cx),
            cx,
        );
        // The form replaces a menu row. Focus it again once the input has
        // joined the window's focus tree, so typing works immediately.
        let input = self.form_input.clone();
        window.on_next_frame(move |window, cx| {
            window.focus(&gpui::Focusable::focus_handle(input.read(cx), cx), cx);
        });
        cx.notify();
    }
    pub(super) fn submit_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.git_form.clone() else {
            return;
        };
        let value = self.form_input.read(cx).text().trim().to_owned();
        let action = match form {
            GitForm::CreateBranch => Action::CreateBranch {
                name: value,
                start: None,
            },
            GitForm::RenameBranch => Action::RenameBranch { name: value },
            GitForm::Stash(include_untracked) => Action::Stash {
                message: value,
                include_untracked,
            },
            GitForm::AddRemote => Action::AddRemote {
                name: value,
                url: self.form_second.read(cx).text().trim().to_owned(),
            },
        };
        self.run_git(action, cx);
    }
    pub(super) fn toggle_amend(&mut self, cx: &mut Context<Self>) {
        if !self.git_enabled()
            || self
                .git_state()
                .is_none_or(|s| s.head.is_none() || s.operation.is_some())
        {
            return;
        }
        self.amend = !self.amend;
        if self.amend
            && self.commit_input.read(cx).text().is_empty()
            && let Some(details) = &self.git_details
        {
            let message = details.last_message.clone();
            self.commit_input
                .update(cx, |input, cx| input.set_text(message, cx));
            self.remember_draft(cx);
        }
        self.git_panel = None;
        self.close_actions_menu(cx);
        cx.notify();
    }

    pub(super) fn render_git_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let operation = self.git_state().and_then(|s| s.operation.as_ref());
        if self.operation_label.is_none() && operation.is_none() && !self.amend {
            return gpui::Empty.into_any_element();
        }
        div()
            .px(px(12.0))
            .py(px(5.0))
            .text_size(px(12.0))
            .text_color(theme.text_muted)
            .when_some(self.operation_label.clone(), |el, label| el.child(label))
            .when_some(operation.cloned(), |el, operation| {
                el.child(SharedString::from(format!(
                    "{operation} in progress · resolve and stage conflicts"
                )))
            })
            .when(self.amend, |el| el.child("Amending last commit"))
            .into_any_element()
    }

    pub(super) fn close_actions_menu(&mut self, cx: &mut Context<Self>) {
        if self.actions_menu.begin_close() {
            popover::reap_popup(cx, |this| &mut this.actions_menu);
            cx.notify();
        }
    }
    pub(super) fn close_commit_menu(&mut self, cx: &mut Context<Self>) {
        if self.commit_menu.begin_close() {
            popover::reap_popup(cx, |this| &mut this.commit_menu);
            cx.notify();
        }
    }

    pub(crate) fn handle_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.actions_menu.is_open() {
            self.close_actions_menu(cx);
            return true;
        }
        if self.commit_menu.is_open() {
            self.close_commit_menu(cx);
            return true;
        }
        if self.confirmation.take().is_some() {
            cx.notify();
            return true;
        }
        self.actions_menu.get().is_some() || self.commit_menu.get().is_some()
    }

    pub(crate) fn render_header_actions(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let enabled = self.git_enabled();
        let refresh_label = if self.refreshing {
            "Refreshing…"
        } else {
            match self.refresh_feedback {
                Some(false) => "Refresh failed",
                _ => "Refresh",
            }
        };
        let refresh_icon = icons::icon(icons::VSC_REFRESH).size(px(16.0)).text_color(
            match (self.refreshing, self.refresh_feedback) {
                (true, _) => theme.accent,
                (_, Some(false)) => theme.danger,
                _ => theme.text_muted,
            },
        );
        let refresh_icon = if self.refreshing {
            let phase =
                crate::motion::pulse_delta(&crate::motion::GRADIENT_SPIN, cx.entity_id(), cx);
            refresh_icon.with_transformation(gpui::Transformation::rotate(gpui::percentage(phase)))
        } else {
            refresh_icon
        };
        let mut more = Self::git_icon("git-more", icons::VSC_MORE, "More Actions", enabled, &theme)
            .relative()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.actions_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                if !this.git_enabled() {
                    return;
                }
                if this.actions_menu.take_press_was_open() {
                    this.close_actions_menu(cx);
                } else {
                    this.close_commit_menu(cx);
                    this.git_panel = Some(GitPanel::Actions);
                    this.git_form = None;
                    this.actions_menu.open(());
                    this.details_key = None;
                    cx.notify();
                }
            }));
        if self.actions_menu.get().is_some() {
            let menu = self.render_git_panel(cx);
            more = more.child(GitDropdown {
                id: "git-actions-dropdown".into(),
                card: popover::popover_card(&theme.for_popup())
                    .id("git-actions-menu-card")
                    .rounded(px(4.0))
                    .w(px(300.0))
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_actions_menu(cx)))
                    .on_click(|_, _, cx| cx.stop_propagation()),
                body: menu,
                closing: self.actions_menu.closing_since(),
                trigger_height: 24.0,
                bounds: self.actions_menu_bounds.clone(),
            });
        }
        div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .child(
                div()
                    .id("git-refresh")
                    .debug_selector(|| "git-refresh".into())
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.0))
                    .role(gpui::Role::Button)
                    .aria_label(refresh_label)
                    .tooltip(crate::settings::widgets::text_tooltip(refresh_label))
                    .when(self.refreshing, |el| el.bg(theme.accent.opacity(0.1)))
                    .when(!self.is_busy() && !self.refreshing, |el| {
                        el.cursor_pointer().hover(|s| s.bg(theme.glass_hover()))
                    })
                    .when(self.is_busy(), |el| el.opacity(0.4))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.refresh(cx);
                    }))
                    .child(refresh_icon),
            )
            .child(more)
            .into_any_element()
    }

    pub(super) fn git_icon(
        id: &'static str,
        icon: &'static str,
        label: &'static str,
        enabled: bool,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .size(px(24.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(3.0))
            .role(gpui::Role::Button)
            .aria_label(label)
            .tooltip(crate::settings::widgets::text_tooltip(label))
            .when(enabled, |el| {
                el.cursor_pointer().hover(|s| s.bg(theme.glass_hover()))
            })
            .when(!enabled, |el| el.opacity(0.4))
            .child(
                icons::icon(icon)
                    .size(px(16.0))
                    .text_color(theme.text_muted),
            )
    }

    pub(super) fn render_git_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(state) = self.git_state() else {
            return gpui::Empty.into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let branch = state.branch.clone().unwrap_or_else(|| {
            state
                .head
                .as_ref()
                .map(|h| h[..7.min(h.len())].into())
                .unwrap_or_else(|| "No commits".into())
        });
        let published = state.upstream.is_some();
        let enabled = self.can_sync();
        let syncing = self.operation_label.as_deref() == Some("Syncing changes…");
        let label = match (state.behind, state.ahead) {
            (Some(behind), Some(ahead)) if published => format!("{behind}↓ {ahead}↑"),
            _ => "Not published".into(),
        };
        let tooltip = if syncing {
            "Syncing incoming and outgoing commits…".into()
        } else if !published {
            "Publish this branch from More Actions to sync changes".into()
        } else if state.conflicts > 0 || state.operation.is_some() {
            "Resolve the current Git operation before syncing changes".into()
        } else {
            format!(
                "Sync Changes ({} incoming, {} outgoing)",
                state.behind.unwrap_or(0), state.ahead.unwrap_or(0)
            )
        };
        let icon = icons::icon(icons::VSC_SYNC)
            .size(px(14.0))
            .text_color(theme.text_muted);
        let icon = if syncing {
            let phase = crate::motion::pulse_delta(&crate::motion::GRADIENT_SPIN, cx.entity_id(), cx);
            icon.with_transformation(gpui::Transformation::rotate(gpui::percentage(phase)))
        } else {
            icon
        };
        div()
            .h(px(24.0))
            .flex_none()
            .w_full()
            .px(px(10.0))
            .flex()
            .items_center()
            .gap(px(7.0))
            .border_t_1()
            .border_color(theme.border)
            .text_size(px(12.0))
            .text_color(theme.text_muted)
            .child(
                icons::icon(icons::VSC_BRANCH)
                    .size(px(14.0))
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(branch)),
            )
            .child(
                div()
                    .id("git-status-sync")
                    .debug_selector(|| "git-status-sync".into())
                    .h_full()
                    .px(px(5.0))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .role(gpui::Role::Button)
                    .aria_label(tooltip.clone())
                    .tooltip(crate::settings::widgets::text_tooltip(tooltip))
                    .when(published, |el| el.child(icon))
                    .child(SharedString::from(label))
                    .when(enabled, |el| {
                        el.cursor_pointer().hover(|style| style.bg(theme.glass_hover()))
                    })
                    .when(!enabled, |el| el.opacity(0.5))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.can_sync() {
                            this.run_git(Action::Sync, cx);
                        }
                    })),
            )
            .into_any_element()
    }

    fn can_sync(&self) -> bool {
        self.git_enabled()
            && self.git_state().is_some_and(|state| {
                state.upstream.is_some()
                    && state.ahead.is_some()
                    && state.behind.is_some()
                    && state.conflicts == 0
                    && state.operation.is_none()
            })
    }

    pub(super) fn commit_with_followup(&mut self, action: Option<Action>, cx: &mut Context<Self>) {
        if !self.can_commit(cx) {
            return;
        }
        self.close_commit_menu(cx);
        self.close_actions_menu(cx);
        self.commit_followup = action;
        self.commit_staged(cx);
    }

    pub(super) fn render_commit_button(
        &mut self,
        enabled: bool,
        committing: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let foreground = if enabled {
            theme.on_solid
        } else {
            theme.text_muted
        };
        let dropdown = self.render_commit_dropdown(foreground, cx);
        div()
            .relative()
            .h(px(28.0))
            .w_full()
            .flex()
            .items_center()
            .rounded(px(2.0))
            .bg(if enabled {
                theme.solid
            } else {
                theme.element_active
            })
            .child(
                div()
                    .id("source-control-commit")
                    .debug_selector(|| "source-control-commit".into())
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.0))
                    .text_size(px(13.0))
                    .text_color(foreground)
                    .role(gpui::Role::Button)
                    .aria_label("Commit staged changes")
                    .when(enabled, |el| {
                        el.cursor_pointer()
                            .hover(|s| s.bg(foreground.opacity(0.08)))
                    })
                    .when(!enabled, |el| el.opacity(0.45))
                    .tooltip(crate::settings::widgets::text_tooltip(
                        "Commit staged changes (Ctrl+Enter)",
                    ))
                    .on_click(cx.listener(|this, _, _, cx| this.commit_with_followup(None, cx)))
                    .child(
                        icons::icon(icons::VSC_CHECK)
                            .size(px(16.0))
                            .text_color(foreground),
                    )
                    .child(if committing {
                        "Committing…"
                    } else if self.amend {
                        "Commit (Amend)"
                    } else {
                        "Commit"
                    }),
            )
            .child(dropdown)
            .into_any_element()
    }

    pub(super) fn render_commit_dropdown(
        &mut self,
        foreground: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let enabled = !self.is_busy() && self.confirmation.is_none();
        let mut trigger = div()
            .id("git-commit-dropdown")
            .relative()
            .w(px(28.0))
            .h_full()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .border_l_1()
            .border_color(foreground.opacity(0.2))
            .text_color(foreground)
            .role(gpui::Role::Button)
            .aria_label("Commit actions")
            .cursor_pointer()
            .hover(move |s| s.bg(foreground.opacity(0.08)))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.commit_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                if this.is_busy() || this.confirmation.is_some() {
                    return;
                }
                if this.commit_menu.take_press_was_open() {
                    this.close_commit_menu(cx);
                } else {
                    this.close_actions_menu(cx);
                    this.commit_menu.open(());
                    cx.notify();
                }
            }))
            .child(
                icons::icon(icons::VSC_CHEVRON)
                    .size(px(16.0))
                    .text_color(foreground),
            );
        if self.commit_menu.get().is_some() {
            let can_commit = self.can_commit(cx);
            let card = popover::popover_card(&theme.for_popup())
                .id("git-commit-menu-card")
                .rounded(px(4.0))
                .w(px(248.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_commit_menu(cx)))
                .on_click(|_, _, cx| cx.stop_propagation());
            let menu = div().flex().flex_col().flex_none()
                .child(button("git-commit-only", "Commit Staged", can_commit, &theme).on_click(cx.listener(|this, _, _, cx| this.commit_with_followup(None, cx))))
                .child(button("git-commit-push", "Commit & Push", can_commit, &theme).on_click(cx.listener(|this, _, _, cx| this.commit_with_followup(Some(Action::Push), cx))))
                .child(button("git-commit-sync", "Commit & Sync", can_commit, &theme).on_click(cx.listener(|this, _, _, cx| this.commit_with_followup(Some(Action::Sync), cx))))
                .child(div().my(px(4.0)).border_b_1().border_color(theme.border))
                .child(button("git-amend", if self.amend { "Cancel Amend" } else { "Commit (Amend)…" }, enabled, &theme).on_click(cx.listener(|this, _, _, cx| { this.close_commit_menu(cx); this.toggle_amend(cx); })))
                .child(button("git-undo-commit", "Undo Last Commit…", enabled, &theme).on_click(cx.listener(|this, _, _, cx| { this.close_commit_menu(cx); this.confirm_git(Action::UndoCommit, "Undo last local commit?".into(), "Its changes will remain staged and its message will return to the commit input.".into(), cx); })));
            trigger = trigger.child(GitDropdown {
                id: "git-commit-options".into(),
                card,
                body: menu.into_any_element(),
                closing: self.commit_menu.closing_since(),
                trigger_height: 28.0,
                bounds: self.commit_menu_bounds.clone(),
            });
        }
        trigger.into_any_element()
    }

    pub(super) fn render_git_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.confirmation.is_some() {
            return gpui::Empty.into_any_element();
        }
        let theme = Theme::of(cx).clone();
        let enabled = self.git_enabled();
        if let Some(form) = &self.git_form {
            let title = match form {
                GitForm::CreateBranch => "Create Branch",
                GitForm::RenameBranch => "Rename Branch",
                GitForm::Stash(_) => "Stash Message (optional)",
                GitForm::AddRemote => "Remote Name and URL",
            };
            return div()
                .flex_none()
                .p(px(10.0))
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .child(title),
                )
                .child(
                    div()
                        .p(px(8.0))
                        .border_1()
                        .border_color(theme.border_strong)
                        .rounded(px(3.0))
                        .bg(theme.input_glass_bg())
                        .child(self.form_input.clone()),
                )
                .when(matches!(form, GitForm::AddRemote), |el| {
                    el.child(
                        div()
                            .p(px(8.0))
                            .border_1()
                            .border_color(theme.border_strong)
                            .rounded(px(3.0))
                            .bg(theme.input_glass_bg())
                            .child(self.form_second.clone()),
                    )
                })
                .when_some(self.error.clone(), |el, error| {
                    el.child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.danger)
                            .child(error),
                    )
                })
                .child(
                    div()
                        .flex()
                        .gap(px(6.0))
                        .child(button("git-form-cancel", "Cancel", true, &theme).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.git_form = None;
                                this.close_actions_menu(cx);
                                cx.notify();
                            }),
                        ))
                        .child(
                            button("git-form-submit", "Save", enabled, &theme)
                                .bg(theme.accent.opacity(0.15))
                                .text_color(theme.accent)
                                .on_click(cx.listener(|this, _, _, cx| this.submit_form(cx))),
                        ),
                )
                .into_any_element();
        }
        let Some(panel) = self.git_panel.clone() else {
            return gpui::Empty.into_any_element();
        };
        let mut body = div()
            .id("git-actions-panel")
            .flex_none()
            .p(px(4.0))
            .flex()
            .flex_col()
            .gap(px(3.0))
            .when(panel != GitPanel::Actions, |el| {
                el.child(
                    button("git-panel-back", "← Git actions", true, &theme).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.git_panel = Some(GitPanel::Actions);
                            cx.notify();
                        },
                    )),
                )
            });
        match panel {
            GitPanel::Actions => {
                for (id, label, action) in [
                    ("git-fetch", "Fetch", Action::Fetch),
                    ("git-pull", "Pull", Action::Pull { rebase: None }),
                    ("git-push", "Push", Action::Push),
                    ("git-sync", "Sync", Action::Sync),
                ] {
                    body = body.child(button(id, label, enabled, &theme).on_click(
                        cx.listener(move |this, _, _, cx| this.run_git(action.clone(), cx)),
                    ));
                }
                body = body.child(div().my(px(4.0)).border_b_1().border_color(theme.border));
                if let Some(key) = self.selected_files.iter().next().cloned() {
                    let paths = self.staging_paths(&key);
                    let discard_paths = self.discard_paths(&key);
                    let discard_repository = key.repository.clone();
                    let staged = key.staged;
                    let complete = self.snapshot.as_ref().is_some_and(|s| {
                        s.repositories
                            .iter()
                            .any(|r| r.path == key.repository && r.complete)
                    });
                    body = body.child(
                        button(
                            "git-stage-selected",
                            if staged {
                                "Unstage Selected Changes"
                            } else {
                                "Stage Selected Changes"
                            },
                            enabled && complete,
                            &theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if complete && this.git_enabled() {
                                this.close_actions_menu(cx);
                                this.set_staged(key.repository.clone(), paths.clone(), !staged, cx);
                            }
                        })),
                    );
                    if !staged {
                        body = body.child(
                            button(
                                "git-discard-selected",
                                "Discard Selected Changes…",
                                enabled && complete && !discard_paths.is_empty(),
                                &theme,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if complete && this.git_enabled() {
                                    this.request_discard(
                                        discard_repository.clone(),
                                        discard_paths.clone(),
                                        false,
                                        cx,
                                    );
                                }
                            })),
                        );
                    }
                }
                body = body.child(
                    button("git-discard-all", "Discard All Changes…", enabled, &theme).on_click(
                        cx.listener(|this, _, _, cx| {
                            if this.git_enabled() {
                                this.discard_all(false, cx);
                            }
                        }),
                    ),
                );
                if self.undo_discard.is_some() {
                    body = body.child(
                        button("git-undo-discard", "Undo Discard", enabled, &theme)
                            .on_click(cx.listener(|this, _, _, cx| this.undo_discard(cx))),
                    );
                }
                if self.git_state().is_some_and(|s| s.operation.is_some()) {
                    body = body.child(button("git-continue", "Continue Git Operation", enabled, &theme).on_click(cx.listener(|this, _, _, cx| this.run_git(Action::Continue, cx))))
                        .child(button("git-abort", "Abort Git Operation…", enabled, &theme).on_click(cx.listener(|this, _, _, cx| this.confirm_git(Action::Abort, "Abort Git operation?".into(), "Git will restore the repository to the state before this operation.".into(), cx))));
                }
                if let Some(selection) = self.selected.clone()
                    && selection.repository == self.active_repository
                    && self.git_state().is_some_and(|s| s.conflicts > 0)
                {
                    let current = selection.path.clone();
                    let incoming = current.clone();
                    body = body
                        .child(
                            button(
                                "git-accept-current",
                                "Accept Current Version",
                                enabled,
                                &theme,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.run_git(
                                        Action::ResolveConflict {
                                            path: current.clone(),
                                            incoming: false,
                                        },
                                        cx,
                                    )
                                },
                            )),
                        )
                        .child(
                            button(
                                "git-accept-incoming",
                                "Accept Incoming Version",
                                enabled,
                                &theme,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.run_git(
                                        Action::ResolveConflict {
                                            path: incoming.clone(),
                                            incoming: true,
                                        },
                                        cx,
                                    )
                                },
                            )),
                        );
                }
                for (id, label, panel) in [
                    ("branches", "Branches…", GitPanel::Branches),
                    (
                        "history",
                        "Incoming / Outgoing / Recent Commits…",
                        GitPanel::History,
                    ),
                    ("stashes", "Stashes…", GitPanel::Stashes),
                    ("remotes", "Remotes / Publish Branch…", GitPanel::Remotes),
                ] {
                    body = body.child(button(id, label, enabled, &theme).on_click(
                        cx.listener(move |this, _, _, cx| this.open_git_panel(panel.clone(), cx)),
                    ));
                }
                body = body
                    .child(button("git-amend", "Amend Last Commit…", enabled, &theme).on_click(cx.listener(|this, _, _, cx| this.toggle_amend(cx))))
                    .child(button("git-undo-commit", "Undo Last Commit…", enabled, &theme).on_click(cx.listener(|this, _, _, cx| this.confirm_git(Action::UndoCommit, "Undo last local commit?".into(), "Its changes will remain staged and its message will return to the commit input. Git keeps a recovery reference.".into(), cx))))
                    .child(button("git-pull-merge", "Pull (Merge)", enabled, &theme).on_click(cx.listener(|this, _, _, cx| this.run_git(Action::Pull { rebase: Some(false) }, cx))))
                    .child(button("git-pull-rebase", "Pull (Rebase)", enabled, &theme).on_click(cx.listener(|this, _, _, cx| this.run_git(Action::Pull { rebase: Some(true) }, cx))))
                    .child(button("git-discard-everything", "Discard Staged and Unstaged Changes…", enabled, &theme).text_color(theme.danger).on_click(cx.listener(|this, _, _, cx| { if this.git_enabled() { this.discard_all(true, cx); } })))
                    .child(button("git-refresh", "Refresh", enabled, &theme).debug_selector(|| "git-menu-refresh".into()).on_click(cx.listener(|this, _, _, cx| { this.git_panel = None; this.close_actions_menu(cx); this.refresh(cx); })));
            }
            GitPanel::Branches => {
                body = body
                    .child(
                        button("git-create-branch", "＋ Create Branch…", enabled, &theme).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.start_form(GitForm::CreateBranch, window, cx)
                            }),
                        ),
                    )
                    .child(
                        button(
                            "git-rename-branch",
                            "Rename Current Branch…",
                            enabled,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.start_form(GitForm::RenameBranch, window, cx)
                        })),
                    );
                if let Some(details) = self.git_details.clone() {
                    for (i, branch) in details.branches.into_iter().enumerate() {
                        let name = branch.name.clone();
                        let merge = name.clone();
                        let rebase = name.clone();
                        let delete = name.clone();
                        body = body.child(div().flex().items_center()
                            .child(button(format!("git-switch-{i}"), SharedString::from(format!("{}{}", if branch.current { "✓ " } else if branch.remote { "↗ " } else { "" }, name)), enabled && !branch.current, &theme).flex_1().min_w_0().truncate().on_click(cx.listener(move |this, _, _, cx| this.run_git(Action::SwitchBranch { branch: name.clone() }, cx))))
                            .when(!branch.current, |el| el
                                .child(button(format!("git-merge-{i}"), "Merge", enabled, &theme).on_click(cx.listener(move |this, _, _, cx| this.run_git(Action::Merge { branch: merge.clone() }, cx))))
                                .child(button(format!("git-rebase-{i}"), "Rebase", enabled, &theme).on_click(cx.listener(move |this, _, _, cx| this.run_git(Action::Rebase { branch: rebase.clone() }, cx))))
                                .when(!branch.remote, |el| el.child(button(format!("git-delete-{i}"), "×", enabled, &theme).tooltip(crate::settings::widgets::text_tooltip("Delete merged branch")).on_click(cx.listener(move |this, _, _, cx| this.confirm_git(Action::DeleteBranch { name: delete.clone() }, format!("Delete branch {delete}?"), "Only a fully merged local branch can be deleted.".into(), cx)))))));
                    }
                }
            }
            GitPanel::Stashes => {
                body = body
                    .child(
                        button("git-stash", "Stash Tracked Changes…", enabled, &theme).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.start_form(GitForm::Stash(false), window, cx)
                            }),
                        ),
                    )
                    .child(
                        button(
                            "git-stash-untracked",
                            "Stash Including Untracked…",
                            enabled,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.start_form(GitForm::Stash(true), window, cx)
                        })),
                    );
                if let Some(details) = self.git_details.clone() {
                    if details.stashes.is_empty() {
                        body = body.child(
                            div()
                                .p(px(8.0))
                                .text_size(px(11.0))
                                .text_color(theme.text_faint)
                                .child("No stashes"),
                        );
                    }
                    for (i, stash) in details.stashes.into_iter().enumerate() {
                        let apply = stash.sha.clone();
                        let pop = apply.clone();
                        let drop = apply.clone();
                        body = body.child(div().py(px(5.0)).border_b_1().border_color(theme.border).flex().flex_col()
                            .child(div().text_size(px(11.0)).text_color(theme.text_muted).child(SharedString::from(stash.subject)))
                            .child(div().flex()
                                .child(button(format!("git-stash-apply-{i}"), "Apply", enabled, &theme).on_click(cx.listener(move |this, _, _, cx| this.run_git(Action::ApplyStash { sha: apply.clone() }, cx))))
                                .child(button(format!("git-stash-pop-{i}"), "Pop", enabled, &theme).on_click(cx.listener(move |this, _, _, cx| this.run_git(Action::PopStash { sha: pop.clone() }, cx))))
                                .child(button(format!("git-stash-drop-{i}"), "Drop…", enabled, &theme).on_click(cx.listener(move |this, _, _, cx| this.confirm_git(Action::DropStash { sha: drop.clone() }, "Delete stash?".into(), "This removes the saved stash without applying it.".into(), cx))))));
                    }
                }
            }
            GitPanel::Remotes => {
                body = body.child(
                    button("git-add-remote", "＋ Add Remote…", enabled, &theme).on_click(
                        cx.listener(|this, _, window, cx| {
                            this.start_form(GitForm::AddRemote, window, cx)
                        }),
                    ),
                );
                if let Some(details) = self.git_details.clone() {
                    if details.remotes.is_empty() {
                        body = body.child(
                            div()
                                .p(px(8.0))
                                .text_size(px(11.0))
                                .text_color(theme.text_faint)
                                .child("Add a remote to publish this branch"),
                        );
                    }
                    for (i, remote) in details.remotes.into_iter().enumerate() {
                        let name = remote.name.clone();
                        let remove = name.clone();
                        body = body.child(div().py(px(5.0)).border_b_1().border_color(theme.border).flex().flex_col()
                            .child(div().text_size(px(12.0)).text_color(theme.text).child(SharedString::from(remote.name)))
                            .child(div().text_size(px(10.0)).text_color(theme.text_faint).overflow_hidden().child(SharedString::from(remote.url)))
                            .child(div().flex()
                                .child(button(format!("git-publish-{i}"), "Publish Branch", enabled, &theme).on_click(cx.listener(move |this, _, _, cx| this.run_git(Action::Publish { remote: name.clone() }, cx))))
                                .child(button(format!("git-remove-remote-{i}"), "Remove…", enabled, &theme).on_click(cx.listener(move |this, _, _, cx| this.confirm_git(Action::RemoveRemote { name: remove.clone() }, format!("Remove remote {remove}?"), "This removes its local configuration and tracking references. The remote repository is kept.".into(), cx))))));
                    }
                }
            }
            GitPanel::History => {
                if let Some(details) = self.git_details.clone() {
                    for (section, commits) in [
                        ("Incoming", details.incoming),
                        ("Outgoing", details.outgoing),
                        ("Recent", details.recent),
                    ] {
                        body = body.child(
                            div()
                                .pt(px(10.0))
                                .pb(px(5.0))
                                .text_size(px(11.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme.text_muted)
                                .child(section),
                        );
                        if commits.is_empty() {
                            body = body.child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme.text_faint)
                                    .child("No commits"),
                            );
                        }
                        for (i, commit) in commits.into_iter().enumerate() {
                            let sha = commit.sha.clone();
                            let pick_sha = sha.clone();

                            let label = format!(
                                "{}  {}",
                                &commit.sha[..7.min(commit.sha.len())],
                                commit.subject
                            );
                            body = body.child(
                                div().flex().items_center().child(button(
                                    format!("git-history-{section}-{i}"),
                                    SharedString::from(label),
                                    enabled,
                                    &theme,
                                )
                                .flex_1().min_w_0()
                                .truncate()
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if !this.git_enabled() {
                                            return;
                                        }
                                        if let Some(target) = this.target.clone() {
                                            this.close_actions_menu(cx);
                                            cx.emit(SourceControlEvent::OpenCommit {
                                                cwd: target.cwd.clone(),
                                                repository: this.active_repository.clone(),
                                                commit: commit.clone(),
                                            });
                                        }
                                    },
                                )))
                                .when(section == "Recent", |el| el.child(button(format!("git-revert-commit-{i}"), "↶", enabled, &theme)
                                    .tooltip(crate::settings::widgets::text_tooltip("Revert commit"))
                                    .on_click(cx.listener(move |this, _, _, cx| this.confirm_git(Action::RevertCommit { sha: sha.clone() }, "Revert this commit?".into(), "Git will create a new commit that reverses this commit's changes. Existing history is kept.".into(), cx)))))
                                .when(section == "Incoming", |el| el.child(button(format!("git-cherry-pick-{i}"), "＋", enabled, &theme)
                                    .tooltip(crate::settings::widgets::text_tooltip("Cherry-pick commit"))
                                    .on_click(cx.listener(move |this, _, _, cx| this.run_git(Action::CherryPick { sha: pick_sha.clone() }, cx))))),
                            );
                        }
                    }
                }
            }
        }
        if self.git_details.is_none() {
            body = body.child(
                div()
                    .p(px(8.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("Loading Git details…"),
            );
        }
        body.into_any_element()
    }

    pub(super) fn discard_button(
        &self,
        id: String,
        enabled: bool,
        group: SharedString,
        selected: bool,
        count: Option<usize>,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let enabled = enabled && !self.is_busy() && self.confirmation.is_none();
        let opacity = if enabled { 1.0 } else { 0.35 };
        let label = match count {
            Some(count) if count > 1 => format!("Discard changes in {count} selected files"),
            _ => "Discard changes".into(),
        };
        div()
            .id(SharedString::from(id))
            .size(px(20.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .role(gpui::Role::Button)
            .aria_label(label.clone())
            .opacity(if selected { opacity } else { 0.0 })
            .group_hover(group, move |el| el.opacity(opacity))
            .when(enabled, |el| {
                el.cursor_pointer().hover(|s| s.bg(theme.glass_hover()))
            })
            .tooltip(crate::settings::widgets::text_tooltip(label))
            .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .child(
                icons::icon(icons::VSC_DISCARD)
                    .size(px(14.0))
                    .text_color(theme.text_muted),
            )
    }

    pub(super) fn open_file_button(
        &self,
        id: String,
        enabled: bool,
        group: SharedString,
        selected: bool,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let enabled = enabled && !self.is_busy();
        let opacity = if enabled { 1.0 } else { 0.35 };
        div()
            .id(SharedString::from(id))
            .size(px(20.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(3.0))
            .role(gpui::Role::Button)
            .aria_label("Open File")
            .opacity(if selected { opacity } else { 0.0 })
            .group_hover(group, move |el| el.opacity(opacity))
            .when(enabled, |el| {
                el.cursor_pointer().hover(|s| s.bg(theme.glass_hover()))
            })
            .tooltip(crate::settings::widgets::text_tooltip("Open File"))
            .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .child(
                icons::icon(icons::VSC_OPEN_FILE)
                    .size(px(16.0))
                    .text_color(theme.text_muted),
            )
    }
}
