// SPDX-License-Identifier: AGPL-3.0-or-later
//! Who is asking — the ONE resolver from an HTTP request to a fairness
//! principal on the client surface (`:9741`).
//!
//! ## Why this exists
//!
//! `MESH_SCALE_100_USERS_1000_CORPORA.md §9.3` measured a daemon that carries
//! a distinct bearer token and a distinct `X-Principal` for ten different
//! callers and behaves *exactly* as if all ten shared one credential: the
//! greedy caller was admitted 102 turns in all three same-path runs, and the
//! polite cohort held 19.0% of service against a 90% population share. The
//! credential was verified and discarded. Nothing downstream had a parameter
//! it could have been passed in.
//!
//! This module is the missing parameter. It answers one question — *which
//! principal is this request from* — and it is the only place in the client
//! surface that answers it.
//!
//! ## This is NOT `sovereign_contracts::PrincipalResolver`
//!
//! `sovereign-contracts/src/traits.rs:106` defines a trait of the same shape
//! and a deliberately different subject: it maps a **conversation id** to a
//! principal, and is consumed only by corpus visibility
//! (`runtime/retrieval/corpus_search.rs`). `/v1/chat/completions` is stateless
//! and carries no conversation id, so that seam is structurally unreachable
//! from here — it cannot be reused, and overloading it would put two
//! different questions behind one name. The distinction is deliberate; see
//! §9.3's third consequence.
//!
//! ## Resolution order
//!
//! Settled by operator intake (note `c874c318`): a principal anchors on an
//! **existing** surface, never on a new identity scheme.
//!
//! 1. **A presented `Authorization: Bearer` credential.** Keyed by a
//!    fingerprint, never by the secret itself, so a principal key is safe to
//!    log and safe to hold in a map. This branch also covers the owner-signed
//!    `WorkerToken` the order names: a worker token rides as a plain bearer
//!    (`sovereign-mesh/src/pinned_transport.rs:128`), so it needs no branch of
//!    its own — one decider, not two.
//! 2. **`X-Principal`, from a loopback caller only.** The local multi-caller
//!    case: desktop, CLI and MCP all reach `127.0.0.1` and are all
//!    auth-exempt (`client_auth.rs:143`), so a self-declared name is the only
//!    identity they can offer. Deliberately **not** honoured from a remote
//!    caller: remote callers all authenticate with the one daemon-wide token,
//!    so honouring a self-declared name there would let a remote caller mint
//!    unlimited principals by rotating a header and escape rationing
//!    entirely. Pinning a remote caller to its credential is the stricter
//!    reading and the safe one.
//! 3. **[`PrincipalKey::Anonymous`].** Nothing was presented. One shared
//!    bucket — which is exactly what these callers are *today*, so this is
//!    the no-change branch, not a new grouping.
//!
//! ## Two limits this resolver does not close, named rather than defaulted
//!
//! - **Remote callers collapse into one bucket** (§9.3 site #3). The client
//!   token is a daemon-wide secret, so every authenticated remote caller
//!   fingerprints identically. Per-caller remote identity needs per-caller
//!   tokens, which is an auth change, not a scheduling one.
//! - **The key is only as honest as the header.** A loopback caller can
//!   rotate its bearer or its `X-Principal` and mint fresh principals. This
//!   is a *fairness* key, not an *authorization* key — the same trust posture
//!   `X-Node-Id` already has on the peer gate (`admission.rs:289`). It is
//!   sufficient for the cooperative-local case it is built for and it must
//!   never be load-bearing for access control.

use std::hash::{Hash, Hasher};
use std::net::SocketAddr;

use axum::http::HeaderMap;

/// `X-Principal` values longer than this are fingerprinted rather than kept
/// verbatim. A principal key becomes a map key and a log field, so an
/// unbounded header value would be an unbounded allocation on the hot path.
/// Truncating instead would silently merge two distinct callers sharing a
/// long prefix — a fingerprint keeps them apart.
const MAX_DECLARED_PRINCIPAL_LEN: usize = 128;

/// The header a local caller uses to name itself.
pub const PRINCIPAL_HEADER: &str = "x-principal";

/// A fairness bucket. `Eq + Hash` because this is a `SchedCore` key.
///
/// Identity comes from what the caller *presented*, never from where it
/// connected from: ARCH_PRINCIPLES §7.5 forbids deriving a key from an
/// address or a counter, which rules out the per-connection `SocketAddr`
/// fallback that would otherwise be the obvious third branch. An
/// unidentified caller is [`Anonymous`](Self::Anonymous) — one honest bucket
/// — rather than a fleet of buckets minted from ephemeral port numbers, which
/// would hand every caller a fresh identity per TCP connection and defeat the
/// cap outright.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PrincipalKey {
    /// A presented bearer credential, by fingerprint. Covers the daemon
    /// client token, a per-caller bearer, and an owner-signed `WorkerToken`.
    Credential(String),
    /// A loopback caller's self-declared `X-Principal`.
    Declared(String),
    /// Nothing presented. Every such caller shares this one bucket.
    Anonymous,
}

