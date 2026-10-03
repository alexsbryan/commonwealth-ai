// SPDX-License-Identifier: AGPL-3.0-or-later
//! Peer-address preference ordering.
//!
//! When a `MemberRecord` carries multiple addresses for a peer (a
//! Tailscale CGNAT IPv4, a Tailscale IPv6 ULA, possibly a LAN IPv4,
//! etc.), every consumer that iterates them — gossip, inference
//! routing, mesh-store push — needs the same preference order.
//! Otherwise some paths reach a peer on its first-tried address and
//! others fail through every address before falling back.
//!
//! The order, lowest rank wins:
//!   0. IPv4 in the Tailscale CGNAT range (100.64.0.0/10).
//!   1. Any other IPv4 (RFC1918 LAN, public, link-local).
//!   2. IPv6 in the Tailscale ULA prefix (fd7a:115c:a1e0::/48).
//!   3. Any other IPv6.
//!
//! IPv4 is strictly preferred over IPv6 today because some operator
//! environments (e.g. Toolbox containers) have working IPv4 over the
//! tailnet but no IPv6 routing. Tailscale's coordination plane still
//! gossips IPv6 ULAs unconditionally, which without this sort caused
//! "all peer addresses failed" errors that masked an otherwise
//! reachable peer. Within each family, Tailscale-tagged addresses
//! win over LAN/public so cross-network paths still work when
//! everyone is on the tailnet.

use std::net::{IpAddr, SocketAddr};

/// Rank a single address. Lower is preferred. See module docs for
/// the rationale.
pub fn rank(addr: &SocketAddr) -> u8 {
    match addr.ip() {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            // Tailscale CGNAT: 100.64.0.0/10 → first byte 100 with
            // top two bits of the second byte equal to 0b01.
            if o[0] == 100 && (o[1] & 0xc0) == 0x40 {
                0
            } else {
                1
            }
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            // Tailscale ULA: fd7a:115c:a1e0::/48
            if s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 {
                2
            } else {
                3
            }
        }
    }
}

/// Sort a peer's addresses in place by preference. Stable: addresses
/// with the same rank keep their original relative order.
pub fn sort_addresses(addrs: &mut [SocketAddr]) {
    addrs.sort_by_key(rank);
}

/// Return a new `Vec` with the addresses sorted by preference.
pub fn sorted_addresses(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = addrs.to_vec();
    sort_addresses(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(a: &str) -> SocketAddr {
        a.parse().unwrap()
    }

    #[test]
    fn ipv4_tailscale_outranks_everything() {
        assert!(rank(&s("100.64.0.3:9742")) < rank(&s("192.168.1.5:9742")));
        assert!(rank(&s("100.64.0.3:9742")) < rank(&s("[fd7a:115c:a1e0::1]:9742")));
        assert!(rank(&s("100.64.0.3:9742")) < rank(&s("[2001:db8::1]:9742")));
    }

    #[test]
    fn ipv4_strictly_outranks_ipv6() {
        // The bug we hit: Tailscale IPv6 ULA was tied with IPv4 CGNAT.
        // Now strictly worse.
        assert!(rank(&s("192.168.1.5:9742")) < rank(&s("[fd7a:115c:a1e0::1]:9742")));
    }

    #[test]
    fn tailscale_ula_outranks_other_ipv6() {
        assert!(rank(&s("[fd7a:115c:a1e0::1]:9742")) < rank(&s("[2001:db8::1]:9742")));
    }

    #[test]
    fn cgnat_boundary() {
        // 100.64.0.0/10 = 100.64.0.0 .. 100.127.255.255
        assert_eq!(rank(&s("100.64.0.1:1")), 0);
        assert_eq!(rank(&s("100.127.255.254:1")), 0);
        assert_eq!(rank(&s("100.128.0.1:1")), 1);
        assert_eq!(rank(&s("100.63.255.254:1")), 1);
    }

    #[test]
    fn sort_realistic_mesh_peer() {
        // What gossip might hand us for a peer behind tailnet + LAN.
        let mut addrs = vec![
            s("[fd7a:115c:a1e0::a3a:241c]:9741"),
            s("100.64.0.2:9741"),
            s("192.168.1.42:9741"),
        ];
        sort_addresses(&mut addrs);
        assert_eq!(addrs[0], s("100.64.0.2:9741"), "Tailscale IPv4 first");
        assert_eq!(addrs[1], s("192.168.1.42:9741"), "LAN IPv4 second");
        assert_eq!(
            addrs[2],
            s("[fd7a:115c:a1e0::a3a:241c]:9741"),
            "IPv6 ULA last"
        );
    }

    #[test]
    fn sort_is_stable_within_rank() {
        // Two same-rank addresses should keep input order.
        let mut addrs = vec![s("100.10.0.1:9741"), s("100.20.0.1:9741")];
        sort_addresses(&mut addrs);
        assert_eq!(addrs[0], s("100.10.0.1:9741"));
        assert_eq!(addrs[1], s("100.20.0.1:9741"));
    }
}
