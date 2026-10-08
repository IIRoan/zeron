use crate::{EngineError, Terminals};
use base64::Engine as _;
use std::collections::HashMap;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use zeron_proto::{TerminalEvent, WorktreeSettings};

const LOG_LIMIT: usize = 256 * 1024;

/// Retain recent output, including errors from retries after a noisy install.
/// Compact in batches instead of rewriting the entire log for every PTY chunk.
async fn append_output(output: &mut tokio::fs::File, bytes: &[u8]) -> std::io::Result<()> {
    let bytes = &bytes[bytes.len().saturating_sub(LOG_LIMIT)..];
    // Tokio can still have the preceding write in flight when write_all
    // returns. Measure its completed size before deciding whether to compact.
    output.flush().await?;
    let length = output.metadata().await?.len();
    if length.saturating_add(bytes.len() as u64) > LOG_LIMIT as u64 {
        let keep = (LOG_LIMIT / 2).min(LOG_LIMIT - bytes.len()).min(length as usize);
        output.seek(std::io::SeekFrom::End(-(keep as i64))).await?;
        let mut tail = vec![0; keep];
        output.read_exact(&mut tail).await?;
        output.set_len(0).await?;
        output.write_all(&tail).await?;
    }
    output.write_all(bytes).await
}

pub(super) async fn run(
    root: &Path,
    checkout: &Path,
    settings: &WorktreeSettings,
    command: &str,
    log: &Path,
    token: &CancellationToken,
    in_project: bool,
) -> Result<(), EngineError> {
    run_with_refs(
        root, checkout, settings, command, log, token, in_project, None,
    )
    .await
}

/// References are passed as environment variables, never interpolated into shell code.
/// Creation needs the intended branch and base before the destination exists.
pub(super) async fn run_with_refs(
    root: &Path,
    checkout: &Path,
    settings: &WorktreeSettings,
    command: &str,
    log: &Path,
    token: &CancellationToken,
    in_project: bool,
    refs: Option<(&str, &str)>,
) -> Result<(), EngineError> {
    if token.is_cancelled() {
        return Err(EngineError::Other("Worktree setup was cancelled.".into()));
    }
    super::files::private_directory(log.parent().unwrap())?;
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).read(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut output = options.open(log).await?;
    let mut environment: HashMap<String, String> = settings.variables.clone().into_iter().collect();
    let source = root.canonicalize()?.to_string_lossy().into_owned();
    let destination = checkout
        .canonicalize()
        .or_else(|_| {
            Ok::<_, std::io::Error>(
                checkout
                    .parent()
                    .unwrap_or(root)
                    .canonicalize()?
                    .join(checkout.file_name().unwrap_or_default()),
            )
        })?
        .to_string_lossy()
        .into_owned();
    let branch = match refs {
        Some((branch, _)) => branch.to_string(),
        None => branch_name(checkout).await,
    };
    let base = match refs {
        Some((_, base)) => base.to_string(),
        None => branch_name(root).await,
    };
    let name = checkout
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    environment.extend([
        ("ZERON_PROJECT_ROOT".into(), source.clone()),
        ("ZERON_CHECKOUT_ROOT".into(), destination.clone()),
        ("ZERON_WORKTREE_PATH".into(), destination.clone()),
        ("CODEX_SOURCE_TREE_PATH".into(), source.clone()),
        ("CODEX_WORKTREE_PATH".into(), destination.clone()),
        ("ZERON_BRANCH_NAME".into(), branch.clone()),
        ("ZERON_BASE_BRANCH".into(), base.clone()),
        ("ZERON_WORKTREE_NAME".into(), name.clone()),
        ("branchname".into(), branch),
        ("basebranch".into(), base),
        ("worktreename".into(), name),
        ("projectroot".into(), source),
        ("worktreepath".into(), destination),
    ]);
    let terminals = Terminals::new();
    let cwd = if in_project { root } else { checkout };
    let session = terminals.open_service(&cwd.to_string_lossy(), &environment, command)?;
    let mut guard = CommandGuard {
        terminals: terminals.clone(),
        id: Some(session.id.clone()),
    };
    let mut events = terminals.subscribe(&session.id, None)?;
    let result=tokio::time::timeout(std::time::Duration::from_secs(settings.command_timeout_seconds),async{
        loop{
            tokio::select!{
                _=token.cancelled()=>return Err(EngineError::Other("Worktree setup was cancelled.".into())),
                event=events.recv()=>match event{
                    Some(TerminalEvent::Data{data,..})=>{
                        if let Ok(bytes)=base64::engine::general_purpose::STANDARD.decode(data){append_output(&mut output, &bytes).await?;}
                    },
                    Some(TerminalEvent::Exit{exit_code,..})=>{output.flush().await?;return if exit_code==0{Ok(())}else{Err(EngineError::Other(format!("Workflow exited with status {exit_code}. Open its setup output for details.")))}} ,
                    None=>return Err(EngineError::Other("Workflow output ended before completion.".into())),
                }
            }
        }
    }).await.unwrap_or_else(|_|Err(EngineError::Other(format!("Workflow timed out after {} seconds. Its processes were stopped.",settings.command_timeout_seconds))));
    let id = session.id;
    tokio::task::spawn_blocking(move || terminals.close_service_and_wait(&id))
        .await
        .map_err(|e| EngineError::Other(e.to_string()))??;
    guard.id = None;
    result
}

async fn branch_name(path: &Path) -> String {
    tokio::process::Command::new("git")
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .current_dir(path)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
}

struct CommandGuard {
    terminals: Terminals,
    id: Option<String>,
}
impl Drop for CommandGuard {
    fn drop(&mut self) {
        if let Some(id) = &self.id {
            let _ = self.terminals.close(id);
        }
    }
}
