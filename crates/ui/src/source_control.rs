//! A compact source-control list. Only metadata is polled; file patches are
//! fetched when a row is selected, independently of the repository list.
use crate::{
    composer::{ComposerInput, ComposerInputEvent},
    file_icons::{self, FileIconIdentity},
    icons, popover,
    state::AppState,
    theme::Theme,
};
use gpui::{
    AnyElement, App, Context, Entity, SharedString, Task, Window, div, prelude::*, px, uniform_list,
};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use zeron_proto::{CheckoutChangeSelection, CheckoutChanges, GitFileState, GitFileStatus};
use zeron_rpc::methods;
mod git;
mod refresh;
mod selection;
use git::{Confirmation, ConfirmationDialog, GitForm, GitPanel, UndoDiscard};
use selection::FileKey;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Target {
    chat: String,
    cwd: String,
    device: Option<String>,
}

#[derive(Clone)]
enum Row {
    Repository(usize),
    Group {
        repository: usize,
        staged: bool,
    },
    File {
        repository: usize,
        file: usize,
        staged: bool,
    },
}

pub enum SourceControlEvent {
    OpenDiff(CheckoutChangeSelection),
    OpenFile(String),
    OpenCommit {
        cwd: String,
        repository: String,
        commit: zeron_proto::GitHistoryCommit,
    },
}

pub struct SourceControl {
    state: Entity<AppState>,
    target: Option<Target>,
    snapshot: Option<CheckoutChanges>,
    rows: Vec<Row>,
    collapsed: HashSet<String>,
    selected: Option<CheckoutChangeSelection>,
    selected_files: HashSet<FileKey>,
    selection_anchor: Option<FileKey>,
    error: Option<SharedString>,
    load_error: Option<SharedString>,
    notice: Option<SharedString>,
    commit_input: Entity<ComposerInput>,
    _commit_input_events: gpui::Subscription,
    active_repository: String,
    drafts: HashMap<(Target, String), String>,
    pending_commit: Option<(Target, String)>,
    busy: bool,
    mutation_epoch: u64,
    poll: Option<Task<()>>,
    refresh_task: Option<Task<()>>,
    refreshing: bool,
    refresh_feedback: Option<bool>,
    mutation: Option<Task<()>>,
    git_details: Option<zeron_proto::RepositoryGitDetails>,
    details_key: Option<(Target, String, Option<zeron_proto::RepositoryGitState>)>,
    details_task: Option<Task<()>>,
    git_panel: Option<GitPanel>,
    git_form: Option<GitForm>,
    form_input: Entity<ComposerInput>,
    form_second: Entity<ComposerInput>,
    _form_events: Vec<gpui::Subscription>,
    confirmation: Option<Confirmation>,
    confirmation_dialog: Option<ConfirmationDialog>,
    undo_discard: Option<UndoDiscard>,
    operation_label: Option<SharedString>,
    amend: bool,
    actions_menu: popover::Popup<()>,
    commit_menu: popover::Popup<()>,
    actions_menu_bounds: selection::MenuBounds,
    commit_menu_bounds: selection::MenuBounds,
    commit_followup: Option<zeron_proto::RepositoryGitAction>,
}
impl gpui::EventEmitter<SourceControlEvent> for SourceControl {}

fn in_group(file: &GitFileStatus, staged: bool) -> bool {
    let conflict = [file.index, file.worktree].contains(&GitFileState::Unmerged)
        || matches!(
            (file.index, file.worktree),
            (GitFileState::Added, GitFileState::Added)
                | (GitFileState::Deleted, GitFileState::Deleted)
        );
    if conflict {
        return !staged;
    }
    if staged {
        !matches!(
            file.index,
            GitFileState::Unchanged | GitFileState::Untracked | GitFileState::Unmerged
        )
    } else {
        file.worktree != GitFileState::Unchanged || file.index == GitFileState::Unmerged
    }
}

fn status_letter(file: &GitFileStatus, staged: bool, submodule: bool) -> &'static str {
    if [file.index, file.worktree].contains(&GitFileState::Unmerged) {
        return "!";
    }
    if submodule {
        return "S";
    }
    use GitFileState::*;
    match if staged { file.index } else { file.worktree } {
        Added => "A",
        Modified => "M",
        Deleted => "D",
        Renamed => "R",
        Copied => "C",
        Unmerged => "!",
        Untracked => "U",
        TypeChanged => "T",
        Unchanged => "!",
    }
}

fn status_color(file: &GitFileStatus, staged: bool, theme: &Theme) -> gpui::Hsla {
    use GitFileState::*;
    match if staged { file.index } else { file.worktree } {
        Added | Untracked => theme.success,
        Deleted | Unmerged => theme.danger,
        Renamed | Copied => theme.accent,
        _ => theme.warning,
    }
}

