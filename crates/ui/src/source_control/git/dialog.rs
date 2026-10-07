use super::*;
use gpui::{FocusHandle, Subscription};

pub(crate) struct ConfirmationDialog {
    buttons: [FocusHandle; 2],
    previous_focus: Option<FocusHandle>,
    _key_interceptor: Subscription,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::SaveFile;
    use gpui::{AppContext, TestAppContext, VisualTestContext};

    struct DialogHost {
        view: Entity<SourceControl>,
        focus: FocusHandle,
        background_clicks: usize,
        _observer: Subscription,
    }

    impl Render for DialogHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let modal = self
                .view
                .update(cx, |view, cx| view.render_confirmation(window, cx));
            div()
                .size_full()
                .track_focus(&self.focus)
                // This is the Shell's capture guard: intercepted keys must
                // not get a second dispatch into the background.
                .capture_key_down(cx.listener(|this, _, _, cx| {
                    if this.view.read(cx).has_confirmation() {
                        cx.stop_propagation();
                    }
                }))
                .on_action(|_: &SaveFile, _, _| panic!("background save ran beneath Git modal"))
                .child(
                    div()
                        .id("git-modal-background")
                        .debug_selector(|| "git-modal-background".into())
                        .absolute()
                        .left(px(500.0))
                        .top(px(30.0))
                        .w(px(100.0))
                        .h(px(40.0))
                        .on_click(cx.listener(|this, _, _, _| this.background_clicks += 1)),
                )
                // Source Control lives in a narrow, clipped sidebar. Its
                // deferred modal must still cover the entire window.
                .child(div().w(px(200.0)).h_full().overflow_hidden().child(modal))
        }
    }

    fn setup(cx: &mut TestAppContext) -> (Entity<DialogHost>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::composer::init(cx, Default::default());
            cx.bind_keys([gpui::KeyBinding::new("ctrl-s", SaveFile, None)]);
        });
        let (host, cx) = cx.add_window_view(|window, cx| {
            let state = cx.new(|_| AppState::new());
            let view = cx.new(|cx| {
                let mut view = SourceControl::new(state, cx);
                view.confirmation = Some(Confirmation {
                    target: Target {
                        chat: "chat".into(),
                        cwd: "/checkout".into(),
                        device: None,
                    },
                    repository: String::new(),
                    head: None,
                    branch: None,
                    title: "Discard changes in 2 files?".into(),
                    body: "Unstaged edits will be discarded. Staged changes will be kept.".into(),
                    command: Command::Git(Action::UndoCommit),
                });
                view
            });
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            DialogHost {
                _observer: cx.observe(&view, |_, _, cx| cx.notify()),
                view,
                focus,
                background_clicks: 0,
            }
        });
        cx.simulate_resize(gpui::size(px(640.0), px(600.0)));
        cx.update(|window, cx| window.draw(cx).clear());
        (host, cx)
    }

    #[gpui::test]
    fn git_confirmation_modal_blocks_the_background_and_restores_focus(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx);
        let card = cx.debug_bounds("git-confirm-dialog-card").unwrap();
        assert_eq!(card.center().x, px(320.0));
        // The entrance animation can offset the painted card slightly.
        assert!((f32::from(card.center().y) - 300.0).abs() < 10.0);
        assert!(card.top() >= px(0.0) && card.bottom() <= px(600.0));
        let background = cx.debug_bounds("git-modal-background").unwrap();
        cx.simulate_click(background.center(), gpui::Modifiers::default());
        host.read_with(cx, |host, cx| {
            assert_eq!(host.background_clicks, 0);
            assert!(host.view.read(cx).has_confirmation());
        });
        cx.simulate_keystrokes("ctrl-s");
        for (keys, button) in [("tab", 1), ("tab", 0), ("shift-tab", 1)] {
            cx.simulate_keystrokes(keys);
            cx.update(|window, cx| {
                let view = host.read(cx).view.read(cx);
                assert!(
                    view.confirmation_dialog.as_ref().unwrap().buttons[button].is_focused(window)
                );
            });
        }
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.update(|window, cx| {
            let host = host.read(cx);
            assert!(!host.view.read(cx).has_confirmation());
            assert!(host.focus.is_focused(window));
        });
    }

    #[gpui::test]
    fn git_confirmation_starts_on_cancel_and_clicking_cancel_dismisses_it(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        host.read_with(cx, |host, cx| {
            assert!(!host.view.read(cx).has_confirmation())
        });

        cx.update(|_, cx| {
            let view = host.read(cx).view.clone();
            view.update(cx, |view, cx| {
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
                view.confirm_git(
                    Action::UndoCommit,
                    "Undo the last commit?".into(),
                    "Changes will be kept.".into(),
                    cx,
                );
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        let cancel = cx.debug_bounds("git-confirm-cancel").unwrap();
        cx.simulate_click(cancel.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        host.read_with(cx, |host, cx| {
            assert!(!host.view.read(cx).has_confirmation())
        });
    }
}

impl SourceControl {
    pub(crate) fn has_confirmation(&self) -> bool {
        self.confirmation.is_some()
    }

    fn sync_confirmation_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirmation.is_none() {
            self.restore_confirmation_focus(window, cx);
            return;
        }
        if self.confirmation_dialog.is_some() {
            return;
        }
        // Bound actions run before Div key listeners. Intercept this window's
        // keys while the modal is open so editor and Git shortcuts cannot
        // bypass the question and affect the background.
        let view = cx.weak_entity();
        let owner_window = window.window_handle();
        let interceptor = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() != owner_window {
                return;
            }
            let _ = view.update(cx, |view, cx| {
                if view.capture_confirmation_key(&event.keystroke, window, cx) {
                    cx.stop_propagation();
                }
            });
        });
        let dialog = ConfirmationDialog {
            buttons: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
            previous_focus: window.focused(cx),
            _key_interceptor: interceptor,
        };
        // Destructive actions start with Cancel focused.
        window.focus(&dialog.buttons[0], cx);
        self.confirmation_dialog = Some(dialog);
    }

    fn restore_confirmation_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dialog) = self.confirmation_dialog.take()
            && dialog.buttons.iter().any(|focus| focus.is_focused(window))
        {
            if let Some(focus) = dialog.previous_focus {
                window.focus(&focus, cx);
            } else {
                window.blur();
            }
        }
    }

    fn finish_confirmation(&mut self, accept: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.restore_confirmation_focus(window, cx);
        if accept {
            self.accept_confirmation(cx);
        } else {
            self.confirmation = None;
        }
        cx.notify();
    }

    fn capture_confirmation_key(
        &mut self,
        key: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(dialog) = self
            .confirmation_dialog
            .as_ref()
            .filter(|_| self.has_confirmation())
        else {
            return false;
        };
        let plain = !key.modifiers.control && !key.modifiers.alt && !key.modifiers.platform;
        let accept = match key.key.as_str() {
            "escape" => Some(false),
            "tab" if plain => {
                let next = usize::from(dialog.buttons[0].is_focused(window));
                window.focus(&dialog.buttons[next], cx);
                None
            }
            "enter" | "space" if plain => Some(dialog.buttons[1].is_focused(window)),
            _ => None,
        };
        if let Some(accept) = accept {
            let identity = dialog.buttons[0].clone();
            let view = cx.weak_entity();
            // GPUI still calls capture listeners after interception. Leave
            // the blocker mounted until this key dispatch has finished.
            window.defer(cx, move |window, cx| {
                let _ = view.update(cx, |view, cx| {
                    if view
                        .confirmation_dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.buttons[0] == identity)
                    {
                        view.finish_confirmation(accept, window, cx);
                    }
                });
            });
        }
        true
    }

    pub(crate) fn render_confirmation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.sync_confirmation_dialog(window, cx);
        let Some(confirmation) = &self.confirmation else {
            return gpui::Empty.into_any_element();
        };
        let dialog = self.confirmation_dialog.as_ref().unwrap();
        let viewport = window.viewport_size();
        let theme = Theme::of(cx).for_popup();
        let label = match &confirmation.command {
            Command::Discard(_) => "Discard Changes",
            Command::Git(Action::UndoCommit) => "Undo Commit",
            Command::Git(Action::Abort) => "Abort",
            Command::Git(Action::DropStash { .. }) => "Delete Stash",
            Command::Git(Action::RevertCommit { .. }) => "Revert Commit",
            _ => "Confirm",
        };
        let card = popover::dialog_card(&theme)
            .id("git-confirm-dialog-card")
            .debug_selector(|| "git-confirm-dialog-card".into())
            .role(gpui::Role::Dialog)
            .aria_label(confirmation.title.clone())
            .w(px(440.0).min(viewport.width - px(32.0)))
            .child(popover::dialog_title(&theme, &confirmation.title))
            .child(
                div()
                    .mt(px(6.0))
                    .child(popover::dialog_body(&theme, &confirmation.body)),
            )
            .child(
                div()
                    .mt(px(20.0))
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "git-confirm-cancel")
                            .id("git-confirm-cancel")
                            .debug_selector(|| "git-confirm-cancel".into())
                            .role(gpui::Role::Button)
                            .track_focus(&dialog.buttons[0])
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .focus_visible(|style| style.border_color(theme.text_muted))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.finish_confirmation(false, window, cx)
                            })),
                    )
                    .child(
                        popover::btn_danger(&theme, label)
                            .id("git-confirm-accept")
                            .debug_selector(|| "git-confirm-accept".into())
                            .role(gpui::Role::Button)
                            .track_focus(&dialog.buttons[1])
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .focus_visible(|style| style.border_color(theme.text_muted))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.finish_confirmation(true, window, cx)
                            })),
                    ),
            )
            .into_any_element();
        popover::modal("git-confirm-dialog", viewport, card)
    }
}
