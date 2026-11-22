//! Local CA + server certificate + phone setup artifacts.
//!
//! Phones trust a CA this app generates once (ECDSA P-256, 10 years),
//! name-constrained to `.local`/`localhost` and private/loopback IPv4. The
//! HTTPS server uses a short-lived leaf (<= 397 days, the iOS limit) issued
//! by that CA and reissued when the SAN set changes or < 30 days remain.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use rcgen::string::Ia5String;
use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, Issuer, KeyPair, KeyUsagePurpose,
    NameConstraints, SanType,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::store::write_atomic_private;

const DAY: u64 = 86_400;
const CA_VALID_DAYS: u64 = 3650;
/// Server-cert validity is 397 days total: not_before is backdated 1 day to
/// tolerate phone clock skew, not_after is 396 days ahead.
const SERVER_BACKDATE_DAYS: u64 = 1;
const SERVER_FORWARD_DAYS: u64 = 396;
const REISSUE_WINDOW: Duration = Duration::from_secs(30 * DAY);

pub struct Ca {
    pub cert_pem: String,
    pub cert_der: Vec<u8>,
    key_pem: String,
    /// The names and addresses it may vouch for (its name constraints).
    permitted: Permitted,
}

/// A CA's permitted subtrees: DNS names (each covers itself and its
/// subdomains) and IPv4 ranges as (network, mask).
#[derive(Clone, Debug, Default)]
struct Permitted {
    dns: Vec<String>,
    v4: Vec<(u32, u32)>,
}

impl Ca {
    fn from_parts(cert_pem: String, cert_der: Vec<u8>, key_pem: String) -> io::Result<Ca> {
        let permitted = permitted(&cert_der)?.unwrap_or_default();
        Ok(Ca {
            cert_pem,
            cert_der,
            key_pem,
            permitted,
        })
    }

    pub fn covers_host(&self, host: &str) -> bool {
        let h = host.to_ascii_lowercase();
        self.permitted
            .dns
            .iter()
            .any(|d| h == *d || h.ends_with(&format!(".{d}")))
    }

    pub fn covers_ip(&self, ip: IpAddr) -> bool {
        match ip.to_canonical() {
            IpAddr::V4(v4) => {
                let a = u32::from(v4);
                self.permitted.v4.iter().any(|(net, mask)| a & mask == *net)
            }
            IpAddr::V6(_) => false,
        }
    }

    /// The `hosts` and `ips` this CA may vouch for: a server certificate
    /// naming anything else would fail to verify for every name on it.
    pub fn covered(&self, hosts: &[String], ips: &[IpAddr]) -> (Vec<String>, Vec<IpAddr>) {
        (
            hosts
                .iter()
                .filter(|h| self.covers_host(h))
                .cloned()
                .collect(),
            ips.iter()
                .copied()
                .filter(|ip| self.covers_ip(*ip))
                .collect(),
        )
    }

    /// Whether some of `hosts`/`ips` (this Mac's names and addresses now)
    /// fall outside the CA: it was made before a rename or an address change.
    pub fn outdated_for(&self, hosts: &[String], ips: &[IpAddr]) -> bool {
        hosts.iter().any(|h| !self.covers_host(h)) || ips.iter().any(|ip| !self.covers_ip(*ip))
    }
}

/// The CA's permitted subtrees, or `None` without a name-constraints
/// extension.
fn permitted(der: &[u8]) -> io::Result<Option<Permitted>> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).map_err(invalid)?;
    let Some(nc) = cert.name_constraints().map_err(invalid)? else {
        return Ok(None);
    };
    let mut p = Permitted::default();
    for st in nc.value.permitted_subtrees.iter().flatten() {
        match &st.base {
            x509_parser::extensions::GeneralName::DNSName(d) => p.dns.push(d.to_ascii_lowercase()),
            x509_parser::extensions::GeneralName::IPAddress(b) if b.len() == 8 => {
                let net = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                let mask = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
                p.v4.push((net, mask));
            }
            _ => {}
        }
    }
    Ok(Some(p))
}

/// A CA from before constraints were narrowed to this Mac: none at all, or
/// every `.local` name and private range. Replaced once (kept as `.bak`).
fn is_legacy(der: &[u8]) -> io::Result<bool> {
    Ok(match permitted(der)? {
        None => true,
        Some(p) => p.dns.iter().any(|d| d == "local"),
    })
}

pub struct ServerCert {
    pub cert_pem: String,
    pub key_pem: String,
    pub hosts: Vec<String>,
    pub ips: Vec<IpAddr>,
    pub not_after: SystemTime,
}

fn err<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}

fn invalid<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

fn odt(t: SystemTime) -> OffsetDateTime {
    OffsetDateTime::from(t)
}

fn pem_to_der(pem: &str) -> io::Result<Vec<u8>> {
    let (_, p) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).map_err(invalid)?;
    Ok(p.contents)
}

