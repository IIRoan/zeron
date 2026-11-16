//! A chat's worktree follows the chat. Archiving soft-closes it: the
//! checkout's own services and its chats' terminals stop and the worktree's
//! cleanup runs, but the files stay so the chat can be restored. Deleting the
//! chat removes the worktree Zeron created for it (its Git branch is kept).
//! Neither happens while another chat on this device still works there.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tokio::sync::watch;
use zeron_proto::Chat;

use crate::project_terminals::ProjectTerminals;
use crate::{EngineError, Repos, Terminals};

#[derive(Clone)]
pub(crate) struct WorktreeLifecycle {
    pub(crate) repos: Repos,
    pub(crate) services: ProjectTerminals,
    pub(crate) terminals: Terminals,
    pub(crate) device_id: String,
}

impl WorktreeLifecycle {
    /// Soft-close the worktree of every chat archived from now on. It acts on
    /// the change, not the state: a chat that arrives already archived (boot,
    /// a replica catching up) is left alone, so old archives never rerun
    /// their cleanup. Ends with the workspace.
    pub(crate) fn watch_archives(self, mut chats: watch::Receiver<Vec<Chat>>) {
        tokio::spawn(async move {
            let mut live = live_ids(&chats.borrow_and_update(), &self.device_id);
            while chats.changed().await.is_ok() {
                let current = chats.borrow_and_update().clone();
                for cwd in archived_since(&current, &live, &self.device_id) {
                    if chats_in(&current, &self.device_id, &cwd).any(|c| !c.archived) {
                        continue;
                    }
                    let ids: HashSet<String> = chats_in(&current, &self.device_id, &cwd)
                        .map(|c| c.id.clone())
                        .collect();
                    let this = self.clone();
                    tokio::spawn(async move {
                        if let Err(error) = this.soft_close(&cwd, ids).await {
                            tracing::warn!(%error, worktree = %cwd, "archived chat's worktree did not close cleanly");
                        }
                    });
                }
                live = live_ids(&current, &self.device_id);
            }
        });
    }

    /// `deleted` is gone from the registry, `remaining` is what is left: once
    /// no chat here uses its worktree any more, archived or not, remove it.
    pub(crate) fn chat_deleted(&self, deleted: &Chat, remaining: &[Chat]) {
        let Some(cwd) = deleted
            .cwd
            .clone()
            .filter(|_| deleted.device_id == self.device_id)
        else {
            return;
        };
        if chats_in(remaining, &self.device_id, &cwd).next().is_some() {
            return;
        }
        let (this, ids) = (self.clone(), HashSet::from([deleted.id.clone()]));
        tokio::spawn(async move {
            if let Err(error) = this.remove(&cwd, ids).await {
                tracing::warn!(%error, worktree = %cwd, "deleted chat's worktree was not removed");
            }
        });
    }

    async fn soft_close(&self, cwd: &str, chat_ids: HashSet<String>) -> Result<(), EngineError> {
        let Some((root, checkout)) = self.linked_worktree(cwd).await else {
            return Ok(());
        };
        self.stop(&root, &checkout, chat_ids).await?;
        let settings = self.repos.worktree_settings();
        let cleaned = settings.cleanup(&root, &checkout).await;
        // Cleanup marks the worktree as being removed. It stays, so a
        // restored chat must be able to prepare it again.
        settings.finish_removal(&checkout);
        cleaned
    }

    /// Uncommitted changes go with the worktree; commits stay on its branch.
    async fn remove(&self, cwd: &str, chat_ids: HashSet<String>) -> Result<(), EngineError> {
        let Some((root, checkout)) = self.linked_worktree(cwd).await else {
            return Ok(());
        };
        // A worktree the user made outlives its chats.
        if self
            .repos
            .worktree_settings()
            .owned_branch(&root, &checkout)?
            .is_none()
        {
            return Ok(());
        }
        self.stop(&root, &checkout, chat_ids).await?;
        self.repos
            .remove_worktree(&root, &checkout, true, false)
            .await
    }

