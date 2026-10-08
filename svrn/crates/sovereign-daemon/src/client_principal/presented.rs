// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a request presented in `Authorization`, decided once: the classifier
//! [`client_auth`](crate::client_auth) admits by and the resolver keys by.
//!
//! A presented credential decides, from any address (ADDRESSED_TEXT §5.5 rule
//! 1). Before this, a loopback caller was admitted before its bearer was read,
//! so a bogus or revoked bearer from 127.0.0.1 passed as the owner.
//!
//! Every bearer this daemon mints starts with [`CREDENTIAL_PREFIX`]. That is
//! what lets the rule keep a published promise: `docs/INTEROP.md` §1 tells any
//! OpenAI client to send a non-empty key, and every OpenAI SDK sends one, so a
//! loopback process sending `Bearer local` is not presenting a credential of
//! ours. Three outcomes, then:
//!
//! - a bearer that verifies is [`Presentation::Verified`], from any address;
//! - a bearer in our form that verifies nothing is
//!   [`Presentation::Unverified`]: a 401 from any address, loopback included;
//! - a bearer NOT in our form that verifies nothing is unverified too, except
//!   from a local process on a listener where loopback is the owner. There it
//!   is [`Presentation::Absent`], ignored and logged at debug with why.
//!
//! An `Authorization` header with no readable bearer in it (blank, `Bearer `
//! with nothing after, another scheme, bytes that are not visible ASCII) is a
//! bearer not in our form. A harness whose `${TOKEN}` expanded to nothing
//! sends one; from a local process under `loopback = owner` it is ignored and
//! named, and from anywhere else it is refused.

use axum::http::HeaderMap;
use sovereign_grants::GuestGrant;
use subtle::ConstantTimeEq;

/// The prefix every credential this daemon mints carries: named credentials,
/// guest grants, a freshly created daemon-wide token, the process's own key.
/// [`crate::client_auth::generate_bearer_token`] is the one place it is
/// written. A bearer that starts with it and verifies nothing is a credential
/// of ours that is wrong, revoked or lapsed, never a placeholder.
pub const CREDENTIAL_PREFIX: &str = "svrn_";

/// A presented credential that verified, and which one.
#[derive(Debug, Clone)]
pub enum Verified {
    /// A live guest grant. The grant bounds the routes.
    Guest(GuestGrant),
    /// A named credential (`svrn daemon key`): the name it was minted under,
    /// which is the subject it asserts, and its groups.
    Named {
        /// The asserted subject: what a call is logged by, and revoked by.
        name: String,
        /// The asserted groups.
        groups: Vec<String>,
    },
    /// The daemon-wide client token, on a posture that admits it.
    Shared,
    /// The daemon-wide client token, under `[daemon] client_tokens =
    /// "named-only"`: it verifies, and is refused with a sentence.
    SharedRefused,
}

/// What a request presented, as far as admitting it goes.
#[derive(Debug, Clone)]
pub enum Presentation {
    /// No credential to read. `ignored` names a header that was there and is
    /// not a credential of ours, read as absent from a local process on a
    /// listener where loopback is the owner; `None` when no header came.
    Absent {
        /// Why a present header was not read as a credential.
        ignored: Option<&'static str>,
    },
    /// A live credential this daemon holds.
    Verified(Verified),
    /// A presentation that verifies nothing and is not ignored: refused (401)
    /// wherever it came from.
    Unverified {
        /// Why, for the log. Never echoes the bearer.
        reason: &'static str,
    },
}

/// What the `Authorization` header carries, before anything is looked up.
enum Header<'a> {
    None,
    /// A header with no readable bearer in it.
    Unreadable,
    Bearer(&'a str),
}

fn header(headers: &HeaderMap) -> Header<'_> {
    let Some(raw) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Header::None;
    };
    let Ok(value) = raw.to_str() else {
        return Header::Unreadable;
    };
    let rest = value.strip_prefix("Bearer ").or_else(|| {
        let (scheme, rest) = value.split_once(' ')?;
        scheme.eq_ignore_ascii_case("bearer").then_some(rest)
    });
    match rest.map(str::trim) {
        Some(token) if !token.is_empty() => Header::Bearer(token),
        _ => Header::Unreadable,
    }
}

/// The bearer in `Authorization`, when the header carries a readable one
/// (`Bearer <token>`, scheme case-insensitive, token trimmed). THE one parse
/// of the header: the resolver, `client_auth` and the guest door read this.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    match header(headers) {
        Header::Bearer(token) => Some(token),
        Header::None | Header::Unreadable => None,
    }
}

