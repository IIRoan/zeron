use std::{collections::HashSet, time::Duration};

use gpui::{
    AnyElement, Context, ListSizingBehavior, SharedString, Task, Window, div, list, prelude::*, px,
};
use zeron_proto::{
    ListWorkspaceDirectoryRequest, SearchWorkspaceFilesRequest, WorkspaceEntryKind,
    WorkspaceFileSearchMatch,
};

use super::{
    FilesSurface, WorkspacePathDrag,
    client::{FilesClientError, FilesRequestContext, WorkspaceFilesClient},
    model::parent_path,
    workspace_path_drag_ghost,
};
use crate::{
    file_icons::{self, FileIconIdentity},
    motion::HoverFade as _,
    theme::Theme,
};

pub const SEARCH_ROW_HEIGHT: f32 = 27.0;
const SEARCH_RESULT_LIMIT: usize = 200;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum SearchMode {
    #[default]
    Files,
    Code,
}

pub(super) enum SearchAnswer {
    Files(Vec<WorkspaceFileSearchMatch>, bool),
    Code(zeron_proto::WorkspaceContentSearchResult),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CodeRow {
    File(String, usize),
    Match(usize),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum RevealIntent {
    #[default]
    SynchronizeSelection,
    ActivateDirectory,
    OpenFile,
}

impl RevealIntent {
    fn clears_search(self) -> bool {
        self != Self::SynchronizeSelection
    }

    fn opens_file(self) -> bool {
        self == Self::OpenFile
    }
}

#[derive(Default)]
pub(super) struct FileSearchState {
    pub query: String,
    pub results: Vec<WorkspaceFileSearchMatch>,
    pub loading: bool,
    pub error: Option<SharedString>,
    pub generation: u64,
    displayed_generation: u64,
    name_index_ready: Option<(FilesRequestContext, std::time::Instant)>,
    pub active: usize,
    pub task: Option<Task<()>>,
    pub reveal_task: Option<Task<()>>,
    reveal_generation: u64,
    reveal_intent: RevealIntent,
    reveal_scroll_pending: bool,
    pub(super) mode: SearchMode,
    pub(super) content: Vec<zeron_proto::WorkspaceContentMatch>,
    pub(super) code_rows: Vec<CodeRow>,
    pub(super) collapsed: HashSet<String>,
    pub(super) indexing: bool,
    pub(super) truncated: bool,
    pub(super) options: zeron_proto::WorkspaceContentSearchOptions,
}

impl FileSearchState {
    fn accepts(&self, generation: u64, query: &str) -> bool {
        self.generation == generation && self.query == query
    }

    pub(super) fn visible_len(&self) -> usize {
        if self.mode == SearchMode::Code {
            self.code_rows.len()
        } else {
            self.results.len()
        }
    }
}

impl FilesSurface {
    pub(super) fn on_search_edited(&mut self, cx: &mut Context<Self>) {
        let text = self.search.read(cx).text();
        let query = if self.search_state.mode == SearchMode::Code {
            text.to_string()
        } else {
            text.trim().to_string()
        };
        if self.search_state.query == query {
            return;
        }
        self.run_workspace_search(query, cx);
    }

    pub(super) fn run_workspace_search(&mut self, query: String, cx: &mut Context<Self>) {
        if self.search_state.reveal_intent.clears_search() {
            self.cancel_reveal();
        }
        self.search_state.generation = self.search_state.generation.wrapping_add(1);
        self.search_state.query = query.clone();
        self.search_state.active = 0;
        self.search_state.error = None;
        self.search_state.task = None;
        self.search_state.indexing = false;
        self.search_state.truncated = false;
        if query.is_empty() {
            self.search_state.loading = false;
            self.search_state.results.clear();
            self.reset_search_results();
            if self.search_state.reveal_scroll_pending {
                self.search_state.reveal_scroll_pending = false;
                self.reveal_tree_selection();
            }
            self.search.update(cx, |search, cx| {
                search.set_mention_controls(false, false, cx)
            });
            cx.notify();
            return;
        }
        self.search.update(cx, |search, cx| {
            search.set_mention_controls(true, false, cx)
        });
        let Some(context) = self.request_context.clone() else {
            self.search_state.loading = false;
            self.search_state.error = Some("No workspace available for this chat.".into());
            cx.notify();
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.search_state.loading = false;
            self.search_state.error = Some("Workspace service is still starting.".into());
            cx.notify();
            return;
        };
        self.search_state.loading = true;
        let generation = self.search_state.generation;
        let mode = self.search_state.mode;
        let options = self.search_state.options.clone();
        let include_ignored = self.tree.include_ignored();
        let client = WorkspaceFilesClient::new(engine, context.clone());
        let mut name_ready =
            self.search_state
                .name_index_ready
                .as_ref()
                .is_some_and(|(ready, used)| {
                    ready == &context && used.elapsed() < Duration::from_secs(60)
                });
        self.search_state.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(80)).await;
            let mut delay = Duration::from_millis(500);
            loop {
                let result = if mode == SearchMode::Code {
                    client.search_content(zeron_proto::SearchWorkspaceContentRequest {
                        target: context.target.clone(), query: query.clone(), limit: Some(2000),
                        per_file_limit: Some(2000), options: Some(options.clone()),
                    }).await.map(SearchAnswer::Code)
                } else {
                    // Probe before searching: a scan that finishes after a
                    // partial answer must still trigger one final refresh.
                    let indexing = if !include_ignored && !name_ready {
                        client.warm_search().await.is_ok_and(|warm| warm.state == zeron_proto::WorkspaceSearchIndexState::Building)
                    } else { false };
                    name_ready = !indexing;
                    match client.search(SearchWorkspaceFilesRequest {
                        target: context.target.clone(), query: query.clone(), include_ignored,
                        limit: Some(SEARCH_RESULT_LIMIT as u16),
                    }).await {
                        Ok(files) => {
                            Ok(SearchAnswer::Files(files, indexing))
                        }
                        Err(error) => Err(error),
                    }
                };
                let retry = this.update(cx, |surface, cx| {
                    if !surface.search_state.accepts(generation, &query) || surface.request_context.as_ref() != Some(&context) { return false; }
                    surface.search_state.loading = false;
                    match result {
                        Ok(SearchAnswer::Files(files, indexing)) => {
                            if !indexing { surface.search_state.name_index_ready = Some((context.clone(), std::time::Instant::now())); }
                            let selected = surface.search_state.results.get(surface.search_state.active).map(|row| row.path.clone());
                            surface.search_state.results = files;
                            if surface.search_state.displayed_generation == generation {
                                surface.search_state.active = selected.and_then(|path| surface.search_state.results.iter().position(|row| row.path == path)).unwrap_or(surface.search_state.active);
                            }
                            surface.search_state.indexing = indexing;
                        }
                        Ok(SearchAnswer::Code(found)) => {
                            let supported = found.options_supported &&
                                (found.advanced_options_supported || (options.folder.trim().is_empty() && !options.include_ignored));
                            let new_query = surface.search_state.displayed_generation != generation;
                            let selected = surface.search_state.selected_code_match();
                            surface.search_state.content = if supported { found.matches } else { Vec::new() };
                            if !supported {
                                surface.search_state.error = Some("Update the workspace's device to use repository search controls.".into());
                            }
                            surface.search_state.indexing = found.indexing && supported;
                            surface.search_state.truncated = found.truncated;
                            surface.search_state.rebuild_code_rows();
                            if new_query {
                                surface.search_state.active = surface.search_state.code_rows.iter().position(|row| matches!(row, CodeRow::Match(_))).unwrap_or(0);
                            } else if let Some((path, line, column)) = selected {
                                surface.search_state.active = surface.search_state.code_rows.iter().position(|row| match row {
                                    CodeRow::Match(index) => {
                                        let found = &surface.search_state.content[*index];
                                        found.path == path && found.line == line && found.column == column
                                    }
                                    _ => false,
                                }).unwrap_or(surface.search_state.active);
                            }
                        }
                        Err(error) => {
                            surface.search_state.indexing = false;
                            surface.search_state.error = Some(match error {
                                FilesClientError::Unsupported(_) => "Update the workspace's device to search its contents.".into(),
                                FilesClientError::Transport(_) => "The workspace's device is unreachable.".into(),
                                FilesClientError::Request(message) => message.into(),
                                FilesClientError::Encode(_) | FilesClientError::Decode(_) => "Search failed. Try again.".into(),
                            });
                            surface.search_state.results.clear();
                            surface.search_state.content.clear();
                            surface.search_state.code_rows.clear();
                        }
                    }
                    surface.search_state.displayed_generation = generation;
                    let len = surface.search_state.visible_len();
                    surface.search_state.active = surface.search_state.active.min(len.saturating_sub(1));
                    surface.search_list.reset_with_uniform_height(len, px(SEARCH_ROW_HEIGHT));
                    surface.search.update(cx, |search, cx| search.set_mention_controls(true, len > 0, cx));
                    cx.notify();
                    surface.search_state.indexing
                }).unwrap_or(false);
                if !retry { break; }
                cx.background_executor().timer(delay).await;
                delay = (delay * 2).min(Duration::from_secs(2));
            }
        }));
        cx.notify();
    }

    pub(super) fn clear_search(&mut self, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.set_text("", cx));
    }

    pub(super) fn activate_search_result(&mut self, cx: &mut Context<Self>) {
        if self.search_state.loading {
            return;
        }
        if self.search_state.mode == SearchMode::Code {
            self.activate_code_result(cx);
        } else if let Some(result) = self
            .search_state
            .results
            .get(self.search_state.active)
            .cloned()
        {
            if result.kind == WorkspaceEntryKind::Directory {
                self.reveal_search_result(result, cx);
            } else {
                // Open the ranked hit immediately. The shell synchronizes
                // explorer selection in the background and keeps the query.
                self.open_tree_file(result.path, cx);
            }
        }
    }

    pub(super) fn reset_search_results(&mut self) {
        self.search_state.content.clear();
        self.search_state.code_rows.clear();
        self.search_state.collapsed.clear();
        self.search_list.reset(0);
    }

    pub(super) fn cancel_reveal(&mut self) {
        self.search_state.reveal_generation = self.search_state.reveal_generation.wrapping_add(1);
        self.search_state.reveal_task = None;
        self.search_state.reveal_intent = RevealIntent::default();
        self.search_state.reveal_scroll_pending = false;
    }

    pub(super) fn reveal_search_result(
        &mut self,
        result: WorkspaceFileSearchMatch,
        cx: &mut Context<Self>,
    ) {
        let intent = match result.kind {
            WorkspaceEntryKind::Directory => RevealIntent::ActivateDirectory,
            WorkspaceEntryKind::File | WorkspaceEntryKind::Symlink => RevealIntent::OpenFile,
        };
        self.reveal_path(result.path, intent, cx);
    }

    pub(super) fn reveal_path(
        &mut self,
        path: String,
        intent: RevealIntent,
        cx: &mut Context<Self>,
    ) {
        self.cancel_reveal();
        let Some(context) = self.request_context.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let mut ancestors = Vec::new();
        let mut current = parent_path(&path);
        while let Some(directory) = current {
            if directory.is_empty() {
                break;
            }
            ancestors.push(directory.clone());
            current = parent_path(&directory);
        }
        ancestors.reverse();
        let directories = std::iter::once(String::new())
            .chain(ancestors.clone())
            .collect::<Vec<_>>();
        let generation = self.tree.generation();
        let reveal_generation = self.search_state.reveal_generation;
        let include_ignored = self.tree.include_ignored();
        let client = WorkspaceFilesClient::new(engine, context.clone());
        self.search_state.reveal_intent = intent;
        self.search_state.reveal_task = Some(cx.spawn(async move |this, cx| {
            let mut pages = Vec::with_capacity(directories.len());
            for (index, directory) in directories.into_iter().enumerate() {
                // Walk through pagination until the next ancestor (or file) is found.
                let required = ancestors.get(index).unwrap_or(&path).clone();
                match client
                    .list_directory_snapshot(
                        ListWorkspaceDirectoryRequest {
                            target: context.target.clone(),
                            directory,
                            include_ignored,
                            cursor: None,
                        },
                        &[required],
                    )
                    .await
                {
                    Ok(page) => pages.push(page),
                    Err(error) => {
                        let _ = this.update(cx, |surface, cx| {
                            if surface.accepts_reveal(&context, generation, reveal_generation) {
                                surface.search_state.error = Some(error.to_string().into());
                                cx.notify();
                            }
                        });
                        return;
                    }
                }
            }
            let _ = this.update(cx, |surface, cx| {
                if !surface.accepts_reveal(&context, generation, reveal_generation) {
                    return;
                }
                for (index, page) in pages.into_iter().enumerate() {
                    surface.tree.apply_page(page, generation);
                    if let Some(next) = ancestors.get(index) {
                        surface.tree.expand(next);
                    }
                }
                surface.tree.select(path.clone());
                surface.sync_tree_list();
                surface.search_state.reveal_intent = RevealIntent::default();
                surface.finish_reveal(path, intent, cx);
            });
        }));
    }

    fn accepts_reveal(
        &self,
        context: &super::client::FilesRequestContext,
        tree_generation: u64,
        reveal_generation: u64,
    ) -> bool {
        self.request_context.as_ref() == Some(context)
            && self.tree.generation() == tree_generation
            && self.search_state.reveal_generation == reveal_generation
    }

    fn finish_reveal(&mut self, path: String, intent: RevealIntent, cx: &mut Context<Self>) {
        if intent.clears_search() {
            self.search.update(cx, |search, cx| search.set_text("", cx));
        }
        if intent.opens_file() {
            self.selected_editor_path = Some(path.clone());
            self.open_tree_file(path, cx);
        }
        if self.search_state.query.is_empty() {
            self.reveal_tree_selection();
        } else {
            self.search_state.reveal_scroll_pending = true;
        }
        cx.notify();
    }

    pub(super) fn render_search_results(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        if let Some(error) = self.search_state.error.clone() {
            return centered_search_message(error, theme.danger.opacity(0.82));
        }
        if self.search_state.visible_len() == 0 {
            let label = if self.search_state.indexing {
                "Indexing workspace…"
            } else if self.search_state.loading {
                "Searching…"
            } else {
                if self.search_state.mode == SearchMode::Code {
                    "No code matches found."
                } else {
                    "No files found."
                }
            };
            return centered_search_message(label.into(), theme.text_faint);
        }
        div()
            .id("files-search-results")
            .role(gpui::Role::List)
            .aria_label("Workspace search results")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_search_status(&theme, cx))
            .child(
                list(
                    self.search_list.clone(),
                    cx.processor(Self::render_search_row),
                )
                .flex_1()
                .min_h_0()
                .with_sizing_behavior(ListSizingBehavior::Auto),
            )
            .into_any_element()
    }

    fn render_search_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.search_state.mode == SearchMode::Code {
            return self.render_code_row(index, window, cx);
        }
        let Some(row) = self.search_state.results.get(index).cloned() else {
            return gpui::Empty.into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let selected = self.search_state.active == index;
        let expanded = false;
        let is_directory = row.kind == WorkspaceEntryKind::Directory;
        let decoration = self.git_decoration(&row.path, is_directory, cx);
        let padding = 8.0;
        let drag_payload = WorkspacePathDrag::new(row.path.clone(), is_directory).with_origin(
            self.interaction_origin(cx),
            super::WorkspacePathSource::Search,
            None,
        );
        div()
            .id(("files-search-result", index))
            .role(gpui::Role::ListItem)
            .aria_label(row.name.clone())
            .aria_selected(selected)
            .h(px(SEARCH_ROW_HEIGHT))
            .w_full()
            .flex_none()
            .pl(px(padding))
            .pr(px(8.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .cursor_pointer()
            .when(selected, |element| element.bg(crate::theme::wash(0.1)))
            .when(!selected, |element| {
                element.hover_bg(
                    ("files-search-row-wash", index),
                    window,
                    gpui::transparent_black(),
                    crate::theme::wash(0.055),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.search_state.active = index;
                this.activate_search_result(cx);
            }))
            .on_drag(drag_payload, |payload, _, _, cx| {
                cx.stop_propagation();
                workspace_path_drag_ghost(payload, cx)
            })
            .child({
                let identity = match row.kind {
                    WorkspaceEntryKind::Directory => {
                        FileIconIdentity::directory(&row.name, expanded)
                    }
                    WorkspaceEntryKind::File => FileIconIdentity::file(&row.name),
                    WorkspaceEntryKind::Symlink => FileIconIdentity::symlink(&row.name),
                };
                file_icons::icon(identity, theme.appearance)
                    .size(px(14.0))
                    .flex_none()
            })
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.font_sans.clone())
                    .text_size(px(11.5))
                    .text_color(if let Some(value) = decoration {
                        value.color(&theme)
                    } else if is_directory {
                        theme.text_muted
                    } else {
                        theme.text
                    })
                    .child(crate::popover::search_highlight(
                        row.name.into(),
                        Some(&self.search_state.query),
                        &theme,
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(10.0))
                    .text_color(theme.text_faint)
                    .child(parent_path(&row.path).unwrap_or_default().to_owned()),
            )
            .into_any_element()
    }
}

