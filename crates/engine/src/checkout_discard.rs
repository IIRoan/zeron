//! Confirmed, recoverable discards. The default restores from the index, not
//! HEAD, so partially staged files keep their staged edits.
use crate::{EngineError, checkout_changes, checkout_git, diff_sync};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use zeron_proto::{CheckoutDiscardPreview, CheckoutDiscardResult, GitFileState};

const BACKUPS_KEPT: usize = 16;
const BACKUP_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const BACKUP_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

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
/// Undo replaces the whole index, so the fingerprint covers every index
/// entry, not only the selected paths. Hashing the streamed raw bytes keeps
/// large indexes and non-UTF-8 paths elsewhere in the repository working.
async fn fingerprint(root: &Path, paths: &[String]) -> Result<String, EngineError> {
    let entries = entries(root, paths, None).await?;
    let state = checkout_git::state(root).await?;
    let mut index = Sha256::new();
    checkout_git::stream(
        root,
        &["ls-files", "--stage", "-z"],
        checkout_git::STREAM_TIMEOUT,
        |chunk| {
            index.update(chunk);
            Ok(true)
        },
    )
    .await?;
    let index = format!("{:x}", index.finalize());
    Ok(hash(
        &serde_json::to_vec(&(entries, state.head, state.branch, index))
            .map_err(|e| fail(e.to_string()))?,
    ))
}

/// The confirmation also binds the staged/unstaged choice, which the
/// recovery fingerprint (also used by older recoveries) does not include.
fn confirmation(fingerprint: &str, include_staged: bool) -> String {
    hash(format!("{fingerprint}\0{include_staged}").as_bytes())
}

/// Index paths among `selected` listed by a NUL-separated Git command.
async fn listed(
    root: &Path,
    args: &[&str],
    selected: &HashSet<&str>,
) -> Result<HashSet<String>, EngineError> {
    let mut found = HashSet::new();
    checkout_git::records(root, args, checkout_git::STREAM_TIMEOUT, |record| {
        if let Ok(path) = std::str::from_utf8(record)
            && selected.contains(path)
        {
            found.insert(path.to_owned());
        }
        Ok(())
    })
    .await?;
    Ok(found)
}

pub async fn preview(
    root: &Path,
    paths: &[String],
    include_staged: bool,
) -> Result<CheckoutDiscardPreview, EngineError> {
    plan(root, paths, include_staged, false).await
}

