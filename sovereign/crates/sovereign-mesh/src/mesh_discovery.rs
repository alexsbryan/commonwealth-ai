//! Pure address-enumeration helpers used by `EmbeddedDaemon` lifecycle
//! and the desktop relay-picker UI.
//!
//! Why this lives outside `daemon.rs`: nothing here touches daemon state.
//! It's all `(IpAddr, port) → SocketAddr | RelayCandidate` math plus a
//! `SOVEREIGN_ADVERTISE_ADDR` env-var override. Splitting it out keeps
//! the daemon module focused on lifecycle and HTTP wiring (ARCH §3.2).

use std::net::SocketAddr;

use tracing::{info, warn};

/// One reachable address the founder can paste into the `?relay=…`
/// query param of a sovereign:// invite when mDNS won't traverse the
/// network between them and the joiner. Built from
/// `local_ip_candidates()` plus a kind classifier — the desktop UI
/// uses `kind` to recommend the best one (Tailscale > LAN > IPv6).
///
/// **Cloud-peer override.** When `SOVEREIGN_ADVERTISE_ADDR` is set in
/// the daemon's environment, `reachable_addresses` ignores the
/// interface-enumeration path entirely and stamps that value into the
/// `MemberRecord.addresses`. This is the cloud-peer escape hatch: a
/// containerised daemon's `if-addrs` table lists the Docker bridge
/// (172.17.0.0/16) **before** the userspace tailscale netstack, so the
/// founder receives `172.17.0.3:9742` as our self-advertised address
/// and immediately marks us Offline because that IP isn't routable
/// from the laptop. The entrypoint sets this env to `$(tailscale
/// ip -4)` before exec-ing the daemon, which makes the founder see the
/// tailnet IP and gossip succeeds. See `HANDOFF_WS2_MESH_FANOUT.md`
/// for the full incident trail.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct RelayCandidate {
    /// Bare IP literal (no brackets for IPv6 — frontend formats it).
    pub ip: String,
    /// What kind of network this address is on. Drives the
    /// recommendation ordering and the human-readable label.
    /// One of: "tailscale", "lan", "ipv6", "other".
    pub kind: String,
    /// Pre-formatted `host:port` (or `[host]:port` for IPv6) ready
    /// to drop into `?relay=<value>`. Saves the UI from having to
    /// re-implement IPv6 bracket rules.
    pub url_fragment: String,
    /// True for the single best candidate the daemon would pick if
    /// asked to autoselect. Today: Tailscale > LAN > IPv6, first
    /// of its tier wins. The frontend pre-selects this in the
    /// invite-card relay picker.
    pub recommended: bool,
}

/// Classify an IP into a coarse "kind" the UI can render. Tailscale
/// uses the CGNAT range 100.64.0.0/10 (RFC 6598) plus an
/// `fd7a:115c:a1e0::/48` ULA for IPv6. We match on those shapes
/// rather than probing tailscaled — the daemon already runs without
/// any Tailscale dependency and this keeps it that way.
pub(crate) fn classify_ip(ip: &std::net::IpAddr) -> &'static str {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            // CGNAT 100.64.0.0/10 — Tailscale's tailnet range.
            if o[0] == 100 && (o[1] & 0xc0) == 64 {
                "tailscale"
            } else {
                "lan"
            }
        }
        std::net::IpAddr::V6(v6) => {
            // Tailscale ULA prefix fd7a:115c:a1e0::/48.
            let s = v6.segments();
            if s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 {
                "tailscale"
            } else {
                "ipv6"
            }
        }
    }
}

/// Format an IP + port the way `?relay=…` expects: bare for IPv4,
/// bracketed for IPv6 so the colon separator parses unambiguously.
pub(crate) fn format_relay_fragment(ip: &std::net::IpAddr, port: u16) -> String {
    match ip {
        std::net::IpAddr::V4(_) => format!("{ip}:{port}"),
        std::net::IpAddr::V6(_) => format!("[{ip}]:{port}"),
    }
}

