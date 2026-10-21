//! Subagent discovery shared by the session lanes and the composer footer.
use chrono::{DateTime, Utc};
use gpui::{
    Context, Entity, EventEmitter, FocusHandle, IntoElement, KeyDownEvent, MouseButton, Render,
    ScrollHandle, SharedString, Subscription, Window, div, prelude::*, px,
};
use zeron_doc::{MessagePart, SubagentStatus};
use zeron_proto::ChatIndicator;

use crate::state::AppState;
use crate::{icons, loaders, popover, theme::Theme, transcript::TranscriptEvent};

/// A spawn chip of the open transcript, as its lane lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubagentRow {
    pub doc_id: String,
    pub title: SharedString,
    pub status: Option<SubagentStatus>,
    pub activity: Option<SharedString>,
    /// When the latest turn that spawned (or steered) it was written — the
    /// closest thing a subagent has to a last-updated time.
    pub spawned_at: DateTime<Utc>,
}

impl SubagentRow {
    /// A settled subagent is frozen: its tab reads the uploaded blob first.
    pub fn frozen(&self) -> bool {
        matches!(
            self.status,
            Some(SubagentStatus::Done) | Some(SubagentStatus::Failed)
        )
    }

    pub(crate) fn indicator(&self) -> ChatIndicator {
        match self.status {
            Some(SubagentStatus::Running) => ChatIndicator::Working,
            Some(SubagentStatus::Done) => ChatIndicator::Completed,
            Some(SubagentStatus::Failed) => ChatIndicator::Errored,
            None => ChatIndicator::Idle,
        }
    }
}

/// The open chat's subagents, one row per subagent doc: running ones first,
/// longest-running leading, then the settled ones most recently updated first
/// (later spawns lead within one turn).
/// Only genuine spawn chips with a stamped doc ref qualify — the chip IS the
/// index (there is no listing endpoint), and a stray ref on a non-Agent tool
/// must not surface as a phantom subagent.
pub(crate) fn subagent_rows(state: &AppState, chat_id: &str) -> Vec<SubagentRow> {
    if state.selected_chat.as_deref() != Some(chat_id) {
        return Vec::new();
    }
    let mut rows: Vec<SubagentRow> = Vec::new();
    for entry in &state.transcript {
        let spawned_at =
            DateTime::<Utc>::from_timestamp_millis(entry.created_at).unwrap_or_else(Utc::now);
        for part in &entry.parts {
            let MessagePart::Tool {
                call,
                subagent_ref: Some(doc_id),
                subagent_status,
                subagent_tail,
                ..
            } = part
            else {
                continue;
            };
            if !call.is_subagent_spawn() {
                continue;
            }
            let row = SubagentRow {
                doc_id: doc_id.clone(),
                title: crate::transcript::subagent_tab_title(call),
                status: *subagent_status,
                activity: subagent_tail
                    .as_ref()
                    .map(|tail| crate::transcript::single_line(tail).into()),
                spawned_at,
            };
            match rows.iter_mut().find(|r| r.doc_id == row.doc_id) {
                // A reopened (steered) subagent updates its row in place.
                Some(existing) => *existing = row,
                None => rows.push(row),
            }
        }
    }
    // Stable sort over the reversed spawn order: ties keep the later spawn
    // on top.
    rows.reverse();
    rows.sort_by_key(|row| std::cmp::Reverse(row.spawned_at));
    // Running subagents lead, longest-running first: one started long ago is
    // buried under everything spawned since, and is the one to find and steer.
    // Oldest-first also keeps the lane still as new subagents join at its
    // foot. The settled tail keeps the newest-first order above.
    let (mut running, settled): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .partition(|row| row.status == Some(SubagentStatus::Running));
    // Ties keep spawn order, like the lane's oldest-first order.
    running.reverse();
    running.sort_by_key(|row| row.spawned_at);
    running.extend(settled);
    running
}

/// The subagent lane: what has not settled rides it, the rest folds away.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SubagentLane {
    /// Running first, then any row whose status is not stamped yet.
    pub active: Vec<SubagentRow>,
    /// Done and failed, most recently updated first.
    pub finished: Vec<SubagentRow>,
}

impl SubagentLane {
    pub fn new(rows: Vec<SubagentRow>) -> Self {
        let (finished, active) = rows.into_iter().partition(SubagentRow::frozen);
        Self { active, finished }
    }

