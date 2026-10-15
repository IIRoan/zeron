//! Branch commands use a recent-first quick picker, with all refs searchable.
use super::*;
use gpui::{FocusHandle, Subscription, UniformListScrollHandle};
use zeron_proto::{RepositoryGitAction as Action, RepositoryGitBranch, RepositoryGitState};

const RECENT_LIMIT: usize = 8;
const RECENT_AGE: u64 = 30 * 24 * 60 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BranchCommand {
    Checkout,
    Pull,
    Merge,
    Rebase,
    Delete,
}

impl BranchCommand {
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Checkout => "git-checkout-to",
            Self::Pull => "git-pull-from",
            Self::Merge => "git-merge-branch",
            Self::Rebase => "git-rebase-branch",
            Self::Delete => "git-delete-branch",
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Checkout => "Checkout to…",
            Self::Pull => "Pull from…",
            Self::Merge => "Merge Branch…",
            Self::Rebase => "Rebase onto…",
            Self::Delete => "Delete Branch…",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::Checkout => "Search branches to check out…",
            Self::Pull => "Search remote branches to pull from…",
            Self::Merge => "Search branches to merge…",
            Self::Rebase => "Search branches to rebase onto…",
            Self::Delete => "Search local branches to delete…",
        }
    }

    fn includes(self, branch: &RepositoryGitBranch) -> bool {
        match self {
            Self::Checkout => true,
            Self::Pull => branch.remote,
            Self::Merge | Self::Rebase => !branch.current,
            Self::Delete => !branch.remote && !branch.current,
        }
    }

    fn action(self, branch: &RepositoryGitBranch) -> Action {
        // Full refs disambiguate identically named local and remote branches.
        let reference = format!(
            "refs/{}/{}",
            if branch.remote { "remotes" } else { "heads" },
            branch.name
        );
        match self {
            Self::Checkout => Action::SwitchBranch { branch: reference },
            Self::Pull => Action::PullFrom { branch: reference },
            Self::Merge => Action::Merge { branch: reference },
            Self::Rebase => Action::Rebase { branch: reference },
            Self::Delete => Action::DeleteBranch { name: reference },
        }
    }
}

pub(super) struct BranchPicker {
    command: BranchCommand,
    target: Target,
    repository: String,
    state: RepositoryGitState,
    search: Entity<ComposerInput>,
    previous_focus: Option<FocusHandle>,
    entries: Vec<RepositoryGitBranch>,
    rows: Vec<usize>,
    active: usize,
    show_all: bool,
    scroll: UniformListScrollHandle,
    loading: bool,
    error: Option<SharedString>,
    _events: Subscription,
    _load: Option<Task<()>>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// Prefer exact/prefix/substring matches; accept fuzzy subsequences as well.
pub(super) fn match_score(name: &str, query: &str) -> Option<u32> {
    let name = name.to_lowercase();
    let mut score = 0;
    for word in query.split_whitespace().map(str::to_lowercase) {
        score += if name == word {
            0
        } else if name.starts_with(&word) {
            1
        } else if name.contains(&word) {
            2
        } else {
            let mut chars = name.chars();
            for wanted in word.chars() {
                chars.find(|&c| c == wanted)?;
            }
            3
        };
    }
    Some(score)
}

fn matching(
    entries: &[RepositoryGitBranch],
    command: BranchCommand,
    query: &str,
    show_all: bool,
    now: u64,
) -> Vec<usize> {
    let searching = !query.trim().is_empty();
    let cutoff = now.saturating_sub(RECENT_AGE);
    let mut rows: Vec<_> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, branch)| {
            if !command.includes(branch) {
                return None;
            }
            if !searching
                && !show_all
                && !branch.current
                && !branch.last_used_at.is_some_and(|time| time >= cutoff)
                && !branch.commit_time.is_some_and(|time| time >= cutoff)
            {
                return None;
            }
            Some((match_score(&branch.name, query)?, index))
        })
        .collect();
    rows.sort_by_key(|&(score, index)| (score, index));
    if !searching && !show_all {
        rows.truncate(RECENT_LIMIT);
    }
    rows.into_iter().map(|(_, index)| index).collect()
}

