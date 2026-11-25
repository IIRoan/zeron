//! Zeron's version, as `ZERON_VERSION`: the root workspace's
//! (`[workspace.package] version` in ../../Cargo.toml). zgx is a workspace of
//! its own at 0.0.0, so its crate version says nothing of which Zeron this
//! is. Templates are stamped with it (src/plugins/live.rs), and one made for
//! another Zeron reads as out of date.
//!
//! And the app's own version, as `ZERON_APP_VERSION`: a beta build's
//! (`ZERON_BETA_VERSION=0.2.136-beta.1` at build time, the release
//! workflow's), else Zeron's. `zeron --version` prints it and the updater
//! compares releases with it.

fn main() {
    zgx_fork_contains_pin();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        use std::{path::PathBuf, process::Command};
        let source = "native/dictation_permission.m";
        println!("cargo:rerun-if-changed={source}");
        let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
        let object = out.join("dictation-permission.o");
        assert!(
            Command::new("clang")
                .args(["-fobjc-arc", "-c", source, "-o"])
                .arg(&object)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("ar")
                .arg("crus")
                .arg(out.join("libdictation-permission.a"))
                .arg(object)
                .status()
                .unwrap()
                .success()
        );
        println!("cargo:rustc-link-search=native={}", out.display());
        println!("cargo:rustc-link-lib=static=dictation-permission");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
    }

    // (Read as it runs, not with `env!`: a target dir shared between
    // checkouts can run a build script compiled in another, which then
    // read that checkout's manifest, or none once it was removed.)
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let manifest = std::path::Path::new(&dir).join("../../Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-env-changed=ZERON_BETA_VERSION");
    let text = std::fs::read_to_string(&manifest).unwrap_or_default();
    let version = workspace_version(&text).unwrap_or_default();
    let app = std::env::var("ZERON_BETA_VERSION")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| version.clone());
    println!("cargo:rustc-env=ZERON_VERSION={version}");
    println!("cargo:rustc-env=ZERON_APP_VERSION={app}");

    // On Windows, Zeron's icon in the exe: without one the taskbar shows
    // the system's blank one, and zgx has none to give the window.
    println!("cargo:rerun-if-changed=../../dist/windows/zeron.rc");
    println!("cargo:rerun-if-changed=../../dist/windows/zeron.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile_for(
            "../../dist/windows/zeron.rc",
            &["zeron-zgx"],
            embed_resource::NONE,
        )
        .manifest_required()
        .expect("Windows app icon resource compilation failed");
    }
}

/// `version = "…"` under `[workspace.package]`.
fn workspace_version(toml: &str) -> Option<String> {
    let mut inside = false;
    for line in toml.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == "[workspace.package]";
        } else if inside && let Some(rest) = line.strip_prefix("version") {
            let value = rest.trim_start().strip_prefix('=')?.trim();
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

/// (Linux fork) zgx comes from the fork's branch through Cargo.toml's
/// `[patch."<upstream's zgx>"]`: the fork's commits on top of upstream's pin.
/// A fork commit without that pin in its history would quietly build an
/// older zgx than the code upstream merged in expects, so refuse it.
fn zgx_fork_contains_pin() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let manifest = std::path::Path::new(&dir).join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let text = std::fs::read_to_string(&manifest).unwrap_or_default();
    let Some(url) = text.lines().find_map(|l| l.strip_prefix("[patch.\"")?.strip_suffix("\"]")) else {
        return;
    };
    let field = |line: &str, key: &str| -> Option<String> {
        let rest = line.split(&format!("{key} = \"")).nth(1)?;
        Some(rest.split('"').next()?.to_owned())
    };
    let (mut pins, mut fork, mut in_patch) = (Vec::new(), None, false);
    for line in text.lines() {
        if line.starts_with('[') {
            in_patch = line == format!("[patch.\"{url}\"]");
        } else if in_patch {
            fork = fork.or(field(line, "git").zip(field(line, "rev")));
        } else if field(line, "git").as_deref() == Some(url) {
            pins.extend(field(line, "rev"));
        }
    }
    let (Some((git, fork)), Some(pin)) = (fork, pins.first()) else {
        panic!("Cargo.toml patches {url}, but nothing depends on it with a rev, or the patch has none");
    };
    assert!(pins.iter().all(|p| p == pin), "upstream's zgx revs in Cargo.toml differ: {pins:?}");
    let repo = git.strip_prefix("file://").unwrap_or(&git);
    let contains = std::process::Command::new("git")
        .args(["-C", repo, "merge-base", "--is-ancestor", pin, &fork])
        .status()
        .is_ok_and(|s| s.success());
    assert!(
        contains,
        "the zgx fork's {fork} (in {repo}) doesn't contain upstream's zgx pin {pin}: it would build an older \
         zgx than this code expects. Run scripts/sync-zgx-fork.py to rebase the fork and take its new commit."
    );
}
