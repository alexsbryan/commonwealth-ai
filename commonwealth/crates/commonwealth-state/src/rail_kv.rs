// SPDX-License-Identifier: AGPL-3.0-or-later
//! A store write as a ring-rail act — ONE vocabulary, ONE fold.
//!
//! # What this is
//!
//! [`MeshStore`](crate::MeshStore) used to be replicated by shipping its rows
//! at peers on a timer. cw-lift 4 makes the rail's journal the truth and the
//! store a LOCAL PROJECTION of it: a write goes out as one signed
//! `RailAct::Record`, and what a node holds is the fold of every op it has
//! admitted. Readers of `MeshStore` are untouched — `get`, `scan`,
//! `list_keys` answer exactly as before.
//!
//! This module is the vocabulary and the fold, and nothing else. It has no
//! I/O, no clock and no journal: the pump that appends and the exchange that
//! ingests live in `sovereign-mesh`, which is also where a `RingJournal`
//! comes from. That split is why the dependency here is
//! `commonwealth-rail-core` (the fold) and not `commonwealth-rail` (the
//! journal on disk).
//!
//! # The wire form
//!
//! ```text
//! {"k": <key>, "v": <base64 of the value bytes>, "t": <unix secs>, "d": <bool>}
//! ```
//!
//! `d: true` is a tombstone and then `v` is ABSENT; a payload carrying both
//! is contradictory and is counted `unreadable` rather than guessed at.
//!
//! **`t` is the ORIGINAL write time, not the journal line's timestamp.** The
//! rail orders ops by `(ts_unix, actor, id)` — when a line was written down —
//! and that is the wrong order for last-write-wins the moment a node SNAPSHOTS
//! (re-appends its live rows after a seal, so the floor can move without the
//! live set evaporating). A snapshot line is new; the write it carries is
//! not. Folding on `t` keeps a re-appended row in the position its author
//! gave it, and `a_snapshot_re_append_keeps_its_lww_position` is the pin.
//!
//! **Base64, not raw text.** A `StoreEntry.value` is `Bytes` and several
//! namespaces put non-UTF-8 in it; JSON has no byte string. Base64 costs 4/3
//! against hex's 2/1 — the rail caps a payload at 64 KiB, so the choice is
//! 48 KiB of value versus 32 KiB.
//!
//! **No `kind` field.** `mesh-measurements` — the other namespace on this
//! rail — spells its act `{"kind": "mesh-measurement", "wire": …}`, and the
//! FIELD SET is already the discriminator: a KV op has `k`/`t`/`d`, a
//! measurement has neither. A `kind` here would be a second answer to a
//! question the shape already answers, and the fold would still have to parse
//! to know (ARCH §10.6). What a foreign act costs is one `unreadable`, which
//! is reported.
//!
//! # No float, ever
//!
//! [`Payload`] refuses fractional numbers because two nodes must derive
//! identical bytes from identical facts. `t` is unix SECONDS as an integer
//! for that reason; there is no sub-second store timestamp to lose, because
//! [`StoreEntry::timestamp`](crate::StoreEntry) has always been `u64` seconds.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use bytes::Bytes;
use commonwealth_rail_core::{Admission, Payload};
use serde_json::{json, Value};

use crate::error::{Error, Result};

/// The payload field carrying the key.
const F_KEY: &str = "k";
/// The payload field carrying the base64 value. Absent on a tombstone.
const F_VALUE: &str = "v";
/// The payload field carrying the ORIGINAL write time, unix seconds.
const F_TIME: &str = "t";
/// The payload field saying this act is a delete.
const F_DELETED: &str = "d";

/// One store write, read back off a journal line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvOp {
    pub key: String,
    /// `None` is a tombstone.
    pub value: Option<Bytes>,
    /// Unix seconds, the ORIGINAL write time — see the module docs.
    pub t: u64,
}

