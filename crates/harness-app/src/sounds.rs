//! Which sound a notification makes, kept as a *logical* name.
//!
//! The stored preference is `"bell"`, `"Glass"`, `"silent"` — never a
//! `soundNamed:` argument or a filesystem path. macOS resolution is one
//! function ([`file_name`]/[`path`]) and nothing else in the app knows how a
//! sound is addressed, so the freedesktop backend Linux would need slots in
//! beside it rather than through it. A pref file full of `.aiff` filenames
//! would have been a macOS assumption baked into user data.
//!
//! Two sources, both free of anything we may not ship:
//!
//! - **Bundled** — originals synthesized for this app, alongside the existing
//!   uh-oh chirp (itself an original homage, since the ICQ sample is
//!   copyrighted; see `notifier.rs`).
//! - **System** — the sounds already on every Mac. Nothing to ship, and the
//!   names are ones the user recognizes from elsewhere in the OS.

/// Where a sound comes from, which decides how it is resolved and played.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Ships in the app bundle's Resources as `<name>.wav`.
    Bundled,
    /// A macOS system sound: `/System/Library/Sounds/<Name>.aiff`.
    System,
    /// Whatever the OS considers its default notification sound.
    SystemDefault,
    /// No sound at all.
    Silent,
}

/// One entry in the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sound {
    /// The stored, platform-neutral identifier.
    pub id: &'static str,
    pub source: Source,
}

/// Bundled first — they are ours, and the first is the historical default —
/// then the system sounds in the order macOS lists them.
pub(crate) const MAC_CATALOG: &[Sound] = &[
    Sound {
        id: "uh-oh",
        source: Source::Bundled,
    },
    Sound {
        id: "bell",
        source: Source::Bundled,
    },
    Sound {
        id: "quack",
        source: Source::Bundled,
    },
    Sound {
        id: "default",
        source: Source::SystemDefault,
    },
    Sound {
        id: "silent",
        source: Source::Silent,
    },
    Sound {
        id: "Basso",
        source: Source::System,
    },
    Sound {
        id: "Blow",
        source: Source::System,
    },
    Sound {
        id: "Bottle",
        source: Source::System,
    },
    Sound {
        id: "Frog",
        source: Source::System,
    },
    Sound {
        id: "Funk",
        source: Source::System,
    },
    Sound {
        id: "Glass",
        source: Source::System,
    },
    Sound {
        id: "Hero",
        source: Source::System,
    },
    Sound {
        id: "Morse",
        source: Source::System,
    },
    Sound {
        id: "Ping",
        source: Source::System,
    },
    Sound {
        id: "Pop",
        source: Source::System,
    },
    Sound {
        id: "Purr",
        source: Source::System,
    },
    Sound {
        id: "Sosumi",
        source: Source::System,
    },
    Sound {
        id: "Submarine",
        source: Source::System,
    },
    Sound {
        id: "Tink",
        source: Source::System,
    },
];

/// Windows: the same bundled sounds, then the notification-ish sounds every
/// Windows install has in `%WINDIR%\Media` ([`windows_media_file`]).
pub(crate) const WINDOWS_CATALOG: &[Sound] = &[
    Sound { id: "uh-oh", source: Source::Bundled },
    Sound { id: "bell", source: Source::Bundled },
    Sound { id: "quack", source: Source::Bundled },
    Sound { id: "default", source: Source::SystemDefault },
    Sound { id: "silent", source: Source::Silent },
    Sound { id: "Notify", source: Source::System },
    Sound { id: "Messaging", source: Source::System },
    Sound { id: "Email", source: Source::System },
    Sound { id: "Calendar", source: Source::System },
    Sound { id: "Nudge", source: Source::System },
    Sound { id: "Ding", source: Source::System },
    Sound { id: "Chimes", source: Source::System },
    Sound { id: "Chord", source: Source::System },
    Sound { id: "Tada", source: Source::System },
];

/// Elsewhere (Linux): the bundled sounds only, until something resolves the
/// desktop's own.
pub(crate) const BUNDLED_CATALOG: &[Sound] = &[
    Sound { id: "uh-oh", source: Source::Bundled },
    Sound { id: "bell", source: Source::Bundled },
    Sound { id: "quack", source: Source::Bundled },
    Sound { id: "default", source: Source::SystemDefault },
    Sound { id: "silent", source: Source::Silent },
];

