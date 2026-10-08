//! VS Code's stash menu invokes commands, then a quick picker. Never lay out
//! every stash description in the menu: a large subject can stall native input.
use super::*;
use gpui::{FocusHandle, Subscription, UniformListScrollHandle};
use zeron_proto::{RepositoryGitAction as Action, RepositoryGitStash};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum StashCommand {
    Apply,
    Pop,
    Drop,
    Restore,
}

impl StashCommand {
    fn title(self) -> &'static str {
        match self {
            Self::Apply => "Pick a stash to apply",
            Self::Pop => "Pick a stash to pop",
            Self::Drop => "Pick a stash to drop",
            Self::Restore => "Pick a stash to apply and restore staging",
        }
    }
    fn action(self, sha: String) -> Action {
        match self {
            Self::Apply | Self::Restore => Action::ApplyStash {
                sha,
                reinstate_staged: self == Self::Restore,
            },
            Self::Pop => Action::PopStash {
                sha,
                reinstate_staged: false,
            },
            Self::Drop => Action::DropStash { sha },
        }
    }
}

pub(super) struct StashPicker {
    command: StashCommand,
    search: Entity<ComposerInput>,
    previous_focus: Option<FocusHandle>,
    entries: Vec<RepositoryGitStash>,
    active: usize,
    scroll: UniformListScrollHandle,
    loading: bool,
    error: Option<SharedString>,
    _events: Subscription,
    _load: Option<Task<()>>,
}

// Text truncation after shaping still shapes the original text. Bound the
// string BEFORE constructing native labels; preserve Unicode boundaries.
fn summary(text: &str) -> String {
    let mut chars = text.chars().map(|c| if c.is_control() { ' ' } else { c });
    let mut text: String = chars.by_ref().take(256).collect();
    if chars.next().is_some() {
        text.push('…');
    }
    text
}

fn matching(entries: &[RepositoryGitStash], query: &str) -> Vec<usize> {
    let words: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return (0..entries.len()).collect();
    }
    entries
        .iter()
        .enumerate()
        .filter_map(|(i, stash)| {
            let text = format!("{} {}", stash.selector, stash.subject).to_lowercase();
            words.iter().all(|word| text.contains(word)).then_some(i)
        })
        .collect()
}

impl StashPicker {
    #[cfg(feature = "source-control-fixture")]
    pub(super) fn fixture_state(&self, cx: &App) -> serde_json::Value {
        serde_json::json!({ "command": format!("{:?}", self.command), "loading": self.loading,
            "count": self.entries.len(), "active": self.active, "error": self.error,
            "visibleCount": matching(&self.entries, self.search.read(cx).text()).len() })
    }
}