fn issuer_for(ca: &Ca) -> io::Result<Issuer<'static, KeyPair>> {
    let key = KeyPair::from_pem(&ca.key_pem).map_err(invalid)?;
    Issuer::from_ca_cert_pem(&ca.cert_pem, key).map_err(invalid)
}

/// What a phone's trust in this CA is limited to: `.local` / `localhost`
/// names and private, link-local and loopback IPv4. A phone trusting the CA
/// (and anything that reads `ca.key`, which any process of this user can)
/// then can't vouch for any public site. rcgen marks the extension critical.
fn ca_name_constraints(hosts: &[String], ips: &[IpAddr]) -> NameConstraints {
    let one = |ip: IpAddr| GeneralSubtree::IpAddress(CidrSubnet::from_addr_prefix(ip, 32));
    let mut permitted: Vec<GeneralSubtree> = hosts
        .iter()
        .map(|h| GeneralSubtree::DnsName(h.to_ascii_lowercase()))
        .collect();
    if !hosts.iter().any(|h| h.eq_ignore_ascii_case("localhost")) {
        permitted.push(GeneralSubtree::DnsName("localhost".into()));
    }
    let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
    permitted.extend(
        ips.iter()
            .copied()
            .filter(|ip| ip.is_ipv4() && *ip != loopback)
            .map(one),
    );
    permitted.push(one(loopback));
    NameConstraints {
        permitted_subtrees: permitted,
        excluded_subtrees: Vec::new(),
    }
}

/// Load `ca.crt`/`ca.key` from `dir`, generating them on first use. Existing
/// but unreadable files are an error (never silently replaced: phones already
/// trust the old CA). The one exception: a readable CA from before name
/// constraints (an unreleased build; no phone trusts it yet) is replaced
/// once, logged, by a constrained one; the server cert follows (it records
/// the CA fingerprint it was issued under).
///
/// A new CA vouches only for `hosts` (this Mac's `.local` name) and `ips`
/// (its addresses now), plus localhost. It is never replaced because those
/// change later ([`Ca::outdated_for`] tells; [`retire_ca`] replaces it on
/// request); only a legacy one is, once, keeping it as `ca.*.bak`.
pub fn load_or_create_ca(
    dir: &Path,
    computer_name: &str,
    hosts: &[String],
    ips: &[IpAddr],
) -> io::Result<Ca> {
    let crt = dir.join("ca.crt");
    let key = dir.join("ca.key");
    match (fs::read_to_string(&crt), fs::read_to_string(&key)) {
        (Ok(cert_pem), Ok(key_pem)) => {
            let cert_der = pem_to_der(&cert_pem)?;
            KeyPair::from_pem(&key_pem).map_err(invalid)?;
            if !is_legacy(&cert_der)? {
                return Ca::from_parts(cert_pem, cert_der, key_pem);
            }
            eprintln!(
                "oo-remote: replacing the remote CA with one limited to this Mac \
                 (the old one is kept as ca.crt.bak; re-add the app on devices that trusted it)"
            );
            retire_ca(dir)?;
        }
        (Err(a), Err(b))
            if a.kind() == io::ErrorKind::NotFound && b.kind() == io::ErrorKind::NotFound => {}
        (Err(e), _) | (_, Err(e)) => return Err(e),
    }
    let now = SystemTime::now();
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(
        DnType::CommonName,
        format!("openOrchestra Remote CA ({computer_name})"),
    );
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.name_constraints = Some(ca_name_constraints(hosts, ips));
    params.not_before = odt(now - Duration::from_secs(DAY));
    params.not_after = odt(now + Duration::from_secs(CA_VALID_DAYS * DAY));
    let kp = KeyPair::generate().map_err(err)?;
    let cert = params.self_signed(&kp).map_err(err)?;
    let ca = Ca::from_parts(cert.pem(), cert.der().to_vec(), kp.serialize_pem())?;
    // Key first: a crash between writes never leaves a cert without its key.
    write_atomic_private(&key, ca.key_pem.as_bytes())?;
    write_atomic_private(&crt, ca.cert_pem.as_bytes())?;
    Ok(ca)
}

/// Set the CA (and the server certificate it signed) aside as `*.bak`, so the
/// next start makes a new one. Devices must trust the new one.
pub fn retire_ca(dir: &Path) -> io::Result<()> {
    for name in [
        "ca.crt",
        "ca.key",
        "server.crt",
        "server.key",
        "server.json",
    ] {
        let from = dir.join(name);
        if from.exists() {
            fs::rename(&from, dir.join(format!("{name}.bak")))?;
        }
    }
    Ok(())
}

/// Issue a leaf for `hosts` + `ips` (127.0.0.1 is always added).
pub fn issue_server_cert(ca: &Ca, hosts: &[String], ips: &[IpAddr]) -> ServerCert {
    issue_at(ca, hosts, ips, SystemTime::now()).expect("server certificate generation")
}

