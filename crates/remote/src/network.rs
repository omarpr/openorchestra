//! Which networks may reach the server. Devices only connect from a network
//! the user trusted (opening "Add a device" there trusts it).
//!
//! A network is known by its subnet and its router's address. macOS hides
//! the Wi-Fi name and router hardware address without Location permission,
//! so two networks with identical addressing (say `192.168.1.0/24` via
//! `192.168.1.1`) look the same; any other network is refused.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};

use serde::{Deserialize, Serialize};

/// A network this Mac is on: one private IPv4 interface address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalNet {
    pub iface: String,
    /// The subnet's first address, and its prefix length.
    pub network: Ipv4Addr,
    pub prefix: u8,
    /// The router, for the default-route interface.
    pub router: Option<Ipv4Addr>,
    /// Whether the default route goes through it.
    pub primary: bool,
}

impl LocalNet {
    /// Stable identity: subnet and router.
    pub fn key(&self) -> String {
        match self.router {
            Some(r) => format!("{}/{} via {r}", self.network, self.prefix),
            None => format!("{}/{}", self.network, self.prefix),
        }
    }

    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        mask(ip, self.prefix) == self.network
    }
}

fn mask(ip: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    let bits = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix.min(32)))
    };
    Ipv4Addr::from(u32::from(ip) & bits)
}

/// A network the user trusted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedNet {
    pub key: String,
    /// RFC 3339.
    pub added: String,
}

/// One interface address: name, address, prefix, and the interface's own
/// router (from its DHCP lease, so a VPN owning the default route doesn't
/// change it).
pub type IfaceAddr = (String, Ipv4Addr, u8, Option<Ipv4Addr>);

/// Build [`LocalNet`]s. The primary one is the default route's interface, or,
/// when that isn't a local network (a VPN), the first with a router. Pure.
pub fn networks_from(addrs: &[IfaceAddr], default_iface: Option<&str>) -> Vec<LocalNet> {
    let local = |ip: &Ipv4Addr| ip.is_private() && !ip.is_loopback();
    let primary_iface = default_iface
        .filter(|d| addrs.iter().any(|(i, ip, _, _)| i == d && local(ip)))
        .map(str::to_string)
        .or_else(|| {
            addrs
                .iter()
                .find(|(_, ip, _, r)| local(ip) && r.is_some())
                .map(|(i, ..)| i.clone())
        });
    let mut out: Vec<LocalNet> = addrs
        .iter()
        .filter(|(_, ip, _, _)| local(ip))
        .map(|(iface, ip, prefix, router)| LocalNet {
            iface: iface.clone(),
            network: mask(*ip, *prefix),
            prefix: *prefix,
            router: *router,
            primary: primary_iface.as_deref() == Some(iface.as_str()),
        })
        .collect();
    out.sort_by(|a, b| b.primary.cmp(&a.primary).then(a.key().cmp(&b.key())));
    out.dedup_by(|a, b| a.key() == b.key());
    out
}

/// `(interface, gateway)` from `route -n get default` output.
pub fn parse_default_route(out: &str) -> Option<(String, Ipv4Addr)> {
    let field = |name: &str| {
        out.lines().find_map(|l| {
            let v = l.trim().strip_prefix(name)?.trim();
            (!v.is_empty()).then(|| v.to_string())
        })
    };
    Some((field("interface:")?, field("gateway:")?.parse().ok()?))
}

/// This Mac's networks now. Blocking (`route`): keep it off request paths.
pub fn local_networks() -> Vec<LocalNet> {
    let ifaces = if_addrs::get_if_addrs().unwrap_or_default();
    let routes = Routes::read();
    let addrs: Vec<IfaceAddr> = ifaces
        .iter()
        .filter_map(|i| match &i.addr {
            if_addrs::IfAddr::V4(v4) if v4.ip.is_private() && !v4.ip.is_loopback() => {
                Some((i.name.clone(), v4.ip, v4.prefixlen, routes.router(i)))
            }
            _ => None,
        })
        .collect();
    networks_from(&addrs, routes.default_iface(&ifaces).as_deref())
}

/// Routers and the default route, from the system's own tools.
#[cfg(not(windows))]
struct Routes;

#[cfg(not(windows))]
impl Routes {
    fn read() -> Self {
        Routes
    }

    /// The router `iface`'s DHCP lease names (`ipconfig getoption IFACE router`).
    fn router(&self, iface: &if_addrs::Interface) -> Option<Ipv4Addr> {
        let out = os_compat::process::command("ipconfig")
            .args(["getoption", &iface.name, "router"])
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }

    fn default_iface(&self, _ifaces: &[if_addrs::Interface]) -> Option<String> {
        let out = os_compat::process::command("route")
            .args(["-n", "get", "default"])
            .output()
            .ok()?;
        parse_default_route(&String::from_utf8_lossy(&out.stdout)).map(|(iface, _)| iface)
    }
}

/// Windows has neither `ipconfig getoption` nor `route -n get`: each
/// adapter's gateway and the default route's interface come from the IP
/// Helper API, matched to interfaces by index. Without them every key was
/// a bare subnet, which any network with the same addressing matched.
#[cfg(windows)]
struct Routes {
    gateways: Vec<(u32, Ipv4Addr)>,
    default: Option<u32>,
}

