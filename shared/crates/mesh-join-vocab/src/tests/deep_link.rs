use super::*;

#[test]
fn parse_simple_join_link() {
    let link = parse_deep_link("sovereign://join/cwth-7f3a-9b2e-4d1c").unwrap();
    match link {
        DeepLink::Join {
            join_key,
            relay_hint,
            mesh_name,
            ..
        } => {
            assert_eq!(join_key, "cwth-7f3a-9b2e-4d1c");
            assert!(relay_hint.is_none());
            assert!(mesh_name.is_none());
        }
        other => panic!("expected a join link, got {other:?}"),
    }
}

#[test]
fn parse_join_link_with_params() {
    let link =
        parse_deep_link("sovereign://join/cwth-7f3a-9b2e-4d1c?relay=192.168.1.100&name=Lab+Squad")
            .unwrap();
    match link {
        DeepLink::Join {
            join_key,
            relay_hint,
            mesh_name,
            ..
        } => {
            assert_eq!(join_key, "cwth-7f3a-9b2e-4d1c");
            assert_eq!(relay_hint.as_deref(), Some("192.168.1.100"));
            assert_eq!(mesh_name.as_deref(), Some("Lab Squad"));
        }
        other => panic!("expected a join link, got {other:?}"),
    }
}

#[test]
fn parse_invalid_link() {
    assert!(parse_deep_link("https://example.com").is_none());
    assert!(parse_deep_link("sovereign://unknown/foo").is_none());
    assert!(parse_deep_link("sovereign://join").is_none());
}

#[test]
fn build_and_parse_round_trip() {
    let url = build_join_link(
        "cwth-abcd-efgh-ijkl",
        Some("10.0.0.5"),
        Some("My Mesh"),
        None,
        false,
        None,
    );
    assert_eq!(
        url,
        "sovereign://join/cwth-abcd-efgh-ijkl?relay=10.0.0.5&name=My+Mesh"
    );

    let link = parse_deep_link(&url).unwrap();
    match link {
        DeepLink::Join {
            join_key,
            relay_hint,
            mesh_name,
            ..
        } => {
            assert_eq!(join_key, "cwth-abcd-efgh-ijkl");
            assert_eq!(relay_hint.as_deref(), Some("10.0.0.5"));
            assert_eq!(mesh_name.as_deref(), Some("My Mesh"));
        }
        other => panic!("expected a join link, got {other:?}"),
    }
}

#[test]
fn parse_https_simple() {
    let link = parse_https_join("https://sovereign.dev/join/cwth-7f3a-9b2e-4d1c").unwrap();
    let DeepLink::Join {
        join_key,
        relay_hint,
        mesh_name,
        ..
    } = link
    else {
        panic!("expected a join link")
    };
    assert_eq!(join_key, "cwth-7f3a-9b2e-4d1c");
    assert!(relay_hint.is_none());
    assert!(mesh_name.is_none());
}

#[test]
fn parse_https_with_params() {
    let link = parse_https_join(
        "https://sovereign.dev/join/cwth-7f3a-9b2e-4d1c?relay=10.0.0.5&name=Lab+Squad",
    )
    .unwrap();
    let DeepLink::Join {
        join_key,
        relay_hint,
        mesh_name,
        ..
    } = link
    else {
        panic!("expected a join link")
    };
    assert_eq!(join_key, "cwth-7f3a-9b2e-4d1c");
    assert_eq!(relay_hint.as_deref(), Some("10.0.0.5"));
    assert_eq!(mesh_name.as_deref(), Some("Lab Squad"));
}

#[test]
fn parse_https_rejects_wrong_host() {
    // Host must match SOVEREIGN_JOIN_HOST (or default). A random
    // https URL on a different domain must not be accepted.
    assert!(parse_https_join("https://evil.example.com/join/cwth-0000-1111-2222").is_none());
}

#[test]
fn parse_join_argument_accepts_bare_key() {
    let link = parse_join_argument("cwth-7f3a-9b2e-4d1c").unwrap();
    let DeepLink::Join { join_key, .. } = link else {
        panic!("expected a join link")
    };
    assert_eq!(join_key, "cwth-7f3a-9b2e-4d1c");
}

#[test]
fn parse_join_argument_accepts_https() {
    let link = parse_join_argument("https://sovereign.dev/join/cwth-abcd-ef01-2345").unwrap();
    let DeepLink::Join { join_key, .. } = link else {
        panic!("expected a join link")
    };
    assert_eq!(join_key, "cwth-abcd-ef01-2345");
}

