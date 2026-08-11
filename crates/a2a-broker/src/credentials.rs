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

    pub fn save(&self, a2a_dir: &Path) -> io::Result<()> {
        fs::create_dir_all(a2a_dir)?;
        let path = Self::path(a2a_dir);
        let body = serde_json::to_vec_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        fs::write(path, body)
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