/// One key's winning act in a namespace, and who signed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projected {
    pub key: String,
    /// `None` is a tombstone — the key was deleted at `t`.
    pub value: Option<Bytes>,
    pub t: u64,
    /// The signing public key. The only field on the line a writer cannot
    /// forge for someone else (ARCH §18.1), which is why attribution is
    /// resolved from it and never from the payload.
    pub actor: String,
}

/// What one fold of a namespace found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Projection {
    /// One row per key, ordered by key so two nodes render the same list.
    pub rows: Vec<Projected>,
    /// Admitted, non-voided acts this build could not read as a store write —
    /// another app's vocabulary on the same rail, or a peer on a shape this
    /// build does not know. Counted and returned rather than dropped: an
    /// empty `rows` beside a non-zero `unreadable` is a very different fact
    /// from an empty `rows` beside a quiet ring (ARCH §18.3).
    pub unreadable: usize,
}

/// Wrap one store write as a rail payload, or say why it cannot travel.
///
/// The refusal is a sentence because it reaches the pump, which acks the
/// outbox row and warns with this text rather than retrying a row the rail
/// will never accept.
pub fn to_payload(key: &str, value: Option<&[u8]>, t: u64) -> Result<Payload> {
    let mut obj = serde_json::Map::new();
    obj.insert(F_KEY.to_string(), Value::String(key.to_string()));
    obj.insert(F_TIME.to_string(), json!(t));
    obj.insert(F_DELETED.to_string(), Value::Bool(value.is_none()));
    if let Some(bytes) = value {
        obj.insert(F_VALUE.to_string(), Value::String(B64.encode(bytes)));
    }
    Payload::new(Value::Object(obj)).map_err(|e| {
        Error::Serialization(format!(
            "the store write {key:?} cannot go on the rail: {e}"
        ))
    })
}

/// Read a store write back off a journal line, or `None` when this line is
/// not one.
///
/// Never fails loudly, for the reason `mesh-measurements` does not: another
/// app's act on the same rail, or a peer on a shape this build cannot read,
/// must not cost the reader every other key. The COUNT is reported instead —
/// see [`Projection::unreadable`].
pub fn from_payload(payload: &Payload) -> Option<KvOp> {
    let obj = payload.as_value().as_object()?;
    let key = obj.get(F_KEY)?.as_str()?.to_string();
    let t = obj.get(F_TIME)?.as_u64()?;
    let deleted = obj.get(F_DELETED)?.as_bool()?;
    let value = match (deleted, obj.get(F_VALUE)) {
        // A tombstone carries no value. Both halves are stated on the line so
        // neither is inferred from the other's absence, and a line claiming
        // both — or neither — is refused rather than resolved in whichever
        // direction this build happens to prefer (ARCH §18.3).
        (true, None) => None,
        (true, Some(_)) | (false, None) => return None,
        (false, Some(v)) => Some(Bytes::from(B64.decode(v.as_str()?).ok()?)),
    };
    Some(KvOp { key, value, t })
}

