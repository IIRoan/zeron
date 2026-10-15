//! Git command selection and follow-up prompts share a searchable quick-pick surface.
use super::*;
use branch::BranchCommand;
use gpui::{FocusHandle, UniformListScrollHandle};
use stash::StashCommand;
use zeron_proto::{GitHistoryCommit, RepositoryGitAction as Action, RepositoryGitState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GitCommand {
    Commands,
    Fetch,
    Pull,
    PullMerge,
    PullRebase,
    Push,
    Sync,
    Checkout,
    PullFrom,
    CreateBranch,
    RenameBranch,
    Merge,
    Rebase,
    DeleteBranch,
    Stash,
    StashUntracked,
    ApplyLatestStash,
    ApplyStash,
    PopLatestStash,
    PopStash,
    DropStash,
    RestoreStash,
    AddRemote,
    RemoveRemote,
    Publish,
    Incoming,
    Outgoing,
    Recent,
    CherryPick,
    Revert,
    Amend,
    UndoCommit,
    SortPath,
    SortName,
    SortStatus,
    StageAll,
    UnstageAll,
    DiscardAll,
    DiscardEverything,
    UndoDiscard,
    Continue,
    Abort,
    Refresh,
    Commit,
    CommitPush,
    CommitSync,
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    fn open_palette(
        cx: &mut TestAppContext,
        panel: GitPanel,
    ) -> (Entity<SourceControl>, &mut VisualTestContext) {
        let (view, cx) = super::super::git::tests::menu_setup(cx, 900.0);
        view.update(cx, |view, cx| view.open_git_panel(panel, cx));
        cx.update(|window, cx| {
            window.draw(cx).clear();
            view.update(cx, |view, cx| {
                // Test frames do not deliver the platform's deferred focus callback.
                window.focus(
                    &gpui::Focusable::focus_handle(
                        view.git_palette.as_ref().unwrap().search.read(cx),
                        cx,
                    ),
                    cx,
                );
            });
            window.draw(cx).clear();
        });
        (view, cx)
    }

    #[gpui::test]
    fn removed_menu_actions_remain_searchable_and_escape_restores_focus(cx: &mut TestAppContext) {
        let (view, cx) = open_palette(cx, GitPanel::Commands);
        assert!(cx.debug_bounds("git-command-palette").is_some());
        for (query, command) in [
            ("amend", GitCommand::Amend),
            ("pull merge", GitCommand::PullMerge),
            ("pull rebase", GitCommand::PullRebase),
            ("sort status", GitCommand::SortStatus),
            ("recent commits", GitCommand::Recent),
        ] {
            view.update(cx, |view, cx| {
                view.git_palette
                    .as_ref()
                    .unwrap()
                    .search
                    .update(cx, |input, cx| input.set_text(query, cx));
                assert!(
                    view.git_choices(cx).iter().any(
                        |choice| matches!(choice, Choice::Command(found) if *found == command)
                    )
                );
                assert!(!view.actions_menu.is_open());
            });
        }
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(view.git_palette.is_none()));
    }

    #[gpui::test]
    fn stash_commands_support_search_arrow_navigation_and_enter(cx: &mut TestAppContext) {
        let (view, cx) = open_palette(cx, GitPanel::Stashes);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (outbound, _requests) = tokio::sync::mpsc::channel(8);
        let (_replies, inbound) = tokio::sync::mpsc::channel(8);
        let engine = {
            let _guard = runtime.enter();
            crate::state::EngineHandle::from_test_client(zeron_rpc::RpcClient::new(
                outbound, inbound,
            ))
        };
        view.update(cx, |view, cx| {
            view.state
                .update(cx, |state, _| state.set_test_engine(engine))
        });
        cx.simulate_input("pop");
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.git_choices(cx)
                    .iter()
                    .map(Choice::label)
                    .collect::<Vec<_>>(),
                ["Pop Latest Stash", "Pop Stash…"]
            )
        });
        cx.simulate_keystrokes("down enter");
        view.read_with(cx, |view, _| {
            assert!(view.git_palette.is_none());
            assert!(view.stash_picker.is_some());
            assert!(!view.actions_menu.is_open());
        });
    }

    #[gpui::test]
    fn remote_prompt_filters_choices_and_confirms_removal(cx: &mut TestAppContext) {
        let (view, cx) = open_palette(cx, GitPanel::RemoveRemote);
        view.update(cx, |view, cx| {
            view.git_details = Some(zeron_proto::RepositoryGitDetails {
                state: view.git_state().cloned().unwrap(),
                remotes: ["origin", "company"]
                    .into_iter()
                    .map(|name| zeron_proto::RepositoryGitRemote {
                        name: name.into(),
                        url: format!("https://example.invalid/{name}.git"),
                        push_url: None,
                    })
                    .collect(),
                ..Default::default()
            });
            cx.notify();
        });
        cx.simulate_input("company");
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, _| {
            assert!(view.git_palette.is_none());
            assert!(matches!(&view.confirmation.as_ref().unwrap().command, git::Command::Git(Action::RemoveRemote { name }) if name == "company"));
        });
    }

    #[gpui::test]
    fn changed_branch_rejects_palette_selection_and_form_submission(cx: &mut TestAppContext) {
        let (view, cx) = open_palette(cx, GitPanel::Commands);
        view.update(cx, |view, cx| {
            view.git_palette
                .as_ref()
                .unwrap()
                .search
                .update(cx, |input, cx| input.set_text("undo last commit", cx));
            view.snapshot.as_mut().unwrap().repositories[0]
                .git
                .as_mut()
                .unwrap()
                .branch = Some("other".into());
        });
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, _| {
            assert!(view.confirmation.is_none());
            assert!(view.error.as_deref().unwrap().contains("branch changed"));
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.start_form(GitForm::CreateBranch, window, cx);
                view.form_input
                    .update(cx, |input, cx| input.set_text("feature/new", cx));
                view.snapshot.as_mut().unwrap().repositories[0]
                    .git
                    .as_mut()
                    .unwrap()
                    .head = Some("new-head".into());
                view.submit_form(cx);
                assert!(view.error.as_deref().unwrap().contains("branch changed"));
                assert_eq!(view.form_input.read(cx).text(), "feature/new");
            })
        });
    }

    #[gpui::test]
    fn add_remote_form_mounts_in_palette_and_tab_moves_between_fields(cx: &mut TestAppContext) {
        let (view, cx) = open_palette(cx, GitPanel::Remotes);
        cx.simulate_keystrokes("enter");
        cx.update(|window, cx| {
            window.draw(cx).clear();
            view.update(cx, |view, cx| {
                window.focus(
                    &gpui::Focusable::focus_handle(view.form_input.read(cx), cx),
                    cx,
                )
            });
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.git_form, Some(GitForm::AddRemote));
            assert!(!view.actions_menu.is_open());
        });
        cx.simulate_keystrokes("tab");
        cx.simulate_input("https://example.invalid/project.git");
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.form_second.read(cx).text(),
                "https://example.invalid/project.git"
            )
        });
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| {
            assert!(view.git_palette.is_none());
            assert!(view.git_form.is_none());
        });
    }
}

