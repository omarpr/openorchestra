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
//! | PATH an installer changed | (not needed)                      | `os_compat::env` (registry) and `login_env::add_login_shell_dirs` (Git Bash rc files), via [`refresh_path`] |
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
    /// The OS this profile describes. Read only by the test that checks
    /// [`Profile::of`] hands each OS its own profile.
    #[cfg_attr(not(test), allow(dead_code))]
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
    /// The agent CLIs the app drives, in `preflight::AGENTS` order, and how to
    /// install each here (the requirements screen's Agents tab).
    pub agents: &'static [AgentInstall],
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

/// How to install each tool the requirements screen checks for.
#[derive(Debug)]
pub struct Install {
    pub git: Fix,
    pub sessions: Fix,
    pub gh: Fix,
    /// rust-analyzer on a machine without rustup: rustup first, then the
    /// component (with rustup present it is only the component).
    pub rustup_then_analyzer: Fix,
    /// A newer Node.js, for an npm-installed agent that is installed but
    /// fails to start on the Node it has (`preflight::MIN_NODE`).
    pub node: Fix,
}

/// How to get a missing tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    /// Shell commands, run in order. Each is runnable exactly as written:
    /// the screen copies it or types it into a new shell tab.
    Run(&'static [&'static str]),
    /// Nothing to run: what to do instead.
    Advice(&'static str),
}

/// rustup's own installer for macOS and Linux (rustup.rs).
const RUSTUP_SH: &str = "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh";
/// The same on every OS once rustup is there.
pub const ADD_ANALYZER: &str = "rustup component add rust-analyzer";

/// An agent CLI the app drives, and how to install it on this OS.
#[derive(Debug)]
pub struct AgentInstall {
    /// The command it runs as (`preflight::AGENTS`).
    pub program: &'static str,
    /// What its maker calls it.
    pub label: &'static str,
    pub fix: Fix,
}

// Each vendor's own recommended installer. Every command runs in the app's
// shell: bash, which on Windows is Git Bash — so a vendor's PowerShell
// installer is invoked through `powershell -Command`.
//
// Claude Code's native installer rather than npm: the npm package needs a
// newer Node than many machines have, the native one needs none.
const CLAUDE_SH: AgentInstall = AgentInstall {
    program: "claude",
    label: "Claude Code",
    fix: Fix::Run(&["curl -fsSL https://claude.ai/install.sh | bash"]),
};
const CLAUDE_PS: AgentInstall = AgentInstall {
    program: "claude",
    label: "Claude Code",
    fix: Fix::Run(&["powershell -NoProfile -Command \"irm https://claude.ai/install.ps1 | iex\""]),
};
const CODEX: AgentInstall = AgentInstall {
    program: "codex",
    label: "Codex",
    fix: Fix::Run(&["npm install -g @openai/codex"]),
};
const OPENCODE_SH: AgentInstall = AgentInstall {
    program: "opencode",
    label: "OpenCode",
    fix: Fix::Run(&["curl -fsSL https://opencode.ai/install | bash"]),
};
const OPENCODE_NPM: AgentInstall = AgentInstall {
    program: "opencode",
    label: "OpenCode",
    fix: Fix::Run(&["npm install -g opencode-ai"]),
};
const KIMI_SH: AgentInstall = AgentInstall {
    program: "kimi",
    label: "Kimi Code",
    fix: Fix::Run(&["curl -fsSL https://code.kimi.com/kimi-code/install.sh | bash"]),
};
const KIMI_PS: AgentInstall = AgentInstall {
    program: "kimi",
    label: "Kimi Code",
    fix: Fix::Run(&[
        "powershell -NoProfile -Command \"irm https://code.kimi.com/kimi-code/install.ps1 | iex\"",
    ]),
};
/// xAI's script supports Git Bash too.
const GROK: AgentInstall = AgentInstall {
    program: "grok",
    label: "Grok Build",
    fix: Fix::Run(&["curl -fsSL https://x.ai/cli/install.sh | bash"]),
};
/// Pinned: the version the native chat pane is tested against
/// (docs/m2-pi-pin.md, `PI_PIN` in script/doctor.sh).
const PI: AgentInstall = AgentInstall {
    program: "pi",
    label: "pi",
    fix: Fix::Run(&["npm install -g @mariozechner/pi-coding-agent@0.73.1"]),
};

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
    agents: &[CLAUDE_SH, CODEX, OPENCODE_SH, KIMI_SH, GROK, PI],
    install: Install {
        git: Fix::Run(&["xcode-select --install"]),
        sessions: Fix::Run(&["brew install tmux"]),
        gh: Fix::Run(&["brew install gh", "gh auth login"]),
        rustup_then_analyzer: Fix::Run(&[RUSTUP_SH, ADD_ANALYZER]),
        node: Fix::Advice("install Node.js 20 or newer: brew upgrade node, or nodejs.org"),
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
    agents: &[CLAUDE_PS, CODEX, OPENCODE_NPM, KIMI_PS, GROK, PI],
    install: Install {
        git: Fix::Run(&["winget install --id Git.Git"]),
        sessions: Fix::Advice(
            "bundled as oo-session.exe beside the app: rebuild (make build) or reinstall",
        ),
        gh: Fix::Run(&["winget install --id GitHub.cli", "gh auth login"]),
        rustup_then_analyzer: Fix::Run(&["winget install --id Rustlang.Rustup", ADD_ANALYZER]),
        // `install` upgrades an older Node it finds, and installs one if none.
        node: Fix::Run(&["winget install --id OpenJS.NodeJS.LTS"]),
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
    agents: &[CLAUDE_SH, CODEX, OPENCODE_SH, KIMI_SH, GROK, PI],
    install: Install {
        git: Fix::Advice("install git with your package manager"),
        sessions: Fix::Advice("install tmux with your package manager"),
        gh: Fix::Advice("install gh with your package manager, then run gh auth login"),
        rustup_then_analyzer: Fix::Run(&[RUSTUP_SH, ADD_ANALYZER]),
        node: Fix::Advice("install Node.js 20 or newer from nodejs.org or your package manager"),
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
        Os::Windows => {
            windows_env_defaults();
            // Directories only Git Bash's rc files add (`~/.grok/bin`).
            crate::login_env::add_login_shell_dirs();
        }
    }
}

/// Git for Windows' bash, where its installer puts it (for everyone, or
/// per user); `None` off Windows. Looked up again when needed: Git may be
/// installed while the app runs, after startup found no `SHELL` to set.
pub fn git_bash() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    match OS {
        Os::Windows => [
            std::env::var_os("ProgramFiles").map(|p| PathBuf::from(p).join(r"Git\bin\bash.exe")),
            std::env::var_os("LOCALAPPDATA")
                .map(|p| PathBuf::from(p).join(r"Programs\Git\bin\bash.exe")),
        ]
        .into_iter()
        .flatten()
        .find(|p| p.is_file()),
        Os::Mac | Os::Linux => None,
    }
}

