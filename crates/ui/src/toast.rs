//! Nonmodal, window-local feedback. Only the card intercepts mouse input.
use crate::{icons, motion, popover, theme::Theme};
use gpui::{
    AnyElement, AppContext, Context, Entity, SharedString, Task, Window, div, prelude::*, px,
};
use std::time::Duration;

const DISPLAY_TIME: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Progress,
    Success,
    Error,
}

#[derive(Clone)]
struct Message {
    title: SharedString,
    body: SharedString,
    kind: Kind,
}

/// Kept by the operation, so its feedback can finish after the originating
/// tool tab is closed. An older operation cannot replace a newer notification.
pub(crate) struct PendingToast {
    toasts: Entity<Toasts>,
    sequence: u64,
}

impl PendingToast {
    pub(crate) fn start(
        toasts: Entity<Toasts>,
        title: SharedString,
        body: SharedString,
        cx: &mut impl AppContext,
    ) -> Self {
        let sequence = toasts.update(cx, |toasts, cx| toasts.progress(title, body, cx));
        Self { toasts, sequence }
    }

    pub(crate) fn finish(
        self,
        result: Result<SharedString, SharedString>,
        cx: &mut impl AppContext,
    ) {
        self.toasts
            .update(cx, |toasts, cx| toasts.finish(self.sequence, result, cx));
    }

    pub(crate) fn dismiss(self, cx: &mut impl AppContext) {
        self.toasts.update(cx, |toasts, cx| {
            if toasts.sequence == self.sequence {
                toasts.dismiss(cx);
            }
        });
    }
}

#[derive(Default)]
pub(crate) struct Toasts {
    popup: popover::Popup<Message>,
    sequence: u64,
    expiry: Option<Task<()>>,
    hovered: bool,
}

impl Toasts {
    pub(crate) fn success(
        &mut self,
        title: SharedString,
        body: SharedString,
        cx: &mut Context<Self>,
    ) {
        self.show(title, body, Kind::Success, cx);
    }

    pub(crate) fn error(
        &mut self,
        title: SharedString,
        body: SharedString,
        cx: &mut Context<Self>,
    ) {
        self.show(title, body, Kind::Error, cx);
    }

    fn show(
        &mut self,
        title: SharedString,
        body: SharedString,
        kind: Kind,
        cx: &mut Context<Self>,
    ) {
        self.expiry = None;
        self.sequence = self.sequence.wrapping_add(1);
        self.popup.open(Message { title, body, kind });
        self.schedule_expiry(cx);
        cx.notify();
    }

    fn progress(&mut self, title: SharedString, body: SharedString, cx: &mut Context<Self>) -> u64 {
        self.expiry = None;
        self.sequence = self.sequence.wrapping_add(1);
        self.popup.open(Message {
            title,
            body,
            kind: Kind::Progress,
        });
        cx.notify();
        self.sequence
    }

    fn finish(
        &mut self,
        sequence: u64,
        result: Result<SharedString, SharedString>,
        cx: &mut Context<Self>,
    ) {
        if self.sequence != sequence {
            return;
        }
        let Some(message) = self.popup.open_mut() else {
            return;
        };
        if message.kind != Kind::Progress {
            return;
        }
        // Keep the same card and entrance animation when the result arrives.
        match result {
            Ok(body) => {
                message.body = body;
                message.kind = Kind::Success;
            }
            Err(body) => {
                message.body = body;
                message.kind = Kind::Error;
            }
        }
        self.schedule_expiry(cx);
        cx.notify();
    }

    fn schedule_expiry(&mut self, cx: &mut Context<Self>) {
        // Progress lasts as long as the operation. Failures remain readable
        // until dismissed; successful results disappear after five seconds.
        if self.hovered
            || !self
                .popup
                .as_open()
                .is_some_and(|m| m.kind == Kind::Success)
        {
            return;
        }
        let sequence = self.sequence;
        self.expiry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DISPLAY_TIME).await;
            this.update(cx, |this, cx| {
                if this.sequence == sequence && !this.hovered {
                    this.dismiss(cx);
                }
            })
            .ok();
        }));
    }

    fn set_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.hovered = hovered;
        self.expiry = None;
        if !hovered {
            self.schedule_expiry(cx);
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.expiry = None;
        self.hovered = false;
        if self.popup.begin_close() {
            popover::reap_popup(cx, |this| &mut this.popup);
            cx.notify();
        }
    }

    #[cfg(any(test, feature = "source-control-fixture"))]
    pub(crate) fn fixture_state(&self) -> serde_json::Value {
        serde_json::json!({
            "open": self.popup.is_open(), "hovered": self.hovered,
            "title": self.popup.get().map(|m| &m.title),
            "message": self.popup.get().map(|m| &m.body),
            "kind": self.popup.get().map(|m| match m.kind {
                Kind::Progress => "progress", Kind::Success => "success", Kind::Error => "error",
            }),
        })
    }
}