impl SourceControl {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let commit_input = cx.new(|cx| {
            ComposerInput::with_context("Message (Ctrl+Enter to commit)", "GitCommitMessage", cx)
                .with_text_metrics(13.0, 18.0)
                .with_max_height(72.0)
        });
        let events = cx.subscribe(&commit_input, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Edited => {
                this.remember_draft(cx);
                this.error = None;
                if !this.commit_input.read(cx).text().is_empty() {
                    this.notice = None;
                }
                cx.notify();
            }
            ComposerInputEvent::Submitted => this.commit_staged(cx),
            _ => {}
        });
        let form_input = cx.new(|cx| {
            ComposerInput::new("Value", cx)
                .with_single_line()
                .with_text_metrics(12.0, 18.0)
        });
        let form_second = cx.new(|cx| {
            ComposerInput::new("Remote URL", cx)
                .with_single_line()
                .with_text_metrics(12.0, 18.0)
        });
        let form_events = [&form_input, &form_second]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this: &mut Self, _, event, cx| {
                    if matches!(event, ComposerInputEvent::Submitted) {
                        this.submit_form(cx);
                    }
                })
            })
            .collect();
        Self {
            state,
            target: None,
            snapshot: None,
            rows: Vec::new(),
            collapsed: HashSet::new(),
            selected: None,
            selected_files: HashSet::new(),
            selection_anchor: None,
            error: None,
            load_error: None,
            notice: None,
            commit_input,
            _commit_input_events: events,
            active_repository: String::new(),
            drafts: HashMap::new(),
            pending_commit: None,
            busy: false,
            mutation_epoch: 0,
            poll: None,
            refresh_task: None,
            refreshing: false,
            refresh_feedback: None,
            mutation: None,
            git_details: None,
            details_key: None,
            details_task: None,
            git_panel: None,
            git_form: None,
            form_input,
            form_second,
            _form_events: form_events,
            confirmation: None,
            confirmation_dialog: None,
            undo_discard: None,
            operation_label: None,
            amend: false,
            actions_menu: Default::default(),
            commit_menu: Default::default(),
            actions_menu_bounds: Default::default(),
            commit_menu_bounds: Default::default(),
            commit_followup: None,
        }
    }

    fn desired_target(&self, cx: &App) -> Option<Target> {
        let state = self.state.read(cx);
        let chat = state.selected_chat_row()?;
        Some(Target {
            chat: chat.id.clone(),
            cwd: chat.cwd.clone()?,
            device: (state.local_device_id.as_deref() != Some(&chat.device_id))
                .then(|| chat.device_id.clone()),
        })
    }

    pub fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        let target = self.desired_target(cx);
        let unavailable = target.is_none() || self.state.read(cx).engine().is_none();
        if self.target == target && (self.poll.is_some() || unavailable) {
            return;
        }
        self.remember_draft(cx);
        self.poll = None;
        self.refresh_task = None;
        self.refreshing = false;
        self.refresh_feedback = None;
        self.mutation = None;
        self.busy = false;
        self.snapshot = None;
        self.rows.clear();
        self.selected = None;
        self.selected_files.clear();
        self.selection_anchor = None;
        self.error = None;
        self.load_error = None;
        self.notice = None;
        self.git_details = None;
        self.details_key = None;
        self.details_task = None;
        self.git_panel = None;
        self.git_form = None;
        self.confirmation = None;
        self.undo_discard = None;
        self.operation_label = None;
        self.amend = false;
        self.actions_menu = Default::default();
        self.commit_menu = Default::default();
        self.commit_followup = None;
        self.active_repository.clear();
        self.target = target.clone();
        self.restore_draft(cx);
        // The cached list must also refresh when its repository is cleared,
        // including when the new conversation has no checkout to poll.
        cx.notify();
        let Some(target) = target else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok((busy, epoch)) = this.update(cx, |view, _| (view.is_busy() || view.refreshing, view.mutation_epoch)) else { return; };
                if !busy {
                    let response = engine.client().call(methods::GET_CHECKOUT_CHANGES, serde_json::json!({ "cwd": target.cwd, "targetDeviceId": target.device })).await;
                    if this.update(cx, |view, cx| {
                        if view.target.as_ref() != Some(&target) || view.is_busy() || view.mutation_epoch != epoch { return; }
                        match response.and_then(|value| serde_json::from_value::<CheckoutChanges>(value).map_err(|e| zeron_rpc::RpcError::Failed(e.to_string()))) {
                            Ok(snapshot) => {
                                let recovered = view.load_error.take().is_some();
                                if view.snapshot.as_ref() != Some(&snapshot) {
                                    view.apply_snapshot(snapshot, cx);
                                    cx.notify();
                                } else if recovered {
                                    cx.notify();
                                }
                            }
                            Err(error) => { view.load_error = Some(format!("Unable to load changes: {error}").into()); cx.notify(); }
                        }
                    }).is_err() { return; }
                }
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        }));
    }

    fn rebuild(&mut self) {
        self.rows.clear();
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        for (repository, repo) in snapshot.repositories.iter().enumerate() {
            self.rows.push(Row::Repository(repository));
            if self.collapsed.contains(&repo.path) {
                continue;
            }
            for staged in [true, false] {
                let files: Vec<_> = repo
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| in_group(f, staged))
                    .map(|(i, _)| i)
                    .collect();
                if files.is_empty() {
                    continue;
                }
                self.rows.push(Row::Group { repository, staged });
                if self.collapsed.contains(&format!("{}:{staged}", repo.path)) {
                    continue;
                }
                self.rows.extend(files.into_iter().map(|file| Row::File {
                    repository,
                    file,
                    staged,
                }));
            }
        }
    }

    fn toggle(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        self.rebuild();
        cx.notify();
    }

    fn set_staged(
        &mut self,
        repository: String,
        paths: Vec<String>,
        staged: bool,
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
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.mutation_epoch += 1;
        let selected_files = self.selected_files.clone();
        let selection_anchor = self.selection_anchor.clone();
        cx.notify();
        self.mutation = Some(cx.spawn(async move |this, cx| {
            let response = engine.client().call(methods::SET_CHECKOUT_STAGED, serde_json::json!({ "cwd": target.cwd, "targetDeviceId": target.device, "repository": repository, "paths": paths, "staged": staged })).await;
            this.update(cx, |view, cx| {
                if view.target.as_ref() != Some(&target) { return; }
                view.busy = false;
                match response.and_then(|value| serde_json::from_value::<CheckoutChanges>(value).map_err(|e| zeron_rpc::RpcError::Failed(e.to_string()))) {
                    Ok(snapshot) => {
                        let unchanged = view.selected_files == selected_files;
                        view.apply_snapshot(snapshot, cx);
                        if unchanged {
                            view.follow_staging(&repository, &paths, staged, selected_files, selection_anchor);
                        }
                    }
                    Err(error) => { view.error = Some(format!("Unable to update staging: {error}").into()); }
                }
                cx.notify();
            }).ok();
        }));
    }

    fn is_busy(&self) -> bool {
        self.busy
            || self
                .pending_commit
                .as_ref()
                .is_some_and(|(target, _)| Some(target) == self.target.as_ref())
    }

    fn remember_draft(&mut self, cx: &App) {
        if let Some(target) = &self.target {
            self.drafts.insert(
                (target.clone(), self.active_repository.clone()),
                self.commit_input.read(cx).text().to_owned(),
            );
        }
    }

    fn restore_draft(&mut self, cx: &mut Context<Self>) {
        let text = self
            .target
            .as_ref()
            .and_then(|target| {
                self.drafts
                    .get(&(target.clone(), self.active_repository.clone()))
            })
            .cloned()
            .unwrap_or_default();
        if self.commit_input.read(cx).text() == text {
            return;
        }
        self.commit_input
            .update(cx, |input, cx| input.set_text(text, cx));
    }

    fn select_repository(&mut self, path: String, cx: &mut Context<Self>) {
        if self.is_busy() || self.confirmation.is_some() || self.active_repository == path {
            return;
        }
        self.remember_draft(cx);
        self.active_repository = path;
        self.selected_files.clear();
        self.selection_anchor = None;
        self.git_details = None;
        self.details_key = None;
        self.details_task = None;
        self.git_panel = None;
        self.git_form = None;
        self.amend = false;
        self.actions_menu = Default::default();
        self.commit_menu = Default::default();
        self.commit_followup = None;
        self.restore_draft(cx);
        self.error = None;
        self.notice = None;
        cx.notify();
    }

    fn apply_snapshot(&mut self, snapshot: CheckoutChanges, cx: &mut Context<Self>) {
        if !snapshot
            .repositories
            .iter()
            .any(|repo| repo.path == self.active_repository)
        {
            self.select_repository(String::new(), cx);
        }
        self.snapshot = Some(snapshot);
        self.rebuild();
        self.prune_selection();
    }

    fn staged_count(&self) -> usize {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .repositories
                    .iter()
                    .find(|repo| repo.path == self.active_repository && repo.complete)
            })
            .map_or(0, |repo| {
                repo.files
                    .iter()
                    .filter(|file| in_group(file, true))
                    .count()
            })
    }

    fn can_commit(&self, cx: &App) -> bool {
        !self.is_busy()
            && self.pending_commit.is_none()
            && self.load_error.is_none()
            && self.confirmation.is_none()
            && (self.staged_count() > 0
                || (self.amend
                    && self
                        .git_state()
                        .is_some_and(|s| s.head.is_some() && s.operation.is_none())))
            && !self.commit_input.read(cx).text().trim().is_empty()
    }

    fn commit_staged(&mut self, cx: &mut Context<Self>) {
        if !self.can_commit(cx) {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.error = Some("Repository is unavailable".into());
            cx.notify();
            return;
        };
        let repository = self.active_repository.clone();
        let message = self.commit_input.read(cx).text().to_owned();
        let amend = self.amend;
        let followup = self.commit_followup.take();
        let head = self.git_state().and_then(|s| s.head.clone());
        self.remember_draft(cx);
        self.pending_commit = Some((target.clone(), repository.clone()));
        self.mutation_epoch += 1;
        self.error = None;
        self.notice = None;
        cx.notify();
        // Closing or switching a view must not cancel a user-submitted commit.
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::COMMIT_CHECKOUT_STAGED,
                    serde_json::json!({
                        "cwd": target.cwd, "targetDeviceId": target.device,
                        "repository": repository, "message": message,
                        "amend": amend, "expectedHead": head,
                    }),
                )
                .await;
            let committed = result.is_ok();
            let followup_result = if committed && let Some(action) = followup {
                match result.as_ref().ok().and_then(|v| v.get("git")).cloned()
                    .and_then(|v| serde_json::from_value::<zeron_proto::RepositoryGitState>(v).ok()) {
                    Some(git) => Some(engine.client().call(methods::RUN_CHECKOUT_GIT_ACTION, serde_json::json!({
                        "cwd": target.cwd, "targetDeviceId": target.device, "repository": repository,
                        "action": action, "expectedHead": git.head, "expectedBranch": git.branch,
                    })).await),
                    None => Some(Err(zeron_rpc::RpcError::Failed("Unable to read the committed branch; refresh before pushing".into()))),
                }
            } else { None };
            let Ok(epoch) = this.update(cx, |view, cx| {
                if !committed {
                    view.pending_commit = None;
                }
                if committed {
                    let key = (target.clone(), repository.clone());
                    if view.drafts.get(&key) == Some(&message) {
                        view.drafts.remove(&key);
                    }
                }
                if view.target.as_ref() == Some(&target) {
                    if committed {
                        view.details_key = None;
                        view.amend = false;
                        if view.active_repository == repository
                            && view.commit_input.read(cx).text() == message
                        {
                            view.commit_input
                                .update(cx, |input, cx| input.set_text("", cx));
                        }
                        view.notice = Some(
                            if amend {
                                "Last commit amended"
                            } else {
                                "Staged changes committed"
                            }
                            .into(),
                        );
                        if let Some(followup) = followup_result {
                            match followup {
                                Ok(value) => { view.notice = value.get("notice").and_then(|v| v.as_str()).map(|s| format!("Commit created. {s}").into()); }
                                Err(error) => { view.error = Some(format!("Commit succeeded; remote action failed: {error}").into()); }
                            }
                        }
                    } else if let Err(error) = result {
                        view.error = Some(format!("Unable to commit: {error}").into());
                    }
                }
                cx.notify();
                view.mutation_epoch
            }) else {
                return;
            };
            if committed {
                let refreshed = engine
                    .client()
                    .call_as::<CheckoutChanges>(
                        methods::GET_CHECKOUT_CHANGES,
                        serde_json::json!({ "cwd": target.cwd, "targetDeviceId": target.device }),
                    )
                    .await;
                this.update(cx, |view, cx| {
                    view.pending_commit = None;
                    cx.notify();
                    if view.target.as_ref() != Some(&target)
                        || view.is_busy()
                        || view.mutation_epoch != epoch
                    {
                        return;
                    }
                    match refreshed {
                        Ok(snapshot) => {
                            view.load_error = None;
                            view.apply_snapshot(snapshot, cx);
                        }
                        Err(error) => {
                            view.load_error = Some(
                                format!("Commit succeeded; unable to refresh changes: {error}")
                                    .into(),
                            )
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn render_commit(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(snapshot) = &self.snapshot else {
            return gpui::Empty.into_any_element();
        };
        let Some(repo) = snapshot
            .repositories
            .iter()
            .find(|repo| repo.path == self.active_repository)
        else {
            return gpui::Empty.into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let enabled = self.can_commit(cx);
        let committing = self
            .pending_commit
            .as_ref()
            .is_some_and(|(target, repository)| {
                Some(target) == self.target.as_ref() && repository == &self.active_repository
            });
        let input_focused =
            gpui::Focusable::focus_handle(self.commit_input.read(cx), cx).is_focused(window);
        div()
            .flex_none()
            .w_full()
            .px(px(10.0))
            .py(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("commit-message")
                    .tooltip(crate::settings::widgets::text_tooltip(format!(
                        "Commit message for {}",
                        repo.name
                    )))
                    .debug_selector(|| "commit-message".into())
                    .w_full()
                    .min_h(px(36.0))
                    .p(px(8.0))
                    .rounded(px(2.0))
                    .border_1()
                    .border_color(if input_focused {
                        theme.accent
                    } else {
                        theme.border_strong
                    })
                    .bg(theme.input_glass_bg())
                    .child(self.commit_input.clone()),
            )
            .child(self.render_commit_button(enabled, committing, cx))
            .into_any_element()
    }

    fn action(
        &self,
        id: String,
        staged: bool,
        enabled: bool,
        row_group: SharedString,
        selected: bool,
        selection_count: Option<usize>,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let enabled = enabled && !self.is_busy();
        let opacity = if enabled { 1.0 } else { 0.35 };
        let verb = if staged { "Unstage" } else { "Stage" };
        let label = match selection_count {
            Some(count) if count > 1 => format!("{verb} selected changes ({count} files)"),
            Some(_) => format!("{verb} changes"),
            None => format!("{verb} all changes"),
        };
        let debug_id = id.clone();
        div()
            .id(SharedString::from(id))
            .debug_selector(move || debug_id.clone().into())
            .role(gpui::Role::Button)
            .aria_label(label.clone())
            .size(px(20.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .opacity(if selected { opacity } else { 0.0 })
            .group_hover(row_group, move |el| el.opacity(opacity))
            .when(enabled, |el| {
                el.cursor_pointer().hover(|s| s.bg(theme.glass_hover()))
            })
            .tooltip(crate::settings::widgets::text_tooltip(label))
            .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .child(
                icons::icon(if staged {
                    icons::VSC_REMOVE
                } else {
                    icons::VSC_ADD
                })
                .size(px(if staged { 16.0 } else { 14.0 }))
                .text_color(theme.text_muted),
            )
    }

    fn count_badge(count: usize, theme: &Theme) -> gpui::Div {
        div()
            .h(px(16.0))
            .min_w(px(18.0))
            .px(px(5.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.0))
            .bg(theme.glass_hover())
            .text_size(px(10.0))
            .text_color(theme.text_muted)
            .child(count.to_string())
    }

    fn render_row(&mut self, row: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(snapshot) = &self.snapshot else {
            return div().into_any_element();
        };
        let row_model = self.rows[row].clone();
        let (repository, staged) = match row_model {
            Row::Repository(i) => (i, false),
            Row::Group {
                repository, staged, ..
            }
            | Row::File {
                repository, staged, ..
            } => (repository, staged),
        };
        let repo = &snapshot.repositories[repository];
        let repo_path = repo.path.clone();
        let row_group: SharedString = format!("source-control-row-{row}").into();
        let base = div()
            .id(("source-control-row", row))
            .group(row_group.clone())
            .h(px(22.0))
            .w_full()
            .flex()
            .items_center()
            .gap(px(6.0))
            .pr(px(10.0))
            .border_l_2()
            .border_color(gpui::transparent_black())
            .text_size(px(13.0));
        match row_model {
            Row::Repository(_) => {
                let collapsed = self.collapsed.contains(&repo.path);
                base.pl(px(6.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.glass_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_repository(repo_path.clone(), cx);
                        this.toggle(repo_path.clone(), cx);
                    }))
                    .child(
                        icons::icon(if collapsed {
                            icons::ALT_ARROW_RIGHT
                        } else {
                            icons::ALT_ARROW_DOWN
                        })
                        .size(px(12.0))
                        .text_color(theme.text_muted),
                    )
                    .child(
                        icons::icon(icons::VSC_BRANCH)
                            .size(px(15.0))
                            .text_color(theme.text_muted),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(SharedString::from(repo.name.clone())),
                    )
                    .when(!repo.path.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(px(10.0))
                                .text_color(theme.text_faint)
                                .child("Submodule"),
                        )
                    })
                    .child(Self::count_badge(repo.files.len(), &theme))
                    .into_any_element()
            }
            Row::Group { .. } => {
                let key = format!("{}:{staged}", repo.path);
                let collapsed = self.collapsed.contains(&key);
                let paths: Vec<_> = repo
                    .files
                    .iter()
                    .filter(|f| in_group(f, staged))
                    .map(|f| f.path.clone())
                    .collect();
                let enabled = repo.complete;
                let discard_paths: Vec<_> = repo
                    .files
                    .iter()
                    .filter(|f| in_group(f, staged) && !repo.submodules.contains(&f.path))
                    .map(|f| f.path.clone())
                    .collect();
                let discard_repository = repo_path.clone();
                let selected_repository = repo_path.clone();
                base.pl(px(18.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.glass_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_repository(selected_repository.clone(), cx);
                        this.toggle(key.clone(), cx);
                    }))
                    .child(
                        icons::icon(if collapsed {
                            icons::ALT_ARROW_RIGHT
                        } else {
                            icons::ALT_ARROW_DOWN
                        })
                        .size(px(12.0))
                        .text_color(theme.text_muted),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text_muted)
                            .child(if staged { "Staged Changes" } else { "Changes" }),
                    )
                    .when(!staged, |el| {
                        el.child(
                            self.discard_button(
                                format!("discard-group-{repository}"),
                                enabled,
                                row_group.clone(),
                                false,
                                None,
                                &theme,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    if enabled && !discard_paths.is_empty() {
                                        this.request_discard(
                                            discard_repository.clone(),
                                            discard_paths.clone(),
                                            false,
                                            cx,
                                        );
                                    }
                                },
                            )),
                        )
                    })
                    .child(
                        self.action(
                            format!("stage-group-{repository}-{staged}"),
                            staged,
                            enabled,
                            row_group,
                            false,
                            None,
                            &theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if enabled {
                                this.select_repository(repo_path.clone(), cx);
                                this.set_staged(repo_path.clone(), paths.clone(), !staged, cx);
                            }
                        })),
                    )
                    .into_any_element()
            }
            Row::File { file, .. } => {
                let file = &repo.files[file];
                let submodule = repo.submodules.contains(&file.path);
                let color = status_color(file, staged, &theme);
                let name = file
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&file.path)
                    .to_owned();
                let parent = file
                    .path
                    .rsplit_once('/')
                    .map(|(p, _)| p.to_owned())
                    .unwrap_or_default();
                let Some(target) = &self.target else {
                    return div().into_any_element();
                };
                let selection = CheckoutChangeSelection {
                    cwd: target.cwd.clone(),
                    repository: repo.path.clone(),
                    path: file.path.clone(),
                    staged,
                };
                let key = FileKey::new(&repo.path, &file.path, staged);
                let selected = self.selected_files.contains(&key);
                let clicked_key = key.clone();
                let debug_key = key.clone();
                let paths = self.staging_paths(&key);
                let path = file.path.clone();
                let enabled = repo.complete;
                let open_path = if repo.path.is_empty() {
                    path.clone()
                } else {
                    format!("{}/{path}", repo.path)
                };
                let discard_paths = self.discard_paths(&key);
                let discard_repository = repo_path.clone();
                let full_path = if repo.path.is_empty() {
                    path.clone()
                } else {
                    format!("{}/{path}", repo.path)
                };
                base.pl(px(36.0))
                    .debug_selector(move || {
                        format!(
                            "sc-file-{}-{}-{}",
                            debug_key.repository, debug_key.staged, debug_key.path
                        )
                        .into()
                    })
                    .cursor_pointer()
                    .border_color(if selected {
                        theme.accent
                    } else {
                        gpui::transparent_black()
                    })
                    .bg(if selected {
                        theme.element_active
                    } else {
                        gpui::transparent_black()
                    })
                    .when(!selected, |el| el.hover(|s| s.bg(theme.glass_hover())))
                    .tooltip(crate::settings::widgets::text_tooltip(format!(
                        "{full_path} ({})",
                        if staged { "Index" } else { "Working Tree" },
                    )))
                    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                        this.select_repository(selection.repository.clone(), cx);
                        if this.active_repository != selection.repository {
                            return;
                        }
                        let modifiers = event.modifiers();
                        this.select_files(
                            clicked_key.clone(),
                            modifiers.shift,
                            modifiers.control || modifiers.platform,
                        );
                        this.selected = Some(selection.clone());
                        cx.emit(SourceControlEvent::OpenDiff(selection.clone()));
                        cx.notify();
                    }))
                    .child(if submodule {
                        icons::icon(icons::VSC_BRANCH)
                            .size(px(15.0))
                            .text_color(theme.accent)
                            .into_any_element()
                    } else {
                        file_icons::icon(FileIconIdentity::file(&file.path), theme.appearance)
                            .size(px(15.0))
                            .into_any_element()
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex_shrink(1.0)
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.text)
                                    .child(SharedString::from(name)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.text_faint)
                                    .text_size(px(11.0))
                                    .child(SharedString::from(parent)),
                            ),
                    )
                    .when(!submodule && file.worktree != GitFileState::Deleted, |el| {
                        el.child(
                            self.open_file_button(
                                format!("open-file-{row}"),
                                enabled,
                                row_group.clone(),
                                selected,
                                &theme,
                            )
                            .on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(SourceControlEvent::OpenFile(open_path.clone()));
                                },
                            )),
                        )
                    })
                    .when(
                        !staged && !submodule && file.index != GitFileState::Unmerged,
                        |el| {
                            el.child(
                                self.discard_button(
                                    format!("discard-file-{row}"),
                                    enabled && !discard_paths.is_empty(),
                                    row_group.clone(),
                                    selected,
                                    Some(discard_paths.len()),
                                    &theme,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        if enabled {
                                            this.request_discard(
                                                discard_repository.clone(),
                                                discard_paths.clone(),
                                                false,
                                                cx,
                                            );
                                        }
                                    },
                                )),
                            )
                        },
                    )
                    .child(
                        self.action(
                            format!("stage-file-{row}"),
                            staged,
                            enabled,
                            row_group,
                            selected,
                            Some(paths.len()),
                            &theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if enabled {
                                this.select_repository(repo_path.clone(), cx);
                                this.set_staged(repo_path.clone(), paths.clone(), !staged, cx);
                            }
                        })),
                    )
                    .child(
                        div()
                            .w(px(16.0))
                            .flex_none()
                            .flex()
                            .justify_center()
                            .text_size(px(10.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(color)
                            .child(status_letter(file, staged, submodule)),
                    )
                    .into_any_element()
            }
        }
    }
}

