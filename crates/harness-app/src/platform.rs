//! Everything the app does differently per OS, in one place.
//!
//! Two kinds of difference, two mechanisms:
//!
//! 1. **Facts** (words, fonts, install commands, which title-bar controls
//!    exist, which sounds the picker lists): one [`Profile`] per OS below,
//!    side by side. All three compile on every OS, so a Windows build also
//!    type-checks the Mac's entries and the tests compare them anywhere. The
//!    rest of the app reads [`CURRENT`] and never asks which OS it is on.
//! 2. **Behaviour** that compiles everywhere but acts differently (how a path
//!    is spelled for the shell, how a sound is played): a `match` on
//!    [`OS`] (`os_compat::Os`), so adding an OS is a compile error at every
//!    place that has to decide.
//!
//! `#[cfg]` is left for code that only *compiles* on one OS (it calls that
//! OS's APIs). Each such piece is listed here, so this file is the map:
//!
//! | Piece                    | macOS                              | Windows                          |
//! |--------------------------|------------------------------------|----------------------------------|
//! | Browser tab              | `browser_pane/webview.rs` (WKWebView) | `browser_pane/webview_win.rs` (WebView2) |
//! | Notifications            | `notifier.rs` (UserNotifications)  | `notifier_win.rs` (WinRT toasts) |
//! | Sessions that outlive the app | tmux                          | `oo-session` (bundled, tmux-compatible) |
//! | Listening ports          | `ports.rs` (`lsof`)                | `os_compat::net` (IP Helper)     |
//! | Keep awake               | `remote/keepawake.rs` (`caffeinate`) | `os_compat::power`             |
//! | Unix file APIs           | `std::os::unix`                    | `os_compat::{fs, ffi, process}`  |
//! | Login environment        | `login_env` (PATH from the login shell) | [`prepare_environment`] (HOME, SHELL) |
//! | Native About panel, link routing, update check via Homebrew | `#[cfg(target_os = "macos")]` in `update_check.rs`, `link_routing/platform.rs` | the app's own dialog |
//!
//! A test (`no_os_checks_outside_platform`) keeps `cfg!(windows)` and
//! `cfg!(target_os = …)` expressions from creeping back into the rest of the
//! app.

pub use os_compat::Os;

use crate::sounds::{self, Sound};

/// The OS this build runs on.
pub const OS: Os = Os::CURRENT;

/// This OS's [`Profile`].
pub const CURRENT: &Profile = Profile::of(OS);

/// What differs per OS, as data.
#[derive(Debug)]
pub struct Profile {
    pub os: Os,
    /// The OS's name, where something comes from it ("macOS" system sounds).
    pub system_name: &'static str,
    /// What the user calls this computer, mid-sentence and starting one:
    /// "Keep this Mac awake", "This PC has no local network address".
    pub this_computer: &'static str,
    pub this_computer_cap: &'static str,
    /// The Ports tab's heading for ports outside the task ("OTHER ON THIS
    /// MAC"); also the key its collapsed state is saved under.
    pub ports_elsewhere: &'static str,
    /// Fallback UI and monospace fonts when the bundled Plex fonts are
    /// missing; the system's own.
    pub ui_font: &'static str,
    pub mono_font: &'static str,
    /// Whether the keyboard has ⌘. Without it the app's ⌘ shortcuts move
    /// to Alt (global) or Ctrl (editor), and labels read `Ctrl+Shift+Z`
    /// instead of `⌃⇧Z` (`shortcuts.rs`).
    pub command_key: bool,
    pub title_bar: TitleBar,
    pub sessions: Sessions,
    /// The commands the first-run checks suggest for missing tools.
    pub install: Install,
    /// The update dialog's advice when the app was not installed with
    /// Homebrew.
    pub upgrade_hint: &'static str,
    /// The About dialog's description of the app.
    pub about_kind: &'static str,
    /// The notification sounds the settings picker offers.
    pub sounds: &'static [Sound],
    /// Where bundled sounds sit relative to the executable once installed.
    pub bundled_sounds_dir: &'static str,
    /// Shell names (besides the POSIX ones) an agent pane can drop back to.
    pub extra_shells: &'static [&'static str],
}