pub(crate) const GIT_COMMANDS: &[GitCommand] = &[
    GitCommand::Fetch,
    GitCommand::Pull,
    GitCommand::PullFrom,
    GitCommand::Push,
    GitCommand::Sync,
    GitCommand::Checkout,
    GitCommand::CreateBranch,
    GitCommand::RenameBranch,
    GitCommand::Merge,
    GitCommand::Rebase,
    GitCommand::DeleteBranch,
    GitCommand::Stash,
    GitCommand::StashUntracked,
    GitCommand::ApplyLatestStash,
    GitCommand::ApplyStash,
    GitCommand::PopLatestStash,
    GitCommand::PopStash,
    GitCommand::DropStash,
    GitCommand::RestoreStash,
    GitCommand::AddRemote,
    GitCommand::RemoveRemote,
    GitCommand::Publish,
    GitCommand::Incoming,
    GitCommand::Outgoing,
    GitCommand::Recent,
    GitCommand::CherryPick,
    GitCommand::Revert,
    GitCommand::Amend,
    GitCommand::UndoCommit,
    GitCommand::PullMerge,
    GitCommand::PullRebase,
    GitCommand::SortPath,
    GitCommand::SortName,
    GitCommand::SortStatus,
    GitCommand::StageAll,
    GitCommand::UnstageAll,
    GitCommand::DiscardAll,
    GitCommand::DiscardEverything,
    GitCommand::UndoDiscard,
    GitCommand::Continue,
    GitCommand::Abort,
    GitCommand::Refresh,
    GitCommand::Commit,
    GitCommand::CommitPush,
    GitCommand::CommitSync,
];

