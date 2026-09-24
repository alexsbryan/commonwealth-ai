// SPDX-License-Identifier: AGPL-3.0-or-later
//! The [`View`] commitment — a bounded proof of *which ops a node holds*.
//!
//! One primitive, three mountings (ARCH §10.6): the sync digest's per-actor
//! entry (so two replicas that diverged at one seq can SEE it and exchange),
//! an act's `view` field (so an equivocation or a cut is weighable from the
//! act itself — `RING_APP_LIBRARY.md` §19 step 0), and the checkpoint
//! document's completeness claim (it carries the digest). The definition
//! below is the wire contract: the golden vectors in the tests are frozen
//! from this implementation and a reimplementation must reproduce them
//! byte-for-byte.
//!
//! The chain is over DERIVED ids (`admit::derived_id` — identity from
//! essence), ids at one seq sorted and deduped, so a tampered `id` field and
//! a second line for the same content fold identically. Known limit,
//! inherited not introduced: `OpId` is 64-bit-truncated BLAKE3 (oplog's
//! `ContentHash::short`), so the chain is as strong as the ids — the same
//! limit every correction target already rides.

use std::collections::{BTreeMap, BTreeSet};

use kernel_types::ContentHash;
use oplog_types::Op;
use serde::{Deserialize, Serialize};

use crate::SignedOp;

/// Domain separation and definition version for the fold. A second
/// definition gets a second label; old heads stay meaningful.
const VIEW_DOMAIN: &[u8] = b"cwth/view/1";

/// One actor's commitment: the contiguous run `[from, mark]` being claimed
/// (`from` is the sealed floor, `mark` the contiguous high-water — the same
/// run the mark alone describes) and `head`, the fold of everything held in
/// it. Equal heads over an equal `(from, mark)` mean equal held sets; the
/// preimage resistance of the hash is the whole argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub from: u64,
    pub mark: u64,
    pub head: String,
}

impl View {
    /// Fold the ops held for `actor` in `[from, mark]`.
    ///
    /// A caller evaluating SOMEONE ELSE's claim may hold more or less in the
    /// window (fork branches, holes) — the fold is a pure function of the set
    /// presented, so equal sets fold equal regardless of who computes it.
    pub fn of<'a>(
        actor: &str,
        from: u64,
        mark: u64,
        ops: impl IntoIterator<Item = &'a Op<SignedOp>>,
    ) -> Self {
        let mut by_seq: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
        for op in ops {
            if op.actor == actor && op.kind.seq >= from && op.kind.seq <= mark {
                by_seq
                    .entry(op.kind.seq)
                    .or_default()
                    .insert(crate::admit::derived_id(op).to_string());
            }
        }
        let mut h = ContentHash::of(&[VIEW_DOMAIN, b"\0", actor.as_bytes()].concat());
        for (seq, ids) in &by_seq {
            for id in ids {
                let mut step = Vec::with_capacity(32 + 8 + 2 + id.len());
                step.extend_from_slice(h.as_bytes());
                step.push(0);
                step.extend_from_slice(&seq.to_be_bytes());
                step.push(0);
                step.extend_from_slice(id.as_bytes());
                h = ContentHash::of(&step);
            }
        }
        View {
            from,
            mark,
            head: h.to_hex(),
        }
    }
}

/// The sync digest: `{actor → View}`, versioned on the wire.
///
/// The version is not decoration (ARCH §18.3): a marks-only digest cannot
/// prove content, so v1 is REFUSED BY NAME rather than read as if it carried
/// a commitment, and an unknown `v` is refused naming the number. Silence and
/// defaults are how a commitment quietly becomes a suggestion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Digest(BTreeMap<String, View>);

/// The one digest envelope this build speaks.
pub const DIGEST_V: u32 = 2;

impl Digest {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn insert(&mut self, actor: String, view: View) {
        self.0.insert(actor, view);
    }

    pub fn get(&self, actor: &str) -> Option<&View> {
        self.0.get(actor)
    }

    pub fn contains_key(&self, actor: &str) -> bool {
        self.0.contains_key(actor)
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.keys()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &View)> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn entries(&self) -> &BTreeMap<String, View> {
        &self.0
    }
}

impl From<BTreeMap<String, View>> for Digest {
    fn from(entries: BTreeMap<String, View>) -> Self {
        Self(entries)
    }
}