fn centered_search_message(message: SharedString, color: gpui::Hsla) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px(px(24.0))
        .text_center()
        .text_size(px(11.5))
        .text_color(color)
        .child(message)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search_match(path: &str, kind: WorkspaceEntryKind, score: i64) -> WorkspaceFileSearchMatch {
        WorkspaceFileSearchMatch {
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            kind,
            score,
        }
    }

    #[test]
    fn search_state_rejects_stale_queries() {
        let state = FileSearchState {
            query: "shell".into(),
            generation: 4,
            ..Default::default()
        };
        assert!(state.accepts(4, "shell"));
        assert!(!state.accepts(3, "shell"));
        assert!(!state.accepts(4, "other"));
    }

    #[test]
    fn file_results_preserve_rank_without_inserting_ancestor_rows() {
        let state = FileSearchState {
            results: vec![
                search_match("deep/src/best.rs", WorkspaceEntryKind::File, 100),
                search_match("README.md", WorkspaceEntryKind::File, 90),
            ],
            ..Default::default()
        };
        assert_eq!(state.visible_len(), 2);
        assert_eq!(state.results[0].path, "deep/src/best.rs");
    }
}

#[cfg(test)]
mod reveal_tests {
    use super::*;
    use crate::files::{FilesEvent, client::FilesRequestContext};
    use gpui::{AppContext, TestAppContext};
    use std::{cell::RefCell, rc::Rc};

