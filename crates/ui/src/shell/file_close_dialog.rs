use super::*;
use crate::files::FileCloseChoice;

pub(super) struct FileCloseDialog {
    id: u64,
    pub(super) buttons: [FocusHandle; 3],
    previous_focus: Option<FocusHandle>,
    _key_interceptor: Subscription,
}

impl Shell {
    fn file_needing_close_confirmation(&self, cx: &App) -> Option<u64> {
        self.file_surfaces
            .iter()
            .filter(|(id, files)| {
                (self.pending_exit.is_some()
                    || self.pending_file_closes.contains(&RightSurface::File(**id)))
                    && files.read(cx).close_confirmation_paths().is_some()
            })
            .map(|(id, _)| *id)
            .min_by_key(|id| (self.main_file.as_ref().map(|(_, id)| *id) != Some(*id), *id))
    }

    pub(super) fn sync_file_close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.file_needing_close_confirmation(cx);
        if self.file_close_dialog.as_ref().map(|dialog| dialog.id) == id {
            return;
        }
        if let Some(dialog) = self.file_close_dialog.take()
            && dialog.buttons.iter().any(|focus| focus.is_focused(window))
        {
            window.focus(&self.shortcut_focus, cx);
        }
        if let Some(id) = id {
            // GPUI resolves bound actions before Div key listeners. Intercept
            // only this window while its modal is open, so shortcuts cannot
            // bypass the modal and act on the editor behind it.
            let shell = cx.weak_entity();
            let owner_window = window.window_handle();
            let key_interceptor = cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != owner_window {
                    return;
                }
                let _ = shell.update(cx, |shell, cx| {
                    let event = gpui::KeyDownEvent {
                        keystroke: event.keystroke.clone(),
                        is_held: false,
                        prefer_character_input: false,
                    };
                    if shell.capture_file_close_dialog(&event, window, cx) {
                        cx.stop_propagation();
                    }
                });
            });
            let dialog = FileCloseDialog {
                id,
                buttons: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
                previous_focus: window.focused(cx),
                _key_interceptor: key_interceptor,
            };
            window.focus(&dialog.buttons[2], cx);
            self.file_close_dialog = Some(dialog);
        }
    }

    fn finish_file_close_dialog(
        &mut self,
        choice: FileCloseChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.file_close_dialog.take() else {
            return;
        };
        // Canceling a window/quit request also cancels the close decisions
        // prepared for its other dirty editors.
        let files = if matches!(choice, FileCloseChoice::Cancel) && self.pending_exit.is_some() {
            self.file_surfaces.values().cloned().collect::<Vec<_>>()
        } else {
            self.file_surfaces
                .get(&dialog.id)
                .cloned()
                .into_iter()
                .collect()
        };
        let focus = match choice {
            FileCloseChoice::Cancel => dialog
                .previous_focus
                .as_ref()
                .unwrap_or(&self.shortcut_focus),
            _ => &self.shortcut_focus,
        };
        window.focus(focus, cx);
        for files in files {
            files.update(cx, |files, cx| files.resolve_close_confirmation(choice, cx));
        }
        cx.notify();
    }

    pub(super) fn capture_file_close_dialog(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(dialog) = self.file_close_dialog.as_ref() else {
            return false;
        };
        let modifiers = event.keystroke.modifiers;
        match event.keystroke.key.as_str() {
            "escape" => self.queue_file_close_choice(FileCloseChoice::Cancel, window, cx),
            "tab" if !modifiers.control && !modifiers.alt && !modifiers.platform => {
                let current = dialog
                    .buttons
                    .iter()
                    .position(|focus| focus.is_focused(window))
                    .unwrap_or(2);
                let next = (current + if modifiers.shift { 2 } else { 1 }) % 3;
                window.focus(&dialog.buttons[next], cx);
            }
            "enter" | "space" if !modifiers.control && !modifiers.alt && !modifiers.platform => {
                let choice = match dialog
                    .buttons
                    .iter()
                    .position(|focus| focus.is_focused(window))
                {
                    Some(0) => FileCloseChoice::Cancel,
                    Some(1) => FileCloseChoice::Discard,
                    _ => FileCloseChoice::Save,
                };
                self.queue_file_close_choice(choice, window, cx);
            }
            _ => {}
        }
        // No editor text, shortcuts, or navigation can run beneath the modal.
        true
    }

    fn queue_file_close_choice(
        &self,
        choice: FileCloseChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.file_close_dialog.as_ref().map(|dialog| dialog.id) else {
            return;
        };
        let shell = cx.weak_entity();
        // GPUI still invokes capture listeners after intercepting a key.
        // Keep the modal's blocker in place until that dispatch finishes.
        window.defer(cx, move |window, cx| {
            let _ = shell.update(cx, |shell, cx| {
                if shell.file_close_dialog.as_ref().map(|dialog| dialog.id) == Some(id) {
                    shell.finish_file_close_dialog(choice, window, cx);
                }
            });
        });
    }

    pub(super) fn render_file_close_dialog(
        &self,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.file_close_dialog.as_ref()?;
        let paths = self
            .file_surfaces
            .get(&dialog.id)?
            .read(cx)
            .close_confirmation_paths()?;
        let theme = Theme::of(cx).for_popup();
        let title = if paths.len() == 1 {
            format!(
                "Save changes to “{}”?",
                paths[0].rsplit('/').next().unwrap_or(&paths[0])
            )
        } else {
            format!("Save changes to {} files?", paths.len())
        };
        let card = popover::dialog_card(&theme)
            .id("file-close-dialog-card")
            .debug_selector(|| "file-close-dialog-card".into())
            .role(gpui::Role::Dialog)
            .aria_label(title.clone())
            .w(px(400.0).min(viewport.width - px(32.0)))
            .child(popover::dialog_title(&theme, &title))
            .child(div().mt(px(6.0)).child(popover::dialog_body(
                &theme,
                "Your changes will be lost if you don’t save them.",
            )))
            .child(
                div()
                    .mt(px(20.0))
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "file-close-cancel")
                            .id("file-close-cancel")
                            .debug_selector(|| "file-close-cancel".into())
                            .role(gpui::Role::Button)
                            .track_focus(&dialog.buttons[0])
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .focus_visible(|style| style.border_color(theme.text_muted))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.finish_file_close_dialog(FileCloseChoice::Cancel, window, cx)
                            })),
                    )
                    .child(
                        popover::btn_ghost(&theme, "Don’t Save", "file-close-discard")
                            .id("file-close-discard")
                            .debug_selector(|| "file-close-discard".into())
                            .role(gpui::Role::Button)
                            .track_focus(&dialog.buttons[1])
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .focus_visible(|style| style.border_color(theme.text_muted))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.finish_file_close_dialog(FileCloseChoice::Discard, window, cx)
                            })),
                    )
                    .child(
                        popover::btn_primary(&theme, "Save")
                            .id("file-close-save")
                            .debug_selector(|| "file-close-save".into())
                            .role(gpui::Role::Button)
                            .track_focus(&dialog.buttons[2])
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .focus_visible(|style| style.border_color(theme.text_muted))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.finish_file_close_dialog(FileCloseChoice::Save, window, cx)
                            })),
                    ),
            )
            .into_any_element();
        Some(popover::modal("file-close-dialog", viewport, card))
    }
}
