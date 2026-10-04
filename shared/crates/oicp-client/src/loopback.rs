// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one "is this endpoint on this machine?" decider, moved out of
//! lib.rs for its arch-gate ceiling.

/// Does this `/v1` endpoint point at something on this machine?
///
/// Host-only, and deliberately conservative: anything we cannot parse or do not
/// recognise as loopback counts as OFF-box. A false "on-box" reading would let
/// a `local_only` turn cross the network, which is the failure this whole check
/// exists to prevent — so the unknown case must fail toward refusal (§18.3).
/// The enrich egress gate consults it too, for the same reason.
pub fn endpoint_is_loopback(endpoint: &str) -> bool {
    let rest = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .unwrap_or(endpoint);
    let authority = rest.split('/').next().unwrap_or("");
    // Strip the port. IPv6 literals arrive bracketed (`[::1]:9741`).
    let host = if let Some(close) = authority.find(']') {
        authority.get(1..close).unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    // An address literal is parsed, never prefix-matched: `127.example.com` is
    // a DNS name anyone can register.
    host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case("ip6-localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
#[path = "loopback_tests.rs"]
mod tests;