/// Build the ordered candidate list the daemon HTTP API serves and
/// the desktop UI renders. Sorted by recommendation tier so the
/// frontend can `[0]` the best one without re-sorting:
///   1. Tailscale — works across networks, peer-to-peer.
///   2. LAN       — works on the same subnet only.
///   3. IPv6 (non-Tailscale) — sometimes routable, often blocked.
///
/// Marks the first candidate as `recommended: true`. If the host has
/// no detected interfaces (no network), returns an empty Vec — the UI
/// then collapses the relay picker.
pub fn relay_candidates(internal_port: u16) -> Vec<RelayCandidate> {
    let mut tagged: Vec<(u8, RelayCandidate)> = local_ip_candidates()
        .into_iter()
        .map(|ip| {
            let kind = classify_ip(&ip);
            let tier: u8 = match kind {
                "tailscale" => 0,
                "lan" => 1,
                "ipv6" => 2,
                _ => 3,
            };
            let cand = RelayCandidate {
                ip: ip.to_string(),
                kind: kind.to_string(),
                url_fragment: format_relay_fragment(&ip, internal_port),
                recommended: false,
            };
            (tier, cand)
        })
        .collect();
    tagged.sort_by_key(|(tier, _)| *tier);
    let mut out: Vec<RelayCandidate> = tagged.into_iter().map(|(_, c)| c).collect();
    if let Some(first) = out.first_mut() {
        first.recommended = true;
    }
    out
}

/// Best-effort list of the host's externally-reachable IPs, so the
/// founder can copy one into `?relay=<ip>:9742` when mDNS is blocked
/// (WiFi AP isolation, multicast filtering, cross-subnet LANs).
///
/// Uses the portable "UDP-connect to a public IP without sending"
/// trick: kernel updates `local_addr` on the socket to reflect the
/// preferred outbound source address. No packets are actually sent.
/// Returns the IPv4 default-route source and, if dual-stack, the
/// IPv6 one. Skips loopback. Not exhaustive (won't enumerate VPN
/// interfaces that aren't the default route) but covers the common
/// home-WiFi and Tailscale cases.
/// Build the `Vec<SocketAddr>` we'll store in our own `MemberRecord`.
/// Each local non-loopback IP becomes `ip:port`. If no interface can
/// be discovered (e.g. no network at all), fall back to the wildcard
/// `0.0.0.0:port` — worse than useless for cross-machine gossip, but
/// at least lets a solo-on-localhost founder start up. Peers that
/// receive a wildcard address will see self-loopback behavior; the
/// warning log below makes that case visible.
pub(crate) fn reachable_addresses(port: u16) -> Vec<SocketAddr> {
    if let Some(override_list) = read_advertise_addr_override(port) {
        info!(
            addrs = ?override_list,
            "mesh: using SOVEREIGN_ADVERTISE_ADDR override instead of \
             auto-detected interfaces — peers will see this exact set"
        );
        return override_list;
    }

    let ips = local_ip_candidates();
    if ips.is_empty() {
        warn!(
            port,
            "no routable local IPs discovered — falling back to \
             0.0.0.0:{port} in MemberRecord. Cross-machine gossip \
             will not work until a network interface is available."
        );
        return vec![SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            port,
        )];
    }
    ips.into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect()
}

