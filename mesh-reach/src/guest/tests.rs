// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unit tests of the guest dialer's private helpers, moved with them from
//! commonwealth-transport iroh.rs (pb-reach-guest).

use super::*;

#[test]
fn parse_relay_mode_empty_is_default() {
    // Empty config = leave the caller on the preset (n0) relays.
    assert!(parse_relay_mode(&[]).is_none());
}

#[test]
fn parse_relay_mode_valid_urls_build_custom() {
    let mode = parse_relay_mode(&[
        "https://relay.corp.example:443".to_string(),
        "https://relay2.corp.example".to_string(),
    ]);
    assert!(
        matches!(mode, Some(iroh::RelayMode::Custom(_))),
        "valid relay URLs must build a custom relay map"
    );
}

#[test]
fn parse_relay_mode_all_invalid_falls_back_to_default() {
    // A fat-fingered relay URL must not abort — fall back to the
    // default relays rather than take the node offline.
    assert!(parse_relay_mode(&["not a url".to_string()]).is_none());
}

#[test]
fn parse_relay_mode_skips_bad_keeps_good() {
    let mode = parse_relay_mode(&[
        "://broken".to_string(),
        "https://relay.corp.example:443".to_string(),
    ]);
    assert!(
        matches!(mode, Some(iroh::RelayMode::Custom(_))),
        "one valid URL among bad ones still yields a custom map"
    );
}

#[test]
fn redact_userinfo_hides_credentials() {
    assert_eq!(
        redact_userinfo("https://user:secret@proxy.corp:443"),
        "https://***@proxy.corp:443"
    );
    // No userinfo → unchanged.
    assert_eq!(
        redact_userinfo("https://proxy.corp:443"),
        "https://proxy.corp:443"
    );
    // Non-URL → unchanged (don't mangle).
    assert_eq!(redact_userinfo("proxy.corp:443"), "proxy.corp:443");
}

#[tokio::test]
async fn build_relayed_endpoint_with_custom_relay_binds() {
    // A configured self-hosted relay must not break endpoint
    // construction: bind is a local operation, the relay connection
    // is a background task (so this succeeds offline). Proves the
    // relay_urls → custom RelayMode path threads through to a valid
    // endpoint. proxy_from_env is a no-op here (env unset).
    let ep = build_relayed_endpoint(
        SecretKey::from_bytes(&[77u8; 32]),
        vec![GUEST_ALPN.to_vec()],
        &RelayConfig {
            relay_urls: vec!["https://relay.corp.example:443".to_string()],
            n0_services: true,
        },
    )
    .await
    .expect("custom-relay endpoint must bind");
    assert!(
        !ep.bound_sockets().is_empty(),
        "endpoint must bind a socket"
    );
}
