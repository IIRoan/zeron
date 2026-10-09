use super::*;
use crate::release_notes::ReleaseNotes;

pub(super) struct ReleaseNotesDialog {
    pub(super) selected: usize,
    pub(super) buttons: [FocusHandle; 5],
    previous_focus: Option<FocusHandle>,
    scroll: gpui::ScrollHandle,
    cache: std::rc::Rc<std::cell::RefCell<crate::markdown::render::RenderCache>>,
    _key_interceptor: Subscription,
}

impl Shell {
    pub(super) fn sync_release_notes_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A startup summary waits for the workspace and any existing modal.
        // It must never steal a save/discard decision or a project setup form.
        if self.release_notes_dialog.is_some()
            || self.file_close_dialog.is_some()
            || self.pending_exit.is_some()
            || self.worktree_settings.editor.is_some()
            || self.project_terminals.editor.is_some()
            || self.command_palette.is_some()
            || self.add_space.is_some()
            || self.delete_confirm.is_some()
            || self.delete_space_confirm.is_some()
            || self.discard_working_tree.is_some()
            || self.sync_flow.has_visible_overlay()
            || crate::app_update::AppUpdate::global(cx)
                .is_some_and(|update| update.read(cx).prompt().is_some())
            || self
                .active_changes(cx)
                .is_some_and(|view| view.read(cx).has_git_confirmation(cx))
        {
            return;
        }
        let Some(notes) = ReleaseNotes::global(cx) else {
            return;
        };
        let allow_automatic = matches!(self.route, Route::Chat) && !self.voice.read(cx).stage_open;
        if !notes.update(cx, |notes, _| notes.claim_open(allow_automatic)) {
            return;
        }
        let owner = window.window_handle();
        let shell = cx.weak_entity();
        let key_interceptor = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() != owner {
                return;
            }
            let _ = shell.update(cx, |shell, cx| {
                if shell.capture_release_notes_key(&event.keystroke, window, cx) {
                    cx.stop_propagation();
                }
            });
        });
        let dialog = ReleaseNotesDialog {
            selected: 0,
            buttons: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
            previous_focus: window.focused(cx),
            scroll: gpui::ScrollHandle::new(),
            cache: Default::default(),
            _key_interceptor: key_interceptor,
        };
        self.composer
            .update(cx, |composer, _| composer.focus_pending = false);
        window.focus(&dialog.buttons[4], cx);
        self.release_notes_dialog = Some(dialog);
    }

    fn close_release_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.release_notes_dialog.take() else {
            return;
        };
        if let Some(notes) = ReleaseNotes::global(cx) {
            notes.update(cx, |notes, cx| notes.dismiss(cx));
        }
        window.focus(
            dialog
                .previous_focus
                .as_ref()
                .unwrap_or(&self.shortcut_focus),
            cx,
        );
        cx.notify();
    }

    fn select_release_notes(&mut self, selected: usize, cx: &mut Context<Self>) {
        if let Some(dialog) = self.release_notes_dialog.as_mut() {
            dialog.selected = selected;
            dialog.cache = Default::default();
            dialog.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
            cx.notify();
        }
    }

    fn release_notes_url(&self, cx: &App) -> Option<String> {
        let selected = self.release_notes_dialog.as_ref()?.selected;
        let notes = ReleaseNotes::global(cx)?;
        let notes = notes.read(cx);
        let catalog = notes.catalog.as_ref()?;
        let index = catalog.current_index(zeron_update::current_version())?;
        catalog
            .releases
            .get(index + selected)
            .map(|entry| entry.release.upstream_url.clone())
    }

    fn capture_release_notes_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(dialog) = self.release_notes_dialog.as_ref() else {
            return false;
        };
        if self.file_close_dialog.is_some() {
            return false;
        }
        let mut buttons = vec![0];
        if let Some(notes) = ReleaseNotes::global(cx) {
            let notes = notes.read(cx);
            if notes.catalog.as_ref().is_some_and(|catalog| {
                catalog
                    .current_index(zeron_update::current_version())
                    .is_some_and(|index| catalog.releases.get(index + 1).is_some())
            }) {
                buttons.push(1);
            }
        }
        buttons.push(2);
        if self.release_notes_url(cx).is_some() {
            buttons.push(3);
        }
        buttons.push(4);
        let focused = buttons
            .iter()
            .position(|index| dialog.buttons[*index].is_focused(window))
            .unwrap_or(buttons.len() - 1);
        let plain = !key.modifiers.control && !key.modifiers.alt && !key.modifiers.platform;
        if key.key == "tab" && plain {
            let next = if key.modifiers.shift {
                (focused + buttons.len() - 1) % buttons.len()
            } else {
                (focused + 1) % buttons.len()
            };
            window.focus(&dialog.buttons[buttons[next]], cx);
        } else if key.key == "escape" || (plain && matches!(key.key.as_str(), "enter" | "space")) {
            let action = if key.key == "escape" {
                4
            } else {
                buttons[focused]
            };
            let shell = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                let _ = shell.update(cx, |shell, cx| {
                    if shell.release_notes_dialog.is_none() {
                        return;
                    }
                    match action {
                        0 | 1 => shell.select_release_notes(action, cx),
                        2 => {
                            if let Some(notes) = ReleaseNotes::global(cx) {
                                notes.update(cx, |notes, cx| notes.toggle_automatic(cx));
                            }
                        }
                        3 => {
                            if let Some(url) = shell.release_notes_url(cx) {
                                cx.open_url(&url);
                            }
                        }
                        _ => shell.close_release_notes(window, cx),
                    }
                });
            });
        }
        // GPUI resolves bound actions before ordinary key handlers. This
        // interceptor prevents save/send/navigation shortcuts reaching the
        // editor or agent beneath the modal, including while closing it.
        true
    }

    pub(super) fn render_release_notes_dialog(
        &self,
        viewport: gpui::Size<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.release_notes_dialog.as_ref()?;
        let notes = ReleaseNotes::global(cx)?;
        let notes = notes.read(cx);
        let theme = Theme::of(cx).for_popup();
        let catalog = notes.catalog.as_ref();
        let current =
            catalog.and_then(|catalog| catalog.current_index(zeron_update::current_version()));
        let previous = current.and_then(|index| catalog?.releases.get(index + 1));
        let entry = current.and_then(|index| catalog?.releases.get(index + dialog.selected));
        let mut tabs = div().flex().gap(px(8.0)).flex_wrap().mt(px(16.0));
        for (index, label) in [
            (
                0,
                format!("This update · v{}", zeron_update::current_version()),
            ),
            (
                1,
                previous
                    .map(|entry| format!("Previous release · v{}", entry.release.version))
                    .unwrap_or_default(),
            ),
        ] {
            if label.is_empty() {
                continue;
            }
            tabs = tabs.child(
                popover::btn_ghost(&theme, &label, format!("release-notes-tab-{index}"))
                    .id(("release-notes-tab", index))
                    .debug_selector(move || format!("release-notes-tab-{index}"))
                    .role(gpui::Role::Button)
                    .aria_label(label)
                    .track_focus(&dialog.buttons[index])
                    .when(dialog.selected == index, |tab| {
                        tab.bg(theme.glass_hover()).text_color(theme.text)
                    })
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select_release_notes(index, cx)),
                    ),
            );
        }
        let mut body = div()
            .id("release-notes-scroll")
            .debug_selector(|| "release-notes-scroll".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&dialog.scroll)
            .mt(px(16.0))
            .pr(px(8.0));
        if let Some(entry) = entry {
            let release = &entry.release;
            if dialog.selected == 0 || !release.fork_changes.is_empty() {
                body = body.child(section_heading(&theme, "In this Linux fork"));
                if release.fork_changes.is_empty() {
                    body = body.child(popover::dialog_body(
                        &theme,
                        "No additional fork changes are recorded for this release.",
                    ));
                } else {
                    body = body.child(div().flex().flex_col().gap(px(8.0)).children(
                        release.fork_changes.iter().map(|change| {
                            div()
                                .flex()
                                .gap(px(8.0))
                                .text_size(crate::typography::ui_rems(13.0))
                                .line_height(px(20.0))
                                .child(div().flex_none().text_color(theme.accent).child("•"))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(SharedString::from(change.clone())),
                                )
                        }),
                    ));
                }
            }
            body = body.child(section_heading(
                &theme,
                &format!("Upstream Zeron · v{}", release.version),
            ));
            if release.notes_source == "git_history" {
                body = body.child(div().mb(px(10.0)).child(popover::dialog_body(&theme,
                    "Release notes were unavailable when this build was prepared. These changes come from upstream’s Git history.")));
            }
            let opts = crate::markdown::render::RenderOptions {
                tasks: None,
                media: None,
                row_key: format!("release-notes-{}", release.version).into(),
                veil: None,
                cache: Some(dialog.cache.clone()),
                now: std::time::Instant::now(),
                copy: None,
                link: None,
                workspace_root: None,
                code: None,
            };
            body = body.child(crate::markdown::render::render_tree(
                &entry.upstream,
                &opts,
                &theme,
                window,
                &|_| None,
            ));
        } else {
            body = body.child(popover::dialog_body(&theme,
                "Release notes weren’t bundled for this build. Your app and project data are unchanged."));
        }
        let footer = div()
            .mt(px(16.0))
            .pt(px(12.0))
            .border_t_1()
            .border_color(crate::theme::hairline(0.10))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .flex_wrap()
            .child(
                popover::btn_ghost(&theme, "Show after updates", "release-notes-auto")
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .id("release-notes-auto")
                    .debug_selector(|| "release-notes-auto".into())
                    .role(gpui::Role::Button)
                    .aria_label(format!(
                        "Show release notes after updates: {}",
                        if notes.state.show_after_updates {
                            "on"
                        } else {
                            "off"
                        }
                    ))
                    .track_focus(&dialog.buttons[2])
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .on_click(cx.listener(|_, _, _, cx| {
                        if let Some(notes) = ReleaseNotes::global(cx) {
                            notes.update(cx, |notes, cx| notes.toggle_automatic(cx));
                        }
                    }))
                    .child(
                        div()
                            .size(px(14.0))
                            .rounded(px(3.0))
                            .border_1()
                            .border_color(theme.text_muted)
                            .when(notes.state.show_after_updates, |check| {
                                check
                                    .bg(theme.accent)
                                    .border_color(theme.accent)
                                    .child(icon(icons::CHECK).size(px(12.0)).text_color(theme.bg))
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .when_some(entry, |row, entry| {
                        let url = entry.release.upstream_url.clone();
                        row.child(
                            popover::btn_ghost(
                                &theme,
                                "Upstream release",
                                "release-notes-upstream",
                            )
                            .id("release-notes-upstream")
                            .role(gpui::Role::Button)
                            .track_focus(&dialog.buttons[3])
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    })
                    .child(
                        popover::btn_primary(&theme, "Got it")
                            .id("release-notes-close")
                            .debug_selector(|| "release-notes-close".into())
                            .role(gpui::Role::Button)
                            .track_focus(&dialog.buttons[4])
                            .focus_visible(|style| style.border_1().border_color(theme.text))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_release_notes(window, cx)
                            })),
                    ),
            );
        let card = popover::dialog_card(&theme)
            .id("release-notes-card")
            .debug_selector(|| "release-notes-card".into())
            .role(gpui::Role::Dialog)
            .aria_label("What’s new")
            .w(px(760.0).min((viewport.width - px(32.0)).max(px(0.0))))
            .h(px(680.0).min((viewport.height - px(32.0)).max(px(0.0))))
            .child(popover::dialog_title(&theme, "What’s new"))
            .child(div().mt(px(6.0)).child(popover::dialog_body(
                &theme,
                "Your Linux fork, with notes from the upstream releases it includes.",
            )))
            .child(tabs)
            .child(body)
            .child(footer)
            .into_any_element();
        Some(popover::modal("release-notes-dialog", viewport, card))
    }
}

fn section_heading(theme: &Theme, title: &str) -> gpui::Div {
    div()
        .mt(px(12.0))
        .mb(px(10.0))
        .text_size(crate::typography::ui_rems(13.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text)
        .child(SharedString::from(title.to_string()))
}