fn issue_at(ca: &Ca, hosts: &[String], ips: &[IpAddr], now: SystemTime) -> io::Result<ServerCert> {
    let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let mut all_ips = ips.to_vec();
    if !all_ips.contains(&loopback) {
        all_ips.push(loopback);
    }
    let mut sans = Vec::new();
    for h in hosts {
        sans.push(SanType::DnsName(
            Ia5String::try_from(h.as_str()).map_err(invalid)?,
        ));
    }
    sans.extend(all_ips.iter().map(|ip| SanType::IpAddress(*ip)));

    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "openOrchestra Remote");
    params.distinguished_name = dn;
    params.subject_alt_names = sans;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let not_after = now + Duration::from_secs(SERVER_FORWARD_DAYS * DAY);
    params.not_before = odt(now - Duration::from_secs(SERVER_BACKDATE_DAYS * DAY));
    params.not_after = odt(not_after);

    let kp = KeyPair::generate().map_err(err)?;
    let cert = params.signed_by(&kp, &issuer_for(ca)?).map_err(err)?;
    Ok(ServerCert {
        cert_pem: cert.pem(),
        key_pem: kp.serialize_pem(),
        hosts: hosts.to_vec(),
        ips: ips.to_vec(),
        not_after,
    })
}

#[derive(Serialize, Deserialize)]
struct ServerMeta {
    hosts: Vec<String>,
    ips: Vec<IpAddr>,
    /// Unix seconds.
    not_after: u64,
    /// SHA-256 fingerprint of the CA that signed it.
    ca: String,
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_server_cert(dir: &Path, ca: &Ca) -> Option<ServerCert> {
    let meta: ServerMeta = serde_json::from_slice(&fs::read(dir.join("server.json")).ok()?).ok()?;
    if meta.ca != fingerprint_sha256(&ca.cert_der) {
        return None;
    }
    let cert_pem = fs::read_to_string(dir.join("server.crt")).ok()?;
    let key_pem = fs::read_to_string(dir.join("server.key")).ok()?;
    pem_to_der(&cert_pem).ok()?;
    KeyPair::from_pem(&key_pem).ok()?;
    Some(ServerCert {
        cert_pem,
        key_pem,
        hosts: meta.hosts,
        ips: meta.ips,
        not_after: UNIX_EPOCH + Duration::from_secs(meta.not_after),
    })
}

/// Reuse the stored server cert unless it needs reissuing; otherwise issue and
/// persist a new one (`server.key`, `server.crt`, then `server.json` last as
/// the commit marker).
pub fn load_or_issue_server_cert(
    dir: &Path,
    ca: &Ca,
    hosts: &[String],
    ips: &[IpAddr],
    now: SystemTime,
) -> io::Result<ServerCert> {
    if let Some(cur) = load_server_cert(dir, ca) {
        if !needs_reissue(&cur, hosts, ips, now) {
            return Ok(cur);
        }
    }
    let sc = issue_at(ca, hosts, ips, now)?;
    let meta = ServerMeta {
        hosts: sc.hosts.clone(),
        ips: sc.ips.clone(),
        not_after: secs(sc.not_after),
        ca: fingerprint_sha256(&ca.cert_der),
    };
    write_atomic_private(&dir.join("server.key"), sc.key_pem.as_bytes())?;
    write_atomic_private(&dir.join("server.crt"), sc.cert_pem.as_bytes())?;
    write_atomic_private(
        &dir.join("server.json"),
        &serde_json::to_vec_pretty(&meta).map_err(err)?,
    )?;
    Ok(sc)
}

fn san_set(hosts: &[String], ips: &[IpAddr]) -> (BTreeSet<String>, BTreeSet<IpAddr>) {
    let mut ip_set: BTreeSet<IpAddr> = ips.iter().copied().collect();
    ip_set.insert(IpAddr::V4(Ipv4Addr::LOCALHOST));
    (
        hosts.iter().map(|h| h.to_ascii_lowercase()).collect(),
        ip_set,
    )
}

/// True when the SAN set differs from `hosts`/`ips` or < 30 days remain.
pub fn needs_reissue(cur: &ServerCert, hosts: &[String], ips: &[IpAddr], now: SystemTime) -> bool {
    if san_set(&cur.hosts, &cur.ips) != san_set(hosts, ips) {
        return true;
    }
    match cur.not_after.duration_since(now) {
        Ok(left) => left < REISSUE_WINDOW,
        Err(_) => true,
    }
}

/// Uppercase hex pairs joined by `:`.
pub fn fingerprint_sha256(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    digest
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn new_uuid() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("system RNG");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// Unsigned iOS configuration profile installing `ca` as a root certificate.
///
/// Each CA gets its own name and `PayloadIdentifier`: iOS replaces an
/// installed profile that has the same identifier, so a shared one would let
/// a second Mac's profile silently remove the first Mac's CA.
pub fn mobileconfig(ca: &Ca) -> String {
    let data = base64::engine::general_purpose::STANDARD.encode(&ca.cert_der);
    let (outer, inner) = (new_uuid(), new_uuid());
    let name = xml_escape(
        &ca_common_name(&ca.cert_der).unwrap_or_else(|| "openOrchestra Remote CA".into()),
    );
    let id: String = fingerprint_sha256(&ca.cert_der).replace(':', "")[..16].to_ascii_lowercase();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>PayloadContent</key>
  <array>
    <dict>
      <key>PayloadCertificateFileName</key>
      <string>ca.crt</string>
      <key>PayloadContent</key>
      <data>{data}</data>
      <key>PayloadDescription</key>
      <string>Lets this device trust openOrchestra Remote over your local network.</string>
      <key>PayloadDisplayName</key>
      <string>{name}</string>
      <key>PayloadIdentifier</key>
      <string>com.openorchestra.remote.ca.{id}.cert</string>
      <key>PayloadType</key>
      <string>com.apple.security.root</string>
      <key>PayloadUUID</key>
      <string>{inner}</string>
      <key>PayloadVersion</key>
      <integer>1</integer>
    </dict>
  </array>
  <key>PayloadDisplayName</key>
  <string>{name}</string>
  <key>PayloadIdentifier</key>
  <string>com.openorchestra.remote.ca.{id}</string>
  <key>PayloadType</key>
  <string>Configuration</string>
  <key>PayloadUUID</key>
  <string>{outer}</string>
  <key>PayloadVersion</key>
  <integer>1</integer>
</dict>
</plist>
"#
    )
}

/// The certificate's subject CN, e.g. "openOrchestra Remote CA (Omar's Mac)".
fn ca_common_name(der: &[u8]) -> Option<String> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    let cn = cert.subject().iter_common_name().next()?;
    cn.as_str().ok().map(str::to_string)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn scutil(key: &str) -> Option<String> {
    let out = os_compat::process::command("scutil").args(["--get", key]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// `scutil --get LocalHostName` (the Bonjour name, without `.local`).
pub fn local_host_name() -> Option<String> {
    scutil("LocalHostName")
}

/// `scutil --get ComputerName`, falling back to "Mac".
pub fn computer_name() -> String {
    scutil("ComputerName").unwrap_or_else(|| "Mac".to_string())
}

fn filter_private(addrs: impl IntoIterator<Item = Ipv4Addr>) -> Vec<IpAddr> {
    let mut v: Vec<Ipv4Addr> = addrs
        .into_iter()
        .filter(|a| !a.is_loopback() && a.is_private())
        .collect();
    v.sort();
    v.dedup();
    v.into_iter().map(IpAddr::V4).collect()
}

/// Non-loopback RFC 1918 IPv4 addresses of this machine, sorted.
pub fn private_ipv4s() -> Vec<IpAddr> {
    let addrs = if_addrs::get_if_addrs().unwrap_or_default();
    filter_private(addrs.into_iter().filter_map(|i| match i.ip() {
        IpAddr::V4(a) => Some(a),
        _ => None,
    }))
}

/// The interface of the default route, from `route -n get default` output
/// (`  interface: en0`).
pub fn parse_default_route_interface(out: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let v = l.trim().strip_prefix("interface:")?.trim();
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// The address a phone on the same network most likely reaches: the
/// default-route interface's private IPv4, else en0's, else en1's, else the
/// lowest private one. VM bridges (Parallels 10.211.55.x, VMware 172.16.x)
/// would otherwise sort ahead of the home 192.168.x address. Pure.
pub fn pick_primary_ipv4(default_if: Option<&str>, addrs: &[(String, Ipv4Addr)]) -> Option<IpAddr> {
    let private = |a: &Ipv4Addr| !a.is_loopback() && a.is_private();
    let on = |name: &str| {
        addrs
            .iter()
            .filter(|(n, a)| n == name && private(a))
            .map(|(_, a)| *a)
            .min()
    };
    default_if
        .and_then(on)
        .or_else(|| on("en0"))
        .or_else(|| on("en1"))
        .or_else(|| addrs.iter().map(|(_, a)| *a).filter(private).min())
        .map(IpAddr::V4)
}

/// [`pick_primary_ipv4`] for this machine (runs `route -n get default`;
/// blocking, keep it off the UI thread).
pub fn primary_ipv4() -> Option<IpAddr> {
    let default_if = os_compat::process::command("route")
        .args(["-n", "get", "default"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_default_route_interface(&String::from_utf8_lossy(&o.stdout)));
    let addrs: Vec<(String, Ipv4Addr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.ip() {
            IpAddr::V4(a) => Some((i.name, a)),
            _ => None,
        })
        .collect();
    pick_primary_ipv4(default_if.as_deref(), &addrs)
}

/// `["<name>.local", "localhost"]`, or `["localhost"]` without a name.
pub fn san_hosts(local_host_name: Option<&str>) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(n) = local_host_name {
        // Lowercase: the CA's name constraint is, and DNS names compare
        // without case anyway.
        v.push(format!("{n}.local").to_ascii_lowercase());
    }
    v.push("localhost".to_string());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::net::IpAddr;
    use os_compat::fs::PermissionsExt;
    use std::time::{Duration, SystemTime};
    use x509_parser::extensions::{GeneralName, ParsedExtension};
    use x509_parser::prelude::*;

    const DAY: u64 = 86_400;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn hosts() -> Vec<String> {
        vec!["mac.local".to_string(), "localhost".to_string()]
    }

    /// The CA a Mac named mac.local at 192.168.1.20 and 10.211.55.2 makes.
    fn ca_at(dir: &Path, name: &str) -> io::Result<Ca> {
        load_or_create_ca(
            dir,
            name,
            &hosts(),
            &[ip("192.168.1.20"), ip("10.211.55.2")],
        )
    }

    #[test]
    fn ca_is_created_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("remote");
        let a = ca_at(&dir, "Test Mac").unwrap();
        let b = ca_at(&dir, "Test Mac").unwrap();
        assert_eq!(a.cert_der, b.cert_der);
        let (_, cert) = X509Certificate::from_der(&a.cert_der).unwrap();
        assert!(cert
            .subject()
            .to_string()
            .contains("openOrchestra Remote CA (Test Mac)"));
        let bc = cert.basic_constraints().unwrap().unwrap().value;
        assert!(bc.ca);
        let v = cert.validity();
        let secs = v.not_after.timestamp() - v.not_before.timestamp();
        assert!(secs >= 3650 * DAY as i64);
        let mode = std::fs::metadata(dir.join("ca.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        #[cfg(unix)] // Windows has no folder modes (os-compat reports any path as 0o600)
        {
            let dmode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(dmode & 0o777, 0o700);
        }
    }

    /// rustls/webpki verification of `leaf` for `name` under `ca` alone.
    fn verify(ca: &Ca, leaf_pem: &str, name: &str) -> Result<(), rustls::Error> {
        use rustls::client::danger::ServerCertVerifier;
        use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(ca.cert_der.clone()))
            .unwrap();
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            std::sync::Arc::new(roots),
            std::sync::Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .unwrap();
        let leaf = CertificateDer::from(super::pem_to_der(leaf_pem).unwrap());
        verifier
            .verify_server_cert(
                &leaf,
                &[],
                &ServerName::try_from(name.to_string()).unwrap(),
                &[],
                UnixTime::now(),
            )
            .map(|_| ())
    }

    #[test]
    fn ca_is_name_constrained_and_still_validates_local_names() {
        let dir = tempfile::tempdir().unwrap();
        let ca = ca_at(dir.path(), "Test Mac").unwrap();
        let (_, cert) = X509Certificate::from_der(&ca.cert_der).unwrap();
        let ext = cert
            .extensions()
            .iter()
            .find(|e| matches!(e.parsed_extension(), ParsedExtension::NameConstraints(_)))
            .expect("name constraints");
        assert!(ext.critical);
        let ParsedExtension::NameConstraints(nc) = ext.parsed_extension() else {
            unreachable!()
        };
        assert!(nc.excluded_subtrees.is_none());
        let mut dns = Vec::new();
        let mut nets = Vec::new();
        for st in nc.permitted_subtrees.as_ref().unwrap() {
            match &st.base {
                GeneralName::DNSName(d) => dns.push(d.to_string()),
                GeneralName::IPAddress(b) => nets.push(b.to_vec()),
                other => panic!("unexpected subtree {other:?}"),
            }
        }
        // This Mac only: its own name and addresses, nothing broader.
        assert_eq!(dns, ["mac.local", "localhost"]);
        assert_eq!(
            nets,
            [
                vec![192, 168, 1, 20, 255, 255, 255, 255],
                vec![10, 211, 55, 2, 255, 255, 255, 255],
                vec![127, 0, 0, 1, 255, 255, 255, 255],
            ]
        );

        // The server cert still validates for each of its names.
        let sc = issue_server_cert(&ca, &hosts(), &[ip("192.168.1.20"), ip("10.211.55.2")]);
        for name in [
            "mac.local",
            "localhost",
            "192.168.1.20",
            "10.211.55.2",
            "127.0.0.1",
        ] {
            verify(&ca, &sc.cert_pem, name).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        // Enforcement: the CA can't vouch for a public name or address.
        let evil = issue_server_cert(&ca, &["example.com".to_string()], &[ip("8.8.8.8")]);
        assert!(verify(&ca, &evil.cert_pem, "example.com").is_err());
        assert!(verify(&ca, &evil.cert_pem, "8.8.8.8").is_err());
        // Nor for another device on the network: a router, another Mac.
        let lan = issue_server_cert(&ca, &["printer.local".to_string()], &[ip("192.168.1.1")]);
        assert!(verify(&ca, &lan.cert_pem, "printer.local").is_err());
        assert!(verify(&ca, &lan.cert_pem, "192.168.1.1").is_err());
    }

    #[test]
    fn unconstrained_ca_on_disk_is_replaced_once() {
        let dir = tempfile::tempdir().unwrap();
        // An old-style CA: same shape, no name constraints.
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "openOrchestra Remote CA (Old)");
        params.distinguished_name = dn;
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let kp = KeyPair::generate().unwrap();
        let old = params.self_signed(&kp).unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("ca.key"), kp.serialize_pem()).unwrap();
        std::fs::write(dir.path().join("ca.crt"), old.pem()).unwrap();
        let old_ca = Ca::from_parts(old.pem(), old.der().to_vec(), kp.serialize_pem()).unwrap();
        // A server cert issued under the old CA.
        let ips = [ip("192.168.1.20")];
        let now = SystemTime::now();
        let old_sc = load_or_issue_server_cert(dir.path(), &old_ca, &hosts(), &ips, now).unwrap();

        let ca = ca_at(dir.path(), "Test Mac").unwrap();
        assert_ne!(ca.cert_der, old.der().to_vec());
        assert!(!is_legacy(&ca.cert_der).unwrap());
        // The old one is kept, not destroyed.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("ca.crt.bak")).unwrap(),
            old.pem()
        );
        // Persisted: the next load keeps the new one.
        let again = ca_at(dir.path(), "Test Mac").unwrap();
        assert_eq!(again.cert_der, ca.cert_der);
        // The server cert is reissued under the new CA.
        let sc = load_or_issue_server_cert(dir.path(), &ca, &hosts(), &ips, now).unwrap();
        assert_ne!(sc.cert_pem, old_sc.cert_pem);
        verify(&ca, &sc.cert_pem, "mac.local").unwrap();
    }

    #[test]
    fn a_broad_ca_from_the_last_version_is_replaced_once_with_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        // The previous version: every .local name and private range.
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.name_constraints = Some(rcgen::NameConstraints {
            permitted_subtrees: vec![
                rcgen::GeneralSubtree::DnsName("local".into()),
                rcgen::GeneralSubtree::IpAddress(rcgen::CidrSubnet::from_addr_prefix(
                    ip("192.168.0.0"),
                    16,
                )),
            ],
            excluded_subtrees: Vec::new(),
        });
        let kp = KeyPair::generate().unwrap();
        let old = params.self_signed(&kp).unwrap();
        std::fs::write(dir.path().join("ca.key"), kp.serialize_pem()).unwrap();
        std::fs::write(dir.path().join("ca.crt"), old.pem()).unwrap();
        assert!(is_legacy(old.der()).unwrap());

        let ca = ca_at(dir.path(), "Test Mac").unwrap();
        assert_ne!(ca.cert_der, old.der().to_vec());
        assert!(dir.path().join("ca.key.bak").is_file());
        assert_eq!(ca_at(dir.path(), "Test Mac").unwrap().cert_der, ca.cert_der);
    }

    #[test]
    fn a_mixed_case_mac_name_still_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = san_hosts(Some("Omars-M5-Pro-2026"));
        assert_eq!(hosts[0], "omars-m5-pro-2026.local");
        let ca = load_or_create_ca(dir.path(), "Mac", &hosts, &[ip("192.168.86.26")]).unwrap();
        let sc = issue_server_cert(&ca, &hosts, &[ip("192.168.86.26")]);
        for name in [
            "omars-m5-pro-2026.local",
            "Omars-M5-Pro-2026.local",
            "192.168.86.26",
        ] {
            verify(&ca, &sc.cert_pem, name).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        assert!(ca.covers_host("Omars-M5-Pro-2026.local"));
    }

    #[test]
    fn an_address_change_never_replaces_the_ca_but_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let ca = ca_at(dir.path(), "Test Mac").unwrap();
        let moved = [ip("192.168.1.77")];
        let again = load_or_create_ca(dir.path(), "Test Mac", &hosts(), &moved).unwrap();
        assert_eq!(again.cert_der, ca.cert_der, "devices keep trusting it");
        assert!(again.outdated_for(&hosts(), &moved));
        assert!(!again.outdated_for(&hosts(), &[ip("192.168.1.20")]));
        // The server certificate only names what the CA covers.
        let (h, i) = again.covered(&hosts(), &[ip("192.168.1.20"), ip("192.168.1.77")]);
        assert_eq!(h, hosts());
        assert_eq!(i, vec![ip("192.168.1.20")]);
        let renamed = vec!["newname.local".to_string()];
        assert!(again.outdated_for(&renamed, &[]));
    }

