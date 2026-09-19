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
//! ## Resolution order — the five arms of the published [`Principal`]
//!
//! Settled by operator intake (note `c874c318`): a principal anchors on an
//! **existing** surface, never on a new identity scheme. The arms are read in
//! this order; the order is load-bearing (see [`AppState::resolve`]).
//!
//! 1. **A presented `Authorization: Bearer` credential.** If the store holds a
//!    **live guest grant** for it, the arm is [`Principal::Guest`] — the grant
//!    bounds the caller's routes. Any other bearer is
//!    [`Principal::RemoteClient`], keyed by a fingerprint, never by the secret
//!    itself, so a principal key is safe to log and safe to hold in a map. The
//!    bearer branch also covers the owner-signed `WorkerToken` the order names:
//!    a worker token rides as a plain bearer
//!    (`sovereign-serving-host/src/pinned_transport.rs:127`), so it needs no
//!    branch of its own — one decider, not two.
//! 2. **`X-Node-Id`** → [`Principal::Member`]. Read *before* the loopback
//!    branch: a mesh peer arrives on the trusting listener over loopback
//!    (`client_auth.rs:41-56`), so a loopback address is not evidence of a
//!    local caller for a peer that named itself.
//! 3. **`X-Principal`, from a loopback caller only**, and only on a listener
//!    that trusts a loopback peer address (`ClientAuthPolicy::trust_loopback`).
//!    The local multi-caller case: desktop, CLI and MCP all reach `127.0.0.1`
//!    and are all auth-exempt (`client_auth.rs:143`), so a self-declared name
//!    is the only identity they can offer. Deliberately **not** honoured from a
//!    remote caller: remote callers all authenticate with the one daemon-wide
//!    token, so honouring a self-declared name there would let a remote caller
//!    mint unlimited principals by rotating a header and escape rationing
//!    entirely. Pinning a remote caller to its credential is the stricter
//!    reading and the safe one. It is likewise **not** honoured on the guest
//!    listener, where the loopback address is the iroh acceptor's own forward
//!    hop and says nothing about who dialled.
//! 4. **[`Principal::Anonymous`].** Nothing was presented. One shared
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
//!   `X-Node-Id` already has on the peer gate (`sovereign-serving-host/src/admission.rs`, moved host-side by REVIEW-build-serving-move-admission). It is
//!   sufficient for the cooperative-local case it is built for and it must
//!   never be load-bearing for access control.

use std::hash::{Hash, Hasher};
use std::net::SocketAddr;

use axum::http::HeaderMap;
use sovereign_serving_host::admission::Principal;

use crate::admission::AdmissionHost;
use crate::client_auth::ClientAuthPolicy;
use crate::state::AppState;

/// `X-Principal` values longer than this are fingerprinted rather than kept
/// verbatim. A principal key becomes a map key and a log field, so an
/// unbounded header value would be an unbounded allocation on the hot path.
/// Truncating instead would silently merge two distinct callers sharing a
/// long prefix — a fingerprint keeps them apart.
const MAX_DECLARED_PRINCIPAL_LEN: usize = 128;

/// The header a local caller uses to name itself.
pub const PRINCIPAL_HEADER: &str = "x-principal";

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