#[test]
fn parse_join_argument_accepts_scheme() {
    let link = parse_join_argument("sovereign://join/cwth-1111-2222-3333").unwrap();
    let DeepLink::Join { join_key, .. } = link else {
        panic!("expected a join link")
    };
    assert_eq!(join_key, "cwth-1111-2222-3333");
}

#[test]
fn parse_join_argument_rejects_garbage() {
    assert!(parse_join_argument("").is_none());
    assert!(parse_join_argument("not-a-key").is_none());
    assert!(parse_join_argument("https://example.com/other").is_none());
}

#[test]
fn build_https_join_link_round_trip() {
    let url = build_https_join_link(
        "cwth-abcd-ef01-2345",
        Some("10.0.0.5"),
        Some("My Mesh"),
        None,
        false,
        None,
    );
    assert_eq!(
        url,
        "https://sovereign.dev/join/cwth-abcd-ef01-2345?relay=10.0.0.5&name=My+Mesh"
    );
    let link = parse_https_join(&url).unwrap();
    let DeepLink::Join {
        join_key,
        relay_hint,
        mesh_name,
        ..
    } = link
    else {
        panic!("expected a join link")
    };
    assert_eq!(join_key, "cwth-abcd-ef01-2345");
    assert_eq!(relay_hint.as_deref(), Some("10.0.0.5"));
    assert_eq!(mesh_name.as_deref(), Some("My Mesh"));
}

#[test]
fn parses_percent_encoded_relay_with_colon() {
    // Regression for a real user-reported link: the desktop UI
    // built the share link via `URL.searchParams.set("relay", ...)`,
    // which encodes `:` as `%3A`. Without decode the parser
    // handed `100.64.0.2%3A9742` to the join handshake as a
    // hostname, which failed with a misleading network error.
    let link = parse_deep_link(
        "sovereign://join/cwth-4d5f-6211-64d6?name=example-host.local%27s+Mesh&relay=100.64.0.2%3A9742"
    ).unwrap();
    let DeepLink::Join {
        join_key,
        relay_hint,
        mesh_name,
        ..
    } = link
    else {
        panic!("expected a join link")
    };
    assert_eq!(join_key, "cwth-4d5f-6211-64d6");
    assert_eq!(relay_hint.as_deref(), Some("100.64.0.2:9742"));
    assert_eq!(mesh_name.as_deref(), Some("example-host.local's Mesh"));
}

#[test]
fn parses_bracketed_ipv6_relay() {
    // IPv6 relays use bracket form — `[fd7a:...]:9742`. The
    // brackets themselves often get percent-encoded too.
    let link = parse_deep_link(
        "sovereign://join/cwth-7f3a-9b2e-4d1c?relay=%5Bfd7a%3A115c%3Aa1e0%3A%3Aa3a%3A241c%5D%3A9742"
    ).unwrap();
    let DeepLink::Join { relay_hint, .. } = link else {
        panic!("expected a join link")
    };
    assert_eq!(
        relay_hint.as_deref(),
        Some("[fd7a:115c:a1e0::a3a:241c]:9742")
    );
}

#[test]
fn percent_decode_passes_through_invalid_escapes() {
    // A lone `%` with no two hex digits shouldn't panic or drop
    // characters — we'd rather hand back a partial value than
    // reject the whole link. Guard for accidental truncation.
    assert_eq!(percent_decode("abc%"), "abc%");
    assert_eq!(percent_decode("abc%ZZ"), "abc%ZZ");
}

#[test]
fn percent_encode_round_trips_dial_string() {
    // The dial string carries every reserved char that breaks a
    // naive query value: `@ : / , .`. Encode → decode must be
    // identity, or the joiner dials a corrupt founder address.
    let dial = "aabbccddeeff00112233@https://relay.example./,10.0.0.5:9742,[fd7a::1]:9742";
    assert_eq!(percent_decode(&percent_encode(dial)), dial);
}

#[test]
fn https_form_round_trips_iroh_and_ttl() {
    let dial = "aabbccddeeff00112233@10.0.0.5:9742";
    let url = build_https_join_link(
        "cwth-1111-2222-3333",
        None,
        None,
        Some(dial),
        true,
        Some(42),
    );
    let DeepLink::Join {
        iroh_dial,
        encrypted,
        expires_at,
        ..
    } = parse_https_join(&url).unwrap()
    else {
        panic!("expected a join link")
    };
    assert_eq!(iroh_dial.as_deref(), Some(dial));
    assert!(encrypted);
    assert_eq!(expires_at, Some(42));
}