    #[test]
    fn retiring_the_ca_sets_everything_aside() {
        let dir = tempfile::tempdir().unwrap();
        let ca = ca_at(dir.path(), "Test Mac").unwrap();
        load_or_issue_server_cert(
            dir.path(),
            &ca,
            &hosts(),
            &[ip("192.168.1.20")],
            SystemTime::now(),
        )
        .unwrap();
        retire_ca(dir.path()).unwrap();
        for f in ["ca.crt", "ca.key", "server.crt", "server.key"] {
            assert!(!dir.path().join(f).exists(), "{f}");
            assert!(dir.path().join(format!("{f}.bak")).exists(), "{f}.bak");
        }
        assert_ne!(ca_at(dir.path(), "Test Mac").unwrap().cert_der, ca.cert_der);
    }

    #[test]
    fn server_cert_has_sans_and_short_validity() {
        let dir = tempfile::tempdir().unwrap();
        let ca = ca_at(dir.path(), "Test Mac").unwrap();
        let sc = issue_server_cert(&ca, &hosts(), &[ip("192.168.1.20")]);
        let der = x509_parser::pem::parse_x509_pem(sc.cert_pem.as_bytes())
            .unwrap()
            .1;
        let (_, cert) = X509Certificate::from_der(&der.contents).unwrap();
        let mut sans = BTreeSet::new();
        let mut eku_server = false;
        for ext in cert.extensions() {
            match ext.parsed_extension() {
                ParsedExtension::SubjectAlternativeName(san) => {
                    for n in &san.general_names {
                        match n {
                            GeneralName::DNSName(d) => {
                                sans.insert(format!("DNS:{d}"));
                            }
                            GeneralName::IPAddress(b) => {
                                let a: IpAddr = match b.len() {
                                    4 => <[u8; 4]>::try_from(*b).unwrap().into(),
                                    16 => <[u8; 16]>::try_from(*b).unwrap().into(),
                                    _ => panic!("bad ip"),
                                };
                                sans.insert(format!("IP:{a}"));
                            }
                            _ => {}
                        }
                    }
                }
                ParsedExtension::ExtendedKeyUsage(e) => eku_server = e.server_auth,
                _ => {}
            }
        }
        let want: BTreeSet<String> = [
            "DNS:mac.local",
            "DNS:localhost",
            "IP:127.0.0.1",
            "IP:192.168.1.20",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(sans, want);
        assert!(eku_server);
        let v = cert.validity();
        let secs = v.not_after.timestamp() - v.not_before.timestamp();
        assert!(secs <= 397 * DAY as i64, "validity {secs}s");
        let ca_der = x509_parser::pem::parse_x509_pem(ca.cert_pem.as_bytes())
            .unwrap()
            .1;
        let (_, ca_cert) = X509Certificate::from_der(&ca_der.contents).unwrap();
        assert_eq!(cert.issuer().to_string(), ca_cert.subject().to_string());
        assert!(!cert
            .basic_constraints()
            .ok()
            .flatten()
            .map(|b| b.value.ca)
            .unwrap_or(false));
        cert.verify_signature(Some(ca_cert.public_key())).unwrap();
    }

    #[test]
    fn needs_reissue_on_ip_change() {
        let dir = tempfile::tempdir().unwrap();
        let ca = ca_at(dir.path(), "Test Mac").unwrap();
        let ips = [ip("192.168.1.20")];
        let now = SystemTime::now();
        let sc = load_or_issue_server_cert(dir.path(), &ca, &hosts(), &ips, now).unwrap();
        assert!(!needs_reissue(&sc, &hosts(), &ips, now));
        let more = [ip("192.168.1.20"), ip("192.168.1.21")];
        assert!(needs_reissue(&sc, &hosts(), &more, now));
        assert!(needs_reissue(
            &sc,
            &hosts(),
            &ips,
            now + Duration::from_secs(370 * DAY)
        ));
        // Reloaded from disk it is the same cert; a changed set reissues.
        let again = load_or_issue_server_cert(dir.path(), &ca, &hosts(), &ips, now).unwrap();
        assert_eq!(again.cert_pem, sc.cert_pem);
        let changed = load_or_issue_server_cert(dir.path(), &ca, &hosts(), &more, now).unwrap();
        assert_ne!(changed.cert_pem, sc.cert_pem);
    }

    #[test]
    fn fingerprint_format() {
        let fp = fingerprint_sha256(b"hello");
        let ok = fp.len() == 95
            && fp.split(':').count() == 32
            && fp
                .split(':')
                .all(|p| p.len() == 2 && p.chars().all(|c| matches!(c, '0'..='9' | 'A'..='F')));
        assert!(ok, "{fp}");
    }

    #[test]
    fn mobileconfig_embeds_ca() {
        use base64::Engine;
        let dir = tempfile::tempdir().unwrap();
        let ca = ca_at(dir.path(), "Test Mac").unwrap();
        let xml = mobileconfig(&ca);
        assert!(xml.contains("com.apple.security.root"));
        assert!(xml.contains(&base64::engine::general_purpose::STANDARD.encode(&ca.cert_der)));
        let v = plist::Value::from_reader_xml(xml.as_bytes()).unwrap();
        let d = v.as_dictionary().unwrap();
        assert_eq!(
            d.get("PayloadType").unwrap().as_string(),
            Some("Configuration")
        );
    }

    #[test]
    fn mobileconfigs_from_two_macs_do_not_replace_each_other() {
        let field = |xml: &str, k: &str| {
            let v = plist::Value::from_reader_xml(xml.as_bytes()).unwrap();
            v.as_dictionary()
                .unwrap()
                .get(k)
                .unwrap()
                .as_string()
                .unwrap()
                .to_string()
        };
        let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = mobileconfig(&ca_at(d1.path(), "Studio <A&B>").unwrap());
        let b = mobileconfig(&ca_at(d2.path(), "Laptop").unwrap());
        assert_ne!(
            field(&a, "PayloadIdentifier"),
            field(&b, "PayloadIdentifier")
        );
        assert_eq!(
            field(&a, "PayloadDisplayName"),
            "openOrchestra Remote CA (Studio <A&B>)"
        );
        assert_eq!(
            field(&b, "PayloadDisplayName"),
            "openOrchestra Remote CA (Laptop)"
        );
    }

    #[test]
    fn san_hosts_shapes() {
        assert_eq!(san_hosts(Some("mac")), vec!["mac.local", "localhost"]);
        assert_eq!(san_hosts(None), vec!["localhost"]);
    }

    #[test]
    fn default_route_interface_parsing() {
        let out = "   route to: default\ndestination: default\n       mask: default\n    gateway: 192.168.86.1\n  interface: en0\n      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>\n";
        assert_eq!(parse_default_route_interface(out).as_deref(), Some("en0"));
        let vpn = "   route to: default\n  interface: utun4\n";
        assert_eq!(parse_default_route_interface(vpn).as_deref(), Some("utun4"));
        assert_eq!(
            parse_default_route_interface("route: writing to routing socket: not in table\n"),
            None
        );
        assert_eq!(parse_default_route_interface("  interface: \n"), None);
    }

    #[test]
    fn primary_prefers_default_route_then_en0_en1() {
        let a = |n: &str, ip: &str| (n.to_string(), ip.parse::<Ipv4Addr>().unwrap());
        let addrs = vec![
            a("lo0", "127.0.0.1"),
            a("bridge100", "10.211.55.2"),
            a("vmnet8", "172.16.12.1"),
            a("en0", "192.168.86.30"),
            a("en1", "192.168.1.9"),
        ];
        assert_eq!(
            pick_primary_ipv4(Some("en0"), &addrs),
            Some(ip("192.168.86.30"))
        );
        assert_eq!(
            pick_primary_ipv4(Some("en1"), &addrs),
            Some(ip("192.168.1.9"))
        );
        // No default route (or one without a private address): en0.
        assert_eq!(pick_primary_ipv4(None, &addrs), Some(ip("192.168.86.30")));
        assert_eq!(
            pick_primary_ipv4(Some("utun3"), &addrs),
            Some(ip("192.168.86.30"))
        );
        // No en0: en1.
        let no_en0: Vec<_> = addrs.iter().filter(|(n, _)| n != "en0").cloned().collect();
        assert_eq!(pick_primary_ipv4(None, &no_en0), Some(ip("192.168.1.9")));
        // Neither: lowest private.
        let only_vm = vec![a("vmnet8", "172.16.12.1"), a("bridge100", "10.211.55.2")];
        assert_eq!(pick_primary_ipv4(None, &only_vm), Some(ip("10.211.55.2")));
        // Public / link-local addresses never qualify.
        let public = vec![a("en0", "8.8.8.8"), a("en0", "169.254.3.4")];
        assert_eq!(pick_primary_ipv4(Some("en0"), &public), None);
    }

    #[test]
    fn private_filter() {
        use std::net::Ipv4Addr;
        let got = filter_private([
            Ipv4Addr::new(127, 0, 0, 1),
            Ipv4Addr::new(192, 168, 1, 20),
            Ipv4Addr::new(10, 0, 0, 2),
            Ipv4Addr::new(8, 8, 8, 8),
            Ipv4Addr::new(169, 254, 1, 1),
            Ipv4Addr::new(172, 16, 0, 1),
        ]);
        assert_eq!(
            got,
            vec![ip("10.0.0.2"), ip("172.16.0.1"), ip("192.168.1.20")]
        );
    }
}