/// The picker's list for this OS (`platform::Profile::sounds`).
pub const CATALOG: &[Sound] = crate::platform::CURRENT.sounds;

/// How the settings screen names the [`Source::System`] sounds' origin.
pub const SYSTEM_LABEL: &str = crate::platform::CURRENT.system_name;

/// The `%WINDIR%\Media` file behind a Windows system sound id.
fn windows_media_file(id: &str) -> Option<&'static str> {
    Some(match id {
        "Notify" => "Windows Notify System Generic.wav",
        "Messaging" => "Windows Notify Messaging.wav",
        "Email" => "Windows Notify Email.wav",
        "Calendar" => "Windows Notify Calendar.wav",
        "Nudge" => "Windows Message Nudge.wav",
        "Ding" => "Windows Ding.wav",
        "Chimes" => "chimes.wav",
        "Chord" => "chord.wav",
        "Tada" => "tada.wav",
        _ => return None,
    })
}

/// What a fresh install uses: the chirp the app has always made.
pub const DEFAULT: &str = "uh-oh";

/// Look up a stored id, falling back to the default when it names something
/// this build does not have — a pref written by a later version, or a bundled
/// sound that was removed, must not silence notifications.
pub fn resolve(id: &str) -> Sound {
    CATALOG
        .iter()
        .copied()
        .find(|s| s.id == id)
        .unwrap_or_else(resolve_default)
}

fn resolve_default() -> Sound {
    CATALOG
        .iter()
        .copied()
        .find(|s| s.id == DEFAULT)
        .expect("the default sound is in the catalog")
}

/// The filename `UNNotificationSound::soundNamed` wants, or `None` when the
/// sound is silent or the system default (both of which the caller expresses
/// differently — an absent sound is not a named one).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn file_name(sound: Sound) -> Option<String> {
    match sound.source {
        Source::Bundled => Some(format!("{}.wav", sound.id)),
        Source::System => Some(format!("{}.aiff", sound.id)),
        Source::SystemDefault | Source::Silent => None,
    }
}

/// An absolute path to play for a preview, or `None` when there is nothing to
/// play or the file is not where it should be.
///
/// Bundled sounds live in `Contents/Resources` in a packaged app and in the
/// crate's `sounds/` directory during development, and the preview button has
/// to work in both — a settings screen whose ▶ does nothing in `make run` is a
/// settings screen nobody can check their choice with.
pub fn path(sound: Sound) -> Option<std::path::PathBuf> {
    use crate::platform::{OS, Os};
    match sound.source {
        Source::System => {
            let p = match OS {
                Os::Windows => {
                    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
                    std::path::PathBuf::from(windir)
                        .join("Media")
                        .join(windows_media_file(sound.id)?)
                }
                Os::Mac => std::path::PathBuf::from(format!(
                    "/System/Library/Sounds/{}.aiff",
                    sound.id
                )),
                Os::Linux => return None,
            };
            p.exists().then_some(p)
        }
        Source::Bundled => {
            let file = format!("{}.wav", sound.id);
            // macOS bundle: Contents/Resources. Windows dist: sounds\ beside
            // the exe.
            let resources = crate::platform::CURRENT.bundled_sounds_dir;
            let bundled = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|d| d.join(resources).join(&file)));
            if let Some(p) = bundled.filter(|p| p.exists()) {
                return Some(p);
            }
            let dev = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("sounds")
                .join(&file);
            dev.exists().then_some(dev)
        }
        Source::SystemDefault | Source::Silent => None,
    }
}

