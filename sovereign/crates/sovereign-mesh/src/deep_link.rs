// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deep link parsing for `sovereign://join/...` URLs.
//!
//! Links carry the join key, optionally a relay hint for remote discovery,
//! and are registered as a URL scheme on install (macOS, Windows, Linux).
//!
//! Also accepts user-facing forms the CLI might receive:
//! - Bare join key: `cwth-XXXX-XXXX-XXXX` (typed into a terminal)
//! - HTTPS URL: `https://sovereign.dev/join/<key>` (click-from-email)
//! - Deep link: `sovereign://join/<key>` (click-from-native app)
//!
//! Use [`parse_join_argument`] when accepting user input; it tries all
//! three forms in order. The host for the HTTPS form is overridable via
//! the `SOVEREIGN_JOIN_HOST` environment variable (useful for dev /
//! testing against a staging domain).

use crate::types::JoinConfirmation;

/// Default host for the HTTPS join link form. Overridable via the
/// `SOVEREIGN_JOIN_HOST` environment variable.
const DEFAULT_HTTPS_HOST: &str = "sovereign.dev";

/// A parsed deep link.
#[derive(Debug, Clone)]
pub enum DeepLink {
    /// Join a mesh: `sovereign://join/<join-key>[?relay=<host>&name=<mesh-name>&iroh=<dial>|dial=<dial>&exp=<unix>]`
    Join {
        join_key: String,
        relay_hint: Option<String>,
        mesh_name: Option<String>,
        /// Founder's iroh dial string (`<hex-pubkey>@<relay-or-addr>[,…]`,
        /// see `commonwealth_transport::iroh::format_dial_string`). Which
        /// query param carried it decides the join posture (`encrypted`
        /// below): `iroh=` ⇒ encrypted mesh, fail-closed key-dialed join;
        /// `dial=` ⇒ plaintext mesh, prefer-iroh join with IP/mDNS
        /// fallback. `None` ⇒ legacy invite, joined over plain HTTP.
        ///
        /// The params are deliberately DISTINCT: a pre-`dial=` build
        /// treats `iroh=` as "this mesh is encrypted" and joins
        /// fail-closed — reusing `iroh=` for plaintext invites would
        /// wedge old joiners into encrypted posture against a plaintext
        /// mesh. Old builds simply ignore the unknown `dial=` param and
        /// degrade to the legacy IP/mDNS join.
        iroh_dial: Option<String>,
        /// True iff the dial string arrived via `iroh=` — the invite is
        /// for an ENCRYPTED mesh (the join tunnels the key over QUIC
        /// and never falls back to plaintext). Meaningless when
        /// `iroh_dial` is `None`.
        encrypted: bool,
        /// Unix-seconds after which this invite is no longer accepted
        /// (short-lived TTL). Display-only on the joiner; the founder is
        /// the authority and rejects an expired key at the join handler.
        /// `None` ⇒ no expiry (legacy / plaintext mesh).
        expires_at: Option<u64>,
    },
    /// Use a mesh node's models as a GUEST: `sovereign://guest/<token>?url=…&exp=…&s=…`
    ///
    /// Not a join. The holder never becomes a member, never receives
    /// `mesh_secret`, and never learns an invite key — they present `token` as
    /// an `Authorization: Bearer` to `url` and get exactly what the issuing
    /// node's grant says, for as long as it says.
    Guest {
        /// The bearer. Opaque here — this crate never interprets it.
        token: String,
        /// Base URL of the issuing node's client API, no trailing `/v1`.
        url: String,
        /// Unix-seconds the grant lapses. Display + a local pre-flight so
        /// `mesh use` can refuse a dead link without a round-trip; the ISSUING
        /// NODE is the authority and rejects an expired token regardless.
        expires_at: u64,
        /// **Display only.** What the minting node said this buys, so `mesh
        /// use` can print it without a round-trip.
        ///
        /// Deliberately one opaque string rather than structured params: the
        /// grant's scope lives in the issuer's store, and a second
        /// machine-readable copy on the wire would be a second answer to "what
        /// does this link permit" (§10.6) — one that travels through a
        /// clipboard and can be edited. A link claiming more than the grant
        /// gives changes nothing; the request is still refused.
        ///
        /// This is also why a future scope needs no wire change at all.
        summary: Option<String>,
    },
}