impl PrincipalKey {
    /// Stable, non-secret rendering for logs and `/status`. A `Credential`
    /// renders as its fingerprint, so the token never reaches a log line.
    pub fn label(&self) -> String {
        match self {
            Self::Credential(fp) => format!("cred:{fp}"),
            Self::Declared(name) => format!("decl:{name}"),
            Self::Anonymous => "anon".to_string(),
        }
    }
}

impl std::fmt::Display for PrincipalKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}

/// Which branch of the resolution order produced the key. Carried separately
/// from the key so a `debug` line can say *how* a caller was identified
/// without parsing the label back apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrincipalSource {
    /// A bearer credential was presented (branch 1).
    Credential,
    /// A loopback caller declared `X-Principal` (branch 2).
    Declared,
    /// Nothing was presented (branch 3).
    Absent,
}

impl PrincipalSource {
    /// Short tracing-friendly name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Credential => "credential",
            Self::Declared => "declared",
            Self::Absent => "absent",
        }
    }
}

/// A resolved principal plus the branch that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPrincipal {
    pub key: PrincipalKey,
    pub source: PrincipalSource,
}

/// Non-cryptographic fingerprint of a secret, for use as a bucket key.
///
/// Deliberately NOT a security primitive and deliberately dependency-free:
/// the only property required is that two different tokens almost never share
/// a bucket, and that the token itself never appears in a log or a map key.
/// `DefaultHasher` is SipHash-1-3 with fixed keys, so the value is stable for
/// the life of a process — which is all a live fairness bucket needs.
fn fingerprint(secret: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    secret.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Extract a bearer token from an `Authorization` header value. Same
/// scheme-insensitive parse as [`crate::client_auth`]'s — kept here as a
/// header read rather than shared, because that one is part of a
/// constant-time credential check and this one must never be mistaken for it.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let rest = value.strip_prefix("Bearer ").or_else(|| {
        let (scheme, rest) = value.split_once(' ')?;
        scheme.eq_ignore_ascii_case("bearer").then_some(rest)
    })?;
    let token = rest.trim();
    (!token.is_empty()).then_some(token)
}