/// Play a sound once, for the settings preview.
///
/// `afplay` rather than binding an audio framework: it is the platform's own
/// one-shot player, it is always present, and a preview that fails is a
/// non-event — the alternative is linking an audio stack to play a 300 ms wav.
pub fn preview(id: &str) {
    let sound = resolve(id);
    if sound.source == Source::Silent {
        return;
    }
    // Windows plays through its own API (`play`); elsewhere `afplay` below.
    if crate::platform::OS == crate::platform::Os::Windows {
        play(sound);
        return;
    }
    // The system default has no file to point at; the closest honest preview is
    // the sound macOS itself uses for a default alert.
    let target = path(sound).or_else(|| {
        (sound.source == Source::SystemDefault)
            .then(|| std::path::PathBuf::from("/System/Library/Sounds/Ping.aiff"))
            .filter(|p| p.exists())
    });
    let Some(target) = target else {
        log::warn!("no audio file for sound {id:?}");
        return;
    };
    // Report the outcome instead of discarding it. A preview that fails
    // silently is the worst version of this feature: the button looks dead and
    // there is nothing anywhere to say why.
    match std::process::Command::new("afplay")
        .arg(&target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => {
            log::debug!("sound preview {id:?} -> {}", target.display());
            let id = id.to_string();
            std::thread::spawn(move || match child.wait_with_output() {
                Ok(out) if out.status.success() => {}
                Ok(out) => log::warn!(
                    "afplay exited {:?} for {id:?}: {}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(e) => log::warn!("afplay could not be waited on: {e}"),
            });
        }
        Err(e) => log::warn!(
            "could not run afplay for {id:?} ({}): {e}",
            target.display()
        ),
    }
}

/// Play `sound` once, asynchronously: a notification's (Windows toasts are
/// posted silent and the app plays the chosen sound) or a preview. The
/// bundled wavs are compiled in, so a bare exe plays them; system sounds
/// come from `%WINDIR%\Media`, and the system default is Windows' own
/// notification sound. Elsewhere this is the preview.
pub fn play(sound: Sound) {
    #[cfg(windows)]
    {
        use windows::Win32::Media::Audio::{
            PlaySoundW, SND_ASYNC, SND_FILENAME, SND_MEMORY, SND_NODEFAULT,
        };
        use windows::core::{HSTRING, PCWSTR};
        static UH_OH: &[u8] = include_bytes!("../sounds/uh-oh.wav");
        static BELL: &[u8] = include_bytes!("../sounds/bell.wav");
        static QUACK: &[u8] = include_bytes!("../sounds/quack.wav");
        let bundled = match (sound.source, sound.id) {
            (Source::Bundled, "uh-oh") => Some(UH_OH),
            (Source::Bundled, "bell") => Some(BELL),
            (Source::Bundled, "quack") => Some(QUACK),
            _ => None,
        };
        // SAFETY: SND_MEMORY reads the static bytes for as long as it plays;
        // SND_FILENAME copies the path before returning.
        let played = unsafe {
            if let Some(bytes) = bundled {
                PlaySoundW(PCWSTR(bytes.as_ptr().cast()), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT)
            } else {
                let file = match sound.source {
                    Source::SystemDefault => {
                        path(Sound { id: "Notify", source: Source::System })
                    }
                    _ => path(sound),
                };
                let Some(file) = file else {
                    log::warn!("no audio file for sound {:?}", sound.id);
                    return;
                };
                PlaySoundW(&HSTRING::from(file.as_os_str()), None, SND_FILENAME | SND_ASYNC | SND_NODEFAULT)
            }
        };
        if !played.as_bool() {
            log::warn!("could not play sound {:?}", sound.id);
        }
    }
    #[cfg(not(windows))]
    preview(sound.id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_the_default_exists() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|s| s.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            count,
            "a duplicate id would make the picker ambiguous"
        );
        assert_eq!(resolve(DEFAULT).id, DEFAULT);
    }

    #[test]
    fn an_unknown_id_falls_back_rather_than_silencing() {
        // A pref written by a newer build, or naming a sound since removed,
        // must not turn notifications off — silence reads as a broken app.
        let got = resolve("no-such-sound");
        assert_eq!(got.id, DEFAULT);
        assert_ne!(got.source, Source::Silent);
    }

    #[test]
    fn silent_and_default_have_no_file_name() {
        assert_eq!(file_name(resolve("silent")), None);
        assert_eq!(file_name(resolve("default")), None);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn file_names_carry_the_extension_each_source_uses() {
        assert_eq!(file_name(resolve("uh-oh")).as_deref(), Some("uh-oh.wav"));
        assert_eq!(file_name(resolve("Glass")).as_deref(), Some("Glass.aiff"));
    }

    #[test]
    fn every_system_sound_in_the_catalog_exists_on_this_mac() {
        // Guards against a typo in the list: a misspelled system sound resolves
        // to no file and plays nothing, which is indistinguishable from silence.
        for s in CATALOG.iter().filter(|s| s.source == Source::System) {
            assert!(
                path(*s).is_some(),
                "{} is not among this OS's system sounds",
                s.id
            );
        }
    }
}
