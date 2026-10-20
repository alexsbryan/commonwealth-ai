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
//!
//! Moved here from `sovereign-mesh` on 2026-09-11: a lifted process — the
//! rails daemon — has to read an invite, and reading one needs nothing from
//! the agent runtime. `sovereign_mesh::deep_link` re-exports every name
//! below, and keeps `join_confirmation_from_link`, whose `JoinConfirmation`
//! is that crate's own wire type.

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
        ///
        /// Where the bearer goes **when `dial` is absent**. When `dial` is
        /// present this is provenance and display only — it names the machine
        /// that lent you something, and is never dialled. See [`Self::Guest`]'s
        /// `dial` field for why there is exactly one path, not a preference
        /// order.
        url: String,
        /// The lender's iroh dial string (`<hex-id>@<relay>[,<addr>…]`), when
        /// the plaintext client API is not the way in.
        ///
        /// An ENCRYPTED mesh forces its client listener loopback-only, so the
        /// address in `url` answers nothing — the sole ingress is the iroh
        /// acceptor. `mesh use` dials this, on `GUEST_ALPN`, and tunnels to
        /// the lender's bearer-checking guest listener.
        ///
        /// **Present means authoritative.** There is no fallback to `url`: a
        /// tunnel that fails is reported, never quietly downgraded to
        /// plaintext against a mesh that asked for encryption (§18.3). Old
        /// builds ignore the unknown param and fail on the unreachable `url`,
        /// which is the honest outcome for them.
        ///
        /// Additive, and the same shape a join link's `dial=` already uses.
        /// On the https fragment form the same string rides under the param
        /// name `iroh=` — the join link's dial param.
        dial: Option<String>,
        /// The issuing ring's digest marks (`<actor-hex>:<n>` pairs,
        /// comma-separated), when the link names the state a guest should
        /// expect. Opaque here, like `token`: this crate never interprets
        /// the marks, and they are never the payload — the checkpoint
        /// document is fetched separately. Present only on the https
        /// fragment form; `None` for the `sovereign://guest` form.
        at: Option<String>,
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
            // `dial=` only — never `iroh=`. A guest link has no plaintext /
            // encrypted split to resolve: guest traffic rides `GUEST_ALPN` or
            // it rides HTTP, and the presence of this param is exactly which.
            let dial = params
                .get("dial")
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty());
            Some(DeepLink::Guest {
                token,
                url,
                dial,
                at: None,
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
pub fn build_guest_link(
    token: &str,
    url: &str,
    dial: Option<&str>,
    expires_at: u64,
    summary: Option<&str>,
) -> String {
    let mut link = format!("sovereign://guest/{token}");
    let mut params = vec![
        format!("url={}", percent_encode(url.trim_end_matches('/'))),
        format!("exp={expires_at}"),
    ];
    if let Some(d) = dial {
        params.push(format!("dial={}", percent_encode(d)));
    }
    if let Some(s) = summary {
        params.push(format!("s={}", percent_encode(s).replace(' ', "+")));
    }
    link.push('?');
    link.push_str(&params.join("&"));
    link
}

/// Build the https form of a guest link:
/// `<base>#token=…&exp=…[&s=…][&at=…&iroh=…]`.
///
/// What a phone opens. The bearer rides the URL FRAGMENT, never the path or
/// query: a browser does not send the fragment to the server, so the token
/// stays out of access logs and `Referer` headers, and only the page's own
/// script reads it. `base` is kept verbatim — it is the page's address.
///
/// The couriers ride the fragment beside the token, on the same grounds:
/// `at` is the ring's digest marks (`<actor-hex>:<n>` pairs) and `iroh` the
/// lender's dial string (the join link's `iroh=` param). Both are appended
/// only when present — a call passing neither produces bytes identical to
/// the links this minted before the params existed. `path` is the DOOR route
/// a runtime page whose base is a bare page origin must fetch (the base
/// cannot spell both the runtime page and the app); appended last, so a call
/// that does not pass it is also byte-identical to before.
pub fn build_https_guest_link(
    token: &str,
    base: &str,
    expires_at: u64,
    summary: Option<&str>,
    at: Option<&str>,
    iroh: Option<&str>,
    path: Option<&str>,
) -> String {
    let mut params = vec![
        format!("token={}", percent_encode(token)),
        format!("exp={expires_at}"),
    ];
    if let Some(s) = summary {
        params.push(format!("s={}", percent_encode(s).replace(' ', "+")));
    }
    if let Some(a) = at {
        params.push(format!("at={}", percent_encode(a)));
    }
    if let Some(d) = iroh {
        params.push(format!("iroh={}", percent_encode(d)));
    }
    if let Some(p) = path {
        params.push(format!("path={}", percent_encode(p)));
    }
    format!("{base}#{}", params.join("&"))
}

/// Parse the https form built by [`build_https_guest_link`] into a
/// [`DeepLink::Guest`] whose `url` is the part before the fragment and whose
/// `at` and `dial` come from the fragment's `at=` / `iroh=` (or are `None`).
/// `token` and `exp` are required, as for the `sovereign://guest` form; one
/// rule covers every protected param — `token`, `at`, `iroh` — anywhere but
/// the fragment is not read, because the parser never looks left of the `#`.
pub fn parse_https_guest_link(link: &str) -> Option<DeepLink> {
    let (base, fragment) = link.split_once('#')?;
    if !(base.starts_with("https://") || base.starts_with("http://")) {
        return None;
    }
    let params = parse_query_params(Some(fragment));
    let token = params.get("token").filter(|t| !t.is_empty())?.clone();
    let expires_at = params.get("exp").and_then(|s| s.parse::<u64>().ok())?;
    Some(DeepLink::Guest {
        token,
        url: base.to_string(),
        dial: params
            .get("iroh")
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty()),
        at: params
            .get("at")
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty()),
        expires_at,
        summary: params.get("s").cloned(),
    })
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
    if crate::join_key::validate_join_key_format(trimmed).is_ok() {
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
#[path = "tests/deep_link.rs"]
mod tests;