/// Parse a `sovereign://` deep link URL.
///
/// Supported formats:
/// - `sovereign://join/cwth-7f3a-9b2e-4d1c`
/// - `sovereign://join/cwth-7f3a-9b2e-4d1c?relay=192.168.1.100&name=Lab+Squad`
pub fn parse_deep_link(url: &str) -> Option<DeepLink> {
    let stripped = url
        .strip_prefix("sovereign://")
        .or_else(|| url.strip_prefix("sovereign:"))?;
    let stripped = stripped.trim_start_matches('/');

    // Split path and query.
    let (path, query) = match stripped.find('?') {
        Some(idx) => (&stripped[..idx], Some(&stripped[idx + 1..])),
        None => (stripped, None),
    };

    let parts: Vec<&str> = path.split('/').collect();
    if parts.is_empty() {
        return None;
    }

    match parts[0] {
        "join" if parts.len() >= 2 => {
            let join_key = parts[1].to_string();
            let params = parse_query_params(query);
            let relay_hint = params.get("relay").cloned();
            let mesh_name = params.get("name").map(|n| n.replace('+', " "));
            // `iroh` / `dial` / `exp` are already percent-decoded by
            // `parse_query_params`, so the dial string's `@ , : /` come
            // back verbatim. A non-numeric `exp` is treated as absent.
            // `iroh=` (encrypted) wins if both params are ever present.
            let (iroh_dial, encrypted) = dial_from_params(&params);
            let expires_at = params.get("exp").and_then(|s| s.parse::<u64>().ok());

            Some(DeepLink::Join {
                join_key,
                relay_hint,
                mesh_name,
                iroh_dial,
                encrypted,
                expires_at,
            })
        }
        // A guest link REQUIRES `url` and `exp`. Neither is optional the way
        // join's hints are: without `url` there is nowhere to send the bearer,
        // and without `exp` the link would read as permanent, which is the one
        // thing an ephemeral grant must never look like. Missing either is a
        // malformed link, not a link with defaults (§18.3).
        "guest" if parts.len() >= 2 => {
            let token = parts[1].to_string();
            if token.is_empty() {
                return None;
            }
            let params = parse_query_params(query);
            let url = params.get("url")?.trim_end_matches('/').to_string();
            if url.is_empty() {
                return None;
            }
            let expires_at = params.get("exp").and_then(|s| s.parse::<u64>().ok())?;
            Some(DeepLink::Guest {
                token,
                url,
                expires_at,
                summary: params.get("s").map(|s| s.replace('+', " ")),
            })
        }
        _ => None,
    }
}

/// Build a `sovereign://guest/…` link. Mirror of [`build_join_link`].
///
/// Carries the bearer, where to send it, and when it dies — and one human
/// string. Not the scope: see [`DeepLink::Guest::summary`].
pub fn build_guest_link(token: &str, url: &str, expires_at: u64, summary: Option<&str>) -> String {
    let mut link = format!("sovereign://guest/{token}");
    let mut params = vec![
        format!("url={}", percent_encode(url.trim_end_matches('/'))),
        format!("exp={expires_at}"),
    ];
    if let Some(s) = summary {
        params.push(format!("s={}", percent_encode(s).replace(' ', "+")));
    }
    link.push('?');
    link.push_str(&params.join("&"));
    link
}

/// Parse an HTTPS join URL: `https://sovereign.dev/join/<key>[?...]`.
///
/// The host is validated against `SOVEREIGN_JOIN_HOST` (or the
/// default) so pasting a random https URL doesn't silently mis-route.
/// Query parameters (`relay`, `name`) share semantics with the
/// `sovereign://` scheme.
pub fn parse_https_join(url: &str) -> Option<DeepLink> {
    let expected_host =
        std::env::var("SOVEREIGN_JOIN_HOST").unwrap_or_else(|_| DEFAULT_HTTPS_HOST.to_string());
    let prefix = format!("https://{expected_host}/join/");
    let stripped = url.strip_prefix(&prefix)?;

    // Reuse the same path/query split as parse_deep_link so semantics
    // stay consistent across transports.
    let (path, query) = match stripped.find('?') {
        Some(idx) => (&stripped[..idx], Some(&stripped[idx + 1..])),
        None => (stripped, None),
    };

    // The key may end at the first `/` if the URL has extra path
    // segments (defensive; the canonical form has exactly one segment).
    let join_key = path.split('/').next()?;
    if join_key.is_empty() {
        return None;
    }

    let params = parse_query_params(query);
    let (iroh_dial, encrypted) = dial_from_params(&params);
    Some(DeepLink::Join {
        join_key: join_key.to_string(),
        relay_hint: params.get("relay").cloned(),
        mesh_name: params.get("name").map(|n| n.replace('+', " ")),
        iroh_dial,
        encrypted,
        expires_at: params.get("exp").and_then(|s| s.parse::<u64>().ok()),
    })
}

