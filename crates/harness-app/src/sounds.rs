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
pub const CATALOG: &[Sound] = &[
    Sound { id: "uh-oh", source: Source::Bundled },
    Sound { id: "bell", source: Source::Bundled },
    Sound { id: "quack", source: Source::Bundled },
    Sound { id: "default", source: Source::SystemDefault },
    Sound { id: "silent", source: Source::Silent },
    Sound { id: "Basso", source: Source::System },
    Sound { id: "Blow", source: Source::System },
    Sound { id: "Bottle", source: Source::System },
    Sound { id: "Frog", source: Source::System },
    Sound { id: "Funk", source: Source::System },
    Sound { id: "Glass", source: Source::System },
    Sound { id: "Hero", source: Source::System },
    Sound { id: "Morse", source: Source::System },
    Sound { id: "Ping", source: Source::System },
    Sound { id: "Pop", source: Source::System },
    Sound { id: "Purr", source: Source::System },
    Sound { id: "Sosumi", source: Source::System },
    Sound { id: "Submarine", source: Source::System },
    Sound { id: "Tink", source: Source::System },
];

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
        .unwrap_or_else(|| resolve_default())
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
    match sound.source {
        Source::System => {
            let p = std::path::PathBuf::from(format!(
                "/System/Library/Sounds/{}.aiff",
                sound.id
            ));
            p.exists().then_some(p)
        }
        Source::Bundled => {
            let file = format!("{}.wav", sound.id);
            let bundled = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|d| d.join("../Resources").join(&file)));
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
        Ok(mut child) => {
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
        Err(e) => log::warn!("could not run afplay for {id:?} ({}): {e}", target.display()),
    }
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
        assert_eq!(ids.len(), count, "a duplicate id would make the picker ambiguous");
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
                "{} is not in /System/Library/Sounds",
                s.id
            );
        }
    }
}
