// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fixtures shared by the rail's test modules.
//!
//! One ring, one set of builders. Two copies of "what does a signed op look
//! like" would be two answers to the question the signature exists to settle
//! (ARCH §10.6), and the sync tests and the admission tests must be talking
//! about the same op or neither proves anything.
//!
//! The payloads here are deliberately NOT expenses. The rail does not know
//! what an expense is, and a fixture that quietly assumed one would let a
//! money rule creep back into this layer without anyone noticing.

use std::collections::BTreeMap;

use crate::*;

pub const NS: &str = "house-things";

pub fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

pub fn p(name: &str) -> Person {
    Person::from(name)
}

/// A ring of three, each signing with one key.
pub fn ring() -> Roster {
    let mut m = BTreeMap::new();
    m.insert(p("alex"), vec![actor_of(&key(1))]);
    m.insert(p("bo"), vec![actor_of(&key(2))]);
    m.insert(p("cy"), vec![actor_of(&key(3))]);
    Roster::new(m)
}

/// An arbitrary well-formed act. `what` is what distinguishes two of them.
pub fn payload(what: &str) -> Payload {
    Payload::new(serde_json::json!({ "kind": "thing", "what": what })).unwrap()
}

pub fn record(what: &str) -> RailAct {
    RailAct::Record {
        payload: payload(what),
    }
}

/// Build the op a node would have written, without going through the journal.
pub fn signed(k: &SigningKey, ts: i64, seq: u64, act: RailAct) -> Op<SignedOp> {
    signed_in(NS, k, ts, seq, act)
}

pub fn signed_in(ns: &str, k: &SigningKey, ts: i64, seq: u64, act: RailAct) -> Op<SignedOp> {
    signed_for(ns, k, ts, seq, act, None)
}

/// [`signed_in`], stating whose words the act was. `body_json` and not a
/// local `to_string` because the signed bytes have one definition
/// (ARCH §10.6) — a fixture that spelled them itself would keep verifying
/// after the real rule changed.
pub fn signed_for(
    ns: &str,
    k: &SigningKey,
    ts: i64,
    seq: u64,
    act: RailAct,
    on_behalf_of: Option<&str>,
) -> Op<SignedOp> {
    let body = body_json(&act, on_behalf_of);
    let signature = sign_ring_op(k, ns, ts, seq, &body);
    Op::new(
        SignedOp {
            seq,
            sig: signature,
            act,
            on_behalf_of: on_behalf_of.map(str::to_string),
        },
        ts,
        actor_of(k),
    )
}

pub fn admitted(ops: &[Op<SignedOp>]) -> Admission {
    admit(ops, &[], &ring(), NS, &Ed25519Verifier)
}

/// The `what` of every act an app's reducer would see, in order — the shape
/// most of these tests assert on.
pub fn applied(a: &Admission) -> Vec<String> {
    a.applied()
        .filter_map(|o| {
            o.payload
                .as_ref()?
                .as_value()
                .get("what")?
                .as_str()
                .map(str::to_string)
        })
        .collect()
}