/// The title bar's OS-provided parts and what the app draws instead.
#[derive(Debug)]
pub struct TitleBar {
    /// macOS draws the traffic lights over the bar's left end.
    pub traffic_lights: bool,
    /// No OS menu bar: the app's menus sit behind a ☰ button.
    pub app_menu_button: bool,
    /// The app draws minimize / maximize / close itself.
    pub caption_buttons: bool,
    /// The OS hit-tests the bar as the window's drag area (Windows'
    /// HTCAPTION), so clickable things on it must claim their clicks.
    pub drag_area_takes_clicks: bool,
}

/// What keeps agent sessions alive across app restarts.
#[derive(Debug)]
pub struct Sessions {
    /// The program's name as the user sees it.
    pub host: &'static str,
    /// Whether the app refuses to start without it (else it degrades:
    /// agents end with their tab).
    pub required: bool,
    /// The settings hint when it is missing.
    pub missing_hint: &'static str,
}

#[derive(Debug)]
pub struct Install {
    pub git: &'static str,
    pub sessions: &'static str,
    pub gh: &'static str,
}

impl Profile {
    pub const fn of(os: Os) -> &'static Profile {
        match os {
            Os::Mac => &MAC,
            Os::Windows => &WINDOWS,
            Os::Linux => &LINUX,
        }
    }
}

const MAC: Profile = Profile {
    os: Os::Mac,
    system_name: "macOS",
    this_computer: "this Mac",
    this_computer_cap: "This Mac",
    ports_elsewhere: "OTHER ON THIS MAC",
    ui_font: "Helvetica Neue",
    mono_font: "Menlo",
    command_key: true,
    title_bar: TitleBar {
        traffic_lights: true,
        app_menu_button: false,
        caption_buttons: false,
        drag_area_takes_clicks: false,
    },
    sessions: Sessions {
        host: "tmux",
        required: true,
        missing_hint: "Install tmux so agent sessions survive app restarts.",
    },
    install: Install {
        git: "xcode-select --install",
        sessions: "brew install tmux",
        gh: "brew install gh   (then: gh auth login)",
    },
    upgrade_hint: "Download the new version from GitHub and replace the app in Applications.",
    about_kind: "A native macOS app",
    sounds: sounds::MAC_CATALOG,
    bundled_sounds_dir: "../Resources",
    extra_shells: &[],
};

const WINDOWS: Profile = Profile {
    os: Os::Windows,
    system_name: "Windows",
    this_computer: "this PC",
    this_computer_cap: "This PC",
    ports_elsewhere: "OTHER ON THIS PC",
    ui_font: "Segoe UI",
    mono_font: "Consolas",
    command_key: false,
    title_bar: TitleBar {
        traffic_lights: false,
        app_menu_button: true,
        caption_buttons: true,
        drag_area_takes_clicks: true,
    },
    sessions: Sessions {
        host: "oo-session",
        required: false,
        missing_hint: "oo-session.exe is missing beside the app: terminals and agents end when \
                       the app quits. Rebuild (make build) or reinstall.",
    },
    install: Install {
        git: "winget install --id Git.Git",
        sessions: "bundled as oo-session.exe beside the app: rebuild (make build) or reinstall",
        gh: "winget install --id GitHub.cli   (then: gh auth login)",
    },
    upgrade_hint: "Download the new version from GitHub, or rebuild it from source with \
                   `make install`.",
    about_kind: "A native app",
    sounds: sounds::WINDOWS_CATALOG,
    bundled_sounds_dir: "sounds",
    extra_shells: &["cmd", "powershell", "pwsh"],
};

