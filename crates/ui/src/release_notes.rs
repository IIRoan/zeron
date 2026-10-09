//! Offline release summaries for the Linux fork. Notes are pinned by the
//! maintenance script; the desktop never queries GitHub or installs upstream.
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::markdown::{BlockTree, parse_full};

const BUNDLED: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/releases/changelog.json"
));
const STATE_FILE: &str = "release-notes-state.json";

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Release {
    pub version: String,
    pub upstream_commit: String,
    pub upstream_url: String,
    pub published_at: Option<String>,
    pub notes_source: String,
    pub upstream_notes: String,
    pub fork_changes: Vec<String>,
}

impl Release {
    fn content_id(&self) -> String {
        // A fork-only release can keep upstream's version. Metadata-only
        // changes and edits to older notes must not reopen the summary.
        let content = serde_json::to_vec(&(
            &self.version,
            &self.upstream_commit,
            &self.upstream_notes,
            &self.fork_changes,
        ))
        .expect("release content serializes");
        format!("{:x}", Sha256::digest(content))
    }
}

pub(crate) struct ReleaseContent {
    pub release: Release,
    pub upstream: BlockTree,
}

pub(crate) struct Catalog {
    pub releases: Vec<ReleaseContent>,
}

impl Catalog {
    fn parse(source: &str) -> anyhow::Result<Self> {
        #[derive(Deserialize)]
        struct Wire {
            schema_version: u32,
            upstream_repository: String,
            fork_repository: String,
            releases: Vec<Release>,
        }
        let wire: Wire = serde_json::from_str(source)?;
        anyhow::ensure!(
            wire.schema_version == 1
                && wire.upstream_repository == "zeronsh/zeron"
                && wire.fork_repository == "IIRoan/zeron",
            "unsupported release catalog"
        );
        let mut previous: Option<&str> = None;
        for release in &wire.releases {
            anyhow::ensure!(
                release.upstream_notes.len() <= 64 * 1024
                    && !release.upstream_notes.trim().is_empty(),
                "invalid release notes"
            );
            anyhow::ensure!(
                release.upstream_url
                    == format!(
                        "https://github.com/zeronsh/zeron/releases/tag/v{}",
                        release.version
                    ),
                "invalid upstream release URL"
            );
            anyhow::ensure!(
                matches!(
                    release.notes_source.as_str(),
                    "github_release" | "git_history"
                ),
                "invalid release provenance"
            );
            if let Some(previous) = previous {
                anyhow::ensure!(
                    zeron_update::version_newer(previous, &release.version),
                    "release notes must be newest first"
                );
            }
            previous = Some(&release.version);
        }
        Ok(Self {
            releases: wire
                .releases
                .into_iter()
                .map(|release| {
                    let upstream = parse_full(&release.upstream_notes);
                    ReleaseContent { release, upstream }
                })
                .collect(),
        })
    }

    pub fn current_index(&self, version: &str) -> Option<usize> {
        self.releases
            .iter()
            .position(|entry| entry.release.version == version)
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct LaunchState {
    pub seen_id: Option<String>,
    pub seen_version: Option<String>,
    pub show_after_updates: bool,
}

impl Default for LaunchState {
    fn default() -> Self {
        Self {
            seen_id: None,
            seen_version: None,
            show_after_updates: true,
        }
    }
}

impl LaunchState {
    fn should_offer(&self, release: &Release) -> bool {
        self.show_after_updates
            && self.seen_id.as_deref() != Some(release.content_id().as_str())
            && !self
                .seen_version
                .as_deref()
                .is_some_and(|seen| zeron_update::version_newer(seen, &release.version))
    }

    fn acknowledge(&mut self, release: &Release) {
        self.seen_id = Some(release.content_id());
        self.seen_version = Some(release.version.clone());
    }
}

fn read_state(root: &Path) -> LaunchState {
    let path = root.join(STATE_FILE);
    if !std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= 8192) {
        return LaunchState::default();
    }
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() <= 8192 => serde_json::from_slice(&bytes).unwrap_or_default(),
        _ => LaunchState::default(),
    }
}