    pub fn is_empty(&self) -> bool {
        self.active.is_empty() && self.finished.is_empty()
    }
}

/// The open chat's active subagents, mounted in its composer footer.
pub(crate) struct Subagents {
    state: Entity<AppState>,
    chat_id: Option<String>,
    popup: popover::Popup<()>,
    selected: Option<String>,
    trigger_focus: FocusHandle,
    menu_focus: FocusHandle,
    composer_focus: FocusHandle,
    scroll: ScrollHandle,
    _observe: Subscription,
}

impl EventEmitter<TranscriptEvent> for Subagents {}

impl Subagents {
    pub(crate) fn new(
        state: Entity<AppState>,
        composer_focus: FocusHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            chat_id: state.read(cx).selected_chat.clone(),
            _observe: cx.observe(&state, |this, _, cx| {
                let chat_id = this.state.read(cx).selected_chat.clone();
                if this.chat_id != chat_id {
                    this.chat_id = chat_id;
                    this.popup = popover::Popup::default();
                    this.selected = None;
                    this.scroll = ScrollHandle::default();
                }
                cx.notify();
            }),
            state,
            popup: popover::Popup::default(),
            selected: None,
            trigger_focus: cx.focus_handle().tab_stop(true),
            menu_focus: cx.focus_handle(),
            composer_focus,
            scroll: ScrollHandle::default(),
        }
    }

    fn rows(&self, cx: &gpui::App) -> Vec<SubagentRow> {
        self.chat_id.as_deref().map_or_else(Vec::new, |chat_id| {
            // Deduplicate and resolve the latest status before filtering: a
            // later completion must retire an earlier running spawn chip.
            SubagentLane::new(subagent_rows(self.state.read(cx), chat_id)).active
        })
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        popover::close_popup(self, cx, |this| &mut this.popup);
    }

    fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.popup.take_press_was_open() || self.popup.is_open() {
            self.dismiss(cx);
            window.focus(&self.trigger_focus, cx);
        } else if let Some(row) = self.rows(cx).first() {
            self.selected = Some(row.doc_id.clone());
            self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
            self.popup.open(());
            window.focus(&self.menu_focus, cx);
            cx.notify();
        }
    }

    fn open_row(&mut self, doc_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.popup.is_open() {
            return;
        }
        // Resolve by document id against live state: a streaming update can
        // reorder rows between paint and click/Enter.
        let Some(row) = self.rows(cx).into_iter().find(|row| row.doc_id == doc_id) else {
            return;
        };
        let Some(chat_id) = self.chat_id.clone() else {
            return;
        };
        self.dismiss(cx);
        window.focus(&self.trigger_focus, cx);
        cx.emit(TranscriptEvent::OpenSubagent {
            chat_id,
            doc_id: row.doc_id.clone(),
            title: row.title.to_string(),
            frozen: row.frozen(),
        });
    }

    fn on_menu_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.popup.is_open() {
            return;
        }
        let rows = self.rows(cx);
        match event.keystroke.key.as_str() {
            "escape" => {
                self.dismiss(cx);
                window.focus(&self.trigger_focus, cx);
            }
            "up" | "down" => {
                let index = self
                    .selected
                    .as_ref()
                    .and_then(|id| rows.iter().position(|row| &row.doc_id == id))
                    .unwrap_or(0);
                let delta = if event.keystroke.key == "up" { -1 } else { 1 };
                if let Some(next) = popover::menu_step(Some(index), rows.len(), delta) {
                    self.selected = rows.get(next).map(|row| row.doc_id.clone());
                    self.scroll.scroll_to_item(next);
                }
                cx.notify();
            }
            "enter" | "space" => {
                if let Some(id) = self.selected.clone() {
                    self.open_row(&id, window, cx);
                }
            }
            "tab" => {
                self.dismiss(cx);
                window.focus(&self.trigger_focus, cx);
                return;
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for Subagents {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows(cx);
        if rows.is_empty() {
            if self.menu_focus.is_focused(window) || self.trigger_focus.is_focused(window) {
                window.focus(&self.composer_focus, cx);
            }
            self.popup = popover::Popup::default();
            self.selected = None;
            return div().into_any_element();
        }
        let theme = Theme::of(cx).clone();
        let running = rows
            .iter()
            .filter(|row| row.status == Some(SubagentStatus::Running))
            .count();
        let label = if running > 0 {
            format!("{running} running")
        } else {
            format!(
                "{} subagent{}",
                rows.len(),
                if rows.len() == 1 { "" } else { "s" }
            )
        };
        let trigger_tone = if running > 0 {
            theme.busy
        } else {
            theme.text_muted
        };
        let mut trigger = div()
            .id("composer-subagents")
            .debug_selector(|| "composer-subagents".into())
            .role(gpui::Role::Button)
            .aria_label(format!("Subagents: {label}"))
            .track_focus(&self.trigger_focus)
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(5.0))
            .px(px(6.0))
            .h(px(24.0))
            .rounded(px(6.0))
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(trigger_tone)
            .cursor_pointer()
            .hover(|style| style.bg(crate::theme::wash(0.08)))
            .when(
                self.popup.is_open() || self.trigger_focus.is_focused(window),
                |el| el.bg(crate::theme::wash(0.08)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.popup.note_trigger_press()),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.toggle(window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space" | "down") {
                    this.toggle(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                icons::icon(icons::BOT)
                    .size(px(13.0))
                    .text_color(trigger_tone),
            )
            .child("Subagents")
            .child(if running > 0 {
                crate::running_pill::running_pill(
                    "composer-subagents-running",
                    running as u32,
                    &theme,
                )
            } else {
                div().child(rows.len().to_string()).into_any_element()
            });
        if self.popup.get().is_some() {
            let theme = theme.for_popup();
            if !rows
                .iter()
                .any(|row| Some(&row.doc_id) == self.selected.as_ref())
            {
                self.selected = rows.first().map(|row| row.doc_id.clone());
            }
            let row_elements = rows
                .into_iter()
                .map(|row| {
                    let key = format!("composer-subagent-{}", row.doc_id);
                    let selected = self.selected.as_ref() == Some(&row.doc_id);
                    let (status, tone) = match row.status {
                        Some(SubagentStatus::Running) => ("Running", theme.busy),
                        Some(SubagentStatus::Done) => ("Completed", theme.text_muted),
                        Some(SubagentStatus::Failed) => ("Failed", theme.danger),
                        None => ("Starting", theme.text_muted),
                    };
                    let mark = if row.status == Some(SubagentStatus::Running) {
                        loaders::mini_glyph_spinner(
                            key.clone(),
                            2.0,
                            theme.glyph,
                            cx.entity_id(),
                            cx,
                        )
                        .into_any_element()
                    } else {
                        icons::icon(if row.status == Some(SubagentStatus::Done) {
                            icons::CHECK
                        } else {
                            icons::BOT
                        })
                        .size(px(13.0))
                        .text_color(tone)
                        .into_any_element()
                    };
                    let title = row.title.clone();
                    popover::menu_row(&theme, selected, key.clone())
                        .id(SharedString::from(key.clone()))
                        .flex_none()
                        .debug_selector(move || key.clone())
                        .role(gpui::Role::Button)
                        .aria_label(format!("Open subagent {title}: {status}"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_row(&row.doc_id, window, cx);
                        }))
                        .child(popover::picker_icon_slot(mark))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(div().whitespace_normal().child(title))
                                .children(row.activity.map(|activity| {
                                    div()
                                        .truncate()
                                        .text_size(crate::typography::ui_rems(11.0))
                                        .text_color(theme.text_muted)
                                        .child(activity)
                                })),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(crate::typography::ui_rems(10.0))
                                .text_color(tone)
                                .child(status),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>();
            let card = popover::popover_card(&theme)
                .id("composer-subagents-menu")
                .debug_selector(|| "composer-subagents-menu".into())
                .w(px(380.0).min(window.viewport_size().width - px(24.0)))
                .flex()
                .flex_col()
                .track_focus(&self.menu_focus)
                .on_key_down(cx.listener(Self::on_menu_key))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss(cx)))
                .child(popover::menu_heading(&theme, "Subagents"))
                .child(
                    div()
                        .id("composer-subagents-list")
                        .debug_selector(|| "composer-subagents-list".into())
                        .max_h((window.viewport_size().height * 0.5).min(px(360.0)))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .flex()
                        .flex_col()
                        .gap(px(popover::MENU_GAP))
                        .children(row_elements),
                );
            trigger = trigger.child(popover::anchored_menu_above(
                "composer-subagents-popover",
                card.into_any_element(),
                self.popup.closing_since(),
            ));
        }
        trigger.into_any_element()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use zeron_doc::{MessageRole, MessageStatus, SessionMessageEntry};

    struct Host {
        menu: Entity<Subagents>,
        composer_focus: FocusHandle,
        opened: Vec<TranscriptEvent>,
        _events: Subscription,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .items_end()
                .track_focus(&self.composer_focus)
                .child(self.menu.clone())
        }
    }

    pub(crate) fn entry(id: &str, status: SubagentStatus) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Tool {
                id: id.into(),
                call: zeron_proto::ToolCall::Unknown {
                    name: format!("Agent: Review {id}"),
                    input: None,
                },
                is_error: false,
                resolved: true,
                output: None,
                diff: None,
                output_ref: None,
                output_bytes: None,
                diff_ref: None,
                diff_stats: None,
                subagent_ref: Some(id.into()),
                subagent_status: Some(status),
                subagent_tail: Some("Inspecting the authentication flow".into()),
            }],
            created_at: 1000,
            device_id: "local".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
            duration_ms: None,
        }
    }

    fn setup(cx: &mut TestAppContext) -> (Entity<Host>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let (host, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.selected_chat = Some("parent".into());
                state.transcript = vec![
                    entry("first", SubagentStatus::Running),
                    entry("second", SubagentStatus::Running),
                    entry("finished", SubagentStatus::Done),
                    entry("failed", SubagentStatus::Failed),
                ];
                state
            });
            let composer_focus = cx.focus_handle();
            let menu = cx.new(|cx| Subagents::new(state, composer_focus.clone(), cx));
            Host {
                _events: cx.subscribe(&menu, |this: &mut Host, _, event, _| {
                    this.opened.push(event.clone());
                }),
                menu,
                composer_focus,
                opened: Vec::new(),
            }
        });
        cx.update(|window, cx| window.draw(cx).clear());
        (host, cx)
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        cx.update(|window, cx| window.draw(cx).clear());
        let position = cx.debug_bounds(selector).unwrap().center();
        cx.simulate_click(position, gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear());
    }

    fn finish_exit(cx: &mut VisualTestContext) {
        let duration = crate::motion::MENU_OUT
            .total()
            .mul_f32(crate::motion::speed_scale());
        std::thread::sleep(duration + std::time::Duration::from_millis(25));
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn footer_subagents_list_only_active_transcripts(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx);
        click(cx, "composer-subagents");
        assert!(cx.debug_bounds("composer-subagent-first").is_some());
        assert!(cx.debug_bounds("composer-subagent-second").is_some());
        assert!(cx.debug_bounds("composer-subagent-finished").is_none());
        assert!(cx.debug_bounds("composer-subagent-failed").is_none());
        click(cx, "composer-subagent-first");
        host.read_with(cx, |host, _| {
            assert!(matches!(&host.opened[..], [TranscriptEvent::OpenSubagent {
                chat_id, doc_id, frozen: false, ..
            }] if chat_id == "parent" && doc_id == "first"));
        });
        finish_exit(cx);
        click(cx, "composer-subagents");
        click(cx, "composer-subagent-second");
        host.read_with(cx, |host, _| {
            assert!(matches!(&host.opened[1], TranscriptEvent::OpenSubagent {
                doc_id, frozen: false, ..
            } if doc_id == "second"));
        });
    }

    #[gpui::test]
    fn footer_subagents_toggle_escape_and_keyboard_navigation(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx);
        let menu = host.read_with(cx, |host, _| host.menu.clone());
        click(cx, "composer-subagents");
        click(cx, "composer-subagents");
        menu.read_with(cx, |menu, _| assert!(!menu.popup.is_open()));
        finish_exit(cx);
        click(cx, "composer-subagents");
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| {
            let menu = menu.read(cx);
            assert!(!menu.popup.is_open());
            assert!(menu.trigger_focus.is_focused(window));
        });
        finish_exit(cx);
        cx.simulate_keystrokes("enter down enter");
        host.read_with(cx, |host, _| {
            assert!(matches!(&host.opened[..], [TranscriptEvent::OpenSubagent {
                doc_id, frozen: false, ..
            }] if doc_id == "second"));
        });
    }

    #[gpui::test]
    fn footer_subagents_follow_status_and_chat_changes(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx);
        let menu = host.read_with(cx, |host, _| host.menu.clone());
        let state = menu.read_with(cx, |menu, _| menu.state.clone());
        click(cx, "composer-subagents");
        // A completion may arrive between paint and click. Its stale row
        // must stop opening a transcript as soon as it settles.
        state.update(cx, |state, cx| {
            state.transcript[0] = entry("first", SubagentStatus::Done);
            cx.notify();
        });
        cx.update(|window, cx| {
            menu.update(cx, |menu, cx| menu.open_row("first", window, cx));
        });
        host.read_with(cx, |host, _| assert!(host.opened.is_empty()));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(cx.debug_bounds("composer-subagent-first").is_none());
        click(cx, "composer-subagent-second");
        host.read_with(cx, |host, _| {
            assert!(matches!(&host.opened[..], [TranscriptEvent::OpenSubagent {
                doc_id, frozen: false, ..
            }] if doc_id == "second"));
        });
        finish_exit(cx);
        click(cx, "composer-subagents");
        state.update(cx, |state, cx| {
            state.selected_chat = Some("other".into());
            state.transcript.clear();
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(cx.debug_bounds("composer-subagents").is_none());
        menu.read_with(cx, |menu, cx| {
            assert!(menu.popup.get().is_none());
            assert!(menu.rows(cx).is_empty());
        });
    }

    #[gpui::test]
    fn footer_subagents_hide_after_completion_and_reappear_on_resume(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx);
        let menu = host.read_with(cx, |host, _| host.menu.clone());
        let state = menu.read_with(cx, |menu, _| menu.state.clone());
        click(cx, "composer-subagents");
        state.update(cx, |state, cx| {
            // Later lifecycle records retire the original running rows.
            state.transcript.extend([
                entry("first", SubagentStatus::Done),
                entry("second", SubagentStatus::Failed),
            ]);
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear();
            assert!(host.read(cx).composer_focus.is_focused(window));
        });
        assert!(cx.debug_bounds("composer-subagents").is_none());
        assert!(cx.debug_bounds("composer-subagents-menu").is_none());
        menu.read_with(cx, |menu, cx| {
            assert!(menu.popup.get().is_none());
            assert!(menu.rows(cx).is_empty());
            // Keep history intact for its existing spawn-chip links.
            let state = menu.state.read(cx);
            assert_eq!(state.transcript.len(), 6);
            assert_eq!(subagent_rows(state, "parent").len(), 4);
        });
        state.update(cx, |state, cx| {
            state
                .transcript
                .push(entry("first", SubagentStatus::Running));
            cx.notify();
        });
        cx.run_until_parked();
        click(cx, "composer-subagents");
        assert!(cx.debug_bounds("composer-subagent-first").is_some());
        assert!(cx.debug_bounds("composer-subagent-second").is_none());
    }

    #[gpui::test]
    fn footer_subagents_scroll_and_fit_a_narrow_window(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx);
        let menu = host.read_with(cx, |host, _| host.menu.clone());
        let state = menu.read_with(cx, |menu, _| menu.state.clone());
        state.update(cx, |state, cx| {
            state.transcript = (0..24)
                .map(|index| entry(&format!("agent-{index}"), SubagentStatus::Running))
                .collect();
            cx.notify();
        });
        cx.simulate_resize(gpui::size(px(300.0), px(400.0)));
        cx.run_until_parked();
        click(cx, "composer-subagents");
        let card = cx.debug_bounds("composer-subagents-menu").unwrap();
        assert!(
            card.left() >= px(0.0) && card.right() <= px(300.0),
            "{card:?}"
        );
        assert!(
            card.top() >= px(0.0) && card.bottom() <= px(400.0),
            "{card:?}"
        );
        let keys = std::iter::repeat_n("down", 23)
            .collect::<Vec<_>>()
            .join(" ");
        cx.simulate_keystrokes(&keys);
        cx.update(|window, cx| window.draw(cx).clear());
        let list = cx.debug_bounds("composer-subagents-list").unwrap();
        let last = cx.debug_bounds("composer-subagent-agent-23").unwrap();
        assert!(last.top() >= list.top() && last.bottom() <= list.bottom());
        cx.simulate_keystrokes("enter");
        host.read_with(cx, |host, _| {
            assert!(matches!(&host.opened[..], [TranscriptEvent::OpenSubagent {
                doc_id, ..
            }] if doc_id == "agent-23"));
        });
    }
}