impl gpui::Render for Toasts {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(message) = self.popup.get().cloned() else {
            return gpui::Empty.into_any_element();
        };
        let theme = Theme::of(cx).for_popup();
        let closing = self.popup.is_closing();
        let width = (f32::from(window.viewport_size().width) - 32.0).clamp(1.0, 360.0);
        let icon = match message.kind {
            Kind::Progress => div()
                .size(px(16.0))
                .flex()
                .items_center()
                .justify_center()
                .child(crate::loaders::mini_mono_spinner(
                    "git-toast-progress",
                    4.0,
                    theme.text_muted,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element(),
            Kind::Success => icons::icon(icons::CHECK)
                .size(px(16.0))
                .text_color(theme.success)
                .into_any_element(),
            Kind::Error => icons::icon(icons::DANGER_TRIANGLE)
                .size(px(16.0))
                .text_color(theme.danger)
                .into_any_element(),
        };
        let card = popover::popover_card(&theme)
            .id("notification-toast")
            .debug_selector(|| "notification-toast".into())
            .role(if message.kind == Kind::Error {
                gpui::Role::Alert
            } else {
                gpui::Role::Status
            })
            .aria_label(format!("{}: {}", message.title, message.body))
            .w(px(width))
            .p(px(12.0))
            .flex()
            .items_start()
            .gap(px(10.0))
            .occlude()
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.set_hovered(*hovered, cx);
            }))
            .child(div().mt(px(2.0)).flex_none().child(icon))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(message.title),
                    )
                    .child(
                        div()
                            .id("toast-message")
                            .mt(px(4.0))
                            .max_h(px(120.0))
                            .overflow_y_scroll()
                            .text_size(px(13.0))
                            .line_height(px(18.0))
                            .child(message.body),
                    ),
            )
            .child(
                popover::btn_ghost(&theme, "", "dismiss-toast")
                    .id("dismiss-toast")
                    .debug_selector(|| "dismiss-toast".into())
                    .size(px(20.0))
                    .p_0()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .tab_index(0)
                    .role(gpui::Role::Button)
                    .aria_label("Dismiss notification")
                    .tooltip(crate::settings::widgets::text_tooltip(
                        "Dismiss notification",
                    ))
                    .child(
                        icons::icon(icons::CLOSE)
                            .size(px(12.0))
                            .text_color(theme.text_muted),
                    )
                    .when(!closing, |button| {
                        button.on_click(cx.listener(|this, _, _, cx| this.dismiss(cx)))
                    }),
            );
        let progress = self.popup.closing_since().map(|since| {
            let span = motion::MENU_OUT.total().mul_f32(motion::speed_scale());
            let raw = if span.is_zero() {
                1.0
            } else {
                (since.elapsed().as_secs_f32() / span.as_secs_f32()).clamp(0.0, 1.0)
            };
            if cx.reduce_motion() {
                1.0
            } else {
                motion::MENU_OUT.progress(raw)
            }
        });
        // Popover's outside-dismiss guard deliberately consumes outside
        // presses. A toast must let those presses reach the app normally.
        let content = crate::frost::frosted(
            popover::CARD_RADIUS,
            crate::frost::MENU_BLUR * (1.0 - progress.unwrap_or(0.0)),
            card.into_any_element(),
        );
        let content: AnyElement = if let Some(t) = progress {
            motion::menu_out(
                SharedString::from(format!("toast-{}-out", self.sequence)),
                t,
                div()
                    .child(content)
                    .child(div().absolute().inset_0().occlude()),
            )
            .into_any_element()
        } else {
            motion::menu_in(
                SharedString::from(format!("toast-{}-in", self.sequence)),
                div().child(content),
            )
            .into_any_element()
        };
        div()
            .absolute()
            .bottom(px(16.0))
            .right(px(16.0))
            .child(content)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[gpui::test]
    fn immediate_errors_cancel_expiry_and_old_progress_cannot_dismiss_them(
        cx: &mut TestAppContext,
    ) {
        let toasts = cx.new(|_| Toasts::default());
        toasts.update(cx, |toasts, cx| {
            toasts.success("Git".into(), "Refreshed".into(), cx)
        });
        let pending =
            PendingToast::start(toasts.clone(), "Git".into(), "Checking changes…".into(), cx);
        toasts.update(cx, |toasts, cx| {
            toasts.error("Git".into(), "Repository is unavailable".into(), cx)
        });
        pending.dismiss(cx);
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(30));
        cx.run_until_parked();
        toasts.read_with(cx, |toasts, _| {
            let message = toasts.popup.as_open().unwrap();
            assert_eq!(message.kind, Kind::Error);
            assert_eq!(message.body, "Repository is unavailable");
        });
    }

    #[gpui::test]
    fn progress_lasts_until_finished_and_success_gets_a_fresh_timeout(cx: &mut TestAppContext) {
        let toasts = cx.new(|_| Toasts::default());
        let pending =
            PendingToast::start(toasts.clone(), "Git".into(), "Pushing commits…".into(), cx);
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(30));
        cx.run_until_parked();
        toasts.read_with(cx, |toasts, _| {
            assert!(toasts.popup.is_open());
            assert_eq!(toasts.popup.get().unwrap().kind, Kind::Progress);
        });
        pending.finish(Ok("Pushed".into()), cx);
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(4));
        cx.run_until_parked();
        toasts.read_with(cx, |toasts, _| {
            assert!(toasts.popup.is_open());
            assert_eq!(toasts.popup.get().unwrap().kind, Kind::Success);
        });
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        toasts.read_with(cx, |toasts, _| assert!(!toasts.popup.is_open()));
    }

    #[gpui::test]
    fn a_failure_stops_progress_and_stays_readable(cx: &mut TestAppContext) {
        let toasts = cx.new(|_| Toasts::default());
        let pending =
            PendingToast::start(toasts.clone(), "Git".into(), "Pushing commits…".into(), cx);
        pending.finish(Err("Remote rejected the push".into()), cx);
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(30));
        cx.run_until_parked();
        toasts.read_with(cx, |toasts, _| {
            assert!(toasts.popup.is_open());
            let message = toasts.popup.get().unwrap();
            assert_eq!(message.kind, Kind::Error);
            assert_eq!(message.body, "Remote rejected the push");
        });
        toasts.update(cx, |toasts, cx| toasts.dismiss(cx));
        toasts.read_with(cx, |toasts, _| assert!(!toasts.popup.is_open()));
    }

    #[gpui::test]
    fn old_completions_cannot_replace_new_feedback_or_reopen_a_dismissed_toast(
        cx: &mut TestAppContext,
    ) {
        let toasts = cx.new(|_| Toasts::default());
        let first = PendingToast::start(toasts.clone(), "Git".into(), "Pushing…".into(), cx);
        let second = PendingToast::start(toasts.clone(), "Git".into(), "Fetching…".into(), cx);
        first.finish(Ok("Pushed".into()), cx);
        toasts.read_with(cx, |toasts, _| {
            let message = toasts.popup.get().unwrap();
            assert_eq!(message.kind, Kind::Progress);
            assert_eq!(message.body, "Fetching…");
        });
        toasts.update(cx, |toasts, cx| toasts.dismiss(cx));
        second.finish(Err("Fetch failed".into()), cx);
        toasts.read_with(cx, |toasts, _| assert!(!toasts.popup.is_open()));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        toasts.read_with(cx, |toasts, _| {
            assert!(!toasts.popup.is_open());
            // The exit animation uses wall time; the virtual timer need not
            // reap its card yet. Completion must still leave it dismissed.
            if let Some(message) = toasts.popup.get() {
                assert_eq!(message.kind, Kind::Progress);
                assert_eq!(message.body, "Fetching…");
            }
        });
    }

    #[gpui::test]
    fn new_messages_get_their_own_timeout_and_survive_an_old_close(cx: &mut TestAppContext) {
        let toasts = cx.new(|_| Toasts::default());
        toasts.update(cx, |toasts, cx| {
            toasts.success("Git".into(), "Pushed".into(), cx)
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(4));
        cx.run_until_parked();
        toasts.update(cx, |toasts, cx| {
            toasts.success("Git".into(), "Synced".into(), cx)
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        toasts.read_with(cx, |toasts, _| assert!(toasts.popup.is_open()));
        toasts.update(cx, |toasts, cx| toasts.dismiss(cx));
        toasts.update(cx, |toasts, cx| {
            toasts.success("Git".into(), "Committed".into(), cx)
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        toasts.read_with(cx, |toasts, _| assert!(toasts.popup.is_open()));
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();
        toasts.read_with(cx, |toasts, _| assert!(!toasts.popup.is_open()));
    }

    #[gpui::test]
    fn hovering_preserves_the_message_until_the_pointer_leaves(cx: &mut TestAppContext) {
        let toasts = cx.new(|_| Toasts::default());
        toasts.update(cx, |toasts, cx| {
            toasts.success("Git".into(), "Synced".into(), cx)
        });
        cx.run_until_parked();
        toasts.update(cx, |toasts, cx| toasts.set_hovered(true, cx));
        cx.executor().advance_clock(Duration::from_secs(20));
        cx.run_until_parked();
        toasts.read_with(cx, |toasts, _| assert!(toasts.popup.is_open()));
        toasts.update(cx, |toasts, cx| toasts.set_hovered(false, cx));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(6));
        cx.run_until_parked();
        toasts.read_with(cx, |toasts, _| assert!(!toasts.popup.is_open()));
    }
}
