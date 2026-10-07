//! Git operations scoped to the selected checkout or an initialized submodule.
use crate::{EngineError, checkout_changes, diff_sync};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use zeron_proto::{
    GitHistoryCommit, RepositoryGitAction as Action, RepositoryGitActionResult,
    RepositoryGitBranch, RepositoryGitDetails, RepositoryGitRemote, RepositoryGitStash,
    RepositoryGitState,
};

fn fail(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}

async fn read(root: &Path, args: &[&str]) -> Result<Vec<u8>, EngineError> {
    let capture = tokio::time::timeout(
        Duration::from_secs(10),
        diff_sync::capture_git(root, args, 3 * 1024 * 1024),
    )
    .await
    .map_err(|_| fail("Git status timed out"))??;
    if capture.truncated {
        return Err(fail("Git metadata is incomplete; narrow the selection"));
    }
    Ok(capture.stdout)
}

pub(crate) async fn text(root: &Path, args: &[&str]) -> Result<String, EngineError> {
    String::from_utf8(read(root, args).await?)
        .map(|s| s.strip_suffix('\n').unwrap_or(&s).to_owned())
        .map_err(|_| fail("Git metadata is not UTF-8"))
}

/// Read-only status never contacts a remote. Fetch explicitly to update counts.
pub async fn state(root: &Path) -> Result<RepositoryGitState, EngineError> {
    let output = read(
        root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=no",
            "--ignore-submodules=all",
        ],
    )
    .await?;
    let mut state = RepositoryGitState::default();
    for record in output.split(|b| *b == 0) {
        let line = std::str::from_utf8(record).map_err(|_| fail("Git status is not UTF-8"))?;
        if let Some(oid) = line.strip_prefix("# branch.oid ") {
            if oid != "(initial)" {
                state.head = Some(oid.into());
            }
        } else if let Some(branch) = line.strip_prefix("# branch.head ") {
            if branch != "(detached)" {
                state.branch = Some(branch.into());
            }
        } else if let Some(upstream) = line.strip_prefix("# branch.upstream ") {
            state.upstream = Some(upstream.into());
        } else if let Some(counts) = line.strip_prefix("# branch.ab ") {
            let mut parts = counts.split_whitespace();
            state.ahead = parts
                .next()
                .and_then(|s| s.strip_prefix('+'))
                .and_then(|s| s.parse().ok());
            state.behind = parts
                .next()
                .and_then(|s| s.strip_prefix('-'))
                .and_then(|s| s.parse().ok());
        } else if line.starts_with("u ") {
            state.conflicts = state.conflicts.saturating_add(1);
        }
    }
    let directory = git_dir(root).await?;
    state.operation =
        if directory.join("rebase-merge").exists() || directory.join("rebase-apply").exists() {
            Some("rebase".into())
        } else if directory.join("MERGE_HEAD").exists() {
            Some("merge".into())
        } else if directory.join("CHERRY_PICK_HEAD").exists() {
            Some("cherryPick".into())
        } else if directory.join("REVERT_HEAD").exists() {
            Some("revert".into())
        } else {
            None
        };
    Ok(state)
}

pub(crate) async fn git_dir(root: &Path) -> Result<PathBuf, EngineError> {
    Ok(PathBuf::from(
        text(root, &["rev-parse", "--absolute-git-dir"]).await?,
    ))
}

fn oid(value: &str) -> Result<&str, EngineError> {
    if matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(value)
    } else {
        Err(fail("Invalid Git object ID"))
    }
}

async fn commits(root: &Path, revision: &str) -> Result<Vec<GitHistoryCommit>, EngineError> {
    let bytes = read(
        root,
        &[
            "log",
            "-30",
            "-z",
            "--format=%H%x00%P%x00%s%x00%an%x00%ae%x00%aI",
            revision,
            "--",
        ],
    )
    .await?;
    let fields: Vec<_> = bytes.split(|b| *b == 0).collect();
    let mut commits = Vec::new();
    for row in fields.chunks_exact(6) {
        let values: Result<Vec<_>, _> = row.iter().map(|s| std::str::from_utf8(s)).collect();
        let values = values.map_err(|_| fail("Commit metadata is not UTF-8"))?;
        oid(values[0])?;
        commits.push(GitHistoryCommit {
            sha: values[0].into(),
            parent_shas: values[1].split_whitespace().map(str::to_owned).collect(),
            subject: values[2].into(),
            author_name: values[3].into(),
            author_email: values[4].into(),
            authored_at: values[5].into(),
            refs: Vec::new(),
        });
    }
    Ok(commits)
}