/// `expanded` accepts a previous plan's output, which also lists the old side
/// of a rename; such a path is not a status row of its own.
async fn plan(
    root: &Path,
    paths: &[String],
    include_staged: bool,
    expanded: bool,
) -> Result<CheckoutDiscardPreview, EngineError> {
    if paths.is_empty() {
        return Err(fail("Select files to discard"));
    }
    let (files, complete) = checkout_changes::status(root).await?;
    let (modules, modules_complete) = checkout_changes::submodules(root).await?;
    if !complete || !modules_complete {
        return Err(fail("Git status is incomplete; refresh before discarding"));
    }
    let files: HashMap<_, _> = files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let modules: HashSet<_> = modules.iter().map(String::as_str).collect();
    let mut selected = BTreeSet::new();
    for path in paths {
        if modules.contains(path.as_str()) {
            return Err(fail(
                "Select the submodule's own Changes group to discard its files",
            ));
        }
        let Some(file) = files.get(path.as_str()) else {
            if expanded {
                continue;
            }
            return Err(fail("File changed; refresh the changes list"));
        };
        if !include_staged && file.worktree == GitFileState::Unchanged {
            return Err(fail("File has no unstaged changes"));
        }
        if !include_staged && file.is_conflicted() {
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
    if selected.iter().any(|path| modules.contains(path.as_str())) {
        return Err(fail(
            "Submodule contents must be handled in their own repository",
        ));
    }
    let paths: Vec<_> = selected.into_iter().collect();
    Ok(CheckoutDiscardPreview {
        checksum: confirmation(&fingerprint(root, &paths).await?, include_staged),
        file_count: paths.len(),
        paths,
        include_staged,
    })
}

async fn write_recovery(backup: &Path, recovery: &Recovery) -> Result<(), EngineError> {
    tokio::fs::write(
        backup.join("recovery.json"),
        serde_json::to_vec(recovery).map_err(|e| fail(e.to_string()))?,
    )
    .await?;
    Ok(())
}

/// Keep a bounded number of recent backups: each can hold up to 256 MiB.
async fn prune_backups(directory: &Path, newest: &Path) -> Result<(), EngineError> {
    let mut listing = match tokio::fs::read_dir(directory).await {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut backups = Vec::new();
    while let Some(entry) = listing.next_entry().await? {
        let is_backup = entry
            .file_name()
            .to_str()
            .is_some_and(|name| uuid::Uuid::parse_str(name).is_ok());
        let metadata = entry.metadata().await?;
        if !is_backup || !metadata.is_dir() {
            continue;
        }
        let mut size = 0;
        let mut files = tokio::fs::read_dir(entry.path()).await?;
        while let Some(file) = files.next_entry().await? {
            size += file.metadata().await?.len();
        }
        backups.push((metadata.modified()?, size, entry.path()));
    }
    // Always retain this operation's recovery, even when timestamps tie.
    backups.sort_by_key(|backup| (backup.2 != newest, std::cmp::Reverse(backup.0)));
    let now = SystemTime::now();
    let mut total = 0;
    for (i, (modified, size, path)) in backups.into_iter().enumerate() {
        total += size;
        let age = now.duration_since(modified).unwrap_or_default();
        if path != newest
            && (i >= BACKUPS_KEPT || age > BACKUP_MAX_AGE || total > BACKUP_TOTAL_BYTES)
        {
            tokio::fs::remove_dir_all(path).await?;
        }
    }
    Ok(())
}

pub async fn discard(
    root: &Path,
    request: CheckoutDiscardPreview,
) -> Result<CheckoutDiscardResult, EngineError> {
    // Rebuild the confirmation from the current status rather than trusting
    // the request's paths or staged mode; it must match exactly.
    let expected = plan(root, &request.paths, request.include_staged, true).await?;
    if expected.paths != request.paths || expected.checksum != request.checksum {
        return Err(fail(
            "Files or staging changed since confirmation; review and discard again",
        ));
    }
    let directory = checkout_git::git_dir(root).await?.join("zeron-discard");
    let recovery_id = uuid::Uuid::new_v4().to_string();
    let backup = directory.join(&recovery_id);
    tokio::fs::create_dir_all(&backup).await?;
    let mut recovery = match prepare(root, &backup, expected).await {
        Ok(recovery) => recovery,
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&backup).await;
            return Err(error);
        }
    };
    // Count the new recovery's files and index in both retention limits.
    // Retention is best effort; a cleanup failure must not prevent recovery.
    let _ = prune_backups(&directory, &backup).await;
    let applied = apply(root, &recovery.preview).await;
    // Record the resulting state even after a partial failure, so the backup
    // stays restorable through undo.
    let finished = match fingerprint(root, &recovery.preview.paths).await {
        Ok(after) => {
            recovery.after = after;
            let saved = write_recovery(&backup, &recovery).await;
            applied.and(saved)
        }
        Err(error) => applied.and(Err(error)),
    };
    if let Err(error) = finished {
        return Err(fail(format!(
            "Discard did not finish: {error}. The original files are saved in Git's zeron-discard/{recovery_id} directory"
        )));
    }
    Ok(CheckoutDiscardResult {
        recovery_id,
        checksum: recovery.after,
        file_count: recovery.preview.file_count,
    })
}

/// Save every selected file and the index before anything is discarded.
async fn prepare(
    root: &Path,
    backup: &Path,
    preview: CheckoutDiscardPreview,
) -> Result<Recovery, EngineError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(backup, std::fs::Permissions::from_mode(0o700)).await?;
    }
    let original = entries(root, &preview.paths, Some(backup)).await?;
    let index = index_bytes(root).await?;
    tokio::fs::write(backup.join("index"), &index).await?;
    let recovery = Recovery {
        entries: original,
        preview,
        after: String::new(),
        index: !index.is_empty(),
    };
    write_recovery(backup, &recovery).await?;
    let current = fingerprint(root, &recovery.preview.paths).await?;
    if confirmation(&current, recovery.preview.include_staged) != recovery.preview.checksum {
        return Err(fail(
            "Files changed while preparing recovery; nothing was discarded",
        ));
    }
    Ok(recovery)
}

async fn apply(root: &Path, preview: &CheckoutDiscardPreview) -> Result<(), EngineError> {
    let selected: HashSet<_> = preview.paths.iter().map(String::as_str).collect();
    let tracked = listed(root, &["ls-files", "-z"], &selected).await?;
    let mut restore = Vec::new();
    if preview.include_staged {
        // HEAD paths plus index paths: newly staged files must be removed too.
        let base = diff_sync::working_diff_base(root).await?;
        let head_paths = listed(
            root,
            &["ls-tree", "-r", "--name-only", "-z", &base],
            &selected,
        )
        .await?;
        restore.extend(
            preview
                .paths
                .iter()
                .filter(|p| tracked.contains(p.as_str()) || head_paths.contains(p.as_str()))
                .cloned(),
        );
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
            preview
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
    let restored: HashSet<_> = restore.iter().map(String::as_str).collect();
    for path in preview
        .paths
        .iter()
        .filter(|p| !restored.contains(p.as_str()))
    {
        let location = validate_path(root, path).await?;
        match tokio::fs::remove_file(location).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

pub async fn undo(root: &Path, recovery_id: &str, expected: &str) -> Result<(), EngineError> {
    let id = uuid::Uuid::parse_str(recovery_id).map_err(|_| fail("Invalid discard recovery ID"))?;
    let backup = checkout_git::git_dir(root)
        .await?
        .join("zeron-discard")
        .join(id.to_string());
    let saved = match tokio::fs::read(backup.join("recovery.json")).await {
        Ok(saved) => saved,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(fail("This discard was already undone or has expired"));
        }
        Err(e) => return Err(e.into()),
    };
    let recovery: Recovery = serde_json::from_slice(&saved).map_err(|e| fail(e.to_string()))?;
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
    if result.is_ok() {
        let _ = tokio::fs::remove_dir_all(&backup).await;
    }
    result
}
