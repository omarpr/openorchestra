//! TLS for the HTTPS server: the rustls config (always the `ring`
//! provider) and the loop that reissues and hot-swaps the server cert when
//! the LAN addresses change or it nears expiry.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum_server::tls_rustls::RustlsConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::certs::{self, Ca, ServerCert};
use crate::server::{stopped, Shared};

/// How often the cert SAN set is recomputed (LAN address changes).
const CERT_CHECK: Duration = Duration::from_secs(60);

/// rustls config for `cert`, always with the `ring` provider (never the
/// process default: the app links both ring and aws-lc).
pub(crate) fn tls_config(cert: &ServerCert) -> io::Result<rustls::ServerConfig> {
    let chain = CertificateDer::pem_slice_iter(cert.cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)?;
    let key = PrivateKeyDer::from_pem_slice(cert.key_pem.as_bytes()).map_err(io::Error::other)?;
    let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(io::Error::other)?
    .with_no_client_auth()
    .with_single_cert(chain, key)
    .map_err(io::Error::other)?;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(cfg)
}

/// Every 60s (or on `test_set_hosts`), reissue the cert if the SAN set
/// changed or it nears expiry, and hot-swap it. Also refreshes status.
pub(crate) async fn cert_loop(
    s: Arc<Shared>,
    ca: Arc<Ca>,
    mut cur: ServerCert,
    tls: RustlsConfig,
    dir: PathBuf,
) {
    let mut stop = s.shutdown.subscribe();
    let mut file = s.file.subscribe();
    let mut ips = cur.ips.clone();
    loop {
        tokio::select! {
            _ = stopped(&mut stop) => return,
            _ = tokio::time::sleep(CERT_CHECK) => {}
            _ = s.cert_wake.notified() => {}
            _ = file.changed() => {
                // Only the stale-IP device list depends on the file.
                s.refresh_status(&ips);
                continue;
            }
        };
        let s2 = s.clone();
        let Ok((hosts, new_ips)) = tokio::task::spawn_blocking(move || s2.desired_sans()).await
        else {
            continue;
        };
        ips = new_ips;
        if certs::needs_reissue(&cur, &hosts, &ips, SystemTime::now()) {
            let (ca2, dir2, h2, i2) = (ca.clone(), dir.clone(), hosts.clone(), ips.clone());
            let issued = tokio::task::spawn_blocking(move || {
                let cert =
                    certs::load_or_issue_server_cert(&dir2, &ca2, &h2, &i2, SystemTime::now())?;
                let cfg = tls_config(&cert)?;
                Ok::<_, io::Error>((cert, cfg))
            })
            .await;
            match issued {
                Ok(Ok((cert, cfg))) => {
                    tls.reload_from_config(Arc::new(cfg));
                    *s.sans.write().unwrap_or_else(|e| e.into_inner()) =
                        (cert.hosts.clone(), cert.ips.clone());
                    cur = cert;
                }
                Ok(Err(e)) => eprintln!("oo-remote: certificate reissue failed: {e}"),
                Err(e) => eprintln!("oo-remote: certificate reissue panicked: {e}"),
            }
        }
        s.refresh_status(&ips);
    }
}
