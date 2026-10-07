//! Confirmed, recoverable discards. The default restores from the index, not
//! HEAD, so partially staged files keep their staged edits.
use crate::{EngineError, checkout_changes, checkout_git, diff_sync};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use zeron_proto::{CheckoutDiscardPreview, CheckoutDiscardResult, GitFileState};

fn fail(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Refuse symlink ancestors and directories: a discard must never clean an
/// initialized submodule, nested repository, or a path outside this checkout.
pub(crate) async fn validate_path(root: &Path, path: &str) -> Result<PathBuf, EngineError> {
    let relative = checkout_changes::relative(path)?;
    if relative.components().any(|c| c.as_os_str() == ".git") {
        return Err(fail("Git metadata cannot be discarded"));
    }
    let mut location = root.to_owned();
    let components: Vec<_> = relative.components().collect();
    for (i, part) in components.iter().enumerate() {
        location.push(part.as_os_str());
        match tokio::fs::symlink_metadata(&location).await {
            Ok(metadata) if i + 1 < components.len() => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(fail("File has an unsafe parent directory"));
                }
                if location.join(".git").exists() {
                    return Err(fail("Select the nested repository to discard its files"));
                }
            }
            Ok(metadata) if metadata.is_dir() => {
                return Err(fail(
                    "Directories and submodules cannot be discarded as files",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(location)
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    path: String,
    kind: String,
    digest: String,
    mode: u32,
    link: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Recovery {
    entries: Vec<Entry>,
    preview: CheckoutDiscardPreview,
    after: String,
    index: bool,
}

async fn entries(
    root: &Path,
    paths: &[String],
    backup: Option<&Path>,
) -> Result<Vec<Entry>, EngineError> {
    let mut result = Vec::new();
    let mut size = 0;
    for (i, path) in paths.iter().enumerate() {
        let location = validate_path(root, path).await?;
        let metadata = match tokio::fs::symlink_metadata(&location).await {
            Ok(m) => Some(m),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let mut entry = Entry {
            path: path.clone(),
            kind: "missing".into(),
            digest: String::new(),
            mode: 0,
            link: None,
        };
        if let Some(metadata) = metadata {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                entry.mode = metadata.permissions().mode();
            }
            if metadata.file_type().is_symlink() {
                let target = tokio::fs::read_link(&location).await?;
                let target = target
                    .to_str()
                    .ok_or_else(|| fail("Symlink target is not UTF-8"))?
                    .to_owned();
                entry.kind = "symlink".into();
                entry.digest = hash(target.as_bytes());
                entry.link = Some(target);
            } else if metadata.is_file() {
                size += metadata.len();
                if size > 256 * 1024 * 1024 {
                    return Err(fail(
                        "Selection exceeds the 256 MiB discard recovery limit; discard fewer files",
                    ));
                }
                let bytes = tokio::fs::read(location).await?;
                entry.kind = "file".into();
                entry.digest = hash(&bytes);
                if let Some(backup) = backup {
                    tokio::fs::write(backup.join(format!("{i}.data")), bytes).await?;
                }
            } else {
                return Err(fail("Only regular files and symlinks can be discarded"));
            }
        }
        result.push(entry);
    }
    Ok(result)
}

async fn index_path(root: &Path) -> Result<PathBuf, EngineError> {
    let path = checkout_git::text(
        root,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )
    .await?;
    Ok(PathBuf::from(path))
}
async fn index_bytes(root: &Path) -> Result<Vec<u8>, EngineError> {
    match tokio::fs::read(index_path(root).await?).await {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}
async fn fingerprint(root: &Path, paths: &[String]) -> Result<String, EngineError> {
    let entries = entries(root, paths, None).await?;
    let state = checkout_git::state(root).await?;
    let index = checkout_git::text(root, &["ls-files", "--stage", "-z"]).await?;
    Ok(hash(
        &serde_json::to_vec(&(entries, state.head, state.branch, hash(index.as_bytes())))
            .map_err(|e| fail(e.to_string()))?,
    ))
}

pub async fn preview(
    root: &Path,
    paths: &[String],
    include_staged: bool,
) -> Result<CheckoutDiscardPreview, EngineError> {
    if paths.is_empty() {
        return Err(fail("Select files to discard"));
    }
    let (files, complete) = checkout_changes::status(root).await?;
    let (modules, modules_complete) = checkout_changes::submodules(root).await?;
    if !complete || !modules_complete {
        return Err(fail("Git status is incomplete; refresh before discarding"));
    }
    let mut selected = BTreeSet::new();
    for path in paths {
        if modules.contains(path) {
            return Err(fail(
                "Select the submodule's own Changes group to discard its files",
            ));
        }
        let file = files
            .iter()
            .find(|f| &f.path == path)
            .ok_or_else(|| fail("File changed; refresh the changes list"))?;
        if !include_staged && file.worktree == GitFileState::Unchanged {
            return Err(fail("File has no unstaged changes"));
        }
        if !include_staged && file.index == GitFileState::Unmerged {
            return Err(fail(
                "Resolve conflicts with Accept Current or Accept Incoming",
            ));
        }
        validate_path(root, path).await?;
        selected.insert(path.clone());
        if (include_staged || matches!(file.worktree, GitFileState::Renamed | GitFileState::Copied))
            && let Some(old) = &file.old_path
        {
            validate_path(root, old).await?;
            selected.insert(old.clone());
        }
    }
    let paths: Vec<_> = selected.into_iter().collect();
    Ok(CheckoutDiscardPreview {
        checksum: fingerprint(root, &paths).await?,
        file_count: paths.len(),
        paths,
        include_staged,
    })
}

pub async fn discard(
    root: &Path,
    request: CheckoutDiscardPreview,
) -> Result<CheckoutDiscardResult, EngineError> {
    // The preview may include the old side of a rename, which isn't a status row.
    if fingerprint(root, &request.paths).await? != request.checksum {
        return Err(fail(
            "Files or staging changed since confirmation; review and discard again",
        ));
    }
    let (modules, complete) = checkout_changes::submodules(root).await?;
    if !complete || request.paths.iter().any(|p| modules.contains(p)) {
        return Err(fail(
            "Submodule contents must be handled in their own repository",
        ));
    }
    let recovery_id = uuid::Uuid::new_v4().to_string();
    let backup = checkout_git::git_dir(root)
        .await?
        .join("zeron-discard")
        .join(&recovery_id);
    tokio::fs::create_dir_all(&backup).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o700)).await?;
    }
    let original = entries(root, &request.paths, Some(&backup)).await?;
    let index = index_bytes(root).await?;
    tokio::fs::write(backup.join("index"), &index).await?;
    let mut recovery = Recovery {
        entries: original,
        preview: request,
        after: String::new(),
        index: !index.is_empty(),
    };
    tokio::fs::write(
        backup.join("recovery.json"),
        serde_json::to_vec(&recovery).map_err(|e| fail(e.to_string()))?,
    )
    .await?;
    if fingerprint(root, &recovery.preview.paths).await? != recovery.preview.checksum {
        return Err(fail(
            "Files changed while preparing recovery; nothing was discarded",
        ));
    }
    let tracked = checkout_git::text(root, &["ls-files", "-z"]).await?;
    let tracked: BTreeSet<_> = tracked.split('\0').collect();
    let mut restore = Vec::new();
    if recovery.preview.include_staged {
        // HEAD paths plus index paths: newly staged files must be removed too.
        let base = diff_sync::working_diff_base(root).await?;
        let head_paths =
            checkout_git::text(root, &["ls-tree", "-r", "--name-only", "-z", &base]).await?;
        for path in &recovery.preview.paths {
            if tracked.contains(path.as_str()) || head_paths.split('\0').any(|p| p == path) {
                restore.push(path.clone());
            }
        }
        if !restore.is_empty() {
            let payload = restore.join("\0") + "\0";
            checkout_git::run(
                root,
                &[
                    "--literal-pathspecs",
                    "restore",
                    "--source",
                    &base,
                    "--staged",
                    "--worktree",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                ],
                Some(payload.as_bytes()),
            )
            .await?;
        }
    } else {
        restore.extend(
            recovery
                .preview
                .paths
                .iter()
                .filter(|p| tracked.contains(p.as_str()))
                .cloned(),
        );
        if !restore.is_empty() {
            let payload = restore.join("\0") + "\0";
            checkout_git::run(
                root,
                &[
                    "--literal-pathspecs",
                    "restore",
                    "--worktree",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                ],
                Some(payload.as_bytes()),
            )
            .await?;
        }
    }
    for path in recovery
        .preview
        .paths
        .iter()
        .filter(|p| !restore.contains(p))
    {
        let location = validate_path(root, path).await?;
        match tokio::fs::remove_file(location).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    recovery.after = fingerprint(root, &recovery.preview.paths).await?;
    tokio::fs::write(
        backup.join("recovery.json"),
        serde_json::to_vec(&recovery).map_err(|e| fail(e.to_string()))?,
    )
    .await?;
    Ok(CheckoutDiscardResult {
        recovery_id,
        checksum: recovery.after,
        file_count: recovery.preview.file_count,
    })
}

pub async fn undo(root: &Path, recovery_id: &str, expected: &str) -> Result<(), EngineError> {
    let id = uuid::Uuid::parse_str(recovery_id).map_err(|_| fail("Invalid discard recovery ID"))?;
    let backup = checkout_git::git_dir(root)
        .await?
        .join("zeron-discard")
        .join(id.to_string());
    let recovery: Recovery =
        serde_json::from_slice(&tokio::fs::read(backup.join("recovery.json")).await?)
            .map_err(|e| fail(e.to_string()))?;
    if recovery.after.is_empty()
        || recovery.after != expected
        || fingerprint(root, &recovery.preview.paths).await? != expected
    {
        return Err(fail(
            "Files, branch, or staging changed after discard; recovery files are preserved in Git's zeron-discard directory",
        ));
    }
    // Check every recovery payload before replacing any worktree file.
    for (i, entry) in recovery.entries.iter().enumerate() {
        validate_path(root, &entry.path).await?;
        match entry.kind.as_str() {
            "file" => {
                if hash(&tokio::fs::read(backup.join(format!("{i}.data"))).await?) != entry.digest {
                    return Err(fail("Discard recovery data is corrupt"));
                }
            }
            "symlink" => {
                if entry
                    .link
                    .as_ref()
                    .is_none_or(|link| hash(link.as_bytes()) != entry.digest)
                {
                    return Err(fail("Discard recovery symlink is corrupt"));
                }
            }
            "missing" => {}
            _ => return Err(fail("Invalid recovery entry")),
        }
    }
    // Reserve Git's index lock before touching any recovered files.
    let index = index_path(root).await?;
    let lock = index.with_file_name("index.lock");
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .await?;
    let result: Result<(), EngineError> = async {
        use tokio::io::AsyncWriteExt;
        file.write_all(&tokio::fs::read(backup.join("index")).await?)
            .await?;
        file.sync_all().await?;
        for (i, entry) in recovery.entries.iter().enumerate() {
            let location = validate_path(root, &entry.path).await?;
            if let Some(parent) = location.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            if tokio::fs::symlink_metadata(&location).await.is_ok() {
                tokio::fs::remove_file(&location).await?;
            }
            match entry.kind.as_str() {
                "file" => {
                    let bytes = tokio::fs::read(backup.join(format!("{i}.data"))).await?;
                    if hash(&bytes) != entry.digest {
                        return Err(fail("Discard recovery data is corrupt"));
                    }
                    tokio::fs::write(&location, bytes).await?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        tokio::fs::set_permissions(
                            &location,
                            std::fs::Permissions::from_mode(entry.mode),
                        )
                        .await?;
                    }
                }
                "symlink" => {
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(
                        entry
                            .link
                            .as_deref()
                            .ok_or_else(|| fail("Invalid recovery symlink"))?,
                        &location,
                    )?;
                    #[cfg(not(unix))]
                    return Err(fail("Symlink recovery is supported on Linux"));
                }
                "missing" => {}
                _ => return Err(fail("Invalid recovery entry")),
            }
        }
        if recovery.index {
            tokio::fs::rename(&lock, &index).await?;
        } else if index.exists() {
            tokio::fs::remove_file(&index).await?;
        }
        Ok(())
    }
    .await;
    let _ = tokio::fs::remove_file(&lock).await;
    result
}
