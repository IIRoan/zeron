//! Source-control metadata and explicit index operations. Submodules have their
//! own index; staging their contents never stages the parent's gitlink.
use std::{
    collections::VecDeque,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use zeron_proto::{
    CheckoutChangeSelection, CheckoutChanges, CheckoutDiff, DiffFileSummary, GitFileState,
    GitFileStatus, RepositoryChanges,
};

use crate::{
    EngineError,
    diff_sync::{self, Capture},
};

const LIMIT: usize = 3 * 1024 * 1024;
const MAX_REPOSITORIES: usize = 128;
const TIMEOUT: Duration = Duration::from_secs(10);

async fn git(root: &Path, args: &[&str]) -> Result<Capture, EngineError> {
    tokio::time::timeout(TIMEOUT, diff_sync::capture_git(root, args, LIMIT))
        .await
        .map_err(|_| EngineError::Other("Git operation timed out".into()))?
}

pub(crate) fn relative(path: &str) -> Result<&Path, EngineError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(EngineError::Other("Invalid checkout-relative path".into()));
    }
    Ok(path)
}

pub(crate) async fn submodules(root: &Path) -> Result<(Vec<String>, bool), EngineError> {
    let output = git(root, &["ls-files", "--stage", "-z"]).await?;
    let mut paths = Vec::new();
    for record in output
        .stdout
        .split(|b| *b == 0)
        .filter(|r| r.starts_with(b"160000 "))
    {
        if let Some(tab) = record.iter().position(|b| *b == b'\t') {
            let path = std::str::from_utf8(&record[tab + 1..])
                .map_err(|_| EngineError::Other("Non-UTF-8 submodule path".into()))?;
            relative(path)?;
            paths.push(path.to_string());
        }
    }
    paths.sort();
    paths.dedup();
    Ok((paths, !output.truncated))
}

pub(crate) async fn status(root: &Path) -> Result<(Vec<GitFileStatus>, bool), EngineError> {
    let output = git(
        root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )
    .await?;
    Ok(diff_sync::git_status::parse(
        &output.stdout,
        output.truncated,
    ))
}

/// Resolve through actual gitlinks rather than accepting arbitrary nested repos.
pub async fn repository(root: &Path, requested: &str) -> Result<PathBuf, EngineError> {
    let root = tokio::fs::canonicalize(root).await?;
    if requested.is_empty() {
        return Ok(root);
    }
    relative(requested)?;
    let mut current = root.clone();
    let mut remainder = requested;
    for _ in 0..MAX_REPOSITORIES {
        let (modules, complete) = submodules(&current).await?;
        if !complete {
            break;
        }
        let Some(module) = modules
            .iter()
            .find(|m| remainder == m.as_str() || remainder.starts_with(&format!("{m}/")))
        else {
            break;
        };
        let next = current.join(module);
        // An uninitialized submodule inherits its parent's Git context. Require
        // its own .git marker before invoking Git in the directory.
        if !next.join(".git").exists() {
            break;
        }
        current = tokio::fs::canonicalize(next).await?;
        if !current.starts_with(&root) {
            break;
        }
        if remainder == module {
            return Ok(current);
        }
        remainder = &remainder[module.len() + 1..];
    }
    Err(EngineError::Other(
        "Submodule is unavailable in this checkout".into(),
    ))
}

pub async fn list(root: &Path) -> Result<CheckoutChanges, EngineError> {
    let root = tokio::fs::canonicalize(root).await?;
    let mut pending = VecDeque::from([(String::new(), root.clone())]);
    let mut repositories = Vec::new();
    while let Some((path, directory)) = pending.pop_front() {
        let (files, status_complete) = status(&directory).await?;
        let (submodules, modules_complete) = submodules(&directory).await?;
        for module in &submodules {
            let child = directory.join(module);
            if !child.join(".git").exists() {
                continue;
            }
            let child = tokio::fs::canonicalize(child).await?;
            if !child.starts_with(&root) {
                continue;
            }
            if repositories.len() + pending.len() + 1 >= MAX_REPOSITORIES {
                return Err(EngineError::Other("Too many submodules to list".into()));
            }
            pending.push_back((
                if path.is_empty() {
                    module.clone()
                } else {
                    format!("{path}/{module}")
                },
                child,
            ));
        }
        let name = if path.is_empty() {
            root.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        } else {
            path.clone()
        };
        repositories.push(RepositoryChanges {
            path,
            name,
            files,
            submodules,
            complete: status_complete && modules_complete,
            git: crate::checkout_git::state(&directory).await.ok(),
        });
    }
    Ok(CheckoutChanges { repositories })
}