/// Pick up `PATH` changes made since the app started, so a tool installed
/// while it runs is found by the requirements screen's Re-check, by an agent
/// tab, and by new terminals. Spawns a shell on Windows: call it off the UI
/// thread ([`refresh_system_path`] is the quick part).
///
/// - macOS / Linux: nothing to do. `PATH` came from the login shell at start
///   and package managers install into directories already on it.
/// - Windows: the registry's `PATH` (winget, .msi installers), then what Git
///   Bash's rc files add (curl-pipe-bash installers such as Grok's).
pub fn refresh_path() {
    match OS {
        Os::Mac | Os::Linux => {}
        Os::Windows => {
            refresh_system_path();
            crate::login_env::add_login_shell_dirs();
        }
    }
}

/// The registry half of [`refresh_path`]: cheap, fine on the UI thread. What
/// a new shell tab needs — bash reads its own rc files.
pub fn refresh_system_path() {
    match OS {
        Os::Mac | Os::Linux => {}
        Os::Windows => {
            if os_compat::env::refresh_path() {
                log::info!("PATH: picked up directories added since start");
            }
        }
    }
}

/// Whether a listening process belongs to the OS itself rather than to
/// anything the user would open or share (Ports hides these). macOS's own
/// paths are checked in `ports.rs` for every OS; this adds Windows': anything
/// under `%SystemRoot%`, and the core services whose executable path a normal
/// user can't read (they show up by name only).
pub fn is_system_process(path: &str) -> bool {
    match OS {
        Os::Mac | Os::Linux => false,
        Os::Windows => {
            const BY_NAME: &[&str] = &[
                "system", "idle", "svchost.exe", "lsass.exe", "wininit.exe", "winlogon.exe",
                "services.exe", "spoolsv.exe", "smss.exe", "csrss.exe", "registry",
            ];
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
            let lower = path.to_ascii_lowercase();
            let under_root = lower.starts_with(&format!("{}\\", root.to_ascii_lowercase()));
            let unreadable_core = !std::path::Path::new(path).is_absolute() && BY_NAME.contains(&lower.as_str());
            under_root || unreadable_core
        }
    }
}

#[cfg(test)]
mod system_process_tests {
    #[test]
    fn windows_services_are_the_os_s_and_user_servers_are_not() {
        let is = super::is_system_process;
        match super::OS {
            super::Os::Windows => {
                let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
                assert!(is(&format!(r"{root}\System32\svchost.exe")));
                assert!(is(&format!(r"{}\system32\spoolsv.exe", root.to_uppercase())));
                assert!(is("svchost.exe"), "a service shown by name only");
                assert!(is("System"));
                assert!(!is(r"C:\Program Files\nodejs\node.exe"));
                assert!(!is("mysqld.exe"), "a database the user may want to see");
            }
            super::Os::Mac | super::Os::Linux => assert!(!is("/usr/bin/node")),
        }
    }
}

/// The OS's own curl, by absolute path so nothing earlier on PATH can stand
/// in for it. Windows 10 (1803) and later ship one in System32.
pub fn system_curl() -> std::path::PathBuf {
    match OS {
        Os::Mac | Os::Linux => "/usr/bin/curl".into(),
        Os::Windows => std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| r"C:\Windows".into())
            .join(r"System32\curl.exe"),
    }
}

fn windows_env_defaults() {
    // SAFETY: called first thing in main, single-threaded.
    unsafe {
        if std::env::var_os("HOME").is_none()
            && let Some(profile) = std::env::var_os("USERPROFILE")
        {
            std::env::set_var("HOME", profile);
        }
        if std::env::var_os("SHELL").is_none()
            && let Some(bash) = git_bash()
        {
            std::env::set_var("SHELL", bash);
        }
        // Git for Windows aliases node, python & co. to `winpty <name>.exe`
        // when TERM is xterm-like, for mintty, which is not a console. The
        // app's terminals are ConPTY consoles, where winpty only gets in the
        // way: a backgrounded `node server.js &` is stopped by it. Bash
        // imports this exported function, so `winpty X` just runs X.
        if std::env::var_os(WINPTY_PASSTHROUGH.0).is_none() {
            std::env::set_var(WINPTY_PASSTHROUGH.0, WINPTY_PASSTHROUGH.1);
        }
    }
}

/// An exported bash function (`name%%` is bash's encoding) that runs its
/// arguments: see `windows_env_defaults`.
const WINPTY_PASSTHROUGH: (&str, &str) = ("BASH_FUNC_winpty%%", "() {  \"$@\"\n}");

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