fn write_state(root: &Path, state: &LaunchState) -> anyhow::Result<()> {
    std::fs::create_dir_all(root)?;
    let temporary = root.join(format!(".release-notes-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        std::fs::write(&temporary, serde_json::to_vec(state)?)?;
        std::fs::rename(&temporary, root.join(STATE_FILE))?;
        anyhow::Ok(())
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

pub struct ReleaseNotes {
    pub(crate) catalog: Option<Arc<Catalog>>,
    pub(crate) loaded: bool,
    pub(crate) state: LaunchState,
    data_dir: PathBuf,
    requested: bool,
    auto_claimed: bool,
    save_generation: Arc<AtomicU64>,
    save_lock: Arc<Mutex<()>>,
    _load: Option<Task<()>>,
}

struct GlobalReleaseNotes(Entity<ReleaseNotes>);
impl Global for GlobalReleaseNotes {}

impl ReleaseNotes {
    pub fn init(data_dir: PathBuf, cx: &mut App) {
        let entity = cx.new(|cx: &mut Context<Self>| {
            let path = data_dir.clone();
            let load =
                cx.background_spawn(async move { (Catalog::parse(BUNDLED), read_state(&path)) });
            let task = cx.spawn(async move |this, cx| {
                let (catalog, state) = load.await;
                let _ = this.update(cx, |this, cx| {
                    match catalog {
                        Ok(catalog) => this.catalog = Some(Arc::new(catalog)),
                        Err(error) => tracing::warn!(%error, "release notes unavailable"),
                    }
                    this.state = state;
                    this.loaded = true;
                    cx.notify();
                });
            });
            Self {
                catalog: None,
                loaded: false,
                state: LaunchState::default(),
                data_dir,
                requested: false,
                auto_claimed: false,
                save_generation: Arc::new(AtomicU64::new(0)),
                save_lock: Arc::new(Mutex::new(())),
                _load: Some(task),
            }
        });
        cx.set_global(GlobalReleaseNotes(entity));
    }

    pub fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalReleaseNotes>()
            .map(|global| global.0.clone())
    }

    #[cfg(test)]
    pub(crate) fn init_fixture(data_dir: PathBuf, cx: &mut App) -> Entity<Self> {
        let entity = cx.new(|_| Self {
            catalog: Some(Arc::new(Catalog::parse(BUNDLED).unwrap())),
            loaded: true,
            state: LaunchState::default(),
            data_dir,
            requested: false,
            auto_claimed: false,
            save_generation: Arc::new(AtomicU64::new(0)),
            save_lock: Arc::new(Mutex::new(())),
            _load: None,
        });
        cx.set_global(GlobalReleaseNotes(entity.clone()));
        entity
    }

    pub(crate) fn current(&self) -> Option<&ReleaseContent> {
        let catalog = self.catalog.as_ref()?;
        catalog
            .current_index(zeron_update::current_version())
            .map(|index| &catalog.releases[index])
    }

    pub(crate) fn claim_open(&mut self, allow_automatic: bool) -> bool {
        if !self.loaded {
            return false;
        }
        let automatic = allow_automatic
            && !self.auto_claimed
            && self
                .current()
                .is_some_and(|entry| self.state.should_offer(&entry.release));
        if !std::mem::take(&mut self.requested) && !automatic {
            return false;
        }
        self.auto_claimed = true;
        true
    }

    pub(crate) fn dismiss(&mut self, cx: &mut Context<Self>) {
        if let Some(release) = self.current().map(|entry| entry.release.clone()) {
            self.state.acknowledge(&release);
            self.save(cx);
        }
    }

    pub(crate) fn toggle_automatic(&mut self, cx: &mut Context<Self>) {
        self.state.show_after_updates = !self.state.show_after_updates;
        self.save(cx);
        cx.notify();
    }

    fn save(&self, cx: &mut Context<Self>) {
        let state = self.state.clone();
        let root = self.data_dir.clone();
        let sequence = self.save_generation.clone();
        let lock = self.save_lock.clone();
        let generation = sequence.fetch_add(1, Ordering::SeqCst) + 1;
        cx.background_spawn(async move {
            let _lock = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if sequence.load(Ordering::SeqCst) != generation {
                return;
            }
            if let Err(error) = write_state(&root, &state) {
                tracing::warn!(%error, "could not save release note preferences");
            }
        })
        .detach();
    }
}

pub fn show(cx: &mut App) {
    if let Some(notes) = ReleaseNotes::global(cx) {
        notes.update(cx, |notes, cx| {
            notes.requested = true;
            cx.notify();
        });
    }
    crate::activate_main_window(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_notes_match_the_running_build_and_have_a_previous_release() {
        let catalog = Catalog::parse(BUNDLED).unwrap();
        let index = catalog
            .current_index(zeron_update::current_version())
            .unwrap();
        assert!(catalog.releases.get(index + 1).is_some());
        assert!(!catalog.releases[index].release.fork_changes.is_empty());
        assert!(catalog.current_index("99.0.0").is_none());
    }

    #[test]
    fn release_notes_acknowledgement_survives_restart_and_fork_only_updates() {
        let catalog = Catalog::parse(BUNDLED).unwrap();
        let release = &catalog.releases[0].release;
        let root = tempfile::tempdir().unwrap();
        let mut state = read_state(root.path());
        assert!(state.should_offer(release));
        state.acknowledge(release);
        write_state(root.path(), &state).unwrap();
        let mut saved = read_state(root.path());
        assert!(!saved.should_offer(release));
        let mut fork_update = release.clone();
        fork_update.fork_changes.push("A new fork feature".into());
        assert!(saved.should_offer(&fork_update));
        saved.show_after_updates = false;
        assert!(!saved.should_offer(&fork_update));
        write_state(root.path(), &saved).unwrap();
        assert!(!read_state(root.path()).show_after_updates);
    }

    #[test]
    fn release_notes_do_not_announce_downgrades_or_metadata_only_edits() {
        let catalog = Catalog::parse(BUNDLED).unwrap();
        let release = &catalog.releases[0].release;
        let mut state = LaunchState::default();
        state.acknowledge(release);
        assert!(!state.should_offer(&catalog.releases[1].release));
        let mut metadata = release.clone();
        metadata.published_at = None;
        assert!(!state.should_offer(&metadata));
    }

    #[test]
    fn release_notes_reject_another_repository_and_recover_invalid_preferences() {
        assert!(Catalog::parse(&BUNDLED.replace("zeronsh/zeron", "wrong/repo")).is_err());
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(STATE_FILE), "corrupt").unwrap();
        assert!(read_state(root.path()).show_after_updates);
    }

    #[gpui::test]
    fn release_notes_open_once_but_manual_requests_work_after_opt_out(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            let notes = ReleaseNotes::init_fixture(root.path().into(), cx);
            notes.update(cx, |notes, _| {
                assert!(
                    !notes.claim_open(false),
                    "wait while another surface is active"
                );
                assert!(notes.claim_open(true));
                assert!(
                    !notes.claim_open(true),
                    "one window consumes the startup summary"
                );
                notes.state.show_after_updates = false;
                notes.requested = true;
                assert!(
                    notes.claim_open(false),
                    "manual requests bypass auto-show preference"
                );
                assert!(!notes.claim_open(true));
            });
        });
    }
}