/// Shared dial-param extraction: `iroh=` carries an encrypted-mesh
/// dial (fail-closed join), `dial=` a plaintext-mesh dial (fail-soft).
/// `iroh=` wins if both are present (never emitted; defensive).
fn dial_from_params(params: &std::collections::HashMap<String, String>) -> (Option<String>, bool) {
    if let Some(d) = params.get("iroh") {
        (Some(d.clone()), true)
    } else {
        (params.get("dial").cloned(), false)
    }
}

/// Accept any of the three user-facing join forms (bare key, HTTPS URL,
/// or `sovereign://` deep link) and return a [`DeepLink::Join`]. Returns
/// `None` if the input doesn't match any known form.
///
/// Order of attempt:
/// 1. `parse_deep_link` (covers the `sovereign://` scheme)
/// 2. `parse_https_join` (covers the https URL form)
/// 3. `validate_join_key_format` (covers a bare `cwth-XXXX-XXXX-XXXX`)
pub fn parse_join_argument(arg: &str) -> Option<DeepLink> {
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Filter to `Join`, don't just forward what parsed. This function's
    // contract is "a join argument", and `parse_deep_link` now answers a wider
    // question — a guest link parses fine and is not a join. Forwarding it
    // would put a `Guest` where every caller's name says `Join`, and the
    // callers that destructure would panic or, worse, the ones that match
    // loosely would treat a guest as a member. The enum makes that
    // representable; this is where it gets refused.
    if let Some(link) = parse_deep_link(trimmed) {
        return matches!(link, DeepLink::Join { .. }).then_some(link);
    }
    if let Some(link) = parse_https_join(trimmed) {
        return Some(link);
    }

    // Bare key fallback: validate format to avoid silently accepting
    // arbitrary strings.
    if commonwealth_discovery::membership::validate_join_key_format(trimmed).is_ok() {
        return Some(DeepLink::Join {
            join_key: trimmed.to_string(),
            relay_hint: None,
            mesh_name: None,
            iroh_dial: None,
            encrypted: false,
            expires_at: None,
        });
    }
    None
}

/// Build an HTTPS join link for the shareable URL form.
/// Host defaults to `sovereign.dev` (override via `SOVEREIGN_JOIN_HOST`).
pub fn build_https_join_link(
    join_key: &str,
    relay_hint: Option<&str>,
    mesh_name: Option<&str>,
    iroh_dial: Option<&str>,
    encrypted: bool,
    expires_at: Option<u64>,
) -> String {
    let host =
        std::env::var("SOVEREIGN_JOIN_HOST").unwrap_or_else(|_| DEFAULT_HTTPS_HOST.to_string());
    let mut url = format!("https://{host}/join/{join_key}");
    let params = join_query_params(relay_hint, mesh_name, iroh_dial, encrypted, expires_at);
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }
    url
}

/// Build a deep link URL for joining a mesh. `iroh_dial` is the
/// founder's dial-by-key string; `encrypted` picks the param that
/// carries it (`iroh=` for an encrypted mesh — fail-closed join;
/// `dial=` for a plaintext mesh — prefer-iroh join with IP fallback).
/// Callers passing `None` for the dial get the historical URL
/// byte-for-byte.
pub fn build_join_link(
    join_key: &str,
    relay_hint: Option<&str>,
    mesh_name: Option<&str>,
    iroh_dial: Option<&str>,
    encrypted: bool,
    expires_at: Option<u64>,
) -> String {
    let mut url = format!("sovereign://join/{join_key}");
    let params = join_query_params(relay_hint, mesh_name, iroh_dial, encrypted, expires_at);
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }
    url
}