#[test]
fn plaintext_invite_has_no_iroh_or_exp() {
    // Back-compat: a plaintext invite (no dial) is byte-identical
    // to before and parses with iroh_dial/expires_at absent.
    let url = build_join_link(
        "cwth-abcd-efgh-ijkl",
        Some("10.0.0.5"),
        Some("My Mesh"),
        None,
        false,
        None,
    );
    assert_eq!(
        url,
        "sovereign://join/cwth-abcd-efgh-ijkl?relay=10.0.0.5&name=My+Mesh"
    );
    let DeepLink::Join {
        iroh_dial,
        encrypted,
        expires_at,
        ..
    } = parse_deep_link(&url).unwrap()
    else {
        panic!("expected a join link")
    };
    assert!(iroh_dial.is_none());
    assert!(!encrypted);
    assert!(expires_at.is_none());
}

// ── Guest links ────────────────────────────────────────────────
//
// A guest link is not an invite. These pin the two properties that make
// that true on the wire: it round-trips without carrying a scope, and it
// cannot be mistaken for something joinable.

#[test]
fn https_guest_link_round_trips_with_the_token_only_in_the_fragment() {
    let base = "http://192.168.1.10:9750/app/ring-doc/";
    let link = build_https_guest_link(
        "deadbeef",
        base,
        1_787_900_000,
        Some("rail:wall"),
        None,
        None,
        None,
    );
    let (before, fragment) = link.split_once('#').expect("a fragment");
    assert!(
        !before.contains("deadbeef"),
        "the token must never reach the path or query: {link}"
    );
    assert!(fragment.contains("token=deadbeef"), "{link}");
    let DeepLink::Guest {
        token,
        url,
        dial,
        at,
        expires_at,
        summary,
    } = parse_https_guest_link(&link).unwrap()
    else {
        panic!("expected a guest link")
    };
    assert_eq!(token, "deadbeef");
    assert_eq!(url, base);
    assert!(dial.is_none());
    assert!(at.is_none());
    assert_eq!(expires_at, 1_787_900_000);
    assert_eq!(summary.as_deref(), Some("rail:wall"));
    // A token outside the fragment is not a guest link.
    assert!(parse_https_guest_link("http://h/?token=deadbeef&exp=1").is_none());
}

/// The couriers ride the fragment beside the token and come back
/// verbatim: the marks (`at=`) and the dial string (`iroh=`) survive the
/// percent-encoder, and the built link is exactly these bytes.
#[test]
fn https_guest_link_round_trips_the_couriers_exactly() {
    let base = "http://192.168.1.10:9750/app/ring-doc/";
    let at = "aa11:1,bb22:2,cc33:3";
    let iroh = "3b1f0a@https://relay.example:443/,192.168.1.10:41234";
    let link = build_https_guest_link(
        "deadbeef",
        base,
        1_787_900_000,
        None,
        Some(at),
        Some(iroh),
        None,
    );
    assert_eq!(
        link,
        "http://192.168.1.10:9750/app/ring-doc/#token=deadbeef&exp=1787900000\
         &at=aa11%3A1%2Cbb22%3A2%2Ccc33%3A3\
         &iroh=3b1f0a%40https%3A%2F%2Frelay.example%3A443%2F%2C192.168.1.10%3A41234"
    );
    let DeepLink::Guest {
        at: marks, dial, ..
    } = parse_https_guest_link(&link).unwrap()
    else {
        panic!("expected a guest link")
    };
    assert_eq!(marks.as_deref(), Some(at));
    assert_eq!(dial.as_deref(), Some(iroh));
}

/// A call with neither courier produces bytes identical to the link this
/// builder minted before the params existed — every existing QR and
/// pasted link stays valid.
#[test]
fn https_guest_link_without_the_new_params_is_byte_identical() {
    assert_eq!(
        build_https_guest_link(
            "deadbeef",
            "http://192.168.1.10:9750/app/ring-doc/",
            1_787_900_000,
            Some("rail:wall"),
            None,
            None,
            None,
        ),
        "http://192.168.1.10:9750/app/ring-doc/#token=deadbeef&exp=1787900000&s=rail%3Awall"
    );
}

/// `path=` is the door route a bare runtime page must fetch, and it is
/// appended last and percent-encoded like every other courier. A caller
/// that does not pass one mints the same bytes as before (asserted
/// above), which is what keeps this addition from invalidating old QRs.
#[test]
fn https_guest_link_carries_the_door_route_in_path() {
    let link = build_https_guest_link(
        "deadbeef",
        "https://svrnme.sh/ring/",
        1_787_900_000,
        None,
        None,
        Some("3b1f0a@https://relay.example./,10.0.0.5:9742"),
        Some("/ring/ring-doc/"),
    );
    assert_eq!(
        link,
        "https://svrnme.sh/ring/#token=deadbeef&exp=1787900000\
         &iroh=3b1f0a%40https%3A%2F%2Frelay.example.%2F%2C10.0.0.5%3A9742\
         &path=%2Fring%2Fring-doc%2F"
    );
}