impl SourceControl {
    pub(super) fn open_stash_picker(
        &mut self,
        command: StashCommand,
        latest: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.git_enabled() {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let repository = self.active_repository.clone();
        let search = cx.new(|cx| {
            ComposerInput::with_context(command.title(), "PaletteSearch", cx)
                .with_single_line()
                .with_text_metrics(13.0, 20.0)
        });
        let events = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited)
                && let Some(picker) = this.stash_picker.as_mut()
            {
                picker.active = 0;
                picker
                    .scroll
                    .scroll_to_item(0, gpui::ScrollStrategy::Nearest);
                cx.notify();
            }
        });
        let identity = search.entity_id();
        self.close_actions_menu(cx);
        self.close_commit_menu(cx);
        self.stash_picker = Some(StashPicker {
            command,
            search: search.clone(),
            previous_focus: window.focused(cx),
            entries: Vec::new(),
            active: 0,
            scroll: UniformListScrollHandle::new(),
            loading: true,
            error: None,
            _events: events,
            _load: None,
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = engine.client().call_as::<Vec<RepositoryGitStash>>(methods::GET_CHECKOUT_STASHES,
                serde_json::json!({ "cwd": target.cwd, "targetDeviceId": target.device, "repository": repository })).await;
            this.update_in(cx, |this, window, cx| {
                if this.target.as_ref() != Some(&target) || this.active_repository != repository { return; }
                let Some(picker) = this.stash_picker.as_mut().filter(|picker| picker.search.entity_id() == identity) else { return; };
                picker.loading = false;
                match result {
                    Ok(entries) => {
                        picker.entries = entries.into_iter().map(|mut stash| { stash.subject = summary(&stash.subject); stash }).collect();
                        if latest {
                            if picker.entries.is_empty() {
                                this.close_stash_picker(window, cx);
                                this.notify_success(&repository, "There are no stashes in this repository", cx);
                            } else { this.activate_stash(0, window, cx); }
                        }
                    }
                    Err(error) => picker.error = Some(format!("Unable to load stashes: {error}").into()),
                }
                cx.notify();
            }).ok();
        });
        self.stash_picker.as_mut().unwrap()._load = Some(task);
        let owner = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            if owner.upgrade().is_some_and(|view| {
                view.read(cx)
                    .stash_picker
                    .as_ref()
                    .is_some_and(|picker| picker.search.entity_id() == identity)
            }) {
                window.focus(&gpui::Focusable::focus_handle(search.read(cx), cx), cx);
            }
        });
        cx.notify();
    }

    fn close_stash_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(picker) = self.stash_picker.take() {
            if let Some(focus) = picker.previous_focus {
                window.focus(&focus, cx);
            }
            cx.notify();
        }
    }

    fn activate_stash(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.git_enabled() {
            return;
        }
        let Some(picker) = &self.stash_picker else {
            return;
        };
        let Some(stash) = picker.entries.get(index).cloned() else {
            return;
        };
        let command = picker.command;
        self.close_stash_picker(window, cx);
        if command == StashCommand::Drop {
            self.confirm_git(
                command.action(stash.sha),
                "Drop stash?".into(),
                format!(
                    "Drop {}: {}? This removes the saved stash without applying it.",
                    stash.selector, stash.subject
                ),
                cx,
            );
        } else {
            self.run_git(command.action(stash.sha), cx);
        }
    }

    fn stash_picker_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.stash_picker.as_mut() else {
            return;
        };
        let rows = matching(&picker.entries, picker.search.read(cx).text());
        match event.keystroke.key.as_str() {
            "escape" => self.close_stash_picker(window, cx),
            "up" | "down" => {
                if !rows.is_empty() {
                    picker.active = if event.keystroke.key == "down" {
                        (picker.active + 1) % rows.len()
                    } else {
                        (picker.active + rows.len() - 1) % rows.len()
                    };
                    picker
                        .scroll
                        .scroll_to_item(picker.active, gpui::ScrollStrategy::Nearest);
                    cx.notify();
                }
            }
            "enter" => {
                if !event.is_held
                    && let Some(&index) = rows.get(picker.active)
                {
                    self.activate_stash(index, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(super) fn render_stash_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(picker) = &self.stash_picker else {
            return gpui::Empty.into_any_element();
        };
        let theme = Theme::of(cx).for_popup();
        let viewport = window.viewport_size();
        let width = px(560.0).min((viewport.width - px(32.0)).max(px(1.0)));
        let rows = matching(&picker.entries, picker.search.read(cx).text());
        let count = rows.len();
        let list_height =
            px((count.min(8).max(1) * 32) as f32).min((viewport.height - px(130.0)).max(px(1.0)));
        let active = picker.active;
        let scroll = picker.scroll.clone();
        let search = picker.search.clone();
        let title = picker.command.title();
        let empty = if let Some(error) = &picker.error {
            error.clone()
        } else if picker.loading {
            "Loading stashes…".into()
        } else if picker.entries.is_empty() {
            "This repository has no stashes".into()
        } else {
            "No matching stashes".into()
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
            uniform_list("git-stash-picker-list", count, move |range, _, cx| {
                entity.update(cx, |this, cx| {
                    range
                        .filter_map(|i| {
                            let index = *rows.get(i)?;
                            let stash = this.stash_picker.as_ref()?.entries.get(index)?;
                            Some(
                                div()
                                    .id(("git-stash-choice", i))
                                    .debug_selector(move || format!("git-stash-choice-{i}"))
                                    .h(px(32.0))
                                    .w_full()
                                    .min_w_0()
                                    .px(px(10.0))
                                    .flex()
                                    .items_center()
                                    .gap(px(10.0))
                                    .text_size(px(13.0))
                                    .text_color(theme.text)
                                    .cursor_pointer()
                                    .when(i == active, |row| row.bg(theme.accent.opacity(0.15)))
                                    .hover(|row| row.bg(theme.glass_hover()))
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_color(theme.text_muted)
                                            .child(stash.selector.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(stash.subject.clone()),
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.activate_stash(index, window, cx)
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
            .id("git-stash-picker")
            .debug_selector(|| "git-stash-picker".into())
            .w(width)
            .rounded(px(8.0))
            .role(gpui::Role::Dialog)
            .aria_label(title)
            .flex()
            .flex_col()
            .overflow_hidden()
            .capture_key_down(cx.listener(Self::stash_picker_key))
            .on_mouse_down_out(
                cx.listener(|this, _, window, cx| this.close_stash_picker(window, cx)),
            )
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .p(px(6.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(popover::search_input_frame(
                        &theme,
                        search.into_any_element(),
                    )),
            )
            .child(body);
        gpui::deferred(
            gpui::anchored()
                .position(gpui::point(
                    (viewport.width - width) / 2.0,
                    px(Theme::TITLEBAR_HEIGHT + 12.0),
                ))
                .child(crate::frost::frosted(
                    8.0,
                    crate::frost::MENU_BLUR,
                    crate::motion::menu_in("git-stash-picker-in", div().occlude().child(card)),
                )),
        )
        .priority(2)
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    struct PickerHost {
        view: Entity<SourceControl>,
        _observer: Subscription,
    }
    impl Render for PickerHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.view
                .update(cx, |view, cx| view.render_stash_picker(window, cx))
        }
    }

    #[gpui::test]
    fn large_stash_picker_virtualizes_rows_and_escape_closes_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::composer::init(cx, Default::default());
        });
        let (host, cx) = cx.add_window_view(|window, cx| {
            let state = cx.new(|_| AppState::new());
            let view = cx.new(|cx| {
                let mut view = SourceControl::new(state, cx);
                let search = cx.new(|cx| {
                    ComposerInput::with_context("Pick a stash to apply", "PaletteSearch", cx)
                        .with_single_line()
                });
                let events = cx.subscribe(&search, |_, _, _: &ComposerInputEvent, _| {});
                view.stash_picker = Some(StashPicker {
                    command: StashCommand::Apply,
                    search,
                    previous_focus: None,
                    active: 0,
                    loading: false,
                    error: None,
                    scroll: UniformListScrollHandle::new(),
                    _events: events,
                    _load: None,
                    entries: (0..10_000)
                        .map(|i| RepositoryGitStash {
                            sha: format!("{i:040x}"),
                            selector: format!("stash@{{{i}}}"),
                            subject: summary(&"🦀".repeat(300)),
                        })
                        .collect(),
                });
                view
            });
            let input = view.read(cx).stash_picker.as_ref().unwrap().search.clone();
            window.focus(&gpui::Focusable::focus_handle(input.read(cx), cx), cx);
            PickerHost {
                _observer: cx.observe(&view, |_, _, cx| cx.notify()),
                view,
            }
        });
        cx.simulate_resize(gpui::size(px(800.0), px(600.0)));
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(cx.debug_bounds("git-stash-choice-0").is_some());
        assert!(cx.debug_bounds("git-stash-choice-9999").is_none());
        cx.simulate_keystrokes("down");
        let view = host.read_with(cx, |host, _| host.view.clone());
        view.read_with(cx, |view, _| {
            assert_eq!(view.stash_picker.as_ref().unwrap().active, 1)
        });
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(view.stash_picker.is_none()));
    }
    #[test]
    fn stash_display_bounds_large_unicode_subjects_before_layout() {
        let text = summary(&"🦀".repeat(300_000));
        assert_eq!(text.chars().count(), 257);
        assert!(text.ends_with('…'));
        assert_eq!(summary("short\nsubject"), "short subject");
    }
    #[test]
    fn stash_picker_matches_selector_and_subject_words() {
        let entries = vec![RepositoryGitStash {
            sha: "a".into(),
            selector: "stash@{3}".into(),
            subject: "On main: Backend Fix".into(),
        }];
        assert_eq!(matching(&entries, "FIX backend"), vec![0]);
        assert_eq!(matching(&entries, "stash@{3}"), vec![0]);
        assert!(matching(&entries, "frontend").is_empty());
    }
}
