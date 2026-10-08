//! Confirm removal without discarding work or leaving a live session in a deleted checkout.
use super::*;
use std::path::Path;

pub(super) struct Removal {
    path: String,
    name: String,
    discard: bool,
    busy: bool,
    error: Option<String>,
    focus_pending: bool,
    buttons: [FocusHandle; 3],
    previous_focus: Option<FocusHandle>,
}

impl Shell {
    fn worktree_removal_blocker(&self, path: &str, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let device = &self.worktree_settings.editor.as_ref()?.project.device;
        let chats: Vec<_> = state
            .chats
            .iter()
            .filter(|chat| {
                &chat.device_id == device
                    && chat
                        .cwd
                        .as_deref()
                        .is_some_and(|cwd| Path::new(cwd).starts_with(path))
            })
            .collect();
        if chats.iter().any(|chat| {
            matches!(
                state.display_status_for(chat, Utc::now()),
                zeron_proto::ChatIndicator::Working | zeron_proto::ChatIndicator::AwaitingInput
            )
        }) {
            return Some("Stop agents using this worktree before removing it.".into());
        }
        if self.file_surface_keys.iter().any(|((_, chat, _), id)| {
            chats.iter().any(|c| &c.id == chat)
                && self
                    .file_surfaces
                    .get(id)
                    .is_some_and(|file| file.read(cx).has_unsaved_changes())
        }) {
            return Some("Save or close unsaved files in this worktree before removing it.".into());
        }
        None
    }

    pub(super) fn begin_worktree_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.worktree_settings.editor.as_ref().filter(|e| !e.busy) else {
            return;
        };
        let Some(path) = editor
            .checkout
            .clone()
            .filter(|path| path != &editor.project.cwd)
        else {
            return;
        };
        if !editor
            .snapshot
            .as_ref()
            .is_some_and(|s| s.worktrees.iter().any(|tree| tree.path == path))
        {
            return;
        }
        if let Some(error) = self.worktree_removal_blocker(&path, cx) {
            self.worktree_settings.editor.as_mut().unwrap().error = Some(error);
            cx.notify();
            return;
        }
        self.reset_worktree_selects();
        let buttons = std::array::from_fn(|_| cx.focus_handle().tab_stop(true));
        let previous_focus = window.focused(cx);
        window.focus(&buttons[1], cx);
        self.worktree_settings.removal = Some(Removal {
            name: Path::new(&path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path,
            discard: false,
            busy: false,
            error: None,
            focus_pending: true,
            buttons,
            previous_focus,
        });
        cx.notify();
    }

