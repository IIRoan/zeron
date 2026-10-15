//! Git operations scoped to the selected checkout or an initialized submodule.
use crate::{EngineError, checkout_changes, diff_sync::Capture};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeron_proto::{
    GitHistoryCommit, RepositoryGitAction as Action, RepositoryGitActionResult,
    RepositoryGitBranch, RepositoryGitDetails, RepositoryGitRemote, RepositoryGitStash,
    RepositoryGitState,
};

pub(crate) const MUTATION_TIMEOUT: Duration = Duration::from_secs(300);
pub(crate) const STREAM_TIMEOUT: Duration = Duration::from_secs(120);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const READ_LIMIT: usize = 3 * 1024 * 1024;
const RECORD_LIMIT: usize = 1024 * 1024;
const TERMINATION_GRACE: Duration = Duration::from_secs(5);
const UNDO_REF_PREFIX: &str = "refs/zeron/undo/";
const UNDO_REFS_KEPT: usize = 50;
const UNDO_REF_MAX_AGE: u64 = 90 * 24 * 60 * 60;

fn fail(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}

/// Every Git child of the source-control actions is started the same way.
fn command(root: &Path, args: &[&str]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true");
    // The checkout path must select the repository; inherited overrides would
    // redirect Git to another repository, object store or index.
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
    ] {
        command.env_remove(name);
    }
    #[cfg(unix)]
    // SAFETY: the hook only calls setsid, which is async-signal-safe.
    unsafe {
        // A new session has no controlling terminal, so ssh and other helpers
        // fail instead of prompting on the engine's TTY. Git also leads its
        // own process group, which lets a timeout signal its hooks too.
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.as_std_mut().creation_flags(0x08000000);
    }
    command
}