impl AppState {
    /// THE resolver: request headers + where the connection came from + the
    /// listener's posture → the principal this request's turns are charged to.
    ///
    /// This is the one place the daemon answers "who is asking"
    /// (`DAEMON_CORE.md` §3.3, "one resolution at the edge, one value"). It
    /// covers all five arms of the published [`Principal`] — see the module
    /// docs for the order and why it is load-bearing — and the three call
    /// sites that need an identity all come through it: `client_auth_layer`'s
    /// credential/grant decision, the host's `AdmissionHost::resolve` port, and
    /// `peer_admission_layer`'s `Member` construction.
    ///
    /// The value it returns is the published [`Principal`] itself, never a
    /// wire-side twin: the type is published language (`DAEMON_CORE.md` §3.3)
    /// and admission keys on it directly, so there is one identity type and no
    /// second scheme to drift (ARCH principle 8).
    ///
    /// `peer` is the real `ConnectInfo<SocketAddr>` address, not a header — the
    /// same source `client_auth` decides loopback from, and for the same reason
    /// (`client_auth.rs:17-22`: the old header-keyed split made "omit the
    /// header" a full-trust bypass). `None` means the listener did not wire
    /// `ConnectInfo`, which `client_auth` already fails closed on; here it is
    /// treated as *not* loopback, the stricter reading.
    ///
    /// `policy` is the listener's posture. It gates the one arm a loopback
    /// address would otherwise decide — `X-Principal` — so a tunnelled caller
    /// on the guest listener cannot name itself the local owner
    /// (`client_auth.rs` "Loopback is a property of the LISTENER").
    ///
    /// Identity comes from what the caller *presented*, never from where it
    /// connected from: ARCH_PRINCIPLES §7.5 forbids deriving a key from an
    /// address or a counter, which rules out the per-connection `SocketAddr`
    /// fallback that would otherwise be the obvious branch. An unidentified
    /// caller is [`Principal::Anonymous`] — one honest bucket — rather than a
    /// fleet of buckets minted from ephemeral port numbers, which would hand
    /// every caller a fresh identity per TCP connection and defeat the cap
    /// outright.
    pub fn resolve(
        &self,
        headers: &HeaderMap,
        peer: Option<SocketAddr>,
        policy: ClientAuthPolicy,
    ) -> Principal {
        // 1. A presented bearer. A live guest grant is its own arm; any other
        //    bearer is a remote client. The grant store decides which, and a
        //    lapsed grant is simply not a grant (`GuestGrantStore::live`).
        if let Some(token) = bearer(headers) {
            let now = commonwealth_core::clock::unix_now_millis();
            if self.inner.node.guest_grants.live(token, now).is_some() {
                return Principal::Guest {
                    grant: fingerprint(token),
                };
            }
            return Principal::RemoteClient {
                credential: fingerprint(token),
            };
        }

        // 2. A mesh peer names its node id. Read BEFORE the loopback branch:
        //    a peer arrives on the trusting listener over loopback
        //    (`client_auth.rs:41-56`). `parse_node_id` is the port over the one
        //    canonical wire parser (`headers::parse_x_node_id`, FE-99); a
        //    present-but-unreadable value does not resolve here, which is what
        //    lets `peer_admission_layer` keep its record-and-zero-bucket path.
        if let Some(node_id) = self.parse_node_id(headers) {
            return Principal::Member { node_id };
        }

        // 3. A loopback caller may name itself, on a listener that trusts a
        //    loopback peer address.
        let from_loopback = peer.is_some_and(|p| p.ip().is_loopback());
        if policy.trust_loopback && from_loopback {
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
                return Principal::LocalOwner {
                    sub_identity: Some(name),
                };
            }
        }

