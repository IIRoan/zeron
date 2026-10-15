//! Find in the live editor buffer or a read-only source preview.
use gpui::{
    AnyElement, Context, Entity, Focusable as _, SharedString, Subscription, Window, div,
    prelude::*, px,
};
use gpui_base::input::SearchMatcher;

use super::{FilesSurface, toolbar, toolbar_button};
use crate::{
    composer::{ComposerInput, ComposerInputEvent},
    icons,
    theme::Theme,
};

type FindVersion = (
    super::document::DocumentKey,
    u64,
    u64,
    Option<String>,
    Option<gpui::EntityId>,
);

pub(super) struct FileFind {
    pub input: Entity<ComposerInput>,
    pub open: bool,
    case_sensitive: bool,
    source: SharedString,
    query: String,
    matched_case: bool,
    version: Option<FindVersion>,
    matcher: SearchMatcher,
    line_starts: Vec<usize>,
    _events: Subscription,
}

impl FileFind {
    pub(super) fn clear_document(&mut self) {
        self.open = false;
        self.source = "".into();
        self.version = None;
        self.matcher = SearchMatcher::new();
        self.line_starts = vec![0];
    }
    pub fn new(cx: &mut Context<FilesSurface>) -> Self {
        let input = cx.new(|cx| {
            // This context binds text editing only, leaving Enter/Shift+Enter
            // to the owning find bar's navigation handler.
            ComposerInput::with_context("Find in file", "PaletteSearch", cx)
                .with_single_line()
                .with_accessibility_role(gpui::Role::SearchInput)
                .with_text_metrics(11.0, 16.0)
        });
        let events = cx.subscribe(&input, |surface: &mut FilesSurface, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                surface.update_file_find(cx);
            }
        });
        Self {
            input,
            open: false,
            case_sensitive: false,
            matched_case: false,
            version: None,
            source: "".into(),
            query: String::new(),
            matcher: SearchMatcher::new(),
            line_starts: vec![0],
            _events: events,
        }
    }

    pub fn ranges_for_line(&self, line: usize, length: usize) -> Vec<std::ops::Range<usize>> {
        if !self.open {
            return Vec::new();
        }
        let Some(&start) = self.line_starts.get(line) else {
            return Vec::new();
        };
        let ranges = self.matcher.matched_ranges();
        let first = ranges.partition_point(|range| range.end <= start);
        ranges[first..]
            .iter()
            .take_while(|range| range.start < start + length)
            .map(|range| range.start.saturating_sub(start)..range.end.min(start + length) - start)
            .collect()
    }
}

impl FilesSurface {
    #[cfg(test)]
    pub(crate) fn test_find_label(&self, cx: &gpui::App) -> Option<String> {
        self.find.open.then(|| {
            self.find_editor()
                .map(|editor| editor.read(cx).search_session().matcher.label())
                .unwrap_or_else(|| self.find.matcher.label())
        })
    }