/// File lists come from the current porcelain status. Literal, NUL-separated
/// pathspecs handle renames, wildcard names and large stage-all selections.
pub async fn set_staged(root: &Path, paths: &[String], staged: bool) -> Result<(), EngineError> {
    if paths.is_empty() {
        return Ok(());
    }
    let (files, complete) = status(root).await?;
    if !complete {
        return Err(EngineError::Other(
            "Git status is incomplete; refresh before staging".into(),
        ));
    }
    let mut arguments = Vec::new();
    for path in paths {
        relative(path)?;
        let file = files.iter().find(|f| &f.path == path).ok_or_else(|| {
            EngineError::Other("File changes have moved; refresh the list".into())
        })?;
        arguments.push(path.clone());
        if let Some(old) = &file.old_path
            && (!staged || matches!(file.worktree, GitFileState::Renamed | GitFileState::Copied))
        {
            relative(old)?;
            arguments.push(old.clone());
        }
    }
    arguments.sort();
    arguments.dedup();
    let mut command = tokio::process::Command::new("git");
    command.arg("-C").arg(root).arg("--literal-pathspecs");
    if staged {
        command.args(["add", "-A"]);
    } else {
        let base = diff_sync::working_diff_base(root).await?;
        command.args(["restore", "--staged", "--source"]).arg(base);
    }
    command
        .args(["--pathspec-from-file=-", "--pathspec-file-nul"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| EngineError::Other("Git stdin unavailable".into()))?;
    let payload = arguments.join("\0") + "\0";
    let result = tokio::time::timeout(TIMEOUT, async move {
        stdin.write_all(payload.as_bytes()).await?;
        drop(stdin);
        child.wait_with_output().await
    })
    .await
    .map_err(|_| EngineError::Other("Staging timed out".into()))??;
    if !result.status.success() {
        return Err(EngineError::Other(
            String::from_utf8_lossy(&result.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

/// Drain both pipes concurrently: hooks can write more than a pipe's capacity.
/// Retain a bounded diagnostic without interrupting an otherwise valid commit.
pub(crate) async fn commit_output(mut stream: impl AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    (&mut stream)
        .take(64 * 1024)
        .read_to_end(&mut output)
        .await?;
    tokio::io::copy(&mut stream, &mut tokio::io::sink()).await?;
    Ok(output)
}

/// Commit the current index. Git owns identity, hooks, signing, conflicts and
/// index locking; no worktree paths are added and no Git settings are overridden.
pub async fn commit_staged(root: &Path, message: &str) -> Result<(), EngineError> {
    commit(root, message, false, None).await
}

pub async fn commit(
    root: &Path,
    message: &str,
    amend: bool,
    expected_head: Option<&str>,
) -> Result<(), EngineError> {
    if message.trim().is_empty() {
        return Err(EngineError::Other("Enter a commit message".into()));
    }
    if message.len() > 32 * 1024 || message.contains('\0') {
        return Err(EngineError::Other(
            "Commit message is invalid or too long".into(),
        ));
    }
    let (files, complete) = status(root).await?;
    if !complete {
        return Err(EngineError::Other(
            "Git status is incomplete; refresh before committing".into(),
        ));
    }
    if expected_head.is_some() || amend {
        let state = crate::checkout_git::state(root).await?;
        if expected_head.is_some() && state.head.as_deref() != expected_head {
            return Err(EngineError::Other(
                "The branch changed; refresh before committing".into(),
            ));
        }
        if amend && (state.head.is_none() || state.operation.is_some()) {
            return Err(EngineError::Other(
                "Amend requires an existing commit and no Git operation in progress".into(),
            ));
        }
    }
    if !amend
        && !files.iter().any(|file| {
            !matches!(
                file.index,
                GitFileState::Unchanged | GitFileState::Untracked
            )
        })
    {
        return Err(EngineError::Other("Stage changes before committing".into()));
    }
    let mut command = tokio::process::Command::new("git");
    if amend {
        command.arg("-C").arg(root).arg("commit").arg("--amend");
    } else {
        command.arg("-C").arg(root).arg("commit");
    }
    command
        .args(["--file=-", "--cleanup=verbatim"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| EngineError::Other("Git stdin unavailable".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| EngineError::Other("Git stdout unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| EngineError::Other("Git stderr unavailable".into()))?;
    let result = tokio::time::timeout(Duration::from_secs(300), async move {
        let write = async move {
            let result = stdin.write_all(message.as_bytes()).await;
            drop(stdin);
            result
        };
        let (write, stdout, stderr, status) = tokio::join!(
            write,
            commit_output(stdout),
            commit_output(stderr),
            child.wait()
        );
        let (stdout, stderr, status) = (stdout?, stderr?, status?);
        if !status.success() {
            let diagnostic = if stderr.is_empty() { &stdout } else { &stderr };
            let diagnostic = String::from_utf8_lossy(diagnostic).trim().to_owned();
            return Err(EngineError::Other(if diagnostic.is_empty() {
                format!("Git commit failed ({status})")
            } else {
                diagnostic
            }));
        }
        write?;
        Ok(())
    })
    .await
    .map_err(|_| {
        EngineError::Other("Git commit timed out; check repository status before retrying".into())
    })?;
    result
}

pub async fn file_diff(
    root: &Path,
    selection: &CheckoutChangeSelection,
    device_id: &str,
) -> Result<CheckoutDiff, EngineError> {
    relative(&selection.path)?;
    let (files, _) = status(root).await?;
    let file = files.iter().find(|f| f.path == selection.path);
    let mut patch;
    let mut summaries;
    let mut truncated = false;
    if !selection.staged && file.is_some_and(|f| f.worktree == GitFileState::Untracked) {
        let path = root.join(&selection.path);
        let parent = tokio::fs::canonicalize(path.parent().unwrap()).await?;
        if !parent.starts_with(tokio::fs::canonicalize(root).await?) {
            return Err(EngineError::Other("File escapes checkout".into()));
        }
        let metadata = tokio::fs::symlink_metadata(&path).await?;
        let bytes = if metadata.file_type().is_symlink() {
            tokio::fs::read_link(&path)
                .await?
                .to_string_lossy()
                .as_bytes()
                .to_vec()
        } else if metadata.is_file() && metadata.len() <= LIMIT as u64 {
            tokio::fs::read(&path).await?
        } else {
            truncated = true;
            Vec::new()
        };
        let binary = bytes.contains(&0) || std::str::from_utf8(&bytes).is_err();
        patch = if binary {
            let a = diff_sync::quote_patch_path(&format!("a/{}", selection.path));
            let b = diff_sync::quote_patch_path(&format!("b/{}", selection.path));
            format!("diff --git {a} {b}\nBinary files differ\n")
        } else {
            diff_sync::untracked_patch(
                &selection.path,
                std::str::from_utf8(&bytes).unwrap_or_default(),
            )
        };
        if metadata.file_type().is_symlink() {
            patch = patch.replacen("100644", "120000", 1);
        }
        summaries = vec![DiffFileSummary {
            path: selection.path.clone(),
            old_path: None,
            status: "added".into(),
            additions: if binary {
                0
            } else {
                std::str::from_utf8(&bytes).unwrap().lines().count() as u32
            },
            deletions: 0,
            binary,
        }];
    } else {
        let base = diff_sync::working_diff_base(root).await?;
        let mut args = vec![
            "--literal-pathspecs",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--find-renames",
            "--ignore-submodules=none",
        ];
        if selection.staged {
            args.extend(["--cached", &base]);
        }
        let mut paths = vec!["--", selection.path.as_str()];
        if let Some(old) = file.and_then(|f| f.old_path.as_deref())
            && (selection.staged
                || file.is_some_and(|f| {
                    matches!(f.worktree, GitFileState::Renamed | GitFileState::Copied)
                }))
        {
            paths.push(old);
        }
        let mut names = args.clone();
        names.push("--name-status");
        names.push("-z");
        names.extend(&paths);
        let mut numbers = args.clone();
        numbers.push("--numstat");
        numbers.push("-z");
        numbers.extend(&paths);
        args.extend(paths);
        let capture = git(root, &args).await?;
        truncated = capture.truncated;
        patch = String::from_utf8_lossy(&capture.stdout).into_owned();
        let names = git(root, &names).await?;
        let numbers = git(root, &numbers).await?;
        truncated |= names.truncated || numbers.truncated;
        summaries = diff_sync::parse_name_status(&names.stdout);
        diff_sync::apply_numstat(&mut summaries, &numbers.stdout);
    }
    let checksum = crate::repos::hex(&Sha256::digest(patch.as_bytes()));
    Ok(CheckoutDiff {
        checkout_id: selection.repository.clone(),
        device_id: device_id.into(),
        cwd: root.to_string_lossy().into_owned(),
        additions: summaries.iter().map(|f| f.additions).sum(),
        deletions: summaries.iter().map(|f| f.deletions).sum(),
        files: summaries,
        patch,
        truncated,
        checksum,
        updated_at: chrono::Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn init(root: &Path) {
        run(root, &["init", "-q"]);
    }
    fn commit(root: &Path) {
        run(root, &["add", "-A"]);
        run(
            root,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "fixture"],
        );
    }
    fn configure_commit(root: &Path) {
        run(root, &["config", "user.name", "Fixture author"]);
        run(root, &["config", "user.email", "author@example.com"]);
        run(root, &["config", "commit.gpgsign", "false"]);
        run(
            root,
            &[
                "config",
                "core.hooksPath",
                root.join("hooks").to_str().unwrap(),
            ],
        );
    }

    #[tokio::test]
    async fn commit_uses_only_the_index_and_preserves_literal_multiline_messages() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init(root);
        configure_commit(root);
        std::fs::write(root.join("tracked"), "original\n").unwrap();
        commit(root);
        std::fs::write(root.join("tracked"), "staged\n").unwrap();
        set_staged(root, &["tracked".into()], true).await.unwrap();
        std::fs::write(root.join("tracked"), "working\n").unwrap();
        std::fs::write(root.join("untracked"), "leave me\n").unwrap();
        let message =
            "Improve $(touch unexpected) `literal` --amend é\n\n# Keep this body exactly.\n";
        commit_staged(root, message).await.unwrap();
        assert_eq!(run(root, &["show", "HEAD:tracked"]), "staged\n");
        assert_eq!(
            std::fs::read_to_string(root.join("tracked")).unwrap(),
            "working\n"
        );
        assert_eq!(run(root, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(run(root, &["ls-files"]), "tracked\n");
        assert_eq!(
            run(root, &["log", "-1", "--format=%B"]),
            format!("{message}\n")
        );
        assert_eq!(
            run(root, &["log", "-1", "--format=%an <%ae>"]),
            "Fixture author <author@example.com>\n"
        );
        assert!(!root.join("unexpected").exists());
    }

    #[tokio::test]
    async fn commit_rejects_blank_messages_and_an_empty_index_without_changing_head() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init(root);
        configure_commit(root);
        std::fs::write(root.join("tracked"), "original\n").unwrap();
        commit(root);
        let head = run(root, &["rev-parse", "HEAD"]);
        assert!(commit_staged(root, " \n\t").await.is_err());
        std::fs::write(root.join("tracked"), "unstaged\n").unwrap();
        assert!(
            commit_staged(root, "Do not stage automatically")
                .await
                .unwrap_err()
                .to_string()
                .contains("Stage changes")
        );
        assert_eq!(run(root, &["rev-parse", "HEAD"]), head);
        set_staged(root, &["tracked".into()], true).await.unwrap();
        assert!(commit_staged(root, "invalid\0message").await.is_err());
        assert!(
            commit_staged(root, &"x".repeat(32 * 1024 + 1))
                .await
                .is_err()
        );
        assert_eq!(run(root, &["rev-parse", "HEAD"]), head);
        assert_eq!(run(root, &["show", ":tracked"]), "unstaged\n");
    }

    #[tokio::test]
    async fn commit_supports_unborn_repositories_and_reports_missing_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init(root);
        configure_commit(root);
        std::fs::write(root.join("first"), "first commit\n").unwrap();
        set_staged(root, &["first".into()], true).await.unwrap();
        run(root, &["config", "user.name", ""]);
        assert!(commit_staged(root, "First commit").await.is_err());
        assert!(
            root.join(".git/refs/heads")
                .read_dir()
                .unwrap()
                .next()
                .is_none()
        );
        assert_eq!(run(root, &["show", ":first"]), "first commit\n");
        configure_commit(root);
        commit_staged(root, "First commit").await.unwrap();
        assert_eq!(run(root, &["rev-list", "--count", "HEAD"]), "1\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn commit_honors_failing_hooks_and_drains_verbose_hook_output() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init(root);
        configure_commit(root);
        std::fs::write(root.join("tracked"), "original\n").unwrap();
        commit(root);
        let head = run(root, &["rev-parse", "HEAD"]);
        std::fs::write(root.join("tracked"), "staged\n").unwrap();
        set_staged(root, &["tracked".into()], true).await.unwrap();
        std::fs::create_dir(root.join("hooks")).unwrap();
        let hook = root.join("hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nprintf 'Rejected by fixture hook\\n' >&2\ni=0\nwhile [ \"$i\" -lt 5000 ]; do\n  printf 'Verbose fixture output that fills both process pipes.\\n'\n  printf 'Verbose fixture diagnostics that fill the stderr pipe.\\n' >&2\n  i=$((i + 1))\ndone\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            commit_staged(root, "Keep the draft"),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(result.to_string().contains("Rejected by fixture hook"));
        assert_eq!(run(root, &["rev-parse", "HEAD"]), head);
        assert_eq!(run(root, &["show", ":tracked"]), "staged\n");
    }

    #[tokio::test]
    async fn committing_a_submodule_preserves_the_parent_index_and_head() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("parent");
        let child = temp.path().join("child");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&child).unwrap();
        init(&child);
        configure_commit(&child);
        std::fs::write(child.join("module"), "original\n").unwrap();
        commit(&child);
        init(&root);
        configure_commit(&root);
        run(
            &root,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                child.to_str().unwrap(),
                "apps/device-apps",
            ],
        );
        commit(&root);
        let parent_head = run(&root, &["rev-parse", "HEAD"]);
        let module = repository(&root, "apps/device-apps").await.unwrap();
        configure_commit(&module);
        std::fs::write(module.join("module"), "staged module\n").unwrap();
        set_staged(&module, &["module".into()], true).await.unwrap();
        commit_staged(&module, "Update the submodule")
            .await
            .unwrap();
        assert_eq!(run(&module, &["show", "HEAD:module"]), "staged module\n");
        assert_eq!(run(&root, &["rev-parse", "HEAD"]), parent_head);
        assert_eq!(run(&root, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(
            status(&root).await.unwrap().0[0].index,
            GitFileState::Unchanged
        );
    }

    fn selection(root: &Path, path: &str, staged: bool) -> CheckoutChangeSelection {
        CheckoutChangeSelection {
            cwd: root.to_string_lossy().into_owned(),
            repository: String::new(),
            path: path.into(),
            staged,
        }
    }

    #[tokio::test]
    async fn partially_staged_diffs_and_unstage_preserve_contents_and_other_index_entries() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init(root);
        std::fs::write(root.join("file.rs"), "old\n").unwrap();
        std::fs::write(root.join("other"), "old\n").unwrap();
        commit(root);
        std::fs::write(root.join("other"), "keep staged\n").unwrap();
        set_staged(root, &["other".into()], true).await.unwrap();
        std::fs::write(root.join("file.rs"), "staged\n").unwrap();
        set_staged(root, &["file.rs".into()], true).await.unwrap();
        std::fs::write(root.join("file.rs"), "working\n").unwrap();
        let staged = file_diff(root, &selection(root, "file.rs", true), "test")
            .await
            .unwrap();
        assert!(staged.patch.contains("-old\n+staged"));
        assert!(!staged.patch.contains("working"));
        let unstaged = file_diff(root, &selection(root, "file.rs", false), "test")
            .await
            .unwrap();
        assert!(unstaged.patch.contains("-staged\n+working"));
        assert!(!unstaged.patch.contains("-old"));
        set_staged(root, &["file.rs".into()], false).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("file.rs")).unwrap(),
            "working\n"
        );
        assert_eq!(run(root, &["show", ":file.rs"]), "old\n");
        assert_eq!(run(root, &["show", ":other"]), "keep staged\n");
    }

    #[tokio::test]
    async fn unborn_repository_and_literal_newline_wildcard_names_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init(root);
        let name = "literal[1]*\n name.txt";
        std::fs::write(root.join(name), "new\n").unwrap();
        std::fs::write(root.join("literal1-other.txt"), "unrelated\n").unwrap();
        let before = file_diff(root, &selection(root, name, false), "test")
            .await
            .unwrap();
        assert_eq!(before.files[0].path, name);
        assert_eq!(before.additions, 1);
        set_staged(root, &[name.into()], true).await.unwrap();
        let files = status(root).await.unwrap().0;
        assert_eq!(
            files.iter().find(|f| f.path == name).unwrap().index,
            GitFileState::Added
        );
        assert_eq!(
            files
                .iter()
                .find(|f| f.path == "literal1-other.txt")
                .unwrap()
                .index,
            GitFileState::Untracked
        );
        set_staged(root, &[name.into()], false).await.unwrap();
        assert!(root.join(name).exists());
        assert_eq!(run(root, &["ls-files"]), "");
    }

    #[tokio::test]
    async fn unstaging_a_rename_restores_both_index_paths_and_keeps_the_rename_on_disk() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init(root);
        std::fs::write(root.join("old name"), "same contents\n").unwrap();
        commit(root);
        run(root, &["mv", "old name", "new name"]);
        let diff = file_diff(root, &selection(root, "new name", true), "test")
            .await
            .unwrap();
        assert_eq!(diff.files[0].old_path.as_deref(), Some("old name"));
        set_staged(root, &["new name".into()], false).await.unwrap();
        assert_eq!(run(root, &["diff", "--cached", "--name-only"]), "");
        assert!(!root.join("old name").exists());
        assert!(root.join("new name").exists());
    }

    #[tokio::test]
    async fn staging_further_edits_to_an_index_rename_does_not_use_the_removed_source() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init(root);
        std::fs::write(root.join("old"), "one\ntwo\nthree\n").unwrap();
        commit(root);
        run(root, &["mv", "old", "new"]);
        std::fs::write(root.join("new"), "one\ntwo\nchanged\n").unwrap();
        set_staged(root, &["new".into()], true).await.unwrap();
        assert_eq!(run(root, &["show", ":new"]), "one\ntwo\nchanged\n");
        assert_eq!(run(root, &["diff", "--name-only"]), "");
    }

    #[tokio::test]
    async fn submodule_contents_use_their_own_index_and_uninitialized_modules_are_not_parent_repos()
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("parent");
        let child = temp.path().join("child");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&child).unwrap();
        init(&child);
        std::fs::write(child.join("file.rs"), "original\n").unwrap();
        commit(&child);
        init(&root);
        run(
            &root,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                child.to_str().unwrap(),
                "apps/device-apps",
            ],
        );
        commit(&root);
        let module = root.join("apps/device-apps");
        std::fs::write(module.join("file.rs"), "module change\n").unwrap();
        let snapshot = list(&root).await.unwrap();
        assert_eq!(snapshot.repositories.len(), 2);
        assert_eq!(snapshot.repositories[0].submodules, ["apps/device-apps"]);
        assert!(
            snapshot.repositories[0]
                .files
                .iter()
                .any(|f| f.path == "apps/device-apps")
        );
        assert_eq!(snapshot.repositories[1].files[0].path, "file.rs");
        let resolved = repository(&root, "apps/device-apps").await.unwrap();
        set_staged(&resolved, &["file.rs".into()], true)
            .await
            .unwrap();
        assert_eq!(run(&root, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(
            run(&module, &["diff", "--cached", "--name-only"]),
            "file.rs\n"
        );
        assert!(repository(&root, "../child").await.is_err());
        assert!(repository(&root, "apps").await.is_err());
        run(
            &root,
            &["submodule", "deinit", "-f", "--", "apps/device-apps"],
        );
        assert_eq!(list(&root).await.unwrap().repositories.len(), 1);
        assert!(repository(&root, "apps/device-apps").await.is_err());
    }

    #[tokio::test]
    async fn binary_files_and_deleted_files_are_listed_and_stageable() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init(root);
        std::fs::write(root.join("deleted"), "delete me\n").unwrap();
        commit(root);
        std::fs::remove_file(root.join("deleted")).unwrap();
        std::fs::write(root.join("binary.bin"), b"a\0b").unwrap();
        let diff = file_diff(root, &selection(root, "binary.bin", false), "test")
            .await
            .unwrap();
        assert!(diff.files[0].binary);
        set_staged(root, &["binary.bin".into(), "deleted".into()], true)
            .await
            .unwrap();
        let files = status(root).await.unwrap().0;
        assert_eq!(
            files.iter().find(|f| f.path == "deleted").unwrap().index,
            GitFileState::Deleted
        );
        set_staged(root, &["binary.bin".into(), "deleted".into()], false)
            .await
            .unwrap();
        assert!(!root.join("deleted").exists());
        assert_eq!(std::fs::read(root.join("binary.bin")).unwrap(), b"a\0b");
    }
}
