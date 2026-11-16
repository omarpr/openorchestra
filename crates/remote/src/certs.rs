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
use std::process::Command;
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
fn ca_name_constraints() -> NameConstraints {
    let net = |a: [u8; 4], prefix: u8| {
        GeneralSubtree::IpAddress(CidrSubnet::from_addr_prefix(IpAddr::V4(a.into()), prefix))
    };
    NameConstraints {
        permitted_subtrees: vec![
            GeneralSubtree::DnsName("local".into()),
            GeneralSubtree::DnsName("localhost".into()),
            net([10, 0, 0, 0], 8),
            net([172, 16, 0, 0], 12),
            net([192, 168, 0, 0], 16),
            net([169, 254, 0, 0], 16),
            net([127, 0, 0, 0], 8),
        ],
        excluded_subtrees: Vec::new(),
    }
}

/// Whether the CA certificate carries a name-constraints extension.
fn has_name_constraints(der: &[u8]) -> io::Result<bool> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).map_err(invalid)?;
    Ok(cert.name_constraints().map_err(invalid)?.is_some())
}

/// Load `ca.crt`/`ca.key` from `dir`, generating them on first use. Existing
/// but unreadable files are an error (never silently replaced: phones already
/// trust the old CA). The one exception: a readable CA from before name
/// constraints (an unreleased build; no phone trusts it yet) is replaced
/// once, logged, by a constrained one; the server cert follows (it records
/// the CA fingerprint it was issued under).
pub fn load_or_create_ca(dir: &Path, computer_name: &str) -> io::Result<Ca> {
    let crt = dir.join("ca.crt");
    let key = dir.join("ca.key");
    match (fs::read_to_string(&crt), fs::read_to_string(&key)) {
        (Ok(cert_pem), Ok(key_pem)) => {
            let cert_der = pem_to_der(&cert_pem)?;
            KeyPair::from_pem(&key_pem).map_err(invalid)?;
            if has_name_constraints(&cert_der)? {
                return Ok(Ca {
                    cert_pem,
                    cert_der,
                    key_pem,
                });
            }
            eprintln!(
                "oo-remote: replacing the remote CA in {} with one limited to local names \
                 (re-add the app on any phone that trusted the old one)",
                dir.display()
            );
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
    params.name_constraints = Some(ca_name_constraints());
    params.not_before = odt(now - Duration::from_secs(DAY));
    params.not_after = odt(now + Duration::from_secs(CA_VALID_DAYS * DAY));
    let kp = KeyPair::generate().map_err(err)?;
    let cert = params.self_signed(&kp).map_err(err)?;
    let ca = Ca {
        cert_pem: cert.pem(),
        cert_der: cert.der().to_vec(),
        key_pem: kp.serialize_pem(),
    };
    // Key first: a crash between writes never leaves a cert without its key.
    write_atomic_private(&key, ca.key_pem.as_bytes())?;
    write_atomic_private(&crt, ca.cert_pem.as_bytes())?;
    Ok(ca)
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
pub fn mobileconfig(ca: &Ca) -> String {
    let data = base64::engine::general_purpose::STANDARD.encode(&ca.cert_der);
    let (outer, inner) = (new_uuid(), new_uuid());
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
      <string>openOrchestra Remote CA</string>
      <key>PayloadIdentifier</key>
      <string>com.openorchestra.remote.ca.cert</string>
      <key>PayloadType</key>
      <string>com.apple.security.root</string>
      <key>PayloadUUID</key>
      <string>{inner}</string>
      <key>PayloadVersion</key>
      <integer>1</integer>
    </dict>
  </array>
  <key>PayloadDisplayName</key>
  <string>openOrchestra Remote CA</string>
  <key>PayloadIdentifier</key>
  <string>com.openorchestra.remote.ca</string>
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

fn scutil(key: &str) -> Option<String> {
    let out = Command::new("scutil").args(["--get", key]).output().ok()?;
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
    let default_if = Command::new("route")
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
        v.push(format!("{n}.local"));
    }
    v.push("localhost".to_string());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::net::IpAddr;
    use std::os::unix::fs::PermissionsExt;
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

    #[test]
    fn ca_is_created_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("remote");
        let a = load_or_create_ca(&dir, "Test Mac").unwrap();
        let b = load_or_create_ca(&dir, "Test Mac").unwrap();
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
        let dmode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(dmode & 0o777, 0o700);
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
        let ca = load_or_create_ca(dir.path(), "Test Mac").unwrap();
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
        assert_eq!(dns, ["local", "localhost"]);
        assert_eq!(
            nets,
            [
                vec![10, 0, 0, 0, 255, 0, 0, 0],
                vec![172, 16, 0, 0, 255, 240, 0, 0],
                vec![192, 168, 0, 0, 255, 255, 0, 0],
                vec![169, 254, 0, 0, 255, 255, 0, 0],
                vec![127, 0, 0, 0, 255, 0, 0, 0],
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
        let old_ca = Ca {
            cert_pem: old.pem(),
            cert_der: old.der().to_vec(),
            key_pem: kp.serialize_pem(),
        };
        // A server cert issued under the old CA.
        let ips = [ip("192.168.1.20")];
        let now = SystemTime::now();
        let old_sc = load_or_issue_server_cert(dir.path(), &old_ca, &hosts(), &ips, now).unwrap();

        let ca = load_or_create_ca(dir.path(), "Test Mac").unwrap();
        assert_ne!(ca.cert_der, old.der().to_vec());
        assert!(has_name_constraints(&ca.cert_der).unwrap());
        // Persisted: the next load keeps the new one.
        let again = load_or_create_ca(dir.path(), "Test Mac").unwrap();
        assert_eq!(again.cert_der, ca.cert_der);
        // The server cert is reissued under the new CA.
        let sc = load_or_issue_server_cert(dir.path(), &ca, &hosts(), &ips, now).unwrap();
        assert_ne!(sc.cert_pem, old_sc.cert_pem);
        verify(&ca, &sc.cert_pem, "mac.local").unwrap();
    }

    #[test]
    fn server_cert_has_sans_and_short_validity() {
        let dir = tempfile::tempdir().unwrap();
        let ca = load_or_create_ca(dir.path(), "Test Mac").unwrap();
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
        let ca = load_or_create_ca(dir.path(), "Test Mac").unwrap();
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
        let ca = load_or_create_ca(dir.path(), "Test Mac").unwrap();
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
