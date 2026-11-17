//! Pairing codes, device tokens and device management over `remote.json`.
//! Raw codes and tokens are never stored: only their SHA-256 hex.

use std::io;
use std::net::IpAddr;
use std::time::{Duration, SystemTime};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::store::new_id;
use crate::{Device, PendingCode, RemoteFile, Store};

const CODE_ALPHABET: &[u8; 31] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
const CODE_TTL: Duration = Duration::from_secs(10 * 60);
/// Wrong codes (from any address) that void the open codes. A ceiling against
/// brute force across many addresses; one address is stopped far sooner by
/// the per-address lock in the server.
const MAX_FAILED: u32 = 50;
const TOUCH_INTERVAL: Duration = Duration::from_secs(60);
/// A device unused this long (no request at all) must pair again.
pub const DEVICE_IDLE_LIMIT: Duration = Duration::from_secs(30 * 86_400);

#[derive(Debug)]
pub enum PairError {
    Invalid,
    Locked,
    Io(io::Error),
}

impl From<io::Error> for PairError {
    fn from(e: io::Error) -> Self {
        PairError::Io(e)
    }
}

fn fill(buf: &mut [u8]) -> io::Result<()> {
    getrandom::fill(buf).map_err(|e| io::Error::other(e.to_string()))
}

fn fmt_time(t: SystemTime) -> String {
    OffsetDateTime::from(t)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

fn parse_time(s: &str) -> Option<SystemTime> {
    OffsetDateTime::parse(s, &Rfc3339)
        .ok()
        .map(SystemTime::from)
}

/// 32 random bytes, base64url without padding.
pub fn new_token() -> String {
    let mut b = [0u8; 32];
    fill(&mut b).expect("system randomness unavailable");
    URL_SAFE_NO_PAD.encode(b)
}

pub fn sha256_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Uppercase, dropping `-` and whitespace.
pub fn normalize_code(input: &str) -> String {
    input
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .flat_map(char::to_uppercase)
        .collect()
}

/// Constant-time equality (length is not secret: both are sha256 hex).
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn random_code() -> io::Result<String> {
    let mut out = String::with_capacity(9);
    let mut n = 0;
    while n < 8 {
        let mut buf = [0u8; 16];
        fill(&mut buf)?;
        for b in buf {
            // Rejection sampling: 248 = 31 * 8, so no modulo bias.
            if b < 248 && n < 8 {
                if n == 4 {
                    out.push('-');
                }
                out.push(CODE_ALPHABET[(b % 31) as usize] as char);
                n += 1;
            }
        }
    }
    Ok(out)
}

fn is_expired(p: &PendingCode, now: SystemTime) -> bool {
    parse_time(&p.expires).is_none_or(|e| now > e)
}

/// New single-use code valid for 10 minutes; prunes expired codes and resets
/// the failure counter.
pub fn issue_code(store: &Store, now: SystemTime) -> io::Result<String> {
    let code = random_code()?;
    let hash = sha256_hex(&normalize_code(&code));
    let expires = fmt_time(now + CODE_TTL);
    store.update(|f| {
        f.pending_codes.retain(|p| !is_expired(p, now));
        f.pending_codes.push(PendingCode {
            code_sha256: hash,
            expires,
        });
        f.failed_pair_attempts = 0;
    })?;
    Ok(code)
}

/// Redeem a pairing code, creating a device. Returns the device and its raw
/// token (shown once; only the hash is stored).
pub fn redeem_code(
    store: &Store,
    code: &str,
    device_name: &str,
    origin_host: &str,
    now: SystemTime,
) -> Result<(Device, String), PairError> {
    let hash = sha256_hex(&normalize_code(code));
    let token = new_token();
    let device = Device {
        id: new_id("d_")?,
        name: device_name.to_string(),
        token_sha256: sha256_hex(&token),
        created: fmt_time(now),
        last_seen: None,
        read_only: false,
        origin_host: origin_host.to_string(),
    };
    store.update(|f| {
        if f.failed_pair_attempts >= MAX_FAILED {
            f.pending_codes.clear();
            return Err(PairError::Locked);
        }
        f.pending_codes.retain(|p| !is_expired(p, now));
        // Scan every code (no early exit) to keep timing independent of the match.
        let mut found = None;
        for (i, p) in f.pending_codes.iter().enumerate() {
            if ct_eq(&p.code_sha256, &hash) {
                found = Some(i);
            }
        }
        match found {
            Some(i) => {
                f.pending_codes.remove(i);
                f.failed_pair_attempts = 0;
                f.devices.push(device.clone());
                Ok(())
            }
            None => {
                f.failed_pair_attempts += 1;
                if f.failed_pair_attempts >= MAX_FAILED {
                    f.pending_codes.clear();
                }
                Err(PairError::Invalid)
            }
        }
    })??;
    Ok((device, token))
}

/// The device owning `token`, compared in constant time across all devices.
pub fn authenticate<'a>(file: &'a RemoteFile, token: &str) -> Option<&'a Device> {
    authenticate_at(file, token, SystemTime::now())
}