impl Render for SourceControl {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::transcript::record_view_frame("source-control");
        self.ensure_loaded(cx);
        self.ensure_git_details(cx);
        let theme = Theme::of(cx).clone();
        let empty = self.snapshot.as_ref().is_some_and(|s| {
            s.repositories
                .iter()
                .all(|r| r.complete && r.files.is_empty())
        });
        let commit = self.render_commit(window, cx);
        let git_toolbar = self.render_git_toolbar(cx);
        let confirmation = self.render_confirmation(window, cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .overflow_hidden()
            .bg(theme.panel_bg())
            .child(git_toolbar)
            .child(commit)
            .when_some(
                self.error.clone().or(self.load_error.clone()),
                |el, error| {
                    el.child(
                        div()
                            .px(px(12.0))
                            .py(px(8.0))
                            .text_size(px(12.0))
                            .text_color(theme.danger)
                            .child(error),
                    )
                },
            )
            .when_some(self.notice.clone(), |el, notice| {
                el.child(
                    div()
                        .px(px(12.0))
                        .py(px(8.0))
                        .text_size(px(11.0))
                        .text_color(theme.success)
                        .child(notice),
                )
            })
            .when(self.snapshot.is_none(), |el| {
                el.child(
                    div()
                        .p(px(16.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_faint)
                        .child(if self.target.is_some() {
                            "Loading changes…"
                        } else {
                            "Select a conversation to view changes"
                        }),
                )
            })
            .when(
                self.snapshot.is_some(),
                |el| {
                    el.child(
                        uniform_list(
                            "source-control-files",
                            self.rows.len(),
                            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                range
                                    .map(|i| this.render_row(i, cx))
                                    .collect::<Vec<AnyElement>>()
                            }),
                        )
                        .flex_1()
                        .min_h_0()
                        .pt(px(8.0))
                        .w_full(),
                    )
                },
            )
            .when(empty, |el| {
                el.child(
                    div()
                        .p(px(12.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_faint)
                        .child("No changes"),
                )
            })
            .child(self.render_git_status(cx))
            .child(confirmation)
    }
}

