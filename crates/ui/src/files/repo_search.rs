//! Repository search controls and virtualized, file-grouped code matches.
use super::{
    FilesEvent, FilesSurface,
    search::{CodeRow, FileSearchState, SEARCH_ROW_HEIGHT, SearchMode},
    toolbar, toolbar_button,
};
use crate::{
    composer::{ComposerInput, ComposerInputEvent},
    file_icons::{self, FileIconIdentity},
    icons,
    motion::HoverFade as _,
    theme::Theme,
};
use gpui::{
    AnyElement, Context, Entity, Focusable as _, SharedString, Subscription, Window, div,
    prelude::*, px,
};
use std::collections::BTreeMap;

pub(super) struct SearchFilters {
    folder: Entity<ComposerInput>,
    include: Entity<ComposerInput>,
    exclude: Entity<ComposerInput>,
    open: bool,
    _events: Vec<Subscription>,
}

impl SearchFilters {
    pub fn new(cx: &mut Context<FilesSurface>) -> Self {
        let make = |placeholder, cx: &mut Context<FilesSurface>| {
            cx.new(|cx| {
                ComposerInput::new(placeholder, cx)
                    .with_single_line()
                    .with_text_metrics(11.0, 16.0)
            })
        };
        let folder = make("Workspace-relative folder (empty = workspace)", cx);
        let include = make("e.g. *.rs, *.ts", cx);
        let exclude = make("e.g. tests, **/*.test.ts", cx);
        let events = [&folder, &include, &exclude]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this: &mut FilesSurface, _, event, cx| {
                    if matches!(event, ComposerInputEvent::Edited) {
                        this.search_state.options.folder =
                            this.search_filters.folder.read(cx).text().to_string();
                        this.search_state.options.include =
                            this.search_filters.include.read(cx).text().to_string();
                        this.search_state.options.exclude =
                            this.search_filters.exclude.read(cx).text().to_string();
                        if this.search_state.mode == SearchMode::Code {
                            this.run_workspace_search(this.search_state.query.clone(), cx);
                        }
                    }
                })
            })
            .collect();
        Self {
            folder,
            include,
            exclude,
            open: false,
            _events: events,
        }
    }
}

impl FileSearchState {
    pub(super) fn selected_code_match(&self) -> Option<(String, u64, u64)> {
        let CodeRow::Match(index) = self.code_rows.get(self.active)? else {
            return None;
        };
        let found = self.content.get(*index)?;
        Some((found.path.clone(), found.line, found.column))
    }
    pub(super) fn rebuild_code_rows(&mut self) {
        let mut files: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (index, found) in self.content.iter().enumerate() {
            files.entry(&found.path).or_default().push(index);
        }
        self.code_rows.clear();
        for (path, mut matches) in files {
            matches.sort_by_key(|&index| (self.content[index].line, self.content[index].column));
            self.code_rows
                .push(CodeRow::File(path.to_owned(), matches.len()));
            if !self.collapsed.contains(path) {
                self.code_rows
                    .extend(matches.into_iter().map(CodeRow::Match));
            }
        }
    }
}

impl FilesSurface {
    pub(super) fn toggle_search_details(&mut self, cx: &mut Context<Self>) {
        self.search_filters.open = !self.search_filters.open;
        cx.notify();
    }
    pub(super) fn search_in_folder(
        &mut self,
        folder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_filters
            .folder
            .update(cx, |input, cx| input.set_text(folder.clone(), cx));
        self.search_state.options.folder = folder;
        self.search_filters.open = true;
        self.set_search_mode(SearchMode::Code, cx);
        self.run_workspace_search(self.search_state.query.clone(), cx);
        self.search.focus_handle(cx).focus(window, cx);
    }

