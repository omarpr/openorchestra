//! `remote.json` store: the single source of truth for which tabs are marked
//! for remote control, paired devices and pending pairing codes.
//!
//! Every change is a read-modify-write of the file's *current* contents under
//! an exclusive `flock` on `remote.lock`, written atomically with mode 0600.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use os_compat::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crossbeam_channel::{unbounded, Receiver};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "remote.json";
const LOCK_NAME: &str = "remote.lock";
const BAD_NAME: &str = "remote.json.bad";
const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// A tab marked for remote control. `id` is the opaque handle phones use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub worktree_key: String,
    pub tab_id: u64,
    pub id: String,
}

/// A paired phone/tablet. Timestamps are RFC 3339 UTC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub token_sha256: String,
    pub created: String,
    #[serde(default)]
    pub last_seen: Option<String>,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub origin_host: String,
}

/// A pairing code awaiting redemption (only its hash is stored).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCode {
    pub code_sha256: String,
    pub expires: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteFile {
    pub enabled: bool,
    pub port: u16,
    pub keep_awake: bool,
    pub marked: Vec<Mark>,
    pub devices: Vec<Device>,
    pub pending_codes: Vec<PendingCode>,
    pub failed_pair_attempts: u32,
}

impl Default for RemoteFile {
    fn default() -> Self {
        RemoteFile {
            enabled: false,
            port: 7443,
            keep_awake: true,
            marked: Vec::new(),
            devices: Vec::new(),
            pending_codes: Vec::new(),
            failed_pair_attempts: 0,
        }
    }
}

impl RemoteFile {
    pub fn mark_for(&self, key: &str, tab_id: u64) -> Option<&Mark> {
        self.marked
            .iter()
            .find(|m| m.worktree_key == key && m.tab_id == tab_id)
    }

    pub fn mark_by_id(&self, id: &str) -> Option<&Mark> {
        self.marked.iter().find(|m| m.id == id)
    }
}

pub struct Store {
    dir: PathBuf,
    path: PathBuf,
}

/// Exclusive advisory lock, released on drop (and when the fd closes).
struct LockGuard(File);

