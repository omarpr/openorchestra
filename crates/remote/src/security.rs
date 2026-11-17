//! `remote/security.json`: settings an older build must not erase. It lives
//! apart from `remote.json` because builds without a field rewrite that file
//! without it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::network::TrustedNet;
use crate::store::write_atomic_private;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityFile {
    /// Networks devices may connect from (see `crate::network`).
    pub trusted_networks: Vec<TrustedNet>,
    /// Set once the first build with trusted networks has run: devices paired
    /// before then were allowed in from the network the Mac was on.
    pub migrated: bool,
}

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join("remote").join("security.json")
}

/// The file, or `None` when missing or unreadable (callers then keep what
/// they had and don't migrate twice on a bad read).
pub fn load(state_dir: &Path) -> Option<SecurityFile> {
    let bytes = fs::read(path(state_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save(state_dir: &Path, f: &SecurityFile) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(f).map_err(io::Error::other)?;
    write_atomic_private(&path(state_dir), &bytes)
}

/// Read, change and write the file (starting from the default when missing).
pub fn update<R>(state_dir: &Path, f: impl FnOnce(&mut SecurityFile) -> R) -> io::Result<R> {
    let mut file = match fs::read(path(state_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => SecurityFile::default(),
        Err(e) => return Err(e),
    };
    let out = f(&mut file);
    save(state_dir, &file)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn round_trips_privately() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), None);
        update(dir.path(), |f| {
            f.migrated = true;
            f.trusted_networks.push(TrustedNet {
                key: "192.168.1.0/24 via 192.168.1.1".into(),
                added: "2026-11-16T00:00:00Z".into(),
            });
        })
        .unwrap();
        let f = load(dir.path()).unwrap();
        assert!(f.migrated);
        assert_eq!(f.trusted_networks.len(), 1);
        let mode = fs::metadata(path(dir.path())).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_corrupt_file_is_not_overwritten_by_an_update() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("remote")).unwrap();
        fs::write(path(dir.path()), "{not json").unwrap();
        assert!(update(dir.path(), |f| f.migrated = true).is_err());
        assert_eq!(fs::read_to_string(path(dir.path())).unwrap(), "{not json");
    }
}