/// Shared query-param builder for both link forms. The dial string
/// carries `@ , : /` which a clipboard round-trip through `URL`
/// percent-escapes, so we encode it on the way out; `parse_query_params`
/// percent-decodes it back. `mesh_name` keeps the historical
/// space-as-`+` minimal encoding for backward-compatible URLs.
fn join_query_params(
    relay_hint: Option<&str>,
    mesh_name: Option<&str>,
    iroh_dial: Option<&str>,
    encrypted: bool,
    expires_at: Option<u64>,
) -> Vec<String> {
    let mut params = Vec::new();
    if let Some(relay) = relay_hint {
        params.push(format!("relay={relay}"));
    }
    if let Some(name) = mesh_name {
        params.push(format!("name={}", name.replace(' ', "+")));
    }
    if let Some(dial) = iroh_dial {
        let param = if encrypted { "iroh" } else { "dial" };
        params.push(format!("{param}={}", percent_encode(dial)));
    }
    if let Some(exp) = expires_at {
        params.push(format!("exp={exp}"));
    }
    params
}

/// Build a join confirmation from a parsed deep link.
pub fn join_confirmation_from_link(link: &DeepLink) -> Option<JoinConfirmation> {
    match link {
        DeepLink::Join {
            join_key,
            relay_hint,
            mesh_name,
            iroh_dial,
            encrypted,
            expires_at,
        } => Some(JoinConfirmation {
            mesh_name: mesh_name
                .clone()
                .unwrap_or_else(|| "Unknown Mesh".to_string()),
            invited_by: None,
            join_key: join_key.clone(),
            relay_hint: relay_hint.clone(),
            iroh_dial: iroh_dial.clone(),
            encrypted: *encrypted,
            expires_at: *expires_at,
        }),
        // A guest link is not a join and must never be confirmable as one:
        // accepting it here would walk a guest into the membership flow, which
        // is the exact conflation this whole surface exists to prevent.
        DeepLink::Guest { .. } => None,
    }
}

fn parse_query_params(query: Option<&str>) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Some(q) = query {
        for pair in q.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                map.insert(percent_decode(key), percent_decode(value));
            }
        }
    }
    map
}

/// Best-effort percent-decoder for query values.
///
/// Browsers and most clipboard-capable chat clients round-trip URLs
/// through `URL`/`encodeURIComponent`, which percent-escapes reserved
/// characters the builder didn't bother to encode — most importantly
/// the `:` in `relay=100.64.0.2:9742` (becomes `%3A`) and `'` in
/// mesh names (becomes `%27`). Without this decode the parser treats
/// `100.64.0.2%3A9742` as a DNS hostname, the join handshake
/// fails, and the user sees a "no peer at that address" error that
/// looks like a networking problem but is actually an encoding one.
///
/// Scope: handles `%XX` sequences and translates `+` to space (the
/// traditional HTML form-encoding convention — existing code already
/// does this for the `name` param). Invalid escapes pass through as
/// literals rather than erroring, because our input is user-pasted
/// and a partial-success link is better than a hard reject.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push(((h << 4) | l) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    // Decoded bytes are only valid UTF-8 if the original encoder
    // produced valid UTF-8 — true for anything `encodeURIComponent`
    // emits. On malformed input, fall back to lossy conversion so we
    // still return something the caller can pattern-match.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Percent-encode a query value — the inverse of [`percent_decode`].