/// Parse `SOVEREIGN_ADVERTISE_ADDR` into one or more `SocketAddr`s.
///
/// Accepted shapes (comma-separated, all entries must parse or the
/// whole override is rejected and we fall back to auto-detect):
///   - `100.112.195.45`           → IP only; combined with `port`
///   - `100.112.195.45:9742`      → explicit host:port
///   - `[fd7a:115c:a1e0::1]:9742` → IPv6 bracketed
///   - `fd7a:115c:a1e0::1`        → bare IPv6; combined with `port`
///
/// Returns None when the env var is unset, empty, or fails to parse.
/// We deliberately ignore unset *or* malformed values rather than
/// erroring at boot — a bad env var should degrade to auto-detect with
/// a warning, not refuse to start a daemon that would otherwise work
/// on its LAN.
pub(crate) fn read_advertise_addr_override(port: u16) -> Option<Vec<SocketAddr>> {
    let raw = std::env::var("SOVEREIGN_ADVERTISE_ADDR").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut out = Vec::new();
    for part in trimmed.split(',') {
        let entry = part.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some(addr) = parse_advertise_entry(entry, port) {
            out.push(addr);
        } else {
            warn!(
                value = entry,
                "SOVEREIGN_ADVERTISE_ADDR contained an entry we couldn't \
                 parse as IP or host:port — ignoring this entry"
            );
        }
    }

    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

pub(crate) fn parse_advertise_entry(entry: &str, default_port: u16) -> Option<SocketAddr> {
    use std::net::IpAddr;
    use std::str::FromStr;

    // host:port (or bracketed IPv6) — `SocketAddr::from_str` handles
    // both. Try this first because `IpAddr::from_str` rejects strings
    // containing a port, while `SocketAddr::from_str` only accepts a
    // complete socket address.
    if let Ok(sa) = SocketAddr::from_str(entry) {
        return Some(sa);
    }

    // Bare IPv4 or IPv6 — combine with the daemon's internal_port.
    if let Ok(ip) = IpAddr::from_str(entry) {
        return Some(SocketAddr::new(ip, default_port));
    }

    None
}

pub fn local_ip_candidates() -> Vec<std::net::IpAddr> {
    // Two-tier strategy:
    //
    //   Tier 1: enumerate EVERY local non-loopback interface via
    //   `if-addrs`. This is what we actually want — on a machine
    //   with both WiFi (192.168.x) and Tailscale (100.x) up, both
    //   addresses need to be published so peers can reach us via
    //   whichever one they can route to. The old default-route
    //   trick missed Tailscale entirely on dual-homed machines,
    //   which is EXACTLY the Commonwealth LAN-+-VPN topology.
    //
    //   Tier 2 (fallback): the "UDP-connect to a public IP without
    //   sending" trick. Kept for cases where `if-addrs` errors out
    //   (should never happen on darwin/linux but the contract is
    //   best-effort). Never used in practice.
    //
    // Ordering: preferred routable IPs first — link-local addresses
    // (169.254.x, fe80::) and private-ranges come after globals.
    // Rationale: the peer tries addresses in list order, so putting
    // the most reliable ones first shortens the mean fan-out path.
    let mut ips: Vec<std::net::IpAddr> = Vec::new();

    match if_addrs::get_if_addrs() {
        Ok(addrs) => {
            for iface in addrs {
                let ip = iface.ip();
                if ip.is_loopback() {
                    continue;
                }
                // Link-local addresses are useless cross-machine:
                // 169.254.x is unconfigured DHCP fallback, fe80::
                // is IPv6 link-local which can't route off the
                // local segment. Macs have lots of these from
                // Thunderbolt / virtual interfaces / utun0,1,2...
                // Including them just spams the startup log and
                // wastes fan-out attempts (reqwest dials them and
                // gets EHOSTUNREACH). Drop outright.
                let is_link_local = match ip {
                    std::net::IpAddr::V4(v4) => v4.octets()[0] == 169 && v4.octets()[1] == 254,
                    std::net::IpAddr::V6(v6) => v6.segments()[0] & 0xffc0 == 0xfe80,
                };
                if is_link_local {
                    continue;
                }
                ips.push(ip);
            }
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "if_addrs::get_if_addrs failed — falling back to \
                 UDP-connect default-route detection"
            );
            if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
                if sock.connect("1.1.1.1:80").is_ok() {
                    if let Ok(addr) = sock.local_addr() {
                        if !addr.ip().is_loopback() {
                            ips.push(addr.ip());
                        }
                    }
                }
            }
            if let Ok(sock) = std::net::UdpSocket::bind("[::]:0") {
                if sock.connect("[2606:4700:4700::1111]:80").is_ok() {
                    if let Ok(addr) = sock.local_addr() {
                        let ip = addr.ip();
                        if !ip.is_loopback() && !ips.contains(&ip) {
                            ips.push(ip);
                        }
                    }
                }
            }
        }
    }

    ips
}

#[cfg(test)]
mod relay_tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn classifies_tailscale_cgnat_v4() {
        // Real Tailscale-assigned: 100.104.36.28
        let ip = IpAddr::V4(Ipv4Addr::new(100, 104, 36, 28));
        assert_eq!(classify_ip(&ip), "tailscale");
        // Boundary: 100.64.0.1 is the lowest CGNAT addr.
        assert_eq!(
            classify_ip(&IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))),
            "tailscale"
        );
        // Boundary: 100.127.255.254 is the highest CGNAT addr.
        assert_eq!(
            classify_ip(&IpAddr::V4(Ipv4Addr::new(100, 127, 255, 254))),
            "tailscale"
        );
    }

    #[test]
    fn does_not_misclassify_neighbouring_ranges_as_tailscale() {
        // 100.63.x is NOT CGNAT; 100.128.x is NOT CGNAT.
        assert_eq!(
            classify_ip(&IpAddr::V4(Ipv4Addr::new(100, 63, 1, 1))),
            "lan"
        );
        assert_eq!(
            classify_ip(&IpAddr::V4(Ipv4Addr::new(100, 128, 1, 1))),
            "lan"
        );
    }

    #[test]
    fn classifies_typical_lan_v4_as_lan() {
        assert_eq!(
            classify_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 3))),
            "lan"
        );
        assert_eq!(classify_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))), "lan");
    }

    #[test]
    fn classifies_tailscale_v6_ula() {
        // fd7a:115c:a1e0::a3a:241c — real Tailscale IPv6 from the
        // user's own daemon log.
        let ip = IpAddr::V6("fd7a:115c:a1e0::a3a:241c".parse().unwrap());
        assert_eq!(classify_ip(&ip), "tailscale");
    }

    #[test]
    fn classifies_other_v6_as_ipv6_not_tailscale() {
        let ip = IpAddr::V6(Ipv6Addr::new(0x2606, 0, 0, 0, 0, 0, 0, 1));
        assert_eq!(classify_ip(&ip), "ipv6");
    }

    #[test]
    fn formats_v4_relay_fragment_without_brackets() {
        let ip = IpAddr::V4(Ipv4Addr::new(100, 104, 36, 28));
        assert_eq!(format_relay_fragment(&ip, 9742), "100.104.36.28:9742");
    }

    #[test]
    fn formats_v6_relay_fragment_with_brackets() {
        // The bracket form is what URL parsers (and `?relay=…`'s
        // own parse_join_argument) expect for IPv6.
        let ip = IpAddr::V6("fd7a:115c:a1e0::a3a:241c".parse().unwrap());
        assert_eq!(
            format_relay_fragment(&ip, 9742),
            "[fd7a:115c:a1e0::a3a:241c]:9742"
        );
    }
}

