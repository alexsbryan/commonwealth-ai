// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn ttl_accepts_the_suffixes_the_help_advertises() {
    assert_eq!(parse_ttl("90"), Ok(90));
    assert_eq!(parse_ttl("90s"), Ok(90));
    assert_eq!(parse_ttl("30m"), Ok(1_800));
    assert_eq!(parse_ttl("2h"), Ok(7_200));
    assert_eq!(parse_ttl("1d"), Ok(86_400));
}

#[test]
fn ttl_refuses_rather_than_defaulting() {
    // Each of these once had a "reasonable" reading. A grant is a security
    // object: an unparseable window must not become a default one.
    assert!(parse_ttl("soon").is_err());
    assert!(parse_ttl("2 hours").is_err());
    assert!(parse_ttl("0").is_err());
    assert!(parse_ttl("0h").is_err());
    assert!(parse_ttl("").is_err());
    assert!(parse_ttl("-1h").is_err());
}

#[test]
fn loopback_binds_are_recognised_in_every_form_config_allows() {
    assert!(bind_is_loopback("127.0.0.1"));
    assert!(bind_is_loopback("127.0.0.53"));
    assert!(bind_is_loopback("::1"));
    assert!(bind_is_loopback("localhost"));
    assert!(bind_is_loopback("  127.0.0.1  "));
    assert!(!bind_is_loopback("0.0.0.0"));
    assert!(!bind_is_loopback("192.168.1.10"));
    assert!(!bind_is_loopback("::"));
}

/// The link carries a BASE; `RemoteApiProvider` appends `/v1`. An operator
/// pasting the URL they use with curl must not produce `/v1/v1`.
#[test]
fn base_url_normalisation_strips_v1_and_adds_a_scheme() {
    assert_eq!(normalise_base("box:9741"), "http://box:9741");
    assert_eq!(normalise_base("http://box:9741/"), "http://box:9741");
    assert_eq!(normalise_base("http://box:9741/v1"), "http://box:9741");
    assert_eq!(normalise_base("http://box:9741/v1/"), "http://box:9741");
    assert_eq!(normalise_base("https://box/v1"), "https://box");
}

#[test]
fn an_explicit_url_wins_over_discovery() {
    assert_eq!(
        guest_base_url(Some("10.0.0.7:9741"), 9741).unwrap(),
        "http://10.0.0.7:9741"
    );
}

/// The cloud-peer case. Reads the env var directly (rather than through a
/// seam) because that is what the daemon does, and a guest link carrying a
/// different address than `MemberRecord.addresses` would be the bug.
///
/// Serialised with the other env-touching test by running them in one
/// body: `cargo test` shares a process, and two tests mutating the same
/// var race.
#[test]
fn the_advertise_override_beats_interface_enumeration_but_not_an_explicit_url() {
    // No other test in this module reads or writes this var, so the
    // shared-process mutation is contained.
    std::env::set_var("SOVEREIGN_ADVERTISE_ADDR", "100.112.195.45");
    assert_eq!(
        guest_base_url(None, 9741).unwrap(),
        "http://100.112.195.45:9741",
        "a containerised daemon publishes the tailnet IP, not the docker bridge"
    );
    assert_eq!(
        guest_base_url(Some("box.example:9741"), 9741).unwrap(),
        "http://box.example:9741",
        "--url is still the most specific instruction"
    );
    std::env::remove_var("SOVEREIGN_ADVERTISE_ADDR");
}

/// What a guest is told, and what they must never be told. The tunnel's
/// local port is this machine's; naming it would say the work happens
/// here, which is the opposite of the truth.
#[test]
fn a_dialled_link_is_described_by_the_lender_never_by_the_local_bridge() {
    let mut link = GuestLink {
        token: "tok".into(),
        url: "http://box:9741".into(),
        dial: None,
        expires_at: 9_000,
        summary: None,
    };
    assert_eq!(describe(&link), "http://box:9741");
    link.dial = Some("beef@https://relay.example".into());
    assert_eq!(describe(&link), "http://box:9741 (over the mesh tunnel)");
    assert!(
        !describe(&link).contains("127.0.0.1"),
        "the guest is told whose machine answers, not which local port carries it"
    );
}

#[test]
fn durations_render_at_the_scale_an_operator_reads() {
    assert_eq!(human_duration(45), "45s");
    assert_eq!(human_duration(90), "1m");
    assert_eq!(human_duration(7_200), "2h0m");
    assert_eq!(human_duration(90_000), "1d1h");
}