    /// Stop the checkout's own services (shared databases keep running), then
    /// close the chats' terminals.
    async fn stop(
        &self,
        root: &Path,
        checkout: &Path,
        chat_ids: HashSet<String>,
    ) -> Result<(), EngineError> {
        let (services, terminals) = (self.services.clone(), self.terminals.clone());
        let (root, checkout) = (root.to_owned(), checkout.to_owned());
        tokio::task::spawn_blocking(move || {
            services.stop_checkout(&root, &checkout)?;
            for (terminal, chat) in terminals.chats() {
                if chat_ids.contains(&chat) {
                    let _ = terminals.close(&terminal); // already closed is fine
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| EngineError::Other(e.to_string()))?
    }

    /// The project root and canonical path of the linked worktree at `cwd`;
    /// `None` for a project folder itself or anything Git does not list.
    async fn linked_worktree(&self, cwd: &str) -> Option<(PathBuf, PathBuf)> {
        let root = self
            .repos
            .workspace_checkout_root(Path::new(cwd))
            .await
            .ok()?;
        let checkout = self.repos.workspace_checkout(&root, Path::new(cwd)).await?;
        (std::fs::canonicalize(&root).ok()? != checkout).then_some((root, checkout))
    }
}

fn live_ids(chats: &[Chat], device_id: &str) -> HashSet<String> {
    chats
        .iter()
        .filter(|c| c.device_id == device_id && !c.archived)
        .map(|c| c.id.clone())
        .collect()
}

/// Folders of this device's chats that were live in `live` and are archived now.
fn archived_since(chats: &[Chat], live: &HashSet<String>, device_id: &str) -> Vec<String> {
    let mut cwds: Vec<String> = chats
        .iter()
        .filter(|c| c.device_id == device_id && c.archived && live.contains(&c.id))
        .filter_map(|c| c.cwd.clone())
        .collect();
    cwds.sort();
    cwds.dedup();
    cwds
}

// ponytail: compares cwd strings as recorded, not canonical paths. Chats in a
// Zeron worktree share the path it was created at; canonicalize per chat if a
// symlinked spelling ever shows up.
fn chats_in<'a>(
    chats: &'a [Chat],
    device_id: &'a str,
    cwd: &'a str,
) -> impl Iterator<Item = &'a Chat> {
    chats.iter().filter(move |c| {
        c.device_id == device_id
            && c.cwd
                .as_deref()
                .is_some_and(|d| Path::new(d).starts_with(cwd))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: &str, device: &str, cwd: &str, archived: bool) -> Chat {
        serde_json::from_value(serde_json::json!({
            "id": id, "deviceId": device, "archived": archived, "cwd": cwd,
            "createdAt": "2026-11-12T00:00:00Z",
        }))
        .unwrap()
    }

    #[test]
    fn only_newly_archived_chats_here_close_their_folder() {
        let before = [
            chat("a", "me", "/wt/a", false),
            chat("b", "me", "/wt/b", true),
            chat("c", "other", "/wt/c", false),
        ];
        let live = live_ids(&before, "me");
        assert_eq!(live, HashSet::from(["a".to_string()]));
        let after = [
            chat("a", "me", "/wt/a", true),
            chat("b", "me", "/wt/b", true),
            chat("c", "other", "/wt/c", true),
            chat("d", "me", "/wt/d", true), // arrived archived
        ];
        assert_eq!(
            archived_since(&after, &live, "me"),
            vec!["/wt/a".to_string()]
        );
    }

    #[test]
    fn a_folder_is_in_use_by_chats_in_it_or_below_on_this_device() {
        let chats = [
            chat("a", "me", "/wt/a/sub", false),
            chat("b", "other", "/wt/b", false),
            chat("c", "me", "/wt/ab", false),
        ];
        let ids = |cwd| {
            chats_in(&chats, "me", cwd)
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("/wt/a"), ["a"]);
        assert!(ids("/wt/b").is_empty());
    }
}