#[cfg(test)]
mod advertise_addr_tests {
    use super::*;

    #[test]
    fn parses_bare_ipv4_with_default_port() {
        let addr = parse_advertise_entry("100.112.195.45", 9742).unwrap();
        assert_eq!(addr.to_string(), "100.112.195.45:9742");
    }

    #[test]
    fn parses_ipv4_with_explicit_port() {
        let addr = parse_advertise_entry("100.112.195.45:9999", 9742).unwrap();
        assert_eq!(addr.to_string(), "100.112.195.45:9999");
    }

    #[test]
    fn parses_bare_ipv6_with_default_port() {
        let addr = parse_advertise_entry("fd7a:115c:a1e0::1", 9742).unwrap();
        assert_eq!(addr.to_string(), "[fd7a:115c:a1e0::1]:9742");
    }

    #[test]
    fn parses_bracketed_ipv6_with_explicit_port() {
        let addr = parse_advertise_entry("[fd7a:115c:a1e0::1]:9999", 9742).unwrap();
        assert_eq!(addr.to_string(), "[fd7a:115c:a1e0::1]:9999");
    }

    #[test]
    fn rejects_garbage_entry() {
        assert!(parse_advertise_entry("not-an-ip", 9742).is_none());
        assert!(parse_advertise_entry("", 9742).is_none());
        assert!(parse_advertise_entry("100.112.195.45:", 9742).is_none());
    }

    // Env-var-reading tests use a Mutex to serialise — the test
    // harness runs tests in parallel by default, and env is per-process
    // shared state.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn env_unset_returns_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("SOVEREIGN_ADVERTISE_ADDR");
        assert!(read_advertise_addr_override(9742).is_none());
    }

    #[test]
    fn env_empty_returns_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("SOVEREIGN_ADVERTISE_ADDR", "   ");
        let got = read_advertise_addr_override(9742);
        std::env::remove_var("SOVEREIGN_ADVERTISE_ADDR");
        assert!(got.is_none());
    }

    #[test]
    fn env_single_ip_yields_one_socket() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("SOVEREIGN_ADVERTISE_ADDR", "100.112.195.45");
        let got = read_advertise_addr_override(9742);
        std::env::remove_var("SOVEREIGN_ADVERTISE_ADDR");
        let got = got.expect("override should parse");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].to_string(), "100.112.195.45:9742");
    }

    #[test]
    fn env_comma_separated_yields_each_socket() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("SOVEREIGN_ADVERTISE_ADDR", "100.112.195.45, 10.0.0.5:9999");
        let got = read_advertise_addr_override(9742);
        std::env::remove_var("SOVEREIGN_ADVERTISE_ADDR");
        let got = got.expect("override should parse");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].to_string(), "100.112.195.45:9742");
        assert_eq!(got[1].to_string(), "10.0.0.5:9999");
    }

    #[test]
    fn env_with_one_bad_entry_drops_just_that_entry() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("SOVEREIGN_ADVERTISE_ADDR", "garbage, 100.112.195.45");
        let got = read_advertise_addr_override(9742);
        std::env::remove_var("SOVEREIGN_ADVERTISE_ADDR");
        let got = got.expect("partial override should parse the good entry");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].to_string(), "100.112.195.45:9742");
    }

    #[test]
    fn env_all_bad_returns_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("SOVEREIGN_ADVERTISE_ADDR", "garbage, more-garbage");
        let got = read_advertise_addr_override(9742);
        std::env::remove_var("SOVEREIGN_ADVERTISE_ADDR");
        assert!(got.is_none());
    }
}