/// THE resolver: request headers + where the connection came from → the
/// principal this request's turns are charged to.
///
/// `peer` is the real `ConnectInfo<SocketAddr>` address, not a header — the
/// same source `client_auth` decides loopback from, and for the same reason
/// (`client_auth.rs:17-22`: the old header-keyed split made "omit the header"
/// a full-trust bypass). `None` means the listener did not wire
/// `ConnectInfo`, which `client_auth` already fails closed on; here it is
/// treated as *not* loopback, the stricter reading.
pub fn resolve_principal(headers: &HeaderMap, peer: Option<SocketAddr>) -> ResolvedPrincipal {
    if let Some(token) = bearer(headers) {
        return ResolvedPrincipal {
            key: PrincipalKey::Credential(fingerprint(token)),
            source: PrincipalSource::Credential,
        };
    }

    let from_loopback = peer.is_some_and(|p| p.ip().is_loopback());
    if from_loopback {
        if let Some(declared) = headers
            .get(PRINCIPAL_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            let name = if declared.len() > MAX_DECLARED_PRINCIPAL_LEN {
                fingerprint(declared)
            } else {
                declared.to_string()
            };
            return ResolvedPrincipal {
                key: PrincipalKey::Declared(name),
                source: PrincipalSource::Declared,
            };
        }
    }

    ResolvedPrincipal {
        key: PrincipalKey::Anonymous,
        source: PrincipalSource::Absent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    fn loopback() -> Option<SocketAddr> {
        Some("127.0.0.1:51000".parse().unwrap())
    }

    fn remote() -> Option<SocketAddr> {
        Some("10.1.2.3:51000".parse().unwrap())
    }

    #[test]
    fn absent_identity_resolves_to_one_shared_anonymous_bucket() {
        let a = resolve_principal(&headers(&[]), loopback());
        let b = resolve_principal(&headers(&[]), remote());
        let c = resolve_principal(&headers(&[]), None);
        assert_eq!(a.key, PrincipalKey::Anonymous);
        assert_eq!(a.source, PrincipalSource::Absent);
        assert_eq!(a.key, b.key, "an unidentified caller is one bucket");
        assert_eq!(a.key, c.key, "including when ConnectInfo is missing");
    }

    #[test]
    fn a_bearer_credential_keys_the_principal_and_never_leaks_the_secret() {
        let r = resolve_principal(
            &headers(&[("authorization", "Bearer super-secret-token")]),
            loopback(),
        );
        assert_eq!(r.source, PrincipalSource::Credential);
        let label = r.key.label();
        assert!(
            !label.contains("super-secret-token"),
            "the token must never appear in a loggable key: {label}"
        );
        assert!(label.starts_with("cred:"));
    }

    #[test]
    fn distinct_bearers_are_distinct_principals_and_equal_ones_collide() {
        // This is precisely what §9.3 measured as absent: ten callers with
        // ten credentials treated as one.
        let a = resolve_principal(&headers(&[("authorization", "Bearer tok-a")]), loopback());
        let b = resolve_principal(&headers(&[("authorization", "Bearer tok-b")]), loopback());
        let a2 = resolve_principal(&headers(&[("authorization", "Bearer tok-a")]), remote());
        assert_ne!(a.key, b.key, "different credentials are different callers");
        assert_eq!(a.key, a2.key, "the same credential is the same caller");
    }

    #[test]
    fn bearer_scheme_is_case_insensitive_and_an_empty_one_is_not_identity() {
        let lower = resolve_principal(&headers(&[("authorization", "bearer tok")]), loopback());
        let upper = resolve_principal(&headers(&[("authorization", "BEARER  tok ")]), loopback());
        assert_eq!(lower.key, upper.key);
        // An empty or non-bearer credential presents nothing.
        for bad in ["Bearer ", "Basic tok"] {
            let r = resolve_principal(&headers(&[("authorization", bad)]), loopback());
            assert_eq!(r.key, PrincipalKey::Anonymous, "{bad} is not an identity");
        }
    }

    #[test]
    fn x_principal_identifies_a_loopback_caller() {
        let r = resolve_principal(&headers(&[("x-principal", "desktop")]), loopback());
        assert_eq!(r.source, PrincipalSource::Declared);
        assert_eq!(r.key, PrincipalKey::Declared("desktop".into()));
        let other = resolve_principal(&headers(&[("x-principal", "cli")]), loopback());
        assert_ne!(r.key, other.key);
    }

    #[test]
    fn x_principal_is_ignored_from_a_remote_caller() {
        // A remote caller may not mint principals: it would escape rationing
        // by rotating one header. It stays in its credential bucket.
        let declared_only = resolve_principal(&headers(&[("x-principal", "whoever")]), remote());
        assert_eq!(declared_only.key, PrincipalKey::Anonymous);
        assert_eq!(declared_only.source, PrincipalSource::Absent);

        let with_cred = resolve_principal(
            &headers(&[("x-principal", "whoever"), ("authorization", "Bearer t")]),
            remote(),
        );
        let cred_alone = resolve_principal(&headers(&[("authorization", "Bearer t")]), remote());
        assert_eq!(
            with_cred.key, cred_alone.key,
            "X-Principal must not move a remote caller out of its credential bucket"
        );
    }

    #[test]
    fn a_presented_credential_outranks_a_declared_name() {
        // Resolution order, as an assertion. The worker-token case rides
        // this branch too — a WorkerToken is a plain bearer.
        let r = resolve_principal(
            &headers(&[
                ("authorization", "Bearer worker-token-abc"),
                ("x-principal", "pretend-to-be-someone-else"),
            ]),
            loopback(),
        );
        assert_eq!(r.source, PrincipalSource::Credential);
        assert!(r.key.label().starts_with("cred:"));
    }

    #[test]
    fn a_blank_or_oversized_declared_name_is_handled_not_trusted() {
        // Blank falls through to Anonymous rather than minting an empty key.
        for blank in ["", "   "] {
            let r = resolve_principal(&headers(&[("x-principal", blank)]), loopback());
            assert_eq!(r.key, PrincipalKey::Anonymous);
        }
        // Oversized is fingerprinted, so two callers sharing a long prefix
        // stay distinct instead of being merged by truncation.
        let long_a = "x".repeat(MAX_DECLARED_PRINCIPAL_LEN) + "aaa";
        let long_b = "x".repeat(MAX_DECLARED_PRINCIPAL_LEN) + "bbb";
        let a = resolve_principal(&headers(&[("x-principal", &long_a)]), loopback());
        let b = resolve_principal(&headers(&[("x-principal", &long_b)]), loopback());
        assert_ne!(a.key, b.key, "a shared prefix must not merge two callers");
        assert!(a.key.label().len() < long_a.len(), "the key stays bounded");
    }

    #[test]
    fn a_principal_key_is_never_derived_from_the_connection_address() {
        // ARCH_PRINCIPLES §7.5 — identity from essence, never an address.
        // Two connections from different ephemeral ports presenting the same
        // credential MUST be one principal; if they were not, a greedy client
        // would mint a fresh identity per TCP connection and the cap would be
        // free to bypass.
        let h = headers(&[("authorization", "Bearer same-token")]);
        let a = resolve_principal(&h, Some("127.0.0.1:40001".parse().unwrap()));
        let b = resolve_principal(&h, Some("127.0.0.1:59999".parse().unwrap()));
        assert_eq!(a.key, b.key);
        // And two anonymous callers on different ports are likewise one.
        let empty = headers(&[]);
        assert_eq!(
            resolve_principal(&empty, Some("127.0.0.1:40001".parse().unwrap())).key,
            resolve_principal(&empty, Some("127.0.0.1:59999".parse().unwrap())).key
        );
    }
}