impl GitCommand {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Commands => "Git: All Commands…",
            Self::Fetch => "Git: Fetch",
            Self::Pull => "Git: Pull",
            Self::PullMerge => "Git: Pull (Merge)",
            Self::PullRebase => "Git: Pull (Rebase)",
            Self::Push => "Git: Push",
            Self::Sync => "Git: Sync",
            Self::Checkout => "Git: Checkout to…",
            Self::PullFrom => "Git: Pull from…",
            Self::CreateBranch => "Git: Create Branch…",
            Self::RenameBranch => "Git: Rename Current Branch…",
            Self::Merge => "Git: Merge Branch…",
            Self::Rebase => "Git: Rebase onto…",
            Self::DeleteBranch => "Git: Delete Branch…",
            Self::Stash => "Git: Stash Tracked Changes…",
            Self::StashUntracked => "Git: Stash Including Untracked…",
            Self::ApplyLatestStash => "Git: Apply Latest Stash",
            Self::ApplyStash => "Git: Apply Stash…",
            Self::PopLatestStash => "Git: Pop Latest Stash",
            Self::PopStash => "Git: Pop Stash…",
            Self::DropStash => "Git: Drop Stash…",
            Self::RestoreStash => "Git: Apply Stash & Restore Staging…",
            Self::AddRemote => "Git: Add Remote…",
            Self::RemoveRemote => "Git: Remove Remote…",
            Self::Publish => "Git: Publish Branch…",
            Self::Incoming => "Git: View Incoming Commits…",
            Self::Outgoing => "Git: View Outgoing Commits…",
            Self::Recent => "Git: View Recent Commits…",
            Self::CherryPick => "Git: Cherry-pick Commit…",
            Self::Revert => "Git: Revert Commit…",
            Self::Amend => "Git: Amend Last Commit",
            Self::UndoCommit => "Git: Undo Last Commit…",
            Self::SortPath => "Git: Sort Changes by Path",
            Self::SortName => "Git: Sort Changes by File Name",
            Self::SortStatus => "Git: Sort Changes by Status",
            Self::StageAll => "Git: Stage All Changes",
            Self::UnstageAll => "Git: Unstage All Changes",
            Self::DiscardAll => "Git: Discard All Changes…",
            Self::DiscardEverything => "Git: Discard Staged and Unstaged Changes…",
            Self::UndoDiscard => "Git: Undo Discard",
            Self::Continue => "Git: Continue Git Operation",
            Self::Abort => "Git: Abort Git Operation…",
            Self::Refresh => "Git: Refresh",
            Self::Commit => "Git: Commit",
            Self::CommitPush => "Git: Commit & Push",
            Self::CommitSync => "Git: Commit & Sync",
        }
    }

    fn branch(self) -> Option<BranchCommand> {
        match self {
            Self::Checkout => Some(BranchCommand::Checkout),
            Self::PullFrom => Some(BranchCommand::Pull),
            Self::Merge => Some(BranchCommand::Merge),
            Self::Rebase => Some(BranchCommand::Rebase),
            Self::DeleteBranch => Some(BranchCommand::Delete),
            _ => None,
        }
    }
}

pub(super) struct GitPalette {
    pub(super) search: Entity<ComposerInput>,
    pub(super) previous_focus: Option<FocusHandle>,
    target: Target,
    repository: String,
    state: RepositoryGitState,
    pub(super) active: usize,
    scroll: UniformListScrollHandle,
    pub(super) focus_pending: bool,
    enter_down: bool,
    _events: gpui::Subscription,
}

#[derive(Clone)]
enum Choice {
    Command(GitCommand),
    Remote { name: String, url: String },
    Commit(GitHistoryCommit),
}

impl Choice {
    fn label(&self) -> String {
        match self {
            Self::Command(command) => command.label().trim_start_matches("Git: ").into(),
            Self::Remote { name, .. } => name.clone(),
            Self::Commit(commit) => format!(
                "{}  {}",
                &commit.sha[..7.min(commit.sha.len())],
                commit.subject
            ),
        }
    }
    fn detail(&self) -> String {
        match self {
            Self::Remote { url, .. } => url.clone(),
            _ => String::new(),
        }
    }
}