#[cfg(feature = "source-control-fixture")]
impl SourceControl {
    pub fn fixture_commit_state(&self, cx: &App) -> serde_json::Value {
        serde_json::json!({
            "repository": self.active_repository,
            "message": self.commit_input.read(cx).text(),
            "canCommit": self.can_commit(cx),
            "committing": self.pending_commit.is_some(),
            "error": self.error,
            "notice": self.notice,
            "stagedCount": self.staged_count(),
            "git": self.git_state(), "details": self.git_details,
            "gitPanel": self.git_panel.as_ref().map(|p| format!("{p:?}")),
            "confirmation": self.confirmation.is_some(), "canUndoDiscard": self.undo_discard.is_some(),
            "busy": self.is_busy(), "amend": self.amend,
            "refreshing": self.refreshing, "refreshFeedback": self.refresh_feedback,
            "selectedFiles": self.selected_files.iter().map(|key| serde_json::json!({ "repository": key.repository, "path": key.path, "staged": key.staged })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext, VisualTestContext};

    struct FileHost(Entity<SourceControl>);
    impl Render for FileHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |view, cx| {
                let content = div()
                    .flex()
                    .flex_col()
                    .w(px(360.0))
                    .h(px(600.0))
                    .child(
                        div()
                            .id("sc-test-header")
                            .debug_selector(|| "sc-test-header".into())
                            .h(px(24.0))
                            .flex_none(),
                    )
                    .children(
                        (0..view.rows.len())
                            .map(|row| view.render_row(row, cx))
                            .collect::<Vec<_>>(),
                    );
                view.selection_surface(content, cx)
            })
        }
    }