        Principal::Anonymous
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        use commonwealth_core::ids::{MeshId, NodeId};
        use commonwealth_core::mesh::Mesh;
        use std::collections::HashMap;
        let mesh = Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::from_u128(1),
            name: "Principal Test".into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: HashMap::new(),
            peers: vec![],
        };
        AppState::new(NodeId::from_u128(1), mesh)
    }

    /// The one resolver on the daemon's own (trusting) listener.
    fn resolve(headers: &HeaderMap, peer: Option<SocketAddr>) -> Principal {
        state().resolve(headers, peer, ClientAuthPolicy::default())
    }

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
        let a = resolve(&headers(&[]), loopback());
        let b = resolve(&headers(&[]), remote());
        let c = resolve(&headers(&[]), None);
        assert_eq!(a, Principal::Anonymous);
        assert_eq!(a, b, "an unidentified caller is one bucket");
        assert_eq!(a, c, "including when ConnectInfo is missing");
    }

    #[test]
    fn a_bearer_credential_keys_the_principal_and_never_leaks_the_secret() {
        let r = resolve(
            &headers(&[("authorization", "Bearer super-secret-token")]),
            loopback(),
        );
        assert!(matches!(r, Principal::RemoteClient { .. }));
        let label = r.label();
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
        let a = resolve(&headers(&[("authorization", "Bearer tok-a")]), loopback());
        let b = resolve(&headers(&[("authorization", "Bearer tok-b")]), loopback());
        let a2 = resolve(&headers(&[("authorization", "Bearer tok-a")]), remote());
        assert_ne!(a, b, "different credentials are different callers");
        assert_eq!(a, a2, "the same credential is the same caller");
    }

    #[test]
    fn bearer_scheme_is_case_insensitive_and_an_empty_one_is_not_identity() {
        let lower = resolve(&headers(&[("authorization", "bearer tok")]), loopback());
        let upper = resolve(&headers(&[("authorization", "BEARER  tok ")]), loopback());
        assert_eq!(lower, upper);
        // An empty or non-bearer credential presents nothing.
        for bad in ["Bearer ", "Basic tok"] {
            let r = resolve(&headers(&[("authorization", bad)]), loopback());
            assert_eq!(r, Principal::Anonymous, "{bad} is not an identity");
        }
    }

    #[test]
    fn x_principal_identifies_a_loopback_caller() {
        let r = resolve(&headers(&[("x-principal", "desktop")]), loopback());
        assert_eq!(
            r,
            Principal::LocalOwner {
                sub_identity: Some("desktop".into())
            }
        );
        let other = resolve(&headers(&[("x-principal", "cli")]), loopback());
        assert_ne!(r, other);
    }

    #[test]
    fn x_principal_is_ignored_from_a_remote_caller() {
        // A remote caller may not mint principals: it would escape rationing
        // by rotating one header. It stays in its credential bucket.
        let declared_only = resolve(&headers(&[("x-principal", "whoever")]), remote());
        assert_eq!(declared_only, Principal::Anonymous);

        let with_cred = resolve(
            &headers(&[("x-principal", "whoever"), ("authorization", "Bearer t")]),
            remote(),
        );
        let cred_alone = resolve(&headers(&[("authorization", "Bearer t")]), remote());
        assert_eq!(
            with_cred, cred_alone,
            "X-Principal must not move a remote caller out of its credential bucket"
        );
    }

    #[test]
    fn a_presented_credential_outranks_a_declared_name() {
        // Resolution order, as an assertion. The worker-token case rides
        // this branch too — a WorkerToken is a plain bearer.
        let r = resolve(
            &headers(&[
                ("authorization", "Bearer worker-token-abc"),
                ("x-principal", "pretend-to-be-someone-else"),
            ]),
            loopback(),
        );
        assert!(matches!(r, Principal::RemoteClient { .. }));
        assert!(r.label().starts_with("cred:"));
    }

    #[test]
    fn a_blank_or_oversized_declared_name_is_handled_not_trusted() {
        // Blank falls through to Anonymous rather than minting an empty key.
        for blank in ["", "   "] {
            let r = resolve(&headers(&[("x-principal", blank)]), loopback());
            assert_eq!(r, Principal::Anonymous);
        }
        // Oversized is fingerprinted, so two callers sharing a long prefix
        // stay distinct instead of being merged by truncation.
        let long_a = "x".repeat(MAX_DECLARED_PRINCIPAL_LEN) + "aaa";
        let long_b = "x".repeat(MAX_DECLARED_PRINCIPAL_LEN) + "bbb";
        let a = resolve(&headers(&[("x-principal", &long_a)]), loopback());
        let b = resolve(&headers(&[("x-principal", &long_b)]), loopback());
        assert_ne!(a, b, "a shared prefix must not merge two callers");
        assert!(a.label().len() < long_a.len(), "the key stays bounded");
    }

    #[test]
    fn a_principal_key_is_never_derived_from_the_connection_address() {
        // ARCH_PRINCIPLES §7.5 — identity from essence, never an address.
        // Two connections from different ephemeral ports presenting the same
        // credential MUST be one principal; if they were not, a greedy client
        // would mint a fresh identity per TCP connection and the cap would be
        // free to bypass.
        let h = headers(&[("authorization", "Bearer same-token")]);
        let a = resolve(&h, Some("127.0.0.1:40001".parse().unwrap()));
        let b = resolve(&h, Some("127.0.0.1:59999".parse().unwrap()));
        assert_eq!(a, b);
        // And two anonymous callers on different ports are likewise one.
        let empty = headers(&[]);
        assert_eq!(
            resolve(&empty, Some("127.0.0.1:40001".parse().unwrap())),
            resolve(&empty, Some("127.0.0.1:59999".parse().unwrap()))
        );
    }

    #[test]
    fn a_live_guest_grant_is_its_own_arm_and_keys_on_a_fingerprint() {
        // The fifth arm: a bearer that is a live guest grant is a Guest, not a
        // remote client. The grant token itself must never appear in the key.
        let s = state();
        let now = commonwealth_core::clock::unix_now_millis();
        s.inner
            .node
            .guest_grants
            .issue("guest-secret-token", Vec::new(), None, 3_600, now);
        let r = s.resolve(
            &headers(&[("authorization", "Bearer guest-secret-token")]),
            loopback(),
            ClientAuthPolicy::default(),
        );
        assert!(matches!(r, Principal::Guest { .. }), "got {r:?}");
        assert!(
            !r.label().contains("guest-secret-token"),
            "the grant must never appear in a loggable key: {}",
            r.label()
        );
        assert!(r.label().starts_with("guest:"));

        // A bearer that is NOT a grant stays a remote client.
        let other = s.resolve(
            &headers(&[("authorization", "Bearer not-a-grant")]),
            loopback(),
            ClientAuthPolicy::default(),
        );
        assert!(
            matches!(other, Principal::RemoteClient { .. }),
            "got {other:?}"
        );
        assert_ne!(r, other, "a grant is a different key from a plain bearer");
    }

    #[test]
    fn an_expired_grant_is_not_a_guest() {
        // `live` evaluates expiry lazily; the resolver inherits that, so a
        // lapsed grant falls back to the credential bucket rather than
        // admitting a stale identity.
        let s = state();
        let now = commonwealth_core::clock::unix_now_millis();
        s.inner.node.guest_grants.issue(
            "stale-token",
            Vec::new(),
            None,
            1,
            now.saturating_sub(10_000),
        );
        let r = s.resolve(
            &headers(&[("authorization", "Bearer stale-token")]),
            loopback(),
            ClientAuthPolicy::default(),
        );
        assert!(matches!(r, Principal::RemoteClient { .. }), "got {r:?}");
    }

    #[test]
    fn an_x_node_id_resolves_to_member_before_the_loopback_branch() {
        // A peer arrives on the trusting listener over loopback, so the node
        // id must be read before the loopback branch, and a peer that also
        // carries an X-Principal must not be read as the local owner.
        let id = commonwealth_core::ids::NodeId::from_u128(0xBEEF);
        let hex: String = id.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let r = resolve(
            &headers(&[("x-node-id", &hex), ("x-principal", "pretend-local")]),
            loopback(),
        );
        assert_eq!(r, Principal::Member { node_id: id });
    }

    #[test]
    fn a_malformed_x_node_id_is_not_a_member() {
        // Present but unreadable: it must not resolve to a Member, so
        // `peer_admission_layer` keeps its record-and-zero-bucket path.
        let r = resolve(&headers(&[("x-node-id", "not-a-node-id")]), loopback());
        assert_eq!(r, Principal::Anonymous);
    }

    #[test]
    fn an_untrusting_listener_ignores_a_self_declared_name() {
        // The guest listener's loopback address is the iroh acceptor's own
        // forward hop, so X-Principal is not evidence of a local caller there.
        let s = state();
        let r = s.resolve(
            &headers(&[("x-principal", "desktop")]),
            loopback(),
            ClientAuthPolicy::UNTRUSTED_LOOPBACK,
        );
        assert_eq!(r, Principal::Anonymous);

        // On a trusting listener the same request is the local owner.
        let trusting = s.resolve(
            &headers(&[("x-principal", "desktop")]),
            loopback(),
            ClientAuthPolicy::default(),
        );
        assert_eq!(
            trusting,
            Principal::LocalOwner {
                sub_identity: Some("desktop".into())
            }
        );
    }
}
