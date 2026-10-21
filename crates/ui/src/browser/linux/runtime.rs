//! Optional private WPE runtime. Its Debian library layout lives only in the
//! helper's mount namespace; the application and system libraries stay separate.
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub(super) fn command(
    executable: &Path,
    profile: Option<&Path>,
    wpe: bool,
) -> Result<Command, String> {
    if !wpe {
        return Ok(Command::new(executable));
    }
    let configured = std::env::var_os("ZERON_BROWSER_WPE_RUNTIME").map(PathBuf::from);
    let adjacent = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|p| p.join("browser-runtime")));
    let Some(root) =
        configured.or_else(|| adjacent.filter(|path| path.join("manifest.json").is_file()))
    else {
        return Ok(Command::new(executable));
    };
    let root = root
        .canonicalize()
        .map_err(|e| format!("Could not open WPE runtime: {e}"))?;
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("manifest.json"))
            .map_err(|e| format!("Could not read WPE runtime manifest: {e}"))?,
    )
    .map_err(|e| format!("Invalid WPE runtime manifest: {e}"))?;
    if manifest["format"] != 1 || manifest["architecture"].as_str() != Some(std::env::consts::ARCH)
    {
        return Err("Unsupported WPE runtime format or architecture".into());
    }
    let libraries = root.join("usr/lib/x86_64-linux-gnu");
    if !libraries.join("libWPEWebKit-2.0.so.1").is_file() {
        return Err("The private WPE runtime is incomplete".into());
    }
    if let Some(profile) = profile {
        if !profile.is_absolute() {
            return Err("Browser profile path must be absolute".into());
        }
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(profile)
            .map_err(|e| format!("Could not create browser profile: {e}"))?;
    }
    let mut command = Command::new("bwrap");
    // Startup runs on a short-lived worker thread. bwrap's --die-with-parent
    // follows that thread's lifetime on Linux and would kill a healthy helper
    // when startup returns. The command pipe's EOF controls helper shutdown.
    command
        .args([
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--dev-bind-try",
            "/dev/dri",
            "/dev/dri",
            "--proc",
            "/proc",
            "--unshare-pid",
            "--unshare-ipc",
            "--tmpfs",
            "/usr/lib",
            "--dir",
            "/usr/lib/x86_64-linux-gnu",
            "--ro-bind",
        ])
        .arg(libraries)
        .arg("/usr/lib/x86_64-linux-gnu")
        .args([
            "--tmpfs",
            "/tmp",
            "--tmpfs",
            "/run",
            "--perms",
            "0700",
            "--dir",
            "/tmp/runtime",
            "--setenv",
            "XDG_DATA_HOME",
            "/tmp/data",
            "--setenv",
            "XDG_CONFIG_HOME",
            "/tmp/config",
            "--setenv",
            "XDG_CACHE_HOME",
            "/tmp/cache",
            "--setenv",
            "XDG_RUNTIME_DIR",
            "/tmp/runtime",
            "--setenv",
            "LD_LIBRARY_PATH",
            "/usr/lib/x86_64-linux-gnu",
            "--unsetenv",
            "DBUS_SESSION_BUS_ADDRESS",
            "--unsetenv",
            "AT_SPI_BUS_ADDRESS",
            "--unsetenv",
            "WAYLAND_DISPLAY",
            "--unsetenv",
            "DISPLAY",
        ]);
    // /etc/resolv.conf often points into /run (systemd-resolved or
    // NetworkManager). Keep that one file visible after isolating /run so
    // WebKit can resolve ordinary website names through the host resolver.
    if let Ok(resolver) = Path::new("/etc/resolv.conf").canonicalize() {
        command.arg("--ro-bind").arg(&resolver).arg(&resolver);
    }
    if let Some(profile) = profile {
        command.arg("--bind").arg(profile).arg(profile);
    }
    command.arg("--ro-bind").arg(executable).arg(executable);
    command.arg("--").arg(executable);
    Ok(command)
}
