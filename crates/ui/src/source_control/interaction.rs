//! File menus and list navigation follow VS Code's SCM view and Git commands.
//! Reference: microsoft/vscode at 20f57d8a036b9b85b49689cfeb848ec8ddf09533,
//! scmViewPane.ts (resource action context) and commands.ts (stage/unstage/clean).
use super::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum PrimaryAction {
    Commit,
    Continue,
    Publish,
    Sync,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(super) enum SortOrder {
    #[default]
    Path,
    Name,
    Status,
}

pub(super) struct FileMenu {
    key: FileKey,
    position: gpui::Point<gpui::Pixels>,
    active: usize,
}

#[derive(Clone, Copy)]
enum FileAction {
    Diff,
    Open,
    Stage,
    Discard,
    CopyPath,
    CopyRelativePath,
    AcceptCurrent,
    AcceptIncoming,
}

impl SourceControl {
    pub(super) fn primary_action(&self) -> PrimaryAction {
        if self
            .git_state()
            .is_some_and(|state| state.operation.is_some())
        {
            return PrimaryAction::Continue;
        }
        let clean = !self.amend
            && self.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.repositories.iter().any(|repo| {
                    repo.path == self.active_repository && repo.complete && repo.files.is_empty()
                })
            });
        if clean && let Some(state) = self.git_state() {
            if state.head.is_some() && state.branch.is_some() && state.upstream.is_none() {
                return PrimaryAction::Publish;
            }
            if state.upstream.is_some()
                && (state.ahead.unwrap_or(0) > 0 || state.behind.unwrap_or(0) > 0)
            {
                return PrimaryAction::Sync;
            }
        }
        PrimaryAction::Commit
    }

    pub(super) fn primary_action_enabled(&self, cx: &App) -> bool {
        match self.primary_action() {
            PrimaryAction::Commit => self.can_request_commit(cx),
            PrimaryAction::Continue => {
                self.git_enabled() && self.git_state().is_some_and(|state| state.conflicts == 0)
            }
            PrimaryAction::Publish => self.git_enabled(),
            PrimaryAction::Sync => self.can_sync(),
        }
    }

    pub(super) fn publish_branch(&mut self, cx: &mut Context<Self>) {
        if !self.git_enabled() {
            return;
        }
        if let Some(remote) = self
            .git_details
            .as_ref()
            .and_then(|details| details.publish_remote.clone())
        {
            self.run_git(zeron_proto::RepositoryGitAction::Publish { remote }, cx);
        } else {
            self.open_git_panel(GitPanel::Remotes, cx);
        }
    }

    pub(super) fn run_primary_action(&mut self, cx: &mut Context<Self>) {
        if !self.primary_action_enabled(cx) {
            return;
        }
        match self.primary_action() {
            PrimaryAction::Commit => self.commit_with_followup(self.commit_primary.clone(), cx),
            PrimaryAction::Continue => self.run_git(zeron_proto::RepositoryGitAction::Continue, cx),
            PrimaryAction::Publish => self.publish_branch(cx),
            PrimaryAction::Sync => self.run_git(zeron_proto::RepositoryGitAction::Sync, cx),
        }
    }
    pub(super) fn close_file_menu(&mut self, cx: &mut Context<Self>) {
        if self.file_menu.begin_close() {
            popover::reap_popup(cx, |view| &mut view.file_menu);
            cx.notify();
        }
    }

    fn file_selection(&self, key: &FileKey) -> Option<CheckoutChangeSelection> {
        let target = self.target.as_ref()?;
        self.snapshot
            .as_ref()?
            .repositories
            .iter()
            .find(|repo| repo.path == key.repository)?
            .files
            .iter()
            .find(|f| f.path == key.path)?;
        Some(CheckoutChangeSelection {
            cwd: target.cwd.clone(),
            repository: key.repository.clone(),
            path: key.path.clone(),
            staged: key.staged,
        })
    }

    pub(super) fn open_file_menu(
        &mut self,
        key: FileKey,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file_selection(&key).is_none() || self.confirmation.is_some() {
            return;
        }
        if !self.selected_files.contains(&key) {
            self.select_repository(key.repository.clone(), cx);
            if self.active_repository != key.repository {
                return;
            }
            self.select_files(key.clone(), false, false);
        }
        self.selected = self.file_selection(&key);
        self.close_actions_menu(cx);
        self.close_commit_menu(cx);
        self.file_menu.open(FileMenu {
            key,
            position,
            active: 0,
        });
        self.list_focus.focus(window, cx);
        cx.notify();
    }

    fn file_actions(&self, key: &FileKey) -> Vec<(FileAction, String, bool)> {
        let Some(repo) = self
            .snapshot
            .as_ref()
            .and_then(|s| s.repositories.iter().find(|r| r.path == key.repository))
        else {
            return Vec::new();
        };
        let Some(file) = repo.files.iter().find(|f| f.path == key.path) else {
            return Vec::new();
        };
        let count = self.staging_paths(key).len();
        let suffix = if count > 1 {
            format!(" Selected Changes ({count})")
        } else {
            " Changes".into()
        };
        let enabled = repo.complete && !self.is_busy();
        let mut actions = vec![(FileAction::Diff, "Open Changes".into(), true)];
        if can_open_file(file) && !repo.submodules.contains(&file.path) {
            actions.push((FileAction::Open, "Open File".into(), true));
        }
        actions.push((
            FileAction::Stage,
            format!("{}{suffix}", if key.staged { "Unstage" } else { "Stage" }),
            enabled,
        ));
        if !key.staged && !file.is_conflicted() && !repo.submodules.contains(&file.path) {
            actions.push((
                FileAction::Discard,
                format!("Discard{suffix}…"),
                enabled && !self.discard_paths(key).is_empty(),
            ));
        }
        if file.is_conflicted() {
            actions.push((
                FileAction::AcceptCurrent,
                "Accept Current Version".into(),
                enabled,
            ));
            actions.push((
                FileAction::AcceptIncoming,
                "Accept Incoming Version".into(),
                enabled,
            ));
        }
        actions.extend([
            (FileAction::CopyPath, "Copy Path".into(), true),
            (
                FileAction::CopyRelativePath,
                "Copy Relative Path".into(),
                true,
            ),
        ]);
        actions
    }

    fn run_file_action(&mut self, key: FileKey, action: FileAction, cx: &mut Context<Self>) {
        let Some(selection) = self.file_selection(&key) else {
            self.close_file_menu(cx);
            return;
        };
        self.close_file_menu(cx);
        match action {
            FileAction::AcceptCurrent | FileAction::AcceptIncoming => self.run_git(
                zeron_proto::RepositoryGitAction::ResolveConflict {
                    path: key.path,
                    incoming: matches!(action, FileAction::AcceptIncoming),
                },
                cx,
            ),
            FileAction::Diff => cx.emit(SourceControlEvent::OpenDiff(selection)),
            FileAction::Open => {
                cx.emit(SourceControlEvent::OpenFile(if key.repository.is_empty() {
                    key.path
                } else {
                    format!("{}/{}", key.repository, key.path)
                }))
            }
            FileAction::Stage => self.set_staged(
                key.repository.clone(),
                self.staging_paths(&key),
                !key.staged,
                cx,
            ),
            FileAction::Discard => {
                self.request_discard(key.repository.clone(), self.discard_paths(&key), false, cx)
            }
            FileAction::CopyPath | FileAction::CopyRelativePath => {
                let mut paths: Vec<_> = if self.selected_files.contains(&key) {
                    self.selected_files
                        .iter()
                        .filter(|file| file.repository == key.repository)
                        .map(|file| file.path.clone())
                        .collect()
                } else {
                    vec![key.path.clone()]
                };
                paths.sort();
                paths.dedup();
                let paths = paths
                    .into_iter()
                    .map(|path| {
                        let relative = if key.repository.is_empty() {
                            path
                        } else {
                            format!("{}/{path}", key.repository)
                        };
                        if matches!(action, FileAction::CopyPath) {
                            std::path::Path::new(&selection.cwd)
                                .join(relative)
                                .to_string_lossy()
                                .into_owned()
                        } else {
                            relative
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(paths));
            }
        }
    }

    pub(super) fn render_file_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.file_menu.get()?;
        let theme = Theme::of(cx).for_popup();
        let mut card = popover::popover_card(&theme)
            .w(px(250.0))
            .flex()
            .flex_col()
            .id("sc-file-menu-card")
            .debug_selector(|| "sc-file-menu-card".into())
            .role(gpui::Role::Menu)
            .on_mouse_down_out(cx.listener(|view, _, _, cx| view.close_file_menu(cx)))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for (index, (action, label, enabled)) in
            self.file_actions(&menu.key).into_iter().enumerate()
        {
            if matches!(action, FileAction::Stage | FileAction::CopyPath) {
                card = card.child(popover::menu_separator());
            }
            let key = menu.key.clone();
            card = card.child(
                popover::menu_row(
                    &theme,
                    menu.active == index,
                    format!("sc-file-action-{index}"),
                )
                .id(gpui::SharedString::from(format!("sc-file-action-{index}")))
                .debug_selector(move || format!("sc-file-action-{index}").into())
                .role(gpui::Role::MenuItem)
                .aria_label(label.clone())
                .child(label)
                .when(matches!(action, FileAction::Discard), |row| {
                    row.text_color(theme.danger)
                })
                .when(!enabled, |row| row.opacity(0.4))
                .when(enabled, |row| {
                    row.on_click(cx.listener(move |view, _, _, cx| {
                        view.run_file_action(key.clone(), action, cx)
                    }))
                }),
            );
        }
        let marker = self.file_menu_bounds.clone();
        let card = card.relative().child(
            gpui::canvas(
                move |bounds, _, _| marker.set(Some(bounds)),
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        );
        Some(popover::menu_at(
            "sc-file-menu",
            menu.position,
            card.into_any_element(),
            self.file_menu.closing_since(),
        ))
    }

    pub(super) fn on_list_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.list_focus.is_focused(window) || self.confirmation.is_some() {
            return;
        }
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if self.file_menu.is_open() {
            let menu = self.file_menu.as_open().unwrap();
            let actions = self.file_actions(&menu.key);
            let active = menu.active;
            let file = menu.key.clone();
            match key {
                "escape" => self.close_file_menu(cx),
                "up" | "down" if !actions.is_empty() => {
                    let next = if key == "up" {
                        (active + actions.len() - 1) % actions.len()
                    } else {
                        (active + 1) % actions.len()
                    };
                    self.file_menu.open_mut().unwrap().active = next;
                    cx.notify();
                }
                "enter" if actions.get(active).is_some_and(|a| a.2) => {
                    self.run_file_action(file, actions[active].0, cx)
                }
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        let focused = self
            .selected
            .as_ref()
            .map(|s| FileKey::new(&s.repository, &s.path, s.staged));
        match key {
            "up" | "down" | "home" | "end" => {
                let keys = self.visible_file_keys();
                if keys.is_empty() {
                    return;
                }
                let current = focused
                    .as_ref()
                    .and_then(|f| keys.iter().position(|k| k == f));
                let index = match key {
                    "home" => 0,
                    "end" => keys.len() - 1,
                    "up" => current.unwrap_or(1).saturating_sub(1),
                    _ => current.map(|i| (i + 1).min(keys.len() - 1)).unwrap_or(0),
                };
                let file = keys[index].clone();
                self.select_repository(file.repository.clone(), cx);
                if self.active_repository != file.repository {
                    return;
                }
                self.select_files(
                    file.clone(),
                    modifiers.shift,
                    modifiers.control || modifiers.platform,
                );
                self.selected = self.file_selection(&file);
                let row = self.rows.iter().position(|row| match row {
                    Row::File {
                        repository,
                        file: ix,
                        staged,
                    } => self.snapshot.as_ref().is_some_and(|s| {
                        s.repositories[*repository].path == file.repository
                            && s.repositories[*repository].files[*ix].path == file.path
                            && *staged == file.staged
                    }),
                    _ => false,
                });
                if let Some(row) = row {
                    self.list_scroll
                        .scroll_to_item(row, gpui::ScrollStrategy::Nearest);
                }
                if let Some(selection) = self.selected.clone() {
                    cx.emit(SourceControlEvent::OpenDiff(selection));
                }
                cx.notify();
            }
            "a" if modifiers.control || modifiers.platform => {
                if let Some(file) = focused {
                    self.selected_files = self
                        .visible_file_keys()
                        .into_iter()
                        .filter(|k| k.repository == file.repository)
                        .collect();
                    cx.notify();
                }
            }
            "enter" => {
                if let Some(selection) = self.selected.clone() {
                    cx.emit(SourceControlEvent::OpenDiff(selection));
                }
            }
            "delete" | "backspace" if !self.is_busy() => {
                if let Some(file) = focused.filter(|f| !f.staged) {
                    let paths = self.discard_paths(&file);
                    if !paths.is_empty() {
                        self.request_discard(file.repository, paths, false, cx);
                    }
                }
            }
            "escape" => {
                self.selected_files.clear();
                self.selected = None;
                self.selection_anchor = None;
                cx.notify();
            }
            "f10" if modifiers.shift => {
                if let Some(file) = focused {
                    let position = self
                        .selected_row_bounds
                        .get()
                        .map(|bounds| bounds.bottom_left())
                        .unwrap_or_default();
                    self.open_file_menu(file, position, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

pub(super) fn can_open_file(file: &GitFileStatus) -> bool {
    if file.is_conflicted() {
        return !(file.index == GitFileState::Deleted && file.worktree == GitFileState::Deleted);
    }
    file.worktree != GitFileState::Deleted
        && !(file.index == GitFileState::Deleted && file.worktree == GitFileState::Unchanged)
}