impl BranchPicker {
    fn rebuild(&mut self, cx: &App) {
        let query = self.search.read(cx).text();
        self.rows = matching(&self.entries, self.command, query, self.show_all, now());
        self.active = if query.trim().is_empty() {
            self.rows
                .iter()
                .position(|&index| !self.entries[index].current)
                .unwrap_or(0)
        } else {
            0
        };
        self.scroll
            .scroll_to_item(self.active, gpui::ScrollStrategy::Nearest);
    }

    #[cfg(feature = "source-control-fixture")]
    pub(super) fn fixture_state(&self, cx: &App) -> serde_json::Value {
        serde_json::json!({ "command": format!("{:?}", self.command), "loading": self.loading,
            "count": self.entries.len(), "active": self.active, "error": self.error,
            "query": self.search.read(cx).text(), "showAll": self.show_all,
            "visible": self.rows.iter().map(|&i| &self.entries[i]).collect::<Vec<_>>() })
    }
}

impl SourceControl {
    pub(super) fn open_branch_picker(
        &mut self,
        command: BranchCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.git_enabled() {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some(state) = self.git_state().cloned() else {
            return;
        };
        let repository = self.active_repository.clone();
        let search = cx.new(|cx| {
            ComposerInput::with_context(command.placeholder(), "PaletteSearch", cx)
                .with_single_line()
                .with_text_metrics(13.0, 20.0)
        });
        let events = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited)
                && let Some(picker) = this.branch_picker.as_mut()
            {
                picker.rebuild(cx);
                cx.notify();
            }
        });
        let identity = search.entity_id();
        self.close_actions_menu(cx);
        self.close_commit_menu(cx);
        self.stash_picker = None;
        self.branch_picker = Some(BranchPicker {
            command,
            target: target.clone(),
            repository: repository.clone(),
            state,
            search: search.clone(),
            previous_focus: window.focused(cx),
            entries: Vec::new(),
            rows: Vec::new(),
            active: 0,
            show_all: false,
            scroll: UniformListScrollHandle::new(),
            loading: true,
            error: None,
            _events: events,
            _load: None,
        });
        if let Some(engine) = self.state.read(cx).engine().cloned() {
            let task = cx.spawn_in(window, async move |this, cx| {
                let result = engine.client().call_as::<Vec<RepositoryGitBranch>>(methods::GET_CHECKOUT_BRANCHES,
                    serde_json::json!({ "cwd": target.cwd, "targetDeviceId": target.device, "repository": repository })).await;
                this.update_in(cx, |this, _, cx| {
                    if this.target.as_ref() != Some(&target) || this.active_repository != repository { return; }
                    let Some(picker) = this.branch_picker.as_mut().filter(|picker| picker.search.entity_id() == identity) else { return; };
                    picker.loading = false;
                    match result {
                        Ok(entries) => { picker.entries = entries; picker.rebuild(cx); }
                        Err(error) => picker.error = Some(format!("Unable to load branches: {error}").into()),
                    }
                    cx.notify();
                }).ok();
            });
            self.branch_picker.as_mut().unwrap()._load = Some(task);
        } else {
            let picker = self.branch_picker.as_mut().unwrap();
            picker.loading = false;
            picker.error = Some("Repository is unavailable".into());
        }
        let owner = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            if owner.upgrade().is_some_and(|view| {
                view.read(cx)
                    .branch_picker
                    .as_ref()
                    .is_some_and(|picker| picker.search.entity_id() == identity)
            }) {
                window.focus(&gpui::Focusable::focus_handle(search.read(cx), cx), cx);
            }
        });
        cx.notify();
    }

    fn close_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(picker) = self.branch_picker.take() {
            if let Some(focus) = picker.previous_focus {
                window.focus(&focus, cx);
            }
            cx.notify();
        }
    }

    fn activate_branch(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.git_enabled() {
            return;
        }
        let Some(picker) = &self.branch_picker else {
            return;
        };
        let Some(branch) = picker
            .entries
            .get(index)
            .filter(|branch| picker.command.includes(branch))
            .cloned()
        else {
            return;
        };
        if self.target.as_ref() != Some(&picker.target)
            || self.active_repository != picker.repository
            || self.git_state().is_none_or(|state| {
                state.head != picker.state.head || state.branch != picker.state.branch
            })
        {
            self.close_branch_picker(window, cx);
            self.notify_error(
                &self.active_repository.clone(),
                "The branch changed; reopen the branch picker",
                cx,
            );
            return;
        }
        let command = picker.command;
        let state = picker.state.clone();
        self.close_branch_picker(window, cx);
        if command == BranchCommand::Checkout && branch.current {
            return;
        }
        if command == BranchCommand::Delete {
            self.confirm_git(
                command.action(&branch),
                format!("Delete branch {}?", branch.name),
                "Only a fully merged local branch can be deleted.".into(),
                cx,
            );
        } else {
            let action = command.action(&branch);
            self.dispatch(methods::RUN_CHECKOUT_GIT_ACTION,
                serde_json::json!({ "action": action, "expectedHead": state.head, "expectedBranch": state.branch }),
                match command {
                    BranchCommand::Checkout => "Switching branch…",
                    BranchCommand::Pull => "Pulling changes…",
                    BranchCommand::Merge => "Merging branch…",
                    BranchCommand::Rebase => "Rebasing branch…",
                    BranchCommand::Delete => unreachable!(),
                }, cx);
        }
    }

    fn branch_picker_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.branch_picker.as_mut() else {
            return;
        };
        match event.keystroke.key.as_str() {
            "escape" => self.close_branch_picker(window, cx),
            "up" | "down" | "tab" => {
                if !picker.rows.is_empty() {
                    let count = picker.rows.len();
                    let forward = event.keystroke.key == "down"
                        || (event.keystroke.key == "tab" && !event.keystroke.modifiers.shift);
                    picker.active = if forward {
                        (picker.active + 1) % count
                    } else {
                        (picker.active + count - 1) % count
                    };
                    picker
                        .scroll
                        .scroll_to_item(picker.active, gpui::ScrollStrategy::Nearest);
                    cx.notify();
                }
            }
            "enter" => {
                if !event.is_held
                    && let Some(&index) = picker.rows.get(picker.active)
                {
                    self.activate_branch(index, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(super) fn render_branch_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(picker) = &self.branch_picker else {
            return gpui::Empty.into_any_element();
        };
        let theme = Theme::of(cx).for_popup();
        let viewport = window.viewport_size();
        let width = px(560.0).min((viewport.width - px(32.0)).max(px(1.0)));
        let rows = picker.rows.clone();
        let count = rows.len();
        let list_height =
            px((count.clamp(1, 8) * 36) as f32).min((viewport.height - px(220.0)).max(px(1.0)));
        let active = picker.active;
        let scroll = picker.scroll.clone();
        let search = picker.search.clone();
        let title = picker.command.label();
        let current = picker.state.branch.as_deref().unwrap_or("Detached HEAD");
        let context = match picker.command {
            BranchCommand::Checkout => format!("Current branch: {current}"),
            BranchCommand::Pull => format!("Pull into {current}"),
            BranchCommand::Merge => format!("Merge into {current}"),
            BranchCommand::Rebase => format!("Rebase {current} onto"),
            BranchCommand::Delete => "Delete a merged local branch".into(),
        };
        let recent = !picker.show_all && picker.search.read(cx).text().trim().is_empty();
        let total = picker
            .entries
            .iter()
            .filter(|branch| picker.command.includes(branch))
            .count();
        let can_show_all = recent && count < total && !picker.loading && picker.error.is_none();
        let empty: SharedString = if let Some(error) = &picker.error {
            error.clone()
        } else if picker.loading {
            "Loading branches…".into()
        } else if total == 0 && picker.command == BranchCommand::Pull {
            "No remote branches. Fetch to update the branch list.".into()
        } else if total == 0 {
            "No branches available".into()
        } else if recent {
            "No recent branches. Search or show all branches.".into()
        } else {
            "No matching branches".into()
        };
        let body = if count == 0 {
            div()
                .h(list_height)
                .px(px(12.0))
                .flex()
                .items_center()
                .text_size(px(13.0))
                .text_color(theme.text_muted)
                .child(empty)
                .into_any_element()
        } else {
            let entity = cx.entity();
            let theme = theme.clone();
            uniform_list("git-branch-picker-list", count, move |range, window, cx| {
                entity.update(cx, |this, cx| {
                    range
                        .filter_map(|i| {
                            let index = *rows.get(i)?;
                            let branch = this.branch_picker.as_ref()?.entries.get(index)?;
                            // Bound shaping while keeping the full ref for Git and the tooltip.
                            let name: String = branch.name.chars().take(256).collect();
                            let label = if name.len() < branch.name.len() {
                                format!("{name}…")
                            } else {
                                name
                            };
                            Some(
                                div()
                                    .id(("git-branch-choice", i))
                                    .debug_selector(move || format!("git-branch-choice-{i}"))
                                    .role(gpui::Role::Button)
                                    .aria_label(branch.name.clone())
                                    .h(px(36.0))
                                    .w_full()
                                    .min_w_0()
                                    .px(px(10.0))
                                    .flex()
                                    .items_center()
                                    .gap(px(10.0))
                                    .text_size(px(13.0))
                                    .text_color(theme.text)
                                    .cursor_pointer()
                                    .hover_bg(
                                        ("git-branch-choice-wash", i),
                                        window,
                                        if i == active {
                                            theme.accent.opacity(0.15)
                                        } else {
                                            gpui::transparent_black()
                                        },
                                        theme.glass_hover(),
                                    )
                                    .tooltip(crate::settings::widgets::text_tooltip(
                                        branch.name.clone(),
                                    ))
                                    .child(
                                        icons::icon(icons::GIT_BRANCH)
                                            .size(px(14.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .child(div().flex_1().min_w_0().truncate().child(label))
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(px(11.0))
                                            .text_color(theme.text_muted)
                                            .child(if branch.current {
                                                "✓ Current"
                                            } else if branch.remote {
                                                "Remote"
                                            } else {
                                                "Local"
                                            }),
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.activate_branch(index, window, cx)
                                    }))
                                    .into_any_element(),
                            )
                        })
                        .collect::<Vec<_>>()
                })
            })
            .track_scroll(&scroll)
            .h(list_height)
            .w_full()
            .into_any_element()
        };
        let card = popover::popover_card(&theme)
            .id("git-branch-picker")
            .debug_selector(|| "git-branch-picker".into())
            .w(width)
            .rounded(px(8.0))
            .role(gpui::Role::Dialog)
            .aria_label(title)
            .flex()
            .flex_col()
            .overflow_hidden()
            .capture_key_down(cx.listener(Self::branch_picker_key))
            .on_mouse_down_out(
                cx.listener(|this, _, window, cx| this.close_branch_picker(window, cx)),
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
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_none()
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
                    ),
            )
            .child(div().p(px(6.0)).child(popover::search_input_frame(
                &theme,
                search.into_any_element(),
            )))
            .child(
                div()
                    .px(px(12.0))
                    .py(px(5.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_muted)
                    .child(if recent {
                        "Recent branches".to_owned()
                    } else {
                        format!(
                            "Branches · {count} {}",
                            if count == 1 { "match" } else { "matches" }
                        )
                    }),
            )
            .child(body)
            .when(can_show_all, |el| {
                el.child(
                    div()
                        .id("git-branch-show-all")
                        .debug_selector(|| "git-branch-show-all".into())
                        .role(gpui::Role::Button)
                        .aria_label("Show all branches")
                        .h(px(30.0))
                        .px(px(12.0))
                        .flex()
                        .items_center()
                        .text_size(px(12.0))
                        .text_color(theme.accent)
                        .cursor_pointer()
                        .hover_bg(
                            "git-branch-show-all-wash",
                            window,
                            gpui::transparent_black(),
                            theme.glass_hover(),
                        )
                        .child(format!("Show all {total} branches…"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(picker) = this.branch_picker.as_mut() {
                                picker.show_all = true;
                                picker.rebuild(cx);
                                window.focus(
                                    &gpui::Focusable::focus_handle(picker.search.read(cx), cx),
                                    cx,
                                );
                                cx.notify();
                            }
                        })),
                )
            })
            .child(
                div()
                    .px(px(12.0))
                    .py(px(7.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("Search all branches · ↑↓ navigate · Enter select · Esc close"),
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
                    crate::motion::menu_in("git-branch-picker-in", div().occlude().child(card)),
                )),
        )
        .priority(2)
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext, VisualTestContext};

    fn entry(
        name: &str,
        remote: bool,
        current: bool,
        used: Option<u64>,
        committed: u64,
    ) -> RepositoryGitBranch {
        RepositoryGitBranch {
            name: name.into(),
            remote,
            current,
            last_used_at: used,
            commit_time: Some(committed),
        }
    }

    #[test]
    fn recent_branches_hide_stale_refs_but_search_and_show_all_recover_them() {
        let time = 2 * RECENT_AGE;
        let mut entries = vec![
            entry("main", false, true, None, 1),
            entry("feature/recent-checkout", false, false, Some(time), 1),
            entry("origin/recent-commit", true, false, None, time),
            entry("feature/ancient", false, false, Some(1), 1),
        ];
        assert_eq!(
            matching(&entries, BranchCommand::Checkout, "", false, time),
            [0, 1, 2]
        );
        assert_eq!(
            matching(&entries, BranchCommand::Checkout, "ancient", false, time),
            [3]
        );
        assert_eq!(
            matching(&entries, BranchCommand::Checkout, "", true, time),
            [0, 1, 2, 3]
        );
        entries.extend((0..20).map(|i| entry(&format!("new/{i}"), false, false, None, time)));
        assert_eq!(
            matching(&entries, BranchCommand::Checkout, "", false, time).len(),
            RECENT_LIMIT
        );
    }

    #[test]
    fn picker_filters_by_operation_and_fuzzy_search_prefers_exact_matches() {
        let entries = vec![
            entry("main", false, true, None, 1),
            entry("origin/main", true, false, None, 1),
            entry("feature/long-branch", false, false, None, 1),
            entry("flb", false, false, None, 1),
        ];
        assert_eq!(matching(&entries, BranchCommand::Pull, "", true, 1), [1]);
        assert_eq!(
            matching(&entries, BranchCommand::Delete, "", true, 1),
            [2, 3]
        );
        assert_eq!(
            matching(&entries, BranchCommand::Merge, "main", false, 1),
            [1]
        );
        assert_eq!(
            matching(&entries, BranchCommand::Checkout, "FLB", false, 1),
            [3, 2]
        );
        assert_eq!(match_score("feature/🌻-search", "🌻 srch"), Some(5));
        assert_eq!(match_score("main", "missing"), None);
        assert_eq!(
            BranchCommand::Pull.action(&entries[1]),
            Action::PullFrom {
                branch: "refs/remotes/origin/main".into()
            }
        );
    }

    struct PickerHost {
        view: Entity<SourceControl>,
        _observer: Subscription,
    }

    impl Render for PickerHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.view
                .update(cx, |view, cx| view.render_branch_picker(window, cx))
        }
    }

    fn setup(
        cx: &mut TestAppContext,
        command: BranchCommand,
        entries: Vec<RepositoryGitBranch>,
    ) -> (Entity<SourceControl>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::composer::init(cx, Default::default());
        });
        let (host, cx) = cx.add_window_view(|window, cx| {
            let state = cx.new(|_| AppState::new());
            let view = cx.new(|cx| {
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
                            head: Some("head".into()),
                            branch: Some("main".into()),
                            ..Default::default()
                        }),
                    }],
                });
                view.open_branch_picker(command, window, cx);
                let picker = view.branch_picker.as_mut().unwrap();
                picker.error = None;
                picker.loading = false;
                picker.entries = entries;
                picker.rebuild(cx);
                view
            });
            // VisualTestContext draws do not deliver a platform frame, so
            // explicitly focus the input after mounting the picker.
            let input = view.read(cx).branch_picker.as_ref().unwrap().search.clone();
            window.focus(&gpui::Focusable::focus_handle(input.read(cx), cx), cx);
            PickerHost {
                _observer: cx.observe(&view, |_, _, cx| cx.notify()),
                view,
            }
        });
        let view = host.read_with(cx, |host, _| host.view.clone());
        cx.simulate_resize(gpui::size(px(800.0), px(600.0)));
        cx.update(|window, cx| window.draw(cx).clear());
        (view, cx)
    }

    #[gpui::test]
    fn picker_searches_hidden_branches_and_supports_keyboard_and_escape(cx: &mut TestAppContext) {
        let entries = vec![
            entry("main", false, true, None, 1),
            entry("recent", false, false, Some(now()), 1),
            entry("ancient", false, false, None, 1),
        ];
        let (view, cx) = setup(cx, BranchCommand::Checkout, entries);
        view.read_with(cx, |view, _| {
            assert_eq!(view.branch_picker.as_ref().unwrap().rows, [0, 1])
        });
        assert!(cx.debug_bounds("git-branch-show-all").is_some());
        cx.simulate_keystrokes("a n c i e n t");
        view.read_with(cx, |view, _| {
            assert_eq!(view.branch_picker.as_ref().unwrap().rows, [2])
        });
        cx.simulate_keystrokes("ctrl-a backspace");
        view.read_with(cx, |view, _| {
            assert_eq!(view.branch_picker.as_ref().unwrap().rows, [0, 1])
        });
        cx.simulate_keystrokes("down");
        view.read_with(cx, |view, _| {
            assert_eq!(view.branch_picker.as_ref().unwrap().active, 0)
        });
        cx.simulate_keystrokes("tab");
        view.read_with(cx, |view, _| {
            assert_eq!(view.branch_picker.as_ref().unwrap().active, 1)
        });
        cx.simulate_keystrokes("shift-tab");
        view.read_with(cx, |view, _| {
            assert_eq!(view.branch_picker.as_ref().unwrap().active, 0)
        });
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(view.branch_picker.is_none()));
    }

    #[gpui::test]
    fn show_all_virtualizes_a_large_branch_list(cx: &mut TestAppContext) {
        let entries = (0..10_000)
            .map(|i| entry(&format!("old/{i:05}"), false, false, None, 1))
            .collect();
        let (view, cx) = setup(cx, BranchCommand::Checkout, entries);
        let show_all = cx.debug_bounds("git-branch-show-all").unwrap();
        cx.simulate_click(show_all.center(), gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear());
        view.read_with(cx, |view, _| {
            assert_eq!(view.branch_picker.as_ref().unwrap().rows.len(), 10_000)
        });
        assert!(cx.debug_bounds("git-branch-choice-0").is_some());
        assert!(cx.debug_bounds("git-branch-choice-9999").is_none());
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(view.branch_picker.is_none()));
    }

    #[gpui::test]
    fn deleting_a_branch_requires_confirmation_and_a_changed_checkout_rejects_selection(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = setup(
            cx,
            BranchCommand::Delete,
            vec![entry("old", false, false, None, now())],
        );
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, _| {
            assert!(view.branch_picker.is_none());
            let confirmation = view.confirmation.as_ref().unwrap();
            assert!(matches!(&confirmation.command, git::Command::Git(Action::DeleteBranch { name }) if name == "refs/heads/old"));
        });
        view.update(cx, |view, cx| {
            view.confirmation = None;
            cx.notify();
        });
        view.update_in(cx, |view, window, cx| {
            view.open_branch_picker(BranchCommand::Pull, window, cx);
            let picker = view.branch_picker.as_mut().unwrap();
            picker.loading = false;
            picker.error = None;
            picker.entries = vec![entry("origin/main", true, false, None, now())];
            picker.rebuild(cx);
            window.focus(
                &gpui::Focusable::focus_handle(picker.search.read(cx), cx),
                cx,
            );
            view.snapshot.as_mut().unwrap().repositories[0]
                .git
                .as_mut()
                .unwrap()
                .branch = Some("changed".into());
        });
        cx.update(|window, cx| window.draw(cx).clear());
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, _| {
            assert!(view.branch_picker.is_none());
            assert!(!view.busy);
            assert!(view.error.as_deref().unwrap().contains("branch changed"));
        });
    }
}