    fn context(checkout: &str) -> FilesRequestContext {
        FilesRequestContext {
            target: zeron_proto::WorkspaceTarget {
                chat_id: Some("chat".into()),
                space_id: None,
                checkout_path: None,
            },
            target_device_id: None,
            cwd: format!("/workspace/{checkout}"),
            checkout_id: Some(checkout.into()),
        }
    }

    #[gpui::test]
    fn selecting_a_tab_preserves_search_and_never_opens_another_file(cx: &mut TestAppContext) {
        let surface = cx.new(|cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new_explorer(state, "chat".into(), false, cx)
        });
        let opened = Rc::new(RefCell::new(Vec::new()));
        let events = opened.clone();
        let _sub = cx.update(|cx| {
            cx.subscribe(&surface, move |_, event, _| {
                if let FilesEvent::OpenFile(path) = event {
                    events.borrow_mut().push(path.clone());
                }
            })
        });
        surface.update(cx, |surface, cx| {
            surface
                .search
                .update(cx, |search, cx| search.set_text("config", cx));
        });
        surface.update(cx, |surface, cx| {
            surface.finish_reveal("src/main.rs".into(), RevealIntent::SynchronizeSelection, cx);
            assert_eq!(surface.search.read(cx).text(), "config");
            assert_eq!(surface.search_state.query, "config");
            assert!(surface.search_state.reveal_scroll_pending);
        });
        assert!(opened.borrow().is_empty());
        surface.update(cx, |surface, cx| {
            surface.finish_reveal("src/config.rs".into(), RevealIntent::OpenFile, cx);
        });
        assert_eq!(*opened.borrow(), ["src/config.rs"]);
        surface.read_with(cx, |surface, cx| {
            assert!(surface.search.read(cx).text().is_empty());
            assert!(!surface.search_state.reveal_scroll_pending);
        });
    }

    #[gpui::test]
    fn activating_a_directory_clears_search_without_opening_a_file(cx: &mut TestAppContext) {
        let surface = cx.new(|cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new_explorer(state, "chat".into(), false, cx)
        });
        let opened = Rc::new(RefCell::new(Vec::new()));
        let events = opened.clone();
        let _sub = cx.update(|cx| {
            cx.subscribe(&surface, move |_, event, _| {
                if let FilesEvent::OpenFile(path) = event {
                    events.borrow_mut().push(path.clone());
                }
            })
        });
        surface.update(cx, |surface, cx| {
            surface
                .search
                .update(cx, |search, cx| search.set_text("emptydir", cx));
            surface.finish_reveal("emptydir".into(), RevealIntent::ActivateDirectory, cx);
        });
        surface.read_with(cx, |surface, cx| {
            assert!(surface.search.read(cx).text().is_empty());
            assert!(surface.search_state.query.is_empty());
            assert!(!surface.search_state.reveal_scroll_pending);
        });
        assert!(opened.borrow().is_empty());
    }

    #[gpui::test]
    fn checkout_switch_and_newer_reveal_reject_old_search_results(cx: &mut TestAppContext) {
        let surface = cx.new(|cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new_explorer(state, "chat".into(), false, cx)
        });
        surface.update(cx, |surface, cx| {
            let first = context("first");
            surface.apply_target(Some(first.clone()), cx);
            let tree_generation = surface.tree.generation();
            let reveal_generation = surface.search_state.reveal_generation;
            assert!(surface.accepts_reveal(&first, tree_generation, reveal_generation));
            surface.cancel_reveal();
            assert!(!surface.accepts_reveal(&first, tree_generation, reveal_generation));
            let reveal_generation = surface.search_state.reveal_generation;
            surface.search_state.query = "config".into();
            let search_generation = surface.search_state.generation;
            surface.apply_target(Some(context("second")), cx);
            assert!(!surface.accepts_reveal(&first, tree_generation, reveal_generation));
            assert!(!surface.search_state.accepts(search_generation, "config"));
            assert!(surface.search_state.results.is_empty());
        });
    }
}