/// Encodes everything outside the unreserved set (`ALPHA DIGIT - . _ ~`),
/// so the iroh dial string's `@ , : /` are escaped on the way out and
/// `parse_query_params` decodes them back verbatim.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
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
        let link = parse_deep_link(
            "sovereign://join/cwth-7f3a-9b2e-4d1c?relay=192.168.1.100&name=Lab+Squad",
        )
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
    fn join_confirmation_from_parsed_link() {
        let link = DeepLink::Join {
            join_key: "cwth-test".into(),
            relay_hint: Some("relay.example.com".into()),
            mesh_name: Some("Test Mesh".into()),
            iroh_dial: None,
            encrypted: false,
            expires_at: None,
        };
        let confirm = join_confirmation_from_link(&link).unwrap();
        assert_eq!(confirm.mesh_name, "Test Mesh");
        assert_eq!(confirm.join_key, "cwth-test");
        assert_eq!(confirm.relay_hint.as_deref(), Some("relay.example.com"));
        assert!(confirm.iroh_dial.is_none());
        assert!(confirm.expires_at.is_none());
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
    fn build_and_parse_round_trip_with_iroh_and_ttl() {
        // An encrypted-mesh invite carries the founder's dial string
        // and a TTL. Both must survive a build → (clipboard-style
        // percent round-trip) → parse cycle verbatim.
        let dial = "aabbccddeeff00112233@https://relay.example./,10.0.0.5:9742";
        let url = build_join_link(
            "cwth-abcd-efgh-ijkl",
            None,
            Some("Secure Mesh"),
            Some(dial),
            true,
            Some(1_900_000_000),
        );
        // The dial string is percent-encoded in the URL (no bare `@`/`/`).
        assert!(url.contains("iroh=aabbcc"));
        assert!(url.contains("%40")); // '@' encoded
        assert!(url.contains("exp=1900000000"));

        let link = parse_deep_link(&url).unwrap();
        let DeepLink::Join {
            join_key,
            iroh_dial,
            encrypted,
            expires_at,
            mesh_name,
            ..
        } = link
        else {
            panic!("expected a join link")
        };
        assert_eq!(join_key, "cwth-abcd-efgh-ijkl");
        assert_eq!(iroh_dial.as_deref(), Some(dial));
        assert!(encrypted);
        assert_eq!(expires_at, Some(1_900_000_000));
        assert_eq!(mesh_name.as_deref(), Some("Secure Mesh"));

        // And the same fields reach the JoinConfirmation the UI shows.
        let confirm = join_confirmation_from_link(&parse_deep_link(&url).unwrap()).unwrap();
        assert_eq!(confirm.iroh_dial.as_deref(), Some(dial));
        assert!(confirm.encrypted);
        assert_eq!(confirm.expires_at, Some(1_900_000_000));
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
    fn plaintext_dial_param_round_trips_unencrypted() {
        // A plaintext mesh's no-VPN invite uses `dial=`, NOT `iroh=` —
        // an old build must not mistake it for an encrypted invite
        // (it ignores the unknown param and joins over IP/mDNS), and a
        // new build must parse it as encrypted=false.
        let dial = "aabbccddeeff00112233@https://relay.example./,10.0.0.5:9742";
        let url = build_join_link(
            "cwth-abcd-efgh-ijkl",
            None,
            Some("House Mesh"),
            Some(dial),
            false,
            None,
        );
        assert!(url.contains("dial=aabbcc"));
        assert!(!url.contains("iroh="));
        assert!(!url.contains("exp="));

        let DeepLink::Join {
            iroh_dial,
            encrypted,
            expires_at,
            ..
        } = parse_deep_link(&url).unwrap()
        else {
            panic!("expected a join link")
        };
        assert_eq!(iroh_dial.as_deref(), Some(dial));
        assert!(!encrypted);
        assert!(expires_at.is_none());

        let confirm = join_confirmation_from_link(&parse_deep_link(&url).unwrap()).unwrap();
        assert_eq!(confirm.iroh_dial.as_deref(), Some(dial));
        assert!(!confirm.encrypted);

        // Same through the https form.
        let https =
            build_https_join_link("cwth-1111-2222-3333", None, None, Some(dial), false, None);
        let DeepLink::Join {
            iroh_dial,
            encrypted,
            ..
        } = parse_https_join(&https).unwrap()
        else {
            panic!("expected a join link")
        };
        assert_eq!(iroh_dial.as_deref(), Some(dial));
        assert!(!encrypted);
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
    fn guest_link_round_trips() {
        let url = build_guest_link(
            "deadbeef",
            "http://192.168.1.10:9741",
            1_787_900_000,
            Some("big-model, small-model"),
        );
        let DeepLink::Guest {
            token,
            url: base,
            expires_at,
            summary,
        } = parse_deep_link(&url).unwrap()
        else {
            panic!("expected a guest link")
        };
        assert_eq!(token, "deadbeef");
        assert_eq!(base, "http://192.168.1.10:9741");
        assert_eq!(expires_at, 1_787_900_000);
        assert_eq!(summary.as_deref(), Some("big-model, small-model"));
    }

    /// The link carries WHERE and WHEN, never WHAT. The issuing node's store
    /// is the sole authority on scope — see `DeepLink::Guest::summary`.
    #[test]
    fn guest_link_carries_no_machine_readable_scope() {
        let url = build_guest_link("tok", "http://h:9741", 1, Some("big-model"));
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

    /// The guardrail against the conflation this feature exists to prevent:
    /// a guest link must never walk into the membership flow.
    #[test]
    fn a_guest_link_is_not_joinable() {
        let url = build_guest_link("tok", "http://h:9741", 1, None);
        let link = parse_deep_link(&url).unwrap();
        assert!(
            join_confirmation_from_link(&link).is_none(),
            "a guest link must not be confirmable as a join"
        );
        // And it is not accepted by the join-argument parser at all, so
        // `svrn mesh join <guest link>` cannot silently half-work.
        assert!(parse_join_argument(&url).is_none());
    }
}