impl Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("v", &DIGEST_V)?;
        map.serialize_entry("entries", &self.0)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let raw = serde_json::Value::deserialize(deserializer)?;
        match raw.get("v") {
            Some(v) if v.as_u64() == Some(u64::from(DIGEST_V)) => {
                let entries = raw
                    .get("entries")
                    .cloned()
                    .ok_or_else(|| D::Error::custom("digest v2 carries no `entries`"))?;
                let entries: BTreeMap<String, View> =
                    serde_json::from_value(entries).map_err(D::Error::custom)?;
                Ok(Digest(entries))
            }
            Some(v) => Err(D::Error::custom(format!(
                "digest v{} is not spoken here (this node speaks v{DIGEST_V})",
                v.as_u64()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| v.to_string())
            ))),
            None if raw.as_object().is_some_and(|o| {
                !o.is_empty() && o.values().all(serde_json::Value::is_u64)
            }) =>
            {
                Err(D::Error::custom(
                    "digest v1 (marks-only) is refused: it cannot prove content — both ends must speak v2",
                ))
            }
            None => Err(D::Error::custom("unrecognised digest envelope")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_support::{key, record, signed};

    fn ops_of(seed: u8, seqs: std::ops::Range<u64>) -> Vec<Op<SignedOp>> {
        seqs.map(|s| signed(&key(seed), 100 + s as i64, s, record("x")))
            .collect()
    }

    /// **Golden vectors — frozen 2026-09-23 from this implementation.**
    /// RFC style: generated once, then pinned. Their job is cross-
    /// implementation conformance and refactor-proofing; a reimplementation
    /// of the fold must reproduce these byte-for-byte, which is also what
    /// makes the A3 signing-bytes vectors meaningful.
    #[test]
    fn view_golden_vectors() {
        let actor = crate::actor_of(&key(1));
        // (from, mark, ops, expected head)
        let cases: &[(u64, u64, Vec<Op<SignedOp>>, &str)] = &[
            // An empty window folds to the bare domain hash.
            (
                0,
                0,
                vec![],
                "6b0ee48a13235b72410a661e043021f58e3c27dcd38c1bd39b80e90c3247e056",
            ),
            // One op at seq 0.
            (
                0,
                0,
                ops_of(1, 0..1),
                "21f6b6a08c22f52bee9ea72df92f495d94d7a9ea5574a4ae98dedf365e104a25",
            ),
            // The same op at a different mark is a different window.
            (
                0,
                1,
                ops_of(1, 0..2),
                "e73fb18ff7733e615a64e3a8b1140c664c6b60c7d6af61f4f1e0bf6be542f70e",
            ),
            // The same window from a different floor is a different claim.
            (
                1,
                1,
                ops_of(1, 0..2),
                "aa860862260bddba139e925f2ea620bf2ba3d4c64f4e32ed480ed808cc51689e",
            ),
        ];
        for (from, mark, ops, expected) in cases {
            let view = View::of(&actor, *from, *mark, ops.iter());
            assert_eq!(
                view.head, *expected,
                "golden vector (from={from}, mark={mark}) — update ONLY with a domain bump"
            );
        }

        // The chain reacts to content, not to arrival: two different ops at
        // one seq fold differently, and the same set folds identically
        // however it is presented.
        let left = signed(&key(1), 200, 2, record("left"));
        let right = signed(&key(1), 200, 2, record("right"));
        let prefix = ops_of(1, 0..2);
        let mut a = prefix.clone();
        a.push(left.clone());
        let mut b = prefix.clone();
        b.push(right.clone());
        let mut both = a.clone();
        both.push(right.clone());
        let mut both_reordered = vec![right, left];
        both_reordered.extend(prefix.iter().cloned());
        assert_ne!(
            View::of(&actor, 0, 2, a.iter()).head,
            View::of(&actor, 0, 2, b.iter()).head,
            "a fork at one seq must not fold equal"
        );
        assert_eq!(
            View::of(&actor, 0, 2, both.iter()).head,
            View::of(&actor, 0, 2, both_reordered.iter()).head,
            "the fold is over the set, not the arrival order"
        );
    }

    /// The envelope's refusals are named. Silence here is how a commitment
    /// becomes a suggestion (ARCH §18.3).
    #[test]
    fn an_unknown_digest_version_is_a_named_refusal() {
        let v1 = serde_json::json!({"8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c": 5});
        let err = serde_json::from_value::<Digest>(v1).unwrap_err();
        assert!(err.to_string().contains("v1"), "{err}");
        assert!(err.to_string().contains("marks-only"), "{err}");

        let v3 = serde_json::json!({"v": 3, "entries": {}});
        let err = serde_json::from_value::<Digest>(v3).unwrap_err();
        assert!(err.to_string().contains("v3"), "{err}");

        let garbage = serde_json::json!({"who": "knows"});
        let err = serde_json::from_value::<Digest>(garbage).unwrap_err();
        assert!(err.to_string().contains("unrecognised"), "{err}");

        // And v2 round-trips.
        let mut d = Digest::new();
        d.insert(
            "a".into(),
            View {
                from: 0,
                mark: 0,
                head: "h".into(),
            },
        );
        let wire = serde_json::to_value(&d).unwrap();
        assert_eq!(wire["v"], 2);
        assert_eq!(serde_json::from_value::<Digest>(wire).unwrap(), d);
    }
}