/// [`authenticate`] at `now`: a device idle past [`DEVICE_IDLE_LIMIT`] is
/// refused (it stays listed, marked expired, until revoked).
pub fn authenticate_at<'a>(
    file: &'a RemoteFile,
    token: &str,
    now: SystemTime,
) -> Option<&'a Device> {
    let hash = sha256_hex(token);
    let mut found = None;
    for d in &file.devices {
        if ct_eq(&d.token_sha256, &hash) {
            found = Some(d);
        }
    }
    found.filter(|d| !is_idle_expired(d, now))
}

/// Whether `d` went unused for [`DEVICE_IDLE_LIMIT`] by `now`, counting from
/// its last request (or pairing, if it never made one). A clock that reads
/// earlier than the record expires nothing.
pub fn is_idle_expired(d: &Device, now: SystemTime) -> bool {
    let last = d
        .last_seen
        .as_deref()
        .and_then(parse_time)
        .or_else(|| parse_time(&d.created));
    last.is_some_and(|t| {
        now.duration_since(t)
            .is_ok_and(|idle| idle >= DEVICE_IDLE_LIMIT)
    })
}

/// Record activity; writes at most once per 60s per device.
pub fn touch_last_seen(store: &Store, device_id: &str, now: SystemTime) -> io::Result<()> {
    let fresh = |f: &RemoteFile| {
        f.devices
            .iter()
            .find(|d| d.id == device_id)
            .and_then(|d| d.last_seen.as_deref().and_then(parse_time))
            .is_some_and(|t| now.duration_since(t).is_ok_and(|age| age < TOUCH_INTERVAL))
    };
    if fresh(&store.load()) {
        return Ok(());
    }
    // Unknown or revoked device: nothing to record, so don't rewrite the file.
    if !store.load().devices.iter().any(|d| d.id == device_id) {
        return Ok(());
    }
    store.update(|f| {
        if fresh(f) {
            return;
        }
        if let Some(d) = f.devices.iter_mut().find(|d| d.id == device_id) {
            d.last_seen = Some(fmt_time(now));
        }
    })
}

/// Void every pairing code not yet used (the code's dialog closed).
pub fn withdraw_codes(store: &Store) -> io::Result<()> {
    store.update(|f| f.pending_codes.clear())
}

pub fn revoke(store: &Store, device_id: &str) -> io::Result<()> {
    store.update(|f| f.devices.retain(|d| d.id != device_id))
}

pub fn set_read_only(store: &Store, device_id: &str, read_only: bool) -> io::Result<()> {
    store.update(|f| {
        if let Some(d) = f.devices.iter_mut().find(|d| d.id == device_id) {
            d.read_only = read_only;
        }
    })
}

/// Bare IP from an origin host that may carry a port, brackets or a zone.
fn host_ip(host: &str) -> Option<IpAddr> {
    let h = host.trim();
    let h = match h.strip_prefix('[') {
        Some(rest) => rest.split(']').next()?,
        None if h.matches(':').count() == 1 => h.split(':').next()?,
        None => h,
    };
    h.split('%').next()?.parse().ok()
}

