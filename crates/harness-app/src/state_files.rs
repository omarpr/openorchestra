//! Private, atomic writes for the app's state under `~/.openorchestra`.
//!
//! macOS home folders are world-traversable by default (`drwxr-xr-x`), so
//! files written with default permissions there are readable by every other
//! account on the Mac. The state folder holds project paths, review comments,
//! the agent launch overrides, and per-task broker keys, so it is kept to this
//! user: the folder at `0700`, and files at `0600`.
//!
//! Writes go through a temp file and a rename, so a crash mid-write never
//! leaves a truncated `prefs.json` that silently resets every setting.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Restrict the state folder to this user. Called at startup, which also
/// fixes installs whose folder was created with default permissions.
pub fn secure_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        // State files an older build wrote world-readable: tighten them now
        // rather than waiting for each one's next save. Top level only — the
        // subfolders hold worktrees, whose files keep their own modes.
        for entry in fs::read_dir(dir)?.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_file()) {
                let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o600));
            }
        }
    }
    Ok(())
}

/// Write `bytes` to `path` readable by this user only, atomically.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state".into());
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&tmp);
    {
        let mut f = open_private(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

#[cfg(unix)]
fn open_private(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    // create_new: never write through something already sitting at the temp
    // path, such as a symlink planted there.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn state_is_private_and_replaces_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".openorchestra");
        secure_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);

        let f = dir.join("prefs.json");
        fs::write(&f, "old").unwrap(); // an older build's world-readable file
        write_private(&f, b"new").unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "new");
        assert_eq!(
            mode(&f),
            0o600,
            "rewritten files must not keep the old mode"
        );
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