    #[cfg(test)]
    pub(crate) fn test_find_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.find
            .input
            .update(cx, |input, cx| input.set_text(query, cx));
    }
    fn find_editor(&self) -> Option<Entity<super::editor::FileEditorState>> {
        self.preview
            .active
            .as_ref()
            .and_then(|path| self.preview.documents.get(path))
            .and_then(|document| document.editor.clone())
    }

    pub(super) fn open_file_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.presentation.is_editor() {
            return false;
        }
        let Some(path) = self.preview.active.clone() else {
            return false;
        };
        let Some(document) = self.preview.documents.get_mut(&path) else {
            return false;
        };
        if document
            .file
            .as_ref()
            .and_then(|file| file.text.as_ref())
            .is_none()
        {
            return false;
        }
        // Source locations map directly to code, so find also works from the
        // Markdown preview without inventing offsets into rendered blocks.
        document.show_markdown = false;
        if let Some(view) = &document.markdown {
            view.update(cx, |view, cx| view.suspend(cx));
        }
        let was_open = self.find.open;
        self.find.open = true;
        if let Some(editor) = self.find_editor()
            && (!was_open || editor.focus_handle(cx).is_focused(window))
        {
            let state = editor.read(cx);
            let selected = state
                .value()
                .get(state.selected_range())
                .unwrap_or_default()
                .to_string();
            editor.update(cx, |editor, cx| editor.open_search(false, cx));
            if !selected.is_empty() && !selected.contains('\n') {
                self.find
                    .input
                    .update(cx, |input, cx| input.set_text(selected, cx));
            }
            self.find.version = None;
        }
        self.update_file_find(cx);
        self.find
            .input
            .update(cx, |input, cx| input.select_all_text(cx));
        let focus = self.find.input.focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));
        cx.notify();
        true
    }

    pub(super) fn close_file_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find.clear_document();
        if let Some(editor) = self.find_editor() {
            editor.update(cx, |editor, cx| {
                // Keep the query in our input, while releasing the editor's
                // matches and avoiding rescans on edits after find is closed.
                editor.set_search_query("", true, cx);
                editor.close_search(cx);
            });
        }
        self.focus_editor(window, cx);
        cx.notify();
    }

    pub(super) fn update_file_find(&mut self, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        let query = self.find.input.read(cx).text().to_string();
        let editor = self.find_editor();
        let version = self
            .preview
            .active
            .as_ref()
            .and_then(|path| self.preview.documents.get(path))
            .map(|document| {
                (
                    document.key.clone(),
                    document.generation,
                    document.revision,
                    document.loaded_hash.clone(),
                    editor.as_ref().map(Entity::entity_id),
                )
            });
        let query_changed =
            self.find.query != query || self.find.matched_case != self.find.case_sensitive;
        let document_changed = self.find.version.as_ref().map(|v| (&v.0, v.4))
            != version.as_ref().map(|v| (&v.0, v.4));
        if !query_changed && self.find.version == version {
            return;
        }
        self.find.version = version;
        self.find.query = query.clone();
        self.find.matched_case = self.find.case_sensitive;
        if let Some(editor) = editor {
            if query_changed || document_changed {
                editor.update(cx, |editor, cx| {
                    editor.open_search(false, cx);
                    editor.set_search_query(query, !self.find.case_sensitive, cx);
                    editor.previous_search_match(cx);
                    editor.next_search_match(cx);
                });
            }
            cx.notify();
            return;
        }
        let source: SharedString = {
            self.preview
                .active
                .as_ref()
                .and_then(|path| self.preview.documents.get(path))
                .and_then(|document| document.file.as_ref())
                .and_then(|file| file.text.clone())
                .map(Into::into)
        }
        .unwrap_or_default();
        if self.find.source != source {
            self.find.line_starts = std::iter::once(0)
                .chain(
                    source
                        .bytes()
                        .enumerate()
                        .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
                )
                .collect();
            self.find.matcher.update(&source.as_ref().into());
            self.find.source = source;
        }
        self.find
            .matcher
            .update_query(&query, !self.find.case_sensitive);
        self.reveal_preview_find();
        cx.notify();
    }

    fn reveal_preview_find(&mut self) {
        if let Some(range) = self
            .find
            .matcher
            .matched_ranges()
            .get(self.find.matcher.current_match_index())
        {
            let line = self
                .find
                .line_starts
                .partition_point(|start| *start <= range.start)
                .saturating_sub(1);
            self.preview.list.scroll_to_reveal_item(line);
        }
    }

    pub(super) fn step_file_find(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.update_file_find(cx);
        if let Some(editor) = self.find_editor() {
            editor.update(cx, |editor, cx| {
                if forward {
                    editor.next_search_match(cx);
                } else {
                    editor.previous_search_match(cx);
                }
            });
        } else {
            if forward {
                self.find.matcher.next();
            } else {
                self.find.matcher.next_back();
            }
            self.reveal_preview_find();
        }
        cx.notify();
    }

    pub(super) fn render_file_find(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.find.open {
            return None;
        }
        self.update_file_find(cx);
        let label = self
            .find_editor()
            .map(|editor| editor.read(cx).search_session().matcher.label())
            .unwrap_or_else(|| self.find.matcher.label());
        let focus = self.find.input.focus_handle(cx);
        Some(
            toolbar(theme)
                .id("file-find-bar")
                .debug_selector(|| "file-find-bar".into())
                .child(
                    crate::surface_chrome::input()
                        .id("file-find-input")
                        .debug_selector(|| "file-find-input".into())
                        .cursor_text()
                        .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                            focus.focus(window, cx);
                        })
                        .child(
                            icons::icon(icons::MAGNIFER)
                                .size(px(12.0))
                                .text_color(theme.text_faint),
                        )
                        .child(div().flex_1().min_w_0().child(self.find.input.clone())),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.0))
                        .text_color(theme.text_muted)
                        .child(label),
                )
                .child(
                    toolbar_button("file-find-case", "Match case")
                        .aria_selected(self.find.case_sensitive)
                        .when(self.find.case_sensitive, |el| {
                            el.bg(crate::theme::wash(0.12))
                        })
                        .child("Aa")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.find.case_sensitive = !this.find.case_sensitive;
                            this.update_file_find(cx);
                        })),
                )
                .child(
                    toolbar_button(
                        "file-find-previous",
                        "Previous match (Shift+Enter / Shift+F3)",
                    )
                    .child("↑")
                    .on_click(cx.listener(|this, _, _, cx| this.step_file_find(false, cx))),
                )
                .child(
                    toolbar_button("file-find-next", "Next match (Enter / F3)")
                        .child("↓")
                        .on_click(cx.listener(|this, _, _, cx| this.step_file_find(true, cx))),
                )
                .child(
                    toolbar_button("file-find-close", "Close find (Escape)")
                        .child("×")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.close_file_find(window, cx)),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::document::{DocumentKey, FileDocument};
    use super::*;
    use gpui::{AppContext, TestAppContext, VisualTestContext};

    fn setup(
        cx: &mut TestAppContext,
        readonly: bool,
    ) -> (Entity<FilesSurface>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            crate::composer::init(cx, Default::default());
            cx.set_global(Theme::dark());
        });
        let (files, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| super::super::test_support::state());
            let mut files =
                FilesSurface::new(state, "chat".into(), false, 1000, 13.0, false, false, cx);
            let path = "note.txt";
            let mut document = FileDocument::loading(DocumentKey {
                chat_id: "chat".into(),
                checkout_id: Some("checkout".into()),
                path: path.into(),
            });
            document.set_loaded(zeron_proto::WorkspaceFileText {
                checkout_id: "checkout".into(),
                path: path.into(),
                text: Some("😀 needle\nNeedle needles\nneedle".into()),
                content_hash: Some("saved".into()),
                size: 36,
                modified_at: None,
                encoding: zeron_proto::WorkspaceTextEncoding::Utf8,
                line_ending: Some(zeron_proto::WorkspaceLineEnding::Lf),
                read_only_reason: readonly
                    .then_some(zeron_proto::WorkspaceReadOnlyReason::OutsideWorkspace),
                truncated: false,
            });
            files.preview.documents.insert(path.into(), document);
            files.preview.active = Some(path.into());
            files.editor_path = Some(path.into());
            files
        });
        cx.update(|window, cx| {
            window.activate_window();
            window.draw(cx).clear();
        });
        files.update_in(cx, |files, window, cx| files.focus_editor(window, cx));
        cx.run_until_parked();
        (files, cx)
    }

    fn shortcut() -> &'static str {
        if cfg!(target_os = "macos") {
            "cmd-f"
        } else {
            "ctrl-f"
        }
    }

    #[gpui::test]
    fn file_find_shortcut_counts_navigates_and_closes_without_editing(cx: &mut TestAppContext) {
        let (files, cx) = setup(cx, false);
        cx.simulate_keystrokes(shortcut());
        cx.simulate_input("needle");
        files.read_with(cx, |files, cx| {
            let editor = files.find_editor().unwrap();
            assert!(files.find.open);
            assert_eq!(editor.read(cx).search_session().matcher.label(), "1/4");
            assert_eq!(files.preview.documents["note.txt"].revision, 0);
        });
        cx.simulate_keystrokes("enter");
        files.read_with(cx, |files, cx| {
            assert_eq!(
                files
                    .find_editor()
                    .unwrap()
                    .read(cx)
                    .search_session()
                    .matcher
                    .label(),
                "2/4"
            )
        });
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_keystrokes("shift-f3");
        files.read_with(cx, |files, cx| {
            assert_eq!(
                files
                    .find_editor()
                    .unwrap()
                    .read(cx)
                    .search_session()
                    .matcher
                    .label(),
                "4/4"
            )
        });
        cx.simulate_keystrokes("f3");
        cx.simulate_keystrokes("escape");
        files.read_with(cx, |files, cx| {
            let editor = files.find_editor().unwrap();
            assert!(!files.find.open);
            assert!(!editor.read(cx).search_session().open);
            assert!(editor.read(cx).search_session().matcher.is_empty());
            assert_eq!(
                editor.read(cx).value().as_ref(),
                "😀 needle\nNeedle needles\nneedle"
            );
            assert!(!files.preview.documents["note.txt"].is_dirty());
        });
        cx.update(|window, cx| {
            assert!(
                files
                    .read(cx)
                    .find_editor()
                    .unwrap()
                    .focus_handle(cx)
                    .is_focused(window)
            )
        });
    }

    #[gpui::test]
    fn file_find_uses_unsaved_buffer_and_selected_text(cx: &mut TestAppContext) {
        let (files, cx) = setup(cx, false);
        files.update_in(cx, |files, window, cx| {
            let editor = files.find_editor().unwrap();
            editor.update(cx, |editor, cx| {
                editor.set_selected_range(0..editor.value().len(), cx);
                editor.replace("live live", window, cx);
                editor.set_selected_range(0..4, cx);
            });
        });
        cx.simulate_keystrokes(shortcut());
        files.read_with(cx, |files, cx| {
            assert_eq!(files.find.input.read(cx).text(), "live");
            assert_eq!(
                files
                    .find_editor()
                    .unwrap()
                    .read(cx)
                    .search_session()
                    .matcher
                    .len(),
                2
            );
            assert_eq!(
                files.preview.documents["note.txt"]
                    .file
                    .as_ref()
                    .unwrap()
                    .text
                    .as_deref(),
                Some("😀 needle\nNeedle needles\nneedle")
            );
            assert!(files.preview.documents["note.txt"].is_dirty());
        });
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        });
        cx.simulate_input("missing");
        cx.simulate_keystrokes(shortcut());
        files.read_with(cx, |files, cx| {
            assert_eq!(files.find.input.read(cx).text(), "missing")
        });
    }

    #[gpui::test]
    fn readonly_file_find_highlights_utf8_and_wraps(cx: &mut TestAppContext) {
        let (files, cx) = setup(cx, true);
        cx.simulate_keystrokes(shortcut());
        cx.simulate_input("needle");
        files.read_with(cx, |files, _| {
            assert!(files.find.open);
            assert!(files.find_editor().is_none());
            assert_eq!(files.find.matcher.label(), "1/4");
            assert_eq!(
                files.find.ranges_for_line(0, "😀 needle".len()),
                vec!["😀 ".len().."😀 needle".len()]
            );
        });
        cx.simulate_keystrokes("shift-enter");
        files.read_with(cx, |files, _| assert_eq!(files.find.matcher.label(), "4/4"));
        cx.simulate_keystrokes("escape");
        files.read_with(cx, |files, _| {
            assert!(files.find.ranges_for_line(0, 11).is_empty())
        });
    }
}