const LINUX: Profile = Profile {
    os: Os::Linux,
    system_name: "Linux",
    this_computer: "this computer",
    this_computer_cap: "This computer",
    ports_elsewhere: "OTHER ON THIS COMPUTER",
    ui_font: "DejaVu Sans",
    mono_font: "DejaVu Sans Mono",
    command_key: false,
    title_bar: TitleBar {
        traffic_lights: false,
        app_menu_button: true,
        caption_buttons: false,
        drag_area_takes_clicks: false,
    },
    sessions: Sessions {
        host: "tmux",
        required: true,
        missing_hint: "Install tmux so agent sessions survive app restarts.",
    },
    install: Install {
        git: "install git with your package manager",
        sessions: "install tmux with your package manager",
        gh: "install gh with your package manager   (then: gh auth login)",
    },
    upgrade_hint: "Download the new version from GitHub.",
    about_kind: "A native app",
    sounds: sounds::BUNDLED_CATALOG,
    bundled_sounds_dir: "sounds",
    extra_shells: &[],
};

/// Make the process environment what the rest of the app assumes, before
/// anything reads it or any thread exists (it writes the environment).
///
/// - macOS / Linux: a Finder- or launcher-started app sees only a minimal
///   `PATH`; take the login shell's (`login_env`).
/// - Windows: the app reads `HOME` (for `~/.openorchestra`) and runs agents
///   through `$SHELL -lic '<POSIX script>'`, and Windows sets neither: default
///   `HOME` to the profile directory and `SHELL` to Git Bash. Its login shell
///   reports a POSIX-form PATH, so PATH is left alone.
pub fn prepare_environment() {
    match OS {
        Os::Mac | Os::Linux => crate::login_env::hydrate_path(),
        Os::Windows => windows_env_defaults(),
    }
}

fn windows_env_defaults() {
    use std::path::PathBuf;
    // SAFETY: called first thing in main, single-threaded.
    unsafe {
        if std::env::var_os("HOME").is_none()
            && let Some(profile) = std::env::var_os("USERPROFILE")
        {
            std::env::set_var("HOME", profile);
        }
        if std::env::var_os("SHELL").is_none() {
            let candidates = [
                std::env::var_os("ProgramFiles").map(|p| PathBuf::from(p).join(r"Git\bin\bash.exe")),
                std::env::var_os("LOCALAPPDATA")
                    .map(|p| PathBuf::from(p).join(r"Programs\Git\bin\bash.exe")),
            ];
            if let Some(bash) = candidates.into_iter().flatten().find(|p| p.is_file()) {
                std::env::set_var("SHELL", bash);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_profile_is_for_its_own_os() {
        for os in Os::ALL {
            assert_eq!(Profile::of(os).os, os);
        }
        assert_eq!(CURRENT.os, OS);
    }

    #[test]
    fn every_os_offers_the_default_sound_and_silence() {
        for os in Os::ALL {
            let ids: Vec<_> = Profile::of(os).sounds.iter().map(|s| s.id).collect();
            assert!(ids.contains(&sounds::DEFAULT), "{os:?}: {ids:?}");
            assert!(ids.contains(&"silent"), "{os:?}: {ids:?}");
        }
    }

    #[test]
    fn only_the_mac_has_traffic_lights_and_a_menu_bar() {
        for os in Os::ALL {
            let bar = &Profile::of(os).title_bar;
            assert_eq!(bar.traffic_lights, os == Os::Mac, "{os:?}");
            assert_eq!(bar.app_menu_button, os != Os::Mac, "{os:?}");
            // Something has to close the window.
            assert!(bar.traffic_lights || bar.caption_buttons || os == Os::Linux);
        }
    }

    /// The rest of the app reads [`CURRENT`] or matches on [`OS`]; an OS
    /// check written inline elsewhere is how per-OS code ends up scattered.
    /// `#[cfg(...)]` attributes (code that only compiles on one OS) are
    /// fine; `cfg!(...)` expressions about the OS are not.
    #[test]
    fn no_os_checks_outside_platform() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") || path.ends_with("platform.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (n, line) in text.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or("");
                    if ["cfg!(windows", "cfg!(unix", "cfg!(target_os", "cfg!(not(", "cfg!(any("]
                        .iter()
                        .any(|p| code.contains(p))
                    {
                        offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "OS checks belong in platform.rs (a Profile field or a match on OS):\n{}",
            offenders.join("\n")
        );
    }
}