/// Stream a read-only command's stdout to `each` without buffering it.
/// Returns `false` when `each` stopped early, in which case Git is killed.
pub(crate) async fn stream(
    root: &Path,
    args: &[&str],
    limit: Duration,
    mut each: impl FnMut(&[u8]) -> Result<bool, EngineError>,
) -> Result<bool, EngineError> {
    let mut child = command(root, args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| fail("Git stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| fail("Git stderr unavailable"))?;
    let stderr = tokio::spawn(checkout_changes::commit_output(stderr));
    let work = async {
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let read = stdout.read(&mut buffer).await?;
            if read == 0 {
                return Ok::<_, EngineError>(Some(child.wait().await?));
            }
            if !each(&buffer[..read])? {
                return Ok(None);
            }
        }
    };
    let Some(status) = tokio::time::timeout(limit, work)
        .await
        .map_err(|_| fail("Reading Git metadata timed out"))??
    else {
        stderr.abort();
        return Ok(false);
    };
    if !status.success() {
        let stderr = tokio::time::timeout(TERMINATION_GRACE, stderr)
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(Result::ok)
            .unwrap_or_default();
        let message = String::from_utf8_lossy(&stderr).trim().to_owned();
        return Err(fail(if message.is_empty() {
            format!("git exited {status}")
        } else {
            format!("git: {message}")
        }));
    }
    Ok(true)
}

/// Feed each NUL-terminated record of a read-only command to `each`.
pub(crate) async fn records(
    root: &Path,
    args: &[&str],
    limit: Duration,
    mut each: impl FnMut(&[u8]) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let mut pending = Vec::new();
    stream(root, args, limit, |mut chunk| {
        while let Some(end) = chunk.iter().position(|b| *b == 0) {
            if pending.is_empty() {
                each(&chunk[..end])?;
            } else {
                pending.extend_from_slice(&chunk[..end]);
                each(&pending)?;
                pending.clear();
            }
            chunk = &chunk[end + 1..];
        }
        pending.extend_from_slice(chunk);
        if pending.len() > RECORD_LIMIT {
            return Err(fail("Git metadata record is too long"));
        }
        Ok(true)
    })
    .await?;
    if !pending.is_empty() {
        each(&pending)?;
    }
    Ok(())
}

/// Capture at most `max_bytes` of a read-only command's stdout.
pub(crate) async fn capture(
    root: &Path,
    args: &[&str],
    max_bytes: usize,
    limit: Duration,
) -> Result<Capture, EngineError> {
    let mut stdout = Vec::new();
    let complete = stream(root, args, limit, |chunk| {
        let room = max_bytes.saturating_sub(stdout.len());
        stdout.extend_from_slice(&chunk[..chunk.len().min(room)]);
        Ok(chunk.len() <= room)
    })
    .await?;
    Ok(Capture {
        stdout,
        truncated: !complete,
    })
}

async fn read(root: &Path, args: &[&str]) -> Result<Vec<u8>, EngineError> {
    let capture = capture(root, args, READ_LIMIT, READ_TIMEOUT).await?;
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

struct Finished {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    write: std::io::Result<()>,
}

/// Run a mutating command in its own task: a dropped request must not kill
/// Git while it holds `index.lock`, a ref lock or a half-applied operation.
/// Returns `None` when Git had to be stopped after `limit`.
async fn execute(
    root: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    limit: Duration,
) -> Result<Option<Finished>, EngineError> {
    let mut child = command(root, args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdin = child.stdin.take();
    let input = input.map(<[u8]>::to_vec);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| fail("Git stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| fail("Git stderr unavailable"))?;
    let task = tokio::spawn(async move {
        let write = tokio::spawn(async move {
            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                stdin.write_all(&input).await?;
            }
            Ok::<_, std::io::Error>(())
        });
        let stdout = tokio::spawn(checkout_changes::commit_output(stdout));
        let stderr = tokio::spawn(checkout_changes::commit_output(stderr));
        let aborts = [
            write.abort_handle(),
            stdout.abort_handle(),
            stderr.abort_handle(),
        ];
        let status = match tokio::time::timeout(limit, child.wait()).await {
            Ok(status) => status,
            Err(_) => {
                terminate(&mut child).await;
                aborts.iter().for_each(tokio::task::AbortHandle::abort);
                return Ok(None);
            }
        }?;
        // A detached grandchild may keep the pipes open after Git exits.
        let collected = tokio::time::timeout(TERMINATION_GRACE, async {
            (write.await, stdout.await, stderr.await)
        })
        .await;
        aborts.iter().for_each(tokio::task::AbortHandle::abort);
        let (write, stdout, stderr) =
            collected.unwrap_or_else(|_| (Ok(Ok(())), Ok(Ok(Vec::new())), Ok(Ok(Vec::new()))));
        let joined = |result: Result<std::io::Result<Vec<u8>>, tokio::task::JoinError>| {
            result.ok().and_then(Result::ok).unwrap_or_default()
        };
        Ok::<_, std::io::Error>(Some(Finished {
            status,
            stdout: joined(stdout),
            stderr: joined(stderr),
            write: write.unwrap_or_else(|error| Err(std::io::Error::other(error))),
        }))
    });
    task.await
        .map_err(|error| fail(format!("Git task failed: {error}")))?
        .map_err(Into::into)
}

/// Git removes its lock files on SIGTERM; SIGKILL would leave `index.lock`
/// behind and make every later Git command in the repository fail.
async fn terminate(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(group) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        // SAFETY: `group` is the unreaped child's process group, created by setsid.
        unsafe { libc::kill(-group, libc::SIGTERM) };
        let deadline = tokio::time::Instant::now() + TERMINATION_GRACE;
        let reaped = matches!(
            tokio::time::timeout_at(deadline, child.wait()).await,
            Ok(Ok(_))
        );
        // Hooks can outlive Git and ignore SIGTERM. Give those helpers the
        // remaining grace period, then stop the still-existing private group.
        while unsafe { libc::kill(-group, 0) } == 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if unsafe { libc::kill(-group, 0) } == 0 {
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        if reaped {
            return;
        }
    }
    let _ = child.kill().await;
}

/// Run a mutating Git command and return its stdout. `label` names the
/// operation in timeout and failure messages.
pub(crate) async fn mutate(
    root: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    limit: Duration,
    label: &str,
) -> Result<Vec<u8>, EngineError> {
    let Some(finished) = execute(root, args, input, limit).await? else {
        return Err(fail(format!(
            "{label} timed out and was stopped; refresh the repository before retrying"
        )));
    };
    if !finished.status.success() {
        let diagnostic = String::from_utf8_lossy(if finished.stderr.is_empty() {
            &finished.stdout
        } else {
            &finished.stderr
        })
        .trim()
        .to_owned();
        return Err(fail(if diagnostic.is_empty() {
            format!("{label} failed ({})", finished.status)
        } else {
            diagnostic
        }));
    }
    finished.write?;
    Ok(finished.stdout)
}

/// Incremental porcelain v2 parser: status output grows with the number of
/// changed files, so it is never buffered or capped.
#[derive(Default)]
struct StatusScan {
    state: RepositoryGitState,
    staged: bool,
    original_path_follows: bool,
}

impl StatusScan {
    fn record(&mut self, record: &[u8]) -> Result<(), EngineError> {
        // With -z, a rename or copy entry is followed by its original path as
        // a separate record, which can look like any other record type.
        if std::mem::take(&mut self.original_path_follows) {
            return Ok(());
        }
        let state = &mut self.state;
        match record {
            [b'#', b' ', header @ ..] => {
                let line =
                    std::str::from_utf8(header).map_err(|_| fail("Git status is not UTF-8"))?;
                if let Some(oid) = line.strip_prefix("branch.oid ") {
                    if oid != "(initial)" {
                        state.head = Some(oid.into());
                    }
                } else if let Some(branch) = line.strip_prefix("branch.head ") {
                    if branch != "(detached)" {
                        state.branch = Some(branch.into());
                    }
                } else if let Some(upstream) = line.strip_prefix("branch.upstream ") {
                    state.upstream = Some(upstream.into());
                } else if let Some(counts) = line.strip_prefix("branch.ab ") {
                    let mut parts = counts.split_whitespace();
                    state.ahead = parts
                        .next()
                        .and_then(|s| s.strip_prefix('+'))
                        .and_then(|s| s.parse().ok());
                    state.behind = parts
                        .next()
                        .and_then(|s| s.strip_prefix('-'))
                        .and_then(|s| s.parse().ok());
                }
            }
            [kind @ (b'1' | b'2'), b' ', index, ..] => {
                self.staged |= *index != b'.';
                self.original_path_follows = *kind == b'2';
            }
            [b'u', b' ', ..] => state.conflicts = state.conflicts.saturating_add(1),
            _ => {}
        }
        Ok(())
    }
}

pub(crate) struct Summary {
    pub(crate) state: RepositoryGitState,
    /// Whether the index differs from HEAD (or holds entries on an unborn branch).
    pub(crate) staged: bool,
}

/// Branch, conflict and staged state without listing untracked files.
/// `submodules` is Git's `--ignore-submodules` mode.
pub(crate) async fn summary(root: &Path, submodules: &str) -> Result<Summary, EngineError> {
    let ignore = format!("--ignore-submodules={submodules}");
    let mut scan = StatusScan::default();
    records(
        root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=no",
            &ignore,
        ],
        STREAM_TIMEOUT,
        |record| scan.record(record),
    )
    .await?;
    let mut state = scan.state;
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
    Ok(Summary {
        state,
        staged: scan.staged,
    })
}

/// Read-only status never contacts a remote. Fetch explicitly to update counts.
pub async fn state(root: &Path) -> Result<RepositoryGitState, EngineError> {
    Ok(summary(root, "all").await?.state)
}

/// `git am` shares `rebase-apply` with the apply rebase backend; the UI shows
/// both as a rebase, but they must be continued with their own command.
async fn applying_mailbox(root: &Path) -> Result<bool, EngineError> {
    Ok(git_dir(root)
        .await?
        .join("rebase-apply")
        .join("applying")
        .exists())
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
    for row in fields.as_chunks::<6>().0 {
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

/// Load only stash metadata; pickers and stash actions do not need branch or history scans.
pub async fn stashes(root: &Path) -> Result<Vec<RepositoryGitStash>, EngineError> {
    let mut stashes = Vec::new();
    for line in text(root, &["stash", "list", "--format=%H%x00%gd%x00%s"])
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
    Ok(stashes)
}

/// Branch-only metadata for quick pickers, without stash/history/remote scans.
pub async fn branches(root: &Path) -> Result<Vec<RepositoryGitBranch>, EngineError> {
    // HEAD's reflog is worktree-local and includes checkouts made outside Zeron.
    // Missing reflogs (unborn repositories or core.logAllRefUpdates=false) are fine.
    let reflog = text(
        root,
        &[
            "reflog",
            "show",
            "-2048",
            "--format=%gs%x00%gD",
            "--date=unix",
            "HEAD",
        ],
    )
    .await
    .unwrap_or_default();
    let mut used = std::collections::HashMap::new();
    for line in reflog.lines() {
        let Some((message, selector)) = line.split_once('\0') else {
            continue;
        };
        let Some(timestamp) = selector
            .strip_prefix("HEAD@{")
            .and_then(|s| s.strip_suffix('}'))
            .and_then(|s| s.parse::<u64>().ok())
        else {
            continue;
        };
        let Some((from, to)) = message
            .strip_prefix("checkout: moving from ")
            .and_then(|s| s.split_once(" to "))
        else {
            continue;
        };
        for name in [from, to] {
            used.entry(name.to_owned())
                .and_modify(|value: &mut u64| *value = (*value).max(timestamp))
                .or_insert(timestamp);
        }
    }
    let refs = text(
        root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)%00%(symref)%00%(HEAD)%00%(committerdate:unix)",
            "refs/heads",
            "refs/remotes",
        ],
    )
    .await?;
    let mut branches = Vec::new();
    for line in refs.lines() {
        let fields: Vec<_> = line.split('\0').collect();
        if fields.len() != 4 || !fields[1].is_empty() {
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
            last_used_at: if remote {
                None
            } else {
                used.get(name).copied()
            },
            commit_time: fields[3].parse().ok(),
        });
    }
    branches.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then_with(|| b.last_used_at.cmp(&a.last_used_at))
            .then_with(|| b.commit_time.cmp(&a.commit_time))
            .then_with(|| a.remote.cmp(&b.remote))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(branches)
}

pub async fn details(root: &Path) -> Result<RepositoryGitDetails, EngineError> {
    let state = state(root).await?;
    let branches = branches(root).await?;
    let mut remotes = Vec::new();
    for name in text(root, &["remote"]).await?.lines() {
        // A remote without a usable URL must not hide every other remote.
        remotes.push(RepositoryGitRemote {
            name: name.into(),
            url: text(root, &["remote", "get-url", "--", name])
                .await
                .map(|url| redact_url(&url))
                .unwrap_or_default(),
            push_url: text(root, &["remote", "get-url", "--push", "--", name])
                .await
                .ok()
                .map(|url| redact_url(&url)),
        });
    }
    let stashes = stashes(root).await?;
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
    let publish_remote = publication_remote(root, &remotes, state.branch.as_deref()).await?;
    Ok(RepositoryGitDetails {
        state,
        branches,
        remotes,
        publish_remote,
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
    let stdout = mutate(root, args, input, MUTATION_TIMEOUT, "Git operation").await?;
    Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
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
    // Pickers send a full ref to distinguish a local branch named origin/main
    // from the remote branch origin/main. Legacy callers still send short names.
    let (name, namespace) = if let Some(name) = name.strip_prefix("refs/heads/") {
        (name, Some(false))
    } else if let Some(name) = name.strip_prefix("refs/remotes/") {
        (name, Some(true))
    } else {
        (name, None)
    };
    let local = format!("refs/heads/{name}");
    let remote = format!("refs/remotes/{name}");
    let refs = text(
        root,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(symref)%00%(HEAD)",
            &local,
            &remote,
        ],
    )
    .await?;
    let mut found = None;
    for line in refs.lines() {
        let fields: Vec<_> = line.split('\0').collect();
        // Patterns also match nested refs and globs; only an exact,
        // non-symbolic branch counts. A local branch wins, as in the list.
        if fields.len() != 3 || !fields[1].is_empty() {
            continue;
        }
        let is_remote = if fields[0] == local {
            false
        } else if fields[0] == remote {
            true
        } else {
            continue;
        };
        if namespace.is_some_and(|remote| remote != is_remote) {
            continue;
        }
        found = Some(RepositoryGitBranch {
            name: name.into(),
            remote: is_remote,
            current: fields[2] == "*",
            last_used_at: None,
            commit_time: None,
        });
        if !is_remote {
            break;
        }
    }
    found.ok_or_else(|| fail("Branch is unavailable; refresh the branch list"))
}

/// The full ref keeps a same-named tag from shadowing the selected branch.
fn branch_ref(branch: &RepositoryGitBranch) -> String {
    let namespace = if branch.remote { "remotes" } else { "heads" };
    format!("refs/{namespace}/{}", branch.name)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// Keep recent undo refs (`<prefix><unix seconds>/<uuid>`) and a bounded
/// number overall. Older names without a timestamp are pruned first.
async fn prune_undo_refs(root: &Path) -> Result<(), EngineError> {
    let refs = text(
        root,
        &["for-each-ref", "--format=%(refname)", UNDO_REF_PREFIX],
    )
    .await?;
    let mut refs: Vec<_> = refs
        .lines()
        .filter_map(|name| {
            let created = name
                .strip_prefix(UNDO_REF_PREFIX)?
                .split_once('/')
                .and_then(|(created, _)| created.parse::<u64>().ok());
            Some((created, name))
        })
        .collect();
    refs.sort_by(|a, b| b.cmp(a));
    let now = unix_now();
    let stale: String = refs
        .iter()
        .enumerate()
        .filter(|(i, (created, _))| {
            *i >= UNDO_REFS_KEPT
                || created.is_some_and(|created| now.saturating_sub(created) > UNDO_REF_MAX_AGE)
        })
        .map(|(_, (_, name))| format!("delete {name}\n"))
        .collect();
    if !stale.is_empty() {
        run(root, &["update-ref", "--stdin"], Some(stale.as_bytes())).await?;
    }
    Ok(())
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
        let metadata = details(root).await?;
        let remote = metadata
            .publish_remote
            .ok_or_else(|| fail("Select a remote to publish this branch"))?;
        return publish(root, &remote, &branch).await;
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

/// Honor explicit Git push destinations, then prefer origin, then a single
/// enabled remote. Ambiguous destinations require a choice in the UI.
async fn publication_remote(
    root: &Path,
    remotes: &[RepositoryGitRemote],
    branch: Option<&str>,
) -> Result<Option<String>, EngineError> {
    let eligible = remotes
        .iter()
        .filter(|remote| remote.push_url.as_deref() != Some("DISABLED"))
        .map(|remote| remote.name.as_str())
        .collect::<Vec<_>>();
    let mut keys = Vec::new();
    if let Some(branch) = branch {
        keys.push(format!("branch.{branch}.pushRemote"));
    }
    keys.push("remote.pushDefault".into());
    if let Some(branch) = branch {
        keys.push(format!("branch.{branch}.remote"));
    }
    for key in keys {
        let configured = text(root, &["config", "--default", "", "--get", &key]).await?;
        if !configured.is_empty() {
            return Ok(eligible
                .contains(&configured.as_str())
                .then_some(configured));
        }
    }
    Ok(if eligible.contains(&"origin") {
        Some("origin".into())
    } else if eligible.len() == 1 {
        Some(eligible[0].into())
    } else {
        None
    })
}

async fn publish(root: &Path, remote: &str, branch: &str) -> Result<(), EngineError> {
    known_remote(root, remote).await?;
    run(
        root,
        &[
            "push",
            "--set-upstream",
            "--",
            remote,
            &format!("HEAD:refs/heads/{branch}"),
        ],
        None,
    )
    .await
    .map(drop)
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
    stashes(root)
        .await?
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
        Action::PullFrom { branch } => {
            let selected = known_branch(root, &branch).await?;
            if !selected.remote {
                return Err(fail("Select a remote branch to pull from"));
            }
            if before.branch.is_none() {
                return Err(fail("Switch to a branch before pulling"));
            }
            // Remote names can contain slashes. Match the longest registered
            // prefix instead of guessing the remote from the first slash.
            let remotes = text(root, &["remote"]).await?;
            let (remote, name) = remotes
                .lines()
                .filter_map(|remote| {
                    selected
                        .name
                        .strip_prefix(remote)
                        .and_then(|s| s.strip_prefix('/'))
                        .map(|name| (remote, name))
                })
                .max_by_key(|(remote, _)| remote.len())
                .ok_or_else(|| fail("The branch's remote is unavailable; fetch and try again"))?;
            let reference = format!("refs/heads/{name}");
            run(
                root,
                &["pull", "--no-rebase", "--no-edit", "--", remote, &reference],
                None,
            )
            .await?;
            "Selected remote branch pulled into the current branch"
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
            publish(root, &remote, &branch).await?;
            "Branch published"
        }
        Action::Sync => {
            pull(root, None).await?;
            push(root).await?;
            "Incoming and outgoing commits synchronized"
        }
        Action::SwitchBranch { branch } => {
            let selected = known_branch(root, &branch).await?;
            let reference = branch_ref(&selected);
            let branch = selected.name;
            if selected.remote {
                let local = branch
                    .split_once('/')
                    .map(|(_, name)| name)
                    .unwrap_or(&branch);
                let tracking = text(
                    root,
                    &[
                        "for-each-ref",
                        "--format=%(refname:strip=2)%00%(upstream)",
                        "refs/heads",
                    ],
                )
                .await?;
                let tracking = tracking
                    .lines()
                    .filter_map(|line| {
                        let (name, upstream) = line.split_once('\0')?;
                        (upstream == reference).then_some(name)
                    })
                    .min_by_key(|&name| (name != local, name));
                if let Some(tracking) = tracking {
                    run(root, &["switch", "--", tracking], None).await?;
                } else {
                    run(root, &["switch", "--track", "--", &reference], None).await?;
                }
            } else {
                run(root, &["switch", "--", &branch], None).await?;
            }
            "Branch switched"
        }
        Action::CreateBranch { name, start } => {
            branch_name(root, &name).await?;
            let mut args = vec!["switch", "--create", &name];
            let start = match &start {
                Some(start) => Some(branch_ref(&known_branch(root, start).await?)),
                None => None,
            };
            if let Some(start) = &start {
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
            run(root, &["branch", "--delete", "--", &selected.name], None).await?;
            "Merged branch deleted"
        }
        Action::Merge { branch } => {
            let reference = branch_ref(&known_branch(root, &branch).await?);
            run(root, &["merge", "--no-edit", "--", &reference], None).await?;
            "Branch merged"
        }
        Action::Rebase { branch } => {
            let reference = branch_ref(&known_branch(root, &branch).await?);
            run(root, &["rebase", "--", &reference], None).await?;
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
            let head = before
                .head
                .as_deref()
                .ok_or_else(|| fail("There is no commit to undo"))?;
            // Read the actual commit headers: rev-list hides parents at a
            // shallow boundary, which must never be mistaken for a root commit.
            let object = text(root, &["cat-file", "-p", head]).await?;
            let parent = object.lines().take_while(|line| !line.is_empty())
                .find_map(|line| line.strip_prefix("parent "));
            if let Some(parent) = parent { oid(parent)?; }
            commit_message = Some(text(root, &["log", "-1", "--format=%B", head, "--"]).await?);
            run(
                root,
                &[
                    "update-ref",
                    &format!("{UNDO_REF_PREFIX}{}/{}", unix_now(), uuid::Uuid::new_v4()),
                    head,
                ],
                None,
            )
            .await?;
            // Retention is best effort; the new recovery ref is already safe.
            let _ = prune_undo_refs(root).await;
            if let Some(parent) = parent {
                run(root, &["reset", "--soft", parent], None).await?;
                "Last commit undone; its changes remain staged"
            } else {
                let branch = before.branch.as_deref().ok_or_else(|| fail("Switch to a branch before undoing an initial commit"))?;
                // Match VS Code's root-commit undo: keep all files, make the
                // branch unborn again and empty the index. Preserve a recovery ref.
                run(root, &["update-ref", "-d", &format!("refs/heads/{branch}"), head], None).await?;
                run(root, &["read-tree", "--empty"], None).await?;
                "Initial commit undone; its files remain in the working tree"
            }
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
            // Git's "no local changes" notice is localized; compare the stash instead.
            let latest = |stashes: Vec<RepositoryGitStash>| {
                (
                    stashes.len(),
                    stashes.into_iter().next().map(|stash| stash.sha),
                )
            };
            let previous = latest(stashes(root).await?);
            run(root, &args, None).await?;
            if latest(stashes(root).await?) == previous {
                "No local changes to stash"
            } else {
                "Changes stashed"
            }
        }
        Action::ApplyStash { sha, reinstate_staged } => {
            stash_entry(root, &sha).await?;
            let mut args = vec!["stash", "apply"];
            if reinstate_staged { args.push("--index"); }
            args.push(&sha);
            run(root, &args, None).await?;
            "Stash applied and kept"
        }
        Action::PopStash { sha, reinstate_staged } => {
            stash_entry(root, &sha).await?;
            let mut args = vec!["stash", "apply"];
            if reinstate_staged { args.push("--index"); }
            args.push(&sha);
            run(root, &args, None).await?;
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
            run(root, &["remote", "remove", "--", &name], None).await?;
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
                Some("rebase") if applying_mailbox(root).await? => ["am", "--continue"],
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
                Some("rebase") if applying_mailbox(root).await? => ["am", "--abort"],
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
            let stages = text(
                root,
                &[
                    "--literal-pathspecs",
                    "ls-files",
                    "--unmerged",
                    "-z",
                    "--",
                    &path,
                ],
            )
            .await?;
            // The pathspec also matches entries below a deleted directory.
            let stages: Vec<_> = stages
                .split('\0')
                .filter_map(|line| line.split_once('\t'))
                .filter(|(_, entry)| *entry == path)
                .map(|(header, _)| header)
                .collect();
            if stages.is_empty() {
                return Err(fail("This file is no longer an unresolved conflict"));
            }
            let desired = if incoming { "3" } else { "2" };
            let has_version = stages
                .iter()
                .any(|header| header.split_whitespace().nth(2) == Some(desired));
            let payload = format!("{path}\0");
            if !has_version {
                run(root, &["--literal-pathspecs", "rm", "--force", "--pathspec-from-file=-", "--pathspec-file-nul"], Some(payload.as_bytes())).await?;
                return Ok(RepositoryGitActionResult { notice: "Conflict resolved by accepting the deleted version".into(), commit_message: None });
            }
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
    async fn checkout_branch_picker_reads_all_refs_and_recent_checkouts_of_old_tips() {
        let dir = repo();
        let root = dir.path();
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "commit",
                "--amend",
                "--no-edit",
                "--date=2000-01-01T00:00:00Z",
            ])
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
            .output()
            .unwrap();
        assert!(output.status.success());
        // More than the old 512-ref cap, written in one process.
        let head = git(root, &["rev-parse", "HEAD"]);
        let mut child = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["update-ref", "--stdin"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            let mut input = child.stdin.take().unwrap();
            for i in 0..600 {
                writeln!(input, "create refs/heads/old/{i:04} {head}").unwrap();
            }
        }
        assert!(child.wait().unwrap().success());
        git(root, &["switch", "-c", "recently-used"]);
        git(root, &["switch", "main"]);
        git(root, &["update-ref", "refs/remotes/origin/main", &head]);
        git(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let entries = branches(root).await.unwrap();
        assert_eq!(entries.len(), 603);
        assert!(entries[0].current);
        assert_eq!(entries[1].name, "recently-used");
        assert!(entries[1].last_used_at.unwrap() >= unix_now() - 60);
        assert!(entries[1].commit_time.unwrap() < 1_000_000_000);
        let old = entries
            .iter()
            .find(|entry| entry.name == "old/0599")
            .unwrap();
        assert_eq!(old.last_used_at, None);
        assert!(old.commit_time.unwrap() < 1_000_000_000);
        assert!(!entries.iter().any(|entry| entry.name == "origin/HEAD"));
        let unborn = tempfile::tempdir().unwrap();
        git(unborn.path(), &["init", "-q", "-b", "main"]);
        assert!(branches(unborn.path()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn checkout_pull_from_fetches_selected_remote_branch_without_changing_checkout_or_upstream()
     {
        let dir = repo();
        let root = dir.path();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
        git(
            root,
            &[
                "remote",
                "add",
                "team/origin",
                remote.path().to_str().unwrap(),
            ],
        );
        git(root, &["push", "-qu", "team/origin", "main"]);
        let source = tempfile::tempdir().unwrap();
        git(
            source.path(),
            &["clone", "-q", remote.path().to_str().unwrap(), "."],
        );
        git(source.path(), &["config", "user.name", "Fixture"]);
        git(
            source.path(),
            &["config", "user.email", "fixture@example.com"],
        );
        git(source.path(), &["config", "commit.gpgsign", "false"]);
        git(source.path(), &["switch", "-c", "feature/remote"]);
        git(source.path(), &["push", "-q", "origin", "HEAD"]);
        git(root, &["fetch", "-q", "team/origin"]);
        // A same-named local branch must never shadow the selected remote ref.
        git(root, &["branch", "team/origin/feature/remote"]);
        assert!(
            act(
                root,
                Action::PullFrom {
                    branch: "team/origin/feature/remote".into()
                }
            )
            .await
            .is_err()
        );
        std::fs::write(source.path().join("incoming.txt"), "selected branch\n").unwrap();
        git(source.path(), &["add", "incoming.txt"]);
        git(source.path(), &["commit", "-qm", "Incoming branch change"]);
        git(source.path(), &["push", "-q", "origin", "HEAD"]);
        std::fs::write(root.join("local.txt"), "local branch commit\n").unwrap();
        git(root, &["add", "local.txt"]);
        git(root, &["commit", "-qm", "Local branch change"]);
        let local_head = git(root, &["rev-parse", "HEAD"]);
        git(root, &["config", "pull.rebase", "true"]);
        let upstream = state(root).await.unwrap().upstream;
        act(
            root,
            Action::PullFrom {
                branch: "refs/remotes/team/origin/feature/remote".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(git(root, &["branch", "--show-current"]), "main");
        assert_eq!(state(root).await.unwrap().upstream, upstream);
        assert_eq!(git(root, &["rev-parse", "HEAD^1"]), local_head);
        assert_eq!(
            git(root, &["rev-parse", "HEAD^2"]),
            git(source.path(), &["rev-parse", "HEAD"])
        );
        assert_eq!(
            std::fs::read_to_string(root.join("incoming.txt")).unwrap(),
            "selected branch\n"
        );
        // This operation also works on a branch without a configured upstream.
        git(root, &["switch", "-c", "unpublished"]);
        act(
            root,
            Action::PullFrom {
                branch: "refs/remotes/team/origin/feature/remote".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(state(root).await.unwrap().upstream, None);
        assert!(
            act(
                root,
                Action::PullFrom {
                    branch: "refs/heads/main".into()
                }
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn checkout_full_remote_ref_switches_to_tracking_branch_and_preserves_local_work() {
        let dir = repo();
        let root = dir.path();
        git(root, &["remote", "add", "origin", "/unused-local-remote"]);
        git(root, &["update-ref", "refs/remotes/origin/topic", "HEAD"]);
        git(root, &["branch", "origin/topic"]);
        std::fs::write(root.join("file.txt"), "local work\n").unwrap();
        act(
            root,
            Action::SwitchBranch {
                branch: "refs/remotes/origin/topic".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(git(root, &["branch", "--show-current"]), "topic");
        assert_eq!(git(root, &["rev-parse", "--symbolic-full-name", "@{upstream}"]), "refs/remotes/origin/topic");
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "local work\n"
        );
        git(root, &["branch", "--move", "custom-topic"]);
        act(
            root,
            Action::SwitchBranch {
                branch: "refs/heads/origin/topic".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(git(root, &["branch", "--show-current"]), "origin/topic");
        act(
            root,
            Action::SwitchBranch {
                branch: "refs/remotes/origin/topic".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(git(root, &["branch", "--show-current"]), "custom-topic");
        act(
            root,
            Action::DeleteBranch {
                name: "refs/heads/origin/topic".into(),
            },
        )
        .await
        .unwrap();
        assert!(known_branch(root, "refs/heads/origin/topic").await.is_err());
    }

    #[test]
    fn status_scan_skips_the_original_path_of_renames_and_copies() {
        let mut scan = StatusScan::default();
        let output: &[u8] = b"# branch.oid 0123\0# branch.head main\0\
            2 R. N... 100644 100644 100644 a a R100 renamed\0u weird\0\
            2 .C N... 100644 100644 100644 a a C100 copy\0# branch.head other\0\
            u UU N... 100644 100644 100644 100644 a b c conflicted\0";
        for record in output.split(|b| *b == 0) {
            scan.record(record).unwrap();
        }
        assert_eq!(scan.state.conflicts, 1);
        assert_eq!(scan.state.branch.as_deref(), Some("main"));
        assert!(scan.staged);
        let mut unstaged = StatusScan::default();
        unstaged
            .record(b"1 .M N... 100644 100644 100644 a a file")
            .unwrap();
        assert!(!unstaged.staged);
    }

    #[tokio::test]
    async fn renaming_a_file_named_like_a_conflict_record_is_not_a_conflict() {
        let dir = repo();
        let root = dir.path();
        std::fs::write(root.join("u weird"), "content\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "Weird"]);
        git(root, &["mv", "u weird", "renamed"]);
        assert_eq!(state(root).await.unwrap().conflicts, 0);
        checkout_changes::commit_staged(root, "Rename")
            .await
            .unwrap();
        assert_eq!(git(root, &["ls-files"]), "file.txt\nrenamed");
    }

    #[tokio::test]
    async fn merge_and_rebase_choose_the_branch_over_a_same_named_tag() {
        for rebase in [false, true] {
            let dir = repo();
            let root = dir.path();
            git(root, &["tag", "other"]);
            git(root, &["switch", "-qc", "other"]);
            std::fs::write(root.join("other.txt"), "branch\n").unwrap();
            git(root, &["add", "-A"]);
            git(root, &["commit", "-qm", "Branch work"]);
            let branch = git(root, &["rev-parse", "refs/heads/other"]);
            git(root, &["switch", "-q", "main"]);
            std::fs::write(root.join("main.txt"), "main\n").unwrap();
            git(root, &["add", "-A"]);
            git(root, &["commit", "-qm", "Main work"]);
            let action = if rebase {
                Action::Rebase {
                    branch: "other".into(),
                }
            } else {
                Action::Merge {
                    branch: "other".into(),
                }
            };
            act(root, action).await.unwrap();
            git(root, &["merge-base", "--is-ancestor", &branch, "HEAD"]);
            assert!(root.join("other.txt").exists());
        }
    }

    #[tokio::test]
    async fn stash_notice_compares_the_stash_instead_of_git_output() {
        let dir = repo();
        let root = dir.path();
        let stash = |message: &str| Action::Stash {
            message: message.into(),
            include_untracked: false,
        };
        let clean = act(root, stash("Nothing")).await.unwrap();
        assert_eq!(clean.notice, "No local changes to stash");
        std::fs::write(root.join("file.txt"), "first\n").unwrap();
        assert_eq!(
            act(root, stash("First")).await.unwrap().notice,
            "Changes stashed"
        );
        assert_eq!(
            act(root, stash("Still nothing")).await.unwrap().notice,
            "No local changes to stash"
        );
        assert_eq!(details(root).await.unwrap().stashes.len(), 1);
    }

    #[tokio::test]
    async fn remotes_with_option_like_or_missing_urls_do_not_break_details() {
        let dir = repo();
        let root = dir.path();
        git(root, &["config", "remote.-bad.url", "/tmp/nowhere"]);
        git(
            root,
            &[
                "config",
                "remote.nourl.fetch",
                "+refs/heads/*:refs/remotes/nourl/*",
            ],
        );
        let remotes = details(root).await.unwrap().remotes;
        let bad = remotes.iter().find(|remote| remote.name == "-bad").unwrap();
        assert_eq!(bad.url, "/tmp/nowhere");
        act(
            root,
            Action::RemoveRemote {
                name: "-bad".into(),
            },
        )
        .await
        .unwrap();
        assert!(
            details(root)
                .await
                .unwrap()
                .remotes
                .iter()
                .all(|remote| remote.name != "-bad")
        );
    }

    #[tokio::test]
    async fn undo_on_an_unborn_branch_explains_there_is_nothing_to_undo() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        let error = act(dir.path(), Action::UndoCommit).await.unwrap_err();
        assert_eq!(error.to_string(), "There is no commit to undo");
    }

    #[tokio::test]
    async fn undo_refs_are_bounded_and_keep_the_newest_recovery() {
        let dir = repo();
        let root = dir.path();
        let head = git(root, &["rev-parse", "HEAD"]);
        let legacy: String = (0..UNDO_REFS_KEPT + 5)
            .map(|_| format!("create {UNDO_REF_PREFIX}{} {head}\n", uuid::Uuid::new_v4()))
            .collect();
        let mut child = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["update-ref", "--stdin"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), legacy.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
        std::fs::write(root.join("file.txt"), "second\n").unwrap();
        git(root, &["commit", "-qam", "Second"]);
        let second = git(root, &["rev-parse", "HEAD"]);
        act(root, Action::UndoCommit).await.unwrap();
        let refs = git(
            root,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                UNDO_REF_PREFIX,
            ],
        );
        assert_eq!(refs.lines().count(), UNDO_REFS_KEPT);
        assert!(refs.lines().any(|line| line.ends_with(&second)));
    }

    #[tokio::test]
    async fn mailbox_application_continues_and_aborts_with_git_am() {
        let dir = repo();
        let root = dir.path();
        git(root, &["switch", "-qc", "patch"]);
        std::fs::write(root.join("file.txt"), "patched\n").unwrap();
        git(root, &["commit", "-qam", "Patch"]);
        let patch = git(root, &["format-patch", "-1", "--stdout"]);
        git(root, &["switch", "-q", "main"]);
        std::fs::write(root.join("file.txt"), "diverged\n").unwrap();
        git(root, &["commit", "-qam", "Diverged"]);
        let mailbox = root.join(".git").join("patch.mbox");
        std::fs::write(&mailbox, patch + "\n").unwrap();
        let applied = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["am", "-q", mailbox.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(!applied.status.success());
        assert_eq!(
            state(root).await.unwrap().operation.as_deref(),
            Some("rebase")
        );
        act(root, Action::Abort).await.unwrap();
        assert_eq!(state(root).await.unwrap().operation, None);
        assert!(!root.join(".git").join("rebase-apply").exists());
    }

    #[cfg(unix)]
    fn slow_filter(root: &Path, seconds: u32) {
        git(
            root,
            &[
                "config",
                "filter.slow.clean",
                &format!("sleep {seconds}; cat"),
            ],
        );
        std::fs::write(root.join(".gitattributes"), "*.slow filter=slow\n").unwrap();
        std::fs::write(root.join("file.slow"), "slow\n").unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_mutations_stop_git_without_leaving_the_index_locked() {
        let dir = repo();
        let root = dir.path();
        slow_filter(root, 60);
        let started = std::time::Instant::now();
        let error = mutate(
            root,
            &["add", "file.slow"],
            None,
            Duration::from_secs(1),
            "Staging",
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(30));
        assert!(!root.join(".git").join("index.lock").exists());
        git(root, &["config", "--unset", "filter.slow.clean"]);
        checkout_changes::set_staged(root, &["file.slow".into()], true)
            .await
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn checkout_git_timeout_stops_helpers_after_the_leader_exits() {
        let dir = tempfile::tempdir().unwrap();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.current_dir(dir.path()).args([
            "-c",
            "trap 'exit 0' TERM; /bin/sh -c 'trap \"\" TERM; echo $$ > helper.pid; while :; do sleep 1; done' & wait",
        ]);
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let mut child = command.spawn().unwrap();
        struct Cleanup(i32);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                unsafe { libc::kill(-self.0, libc::SIGKILL) };
            }
        }
        let cleanup = Cleanup(child.id().unwrap() as i32);
        let pid_file = dir.path().join("helper.pid");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !pid_file.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let helper: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        terminate(&mut child).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let status = std::fs::read_to_string(format!("/proc/{helper}/stat")).unwrap_or_default();
        assert!(
            status.is_empty() || status.split_once(") ").unwrap().1.starts_with('Z'),
            "a TERM-resistant Git helper survived after its leader exited: {status}"
        );
        drop(cleanup);
    }

    #[tokio::test]
    async fn checkout_discard_retention_includes_the_newest_recovery() {
        let dir = repo();
        let root = dir.path();
        let mut latest = None;
        for i in 0..18 {
            std::fs::write(root.join("file.txt"), format!("edit {i}\n")).unwrap();
            let preview = crate::checkout_discard::preview(root, &["file.txt".into()], false)
                .await
                .unwrap();
            latest = Some(
                crate::checkout_discard::discard(root, preview)
                    .await
                    .unwrap(),
            );
        }
        let directory = git_dir(root).await.unwrap().join("zeron-discard");
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 16);
        let latest = latest.unwrap();
        crate::checkout_discard::undo(root, &latest.recovery_id, &latest.checksum)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "edit 17\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_mutations_still_finish_and_release_the_index() {
        let dir = repo();
        let root = dir.path();
        slow_filter(root, 1);
        let dropped = tokio::time::timeout(
            Duration::from_millis(200),
            run(root, &["add", "file.slow"], None),
        )
        .await;
        assert!(dropped.is_err());
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while root.join(".git").join("index.lock").exists() && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!root.join(".git").join("index.lock").exists());
        assert!(git(root, &["ls-files"]).contains("file.slow"));
    }

    #[tokio::test]
    async fn discard_rejects_a_request_that_differs_from_its_preview() {
        let dir = repo();
        let root = dir.path();
        std::fs::write(root.join("file.txt"), "staged\n").unwrap();
        git(root, &["add", "file.txt"]);
        std::fs::write(root.join("file.txt"), "worktree\n").unwrap();
        std::fs::write(root.join("other.txt"), "untracked\n").unwrap();
        let preview = crate::checkout_discard::preview(root, &["file.txt".into()], false)
            .await
            .unwrap();
        let mut staged = preview.clone();
        staged.include_staged = true;
        assert!(
            crate::checkout_discard::discard(root, staged)
                .await
                .is_err()
        );
        let mut widened = preview.clone();
        widened.paths.push("other.txt".into());
        assert!(
            crate::checkout_discard::discard(root, widened)
                .await
                .is_err()
        );
        assert_eq!(git(root, &["show", ":file.txt"]), "staged");
        assert!(root.join("other.txt").exists());
        let saved = crate::checkout_discard::discard(root, preview)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "staged\n"
        );
        let backup = root
            .join(".git")
            .join("zeron-discard")
            .join(&saved.recovery_id);
        assert!(backup.exists());
        crate::checkout_discard::undo(root, &saved.recovery_id, &saved.checksum)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "worktree\n"
        );
        assert!(!backup.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn large_indexes_and_non_utf8_paths_do_not_hit_metadata_caps() {
        use std::os::unix::ffi::OsStrExt;
        let dir = repo();
        let root = dir.path();
        let component = "d".repeat(200);
        let nested = root.join([component.as_str(); 4].join("/"));
        std::fs::create_dir_all(&nested).unwrap();
        // Long paths push `ls-files --stage` past the 3 MiB metadata cap.
        for i in 0..4000 {
            std::fs::write(nested.join(i.to_string()), "x\n").unwrap();
        }
        std::fs::write(root.join(std::ffi::OsStr::from_bytes(b"bad-\xff")), "x\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "Large"]);
        assert!(git(root, &["ls-files", "--stage"]).len() > READ_LIMIT);
        assert!(
            checkout_changes::submodules(root)
                .await
                .unwrap()
                .0
                .is_empty()
        );
        std::fs::write(root.join("file.txt"), "changed\n").unwrap();
        let preview = crate::checkout_discard::preview(root, &["file.txt".into()], false)
            .await
            .unwrap();
        let saved = crate::checkout_discard::discard(root, preview)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "original\n"
        );
        crate::checkout_discard::undo(root, &saved.recovery_id, &saved.checksum)
            .await
            .unwrap();
        checkout_changes::set_staged(root, &["file.txt".into()], true)
            .await
            .unwrap();
        checkout_changes::commit_staged(root, "Change")
            .await
            .unwrap();
        assert_eq!(git(root, &["show", "HEAD:file.txt"]), "changed");
    }

    #[tokio::test]
    async fn diff_sync_capture_drains_large_stderr_while_reading_stdout() {
        let dir = repo();
        let capture = tokio::time::timeout(
            Duration::from_secs(20),
            crate::diff_sync::capture_git(
                dir.path(),
                &[
                    "-c",
                    "alias.noisy=!head -c 1000000 /dev/zero >&2; echo done",
                    "noisy",
                ],
                1024,
            ),
        )
        .await
        .expect("a full stderr pipe must not stall Git")
        .unwrap();
        assert_eq!(capture.stdout, b"done\n");
    }

    #[tokio::test]
    async fn stash_apply_defaults_to_unstaged_and_failed_pop_keeps_the_stash() {
        let dir = repo();
        let root = dir.path();
        std::fs::write(root.join("file.txt"), "saved\n").unwrap();
        git(root, &["add", "file.txt"]);
        act(root, Action::Stash { message: "Saved".into(), include_untracked: false }).await.unwrap();
        let sha = details(root).await.unwrap().stashes[0].sha.clone();
        act(root, Action::ApplyStash { sha: sha.clone(), reinstate_staged: false }).await.unwrap();
        assert_eq!(git(root, &["show", ":file.txt"]), "original");
        assert_eq!(std::fs::read_to_string(root.join("file.txt")).unwrap(), "saved\n");
        assert_eq!(details(root).await.unwrap().stashes.len(), 1);
        git(root, &["restore", "file.txt"]);
        std::fs::write(root.join("file.txt"), "diverged\n").unwrap();
        git(root, &["add", "file.txt"]);
        git(root, &["commit", "-qm", "Diverged"]);
        assert!(act(root, Action::PopStash { sha: sha.clone(), reinstate_staged: false }).await.is_err());
        assert_eq!(details(root).await.unwrap().stashes[0].sha, sha);
        assert_eq!(state(root).await.unwrap().conflicts, 1);
    }

    #[tokio::test]
    async fn undo_initial_commit_keeps_files_and_a_recovery_reference() {
        let dir = repo();
        let root = dir.path();
        let head = git(root, &["rev-parse", "HEAD"]);
        std::fs::write(root.join("file.txt"), "unsaved work\n").unwrap();
        git(root, &["add", "file.txt"]);
        let result = act(root, Action::UndoCommit).await.unwrap();
        assert_eq!(result.commit_message.as_deref().map(str::trim), Some("Initial"));
        assert_eq!(state(root).await.unwrap().head, None);
        assert_eq!(state(root).await.unwrap().branch.as_deref(), Some("main"));
        assert_eq!(git(root, &["ls-files"]), "");
        assert_eq!(std::fs::read_to_string(root.join("file.txt")).unwrap(), "unsaved work\n");
        assert!(git(root, &["for-each-ref", "--format=%(objectname)", "refs/zeron/undo"]).contains(&head));
    }

    #[tokio::test]
    async fn undo_refuses_a_missing_shallow_parent_without_deleting_the_branch() {
        let source = repo();
        std::fs::write(source.path().join("file.txt"), "second\n").unwrap();
        git(source.path(), &["add", "-A"]);
        git(source.path(), &["commit", "-qm", "Second"]);
        let clone = tempfile::tempdir().unwrap();
        git(clone.path(), &["clone", "-q", "--depth=1", &format!("file://{}", source.path().display()), "."]);
        git(clone.path(), &["remote", "remove", "origin"]);
        let head = git(clone.path(), &["rev-parse", "HEAD"]);
        assert!(act(clone.path(), Action::UndoCommit).await.is_err());
        assert_eq!(git(clone.path(), &["rev-parse", "HEAD"]), head);
        assert_eq!(git(clone.path(), &["branch", "--show-current"]), "main");
        assert_eq!(git(clone.path(), &["show", ":file.txt"]), "second");
    }

    #[tokio::test]
    async fn modify_delete_conflicts_can_accept_either_version() {
        for incoming in [false, true] {
            let dir = repo();
            let root = dir.path();
            git(root, &["switch", "-qc", "other"]);
            git(root, &["rm", "file.txt"]);
            git(root, &["commit", "-qm", "Delete"]);
            git(root, &["switch", "-q", "main"]);
            std::fs::write(root.join("file.txt"), "modified\n").unwrap();
            git(root, &["add", "-A"]);
            git(root, &["commit", "-qm", "Modify"]);
            assert!(act(root, Action::Merge { branch: "other".into() }).await.is_err());
            act(root, Action::ResolveConflict { path: "file.txt".into(), incoming }).await.unwrap();
            if incoming {
                assert!(!root.join("file.txt").exists());
            } else {
                assert_eq!(std::fs::read_to_string(root.join("file.txt")).unwrap(), "modified\n");
                checkout_changes::set_staged(root, &["file.txt".into()], true).await.unwrap();
            }
            assert_eq!(state(root).await.unwrap().conflicts, 0);
            act(root, Action::Continue).await.unwrap();
            assert!(state(root).await.unwrap().operation.is_none());
        }
    }
    #[tokio::test]
    async fn push_publishes_new_branches_and_respects_the_configured_destination() {
        let project = repo();
        let root = project.path();
        let origin = tempfile::tempdir().unwrap();
        let fork = tempfile::tempdir().unwrap();
        for remote in [&origin, &fork] {
            git(remote.path(), &["init", "-q", "--bare"]);
        }
        git(
            root,
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git(
            root,
            &["remote", "add", "fork", fork.path().to_str().unwrap()],
        );
        git(
            root,
            &["remote", "add", "upstream", origin.path().to_str().unwrap()],
        );
        git(
            root,
            &["remote", "set-url", "--push", "upstream", "DISABLED"],
        );
        git(root, &["checkout", "-qb", "feature/first"]);
        std::fs::write(root.join("file.txt"), "first feature commit\n").unwrap();
        git(root, &["add", "file.txt"]);
        git(root, &["commit", "-qm", "Feature"]);
        assert_eq!(
            details(root).await.unwrap().publish_remote.as_deref(),
            Some("origin")
        );
        act(root, Action::Push).await.unwrap();
        assert_eq!(
            state(root).await.unwrap().upstream.as_deref(),
            Some("origin/feature/first")
        );
        assert_eq!(
            git(origin.path(), &["rev-parse", "refs/heads/feature/first"]),
            git(root, &["rev-parse", "HEAD"])
        );
        git(root, &["checkout", "-qb", "feature/second"]);
        git(
            root,
            &["config", "branch.feature/second.pushRemote", "fork"],
        );
        assert_eq!(
            details(root).await.unwrap().publish_remote.as_deref(),
            Some("fork")
        );
        act(root, Action::Push).await.unwrap();
        assert_eq!(
            state(root).await.unwrap().upstream.as_deref(),
            Some("fork/feature/second")
        );
        assert_eq!(
            git(fork.path(), &["rev-parse", "refs/heads/feature/second"]),
            git(root, &["rev-parse", "HEAD"])
        );
        git(root, &["checkout", "-qb", "feature/disabled"]);
        git(root, &["config", "remote.pushDefault", "upstream"]);
        assert_eq!(details(root).await.unwrap().publish_remote, None);
        assert!(act(root, Action::Push).await.is_err());
        assert!(state(root).await.unwrap().upstream.is_none());
    }

    #[tokio::test]
    async fn ambiguous_publication_requires_a_remote_choice_before_pushing() {
        let project = repo();
        let root = project.path();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "-q", "--bare"]);
        for name in ["personal", "company"] {
            git(
                root,
                &["remote", "add", name, remote.path().to_str().unwrap()],
            );
        }
        assert_eq!(details(root).await.unwrap().publish_remote, None);
        assert!(act(root, Action::Push).await.is_err());
        assert!(state(root).await.unwrap().upstream.is_none());
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
        act(root, Action::PopStash { sha: saved, reinstate_staged: true }).await.unwrap();
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
