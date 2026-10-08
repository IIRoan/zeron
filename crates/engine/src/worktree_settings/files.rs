use crate::EngineError;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use tokio_util::sync::CancellationToken;

pub(super) fn private_directory(path: &Path) -> Result<(), EngineError> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(super) fn relative(path: &str) -> Result<(), EngineError> {
    if path.is_empty()
        || path.len() > 4096
        || path.contains('\0')
        || Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        || Path::new(path)
            .components()
            .any(|c| c.as_os_str() == ".git")
        || path == "."
    {
        return Err(EngineError::Other("Choose a relative file or folder inside the checkout; parent paths and .git are not allowed.".into()));
    }
    Ok(())
}

pub(super) fn source(root: &Path, path: &str) -> Result<PathBuf, EngineError> {
    relative(path)?;
    let root = root.canonicalize()?;
    let candidate = root.join(path);
    let resolved = candidate
        .canonicalize()
        .map_err(|_| EngineError::Other(format!("Selected source is missing: {path}")))?;
    if !resolved.starts_with(&root) {
        return Err(EngineError::Other(format!(
            "Selected source leaves the project: {path}"
        )));
    }
    Ok(resolved)
}

pub(super) fn destination(root: &Path, path: &str) -> Result<PathBuf, EngineError> {
    relative(path)?;
    let root = root.canonicalize()?;
    let target = root.join(path);
    let mut current = root.clone();
    for component in Path::new(path).components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(EngineError::Other(format!(
                    "Destination is a symbolic link; it was preserved: {path}"
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(target)
}

pub(super) fn read_env(path: &Path) -> Result<Vec<u8>, EngineError> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err(EngineError::Other(
            "Environment files must be regular files smaller than 1 MiB.".into(),
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(EngineError::Other(
            "Environment file grew beyond 1 MiB.".into(),
        ));
    }
    Ok(bytes)
}

pub(super) fn private_write(path: &Path, bytes: &[u8], replace: bool) -> Result<(), EngineError> {
    let parent = path
        .parent()
        .ok_or_else(|| EngineError::Other("Invalid destination file.".into()))?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    if replace {
        file.persist(path).map_err(|e| e.error)?;
    } else {
        match file.persist_noclobber(path) {
            Ok(_) => {}
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.error.into()),
        }
    }
    Ok(())
}

pub(super) fn copy_env(root: &Path, checkout: &Path, path: &str) -> Result<(), EngineError> {
    let target = destination(checkout, path)?;
    if target.exists() {
        read_env(&target)?;
        return Ok(());
    }
    private_write(&target, &read_env(&source(root, path)?)?, false)
}

pub(super) fn link(root: &Path, checkout: &Path, path: &str) -> Result<(), EngineError> {
    relative(path)?;
    let source = source(root, path)?;
    let target = checkout.join(path);
    if let Ok(existing) = std::fs::symlink_metadata(&target) {
        if existing.file_type().is_symlink() && target.canonicalize().ok().as_ref() == Some(&source)
        {
            return Ok(());
        }
        return Err(EngineError::Other(format!(
            "{path} already exists. It was preserved; remove it explicitly or choose independent copies."
        )));
    }
    let target = destination(checkout, path)?;
    std::fs::create_dir_all(target.parent().unwrap())?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target)?;
    }
    #[cfg(not(unix))]
    {
        return Err(EngineError::Other(
            "Shared symbolic links are supported by this Linux fork.".into(),
        ));
    }
    Ok(())
}

pub(super) fn check_cancel(token: &CancellationToken) -> Result<(), EngineError> {
    if token.is_cancelled() {
        Err(EngineError::Other("Worktree setup was cancelled.".into()))
    } else {
        Ok(())
    }
}

pub(super) fn copy_dependencies(
    root: &Path,
    checkout: &Path,
    path: &str,
    token: &CancellationToken,
) -> Result<(), EngineError> {
    let target = destination(checkout, path)?;
    if target.exists() {
        return Ok(());
    }
    let source = source(root, path)?;
    if !source.is_dir() {
        return Err(EngineError::Other(format!(
            "Dependency folder is unavailable: {path}"
        )));
    }
    std::fs::create_dir_all(target.parent().unwrap())?;
    let staging = tempfile::tempdir_in(target.parent().unwrap())?;
    let copied = staging.path().join("dependencies");
    copy_tree(&source, &copied, root, checkout, token, &mut 0)?;
    check_cancel(token)?;
    if target.exists() {
        return Err(EngineError::Other(format!(
            "Dependency destination appeared during copying; it was preserved: {path}"
        )));
    }
    std::fs::rename(copied, target)?;
    Ok(())
}

fn copy_tree(
    source: &Path,
    target: &Path,
    root: &Path,
    checkout: &Path,
    token: &CancellationToken,
    count: &mut usize,
) -> Result<(), EngineError> {
    check_cancel(token)?;
    if target.components().count() > 128 {
        return Err(EngineError::Other(
            "Dependency folders are nested too deeply. Use a separate install for this checkout."
                .into(),
        ));
    }
    *count += 1;
    if *count > 500_000 {
        return Err(EngineError::Other(
            "Dependency copy exceeded 500,000 entries. Use a separate install for this checkout."
                .into(),
        ));
    }
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        let original = std::fs::read_link(source)?;
        let rewritten = if original.is_absolute() && original.starts_with(root) {
            checkout.join(original.strip_prefix(root).unwrap())
        } else {
            original
        };
        #[cfg(unix)]
        std::os::unix::fs::symlink(rewritten, target)?;
    } else if metadata.is_dir() {
        std::fs::create_dir(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(
                &entry.path(),
                &target.join(entry.file_name()),
                root,
                checkout,
                token,
                count,
            )?;
        }
    } else if metadata.is_file() {
        std::fs::copy(source, target)?;
    } else {
        return Err(EngineError::Other(
            "A dependency folder contains a special file that cannot be copied.".into(),
        ));
    }
    Ok(())
}

/// Bounded filename-only discovery. Ignore dependency/build trees and links;
/// no env values are ever read by discovery.
pub(super) fn detect(root: &Path) -> (Vec<String>, Vec<String>) {
    let mut env = Vec::new();
    let mut dependencies = Vec::new();
    let started = std::time::Instant::now();
    let mut remaining = 25_000usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = stack.pop() {
        if depth > 10 || remaining == 0 || started.elapsed() > std::time::Duration::from_secs(2) {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            if remaining == 0 {
                break;
            }
            remaining -= 1;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().into_owned();
            if name == "node_modules" {
                dependencies.push(relative);
                continue;
            }
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if !matches!(
                    name.as_str(),
                    ".git"
                        | ".next"
                        | ".nuxt"
                        | ".cache"
                        | ".turbo"
                        | "target"
                        | "build"
                        | "dist"
                        | "vendor"
                        | ".venv"
                ) {
                    stack.push((path, depth + 1));
                }
            } else if kind.is_file()
                && (name == ".env" || name.starts_with(".env.") || name == ".dev.vars")
                && !name.contains("example")
                && !name.contains("sample")
                && !name.contains("template")
            {
                env.push(relative);
            }
        }
    }
    env.sort();
    env.truncate(128);
    dependencies.sort();
    dependencies.truncate(64);
    (env, dependencies)
}
