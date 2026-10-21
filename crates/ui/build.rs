use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(zeron_wpe)");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=src/dictation/permission.m");
        let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
        let object = out.join("voice-permission.o");
        assert!(
            Command::new("clang")
                .args(["-fobjc-arc", "-c", "src/dictation/permission.m", "-o"])
                .arg(&object)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("ar")
                .arg("crus")
                .arg(out.join("libvoice-permission.a"))
                .arg(object)
                .status()
                .unwrap()
                .success()
        );
        println!("cargo:rustc-link-search=native={}", out.display());
        println!("cargo:rustc-link-lib=static=voice-permission");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
    }

    println!("cargo:rerun-if-changed=src/browser/linux/helper.c");
    println!("cargo:rerun-if-changed=src/browser/linux/wpe_platform.h");
    println!("cargo:rerun-if-changed=src/browser/linux/storage_compat.h");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("zeron-webkit");
    let flags = Command::new("pkg-config")
        .args(["--cflags", "--libs", "webkit2gtk-4.1", "json-glib-1.0"])
        .output()
        .expect("pkg-config is required to build the Linux browser helper");
    assert!(
        flags.status.success(),
        "The Linux browser requires WebKitGTK 4.1 and JSON-GLib development files \
         discoverable by pkg-config (webkit2gtk-4.1 and json-glib-1.0). \
         See docs/reference/linux-browser.md for distribution-specific installation commands.\n{}",
        String::from_utf8_lossy(&flags.stderr)
    );
    let status = Command::new(env::var("CC").unwrap_or_else(|_| "cc".into()))
        // WebKit is system-managed. Cargo may need Homebrew's library path,
        // but using it to resolve GTK's dependencies mixes incompatible ABIs.
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("LIBRARY_PATH")
        .args([
            "-std=c11",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Wno-unused-parameter",
            "src/browser/linux/helper.c",
            "-o",
        ])
        .arg(&output)
        .args(String::from_utf8(flags.stdout).unwrap().split_whitespace())
        .status()
        .expect("C compiler is required to build the Linux browser helper");
    assert!(status.success(), "Linux browser helper compilation failed");

    // WPE is optional at build and run time. Releases built with its headers
    // prefer real GPU compositing; GTK remains available on older systems.
    let wpe = Command::new("pkg-config")
        .args(["--atleast-version=2.54", "wpe-webkit-2.0"])
        .output()
        .is_ok_and(|result| result.status.success());
    if wpe {
        let flags = Command::new("pkg-config")
            .args([
                "--cflags",
                "--libs",
                "wpe-webkit-2.0",
                "json-glib-1.0",
                "cairo",
                "xkbcommon",
            ])
            .output()
            .expect("WPE browser build flags");
        assert!(
            flags.status.success(),
            "WPE browser development dependencies are incomplete"
        );
        let status = Command::new(env::var("CC").unwrap_or_else(|_| "cc".into()))
            .env_remove("LD_LIBRARY_PATH")
            .env_remove("LIBRARY_PATH")
            .args([
                "-std=c11",
                "-O2",
                "-Wall",
                "-Wextra",
                "-Wno-unused-parameter",
                "-DZERON_WPE",
                "src/browser/linux/helper.c",
                "-o",
            ])
            .arg(output.with_file_name("zeron-webkit-wpe"))
            .args(String::from_utf8(flags.stdout).unwrap().split_whitespace())
            .status()
            .expect("C compiler is required to build the WPE browser helper");
        assert!(status.success(), "WPE browser helper compilation failed");
        println!("cargo:rustc-cfg=zeron_wpe");
    }
}