/// Fold one namespace's admitted ops into the rows a store should hold.
///
/// Per key, the act with the greatest `(t, actor, id)` among the admitted,
/// non-voided ops of this shape wins. All three terms are needed and all
/// three are the same on every node:
///
/// - `t` is last-write-wins, the rule `MeshStore` has always used.
/// - `actor` then `id` break a tie. Two nodes writing one key in one second
///   used to DIVERGE — `upsert_if_newer` keeps the incumbent, so each node
///   kept whichever value it saw first (pinned as a known limitation by
///   `merge_entry_equal_timestamp_keeps_incumbent`). On the rail there is no
///   arrival order to depend on, and the tie-break is total.
///
/// `admission.applied()` is the ONE definition of "surviving" (voided ops
/// out, seals out); this fold never re-derives it.
pub fn project(admission: &Admission) -> Projection {
    // (t, actor, op id) -> the winning act's value. BTreeMap so `rows` comes
    // out key-ordered on every node without a second sort.
    let mut best: std::collections::BTreeMap<String, (u64, String, String, Option<Bytes>)> =
        std::collections::BTreeMap::new();
    let mut unreadable = 0usize;

    for op in admission.applied() {
        let Some(payload) = op.payload.as_ref() else {
            // `applies()` already guarantees `payload.is_some()`; this arm is
            // the type's, not the rail's.
            continue;
        };
        let Some(kv) = from_payload(payload) else {
            unreadable += 1;
            tracing::debug!(
                actor = %op.actor,
                op_id = %op.id,
                "rail_kv.unreadable_act"
            );
            continue;
        };
        let id = op.id.as_str();
        let wins = match best.get(&kv.key) {
            None => true,
            Some((t, actor, held_id, _)) => {
                (kv.t, op.actor.as_str(), id) > (*t, actor.as_str(), held_id.as_str())
            }
        };
        if wins {
            best.insert(kv.key, (kv.t, op.actor.clone(), id.to_string(), kv.value));
        } else {
            tracing::debug!(
                key = %kv.key,
                actor = %op.actor,
                t = kv.t,
                "rail_kv.superseded"
            );
        }
    }

    let rows: Vec<Projected> = best
        .into_iter()
        .map(|(key, (t, actor, _id, value))| Projected {
            key,
            value,
            t,
            actor,
        })
        .collect();
    tracing::debug!(
        rows = rows.len(),
        unreadable,
        held = admission.held,
        gaps = admission.gaps.len(),
        "rail_kv.projected"
    );
    Projection { rows, unreadable }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_rail_core::tests_support::{admitted, key, signed};
    use commonwealth_rail_core::{OpId, RailAct};

    /// One store write as the act a node would have signed. `ts` is the
    /// JOURNAL line's time and `t` is the WRITE's — they are deliberately
    /// different in these tests, because a fold that confuses them passes
    /// every test where they agree.
    fn write(k: &str, v: Option<&[u8]>, t: u64) -> RailAct {
        RailAct::Record {
            payload: to_payload(k, v, t).unwrap(),
        }
    }

    fn value_of(p: &Projection, key: &str) -> Option<Bytes> {
        p.rows
            .iter()
            .find(|r| r.key == key)
            .and_then(|r| r.value.clone())
    }

    #[test]
    fn a_value_round_trips_through_the_payload() {
        // Non-UTF-8 on purpose: a store value is `Bytes` and JSON has no byte
        // string, which is the whole reason for base64.
        let raw = [0u8, 159, 146, 150, b'a'];
        let p = to_payload("shard:7", Some(&raw), 1_700_000_000).unwrap();
        let back = from_payload(&p).unwrap();
        assert_eq!(back.key, "shard:7");
        assert_eq!(back.value.as_deref(), Some(&raw[..]));
        assert_eq!(back.t, 1_700_000_000);
    }

    #[test]
    fn a_tombstone_carries_no_value() {
        let p = to_payload("gone", None, 42).unwrap();
        assert_eq!(p.as_value().get(F_VALUE), None, "a tombstone has no `v`");
        assert_eq!(p.as_value().get(F_DELETED), Some(&Value::Bool(true)));
        let back = from_payload(&p).unwrap();
        assert_eq!(back.value, None);
        assert_eq!(back.t, 42);
    }

    /// An EMPTY value is not a tombstone. Both spellings exist on the wire
    /// (`"v": ""` with `"d": false`, versus no `v` with `"d": true`) and
    /// collapsing them would make `set(k, b"")` delete the key on every peer.
    #[test]
    fn an_empty_value_is_not_a_tombstone() {
        let p = to_payload("k", Some(b""), 9).unwrap();
        let back = from_payload(&p).unwrap();
        assert_eq!(back.value, Some(Bytes::new()));
        assert_ne!(back.value, None);
    }

    /// The named failing inputs. Each is a line this build must call
    /// unreadable rather than guess at.
    #[test]
    fn a_line_this_build_cannot_read_is_refused_rather_than_guessed() {
        let cases: Vec<(&str, Value)> = vec![
            (
                "another app's act",
                json!({"kind": "mesh-measurement", "wire": "{}"}),
            ),
            ("no key", json!({"t": 1, "d": false, "v": "aGk="})),
            ("no time", json!({"k": "a", "d": false, "v": "aGk="})),
            ("no deleted flag", json!({"k": "a", "t": 1, "v": "aGk="})),
            // Contradictory: says it is a delete AND carries a value.
            (
                "tombstone with a value",
                json!({"k": "a", "t": 1, "d": true, "v": "aGk="}),
            ),
            // Contradictory the other way: not a delete and no value.
            (
                "a live act with no value",
                json!({"k": "a", "t": 1, "d": false}),
            ),
            (
                "not base64",
                json!({"k": "a", "t": 1, "d": false, "v": "!!!!"}),
            ),
            (
                "a negative time",
                json!({"k": "a", "t": -1, "d": false, "v": "aGk="}),
            ),
        ];
        for (why, v) in cases {
            let p = Payload::new(v).unwrap();
            assert!(
                from_payload(&p).is_none(),
                "{why} was read as a store write"
            );
        }
    }

    /// `Payload` refuses fractional numbers, so a `t` that is not a whole
    /// number cannot even be constructed. Stated here because the module's
    /// choice of unix SECONDS rests on it.
    #[test]
    fn a_fractional_time_cannot_be_a_payload_at_all() {
        assert!(Payload::new(json!({"k": "a", "t": 1.5, "d": false, "v": "aGk="})).is_err());
    }

    /// **LWW, and the control.** The rail's total order is
    /// `(ts_unix, actor, id)` — when a line was written DOWN. That is not the
    /// order last-write-wins needs, and the difference only shows when the two
    /// disagree: here the second act is later on the journal and older as a
    /// write.
    #[test]
    fn a_lower_t_arriving_later_does_not_overwrite() {
        let alex = key(1);
        let a = admitted(&[
            signed(&alex, 100, 0, write("k", Some(b"first"), 500)),
            // Later line, OLDER write. Must lose.
            signed(&alex, 200, 1, write("k", Some(b"stale"), 400)),
        ]);
        let p = project(&a);
        assert_eq!(p.rows.len(), 1);
        assert_eq!(value_of(&p, "k").as_deref(), Some(&b"first"[..]));
        assert_eq!(p.rows[0].t, 500);

        // The control: a genuinely newer write DOES take it, so the assertion
        // above is about `t` and not about "the fold never updates".
        let a = admitted(&[
            signed(&alex, 100, 0, write("k", Some(b"first"), 500)),
            signed(&alex, 200, 1, write("k", Some(b"stale"), 400)),
            signed(&alex, 300, 2, write("k", Some(b"newest"), 600)),
        ]);
        assert_eq!(value_of(&project(&a), "k").as_deref(), Some(&b"newest"[..]));
    }

    /// **A snapshot re-append keeps its position.** After a seal the writer
    /// re-appends its live rows so the floor can move without the live set
    /// evaporating. Those lines are NEW — a much later `ts_unix`, a higher
    /// `seq` — and the writes they carry are not. A fold ordering on the line
    /// would hand every key back to whoever snapshotted most recently.
    #[test]
    fn a_snapshot_re_append_keeps_its_lww_position() {
        let (alex, bo) = (key(1), key(2));
        let a = admitted(&[
            signed(&alex, 100, 0, write("k", Some(b"alex-old"), 100)),
            // bo overwrote it.
            signed(&bo, 110, 0, write("k", Some(b"bo-new"), 200)),
            // alex seals and snapshots MUCH later — same write, original `t`.
            signed(&alex, 9_000, 1, write("k", Some(b"alex-old"), 100)),
        ]);
        let p = project(&a);
        assert_eq!(
            value_of(&p, "k").as_deref(),
            Some(&b"bo-new"[..]),
            "a snapshot re-append must not resurrect the write it carries"
        );
        assert_eq!(p.rows[0].t, 200);
    }

    /// A tombstone is an act like any other and loses to a newer write.
    #[test]
    fn a_tombstone_is_ordered_by_t_like_every_other_act() {
        let alex = key(1);
        let a = admitted(&[
            signed(&alex, 100, 0, write("k", Some(b"v"), 100)),
            signed(&alex, 200, 1, write("k", None, 200)),
        ]);
        let p = project(&a);
        assert_eq!(p.rows.len(), 1, "the tombstone is a row, not an absence");
        assert_eq!(p.rows[0].value, None);

        // Written again after the delete, and it comes back.
        let a = admitted(&[
            signed(&alex, 100, 0, write("k", Some(b"v"), 100)),
            signed(&alex, 200, 1, write("k", None, 200)),
            signed(&alex, 300, 2, write("k", Some(b"again"), 300)),
        ]);
        assert_eq!(value_of(&project(&a), "k").as_deref(), Some(&b"again"[..]));
    }

    /// **A foreign act costs one `unreadable`, not a key.** `mesh-measurements`
    /// rides the same rail with its own vocabulary, and a peer on a shape this
    /// build does not know is the same case. Both are COUNTED (ARCH §18.3) —
    /// an empty projection beside a non-zero `unreadable` is a very different
    /// fact from an empty projection beside a quiet ring.
    #[test]
    fn an_act_this_build_cannot_read_is_counted_not_dropped() {
        let alex = key(1);
        let foreign = RailAct::Record {
            payload: Payload::new(json!({"kind": "mesh-measurement", "wire": "{}"})).unwrap(),
        };
        let a = admitted(&[
            signed(&alex, 100, 0, write("k", Some(b"v"), 100)),
            signed(&alex, 200, 1, foreign),
        ]);
        let p = project(&a);
        assert_eq!(p.rows.len(), 1, "the readable key still projects");
        assert_eq!(p.unreadable, 1, "the foreign act is reported");
    }

    /// **A tie is broken the same way on every node.** Two nodes writing one
    /// key in one second used to leave the mesh DIVERGED — `upsert_if_newer`
    /// keeps the incumbent, so each node kept whatever it saw first. On the
    /// rail there is no arrival order: the tie-break is `(actor, id)`, both of
    /// which every node reads off the same signed line.
    #[test]
    fn a_tie_on_t_is_broken_the_same_way_on_every_node() {
        let (alex, bo) = (key(1), key(2));
        let one = signed(&alex, 100, 0, write("k", Some(b"alex"), 777));
        let two = signed(&bo, 100, 0, write("k", Some(b"bo"), 777));

        let forwards = project(&admitted(&[one.clone(), two.clone()]));
        let backwards = project(&admitted(&[two, one]));
        assert_eq!(
            forwards, backwards,
            "the fold must not depend on the order lines arrived in"
        );
        assert_eq!(forwards.rows.len(), 1);
        assert!(forwards.rows[0].value.is_some());
    }

    /// A correction voids what it names, and `applied()` is the ONE definition
    /// of surviving — this fold never re-derives it.
    #[test]
    fn a_voided_act_cannot_win_a_key() {
        let alex = key(1);
        let first = signed(&alex, 100, 0, write("k", Some(b"wrong"), 100));
        let void = signed(
            &alex,
            200,
            1,
            RailAct::Correct {
                corrects: OpId::from_raw(first.id.as_str()),
                replacement: None,
            },
        );
        let p = project(&admitted(&[first, void]));
        assert!(
            p.rows.is_empty(),
            "a voided write is not a tombstone and not a value; it never happened"
        );
        assert_eq!(p.unreadable, 0, "a void carries no payload to fail to read");
    }
}