impl LockGuard {
    fn acquire(path: &Path) -> io::Result<LockGuard> {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(path)?;
        // std's portable advisory lock: flock on Unix, LockFileEx on Windows.
        f.lock()?;
        Ok(LockGuard(f))
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    if !dir.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    Ok(())
}

/// Write `bytes` to `path` via a temp file + rename, mode 0600. The parent
/// directory is created 0700 if missing.
pub(crate) fn write_atomic_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_private_dir(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        // Enforce 0600 even if the temp file pre-existed with other bits.
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub(crate) fn new_id(prefix: &str) -> io::Result<String> {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).map_err(|e| io::Error::other(e.to_string()))?;
    let mut id = String::from(prefix);
    id.extend(buf.iter().map(|b| BASE32[(*b & 31) as usize] as char));
    Ok(id)
}

/// Remove a stored screenshot unless delivery may have made its path visible to the agent.
pub(crate) struct PendingScreenshot {
    path: PathBuf,
    pub(crate) retain: bool,
}

impl PendingScreenshot {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PendingScreenshot {
    fn drop(&mut self) {
        if !self.retain {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(crate) fn save_screenshot(
    state_dir: &Path,
    extension: &str,
    bytes: &[u8],
) -> io::Result<PendingScreenshot> {
    const MAX_STORAGE: u64 = 256 * 1024 * 1024;
    const MAX_FILES: usize = 512;
    const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

    let dir = state_dir.join("remote/screenshots");
    ensure_private_dir(&dir)?;
    let _lock = LockGuard::acquire(&dir.join(".lock"))?;
    let now = SystemTime::now();
    let mut used = 0u64;
    let mut files = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".lock" {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        let old = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= RETENTION);
        if old && fs::remove_file(entry.path()).is_ok() {
            continue;
        }
        used = used.saturating_add(metadata.len());
        files += 1;
    }
    if files >= MAX_FILES || used.saturating_add(bytes.len() as u64) > MAX_STORAGE {
        return Err(io::ErrorKind::StorageFull.into());
    }
    let path = dir.join(new_id("screenshot_")?).with_extension(extension);
    write_atomic_private(&path, bytes)?;
    Ok(PendingScreenshot {
        path,
        retain: false,
    })
}

impl Store {
    pub fn new(state_dir: &Path) -> Store {
        Store {
            dir: state_dir.to_path_buf(),
            path: state_dir.join(FILE_NAME),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Current contents; default when missing or unparseable. Never writes.
    pub fn load(&self) -> RemoteFile {
        fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Current contents, distinguishing a bad read: `Some(default)` when the
    /// file doesn't exist, `None` when it can't be read or parsed (e.g. a
    /// partial write seen mid-replace, a permission glitch). Never writes.
    pub fn try_load(&self) -> Option<RemoteFile> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).ok(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Some(RemoteFile::default()),
            Err(_) => None,
        }
    }

    /// Read-modify-write under an exclusive lock. A corrupt file is moved to
    /// `remote.json.bad` and the update starts from the default.
    pub fn update<R>(&self, f: impl FnOnce(&mut RemoteFile) -> R) -> io::Result<R> {
        ensure_private_dir(&self.dir)?;
        let _lock = LockGuard::acquire(&self.dir.join(LOCK_NAME))?;
        let mut file = match fs::read(&self.path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(parsed) => parsed,
                Err(_) => {
                    fs::rename(&self.path, self.dir.join(BAD_NAME))?;
                    RemoteFile::default()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => RemoteFile::default(),
            Err(e) => return Err(e),
        };
        let out = f(&mut file);
        let bytes = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
        write_atomic_private(&self.path, &bytes)?;
        Ok(out)
    }

    /// Mark a tab for remote control; returns its id (existing id if already marked).
    pub fn mark(&self, key: &str, tab_id: u64) -> io::Result<String> {
        let new_id = new_id("r_")?;
        self.update(|f| {
            if let Some(m) = f.mark_for(key, tab_id) {
                return m.id.clone();
            }
            f.marked.push(Mark {
                worktree_key: key.to_string(),
                tab_id,
                id: new_id.clone(),
            });
            new_id
        })
    }

    /// Unmark one tab, or every tab of `key` when `tab_id` is `None`.
    pub fn unmark(&self, key: &str, tab_id: Option<u64>) -> io::Result<()> {
        self.update(|f| {
            f.marked
                .retain(|m| !(m.worktree_key == key && tab_id.is_none_or(|t| m.tab_id == t)));
        })
    }

    /// Watch the state directory (atomic renames replace the inode) and emit
    /// `()` whenever `remote.json` changes.
    pub fn watch(&self) -> io::Result<(RecommendedWatcher, Receiver<()>)> {
        ensure_private_dir(&self.dir)?;
        let (tx, rx) = unbounded::<()>();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                if event
                    .paths
                    .iter()
                    .any(|p| p.file_name().is_some_and(|n| n == FILE_NAME))
                {
                    let _ = tx.send(());
                }
            }
        })
        .map_err(io::Error::other)?;
        watcher
            .watch(&self.dir, RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;
        Ok((watcher, rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    fn device(n: usize, tag: &str) -> Device {
        Device {
            id: format!("d_{tag}_{n}"),
            name: format!("phone {n}"),
            token_sha256: "00".into(),
            created: "2026-01-01T00:00:00Z".into(),
            last_seen: None,
            read_only: false,
            origin_host: "mac.local".into(),
        }
    }

    #[test]
    fn default_when_missing() {
        let tmp = TempDir::new().unwrap();
        let s = Store::new(tmp.path());
        assert_eq!(s.load(), RemoteFile::default());
        assert_eq!(s.load().port, 7443);
    }

    #[test]
    fn mark_is_idempotent_and_persists() {
        let tmp = TempDir::new().unwrap();
        let a = Store::new(tmp.path()).mark("main:a", 3).unwrap();
        let b = Store::new(tmp.path()).mark("main:a", 3).unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with("r_"));
        assert_eq!(a.len(), 18);
        let f = Store::new(tmp.path()).load();
        assert_eq!(f.marked.len(), 1);
        assert_eq!(f.mark_by_id(&a).unwrap().tab_id, 3);
        assert_eq!(f.mark_for("main:a", 3).unwrap().id, a);
    }

    #[test]
    fn unmark_tab_and_whole_key() {
        let tmp = TempDir::new().unwrap();
        let s = Store::new(tmp.path());
        s.mark("k", 1).unwrap();
        s.mark("k", 2).unwrap();
        s.mark("j", 1).unwrap();
        s.unmark("k", Some(1)).unwrap();
        let f = s.load();
        assert!(f.mark_for("k", 1).is_none());
        assert!(f.mark_for("k", 2).is_some());
        s.unmark("k", None).unwrap();
        let f = s.load();
        assert_eq!(f.marked.len(), 1);
        assert!(f.mark_for("j", 1).is_some());
    }

    #[test]
    fn file_is_private() {
        let tmp = TempDir::new().unwrap();
        let s = Store::new(&tmp.path().join("state"));
        s.mark("k", 1).unwrap();
        let mode = fs::metadata(s.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        #[cfg(unix)] // Windows has no folder modes (os-compat reports any path as 0o600)
        {
            let dmode = fs::metadata(tmp.path().join("state"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(dmode & 0o777, 0o700);
        }
    }

    #[test]
    fn corrupt_file_is_quarantined() {
        let tmp = TempDir::new().unwrap();
        let s = Store::new(tmp.path());
        fs::write(s.path(), "{not json").unwrap();
        assert_eq!(s.load(), RemoteFile::default());
        s.update(|f| f.port = 9000).unwrap();
        assert_eq!(fs::read(tmp.path().join(BAD_NAME)).unwrap(), b"{not json");
        assert_eq!(s.load().port, 9000);
    }

    #[test]
    fn concurrent_updates_both_land() {
        let tmp = TempDir::new().unwrap();
        let handles: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|tag| {
                let dir = tmp.path().to_path_buf();
                std::thread::spawn(move || {
                    let s = Store::new(&dir);
                    for n in 0..50 {
                        s.update(|f| f.devices.push(device(n, tag))).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(Store::new(tmp.path()).load().devices.len(), 100);
    }

    #[test]
    fn update_reads_current_file() {
        let tmp = TempDir::new().unwrap();
        let a = Store::new(tmp.path());
        let b = Store::new(tmp.path());
        a.update(|f| f.devices.push(device(0, "x"))).unwrap();
        let _stale = a.load();
        b.update(|f| f.devices.clear()).unwrap();
        a.update(|f| f.keep_awake = false).unwrap();
        let f = a.load();
        assert!(f.devices.is_empty());
        assert!(!f.keep_awake);
    }

    #[test]
    fn watch_fires_on_external_write() {
        let tmp = TempDir::new().unwrap();
        let s = Store::new(tmp.path());
        let (_w, rx) = s.watch().unwrap();
        Store::new(tmp.path()).mark("k", 1).unwrap();
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }
}
