//! Searchable Git actions, using the same card, rows and navigation as projects.
use super::branch::BranchCommand;
use super::git::GitPanel;
use super::*;
use gpui::Focusable;
use zeron_proto::RepositoryGitAction as Action;

pub(super) struct ActionsMenu {
    pub search: Entity<ComposerInput>,
    pub scroll: gpui::ScrollHandle,
    active: usize,
    focus_pending: bool,
    previous_focus: Option<gpui::FocusHandle>,
    pub owner: Option<gpui::AnyWindowHandle>,
    _events: gpui::Subscription,
}

#[derive(Clone)]
enum ActionKind {
    Run(Action),
    Branch(BranchCommand),
    Stage(FileKey),
    DiscardSelected(FileKey),
    DiscardAll(bool),
    UndoDiscard,
    Resolve(bool),
    Panel(GitPanel),
    UndoCommit,
}

struct ActionRow {
    id: &'static str,
    label: String,
    icon: &'static str,
    group: u8,
    enabled: bool,
    danger: bool,
    action: ActionKind,
}

impl SourceControl {
    pub(super) fn mount_actions_menu(&mut self, cx: &mut Context<Self>) {
        let search =
            cx.new(|cx| ComposerInput::with_context("Search Git actions…", "PaletteSearch", cx));
        let events = cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(menu) = this.actions_menu.open_mut() {
                    menu.active = 0;
                    menu.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                }
                cx.emit(SourceControlEvent::Updated);
                cx.notify();
            }
        });
        self.actions_menu.open(ActionsMenu {
            search,
            scroll: Default::default(),
            active: 0,
            focus_pending: true,
            previous_focus: None,
            owner: None,
            _events: events,
        });
    }

    pub(super) fn focus_actions_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.actions_menu.open_mut() {
            menu.previous_focus = window.focused(cx);
            menu.owner = Some(window.window_handle());
            window.focus(&menu.search.read(cx).focus_handle(cx), cx);
        }
    }

    /// The header mounts outside the cached file list. Claim focus after
    /// both have painted, using the same lifecycle as the Git palettes.
    pub(super) fn sync_actions_menu_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self
            .actions_menu
            .open_mut()
            .filter(|menu| menu.focus_pending)
        else {
            return;
        };
        menu.focus_pending = false;
        let input = menu.search.clone();
        let identity = input.entity_id();
        let owner = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            if owner.upgrade().is_some_and(|view| {
                view.read(cx)
                    .actions_menu
                    .as_open()
                    .is_some_and(|menu| menu.search.entity_id() == identity)
            }) {
                window.focus(&input.read(cx).focus_handle(cx), cx);
            }
        });
    }

    fn action_rows(&self, cx: &App) -> Vec<ActionRow> {
        let enabled = self.git_enabled();
        let mut rows = Vec::new();
        let mut add = |id, label: String, icon, group, available, danger, action| {
            rows.push(ActionRow {
                id,
                label,
                icon,
                group,
                enabled: enabled && available,
                danger,
                action,
            })
        };
        for (id, label, icon, action) in [
            ("git-fetch", "Fetch", icons::RESTART, Action::Fetch),
            (
                "git-pull",
                "Pull",
                icons::ALT_ARROW_DOWN,
                Action::Pull { rebase: None },
            ),
        ] {
            add(
                id,
                label.into(),
                icon,
                0,
                true,
                false,
                ActionKind::Run(action),
            );
        }
        add(
            BranchCommand::Pull.id(),
            BranchCommand::Pull.label().into(),
            icons::ALT_ARROW_DOWN,
            0,
            true,
            false,
            ActionKind::Branch(BranchCommand::Pull),
        );
        add(
            "git-push",
            "Push".into(),
            icons::ALT_ARROW_UP,
            0,
            true,
            false,
            ActionKind::Run(Action::Push),
        );
        add(
            BranchCommand::Checkout.id(),
            BranchCommand::Checkout.label().into(),
            icons::SOURCE_CONTROL,
            1,
            true,
            false,
            ActionKind::Branch(BranchCommand::Checkout),
        );
        for staged in [false, true] {
            let Some(key) = self
                .selected_files
                .iter()
                .find(|key| key.staged == staged)
                .cloned()
            else {
                continue;
            };
            let complete = self.snapshot.as_ref().is_some_and(|s| {
                s.repositories
                    .iter()
                    .any(|r| r.path == key.repository && r.complete)
            });
            add(
                if staged {
                    "git-unstage-selected"
                } else {
                    "git-stage-selected"
                },
                format!(
                    "{} Selected Changes ({})",
                    if staged { "Unstage" } else { "Stage" },
                    self.staging_paths(&key).len()
                ),
                icons::SOURCE_CONTROL,
                1,
                complete,
                false,
                ActionKind::Stage(key.clone()),
            );
            if !staged {
                add(
                    "git-discard-selected",
                    "Discard Selected Changes…".into(),
                    icons::STOP,
                    1,
                    complete && !self.discard_paths(&key).is_empty(),
                    true,
                    ActionKind::DiscardSelected(key),
                );
            }
        }
        add(
            "git-discard-all",
            "Discard All Changes…".into(),
            icons::STOP,
            1,
            true,
            true,
            ActionKind::DiscardAll(false),
        );
        if self.can_undo_discard() {
            add(
                "git-undo-discard",
                "Undo Discard".into(),
                icons::HISTORY,
                1,
                true,
                false,
                ActionKind::UndoDiscard,
            );
        }
        if self.git_state().is_some_and(|s| s.operation.is_some()) {
            add(
                "git-continue",
                "Continue Git Operation".into(),
                icons::ACTION_PLAY,
                1,
                true,
                false,
                ActionKind::Run(Action::Continue),
            );
            add(
                "git-abort",
                "Abort Git Operation…".into(),
                icons::STOP,
                1,
                true,
                true,
                ActionKind::Run(Action::Abort),
            );
        }
        if self.selected.as_ref().is_some_and(|selection| {
            selection.repository == self.active_repository
                && self.snapshot.as_ref().is_some_and(|s| {
                    s.repositories.iter().any(|r| {
                        r.path == selection.repository
                            && r.files
                                .iter()
                                .any(|f| f.path == selection.path && f.is_conflicted())
                    })
                })
        }) {
            for (id, incoming) in [("git-accept-current", false), ("git-accept-incoming", true)] {
                add(
                    id,
                    self.conflict_choice_label(incoming).into(),
                    icons::SOURCE_CONTROL,
                    1,
                    self.selected_files.len() <= 1,
                    false,
                    ActionKind::Resolve(incoming),
                );
            }
        }
        for (id, label, icon, panel) in [
            (
                "branches",
                "Branch…",
                icons::SOURCE_CONTROL,
                GitPanel::Branches,
            ),
            ("stashes", "Stashes…", icons::HARD_DRIVE, GitPanel::Stashes),
            (
                "remotes",
                "Remotes / Publish Branch…",
                icons::GLOBE,
                GitPanel::Remotes,
            ),
        ] {
            add(
                id,
                label.into(),
                icon,
                2,
                true,
                false,
                ActionKind::Panel(panel),
            );
        }
        add(
            "git-undo-commit",
            "Undo Last Commit…".into(),
            icons::HISTORY,
            3,
            true,
            false,
            ActionKind::UndoCommit,
        );
        add(
            "git-discard-everything",
            "Discard Staged and Unstaged Changes…".into(),
            icons::STOP,
            3,
            true,
            true,
            ActionKind::DiscardAll(true),
        );
        let query = self
            .actions_menu
            .get()
            .map(|m| m.search.read(cx).text().to_string())
            .unwrap_or_default();
        let labels = rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>();
        let order = popover::filter_indices(&query, &labels);
        let mut rows = rows.into_iter().map(Some).collect::<Vec<_>>();
        order.into_iter().filter_map(|i| rows[i].take()).collect()
    }

    fn activate_action_row(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.actions_menu.is_open() {
            return;
        }
        // Rebuild against current selection, completeness and busy state.
        let Some(row) = self
            .action_rows(cx)
            .into_iter()
            .find(|r| r.id == id && r.enabled)
        else {
            return;
        };
        if let Some(focus) = self
            .actions_menu
            .as_open()
            .and_then(|m| m.previous_focus.clone())
        {
            window.focus(&focus, cx);
        }
        self.close_actions_menu(cx);
        match row.action {
            ActionKind::Run(Action::Abort) => self.confirm_git(Action::Abort, "Abort Git operation?".into(), "Git will restore the repository to the state before this operation.".into(), cx),
            ActionKind::Run(action) => self.run_git(action, cx),
            ActionKind::Branch(command) => self.open_branch_picker(command, window, cx),
            ActionKind::Stage(key) => { let paths = self.staging_paths(&key); self.set_staged(key.repository, paths, !key.staged, cx); }
            ActionKind::DiscardSelected(key) => { let paths = self.discard_paths(&key); if !paths.is_empty() { self.request_discard(key.repository, paths, false, cx); } }
            ActionKind::DiscardAll(staged) => self.discard_all(staged, cx),
            ActionKind::UndoDiscard => self.undo_discard(cx),
            ActionKind::Resolve(incoming) => { if let Some(selection) = &self.selected { self.confirm_resolve_conflict(selection.path.clone(), incoming, cx); } }
            ActionKind::Panel(panel) => self.open_git_panel(panel, cx),
            ActionKind::UndoCommit => self.confirm_git(Action::UndoCommit, "Undo last local commit?".into(), "Your files will be kept and its message will return to the commit input. Git keeps a recovery reference.".into(), cx),
        }
    }

    pub(super) fn actions_menu_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.actions_menu.is_open() {
            return;
        }
        match popover::classify_key(
            &event.keystroke.key,
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        ) {
            popover::MenuKey::Escape => {
                let focus = self
                    .actions_menu
                    .as_open()
                    .and_then(|m| m.previous_focus.clone());
                self.close_actions_menu(cx);
                if let Some(focus) = focus {
                    window.focus(&focus, cx);
                }
                cx.stop_propagation();
            }
            key @ (popover::MenuKey::Up | popover::MenuKey::Down) => {
                let rows = self.action_rows(cx);
                let enabled = rows
                    .iter()
                    .enumerate()
                    .filter_map(|(i, r)| r.enabled.then_some(i))
                    .collect::<Vec<_>>();
                if let Some(menu) = self.actions_menu.open_mut() {
                    let current = enabled.iter().position(|i| *i == menu.active);
                    if let Some(next) = popover::menu_step(
                        current,
                        enabled.len(),
                        if key == popover::MenuKey::Up { -1 } else { 1 },
                    ) {
                        menu.active = enabled[next];
                        // The scroller contains the grouped body. Reveal its
                        // row using the shared picker metrics rather than
                        // treating a group divider as a keyboard target.
                        let searching = !menu.search.read(cx).text().trim().is_empty();
                        let dividers = if searching {
                            0
                        } else {
                            rows[..=menu.active]
                                .windows(2)
                                .filter(|pair| pair[0].group != pair[1].group)
                                .count()
                        };
                        let top = menu.active as f32
                            * (popover::PICKER_ROW_HEIGHT + popover::MENU_GAP)
                            + dividers as f32 * (5.0 + popover::MENU_GAP);
                        let visible = f32::from(menu.scroll.bounds().size.height);
                        let offset = menu.scroll.offset();
                        let current = -f32::from(offset.y);
                        let target = if top < current {
                            top
                        } else if top + popover::PICKER_ROW_HEIGHT > current + visible {
                            top + popover::PICKER_ROW_HEIGHT - visible
                        } else {
                            current
                        };
                        menu.scroll
                            .set_offset(gpui::point(offset.x, px(-target.max(0.0))));
                    }
                }
                cx.notify();
                cx.stop_propagation();
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let active = self.actions_menu.as_open().map(|m| m.active).unwrap_or(0);
                if let Some(row) = self.action_rows(cx).get(active) {
                    self.activate_action_row(row.id, window, cx);
                }
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    pub(super) fn render_actions_menu_rows(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).for_popup();
        let active = self.actions_menu.get().map(|m| m.active).unwrap_or(0);
        let rows = self.action_rows(cx);
        let mut body = div()
            .id("git-actions-panel")
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(popover::MENU_GAP));
        let mut previous = None;
        let searching = self
            .actions_menu
            .get()
            .is_some_and(|m| !m.search.read(cx).text().trim().is_empty());
        if rows.is_empty() {
            body = body.child(
                div()
                    .p(px(12.0))
                    .text_color(theme.text_muted)
                    .child("No Git actions found"),
            );
        }
        for (index, row) in rows.into_iter().enumerate() {
            if !searching && previous.is_some_and(|group| group != row.group) {
                body = body.child(popover::picker_divider());
            }
            previous = Some(row.group);
            let id = row.id;
            body = body.child(
                popover::picker_row(&theme, false, active == index, format!("git-action-{id}"))
                    .id(id)
                    .debug_selector(move || id.into())
                    .flex_none()
                    .role(gpui::Role::MenuItem)
                    .aria_label(row.label.clone())
                    .tooltip(crate::settings::widgets::text_tooltip(row.label.clone()))
                    .child(popover::picker_icon_slot(
                        icons::icon(row.icon)
                            .size(px(14.0))
                            .text_color(theme.text_muted)
                            .into_any_element(),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(row.label)),
                    )
                    .when(row.danger, |el| el.text_color(theme.danger))
                    .when(!row.enabled, |el| el.opacity(0.4).cursor_default())
                    .when(row.enabled, |el| {
                        el.on_click(cx.listener(move |this, _, window, cx| {
                            this.activate_action_row(id, window, cx)
                        }))
                    }),
            );
        }
        body.into_any_element()
    }

    pub(super) fn actions_menu_restore_focus(
        &self,
        cx: &App,
    ) -> Option<(gpui::AnyWindowHandle, gpui::FocusHandle, gpui::FocusHandle)> {
        let menu = self.actions_menu.get()?;
        Some((
            menu.owner?,
            menu.search.read(cx).focus_handle(cx),
            menu.previous_focus.clone()?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn search_uses_project_picker_ranking_and_current_selection(cx: &mut TestAppContext) {
        let (view, cx) = super::super::git::tests::menu_setup(cx, 900.0);
        view.update(cx, |view, cx| {
            let search = view.actions_menu.as_open().unwrap().search.clone();
            search.update(cx, |input, cx| input.set_text("bRaNcH", cx));
            let rows = view.action_rows(cx);
            assert_eq!(
                rows[0].id, "branches",
                "prefix matches precede substring matches"
            );
            assert!(
                rows.iter()
                    .all(|row| row.label.to_lowercase().contains("branch"))
            );
            search.update(cx, |input, cx| input.set_text("nothing matches this", cx));
            assert!(view.action_rows(cx).is_empty());
        });
    }

    #[gpui::test]
    fn keyboard_search_opens_stashes_and_empty_results_do_not_activate(cx: &mut TestAppContext) {
        let (view, cx) = super::super::git::tests::menu_setup(cx, 900.0);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.focus_actions_menu(window, cx));
            window.draw(cx).clear();
        });
        cx.simulate_input("no such Git action");
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, _| assert!(view.actions_menu.is_open()));
        cx.simulate_keystrokes("ctrl-a");
        cx.simulate_input("Stashes");
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, _| {
            assert!(!view.actions_menu.is_open());
            assert_eq!(view.git_panel, Some(GitPanel::Stashes));
        });
    }

    #[gpui::test]
    fn keyboard_wrap_reveals_last_row_in_a_short_window(cx: &mut TestAppContext) {
        let (view, cx) = super::super::git::tests::menu_setup(cx, 280.0);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.focus_actions_menu(window, cx));
            window.draw(cx).clear();
        });
        cx.simulate_keystrokes("up");
        cx.update(|window, cx| window.draw(cx).clear());
        view.read_with(cx, |view, cx| {
            let active = view.actions_menu.as_open().unwrap().active;
            assert_eq!(view.action_rows(cx)[active].id, "git-discard-everything");
        });
        let row = cx.debug_bounds("git-discard-everything").unwrap();
        let scroll = cx.debug_bounds("git-menu-scroll").unwrap();
        assert!(row.top() >= scroll.top() && row.bottom() <= scroll.bottom());
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, _| assert!(!view.actions_menu.is_open()));
    }

    #[gpui::test]
    fn disabled_or_removed_actions_cannot_run_from_a_stale_menu(cx: &mut TestAppContext) {
        let (view, cx) = super::super::git::tests::menu_setup(cx, 900.0);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.busy = true;
                view.activate_action_row("git-push", window, cx);
                assert!(view.actions_menu.is_open());
                assert!(view.operation_label.is_none());
                view.busy = false;
                view.activate_action_row("git-stage-selected", window, cx);
                assert!(
                    view.actions_menu.is_open(),
                    "removed selection actions are rejected"
                );
            })
        });
    }

    #[gpui::test]
    fn closing_a_menu_before_its_first_frame_cancels_pending_focus(cx: &mut TestAppContext) {
        let (view, cx) = super::super::git::tests::menu_setup(cx, 900.0);
        let previous = cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                window.focus(&view.list_focus, cx);
                view.focus_actions_menu(window, cx);
                view.sync_actions_menu_focus(window, cx);
                view.close_actions_menu(cx);
                view.list_focus.clone()
            })
        });
        cx.update(|window, cx| window.draw(cx).clear());
        cx.update(|window, _| {
            assert!(
                previous.is_focused(window),
                "a closing menu must not steal focus on its deferred frame"
            )
        });
    }
}
