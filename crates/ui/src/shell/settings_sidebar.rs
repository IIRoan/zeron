//! Stacked project editors above the projects sidebar. Their captured project
//! and drafts survive navigation in the workspace behind them.
use super::*;
use std::cell::RefCell;
use std::rc::Rc;
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

const WIDTH: f32 = 560.0;
const TOOLBAR: f32 = 40.0;
pub(super) const EDGE: f32 = 8.0;
const BORDER: f32 = 1.0;
const RADIUS: f32 = 16.0;
const OPEN: motion::MotionSpec = motion::MotionSpec::new(280, motion::EASE_TAILWIND);
pub(super) const CLOSE: motion::MotionSpec =
    motion::MotionSpec::new(180, motion::CubicBezier::new(0.4, 0.0, 1.0, 1.0));

/// One clock for both directions, so closing during the entrance or reopening
/// during the exit continues from the position already on screen.
struct SheetMotion {
    tween: motion::Tween,
    closing: Option<Instant>,
    last_value: f32,
    pending: bool,
    barrier_queued: bool,
    generation: u64,
}

impl SheetMotion {
    fn new(now: Instant) -> Self {
        Self {
            tween: motion::Tween::at(0.0, OPEN, now),
            closing: None,
            last_value: 0.0,
            pending: true,
            barrier_queued: false,
            generation: 0,
        }
    }

    fn play(&mut self, reduced: bool, now: Instant) {
        if self.pending {
            self.pending = false;
            self.tween = motion::Tween::at(
                self.last_value,
                if self.closing.is_some() { CLOSE } else { OPEN },
                now,
            );
            self.tween
                .retarget(if self.closing.is_some() { 0.0 } else { 1.0 }, reduced, now);
        }
    }

    fn update(&mut self, closing: Option<Instant>, reduced: bool, now: Instant) -> f32 {
        if self.closing != closing {
            // Freeze the last painted position until the new direction has
            // prepared a frame. Input handling and layout consume no motion.
            self.closing = closing;
            self.tween = motion::Tween::at(
                self.last_value,
                if closing.is_some() { CLOSE } else { OPEN },
                now,
            );
            self.pending = true;
            self.barrier_queued = false;
            self.generation = self.generation.wrapping_add(1);
        }
        if reduced {
            self.pending = false;
            self.tween
                .retarget(if closing.is_some() { 0.0 } else { 1.0 }, true, now);
        }
        self.last_value = self.tween.value_at(now);
        self.last_value
    }

    fn take_barrier(&mut self) -> Option<u64> {
        if !self.pending || self.barrier_queued {
            return None;
        }
        self.barrier_queued = true;
        Some(self.generation)
    }

    fn settled_at(&self, now: Instant) -> bool {
        !self.pending && !self.tween.animating_at(now)
    }
}

fn slide_progress(
    id: &'static str,
    closing: Option<Instant>,
    reduced: bool,
    window: &mut Window,
) -> (f32, bool) {
    let now = Instant::now();
    let (progress, animating, settled) = window.with_global_id(
        SharedString::from(format!("{id}-slide")).into(),
        |id, window| {
            window.with_element_state(id, |state: Option<Rc<RefCell<SheetMotion>>>, window| {
                let state = state.unwrap_or_else(|| Rc::new(RefCell::new(SheetMotion::new(now))));
                let progress = state.borrow_mut().update(closing, reduced, now);
                let barrier = state.borrow_mut().take_barrier();
                if let Some(generation) = barrier {
                    // Lay out and paint the start pose first. Only the next
                    // frame starts the clock, after cold text/layout work.
                    let pending = Rc::downgrade(&state);
                    let owner = window.current_view();
                    window.on_next_frame(move |_, cx| {
                        if let Some(state) = pending.upgrade() {
                            let mut state = state.borrow_mut();
                            if state.generation == generation && state.pending {
                                state.play(motion::reduced_motion(cx), Instant::now());
                                cx.notify(owner);
                            }
                        }
                    });
                }
                let animating = state.borrow().tween.animating_at(now);
                let settled = state.borrow().settled_at(now);
                ((progress, animating, settled), state)
            })
        },
    );
    if animating {
        window.request_animation_frame();
    }
    (progress, settled)
}

/// The sheet owns its frame invalidations. Other cached workspace content can
/// stay cached, and unrelated window frames can reuse the prepared form.
pub(super) struct SettingsSheet {
    shell: gpui::WeakEntity<Shell>,
    form: UnsavedForm,
    _changed: Subscription,
}