    fn selection_setup(cx: &mut TestAppContext) -> (Entity<SourceControl>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::composer::init(cx, Default::default());
        });
        let (host, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| AppState::new());
            FileHost(cx.new(|cx| {
                let mut view = SourceControl::new(state, cx);
                view.target = Some(Target {
                    chat: "chat".into(),
                    cwd: "/checkout".into(),
                    device: None,
                });
                view.snapshot = Some(CheckoutChanges {
                    repositories: ["", "nested"]
                        .into_iter()
                        .map(|path| zeron_proto::RepositoryChanges {
                            path: path.into(),
                            name: path.into(),
                            complete: true,
                            git: None,
                            submodules: Vec::new(),
                            files: ["a.rs", "b.rs", "c.rs"]
                                .into_iter()
                                .map(|name| GitFileStatus {
                                    path: name.into(),
                                    old_path: None,
                                    index: if name == "a.rs" {
                                        GitFileState::Modified
                                    } else {
                                        GitFileState::Untracked
                                    },
                                    worktree: GitFileState::Modified,
                                })
                                .collect(),
                        })
                        .collect(),
                });
                view.rebuild();
                view
            }))
        });
        let view = host.read_with(cx, |host, _| host.0.clone());
        cx.simulate_resize(gpui::size(px(640.0), px(600.0)));
        cx.update(|window, cx| window.draw(cx).clear());
        (view, cx)
    }

    #[gpui::test]
    fn outside_click_clears_the_selection_and_anchor_but_inside_controls_keep_them(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = selection_setup(cx);
        let first = cx.debug_bounds("sc-file--false-a.rs").unwrap();
        let last = cx.debug_bounds("sc-file--false-c.rs").unwrap();
        cx.simulate_click(first.center(), gpui::Modifiers::default());
        cx.simulate_click(
            last.center(),
            gpui::Modifiers {
                shift: true,
                ..Default::default()
            },
        );
        let header = cx.debug_bounds("sc-test-header").unwrap();
        cx.simulate_click(header.center(), gpui::Modifiers::default());
        view.read_with(cx, |view, _| assert_eq!(view.selected_files.len(), 3));
        cx.simulate_click(
            gpui::point(px(500.0), px(200.0)),
            gpui::Modifiers::default(),
        );
        view.read_with(cx, |view, _| {
            assert!(view.selected_files.is_empty());
            assert!(view.selection_anchor.is_none());
            assert!(view.selected.is_none());
        });
        cx.simulate_click(
            last.center(),
            gpui::Modifiers {
                shift: true,
                ..Default::default()
            },
        );
        view.read_with(cx, |view, _| assert_eq!(view.selected_files.len(), 1));
    }

    #[gpui::test]
    fn floating_git_menu_click_keeps_selection_until_a_click_outside_the_menu(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = selection_setup(cx);
        view.update(cx, |view, _| {
            view.select_files(FileKey::new("", "a.rs", false), false, false);
            view.actions_menu.open(());
            view.actions_menu_bounds.set(Some(gpui::Bounds::new(
                gpui::point(px(400.0), px(100.0)),
                gpui::size(px(200.0), px(300.0)),
            )));
        });
        cx.simulate_click(
            gpui::point(px(500.0), px(200.0)),
            gpui::Modifiers::default(),
        );
        view.read_with(cx, |view, _| assert_eq!(view.selected_files.len(), 1));
        cx.simulate_click(
            gpui::point(px(620.0), px(200.0)),
            gpui::Modifiers::default(),
        );
        view.read_with(cx, |view, _| assert!(view.selected_files.is_empty()));
    }

    #[gpui::test]
    fn shift_click_selects_a_range_and_control_click_toggles_a_file(cx: &mut TestAppContext) {
        let (view, cx) = selection_setup(cx);
        let click = |cx: &mut VisualTestContext, path: &str, modifiers| {
            let selector = match path {
                "a.rs" => "sc-file--false-a.rs",
                "b.rs" => "sc-file--false-b.rs",
                "c.rs" => "sc-file--false-c.rs",
                _ => unreachable!(),
            };
            let bounds = cx.debug_bounds(selector).unwrap();
            cx.simulate_click(bounds.center(), modifiers);
        };
        click(cx, "a.rs", gpui::Modifiers::default());
        click(
            cx,
            "c.rs",
            gpui::Modifiers {
                shift: true,
                ..Default::default()
            },
        );
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.staging_paths(&FileKey::new("", "c.rs", false)),
                ["a.rs", "b.rs", "c.rs"]
            );
            assert!(
                !view
                    .selected_files
                    .contains(&FileKey::new("", "a.rs", true))
            );
        });
        click(
            cx,
            "b.rs",
            gpui::Modifiers {
                control: true,
                ..Default::default()
            },
        );
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.staging_paths(&FileKey::new("", "a.rs", false)),
                ["a.rs", "c.rs"]
            );
            assert_eq!(
                view.staging_paths(&FileKey::new("", "b.rs", false)),
                ["b.rs"]
            );
        });
        click(cx, "b.rs", gpui::Modifiers::default());
        view.read_with(cx, |view, _| assert_eq!(view.selected_files.len(), 1));
    }

    #[gpui::test]
    fn selection_never_batches_different_repositories_or_index_sides(cx: &mut TestAppContext) {
        let (view, cx) = selection_setup(cx);
        view.update(cx, |view, _| {
            view.select_files(FileKey::new("", "a.rs", false), false, false);
            view.select_files(FileKey::new("", "c.rs", false), true, false);
            view.select_files(FileKey::new("", "a.rs", true), true, false);
            assert_eq!(
                view.staging_paths(&FileKey::new("", "a.rs", true)),
                ["a.rs"]
            );
            view.select_files(FileKey::new("nested", "a.rs", false), true, false);
            assert_eq!(
                view.staging_paths(&FileKey::new("nested", "a.rs", false)),
                ["a.rs"]
            );
            assert_eq!(view.selected_files.len(), 1);
        });
    }

    #[gpui::test]
    fn batch_selection_follows_staging_and_prunes_only_files_that_disappear(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = selection_setup(cx);
        view.update(cx, |view, cx| {
            view.select_files(FileKey::new("", "a.rs", false), false, false);
            view.select_files(FileKey::new("", "c.rs", false), true, false);
            let selected = view.selected_files.clone();
            let anchor = view.selection_anchor.clone();
            let paths = view.staging_paths(&FileKey::new("", "a.rs", false));
            let mut snapshot = view.snapshot.clone().unwrap();
            for file in &mut snapshot.repositories[0].files {
                file.index = GitFileState::Added;
                file.worktree = if file.path == "b.rs" {
                    GitFileState::Modified
                } else {
                    GitFileState::Unchanged
                };
            }
            view.apply_snapshot(snapshot, cx);
            view.follow_staging("", &paths, true, selected, anchor);
            assert_eq!(
                view.staging_paths(&FileKey::new("", "c.rs", true)),
                ["a.rs", "b.rs", "c.rs"]
            );
            assert!(
                !view
                    .selected_files
                    .contains(&FileKey::new("", "b.rs", false))
            );
            let mut snapshot = view.snapshot.clone().unwrap();
            snapshot.repositories[0]
                .files
                .retain(|file| file.path != "b.rs");
            snapshot.repositories[0].files.reverse();
            view.apply_snapshot(snapshot, cx);
            assert_eq!(
                view.staging_paths(&FileKey::new("", "c.rs", true)),
                ["a.rs", "c.rs"]
            );
            view.select_repository("nested".into(), cx);
            assert!(view.selected_files.is_empty());
            assert!(view.selection_anchor.is_none());
        });
    }

    #[gpui::test]
    fn discard_uses_the_selected_files_and_keeps_parent_submodules(cx: &mut TestAppContext) {
        let (view, cx) = selection_setup(cx);
        view.update(cx, |view, _| {
            view.snapshot.as_mut().unwrap().repositories[0]
                .submodules
                .push("a.rs".into());
            view.select_files(FileKey::new("", "a.rs", false), false, false);
            view.select_files(FileKey::new("", "c.rs", false), true, false);
            assert_eq!(
                view.discard_paths(&FileKey::new("", "b.rs", false)),
                ["b.rs", "c.rs"]
            );
            assert_eq!(
                view.staging_paths(&FileKey::new("", "b.rs", false)),
                ["a.rs", "b.rs", "c.rs"]
            );
            assert_eq!(
                view.discard_paths(&FileKey::new("nested", "b.rs", false)),
                ["b.rs"]
            );
            view.select_files(FileKey::new("", "b.rs", false), false, false);
            assert_eq!(
                view.discard_paths(&FileKey::new("", "c.rs", false)),
                ["c.rs"]
            );
        });
    }

    struct CommitHost(Entity<SourceControl>);
    impl Render for CommitHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |view, cx| view.render_commit(window, cx))
        }
    }

    fn commit_setup(cx: &mut TestAppContext) -> (Entity<SourceControl>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::composer::init(cx, Default::default());
        });
        let (host, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| AppState::new());
            let view = cx.new(|cx| {
                let mut view = SourceControl::new(state, cx);
                view.target = Some(Target {
                    chat: "chat".into(),
                    cwd: "/checkout".into(),
                    device: None,
                });
                view.snapshot = Some(CheckoutChanges {
                    repositories: ["", "apps/device-apps"]
                        .into_iter()
                        .map(|path| zeron_proto::RepositoryChanges {
                            path: path.into(),
                            name: if path.is_empty() {
                                "checkout".into()
                            } else {
                                path.into()
                            },
                            complete: true,
                            git: None,
                            submodules: Vec::new(),
                            files: vec![GitFileStatus {
                                path: "file.rs".into(),
                                old_path: None,
                                index: GitFileState::Modified,
                                worktree: GitFileState::Unchanged,
                            }],
                        })
                        .collect(),
                });
                view
            });
            CommitHost(view)
        });
        let view = host.read_with(cx, |host, _| host.0.clone());
        cx.update(|window, cx| window.draw(cx).clear());
        (view, cx)
    }

    #[gpui::test]
    fn a_stale_manual_refresh_cannot_replace_a_newer_staging_snapshot(cx: &mut TestAppContext) {
        let (view, cx) = commit_setup(cx);
        view.update(cx, |view, cx| {
            let target = view.target.clone().unwrap();
            let mut stale = view.snapshot.clone().unwrap();
            stale.repositories[0].files.clear();
            view.refreshing = true;
            view.mutation_epoch = 2;
            assert!(!view.complete_refresh(
                &target,
                1,
                "",
                Ok(stale),
                Err(zeron_rpc::RpcError::Failed("stale details".into())),
                cx,
            ));
            assert_eq!(view.staged_count(), 1);
            assert!(!view.refreshing);
            assert!(view.refresh_feedback.is_none());
            assert!(view.load_error.is_none());
        });
    }

    #[gpui::test]
    fn a_failed_manual_refresh_keeps_files_and_the_commit_draft(cx: &mut TestAppContext) {
        let (view, cx) = commit_setup(cx);
        view.update(cx, |view, cx| {
            let target = view.target.clone().unwrap();
            view.commit_input
                .update(cx, |input, cx| input.set_text("Keep my message", cx));
            view.refreshing = true;
            assert!(view.complete_refresh(
                &target,
                view.mutation_epoch,
                "",
                Err(zeron_rpc::RpcError::Failed("offline".into())),
                Err(zeron_rpc::RpcError::Failed("offline".into())),
                cx,
            ));
            assert_eq!(view.staged_count(), 1);
            assert_eq!(view.commit_input.read(cx).text(), "Keep my message");
            assert!(!view.refreshing);
            assert_eq!(view.refresh_feedback, Some(false));
            assert!(view.load_error.as_ref().unwrap().contains("offline"));
        });
    }

    #[gpui::test]
    fn source_control_without_a_checkout_settles_without_repeated_input_events(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::composer::init(cx, Default::default());
        });
        let (view, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| AppState::new());
            SourceControl::new(state, cx)
        });
        cx.update(|window, cx| window.draw(cx).clear());
        cx.run_until_parked();
        view.read_with(cx, |view, cx| {
            assert!(view.target.is_none());
            assert!(view.commit_input.read(cx).text().is_empty());
        });
    }

    #[gpui::test]
    fn commit_messages_follow_the_selected_repository_and_checkout(cx: &mut TestAppContext) {
        let (view, cx) = commit_setup(cx);
        view.update(cx, |view, cx| {
            view.commit_input
                .update(cx, |input, cx| input.set_text("Root draft", cx));
            view.select_repository("apps/device-apps".into(), cx);
            assert!(view.commit_input.read(cx).text().is_empty());
            view.commit_input
                .update(cx, |input, cx| input.set_text("Module draft", cx));
            view.select_repository(String::new(), cx);
            assert_eq!(view.commit_input.read(cx).text(), "Root draft");
            view.select_repository("apps/device-apps".into(), cx);
            assert_eq!(view.commit_input.read(cx).text(), "Module draft");
            view.remember_draft(cx);
            view.target.as_mut().unwrap().cwd = "/another-checkout".into();
            view.active_repository.clear();
            view.restore_draft(cx);
            assert!(view.commit_input.read(cx).text().is_empty());
        });
    }

    #[gpui::test]
    fn enter_adds_a_newline_and_ctrl_enter_submits_without_losing_a_failed_message(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = commit_setup(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.commit_input
                    .update(cx, |input, cx| input.set_text("Subject", cx));
                window.focus(
                    &gpui::Focusable::focus_handle(view.commit_input.read(cx), cx),
                    cx,
                );
            });
            window.draw(cx).clear();
        });
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, cx| {
            assert_eq!(view.commit_input.read(cx).text(), "Subject\n")
        });
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-enter"
        } else {
            "ctrl-enter"
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.error.as_deref(), Some("Repository is unavailable"));
            assert_eq!(view.commit_input.read(cx).text(), "Subject\n");
        });
    }

    #[gpui::test]
    fn committing_requires_staged_changes_a_message_and_an_idle_repository(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = commit_setup(cx);
        view.update(cx, |view, cx| {
            assert!(!view.can_commit(cx));
            view.commit_input
                .update(cx, |input, cx| input.set_text("  \n", cx));
            assert!(!view.can_commit(cx));
            view.commit_input
                .update(cx, |input, cx| input.set_text("Commit staged changes", cx));
            assert!(view.can_commit(cx));
            view.load_error = Some("Unable to refresh changes".into());
            assert!(!view.can_commit(cx));
            view.load_error = None;
            view.pending_commit = Some((view.target.clone().unwrap(), String::new()));
            assert!(!view.can_commit(cx));
            view.pending_commit = None;
            view.snapshot.as_mut().unwrap().repositories[0].complete = false;
            assert!(!view.can_commit(cx));
            let repo = &mut view.snapshot.as_mut().unwrap().repositories[0];
            repo.complete = true;
            repo.files[0].index = GitFileState::Unchanged;
            repo.files[0].worktree = GitFileState::Modified;
            assert!(!view.can_commit(cx));
        });
    }

    #[test]
    fn partially_staged_files_appear_on_both_sides() {
        let file = GitFileStatus {
            path: "a.rs".into(),
            old_path: None,
            index: GitFileState::Modified,
            worktree: GitFileState::Modified,
        };
        assert!(in_group(&file, true));
        assert!(in_group(&file, false));
        let untracked = GitFileStatus {
            index: GitFileState::Untracked,
            worktree: GitFileState::Untracked,
            ..file.clone()
        };
        assert!(!in_group(&untracked, true));
        assert!(in_group(&untracked, false));
        let staged = GitFileStatus {
            index: GitFileState::Added,
            worktree: GitFileState::Unchanged,
            ..file
        };
        assert!(in_group(&staged, true));
        assert!(!in_group(&staged, false));
    }

    #[test]
    fn conflicts_are_only_in_the_working_group() {
        for (index, worktree) in [
            (GitFileState::Unmerged, GitFileState::Unmerged),
            (GitFileState::Added, GitFileState::Added),
            (GitFileState::Deleted, GitFileState::Deleted),
        ] {
            let file = GitFileStatus {
                path: "conflict".into(),
                old_path: None,
                index,
                worktree,
            };
            assert!(!in_group(&file, true));
            assert!(in_group(&file, false));
        }
    }
}