/// The marks are read from the fragment only: an `at=` in the query —
/// the part a browser sends to the server — is not the marks.
#[test]
fn an_at_outside_the_fragment_is_not_read() {
    let link = "http://h:9750/app/ring-doc/?at=aa11%3A7#token=deadbeef&exp=1";
    let DeepLink::Guest { at, .. } = parse_https_guest_link(link).unwrap() else {
        panic!("expected a guest link")
    };
    assert!(at.is_none());
}

/// Same rule for the dial string: an `iroh=` in the query is ignored.
#[test]
fn an_iroh_outside_the_fragment_is_not_read() {
    let link = "http://h:9750/app/ring-doc/?iroh=zz11%40relay%3A1#token=deadbeef&exp=1";
    let DeepLink::Guest { dial, .. } = parse_https_guest_link(link).unwrap() else {
        panic!("expected a guest link")
    };
    assert!(dial.is_none());
}

#[test]
fn guest_link_round_trips() {
    let url = build_guest_link(
        "deadbeef",
        "http://192.168.1.10:9741",
        None,
        1_787_900_000,
        Some("big-model, small-model"),
    );
    let DeepLink::Guest {
        token,
        url: base,
        dial,
        at,
        expires_at,
        summary,
    } = parse_deep_link(&url).unwrap()
    else {
        panic!("expected a guest link")
    };
    assert_eq!(token, "deadbeef");
    assert_eq!(base, "http://192.168.1.10:9741");
    assert!(dial.is_none(), "no dial was minted, so none comes back");
    assert!(at.is_none(), "the sovereign:// form carries no marks");
    assert_eq!(expires_at, 1_787_900_000);
    assert_eq!(summary.as_deref(), Some("big-model, small-model"));
}

/// The encrypted-mesh shape. The dial string's `@ , :` survive the
/// percent-encoding round trip — a mangled one produces a link that dials
/// nothing, and the failure lands on the guest's machine.
#[test]
fn a_guest_link_round_trips_the_dial_string_verbatim() {
    let dial = "3b1f0a@https://relay.example:443/,192.168.1.10:41234";
    let url = build_guest_link("tok", "http://192.168.1.10:9741", Some(dial), 42, None);
    let DeepLink::Guest {
        dial: parsed,
        url: base,
        ..
    } = parse_deep_link(&url).unwrap()
    else {
        panic!("expected a guest link")
    };
    assert_eq!(parsed.as_deref(), Some(dial));
    // `url` still rides along as provenance — nothing dials it while a
    // dial string is present, but the guest is told whose machine this is.
    assert_eq!(base, "http://192.168.1.10:9741");
}

/// An empty `dial=` is absent, not "dial the empty string". The latter
/// would take the iroh path and then fail to parse an endpoint, which
/// reports the wrong cause.
#[test]
fn an_empty_dial_param_is_absent_rather_than_a_target() {
    let DeepLink::Guest { dial, .. } =
        parse_deep_link("sovereign://guest/tok?url=http://h:9741&exp=9&dial=").unwrap()
    else {
        panic!("expected a guest link")
    };
    assert!(dial.is_none());
}

/// The link carries WHERE and WHEN, never WHAT. The issuing node's store
/// is the sole authority on scope — see `DeepLink::Guest::summary`.
#[test]
fn guest_link_carries_no_machine_readable_scope() {
    let url = build_guest_link("tok", "http://h:9741", None, 1, Some("big-model"));
    // The only place a model name appears is inside the opaque display
    // string. Nothing parses it, so nothing can act on it.
    assert!(!url.contains("m="), "no per-model param: {url}");
    assert!(!url.contains("scope"), "no scope param: {url}");
}

/// Both are load-bearing: no `url` means nowhere to send the bearer, no
/// `exp` means the link reads as permanent. Neither gets a default.
#[test]
fn a_guest_link_without_url_or_exp_is_malformed_not_defaulted() {
    assert!(parse_deep_link("sovereign://guest/tok?exp=123").is_none());
    assert!(parse_deep_link("sovereign://guest/tok?url=http://h:9741").is_none());
    assert!(parse_deep_link("sovereign://guest/tok").is_none());
    assert!(parse_deep_link("sovereign://guest/").is_none());
    // A non-numeric expiry is absent, not zero — zero would read as
    // "expired at the epoch", which is a different (and wrong) claim.
    assert!(parse_deep_link("sovereign://guest/tok?url=http://h&exp=soon").is_none());
}