impl Render for SettingsSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let content = self
            .shell
            .update(cx, |shell, cx| match self.form {
                UnsavedForm::WorktreeSettings => {
                    shell.render_worktree_settings(viewport, window, cx)
                }
                UnsavedForm::Services => shell.render_project_terminal_editor(viewport, window, cx),
            })
            .ok()
            .flatten();
        div().size_full().children(content)
    }
}

fn frame_size(viewport: gpui::Size<Pixels>) -> gpui::Size<Pixels> {
    gpui::size(
        (viewport.width - px(EDGE * 2.0))
            .max(px(0.0))
            .min(px(WIDTH)),
        (viewport.height - px(EDGE * 2.0)).max(px(0.0)),
    )
}

pub(super) fn content_size(viewport: gpui::Size<Pixels>) -> gpui::Size<Pixels> {
    let frame = frame_size(viewport);
    gpui::size(
        (frame.width - px(BORDER * 2.0)).max(px(0.0)),
        (frame.height - px(BORDER * 2.0 + TOOLBAR)).max(px(0.0)),
    )
}

impl Shell {
    pub(super) fn settings_sidebar_view(
        &mut self,
        form: UnsavedForm,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (index, mounted) = match form {
            UnsavedForm::WorktreeSettings => (0, self.worktree_settings.editor.get().is_some()),
            UnsavedForm::Services => (1, self.project_terminals.editor.get().is_some()),
        };
        if !mounted {
            return None;
        }
        let sheet = self.settings_sidebars[index].get_or_insert_with(|| {
            let shell = cx.entity();
            cx.new(|cx| SettingsSheet {
                shell: shell.downgrade(),
                form,
                _changed: cx.observe(&shell, |_, _, cx| cx.notify()),
            })
        });
        Some(
            sheet
                .clone()
                .cached(gpui::StyleRefinement::default().absolute().inset_0())
                .into_any_element(),
        )
    }