fn redact_url(value: &str) -> String {
    if let Some((scheme, rest)) = value.split_once("://") {
        let authority = rest.split('/').next().unwrap_or(rest);
        if let Some(at) = authority.rfind('@') {
            return format!("{scheme}://{}", &rest[at + 1..]);
        }
    }
    value.into()
}

pub async fn details(root: &Path) -> Result<RepositoryGitDetails, EngineError> {
    let state = state(root).await?;
    let refs = text(
        root,
        &[
            "for-each-ref",
            "--count=512",
            "--format=%(refname)%00%(symref)%00%(HEAD)",
            "refs/heads",
            "refs/remotes",
        ],
    )
    .await?;
    let mut branches = Vec::new();
    for line in refs.lines() {
        let fields: Vec<_> = line.split('\0').collect();
        if fields.len() != 3 || !fields[1].is_empty() {
            continue;
        }
        let (name, remote) = if let Some(name) = fields[0].strip_prefix("refs/heads/") {
            (name, false)
        } else if let Some(name) = fields[0].strip_prefix("refs/remotes/") {
            (name, true)
        } else {
            continue;
        };
        branches.push(RepositoryGitBranch {
            name: name.into(),
            remote,
            current: fields[2] == "*",
        });
    }
    let mut remotes = Vec::new();
    for name in text(root, &["remote"]).await?.lines() {
        remotes.push(RepositoryGitRemote {
            name: name.into(),
            url: redact_url(&text(root, &["remote", "get-url", name]).await?),
        });
    }
    let mut stashes = Vec::new();
    for line in text(root, &["stash", "list", "-100", "--format=%H%x00%gd%x00%s"])
        .await?
        .lines()
    {
        let fields: Vec<_> = line.splitn(3, '\0').collect();
        if fields.len() == 3 {
            stashes.push(RepositoryGitStash {
                sha: fields[0].into(),
                selector: fields[1].into(),
                subject: fields[2].into(),
            });
        }
    }
    let (recent, last_message) = if state.head.is_some() {
        (
            commits(root, "HEAD").await?,
            text(root, &["log", "-1", "--format=%B"]).await?,
        )
    } else {
        (Vec::new(), String::new())
    };
    let (incoming, outgoing) = if state.upstream.is_some() && state.ahead.is_some() {
        (
            commits(root, "HEAD..@{upstream}").await?,
            commits(root, "@{upstream}..HEAD").await?,
        )
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(RepositoryGitDetails {
        state,
        branches,
        remotes,
        stashes,
        incoming,
        outgoing,
        recent,
        last_message,
    })
}

/// Drain both output pipes while Git runs, including credential/signing hooks.
pub(crate) async fn run(
    root: &Path,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<String, EngineError> {
    let mut command = tokio::process::Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdin = child.stdin.take();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| fail("Git stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| fail("Git stderr unavailable"))?;
    tokio::time::timeout(Duration::from_secs(300), async move {
        let write = async move {
            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                stdin.write_all(input).await?;
            }
            Ok::<_, std::io::Error>(())
        };
        let (write, stdout, stderr, status) = tokio::join!(
            write,
            checkout_changes::commit_output(stdout),
            checkout_changes::commit_output(stderr),
            child.wait()
        );
        let (stdout, stderr, status) = (stdout?, stderr?, status?);
        if !status.success() {
            let diagnostic =
                String::from_utf8_lossy(if stderr.is_empty() { &stdout } else { &stderr })
                    .trim()
                    .to_owned();
            return Err(fail(if diagnostic.is_empty() {
                format!("Git operation failed ({status})")
            } else {
                diagnostic
            }));
        }
        write?;
        Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
    })
    .await
    .map_err(|_| fail("Git operation timed out; refresh the repository before retrying"))?
}

async fn branch_name(root: &Path, name: &str) -> Result<(), EngineError> {
    if name.is_empty()
        || name.len() > 1024
        || name.contains('\0')
        || name.starts_with('-')
        || name.starts_with('@')
    {
        return Err(fail("Enter a valid branch name"));
    }
    run(root, &["check-ref-format", "--branch", name], None).await?;
    Ok(())
}

async fn known_branch(root: &Path, name: &str) -> Result<RepositoryGitBranch, EngineError> {
    details(root)
        .await?
        .branches
        .into_iter()
        .find(|b| b.name == name)
        .ok_or_else(|| fail("Branch is unavailable; refresh the branch list"))
}

async fn known_remote(root: &Path, name: &str) -> Result<(), EngineError> {
    if !text(root, &["remote"])
        .await?
        .lines()
        .any(|remote| remote == name)
    {
        return Err(fail("Select an existing remote"));
    }
    Ok(())
}

async fn push(root: &Path) -> Result<(), EngineError> {
    let state = state(root).await?;
    let branch = state
        .branch
        .ok_or_else(|| fail("Switch to a branch before pushing"))?;
    if state.head.is_none() {
        return Err(fail("Create a commit before pushing"));
    }
    if state.upstream.is_none() {
        return Err(fail("Publish this branch to set its upstream"));
    }
    let tracking = text(
        root,
        &[
            "for-each-ref",
            "--format=%(upstream:remotename)%00%(upstream:remoteref)",
            &format!("refs/heads/{branch}"),
        ],
    )
    .await?;
    let (tracking_remote, tracking_ref) = tracking
        .split_once('\0')
        .ok_or_else(|| fail("Upstream is unavailable; fetch and refresh"))?;
    let configured_remote = text(
        root,
        &["config", "--get", &format!("branch.{branch}.pushRemote")],
    )
    .await
    .ok()
    .or(text(root, &["config", "--get", "remote.pushDefault"])
        .await
        .ok());
    let remote = configured_remote.as_deref().unwrap_or(tracking_remote);
    if remote != "." {
        known_remote(root, remote).await?;
    }
    let reference = if remote == tracking_remote {
        tracking_ref.to_owned()
    } else {
        format!("refs/heads/{branch}")
    };
    if !reference.starts_with("refs/heads/") {
        return Err(fail("The upstream does not identify a remote branch"));
    }
    run(
        root,
        &["push", "--", remote, &format!("HEAD:{reference}")],
        None,
    )
    .await?;
    Ok(())
}

async fn pull(root: &Path, rebase: Option<bool>) -> Result<(), EngineError> {
    if state(root).await?.upstream.is_none() {
        return Err(fail(
            "Publish this branch or set an upstream before pulling",
        ));
    }
    let mut args = vec!["pull", "--no-edit"];
    if let Some(rebase) = rebase {
        args.push(if rebase { "--rebase" } else { "--no-rebase" });
    }
    run(root, &args, None).await?;
    Ok(())
}

async fn stash_entry(root: &Path, sha: &str) -> Result<RepositoryGitStash, EngineError> {
    oid(sha)?;
    details(root)
        .await?
        .stashes
        .into_iter()
        .find(|stash| stash.sha == sha)
        .ok_or_else(|| fail("Stash is unavailable; refresh the list"))
}

pub async fn perform(
    root: &Path,
    action: Action,
    expected_head: Option<&str>,
) -> Result<RepositoryGitActionResult, EngineError> {
    // Keep the many Git action futures off the caller's stack, especially the
    // dispatcher and tests that submit several operations in one async block.
    Box::pin(perform_inner(root, action, expected_head)).await
}

async fn perform_inner(
    root: &Path,
    action: Action,
    expected_head: Option<&str>,
) -> Result<RepositoryGitActionResult, EngineError> {
    let before = state(root).await?;
    if !matches!(action, Action::Fetch) && before.head.as_deref() != expected_head {
        return Err(fail(
            "The branch changed; refresh before running this action",
        ));
    }
    let mut commit_message = None;
    let notice = match action {
        Action::Fetch => {
            run(root, &["fetch", "--all", "--prune"], None).await?;
            "Remote references fetched"
        }
        Action::Pull { rebase } => {
            pull(root, rebase).await?;
            "Incoming commits pulled"
        }
        Action::Push => {
            push(root).await?;
            "Commits pushed"
        }
        Action::Publish { remote } => {
            known_remote(root, &remote).await?;
            let branch = before
                .branch
                .ok_or_else(|| fail("Switch to a branch before publishing"))?;
            if before.head.is_none() {
                return Err(fail("Create a commit before publishing"));
            }
            run(
                root,
                &[
                    "push",
                    "--set-upstream",
                    "--",
                    &remote,
                    &format!("HEAD:refs/heads/{branch}"),
                ],
                None,
            )
            .await?;
            "Branch published"
        }
        Action::Sync => {
            pull(root, None).await?;
            push(root).await?;
            "Incoming and outgoing commits synchronized"
        }
        Action::SwitchBranch { branch } => {
            let selected = known_branch(root, &branch).await?;
            if selected.remote {
                let local = branch
                    .split_once('/')
                    .map(|(_, name)| name)
                    .unwrap_or(&branch);
                let tracking = text(
                    root,
                    &[
                        "for-each-ref",
                        "--format=%(upstream:short)",
                        &format!("refs/heads/{local}"),
                    ],
                )
                .await?;
                if tracking == branch {
                    run(root, &["switch", "--", local], None).await?;
                } else {
                    run(root, &["switch", "--track", "--", &branch], None).await?;
                }
            } else {
                run(root, &["switch", "--", &branch], None).await?;
            }
            "Branch switched"
        }
        Action::CreateBranch { name, start } => {
            branch_name(root, &name).await?;
            let mut args = vec!["switch", "--create", &name];
            if let Some(start) = &start {
                known_branch(root, start).await?;
                args.extend(["--", start.as_str()]);
            }
            run(root, &args, None).await?;
            "Branch created"
        }
        Action::RenameBranch { name } => {
            branch_name(root, &name).await?;
            run(root, &["branch", "--move", &name], None).await?;
            "Branch renamed"
        }
        Action::DeleteBranch { name } => {
            let selected = known_branch(root, &name).await?;
            if selected.remote || selected.current {
                return Err(fail(
                    "Select a local branch that is not currently checked out",
                ));
            }
            run(root, &["branch", "--delete", "--", &name], None).await?;
            "Merged branch deleted"
        }
        Action::Merge { branch } => {
            known_branch(root, &branch).await?;
            run(root, &["merge", "--no-edit", "--", &branch], None).await?;
            "Branch merged"
        }
        Action::Rebase { branch } => {
            known_branch(root, &branch).await?;
            run(root, &["rebase", "--", &branch], None).await?;
            "Branch rebased"
        }
        Action::UndoCommit => {
            if before.operation.is_some() {
                return Err(fail(
                    "Finish the current Git operation before undoing a commit",
                ));
            }
            if before.upstream.is_some() && !before.ahead.is_some_and(|n| n > 0) {
                return Err(fail(
                    "The last commit is not known to be unpublished; fetch before undoing it",
                ));
            }
            let parent = text(root, &["rev-parse", "--verify", "HEAD^"]).await?;
            oid(&parent)?;
            commit_message = Some(text(root, &["log", "-1", "--format=%B"]).await?);
            let head = before
                .head
                .as_deref()
                .ok_or_else(|| fail("There is no commit to undo"))?;
            run(
                root,
                &[
                    "update-ref",
                    &format!("refs/zeron/undo/{}", uuid::Uuid::new_v4()),
                    head,
                ],
                None,
            )
            .await?;
            run(root, &["reset", "--soft", &parent], None).await?;
            "Last commit undone; its changes remain staged"
        }
        Action::RevertCommit { sha } => {
            oid(&sha)?;
            run(root, &["revert", "--no-edit", &sha], None).await?;
            "Commit reverted with a new commit"
        }
        Action::CherryPick { sha } => {
            oid(&sha)?;
            run(root, &["cherry-pick", &sha], None).await?;
            "Commit cherry-picked"
        }
        Action::Stash {
            message,
            include_untracked,
        } => {
            if message.len() > 32 * 1024 || message.contains('\0') {
                return Err(fail("Stash message is invalid or too long"));
            }
            let mut args = vec!["stash", "push"];
            if include_untracked {
                args.push("--include-untracked");
            }
            if !message.is_empty() {
                args.extend(["--message", &message]);
            }
            let output = run(root, &args, None).await?;
            if output.contains("No local changes to save") {
                "No local changes to stash"
            } else {
                "Changes stashed"
            }
        }
        Action::ApplyStash { sha } => {
            stash_entry(root, &sha).await?;
            run(root, &["stash", "apply", "--index", &sha], None).await?;
            "Stash applied and kept"
        }
        Action::PopStash { sha } => {
            stash_entry(root, &sha).await?;
            run(root, &["stash", "apply", "--index", &sha], None).await?;
            let entry = stash_entry(root, &sha).await?;
            run(root, &["stash", "drop", &entry.selector], None).await?;
            "Stash applied and removed"
        }
        Action::DropStash { sha } => {
            let entry = stash_entry(root, &sha).await?;
            run(root, &["stash", "drop", &entry.selector], None).await?;
            "Stash removed"
        }
        Action::AddRemote { name, url } => {
            if name.is_empty()
                || name.starts_with('-')
                || name.contains(['\0', '\n', '\r'])
                || url.trim().is_empty()
                || url.len() > 8192
                || url.contains(['\0', '\n', '\r'])
            {
                return Err(fail("Enter a valid remote name and URL"));
            }
            run(root, &["remote", "add", "--", &name, &url], None).await?;
            "Remote added"
        }
        Action::RemoveRemote { name } => {
            known_remote(root, &name).await?;
            run(root, &["remote", "remove", &name], None).await?;
            "Remote removed"
        }
        Action::Continue => {
            if before.conflicts != 0 {
                return Err(fail(
                    "Resolve and stage all merge conflicts before continuing",
                ));
            }
            let args = match before.operation.as_deref() {
                Some("merge") => ["merge", "--continue"],
                Some("rebase") => ["rebase", "--continue"],
                Some("cherryPick") => ["cherry-pick", "--continue"],
                Some("revert") => ["revert", "--continue"],
                _ => return Err(fail("There is no Git operation to continue")),
            };
            run(root, &args, None).await?;
            "Git operation completed"
        }
        Action::Abort => {
            let args = match before.operation.as_deref() {
                Some("merge") => ["merge", "--abort"],
                Some("rebase") => ["rebase", "--abort"],
                Some("cherryPick") => ["cherry-pick", "--abort"],
                Some("revert") => ["revert", "--abort"],
                _ => return Err(fail("There is no Git operation to abort")),
            };
            run(root, &args, None).await?;
            "Git operation aborted"
        }
        Action::ResolveConflict { path, incoming } => {
            crate::checkout_discard::validate_path(root, &path).await?;
            let (files, complete) = checkout_changes::status(root).await?;
            if !complete
                || !files.iter().any(|f| {
                    f.path == path
                        && (f.index == zeron_proto::GitFileState::Unmerged
                            || f.worktree == zeron_proto::GitFileState::Unmerged)
                })
            {
                return Err(fail("This file is no longer an unresolved conflict"));
            }
            let payload = format!("{path}\0");
            run(
                root,
                &[
                    "--literal-pathspecs",
                    "restore",
                    if incoming { "--theirs" } else { "--ours" },
                    "--worktree",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                ],
                Some(payload.as_bytes()),
            )
            .await?;
            "Conflict version selected; review the file and stage it when resolved"
        }
    };
    Ok(RepositoryGitActionResult {
        notice: notice.into(),
        commit_message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    fn git(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim_end().into()
    }
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.name", "Fixture"]);
        git(dir.path(), &["config", "user.email", "fixture@example.com"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join("file.txt"), "original\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "Initial"]);
        dir
    }
    async fn act(root: &Path, action: Action) -> Result<RepositoryGitActionResult, EngineError> {
        let head = state(root).await.unwrap().head;
        perform(root, action, head.as_deref()).await
    }
    #[tokio::test]
    async fn branches_stashes_amend_and_undo_preserve_work() {
        let dir = repo();
        let root = dir.path();
        assert_eq!(details(root).await.unwrap().recent.len(), 1);
        act(
            root,
            Action::CreateBranch {
                name: "feature/git".into(),
                start: Some("main".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            state(root).await.unwrap().branch.as_deref(),
            Some("feature/git")
        );
        std::fs::write(root.join("file.txt"), "staged\n").unwrap();
        git(root, &["add", "file.txt"]);
        std::fs::write(root.join("file.txt"), "unstaged\n").unwrap();
        std::fs::write(root.join("new.txt"), "untracked\n").unwrap();
        act(
            root,
            Action::Stash {
                message: "Saved work".into(),
                include_untracked: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "original\n"
        );
        let saved = details(root).await.unwrap().stashes[0].sha.clone();
        act(root, Action::PopStash { sha: saved }).await.unwrap();
        assert!(details(root).await.unwrap().stashes.is_empty());
        assert_eq!(git(root, &["show", ":file.txt"]), "staged");
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "unstaged\n"
        );
        checkout_changes::commit_staged(root, "Feature")
            .await
            .unwrap();
        let head = state(root).await.unwrap().head.unwrap();
        checkout_changes::commit(root, "Amended feature", true, Some(&head))
            .await
            .unwrap();
        assert_eq!(
            details(root).await.unwrap().last_message.trim(),
            "Amended feature"
        );
        let undone = act(root, Action::UndoCommit).await.unwrap();
        assert_eq!(
            undone.commit_message.as_deref().map(str::trim),
            Some("Amended feature")
        );
        assert_eq!(git(root, &["show", ":file.txt"]), "staged");
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "unstaged\n"
        );
        act(
            root,
            Action::RenameBranch {
                name: "renamed".into(),
            },
        )
        .await
        .unwrap();
        act(
            root,
            Action::SwitchBranch {
                branch: "main".into(),
            },
        )
        .await
        .unwrap();
        act(
            root,
            Action::DeleteBranch {
                name: "renamed".into(),
            },
        )
        .await
        .unwrap();
        assert!(
            perform(root, Action::UndoCommit, Some(&head))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn publish_fetch_pull_push_and_sync_count_commits_on_a_local_remote() {
        let dir = repo();
        let root = dir.path();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
        act(
            root,
            Action::AddRemote {
                name: "origin".into(),
                url: remote.path().to_str().unwrap().into(),
            },
        )
        .await
        .unwrap();
        act(
            root,
            Action::Publish {
                remote: "origin".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(state(root).await.unwrap().ahead, Some(0));
        let peer = tempfile::tempdir().unwrap();
        git(
            peer.path(),
            &["clone", "-q", remote.path().to_str().unwrap(), "."],
        );
        git(peer.path(), &["config", "user.name", "Peer"]);
        git(peer.path(), &["config", "user.email", "peer@example.com"]);
        std::fs::write(peer.path().join("remote.txt"), "remote\n").unwrap();
        git(peer.path(), &["add", "-A"]);
        git(
            peer.path(),
            &["-c", "commit.gpgsign=false", "commit", "-qm", "Incoming"],
        );
        git(peer.path(), &["push", "-q"]);
        std::fs::write(root.join("local.txt"), "local\n").unwrap();
        git(root, &["add", "-A"]);
        checkout_changes::commit_staged(root, "Outgoing")
            .await
            .unwrap();
        act(root, Action::Fetch).await.unwrap();
        let metadata = details(root).await.unwrap();
        assert_eq!(
            (metadata.state.ahead, metadata.state.behind),
            (Some(1), Some(1))
        );
        assert_eq!(metadata.incoming[0].subject, "Incoming");
        assert_eq!(metadata.outgoing[0].subject, "Outgoing");
        assert!(act(root, Action::Push).await.is_err());
        act(root, Action::Pull { rebase: Some(true) })
            .await
            .unwrap();
        act(root, Action::Push).await.unwrap();
        assert_eq!(
            (
                state(root).await.unwrap().ahead,
                state(root).await.unwrap().behind
            ),
            (Some(0), Some(0))
        );
        assert!(act(root, Action::UndoCommit).await.is_err());
        git(root, &["config", "pull.rebase", "true"]);
        act(root, Action::Sync).await.unwrap();
        assert_eq!(
            git(remote.path(), &["rev-parse", "main"]),
            git(root, &["rev-parse", "HEAD"])
        );
        act(
            root,
            Action::RemoveRemote {
                name: "origin".into(),
            },
        )
        .await
        .unwrap();
        assert!(details(root).await.unwrap().remotes.is_empty());
    }
    #[tokio::test]
    async fn merge_conflict_versions_continue_and_abort_use_real_git_state() {
        let dir = repo();
        let root = dir.path();
        git(root, &["switch", "-qc", "other"]);
        std::fs::write(root.join("file.txt"), "incoming\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "Other"]);
        git(root, &["switch", "-q", "main"]);
        std::fs::write(root.join("file.txt"), "current\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "Current"]);
        assert!(
            act(
                root,
                Action::Merge {
                    branch: "other".into()
                }
            )
            .await
            .is_err()
        );
        let status = state(root).await.unwrap();
        assert_eq!(status.operation.as_deref(), Some("merge"));
        assert_eq!(status.conflicts, 1);
        assert!(act(root, Action::Continue).await.is_err());
        act(
            root,
            Action::ResolveConflict {
                path: "file.txt".into(),
                incoming: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "incoming\n"
        );
        act(root, Action::Abort).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "current\n"
        );
        assert!(
            act(
                root,
                Action::Merge {
                    branch: "other".into()
                }
            )
            .await
            .is_err()
        );
        act(
            root,
            Action::ResolveConflict {
                path: "file.txt".into(),
                incoming: false,
            },
        )
        .await
        .unwrap();
        checkout_changes::set_staged(root, &["file.txt".into()], true)
            .await
            .unwrap();
        act(root, Action::Continue).await.unwrap();
        assert_eq!(state(root).await.unwrap().operation, None);
    }
    #[tokio::test]
    async fn discard_restores_index_and_undo_recovers_partial_staging_and_untracked_files() {
        let dir = repo();
        let root = dir.path();
        std::fs::write(root.join("file.txt"), "staged\n").unwrap();
        git(root, &["add", "file.txt"]);
        std::fs::write(root.join("file.txt"), "worktree\n").unwrap();
        std::fs::write(root.join("[literal]\nfile.txt"), "untracked\n").unwrap();
        let index = git(root, &["diff", "--cached", "--binary"]);
        let paths = vec!["file.txt".into(), "[literal]\nfile.txt".into()];
        let preview = crate::checkout_discard::preview(root, &paths, false)
            .await
            .unwrap();
        let recovery = crate::checkout_discard::discard(root, preview)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "staged\n"
        );
        assert!(!root.join("[literal]\nfile.txt").exists());
        assert_eq!(git(root, &["diff", "--cached", "--binary"]), index);
        crate::checkout_discard::undo(root, &recovery.recovery_id, &recovery.checksum)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "worktree\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("[literal]\nfile.txt")).unwrap(),
            "untracked\n"
        );
        assert_eq!(git(root, &["diff", "--cached", "--binary"]), index);
        let preview = crate::checkout_discard::preview(root, &paths, false)
            .await
            .unwrap();
        std::fs::write(root.join("file.txt"), "newer\n").unwrap();
        assert!(
            crate::checkout_discard::discard(root, preview)
                .await
                .is_err()
        );
        let preview = crate::checkout_discard::preview(root, &paths, true)
            .await
            .unwrap();
        let recovery = crate::checkout_discard::discard(root, preview)
            .await
            .unwrap();
        assert!(git(root, &["diff", "--cached"]).is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "original\n"
        );
        std::fs::write(root.join("file.txt"), "do not overwrite\n").unwrap();
        assert!(
            crate::checkout_discard::undo(root, &recovery.recovery_id, &recovery.checksum)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "do not overwrite\n"
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn discard_handles_symlinks_deletions_renames_and_refuses_symlink_ancestors() {
        let dir = repo();
        let root = dir.path();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("file.txt"), "outside\n").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("escape")).unwrap();
        assert!(
            crate::checkout_discard::validate_path(root, "escape/file.txt")
                .await
                .is_err()
        );
        assert!(
            crate::checkout_discard::validate_path(root, "../file.txt")
                .await
                .is_err()
        );
        assert!(
            crate::checkout_discard::validate_path(root, ".git/config")
                .await
                .is_err()
        );
        std::os::unix::fs::symlink("file.txt", root.join("link")).unwrap();
        git(root, &["add", "link"]);
        git(root, &["commit", "-qm", "Link"]);
        std::fs::remove_file(root.join("link")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("file.txt"), root.join("link")).unwrap();
        std::fs::remove_file(root.join("file.txt")).unwrap();
        let preview =
            crate::checkout_discard::preview(root, &["link".into(), "file.txt".into()], false)
                .await
                .unwrap();
        let saved = crate::checkout_discard::discard(root, preview)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_link(root.join("link")).unwrap(),
            PathBuf::from("file.txt")
        );
        crate::checkout_discard::undo(root, &saved.recovery_id, &saved.checksum)
            .await
            .unwrap();
        assert!(!root.join("file.txt").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("link")).unwrap(),
            "outside\n"
        );
        git(root, &["restore", "file.txt"]);
        git(root, &["mv", "file.txt", "renamed.txt"]);
        let preview = crate::checkout_discard::preview(root, &["renamed.txt".into()], true)
            .await
            .unwrap();
        let saved = crate::checkout_discard::discard(root, preview)
            .await
            .unwrap();
        assert!(root.join("file.txt").exists());
        assert!(!root.join("renamed.txt").exists());
        crate::checkout_discard::undo(root, &saved.recovery_id, &saved.checksum)
            .await
            .unwrap();
        assert!(!root.join("file.txt").exists());
        assert!(root.join("renamed.txt").exists());
    }

    #[tokio::test]
    async fn revert_and_cherry_pick_create_commits_without_rewriting_history() {
        let dir = repo();
        let root = dir.path();
        let initial = git(root, &["rev-parse", "HEAD"]);
        git(root, &["switch", "-qc", "feature"]);
        std::fs::write(root.join("feature.txt"), "feature\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "Feature"]);
        let feature = git(root, &["rev-parse", "HEAD"]);
        git(root, &["switch", "-q", "main"]);
        act(root, Action::CherryPick { sha: feature })
            .await
            .unwrap();
        let picked = git(root, &["rev-parse", "HEAD"]);
        assert_eq!(git(root, &["rev-parse", "HEAD^"]), initial);
        assert!(root.join("feature.txt").exists());
        act(
            root,
            Action::RevertCommit {
                sha: picked.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(git(root, &["rev-parse", "HEAD^"]), picked);
        assert!(!root.join("feature.txt").exists());
    }
    #[tokio::test]
    async fn discard_and_recovery_work_before_the_initial_commit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("new.txt"), "staged\n").unwrap();
        git(root, &["add", "new.txt"]);
        std::fs::write(root.join("new.txt"), "unstaged\n").unwrap();
        let preview = crate::checkout_discard::preview(root, &["new.txt".into()], false)
            .await
            .unwrap();
        let saved = crate::checkout_discard::discard(root, preview)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("new.txt")).unwrap(),
            "staged\n"
        );
        crate::checkout_discard::undo(root, &saved.recovery_id, &saved.checksum)
            .await
            .unwrap();
        let preview = crate::checkout_discard::preview(root, &["new.txt".into()], true)
            .await
            .unwrap();
        let saved = crate::checkout_discard::discard(root, preview)
            .await
            .unwrap();
        assert!(!root.join("new.txt").exists());
        assert!(git(root, &["ls-files"]).is_empty());
        crate::checkout_discard::undo(root, &saved.recovery_id, &saved.checksum)
            .await
            .unwrap();
        assert_eq!(git(root, &["show", ":new.txt"]), "staged");
        assert_eq!(
            std::fs::read_to_string(root.join("new.txt")).unwrap(),
            "unstaged\n"
        );
    }
    #[tokio::test]
    async fn submodule_discard_and_linked_worktree_recovery_preserve_the_parent_index() {
        let parent = repo();
        let origin = repo();
        let root = parent.path();
        git(
            root,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                origin.path().to_str().unwrap(),
                "modules/child",
            ],
        );
        git(root, &["commit", "-qm", "Module"]);
        let child = checkout_changes::repository(root, "modules/child")
            .await
            .unwrap();
        std::fs::write(child.join("file.txt"), "child edit\n").unwrap();
        assert!(
            crate::checkout_discard::preview(root, &["modules/child".into()], false)
                .await
                .is_err()
        );
        let parent_index = git(root, &["ls-files", "--stage"]);
        let preview = crate::checkout_discard::preview(&child, &["file.txt".into()], false)
            .await
            .unwrap();
        let saved = crate::checkout_discard::discard(&child, preview)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(child.join("file.txt")).unwrap(),
            "original\n"
        );
        crate::checkout_discard::undo(&child, &saved.recovery_id, &saved.checksum)
            .await
            .unwrap();
        assert_eq!(git(root, &["ls-files", "--stage"]), parent_index);
        let other = tempfile::tempdir().unwrap();
        let linked = other.path().join("linked");
        git(
            root,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                linked.to_str().unwrap(),
            ],
        );
        std::fs::write(linked.join("file.txt"), "worktree edit\n").unwrap();
        let preview = crate::checkout_discard::preview(&linked, &["file.txt".into()], false)
            .await
            .unwrap();
        let saved = crate::checkout_discard::discard(&linked, preview)
            .await
            .unwrap();
        crate::checkout_discard::undo(&linked, &saved.recovery_id, &saved.checksum)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(linked.join("file.txt")).unwrap(),
            "worktree edit\n"
        );
        assert_eq!(git(root, &["ls-files", "--stage"]), parent_index);
    }
}
