//! Persisted broker credentials so API keys (and preferred ports) survive restarts.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::generate_api_key;

const FILE_NAME: &str = "credentials.json";

/// On-disk broker credentials for one openOrchestra task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrokerCredentials {
    pub api_key: String,
    /// Last successfully bound loopback port, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl BrokerCredentials {
    pub fn path(a2a_dir: &Path) -> PathBuf {
        a2a_dir.join(FILE_NAME)
    }

    /// Load existing credentials, or create a new API key and write it.
    pub fn load_or_create(a2a_dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(a2a_dir)?;
        let path = Self::path(a2a_dir);
        if path.exists() {
            let text = fs::read_to_string(&path)?;
            if let Ok(creds) = serde_json::from_str::<BrokerCredentials>(&text) {
                if !creds.api_key.is_empty() {
                    Self::tighten_existing(a2a_dir);
                    return Ok(creds);
                }
            }
        }
        let creds = BrokerCredentials {
            api_key: generate_api_key(),
            port: None,
        };
        creds.save(a2a_dir)?;
        Ok(creds)
    }

    /// Write the key readable by this user only (`0600`, in a `0700` folder),
    /// via a temp file and rename so a reader never sees a half-written key.
    ///
    /// The key is the broker's only authentication, and whoever holds it can
    /// type messages into this task's agents. Written with default permissions
    /// it was readable by every account on the Mac, since home folders are
    /// world-traversable by default.
    pub fn save(&self, a2a_dir: &Path) -> io::Result<()> {
        fs::create_dir_all(a2a_dir)?;
        restrict_dir(a2a_dir)?;
        let path = Self::path(a2a_dir);
        let body = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let tmp = a2a_dir.join(format!(".{FILE_NAME}.tmp"));
        let _ = fs::remove_file(&tmp);
        {
            use std::io::Write;
            let mut f = private_file(&tmp)?;
            f.write_all(&body)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path)
    }
}

#[cfg(unix)]
fn private_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(unix)]
fn restrict_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

impl BrokerCredentials {
    /// Re-tighten a key file written by an older build with default
    /// permissions. Called on load so existing installs are fixed in place.
    fn tighten_existing(a2a_dir: &Path) {
        let _ = restrict_dir(a2a_dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(Self::path(a2a_dir), fs::Permissions::from_mode(0o600));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_or_create_reuses_key() {
        let dir = tempfile::tempdir().unwrap();
        let a = BrokerCredentials::load_or_create(dir.path()).unwrap();
        let b = BrokerCredentials::load_or_create(dir.path()).unwrap();
        assert_eq!(a.api_key, b.api_key);
        assert!(!a.api_key.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_key_is_readable_by_this_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let a2a = dir.path().join("a2a");
        BrokerCredentials::load_or_create(&a2a).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&BrokerCredentials::path(&a2a)), 0o600);
        assert_eq!(mode(&a2a), 0o700);
    }

    #[test]
    fn save_round_trips_port() {
        let dir = tempfile::tempdir().unwrap();
        let mut creds = BrokerCredentials::load_or_create(dir.path()).unwrap();
        creds.port = Some(41234);
        creds.save(dir.path()).unwrap();
        let loaded = BrokerCredentials::load_or_create(dir.path()).unwrap();
        assert_eq!(loaded.port, Some(41234));
        assert_eq!(loaded.api_key, creds.api_key);
    }
}