    /// Begin a logical close. Rendering owns the deferred exit and removes
    /// only this closing editor after its last animation frame.
    pub(super) fn close_settings_sidebar<T: 'static>(
        &mut self,
        cx: &mut Context<Self>,
        popup: impl Fn(&mut Self) -> &mut popover::Popup<T> + 'static,
    ) {
        if !popup(self).begin_close() {
            return;
        }
        if motion::reduced_motion(cx) {
            *popup(self) = Default::default();
        }
        cx.notify();
    }

    pub(super) fn render_settings_sidebar(
        &mut self,
        id: &'static str,
        form: UnsavedForm,
        viewport: gpui::Size<Pixels>,
        content: AnyElement,
        closing: Option<Instant>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).for_popup();
        let frame = frame_size(viewport);
        let handles = self
            .settings_sidebar_focus
            .get_or_insert_with(|| {
                std::array::from_fn(|_| cx.focus_handle().tab_index(0).tab_stop(true))
            })
            .clone();
        let offset = if matches!(form, UnsavedForm::Services) {
            2
        } else {
            0
        };
        let back =
            if matches!(form, UnsavedForm::Services) && self.worktree_settings.editor.is_open() {
                "Back to project settings"
            } else {
                "Back to workspace"
            };
        let button = |index: usize, suffix: &'static str, glyph, label: &'static str| {
            div()
                .id(SharedString::from(format!("{id}-{suffix}")))
                .role(gpui::Role::Button)
                .track_focus(&handles[offset + index])
                .tab_index(0)
                .aria_label(label)
                .h(px(28.0))
                .px(px(8.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .cursor_pointer()
                .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text))
                .child(icon(glyph).size(px(15.0)).text_color(theme.text_muted))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.request_close_form(form, window, cx);
                }))
                .on_key_down(
                    cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                        if !event.is_held
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.request_close_form(form, window, cx);
                            cx.stop_propagation();
                        }
                    }),
                )
        };
        let toolbar = div()
            .h(px(TOOLBAR))
            .w_full()
            .flex_none()
            .px(px(12.0))
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(button(0, "back", icons::ALT_ARROW_LEFT, back).child(back))
            .child(button(1, "close", icons::CLOSE, "Close settings"));
        let mut sheet = div()
            .id(id)
            .debug_selector(move || id.into())
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .w(frame.width)
            .h(frame.height)
            .bg(theme.surface_dialog)
            .border_1()
            .border_color(theme.border)
            .rounded(px(RADIUS))
            .shadow_lg()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(toolbar)
            .child(content);
        if closing.is_some() {
            sheet = sheet.relative().child(div().absolute().inset_0().occlude());
        }
        let (progress, settled) = slide_progress(id, closing, motion::reduced_motion(cx), window);
        if let Some(closing) = closing.filter(|_| settled) {
            // Drop only after the actual exit has painted its end pose. A
            // wall-clock timer from the input event could cut off a deferred
            // exit, or remove a newer editor after an interrupted close.
            let shell = cx.weak_entity();
            cx.defer(move |cx| {
                let _ = shell.update(cx, |shell, cx| {
                    match form {
                        UnsavedForm::WorktreeSettings
                            if shell.worktree_settings.editor.closing_since() == Some(closing) =>
                        {
                            shell.worktree_settings.editor = Default::default();
                        }
                        UnsavedForm::Services
                            if shell.project_terminals.editor.closing_since() == Some(closing) =>
                        {
                            shell.project_terminals.editor = Default::default();
                        }
                        _ => return,
                    }
                    cx.notify();
                });
            });
        }
        let layer = sheet
            .relative()
            .left((frame.width + px(EDGE)) * (1.0 - progress));
        gpui::deferred(
            gpui::anchored()
                .position(gpui::point(
                    (viewport.width - frame.width - px(EDGE)).max(px(0.0)),
                    px(EDGE),
                ))
                .child(layer),
        )
        .priority(2)
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reversing_the_slide_keeps_the_current_position_and_settles_in_both_directions() {
        let start = Instant::now();
        let opening = OPEN.total().mul_f32(motion::speed_scale());
        let closing = CLOSE.total().mul_f32(motion::speed_scale());
        let mut slide = SheetMotion::new(start);
        slide.play(false, start);
        assert_eq!(slide.update(None, false, start), 0.0);
        let close_at = start + opening / 3;
        let before = slide.update(None, false, close_at);
        assert!(before > 0.0 && before < 1.0);
        assert_eq!(slide.update(Some(close_at), false, close_at), before);
        slide.play(false, close_at);
        let reopen_at = close_at + closing / 2;
        let before = slide.update(Some(close_at), false, reopen_at);
        assert_eq!(slide.update(None, false, reopen_at), before);
        slide.play(false, reopen_at);
        let opened = reopen_at + opening;
        assert_eq!(slide.update(None, false, opened), 1.0);
        slide.update(Some(opened), false, opened);
        slide.play(false, opened);
        assert_eq!(slide.update(Some(opened), false, opened + closing), 0.0);
        assert!(!slide.tween.animating_at(opened + closing));
    }

    #[test]
    fn reduced_motion_snaps_an_opening_or_closing_slide_without_scheduling_frames() {
        let now = Instant::now();
        let mut slide = SheetMotion::new(now);
        assert_eq!(slide.update(None, true, now), 1.0);
        assert!(!slide.tween.animating_at(now));
        assert_eq!(slide.update(Some(now), true, now), 0.0);
        assert!(!slide.tween.animating_at(now));
        assert_eq!(slide.update(None, true, now), 1.0);
    }

    #[test]
    fn cold_layout_cannot_consume_the_entrance_or_restart_a_cancelled_open() {
        let start = Instant::now();
        let mut slide = SheetMotion::new(start);
        let painted = start + Duration::from_secs(2);
        assert_eq!(slide.update(None, false, painted), 0.0);
        assert!(!slide.tween.animating_at(painted));
        slide.play(false, painted);
        assert_eq!(slide.update(None, false, painted), 0.0);
        let mid = painted + OPEN.total().mul_f32(motion::speed_scale()) / 2;
        assert!(slide.update(None, false, mid) > 0.0);

        let mut cancelled = SheetMotion::new(start);
        assert_eq!(cancelled.update(Some(start), false, painted), 0.0);
        cancelled.play(false, painted);
        assert_eq!(cancelled.update(Some(start), false, painted), 0.0);
        assert!(!cancelled.tween.animating_at(painted));
    }

    struct MotionView {
        mounted: bool,
        closing: Option<Instant>,
        seen: Rc<std::cell::Cell<f32>>,
    }

    impl Render for MotionView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            if self.mounted {
                self.seen.set(
                    slide_progress(
                        "test-sheet",
                        self.closing,
                        motion::reduced_motion(cx),
                        window,
                    )
                    .0,
                );
            }
            div().size_full()
        }
    }

    struct MotionHost {
        view: Entity<MotionView>,
        background: Entity<StaticContent>,
    }

    struct StaticContent(Rc<std::cell::Cell<usize>>);

    impl Render for StaticContent {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.0.set(self.0.get() + 1);
            div().size_full()
        }
    }

    impl Render for MotionHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(
                    self.background
                        .clone()
                        .cached(gpui::StyleRefinement::default().absolute().inset_0()),
                )
                .child(
                    self.view
                        .clone()
                        .cached(gpui::StyleRefinement::default().absolute().inset_0()),
                )
        }
    }

    fn paint(window: gpui::AnyWindowHandle, cx: &mut gpui::TestAppContext) {
        cx.update_window(window, |_, window, cx| window.draw(cx).clear())
            .unwrap();
    }

    fn next_frame(window: gpui::AnyWindowHandle, cx: &mut gpui::TestAppContext) -> usize {
        cx.update_window(window, |_, window, cx| window.simulate_next_frame(cx))
            .unwrap()
    }

    #[gpui::test]
    fn sheet_waits_for_paint_and_animates_without_repainting_other_cached_content(
        cx: &mut gpui::TestAppContext,
    ) {
        let seen = Rc::new(std::cell::Cell::new(-1.0));
        let renders = Rc::new(std::cell::Cell::new(0));
        let view = cx.new(|_| MotionView {
            mounted: true,
            closing: None,
            seen: seen.clone(),
        });
        let background = cx.new(|_| StaticContent(renders.clone()));
        let window = cx
            .add_window(|_, _| MotionHost {
                view: view.clone(),
                background,
            })
            .into();
        paint(window, cx);
        assert_eq!(seen.get(), 0.0);
        for _ in 0..3 {
            view.update(cx, |_, cx| cx.notify());
            paint(window, cx);
            assert_eq!(
                seen.get(),
                0.0,
                "renders before the frame barrier cannot start the clock"
            );
        }
        let host_renders = renders.get();
        assert_eq!(
            next_frame(window, cx),
            1,
            "one mount queues one preparation barrier"
        );
        paint(window, cx);
        assert!(seen.get() < 1.0);
        assert_eq!(
            renders.get(),
            host_renders,
            "the sheet's frame invalidation leaves unrelated cached content intact"
        );
        std::thread::sleep(OPEN.total().mul_f32(motion::speed_scale()) + Duration::from_millis(20));
        next_frame(window, cx);
        paint(window, cx);
        assert_eq!(seen.get(), 1.0);
        next_frame(window, cx);
        assert_eq!(
            next_frame(window, cx),
            0,
            "a settled sheet stops requesting frames"
        );
    }

    #[gpui::test]
    fn an_unmounted_sheet_cancels_its_pending_start_and_reduced_motion_bypasses_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let seen = Rc::new(std::cell::Cell::new(-1.0));
        let view = cx.new(|_| MotionView {
            mounted: true,
            closing: None,
            seen: seen.clone(),
        });
        let background = cx.new(|_| StaticContent(Default::default()));
        let window = cx
            .add_window(|_, _| MotionHost {
                view: view.clone(),
                background,
            })
            .into();
        paint(window, cx);
        view.update(cx, |view, cx| {
            view.mounted = false;
            cx.notify();
        });
        paint(window, cx);
        next_frame(window, cx);
        assert_eq!(
            next_frame(window, cx),
            0,
            "the retired barrier cannot start an animation"
        );
        cx.update(|cx| motion::set_reduced_motion(cx, true));
        view.update(cx, |view, cx| {
            view.mounted = true;
            cx.notify();
        });
        paint(window, cx);
        assert_eq!(seen.get(), 1.0);
        assert_eq!(next_frame(window, cx), 0);
        view.update(cx, |view, cx| {
            view.closing = Some(Instant::now());
            cx.notify();
        });
        paint(window, cx);
        assert_eq!(seen.get(), 0.0);
    }

    #[test]
    fn each_direction_owns_one_barrier_and_old_callbacks_cannot_start_a_new_phase() {
        let now = Instant::now();
        let mut slide = SheetMotion::new(now);
        let opening = slide.take_barrier().unwrap();
        assert!(slide.take_barrier().is_none());
        slide.update(Some(now), false, now);
        let closing = slide.take_barrier().unwrap();
        assert_ne!(opening, closing);
        slide.update(None, false, now);
        let reopened = slide.take_barrier().unwrap();
        assert_ne!(closing, reopened);
        assert_ne!(opening, reopened);
        assert!(slide.pending);
        assert!(!slide.settled_at(now + Duration::from_secs(2)));
        assert_eq!(slide.update(None, false, now + Duration::from_secs(2)), 0.0);
    }
}