#[cfg(windows)]
impl Routes {
    fn read() -> Self {
        Routes {
            gateways: os_compat::net::ipv4_gateways(),
            default: os_compat::net::default_route_index(),
        }
    }

    fn router(&self, iface: &if_addrs::Interface) -> Option<Ipv4Addr> {
        let index = iface.index?;
        self.gateways.iter().find(|(i, _)| *i == index).map(|(_, ip)| *ip)
    }

    fn default_iface(&self, ifaces: &[if_addrs::Interface]) -> Option<String> {
        let index = self.default?;
        ifaces
            .iter()
            .find(|i| i.index == Some(index) && matches!(i.addr, if_addrs::IfAddr::V4(_)))
            .map(|i| i.name.clone())
    }
}

/// `next` with routers a failed lookup lost filled back in from `prev` (same
/// interface and subnet), so one bad `ipconfig` doesn't change a network's
/// identity and lock devices out until the next look.
pub fn keep_routers(prev: &[LocalNet], mut next: Vec<LocalNet>) -> Vec<LocalNet> {
    for n in next.iter_mut().filter(|n| n.router.is_none()) {
        if let Some(p) = prev
            .iter()
            .find(|p| p.iface == n.iface && p.network == n.network && p.prefix == n.prefix)
        {
            n.router = p.router;
        }
    }
    next
}

/// Whether `peer` may connect, given this Mac's `nets` and the `trusted`
/// keys. Loopback always may. An IPv4 peer must be on a trusted local
/// network; an IPv6 one (link-local or ULA, which name no subnet here) is
/// allowed only while the primary network is trusted.
pub fn peer_trusted(peer: IpAddr, nets: &[LocalNet], trusted: &HashSet<String>) -> bool {
    match peer.to_canonical() {
        ip if ip.is_loopback() => true,
        IpAddr::V4(v4) => nets
            .iter()
            .any(|n| n.contains(v4) && trusted.contains(&n.key())),
        IpAddr::V6(_) => nets.iter().any(|n| n.primary && trusted.contains(&n.key())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn v4(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    fn home_addrs() -> Vec<IfaceAddr> {
        vec![
            (
                "en0".into(),
                v4("192.168.86.26"),
                24,
                Some(v4("192.168.86.1")),
            ),
            ("bridge100".into(), v4("192.168.139.3"), 24, None),
            ("lo0".into(), v4("127.0.0.1"), 8, None),
        ]
    }

    fn home() -> Vec<LocalNet> {
        networks_from(&home_addrs(), Some("en0"))
    }

    #[test]
    fn a_vpn_owning_the_default_route_changes_nothing() {
        assert_eq!(networks_from(&home_addrs(), Some("utun4")), home());
    }

    #[test]
    fn a_failed_router_lookup_keeps_the_last_identity() {
        let mut lost = home_addrs();
        lost[0].3 = None;
        let next = networks_from(&lost, Some("en0"));
        assert_ne!(next[0].key(), home()[0].key());
        assert_eq!(keep_routers(&home(), next)[0].key(), home()[0].key());
    }

    #[test]
    fn networks_carry_subnet_router_and_primary() {
        let n = home();
        assert_eq!(n.len(), 2, "{n:?}");
        assert_eq!(n[0].key(), "192.168.86.0/24 via 192.168.86.1");
        assert!(n[0].primary);
        assert_eq!(n[1].key(), "192.168.139.0/24");
    }

    #[test]
    fn only_peers_on_a_trusted_network_get_in() {
        let n = home();
        let trusted: HashSet<String> = ["192.168.86.0/24 via 192.168.86.1".to_string()].into();
        assert!(peer_trusted(ip("192.168.86.40"), &n, &trusted));
        assert!(peer_trusted(ip("127.0.0.1"), &n, &HashSet::new()));
        assert!(
            !peer_trusted(ip("192.168.139.9"), &n, &trusted),
            "VM network not trusted"
        );
        assert!(
            !peer_trusted(ip("10.0.0.5"), &n, &trusted),
            "not a local network"
        );
        assert!(
            peer_trusted(ip("fe80::1"), &n, &trusted),
            "IPv6 follows the primary network"
        );
        assert!(!peer_trusted(ip("fe80::1"), &n, &HashSet::new()));
    }

    #[test]
    fn same_subnet_behind_another_router_is_another_network() {
        let cafe = networks_from(
            &[(
                "en0".into(),
                v4("192.168.86.77"),
                24,
                Some(v4("192.168.86.254")),
            )],
            Some("en0"),
        );
        let trusted: HashSet<String> = ["192.168.86.0/24 via 192.168.86.1".to_string()].into();
        assert!(!peer_trusted(ip("192.168.86.40"), &cafe, &trusted));
    }

    #[test]
    fn default_route_parsing() {
        let out = "   route to: default\ndestination: default\n    gateway: 192.168.86.1\n  interface: en0\n";
        assert_eq!(
            parse_default_route(out),
            Some(("en0".to_string(), "192.168.86.1".parse().unwrap()))
        );
        assert_eq!(parse_default_route("interface: en0\n"), None);
    }
}