impl SourceControl {
    pub(crate) fn request_git_command(&mut self, command: GitCommand, cx: &mut Context<Self>) {
        self.pending_git_command = self.desired_target(cx).map(|target| (target, command));
        cx.notify();
    }

    pub(super) fn mount_git_palette(&mut self, cx: &mut Context<Self>) {
        if self.git_palette.is_some() {
            return;
        }
        let (Some(target), Some(state)) = (self.target.clone(), self.git_state().cloned()) else {
            return;
        };
        let search = cx.new(|cx| {
            ComposerInput::with_context("Search Git commands…", "PaletteSearch", cx)
                .with_single_line()
                .with_text_metrics(13.0, 20.0)
        });
        let events = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(palette) = &mut this.git_palette {
                    palette.active = 0;
                    palette.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
                }
                cx.notify();
            }
        });
        self.git_palette = Some(GitPalette {
            search,
            previous_focus: None,
            target,
            repository: self.active_repository.clone(),
            state,
            active: 0,
            scroll: UniformListScrollHandle::new(),
            focus_pending: true,
            enter_down: false,
            _events: events,
        });
    }

    pub(super) fn git_palette_current(&self) -> bool {
        self.git_palette.as_ref().is_some_and(|palette| {
            self.target.as_ref() == Some(&palette.target)
                && self.active_repository == palette.repository
                && self.git_state().is_some_and(|state| {
                    state.head == palette.state.head && state.branch == palette.state.branch
                })
        })
    }

    pub(crate) fn git_picker_open(&self) -> bool {
        self.git_palette.is_some() || self.branch_picker.is_some() || self.stash_picker.is_some()
    }

    pub(super) fn close_git_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(palette) = self.git_palette.take() {
            if let Some(focus) = palette.previous_focus {
                window.focus(&focus, cx);
            }
            self.git_panel = None;
            self.git_form = None;
            cx.notify();
        }
    }

    pub(super) fn execute_git_command(
        &mut self,
        command: GitCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.git_enabled() {
            return;
        }
        if let Some(branch) = command.branch() {
            self.close_git_palette(window, cx);
            self.open_branch_picker(branch, window, cx);
            return;
        }
        let form = match command {
            GitCommand::CreateBranch => Some(GitForm::CreateBranch),
            GitCommand::RenameBranch => Some(GitForm::RenameBranch),
            GitCommand::Stash => Some(GitForm::Stash(false)),
            GitCommand::StashUntracked => Some(GitForm::Stash(true)),
            GitCommand::AddRemote => Some(GitForm::AddRemote),
            _ => None,
        };
        if let Some(form) = form {
            self.start_form(form, window, cx);
            return;
        }
        let panel = match command {
            GitCommand::Commands => Some(GitPanel::Commands),
            GitCommand::RemoveRemote => Some(GitPanel::RemoveRemote),
            GitCommand::Publish => Some(GitPanel::Publish),
            GitCommand::Incoming => Some(GitPanel::Incoming),
            GitCommand::Outgoing => Some(GitPanel::Outgoing),
            GitCommand::Recent => Some(GitPanel::Recent),
            GitCommand::CherryPick => Some(GitPanel::CherryPick),
            GitCommand::Revert => Some(GitPanel::Revert),
            _ => None,
        };
        if let Some(panel) = panel {
            self.open_git_panel(panel, cx);
            return;
        }
        let stash = match command {
            GitCommand::ApplyLatestStash => Some((StashCommand::Apply, true)),
            GitCommand::ApplyStash => Some((StashCommand::Apply, false)),
            GitCommand::PopLatestStash => Some((StashCommand::Pop, true)),
            GitCommand::PopStash => Some((StashCommand::Pop, false)),
            GitCommand::DropStash => Some((StashCommand::Drop, false)),
            GitCommand::RestoreStash => Some((StashCommand::Restore, false)),
            _ => None,
        };
        if let Some((stash, latest)) = stash {
            self.close_git_palette(window, cx);
            self.open_stash_picker(stash, latest, window, cx);
            return;
        }
        self.close_git_palette(window, cx);
        match command {
            GitCommand::Fetch => self.run_git(Action::Fetch, cx), GitCommand::Pull => self.run_git(Action::Pull { rebase: None }, cx),
            GitCommand::PullMerge => self.run_git(Action::Pull { rebase: Some(false) }, cx),
            GitCommand::PullRebase => self.run_git(Action::Pull { rebase: Some(true) }, cx),
            GitCommand::Push => self.run_git(Action::Push, cx), GitCommand::Sync => self.run_git(Action::Sync, cx),
            GitCommand::Continue => self.run_git(Action::Continue, cx),
            GitCommand::Abort => self.confirm_git(Action::Abort, "Abort Git operation?".into(), "Git will restore the repository to the state before this operation.".into(), cx),
            GitCommand::Amend => self.toggle_amend(cx),
            GitCommand::UndoCommit => self.confirm_git(Action::UndoCommit, "Undo last local commit?".into(), "Your files will be kept and its message will return to the commit input. Git keeps a recovery reference.".into(), cx),
            GitCommand::SortPath | GitCommand::SortName | GitCommand::SortStatus => {
                self.sort_order = match command { GitCommand::SortName => interaction::SortOrder::Name, GitCommand::SortStatus => interaction::SortOrder::Status, _ => interaction::SortOrder::Path };
                self.rebuild(); cx.notify();
            }
            GitCommand::StageAll | GitCommand::UnstageAll => {
                let staged = command == GitCommand::UnstageAll;
                let paths = self.snapshot.as_ref().into_iter().flat_map(|s| &s.repositories)
                    .filter(|r| r.path == self.active_repository && r.complete).flat_map(|r| &r.files)
                    .filter(|file| in_group(file, staged)).map(|file| file.path.clone()).collect();
                self.set_staged(self.active_repository.clone(), paths, !staged, cx);
            }
            GitCommand::DiscardAll => self.discard_all(false, cx), GitCommand::DiscardEverything => self.discard_all(true, cx),
            GitCommand::UndoDiscard => self.undo_discard(cx), GitCommand::Refresh => self.refresh(cx),
            GitCommand::Commit => self.commit_with_followup(None, cx),
            GitCommand::CommitPush => self.commit_with_followup(Some(Action::Push), cx),
            GitCommand::CommitSync => self.commit_with_followup(Some(Action::Sync), cx),
            _ => unreachable!("follow-up commands handled above"),
        }
    }

    fn git_choices(&self, cx: &App) -> Vec<Choice> {
        if self.git_form.is_some() {
            return Vec::new();
        }
        let Some(palette) = &self.git_palette else {
            return Vec::new();
        };
        let panel = self.git_panel.unwrap_or(GitPanel::Commands);
        let commands: &[GitCommand] = match panel {
            GitPanel::Branches => &[
                GitCommand::CreateBranch,
                GitCommand::RenameBranch,
                GitCommand::Checkout,
                GitCommand::PullFrom,
                GitCommand::Merge,
                GitCommand::Rebase,
                GitCommand::DeleteBranch,
            ],
            GitPanel::Stashes => &[
                GitCommand::Stash,
                GitCommand::StashUntracked,
                GitCommand::ApplyLatestStash,
                GitCommand::ApplyStash,
                GitCommand::PopLatestStash,
                GitCommand::PopStash,
                GitCommand::DropStash,
                GitCommand::RestoreStash,
            ],
            GitPanel::Remotes => &[
                GitCommand::AddRemote,
                GitCommand::Publish,
                GitCommand::RemoveRemote,
            ],
            GitPanel::Commands => GIT_COMMANDS,
            _ => &[],
        };
        let mut choices: Vec<_> = commands.iter().copied().map(Choice::Command).collect();
        if let Some(details) = self.git_details.as_ref().filter(|details| {
            details.state.head == palette.state.head && details.state.branch == palette.state.branch
        }) {
            match panel {
                GitPanel::Publish | GitPanel::RemoveRemote | GitPanel::CommitPublish => {
                    choices.extend(
                        details
                            .remotes
                            .iter()
                            .filter(|r| {
                                panel == GitPanel::RemoveRemote
                                    || r.push_url.as_deref() != Some("DISABLED")
                            })
                            .map(|r| Choice::Remote {
                                name: r.name.clone(),
                                url: r.url.clone(),
                            }),
                    );
                    if choices.is_empty() && panel != GitPanel::RemoveRemote {
                        choices.push(Choice::Command(GitCommand::AddRemote));
                    }
                }
                GitPanel::Incoming | GitPanel::CherryPick => {
                    choices.extend(details.incoming.iter().cloned().map(Choice::Commit))
                }
                GitPanel::Outgoing => {
                    choices.extend(details.outgoing.iter().cloned().map(Choice::Commit))
                }
                GitPanel::Recent | GitPanel::Revert => {
                    choices.extend(details.recent.iter().cloned().map(Choice::Commit))
                }
                _ => {}
            }
        }
        let query = palette.search.read(cx).text();
        let mut matches: Vec<_> = choices
            .into_iter()
            .filter_map(|choice| {
                branch::match_score(
                    &format!("Git: {} {}", choice.label(), choice.detail()),
                    query,
                )
                .map(|score| (score, choice))
            })
            .collect();
        matches.sort_by_key(|(score, _)| *score);
        matches.into_iter().map(|(_, choice)| choice).collect()
    }

    fn activate_git_choice(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        if !self.git_enabled() {
            return;
        }
        if !self.git_palette_current() {
            self.close_git_palette(window, cx);
            self.notify_error(
                &self.active_repository.clone(),
                "The branch changed; reopen Git commands",
                cx,
            );
            return;
        }
        match choice {
            Choice::Command(command) => self.execute_git_command(command, window, cx),
            Choice::Remote { name, .. } => {
                let panel = self.git_panel;
                self.close_git_palette(window, cx);
                match panel {
                    Some(GitPanel::RemoveRemote) => self.confirm_git(Action::RemoveRemote { name: name.clone() }, format!("Remove remote {name}?"), "This removes its local configuration and tracking references. The remote repository is kept.".into(), cx),
                    Some(GitPanel::CommitPublish) => self.commit_with_followup(Some(Action::Publish { remote: name }), cx),
                    _ => self.run_git(Action::Publish { remote: name }, cx),
                }
            }
            Choice::Commit(commit) => {
                let panel = self.git_panel;
                self.close_git_palette(window, cx);
                match panel {
                    Some(GitPanel::Revert) => self.confirm_git(Action::RevertCommit { sha: commit.sha }, "Revert this commit?".into(), "Git will create a new commit that reverses this commit's changes. Existing history is kept.".into(), cx),
                    Some(GitPanel::CherryPick) => self.run_git(Action::CherryPick { sha: commit.sha }, cx),
                    _ => { if let Some(target) = &self.target { cx.emit(SourceControlEvent::OpenCommit { cwd: target.cwd.clone(), repository: self.active_repository.clone(), commit }); } }
                }
            }
        }
    }

    fn git_palette_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.git_form.is_some() {
            if event.keystroke.key == "escape" {
                self.close_git_palette(window, cx);
                cx.stop_propagation();
            } else if event.keystroke.key == "tab" && self.git_form == Some(GitForm::AddRemote) {
                let first = gpui::Focusable::focus_handle(self.form_input.read(cx), cx);
                let second = gpui::Focusable::focus_handle(self.form_second.read(cx), cx);
                window.focus(
                    if first.is_focused(window) {
                        &second
                    } else {
                        &first
                    },
                    cx,
                );
                cx.stop_propagation();
            }
            return;
        }
        let choices = self.git_choices(cx);
        let Some(palette) = &mut self.git_palette else {
            return;
        };
        match event.keystroke.key.as_str() {
            "escape" => self.close_git_palette(window, cx),
            "up" | "down" | "tab" => {
                if !choices.is_empty() {
                    let forward = event.keystroke.key == "down"
                        || (event.keystroke.key == "tab" && !event.keystroke.modifiers.shift);
                    let count = choices.len();
                    palette.active = if forward {
                        (palette.active + 1) % count
                    } else {
                        (palette.active.min(count - 1) + count - 1) % count
                    };
                    palette
                        .scroll
                        .scroll_to_item(palette.active, gpui::ScrollStrategy::Nearest);
                    cx.notify();
                }
            }
            "enter" => {
                let down = std::mem::replace(&mut palette.enter_down, true);
                if !down
                    && !event.is_held
                    && let Some(choice) = choices
                        .get(palette.active.min(choices.len().saturating_sub(1)))
                        .cloned()
                {
                    self.activate_git_choice(choice, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(super) fn render_git_palette(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let choices = self.git_choices(cx);
        let Some(palette) = &mut self.git_palette else {
            return gpui::Empty.into_any_element();
        };
        if palette.focus_pending {
            palette.focus_pending = false;
            if palette.previous_focus.is_none() {
                palette.previous_focus = window.focused(cx);
            }
            let input = if self.git_form.is_some() {
                self.form_input.clone()
            } else {
                palette.search.clone()
            };
            let owner = cx.weak_entity();
            let identity = palette.search.entity_id();
            window.on_next_frame(move |window, cx| {
                if owner.upgrade().is_some_and(|view| {
                    view.read(cx)
                        .git_palette
                        .as_ref()
                        .is_some_and(|palette| palette.search.entity_id() == identity)
                }) {
                    window.focus(&gpui::Focusable::focus_handle(input.read(cx), cx), cx);
                }
            });
        }
        let theme = Theme::of(cx).for_popup();
        let viewport = window.viewport_size();
        let width = px(560.0).min((viewport.width - px(32.0)).max(px(1.0)));
        let search = palette.search.clone();
        let scroll = palette.scroll.clone();
        palette.active = palette.active.min(choices.len().saturating_sub(1));
        let active = palette.active;
        let panel = self.git_panel.unwrap_or(GitPanel::Commands);
        let title = if let Some(form) = self.git_form {
            match form {
                GitForm::CreateBranch => "Create Branch",
                GitForm::RenameBranch => "Rename Current Branch",
                GitForm::Stash(_) => "Stash Changes",
                GitForm::AddRemote => "Add Remote",
            }
        } else {
            match panel {
                GitPanel::Branches => "Branch",
                GitPanel::Stashes => "Stashes",
                GitPanel::Remotes => "Remotes",
                GitPanel::Publish => "Publish Branch · Select Remote",
                GitPanel::RemoveRemote => "Remove Remote",
                GitPanel::CommitPublish => "Commit & Publish · Select Remote",
                GitPanel::Incoming => "Incoming Commits",
                GitPanel::Outgoing => "Outgoing Commits",
                GitPanel::Recent => "Recent Commits",
                GitPanel::Revert => "Revert Commit",
                GitPanel::CherryPick => "Cherry-pick Commit",
                _ => "Git Commands",
            }
        };
        let context = format!(
            "{} · {}",
            if palette.repository.is_empty() {
                "Repository"
            } else {
                &palette.repository
            },
            palette.state.branch.as_deref().unwrap_or("Detached HEAD")
        );
        let mut card = popover::popover_card(&theme)
            .id("git-command-palette")
            .debug_selector(|| "git-command-palette".into())
            .w(width)
            .rounded(px(8.0))
            .role(gpui::Role::Dialog)
            .aria_label(title)
            .flex()
            .flex_col()
            .overflow_hidden()
            .capture_key_down(cx.listener(Self::git_palette_key))
            .on_key_up(cx.listener(|this, event: &gpui::KeyUpEvent, _, _| {
                if event.keystroke.key == "enter"
                    && let Some(palette) = &mut this.git_palette
                {
                    palette.enter_down = false;
                }
            }))
            .on_mouse_down_out(
                cx.listener(|this, _, window, cx| this.close_git_palette(window, cx)),
            )
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px(px(12.0))
                    .pt(px(10.0))
                    .pb(px(4.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(context),
                    )
                    .when(
                        panel != GitPanel::Commands || self.git_form.is_some(),
                        |el| {
                            el.child(
                                div()
                                    .id("git-command-back")
                                    .cursor_pointer()
                                    .text_size(px(12.0))
                                    .text_color(theme.accent)
                                    .child("← Commands")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.open_git_panel(GitPanel::Commands, cx)
                                    })),
                            )
                        },
                    ),
            );
        if let Some(form) = self.git_form {
            let valid = matches!(form, GitForm::Stash(_))
                || !self.form_input.read(cx).text().trim().is_empty();
            let valid = valid
                && (!matches!(form, GitForm::AddRemote)
                    || !self.form_second.read(cx).text().trim().is_empty());
            card = card.child(
                div()
                    .p(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(popover::search_input_frame(
                        &theme,
                        self.form_input.clone().into_any_element(),
                    ))
                    .when(matches!(form, GitForm::AddRemote), |el| {
                        el.child(popover::search_input_frame(
                            &theme,
                            self.form_second.clone().into_any_element(),
                        ))
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
                            .justify_end()
                            .gap(px(8.0))
                            .child(
                                super::git::button("git-form-cancel", "Cancel", true, &theme)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_git_palette(window, cx)
                                    })),
                            )
                            .child(
                                super::git::button(
                                    "git-form-submit",
                                    "Save",
                                    valid && self.git_enabled(),
                                    &theme,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.submit_form(cx))),
                            ),
                    ),
            );
        } else {
            let height = px((choices.len().clamp(1, 10) * 40) as f32)
                .min((viewport.height - px(190.0)).max(px(1.0)));
            card = card.child(div().p(px(6.0)).child(popover::search_input_frame(
                &theme,
                search.into_any_element(),
            )));
            if choices.is_empty() {
                let empty = if self.git_details.is_none()
                    && !matches!(
                        panel,
                        GitPanel::Commands
                            | GitPanel::Branches
                            | GitPanel::Stashes
                            | GitPanel::Remotes
                    ) {
                    if self.details_task.is_some() {
                        "Loading Git details…"
                    } else {
                        "Git details unavailable. Refresh to try again."
                    }
                } else {
                    "No matching results"
                };
                card = card.child(
                    div()
                        .h(height)
                        .px(px(12.0))
                        .flex()
                        .items_center()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(empty),
                );
            } else {
                let entity = cx.entity();
                card = card.child(
                    uniform_list(
                        "git-command-choices",
                        choices.len(),
                        move |range, window, cx| {
                            entity.update(cx, |_, cx| {
                                let theme = Theme::of(cx).for_popup();
                                range
                                    .map(|i| {
                                        let choice = choices[i].clone();
                                        let label = choice.label();
                                        let detail = choice.detail();
                                        let id = match &choice {
                                            Choice::Command(command) => command
                                                .branch()
                                                .map(|branch| branch.id().to_owned())
                                                .unwrap_or_else(|| {
                                                    format!("git-command-{command:?}")
                                                }),
                                            _ => format!("git-choice-{i}"),
                                        };
                                        div()
                                            .id(SharedString::from(id.clone()))
                                            .debug_selector(move || id.clone())
                                            .role(gpui::Role::Button)
                                            .aria_label(label.clone())
                                            .h(px(40.0))
                                            .w_full()
                                            .min_w_0()
                                            .px(px(12.0))
                                            .flex()
                                            .flex_col()
                                            .justify_center()
                                            .text_size(px(13.0))
                                            .text_color(theme.text)
                                            .cursor_pointer()
                                            .hover_bg(
                                                ("git-command-wash", i),
                                                window,
                                                if i == active {
                                                    theme.accent.opacity(0.15)
                                                } else {
                                                    gpui::transparent_black()
                                                },
                                                theme.glass_hover(),
                                            )
                                            .child(div().truncate().child(label))
                                            .when(!detail.is_empty(), |el| {
                                                el.child(
                                                    div()
                                                        .truncate()
                                                        .text_size(px(10.0))
                                                        .text_color(theme.text_muted)
                                                        .child(detail),
                                                )
                                            })
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.activate_git_choice(choice.clone(), window, cx)
                                            }))
                                            .into_any_element()
                                    })
                                    .collect::<Vec<_>>()
                            })
                        },
                    )
                    .track_scroll(&scroll)
                    .h(height)
                    .w_full(),
                );
            }
        }
        card = card.child(
            div()
                .px(px(12.0))
                .py(px(7.0))
                .border_t_1()
                .border_color(theme.border)
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .child(if self.git_form.is_some() {
                    "Enter save · Tab next field · Esc cancel"
                } else {
                    "↑↓ navigate · Enter select · Esc close"
                }),
        );
        gpui::deferred(
            gpui::anchored()
                .position(gpui::point(
                    (viewport.width - width) / 2.0,
                    px(Theme::TITLEBAR_HEIGHT + 12.0),
                ))
                .child(crate::frost::frosted(
                    8.0,
                    crate::frost::MENU_BLUR,
                    crate::motion::menu_in("git-command-palette-in", div().occlude().child(card)),
                )),
        )
        .priority(2)
        .into_any_element()
    }

    #[cfg(feature = "source-control-fixture")]
    pub(super) fn fixture_git_palette(&self, cx: &App) -> serde_json::Value {
        self.git_palette.as_ref().map_or(serde_json::Value::Null, |palette| serde_json::json!({
            "query": palette.search.read(cx).text(), "panel": format!("{:?}", self.git_panel),
            "form": format!("{:?}", self.git_form), "active": palette.active,
            "rows": self.git_choices(cx).iter().map(Choice::label).collect::<Vec<_>>()
        }))
    }
}