/// Names of devices whose `origin_host` is an IP that is no longer one of
/// this machine's addresses (their pinned URL is stale).
pub fn devices_on_stale_ip(file: &RemoteFile, current_ips: &[IpAddr]) -> Vec<String> {
    file.devices
        .iter()
        .filter(|d| host_ip(&d.origin_host).is_some_and(|ip| !current_ips.contains(&ip)))
        .map(|d| d.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let s = Store::new(tmp.path());
        (tmp, s)
    }

    #[test]
    fn code_format() {
        let (_t, s) = setup();
        let c = issue_code(&s, SystemTime::now()).unwrap();
        assert_eq!(c.len(), 9);
        for (i, ch) in c.chars().enumerate() {
            if i == 4 {
                assert_eq!(ch, '-');
            } else {
                assert!(CODE_ALPHABET.contains(&(ch as u8)), "{c}");
            }
        }
    }

    #[test]
    fn redeem_once() {
        let (_t, s) = setup();
        let now = SystemTime::now();
        let c = issue_code(&s, now).unwrap();
        let variant = c.replace('-', "").to_lowercase();
        let (dev, token) = redeem_code(&s, &variant, "iPhone", "mac.local", now).unwrap();
        assert!(matches!(
            redeem_code(&s, &c, "x", "mac.local", now),
            Err(PairError::Invalid)
        ));
        let f = s.load();
        assert_eq!(f.devices.len(), 1);
        assert_eq!(f.devices[0].token_sha256, sha256_hex(&token));
        assert_eq!(f.devices[0].id, dev.id);
        assert!(dev.id.starts_with("d_") && dev.id.len() == 18);
        assert_eq!(dev.origin_host, "mac.local");
        let raw = std::fs::read_to_string(s.path()).unwrap();
        assert!(!raw.contains(&token));
        assert!(!raw.contains(&c));
        assert!(!raw.contains(&c.replace('-', "")));
    }

    #[test]
    fn code_expires() {
        let (_t, s) = setup();
        let now = SystemTime::now();
        let c = issue_code(&s, now).unwrap();
        let late = now + Duration::from_secs(10 * 60 + 1);
        assert!(matches!(
            redeem_code(&s, &c, "x", "h", late),
            Err(PairError::Invalid)
        ));
    }

    #[test]
    fn devices_unused_for_30_days_are_refused() {
        let (_t, s) = setup();
        let paired = SystemTime::now();
        let c = issue_code(&s, paired).unwrap();
        let (_, token) = redeem_code(&s, &c, "x", "h", paired).unwrap();
        let f = s.load();
        let day = Duration::from_secs(86_400);
        assert!(authenticate_at(&f, &token, paired + 29 * day).is_some());
        assert!(authenticate_at(&f, &token, paired + 30 * day).is_none());
        // Use resets the clock.
        touch_last_seen(&s, &f.devices[0].id, paired + 29 * day).unwrap();
        assert!(authenticate_at(&s.load(), &token, paired + 58 * day).is_some());
        // A clock running behind the records never expires anyone.
        assert!(authenticate_at(&s.load(), &token, paired - day).is_some());
    }

    #[test]
    fn fifty_failures_void_codes() {
        let (_t, s) = setup();
        let now = SystemTime::now();
        let c = issue_code(&s, now).unwrap();
        for _ in 0..MAX_FAILED {
            assert!(matches!(
                redeem_code(&s, "AAAA-AAAA", "x", "h", now),
                Err(PairError::Invalid)
            ));
        }
        assert!(matches!(
            redeem_code(&s, &c, "x", "h", now),
            Err(PairError::Locked)
        ));
        assert!(s.load().pending_codes.is_empty());
        let c2 = issue_code(&s, now).unwrap();
        assert_eq!(s.load().failed_pair_attempts, 0);
        assert!(redeem_code(&s, &c2, "x", "h", now).is_ok());
    }

    #[test]
    fn code_valid_just_before_expiry() {
        let (_t, s) = setup();
        let now = SystemTime::now();
        let c = issue_code(&s, now).unwrap();
        let t = now + Duration::from_secs(9 * 60 + 59);
        assert!(redeem_code(&s, &c, "x", "h", t).is_ok());
    }

    #[test]
    fn touch_unknown_device_does_not_write() {
        let (_t, s) = setup();
        let now = SystemTime::now();
        let c = issue_code(&s, now).unwrap();
        redeem_code(&s, &c, "p", "h", now).unwrap();
        let before = std::fs::read(s.path()).unwrap();
        let m0 = std::fs::metadata(s.path()).unwrap().modified().unwrap();
        touch_last_seen(&s, "d_missing", now).unwrap();
        assert_eq!(std::fs::read(s.path()).unwrap(), before);
        assert_eq!(std::fs::metadata(s.path()).unwrap().modified().unwrap(), m0);
    }

    #[test]
    fn authenticate_and_revoke() {
        let (_t, s) = setup();
        let now = SystemTime::now();
        let c = issue_code(&s, now).unwrap();
        let (dev, token) = redeem_code(&s, &c, "p", "h", now).unwrap();
        assert_eq!(authenticate(&s.load(), &token).unwrap().id, dev.id);
        assert!(authenticate(&s.load(), "nope").is_none());
        revoke(&s, &dev.id).unwrap();
        assert!(authenticate(&s.load(), &token).is_none());
    }

    #[test]
    fn read_only_flag_roundtrip() {
        let (_t, s) = setup();
        let now = SystemTime::now();
        let c = issue_code(&s, now).unwrap();
        let (dev, _) = redeem_code(&s, &c, "p", "h", now).unwrap();
        set_read_only(&s, &dev.id, true).unwrap();
        assert!(s.load().devices[0].read_only);
        set_read_only(&s, &dev.id, false).unwrap();
        assert!(!s.load().devices[0].read_only);
    }

    #[test]
    fn touch_last_seen_throttles() {
        let (_t, s) = setup();
        let now = SystemTime::now();
        let c = issue_code(&s, now).unwrap();
        let (dev, _) = redeem_code(&s, &c, "p", "h", now).unwrap();
        touch_last_seen(&s, &dev.id, now).unwrap();
        let first = s.load().devices[0].last_seen.clone().unwrap();
        touch_last_seen(&s, &dev.id, now + Duration::from_secs(30)).unwrap();
        assert_eq!(
            s.load().devices[0].last_seen.as_deref(),
            Some(first.as_str())
        );
        touch_last_seen(&s, &dev.id, now + Duration::from_secs(61)).unwrap();
        assert_ne!(
            s.load().devices[0].last_seen.as_deref(),
            Some(first.as_str())
        );
    }

    #[test]
    fn devices_on_stale_ip_lists_changed() {
        let (_t, s) = setup();
        s.update(|f| {
            for (n, h) in [
                ("a", "mac.local"),
                ("b", "192.168.1.20"),
                ("c", "192.168.1.30"),
                ("d", "192.168.1.30:8443"),
                ("e", "[fe80::1]"),
                ("f", "fe80::1%en0"),
                ("g", "[fe80::2]:7443"),
                ("h", "192.168.1.20:7443"),
            ] {
                f.devices.push(Device {
                    id: format!("d_{n}"),
                    name: n.into(),
                    token_sha256: "00".into(),
                    created: "2026-01-01T00:00:00Z".into(),
                    last_seen: None,
                    read_only: false,
                    origin_host: h.into(),
                });
            }
        })
        .unwrap();
        let ips: Vec<IpAddr> = vec!["192.168.1.20".parse().unwrap()];
        assert_eq!(
            devices_on_stale_ip(&s.load(), &ips),
            vec![
                "c".to_string(),
                "d".to_string(),
                "e".to_string(),
                "f".to_string(),
                "g".to_string()
            ]
        );
    }
}
