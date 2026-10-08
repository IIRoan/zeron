use super::*;

pub(super) type MenuBounds = std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>;

/// The index and worktree copies of a partially staged file are separate rows.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct FileKey {
    pub repository: String,
    pub path: String,
    pub staged: bool,
}

impl FileKey {
    pub(super) fn new(repository: &str, path: &str, staged: bool) -> Self {
        Self {
            repository: repository.into(),
            path: path.into(),
            staged,
        }
    }

    pub(super) fn same_group(&self, other: &Self) -> bool {
        self.repository == other.repository && self.staged == other.staged
    }
}

impl SourceControl {
    /// Covers the list and its header controls. Floating menus can extend
    /// beyond the panel in a narrow window and still belong to this surface.
    pub(crate) fn selection_surface(
        &mut self,
        content: gpui::Div,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        content
            .id("source-control-surface")
            .debug_selector(|| "source-control-surface".into())
            .on_mouse_down_out(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                if this.confirmation.is_some() {
                    return;
                }
                let in_menu = [
                    (this.actions_menu.get().is_some(), &this.actions_menu_bounds),
                    (this.commit_menu.get().is_some(), &this.commit_menu_bounds),
                    (this.file_menu.get().is_some(), &this.file_menu_bounds),
                ]
                .iter()
                .any(|(open, bounds)| {
                    *open
                        && bounds
                            .get()
                            .is_some_and(|bounds| bounds.contains(&event.position))
                });
                if !in_menu && (!this.selected_files.is_empty() || this.selected.is_some()) {
                    this.selected_files.clear();
                    this.selection_anchor = None;
                    this.selected = None;
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    pub(super) fn visible_file_keys(&self) -> Vec<FileKey> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        self.rows
            .iter()
            .filter_map(|row| {
                let Row::File {
                    repository,
                    file,
                    staged,
                } = row
                else {
                    return None;
                };
                let repo = &snapshot.repositories[*repository];
                Some(FileKey::new(&repo.path, &repo.files[*file].path, *staged))
            })
            .collect()
    }

    fn valid_file_keys(&self) -> HashSet<FileKey> {
        self.snapshot
            .iter()
            .flat_map(|snapshot| &snapshot.repositories)
            .flat_map(|repo| {
                repo.files.iter().flat_map(move |file| {
                    [true, false].into_iter().filter_map(move |staged| {
                        in_group(file, staged).then(|| FileKey::new(&repo.path, &file.path, staged))
                    })
                })
            })
            .collect()
    }

    pub(super) fn select_files(&mut self, key: FileKey, extend: bool, toggle: bool) {
        // Keep the repository's input/draft active. Mixed index/worktree
        // selections are allowed; each resource action filters its own side.
        if self
            .selected_files
            .iter()
            .any(|selected| selected.repository != key.repository)
        {
            self.selected_files.clear();
            self.selection_anchor = None;
        }
        if extend {
            let rows = self.visible_file_keys();
            let range = self
                .selection_anchor
                .as_ref()
                .filter(|anchor| anchor.repository == key.repository)
                .and_then(|anchor| {
                    Some((
                        rows.iter().position(|row| row == anchor)?,
                        rows.iter().position(|row| row == &key)?,
                    ))
                });
            if let Some((start, end)) = range {
                if !toggle { self.selected_files.clear(); }
                self.selected_files.extend(
                    rows[start.min(end)..=start.max(end)]
                        .iter()
                        .filter(|row| row.repository == key.repository)
                        .cloned(),
                );
            } else {
                self.selected_files.insert(key.clone());
                self.selection_anchor = Some(key);
            }
        } else if toggle {
            if !self.selected_files.remove(&key) {
                self.selected_files.insert(key.clone());
            }
            self.selection_anchor = Some(key);
        } else {
            self.selected_files.clear();
            self.selected_files.insert(key.clone());
            self.selection_anchor = Some(key);
        }
    }

    /// Hovering an unselected row still stages only that file.
    pub(super) fn staging_paths(&self, key: &FileKey) -> Vec<String> {
        if !self.selected_files.contains(key) {
            return vec![key.path.clone()];
        }
        let mut paths: Vec<_> = self
            .selected_files
            .iter()
            .filter(|selected| selected.same_group(key))
            .map(|selected| selected.path.clone())
            .collect();
        paths.sort();
        paths
    }

    pub(super) fn discard_paths(&self, key: &FileKey) -> Vec<String> {
        let mut paths = self.staging_paths(key);
        if let Some(repo) = self.snapshot.as_ref().and_then(|snapshot| {
            snapshot
                .repositories
                .iter()
                .find(|repo| repo.path == key.repository)
        }) {
            // A parent gitlink describes a separate checkout. Like Discard
            // All, a multi-selection only discards this repository's files.
            paths.retain(|path| !repo.submodules.contains(path)
                && repo.files.iter().any(|file| &file.path == path && !file.is_conflicted()));
        }
        paths
    }

    pub(super) fn prune_selection(&mut self) {
        if self.selected_files.is_empty() && self.selection_anchor.is_none() {
            return;
        }
        let valid = self.valid_file_keys();
        self.selected_files.retain(|key| valid.contains(key));
        if self
            .selection_anchor
            .as_ref()
            .is_some_and(|key| !valid.contains(key))
        {
            self.selection_anchor = None;
        }
    }

    pub(super) fn follow_staging(
        &mut self,
        repository: &str,
        paths: &[String],
        staged: bool,
        selection: HashSet<FileKey>,
        anchor: Option<FileKey>,
    ) {
        let valid = self.valid_file_keys();
        let paths: HashSet<_> = paths.iter().map(String::as_str).collect();
        let move_key = |key: FileKey| {
            if key.repository == repository && paths.contains(key.path.as_str()) {
                let destination = FileKey {
                    staged,
                    ..key.clone()
                };
                if valid.contains(&destination) {
                    return Some(destination);
                }
            }
            valid.contains(&key).then_some(key)
        };
        self.selected_files = selection.into_iter().filter_map(move_key).collect();
        self.selection_anchor = anchor.and_then(move_key);
    }
}