    pub(in crate::shell) fn dismiss_worktree_removal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(removal) = self.worktree_settings.removal.take() else {
            return false;
        };
        if let Some(focus) = removal.previous_focus {
            window.focus(&focus, cx);
        }
        cx.notify();
        true
    }

    fn confirm_worktree_removal(&mut self, cx: &mut Context<Self>) {
        let Some(removal) = self.worktree_settings.removal.as_ref().filter(|r| !r.busy) else {
            return;
        };
        let path = removal.path.clone();
        if let Some(error) = self.worktree_removal_blocker(&path, cx) {
            self.worktree_settings.removal.as_mut().unwrap().error = Some(error);
            cx.notify();
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.worktree_settings.removal.as_mut().unwrap().error =
                Some("Connect to the project's device before removing this worktree.".into());
            cx.notify();
            return;
        };
        let removal = self.worktree_settings.removal.as_mut().unwrap();
        let name = removal.name.clone();
        let editor = self.worktree_settings.editor.as_mut().unwrap();
        let project = editor.project.clone();
        let params = project.params(serde_json::json!({
            "repoPath":project.cwd,"worktreePath":path,"force":removal.discard,
            "deleteBranch":false,"archiveSessions":true,
        }));
        removal.busy = true;
        removal.error = None;
        editor.busy = true;
        editor.error = None;
        let epoch = self.worktree_settings.epoch;
        let toast = crate::toast::PendingToast::start(
            self.toasts.clone(),
            "Removing worktree…".into(),
            name.clone().into(),
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::DELETE_WORKTREE, params).await;
            this.update(cx, |shell, cx| {
                toast.finish(
                    result
                        .as_ref()
                        .map(|_| SharedString::from(format!("{name} removed. Git branch kept.")))
                        .map_err(|error| SharedString::from(error.to_string())),
                    cx,
                );
                if result.is_ok() {
                    let selected_removed =
                        shell
                            .state
                            .read(cx)
                            .selected_chat_row()
                            .is_some_and(|chat| {
                                chat.device_id == project.device
                                    && chat
                                        .cwd
                                        .as_deref()
                                        .is_some_and(|cwd| Path::new(cwd).starts_with(&path))
                            });
                    if selected_removed {
                        shell.open_new_session(Some(project.space.clone()), cx);
                    }
                }
                if epoch != shell.worktree_settings.epoch {
                    return;
                }
                let Some(editor) = shell.worktree_settings.editor.as_mut() else {
                    return;
                };
                editor.busy = false;
                match result {
                    Ok(_) => {
                        shell.worktree_settings.removal = None;
                        shell.select_worktree_settings(None, cx);
                        if let Some(editor) = shell.worktree_settings.editor.as_mut() {
                            editor.drafts.remove(&Some(path));
                        }
                    }
                    Err(error) => {
                        if let Some(removal) = shell.worktree_settings.removal.as_mut() {
                            removal.busy = false;
                            removal.error = Some(error.to_string());
                        } else {
                            editor.error = Some(error.to_string());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn worktree_removal_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(removal) = self.worktree_settings.removal.as_ref() else {
            return;
        };
        let active = removal
            .buttons
            .iter()
            .position(|button| button.is_focused(window))
            .unwrap_or(1);
        match event.keystroke.key.as_str() {
            "tab" => {
                let next = (active
                    + if event.keystroke.modifiers.shift {
                        2
                    } else {
                        1
                    })
                    % 3;
                window.focus(&removal.buttons[next], cx);
            }
            "escape" => {
                self.dismiss_worktree_removal(window, cx);
            }
            "enter" | "space" => match active {
                0 if !removal.busy => {
                    let removal = self.worktree_settings.removal.as_mut().unwrap();
                    removal.discard = !removal.discard;
                    cx.notify();
                }
                1 => {
                    self.dismiss_worktree_removal(window, cx);
                }
                2 if !removal.busy => self.confirm_worktree_removal(cx),
                _ => {}
            },
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(in crate::shell) fn render_worktree_removal(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let removal = self.worktree_settings.removal.as_mut()?;
        if std::mem::take(&mut removal.focus_pending) {
            window.focus(&removal.buttons[1], cx);
        }
        let removal = self.worktree_settings.removal.as_ref()?;
        let theme = Theme::of(cx).for_popup();
        let busy = removal.busy;
        let card = popover::dialog_card(&theme)
            .w(px(460.0)).max_w((viewport.width - px(40.0)).max(px(200.0)))
            .on_key_down(cx.listener(Self::worktree_removal_key))
            .child(popover::dialog_title(&theme, if busy { "Removing worktree…" } else { "Remove worktree?" }))
            .child(div().mt(px(8.0)).child(popover::dialog_body(&theme, format!(
                "\u{201C}{}\u{201D} and its files will be removed. Services stop first, then your saved cleanup workflow runs. The Git branch is kept. Sessions using this worktree are archived; their history stays available.", removal.name))))
            .child(div().mt(px(10.0)).text_size(px(11.0)).text_color(theme.text_muted)
                .child(SharedString::from(removal.path.clone())))
            .child(div().id("worktree-removal-discard").debug_selector(|| "worktree-removal-discard".into())
                .track_focus(&removal.buttons[0]).role(gpui::Role::CheckBox).aria_label("Discard uncommitted changes")
                .flex().items_center().gap(px(10.0)).mt(px(16.0)).cursor_pointer()
                .when(busy, |b| b.opacity(0.5).cursor_default())
                .when(!busy, |b| b.on_click(cx.listener(|this, _, _, cx| {
                    if let Some(removal) = this.worktree_settings.removal.as_mut() {
                        removal.discard = !removal.discard;
                    }
                    cx.notify();
                })))
                .child(widgets::toggle_switch(&theme, removal.discard, "discard-worktree-changes"))
                .child(popover::dialog_body(&theme, "Discard uncommitted changes")))
            .when_some(removal.error.clone(), |card, error| card.child(
                div().mt(px(12.0)).text_size(px(12.0)).text_color(theme.danger).child(SharedString::from(error)),
            ))
            .child(div().mt(px(20.0)).flex().justify_end().gap(px(8.0))
                .child(popover::btn_ghost(&theme, if busy { "Close" } else { "Cancel" }, "cancel-worktree-removal")
                    .id("cancel-worktree-removal").debug_selector(|| "cancel-worktree-removal".into())
                    .track_focus(&removal.buttons[1]).role(gpui::Role::Button)
                    .on_click(cx.listener(|this, _, window, cx| { this.dismiss_worktree_removal(window, cx); })))
                .child(popover::btn_danger(&theme, if busy { "Removing…" } else { "Remove worktree" })
                    .id("confirm-worktree-removal").debug_selector(|| "confirm-worktree-removal".into())
                    .track_focus(&removal.buttons[2]).role(gpui::Role::Button)
                    .when(busy, |b| b.opacity(0.5).cursor_default())
                    .when(!busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.confirm_worktree_removal(cx))))));
        Some(popover::modal(
            "remove-worktree-dialog",
            viewport,
            card.into_any_element(),
        ))
    }
}