/// Whether `token` is in the form this daemon mints.
pub fn in_our_form(token: &str) -> bool {
    token.starts_with(CREDENTIAL_PREFIX)
}

impl crate::state::AppState {
    /// The credential `token` is, if this daemon holds it live. Every store
    /// that admits a bearer is read here and nowhere else: the guest grants,
    /// the named credentials and the daemon-wide token, each compared in
    /// constant time by its own store.
    pub fn verify_bearer(&self, token: &str) -> Option<Verified> {
        let node = &self.inner.node;
        let now = sovereign_time::unix_millis();
        if let Some(grant) = node.guest_grants.live(token, now) {
            return Some(Verified::Guest(grant));
        }
        if let Some((name, groups)) = node.named_client_tokens.asserted_for(token) {
            return Some(Verified::Named { name, groups });
        }
        let shared = self.client_token()?;
        bool::from(token.as_bytes().ct_eq(shared.as_bytes())).then(|| {
            if node.client_tokens.admits_shared_token() {
                Verified::Shared
            } else {
                Verified::SharedRefused
            }
        })
    }

    /// THE classification of what a request presented. `local_owner` is
    /// whether this request is a local process on a listener and posture
    /// where loopback is the owner: the one case a bearer not in our form is
    /// read as absent.
    pub fn presentation(&self, headers: &HeaderMap, local_owner: bool) -> Presentation {
        let token = match header(headers) {
            Header::None => return Presentation::Absent { ignored: None },
            Header::Unreadable if local_owner => {
                return Presentation::Absent {
                    ignored: Some("an Authorization header with no bearer in it"),
                }
            }
            Header::Unreadable => {
                return Presentation::Unverified {
                    reason: "an Authorization header with no bearer in it",
                }
            }
            Header::Bearer(token) => token,
        };
        if let Some(verified) = self.verify_bearer(token) {
            return Presentation::Verified(verified);
        }
        if in_our_form(token) {
            return Presentation::Unverified {
                reason: "a credential of this daemon's form that it does not hold \
                         (wrong, revoked or lapsed)",
            };
        }
        if local_owner {
            return Presentation::Absent {
                ignored: Some("a bearer not in this daemon's form, from a local process"),
            };
        }
        Presentation::Unverified {
            reason: "a bearer this daemon does not hold",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;

    fn with_auth(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(axum::http::header::AUTHORIZATION, value.parse().unwrap());
        h
    }

    fn state() -> AppState {
        AppState::new(kernel_types::NodeId::from_u128(1))
    }

    /// The three verdicts, and the one place the posture changes them.
    /// Failing input: drop the `in_our_form` branch and a revoked `svrn_`
    /// bearer from a local process reads as absent.
    #[test]
    fn our_form_never_reads_as_absent_and_a_foreign_one_does_only_for_a_local_owner() {
        let s = state();
        let ours = format!("{CREDENTIAL_PREFIX}{}", "0".repeat(64));
        for local_owner in [true, false] {
            assert!(
                matches!(
                    s.presentation(&with_auth(&format!("Bearer {ours}")), local_owner),
                    Presentation::Unverified { .. }
                ),
                "local_owner={local_owner}"
            );
        }
        for foreign in ["Bearer local", "Bearer ", "Basic dXNlcg==", ""] {
            assert!(matches!(
                s.presentation(&with_auth(foreign), true),
                Presentation::Absent { ignored: Some(_) }
            ));
            assert!(matches!(
                s.presentation(&with_auth(foreign), false),
                Presentation::Unverified { .. }
            ));
        }
        assert!(matches!(
            s.presentation(&HeaderMap::new(), false),
            Presentation::Absent { ignored: None }
        ));
    }

    /// A live grant verifies from any address, whatever its form.
    #[test]
    fn a_live_grant_verifies_whatever_the_listener() {
        let s = state();
        let now = sovereign_time::unix_millis();
        s.inner
            .node
            .guest_grants
            .issue("legacy-unprefixed-grant", Vec::new(), None, 3_600, now);
        for local_owner in [true, false] {
            assert!(matches!(
                s.presentation(&with_auth("Bearer legacy-unprefixed-grant"), local_owner),
                Presentation::Verified(Verified::Guest(_))
            ));
        }
    }
}
