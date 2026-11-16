//! Peer address filter: only loopback, RFC 1918, link-local and IPv6 ULA.

use std::net::IpAddr;

/// Whether a connecting peer is on the local network. IPv4-mapped IPv6
/// addresses are normalized to IPv4 first.
pub fn is_allowed_peer(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let seg0 = v6.segments()[0];
            v6.is_loopback() || (seg0 & 0xffc0) == 0xfe80 || (seg0 & 0xfe00) == 0xfc00
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_filter() {
        for ok in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.0.5",
            "169.254.1.1",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:192.168.1.2",
        ] {
            assert!(is_allowed_peer(ok.parse().unwrap()), "{ok}");
        }
        for bad in [
            "172.32.0.1",
            "8.8.8.8",
            "100.64.0.1",
            "2001:db8::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_allowed_peer(bad.parse().unwrap()), "{bad}");
        }
    }
}
