//! Windows only: put `WebView2Loader.dll` (and Windows Terminal's ConPTY,
//! see `copy_conpty`) next to the executable.
//!
//! The browser tab loads it at runtime (`browser_pane/webview_win.rs`), and
//! the GNU toolchain has no static loader to link instead. The DLL ships in
//! the `webview2-com-sys` crate the app already depends on; this copies the
//! locked version's copy for the target architecture into the profile
//! directory (`target/<profile>/`), where `cargo run` and `cargo build`
//! leave `harness-app.exe`. Packaging (`scripts/windows/make.ps1 dist`)
//! copies it from there. Elsewhere this does nothing.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.lock");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x64",
        Ok("aarch64") => "arm64",
        Ok("x86") => "x86",
        _ => return,
    };
    let Some(src) = loader_dll(arch) else {
        println!("cargo:warning=WebView2Loader.dll not found; browser tabs will not open");
        return;
    };
    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out.
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let Some(profile_dir) = out.ancestors().nth(3) else {
        return;
    };
    let dest = profile_dir.join("WebView2Loader.dll");
    if let Err(e) = std::fs::copy(&src, &dest) {
        // A running app holds the DLL open; the copy there is the same one.
        if !dest.exists() {
            println!("cargo:warning=could not copy WebView2Loader.dll: {e}");
        }
    }
    copy_conpty(profile_dir, arch);
    embed_icon(&out);
}

/// Windows Terminal's ConPTY (`conpty.dll` + `OpenConsole.exe`) beside the
/// exe, where the terminal library looks for it before falling back to the
/// ConPTY built into Windows, which delivers arrow keys and mouse reports to
/// console-API programs (Codex, PowerShell) as text. `make build` downloads
/// it into `target/conpty/<version>/pkg` (`script/windows/make.ps1`,
/// `Use-ConPty`); this copies the newest version there.
fn copy_conpty(profile_dir: &Path, arch: &str) {
    let Some(target) = profile_dir.parent() else {
        return;
    };
    let cache = target.join("conpty");
    println!("cargo:rerun-if-changed={}", cache.display());
    let newest = std::fs::read_dir(&cache).ok().and_then(|dirs| {
        dirs.flatten()
            .map(|d| d.path().join("pkg"))
            .filter(|p| p.is_dir())
            .max()
    });
    let Some(pkg) = newest else {
        println!(
            "cargo:warning=no Windows Terminal ConPTY in target/conpty (run `make build` once): \
             arrow keys reach Codex and PowerShell as text"
        );
        return;
    };
    for (from, name) in [
        (pkg.join(format!("runtimes/win-{arch}/native/conpty.dll")), "conpty.dll"),
        (pkg.join(format!("build/native/runtimes/{arch}/OpenConsole.exe")), "OpenConsole.exe"),
    ] {
        let dest = profile_dir.join(name);
        // A running app holds them open; the copy there is the same one.
        if std::fs::copy(&from, &dest).is_err() && !dest.exists() {
            println!("cargo:warning=could not copy {name} from {}", from.display());
        }
    }
}

/// The exe's icon as resource 1, which gpui loads for the window and the
/// taskbar (and Explorer shows for the exe): `assets/openOrchestra.ico`
/// (`make icon`), compiled with WinLibs' `windres`. Skipped, with a
/// warning, without either.
fn embed_icon(out: &Path) {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = manifest.join("../../assets/openOrchestra.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    let Ok(ico) = ico.canonicalize() else {
        println!("cargo:warning=assets/openOrchestra.ico missing (make icon); the exe has no icon");
        return;
    };
    let rc = out.join("app.rc");
    let res = out.join("app-res.o");
    // windres takes forward slashes in the .rc; strip the \\?\ prefix.
    let ico = ico.to_string_lossy().trim_start_matches(r"\\?\").replace('\\', "/");
    if std::fs::write(&rc, format!("1 ICON \"{ico}\"\n")).is_err() {
        return;
    }
    let status = std::process::Command::new("windres")
        .args(["-O", "coff", "-i"])
        .arg(&rc)
        .arg("-o")
        .arg(&res)
        .status();
    match status {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bins={}", res.display()),
        Ok(s) => println!("cargo:warning=windres failed ({s}); the exe has no icon"),
        Err(e) => println!("cargo:warning=no windres ({e}); the exe has no icon"),
    }
}

/// The locked `webview2-com-sys`'s loader for `arch` in cargo's registry.
fn loader_dll(arch: &str) -> Option<PathBuf> {
    let version = locked_version("webview2-com-sys")?;
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|h| Path::new(&h).join(".cargo")))?;
    let registry = home.join("registry").join("src");
    std::fs::read_dir(registry).ok()?.flatten().find_map(|index| {
        let dll = index
            .path()
            .join(format!("webview2-com-sys-{version}"))
            .join(arch)
            .join("WebView2Loader.dll");
        dll.is_file().then_some(dll)
    })
}

fn locked_version(package: &str) -> Option<String> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").ok()?);
    let lock = std::fs::read_to_string(manifest.join("Cargo.lock")).ok()?;
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line.trim() == format!("name = \"{package}\"") {
            let version = lines.next()?.trim();
            return version
                .strip_prefix("version = \"")
                .and_then(|v| v.strip_suffix('"'))
                .map(str::to_string);
        }
    }
    None
}