    fn set_search_groups_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        let selected = self.search_state.selected_code_match().map(|hit| hit.0);
        self.search_state.collapsed = if collapsed {
            self.search_state
                .content
                .iter()
                .map(|found| found.path.clone())
                .collect()
        } else {
            Default::default()
        };
        self.search_state.rebuild_code_rows();
        self.search_state.active = selected
            .and_then(|path| {
                self.search_state
                    .code_rows
                    .iter()
                    .position(|row| matches!(row, CodeRow::File(found, _) if *found == path))
            })
            .unwrap_or(0);
        let len = self.search_state.visible_len();
        self.search_list
            .reset_with_uniform_height(len, px(SEARCH_ROW_HEIGHT));
        self.search.update(cx, |input, cx| {
            input.set_mention_controls(true, len > 0, cx)
        });
        cx.notify();
    }

    fn copy_search_results(&self, cx: &mut Context<Self>) {
        let text = self
            .search_state
            .content
            .iter()
            .map(|found| format!("{}:{}: {}", found.path, found.line, found.preview))
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
    }

    #[cfg(test)]
    pub(crate) fn test_reset_repo_search(&mut self, cx: &mut Context<Self>) {
        self.close_search(cx);
    }
    #[cfg(test)]
    pub(crate) fn test_repo_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.search
            .update(cx, |input, cx| input.set_text(query, cx));
    }

    #[cfg(test)]
    pub(crate) fn test_repo_scope(
        &mut self,
        folder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_in_folder(folder.into(), window, cx);
        self.search_filters
            .include
            .update(cx, |input, cx| input.set_text("*.rs", cx));
    }

    #[cfg(test)]
    pub(crate) fn test_repo_matches(&self) -> Option<&[zeron_proto::WorkspaceContentMatch]> {
        (!self.search_state.loading
            && !self.search_state.indexing
            && self.search_state.error.is_none())
        .then_some(&self.search_state.content)
    }

    #[cfg(test)]
    pub(crate) fn test_open_first_code_match(&mut self, cx: &mut Context<Self>) {
        self.search_state.active = self
            .search_state
            .code_rows
            .iter()
            .position(|row| matches!(row, CodeRow::Match(_)))
            .unwrap();
        self.activate_search_result(cx);
    }

    pub(crate) fn focus_code_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_search_mode(SearchMode::Code, cx);
        let focus = self.search.focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));
    }

    fn set_search_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        if self.search_state.mode == mode {
            return;
        }
        self.cancel_reveal();
        self.search_state.mode = mode;
        if mode == SearchMode::Code && self.search_state.options.match_case.is_none() {
            self.search_state.options.match_case = Some(false);
        }
        self.search_state.results.clear();
        self.reset_search_results();
        self.search.update(cx, |search, cx| {
            search.set_placeholder(
                if mode == SearchMode::Code {
                    "Search code"
                } else {
                    "Search files"
                },
                cx,
            )
        });
        let text = self.search.read(cx).text().to_string();
        self.run_workspace_search(text, cx);
    }

    pub(super) fn render_repo_controls(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.search_state.mode;
        let controls = toolbar(theme)
            .id("repo-search-controls")
            .debug_selector(|| "repo-search-controls".into())
            .children(
                [
                    (SearchMode::Files, "repo-search-files", "Files"),
                    (SearchMode::Code, "repo-search-code", "Code"),
                ]
                .into_iter()
                .map(|(value, id, label)| {
                    toolbar_button(id, label)
                        .debug_selector(move || id.into())
                        .w(px(45.0))
                        .role(gpui::Role::Tab)
                        .aria_selected(value == mode)
                        .text_size(px(11.0))
                        .text_color(if value == mode {
                            theme.text
                        } else {
                            theme.text_muted
                        })
                        .when(value == mode, |el| el.bg(theme.glass_hover()))
                        .child(label)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_search_mode(value, cx);
                            this.search.focus_handle(cx).focus(window, cx);
                        }))
                }),
            )
            .when(mode == SearchMode::Code, |el| {
                el.child(div().flex_1())
                    .child(
                        toolbar_button("repo-search-case", "Match case")
                            .aria_selected(self.search_state.options.match_case == Some(true))
                            .when(self.search_state.options.match_case == Some(true), |el| {
                                el.bg(theme.glass_hover())
                            })
                            .child("Aa")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.search_state.options.match_case =
                                    Some(this.search_state.options.match_case != Some(true));
                                this.run_workspace_search(this.search_state.query.clone(), cx);
                            })),
                    )
                    .child(
                        toolbar_button("repo-search-word", "Match whole word")
                            .aria_selected(self.search_state.options.whole_word)
                            .when(self.search_state.options.whole_word, |el| {
                                el.bg(theme.glass_hover())
                            })
                            .child("ab")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.search_state.options.whole_word =
                                    !this.search_state.options.whole_word;
                                this.run_workspace_search(this.search_state.query.clone(), cx);
                            })),
                    )
                    .child(
                        toolbar_button("repo-search-regex", "Use regular expression")
                            .aria_selected(self.search_state.options.use_regex)
                            .when(self.search_state.options.use_regex, |el| {
                                el.bg(theme.glass_hover())
                            })
                            .child(".*")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.search_state.options.use_regex =
                                    !this.search_state.options.use_regex;
                                this.run_workspace_search(this.search_state.query.clone(), cx);
                            })),
                    )
                    .child(
                        toolbar_button("repo-search-filters", "Search details (Ctrl+Shift+J)")
                            .debug_selector(|| "repo-search-filters".into())
                            .w(px(46.0))
                            .aria_expanded(self.search_filters.open)
                            .when(
                                self.search_filters.open
                                    || !self.search_state.options.folder.is_empty()
                                    || self.search_state.options.include_ignored
                                    || !self.search_state.options.include.is_empty()
                                    || !self.search_state.options.exclude.is_empty(),
                                |el| el.bg(theme.glass_hover()),
                            )
                            .child("Filters")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_search_details(cx);
                            })),
                    )
            });
        div()
            .flex_none()
            .flex()
            .flex_col()
            .child(controls)
            .when(mode == SearchMode::Code && self.search_filters.open, |el| {
                el.children(
                    [
                        (
                            "repo-search-folder",
                            "Search in folder",
                            self.search_filters.folder.clone(),
                        ),
                        (
                            "repo-search-include",
                            "Files to include",
                            self.search_filters.include.clone(),
                        ),
                        (
                            "repo-search-exclude",
                            "Files to exclude",
                            self.search_filters.exclude.clone(),
                        ),
                    ]
                    .into_iter()
                    .map(|(id, label, input)| {
                        let focus = input.focus_handle(cx);
                        div()
                            .px(px(10.0))
                            .py(px(4.0))
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .text_size(px(10.0))
                            .text_color(theme.text_faint)
                            .child(label)
                            .child(
                                crate::surface_chrome::input()
                                    .id(id)
                                    .debug_selector(move || id.into())
                                    .h(px(26.0))
                                    .w_full()
                                    .cursor_text()
                                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                                        focus.focus(window, cx)
                                    })
                                    .child(input),
                            )
                    }),
                )
                .child(
                    div()
                        .px(px(10.0))
                        .pb(px(4.0))
                        .text_size(px(10.0))
                        .text_color(theme.text_faint)
                        .child("Comma-separated globs · ./ anchors to workspace"),
                )
                .child(
                    div().px(px(10.0)).pb(px(6.0)).child(
                        toolbar_button(
                            "repo-search-ignored",
                            "Include files excluded by ignore rules",
                        )
                        .debug_selector(|| "repo-search-ignored".into())
                        .w_full()
                        .text_size(px(11.0))
                        .aria_selected(self.search_state.options.include_ignored)
                        .when(self.search_state.options.include_ignored, |el| {
                            el.bg(theme.glass_hover())
                        })
                        .child("Include ignored files")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.search_state.options.include_ignored =
                                !this.search_state.options.include_ignored;
                            this.run_workspace_search(this.search_state.query.clone(), cx);
                        })),
                    ),
                )
            })
            .into_any_element()
    }

    pub(super) fn render_search_status(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.search_state;
        let label = if state.indexing {
            "Indexing workspace…".to_owned()
        } else if state.loading {
            "Searching…".to_owned()
        } else if state.mode == SearchMode::Code {
            let files = state
                .code_rows
                .iter()
                .filter(|row| matches!(row, CodeRow::File(..)))
                .count();
            format!(
                "{} matching line{} in {} file{}{}",
                state.content.len(),
                if state.content.len() == 1 { "" } else { "s" },
                files,
                if files == 1 { "" } else { "s" },
                if state.truncated {
                    " · Results limited; narrow your search"
                } else {
                    ""
                }
            )
        } else {
            format!(
                "{} file{}{}",
                state.results.len(),
                if state.results.len() == 1 { "" } else { "s" },
                if state.results.len() >= 200 {
                    " · First 200 matches"
                } else {
                    ""
                }
            )
        };
        div()
            .min_h(px(26.0))
            .px(px(10.0))
            .flex_none()
            .flex()
            .items_center()
            .text_size(px(10.0))
            .text_color(theme.text_faint)
            .child(div().flex_1().min_w_0().truncate().child(label))
            .children((state.mode == SearchMode::Code).then(|| {
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(
                        toolbar_button("repo-search-refresh", "Refresh search")
                            .debug_selector(|| "repo-search-refresh".into())
                            .child(
                                icons::icon(icons::REFRESH)
                                    .size(px(12.0))
                                    .text_color(theme.text_muted),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.run_workspace_search(this.search_state.query.clone(), cx)
                            })),
                    )
                    .child(
                        toolbar_button("repo-search-collapse", "Collapse all results")
                            .debug_selector(|| "repo-search-collapse".into())
                            .child(
                                icons::icon(icons::ALT_ARROW_UP)
                                    .size(px(12.0))
                                    .text_color(theme.text_muted),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_search_groups_collapsed(true, cx)
                            })),
                    )
                    .child(
                        toolbar_button("repo-search-expand", "Expand all results")
                            .debug_selector(|| "repo-search-expand".into())
                            .child(
                                icons::icon(icons::ALT_ARROW_DOWN)
                                    .size(px(12.0))
                                    .text_color(theme.text_muted),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_search_groups_collapsed(false, cx)
                            })),
                    )
                    .child(
                        toolbar_button("repo-search-copy", "Copy search results")
                            .debug_selector(|| "repo-search-copy".into())
                            .child(
                                icons::icon(icons::COPY)
                                    .size(px(12.0))
                                    .text_color(theme.text_muted),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.copy_search_results(cx))),
                    )
                    .child(
                        toolbar_button("repo-search-clear", "Clear search results")
                            .debug_selector(|| "repo-search-clear".into())
                            .child(
                                icons::icon(icons::CLOSE)
                                    .size(px(12.0))
                                    .text_color(theme.text_muted),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.clear_search(cx))),
                    )
            }))
            .into_any_element()
    }

    pub(super) fn activate_code_result(&mut self, cx: &mut Context<Self>) {
        match self
            .search_state
            .code_rows
            .get(self.search_state.active)
            .cloned()
        {
            Some(CodeRow::File(path, _)) => {
                if !self.search_state.collapsed.remove(&path) {
                    self.search_state.collapsed.insert(path.clone());
                }
                self.search_state.rebuild_code_rows();
                self.search_state.active = self
                    .search_state
                    .code_rows
                    .iter()
                    .position(
                        |row| matches!(row, CodeRow::File(candidate, _) if candidate == &path),
                    )
                    .unwrap_or(0);
                self.search_list.reset_with_uniform_height(
                    self.search_state.visible_len(),
                    px(SEARCH_ROW_HEIGHT),
                );
                cx.notify();
            }
            Some(CodeRow::Match(index)) => {
                if let Some(found) = self.search_state.content.get(index) {
                    cx.emit(FilesEvent::OpenFileAt {
                        path: found.path.clone(),
                        line: found.line.min(u32::MAX as u64) as u32,
                        column: found.column,
                    });
                }
            }
            None => {}
        }
    }

    pub(super) fn render_code_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.search_state.code_rows.get(index).cloned() else {
            return gpui::Empty.into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let selected = self.search_state.active == index;
        let mut element = div()
            .id(("repo-code-result", index))
            .h(px(SEARCH_ROW_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .gap(px(5.0))
            .px(px(10.0))
            .cursor_pointer()
            .role(gpui::Role::ListItem)
            .aria_selected(selected)
            .when(selected, |el| el.bg(theme.glass_hover()))
            .when(!selected, |el| {
                el.hover_bg(
                    ("repo-code-row-wash", index),
                    window,
                    gpui::transparent_black(),
                    crate::theme::wash(0.055),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.search_state.active = index;
                this.activate_search_result(cx);
            }));
        match row {
            CodeRow::File(path, count) => {
                let expanded = !self.search_state.collapsed.contains(&path);
                element = element
                    .aria_label(path.clone())
                    .aria_expanded(expanded)
                    .child(
                        icons::disclosure_chevron(
                            SharedString::from(format!("code-chevron-{path}")),
                            expanded,
                            window,
                            cx,
                        )
                        .size(px(10.0))
                        .text_color(theme.text_faint),
                    )
                    .child(
                        file_icons::icon(FileIconIdentity::file(&path), theme.appearance)
                            .size(px(14.0)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(theme.text)
                            .child(path),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(theme.text_faint)
                            .child(count.to_string()),
                    );
            }
            CodeRow::Match(match_index) => {
                let found = &self.search_state.content[match_index];
                element = element
                    .aria_label(format!("{}:{} {}", found.path, found.line, found.preview))
                    .child(
                        div()
                            .w(px(36.0))
                            .flex_none()
                            .text_right()
                            .font_family(theme.font_mono.clone())
                            .text_size(px(10.0))
                            .text_color(theme.text_faint)
                            .child(found.line.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme.font_mono.clone())
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(crate::popover::search_highlight_ranges(
                                found.preview.clone().into(),
                                found
                                    .ranges
                                    .iter()
                                    .map(|&(start, end)| start as usize..end as usize)
                                    .collect(),
                                &theme,
                            )),
                    );
            }
        }
        element.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext};

    fn found(path: &str, line: u64, column: u64) -> zeron_proto::WorkspaceContentMatch {
        zeron_proto::WorkspaceContentMatch {
            path: path.into(),
            line,
            column,
            preview: "😀 needle".into(),
            ranges: vec![(5, 11)],
        }
    }

    #[gpui::test]
    fn repository_search_opens_exact_location_and_keeps_query_and_groups(cx: &mut TestAppContext) {
        cx.update(|cx| crate::composer::init(cx, Default::default()));
        let (files, cx) = super::super::test_support::setup(cx);
        let opened = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let events = opened.clone();
        let _events = cx.update(|_, cx| {
            cx.subscribe(&files, move |_, event, _| {
                if let FilesEvent::OpenFileAt { path, line, column } = event {
                    events.borrow_mut().push((path.clone(), *line, *column));
                }
            })
        });
        files.update_in(cx, |files, window, cx| {
            files.focus_code_search(window, cx);
            files
                .search
                .update(cx, |input, cx| input.set_text("needle", cx));
        });
        cx.run_until_parked();
        files.update_in(cx, |files, window, cx| {
            assert_eq!(files.search_state.query, "needle");
            files.search.focus_handle(cx).focus(window, cx);
            files.search_state.error = None;
            files.search_state.loading = false;
            files.search_state.content = vec![
                found("src/a.rs", 7, 5),
                found("src/a.rs", 2, 10),
                found("src/b.rs", 4, 0),
            ];
            files.search_state.rebuild_code_rows();
            files.search_state.active = 1;
            files
                .search_list
                .reset_with_uniform_height(5, px(SEARCH_ROW_HEIGHT));
            files
                .search
                .update(cx, |input, cx| input.set_mention_controls(true, true, cx));
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        assert_eq!(*opened.borrow(), [("src/a.rs".into(), 2, 10)]);
        files.read_with(cx, |files, cx| {
            assert_eq!(files.search.read(cx).text(), "needle")
        });
        files.update(cx, |files, cx| {
            files.search_state.active = 0;
            files.activate_code_result(cx);
        });
        files.read_with(cx, |files, _| {
            assert_eq!(files.search_state.code_rows.len(), 3);
            assert!(files.search_state.collapsed.contains("src/a.rs"));
        });
        files.update(cx, |files, cx| files.activate_code_result(cx));
        files.read_with(cx, |files, _| {
            assert_eq!(files.search_state.code_rows.len(), 5)
        });
    }

    #[gpui::test]
    fn repository_search_controls_remain_discoverable_and_filters_start_collapsed(
        cx: &mut TestAppContext,
    ) {
        let (files, cx) = super::super::test_support::setup(cx);
        cx.update(|window, cx| window.draw(cx).clear());
        if cfg!(target_os = "linux") {
            assert!(cx.debug_bounds("files-sections").is_none());
        }
        let code = cx.debug_bounds("repo-search-code").unwrap().center();
        cx.simulate_click(code, Modifiers::default());
        files.read_with(cx, |files, _| {
            assert_eq!(files.search_state.mode, SearchMode::Code);
            assert!(!files.search_filters.open);
        });
        files.update(cx, |files, cx| {
            files.search_filters.open = true;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(cx.debug_bounds("repo-search-include").is_some());
        assert!(cx.debug_bounds("repo-search-exclude").is_some());
        assert!(cx.debug_bounds("repo-search-folder").is_some());
        assert!(cx.debug_bounds("repo-search-ignored").is_some());
    }

    #[gpui::test]
    fn repository_search_folder_filters_and_result_actions_preserve_query(cx: &mut TestAppContext) {
        cx.update(|cx| crate::composer::init(cx, Default::default()));
        let (files, cx) = super::super::test_support::setup(cx);
        files.update_in(cx, |files, window, cx| {
            files.search_in_folder("src".into(), window, cx);
            files
                .search
                .update(cx, |input, cx| input.set_text("needle", cx));
            files
                .search_filters
                .include
                .update(cx, |input, cx| input.set_text("*.rs", cx));
        });
        cx.run_until_parked();
        files.read_with(cx, |files, _| {
            assert_eq!(files.search_state.options.folder, "src");
            assert_eq!(files.search_state.options.include, "*.rs");
            assert!(!files.search_state.options.include_ignored);
        });
        cx.update(|window, cx| window.draw(cx).clear());
        let toggle = cx.debug_bounds("repo-search-ignored").unwrap().center();
        cx.simulate_click(toggle, Modifiers::default());
        files.update(cx, |files, cx| {
            assert!(files.search_state.options.include_ignored);
            files.search_state.error = None;
            files.search_state.loading = false;
            files.search_state.content = vec![
                found("src/a.rs", 1, 5),
                found("src/a.rs", 7, 5),
                found("src/b.rs", 2, 5),
            ];
            files.search_state.rebuild_code_rows();
            files
                .search_list
                .reset_with_uniform_height(5, px(SEARCH_ROW_HEIGHT));
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear());
        let collapse = cx.debug_bounds("repo-search-collapse").unwrap().center();
        cx.simulate_click(collapse, Modifiers::default());
        files.read_with(cx, |files, _| {
            assert_eq!(files.search_state.code_rows.len(), 2)
        });
        cx.update(|window, cx| window.draw(cx).clear());
        let expand = cx.debug_bounds("repo-search-expand").unwrap().center();
        cx.simulate_click(expand, Modifiers::default());
        files.read_with(cx, |files, cx| {
            assert_eq!(files.search_state.code_rows.len(), 5);
            assert_eq!(files.search.read(cx).text(), "needle");
        });
        files.update(cx, |files, cx| files.copy_search_results(cx));
        cx.update(|_, cx| {
            assert!(
                cx.read_from_clipboard()
                    .unwrap()
                    .text()
                    .unwrap()
                    .contains("src/b.rs:2: 😀 needle")
            )
        });
        let clear = cx.debug_bounds("repo-search-clear").unwrap().center();
        cx.simulate_click(clear, Modifiers::default());
        cx.run_until_parked();
        files.read_with(cx, |files, cx| {
            assert!(files.search.read(cx).text().is_empty());
            assert_eq!(files.search_state.options.folder, "src");
        });
        files.update_in(cx, |files, window, cx| {
            files.search.focus_handle(cx).focus(window, cx)
        });
        let shortcut = if cfg!(target_os = "macos") {
            "cmd-shift-j"
        } else {
            "ctrl-shift-j"
        };
        cx.simulate_keystrokes(shortcut);
        files.read_with(cx, |files, _| assert!(!files.search_filters.open));
        cx.simulate_keystrokes(shortcut);
        files.read_with(cx, |files, _| assert!(files.search_filters.open));
    }
}
