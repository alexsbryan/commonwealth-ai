// SPDX-License-Identifier: AGPL-3.0-or-later
//! A store write as a ring-rail act — ONE vocabulary, ONE fold.
//!
//! # What this is
//!
//! [`MeshStore`](crate::MeshStore) used to be replicated by shipping its rows
//! at peers on a timer. cw-lift 4 makes the rail's journal the truth and the
//! store a LOCAL PROJECTION of it: a write goes out as one signed
//! `RailAct::Record`, and what a node holds is the fold of every op it has
//! admitted. Readers of `MeshStore` are untouched — `get` and `scan`
//! answer exactly as before.
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
//! rail orders ops by `(ts_unix, actor, seq, id)` — when a line was written down —
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
//! # The snapshot mark — how a seal says "this is my WHOLE live set"
//!
//! A seal retires everything its author wrote below it, and the snapshot that
//! follows re-appends that author's LIVE rows above the new floor. A tombstone
//! is not a live row, so a delete would stop travelling the moment the seal
//! that retired it lands, and a peer that never received the tombstone would
//! keep the stale value forever. That is the KV shape of K7's "no history past
//! the next seal", and it is closed HERE rather than left to be remembered
//! (ARCH §7.1): **a seal followed by its whole snapshot IS the actor's live
//! set**, so a node that holds all of it may retire every other row of that
//! actor's — see [`Projection::sealed_actors`].
//!
//! "Holds all of it" is the entire difficulty, because the ring's pull is
//! CHUNKED: a seal can arrive in one chunk and its snapshot in the next, and a
//! fold at that instant sees an actor whose live set is empty. So the snapshot
//! ends with a MARK — `{"snap": <the seal's seq>}` — appended after the last
//! row it covers:
//!
//! ```text
//! seq N      Seal
//! seq N+1 …  one Record per live row, each carrying its ORIGINAL `t`
//! seq N+k+1  {"snap": N}
//! ```
//!
//! Holding that mark, with no [`SequenceHole`](commonwealth_rail_core::RailGap::SequenceHole)
//! for its actor, means every seq from the floor to the mark is on this node —
//! which is every row of the snapshot. **Neither half of the cheaper
//! alternatives works.** `Admission::is_complete()` is TRUE on a node holding
//! only the seal, because the hole audit runs `floor..=highest` and the seal is
//! both. "Some op landed above the floor" is true of a half-arrived snapshot,
//! and FALSE of an empty one — and the empty snapshot is exactly the K7 case,
//! an actor whose last live key was deleted just before it sealed.
//!
//! A build that does not know the mark counts it `unreadable` and reconciles
//! nothing, which is the old behaviour: the marker's failure direction is
//! always "do not retire" (ARCH §18.3).
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
use commonwealth_rail_core::{Admission, Payload, RailGap};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, Result};

/// The payload field carrying the key.
const F_KEY: &str = "k";
/// The payload field carrying the base64 value. Absent on a tombstone.
const F_VALUE: &str = "v";
/// The payload field carrying the ORIGINAL write time, unix seconds.
const F_TIME: &str = "t";
/// The payload field saying this act is a delete.
const F_DELETED: &str = "d";
/// The payload field of the SNAPSHOT MARK, carrying the seq of the seal whose
/// snapshot it closes. The one field of that act, and it appears on no other:
/// the field SET is the discriminator here exactly as it is for a store write
/// (see the module docs on `kind`).
const F_SNAPSHOT: &str = "snap";

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
    /// Per actor whose seal AND whole snapshot this node holds, every key that
    /// actor still asserts. See the module docs for what closes a snapshot.
    ///
    /// This is the one claim a fold can make about a set it cannot see: a row
    /// in the store originated by one of these actors and ABSENT from its set
    /// is retired — tombstoned before the seal, or never re-snapshotted, and
    /// either way its author no longer asserts it.
    /// [`MeshStore::apply_projection`](crate::MeshStore::apply_projection) is
    /// what acts on that.
    ///
    /// An actor absent from this map makes NO claim about its whole set, and
    /// an absent entry is never read as an empty one (ARCH §18.3): a seal
    /// whose snapshot is still arriving, an actor that has never sealed, and a
    /// snapshot written by a build that does not mark them all land here, and
    /// all three mean "retire nothing of theirs".
    pub sealed_actors: BTreeMap<String, BTreeSet<String>>,
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

/// The act that closes a snapshot: "everything of mine that is still live is
/// at or below this line."
///
/// Appended by the snapshotting node AFTER the last row it re-appended, so
/// holding it — with no hole in that actor's run above the floor — is holding
/// the whole snapshot. `floor` is the seq of the seal it closes, so a mark left
/// over from an EARLIER seal (held by a node that has not compacted yet) names
/// a floor that no longer matches and closes nothing.
pub fn snapshot_mark(floor: u64) -> Result<Payload> {
    let mut obj = serde_json::Map::new();
    obj.insert(F_SNAPSHOT.to_string(), json!(floor));
    Payload::new(Value::Object(obj))
        .map_err(|e| Error::Serialization(format!("the snapshot mark cannot go on the rail: {e}")))
}

/// Read a snapshot mark back off a journal line, or `None` when this line is
/// not one.
///
/// Disjoint from [`from_payload`] by field set, and pinned that way by
/// `a_mark_is_not_a_store_write_and_a_store_write_is_not_a_mark`.
pub fn read_snapshot_mark(payload: &Payload) -> Option<u64> {
    let obj = payload.as_value().as_object()?;
    if obj.len() != 1 {
        return None;
    }
    obj.get(F_SNAPSHOT)?.as_u64()
}

/// Fold one namespace's admitted ops into the rows a store should hold.
///
/// Per key, the act with the greatest `(t, actor, seq)` among the admitted,
/// non-voided ops of this shape wins. All three terms are needed and all
/// three are the same on every node:
///
/// - `t` is last-write-wins, the rule `MeshStore` has always used.
/// - `actor` then `seq` break a tie. Two nodes writing one key in one second
///   used to DIVERGE — `upsert_if_newer` keeps the incumbent, so each node
///   kept whichever value it saw first (pinned as a known limitation by
///   `merge_entry_equal_timestamp_keeps_incumbent`). On the rail there is no
///   arrival order to depend on, and the tie-break is total.
/// - `seq` is the last term and not the op id, because ONE actor writing a
///   key twice inside a second is the common case (a claim bumped, a model
///   catalogue republished) and `seq` is that actor's program order, while
///   the id is a content hash that put the two in an arbitrary order — pinned
///   by `two_writes_by_one_actor_in_one_second_fold_in_program_order`.
///
/// `admission.applied()` is the ONE definition of "surviving" (voided ops
/// out, seals out); this fold never re-derives it. What it adds on top is the
/// floor: an op strictly below its own actor's
/// [`Admission::floors`](commonwealth_rail_core::Admission) entry is RETIRED
/// and does not fold, because that is exactly the set
/// `RingJournal::compact` deletes. Folding it would make the answer depend on
/// whether this node's prune had run yet, and would resurrect the keys the
/// author's snapshot declined to re-append.
///
/// # The second answer: whose whole live set this is
///
/// Alongside the winning row per key, the fold reports
/// [`Projection::sealed_actors`] — per actor whose seal AND whole snapshot are
/// held, the keys that actor still asserts. It is computed HERE, in the same
/// walk, because it is a reading of the same op set and a second walk somewhere
/// else would be a second answer to it (ARCH §10.6). Two conditions, both
/// necessary and argued in the module docs:
///
/// - the actor's [`snapshot_mark`] for its CURRENT floor is among the applied
///   ops, and
/// - no [`RailGap::SequenceHole`] names that actor, so the run from the floor
///   to the mark has no missing seq — every row of the snapshot is on this node.
///
/// The per-actor fold is over that actor's OWN ops at or above its floor, and
/// it is deliberately not the namespace-wide winner: a key another actor won is
/// a row that node holds under the OTHER origin, and reconciliation matches on
/// origin.
pub fn project(admission: &Admission) -> Projection {
    // (t, actor, op id) -> the winning act's value. BTreeMap so `rows` comes
    // out key-ordered on every node without a second sort.
    let mut best: BTreeMap<String, (u64, String, u64, Option<Bytes>)> = BTreeMap::new();
    let mut unreadable = 0usize;

    // Only actors admission gave a floor — an actor that has never sealed makes
    // no claim about its whole set, and there is nothing here to fill in.
    let holed: BTreeSet<&str> = admission
        .gaps
        .iter()
        .filter_map(|g| match g {
            RailGap::SequenceHole { actor, .. } => Some(actor.as_str()),
            _ => None,
        })
        .collect();
    // Every actor with a floor gets an entry here; the ones whose snapshot is
    // CLOSED — the mark below — also land in `marked`. Two collections rather
    // than a flag on one, because they answer different questions: what the
    // actor's post-floor history says, and whether that history is all of it.
    let mut live: BTreeMap<&str, BTreeMap<String, (u64, u64, bool)>> = admission
        .floors
        .keys()
        .map(|a| (a.as_str(), BTreeMap::new()))
        .collect();
    let mut marked: BTreeSet<&str> = BTreeSet::new();

    for op in admission.applied() {
        let Some(payload) = op.payload.as_ref() else {
            // `applies()` already guarantees `payload.is_some()`; this arm is
            // the type's, not the rail's.
            continue;
        };
        let floor = admission.floors.get(&op.actor).copied();
        if floor.is_some_and(|f| op.seq < f) {
            // RETIRED by its own author's seal. `RingJournal::compact` deletes
            // exactly these lines — strictly below the floor `admit` reported —
            // so folding them would make this node's answer depend on whether
            // its local prune had run yet, and would resurrect precisely the
            // keys a snapshot declined to re-append (ARCH §10.6).
            tracing::debug!(
                actor = %op.actor,
                seq = op.seq,
                floor = floor.unwrap_or_default(),
                "rail_kv.retired_by_seal"
            );
            continue;
        }
        if let Some(closed_floor) = read_snapshot_mark(payload) {
            // Never a store row, and never `unreadable` — it is this build's
            // own vocabulary, and a count that fires when nothing is wrong
            // stops being read (ARCH §18.3).
            let closes = floor == Some(closed_floor);
            if closes {
                if let Some((actor, _)) = live.get_key_value(op.actor.as_str()) {
                    marked.insert(*actor);
                }
            }
            tracing::debug!(
                actor = %op.actor,
                closed_floor,
                floor = floor.unwrap_or_default(),
                closes,
                "rail_kv.snapshot_mark"
            );
            continue;
        }
        let Some(kv) = from_payload(payload) else {
            unreadable += 1;
            tracing::debug!(
                actor = %op.actor,
                op_id = %op.id,
                "rail_kv.unreadable_act"
            );
            continue;
        };
        if floor.is_some() {
            // This actor's own post-floor history — everything below the floor
            // took the `continue` above — folded on its own. That is the set
            // its snapshot claims to be all of.
            let keys = live
                .get_mut(op.actor.as_str())
                .expect("a floor means an entry");
            let wins = match keys.get(&kv.key) {
                None => true,
                Some((t, held_seq, _)) => (kv.t, op.seq) > (*t, *held_seq),
            };
            if wins {
                keys.insert(kv.key.clone(), (kv.t, op.seq, kv.value.is_some()));
            }
        }
        let wins = match best.get(&kv.key) {
            None => true,
            Some((t, actor, held_seq, _)) => {
                (kv.t, op.actor.as_str(), op.seq) > (*t, actor.as_str(), *held_seq)
            }
        };
        if wins {
            best.insert(kv.key, (kv.t, op.actor.clone(), op.seq, kv.value));
        } else {
            tracing::debug!(
                key = %kv.key,
                actor = %op.actor,
                t = kv.t,
                "rail_kv.superseded"
            );
        }
    }

    let sealed_actors: BTreeMap<String, BTreeSet<String>> = live
        .into_iter()
        .filter_map(|(actor, keys)| {
            let complete = marked.contains(actor) && !holed.contains(actor);
            tracing::debug!(
                actor,
                marked = marked.contains(actor),
                holed = holed.contains(actor),
                live = keys.values().filter(|(_, _, live)| *live).count(),
                complete,
                "rail_kv.sealed_actor"
            );
            complete.then(|| {
                (
                    actor.to_string(),
                    keys.into_iter()
                        .filter(|(_, (_, _, live))| *live)
                        .map(|(key, _)| key)
                        .collect(),
                )
            })
        })
        .collect();

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
        sealed_actors = sealed_actors.len(),
        held = admission.held,
        gaps = admission.gaps.len(),
        "rail_kv.projected"
    );
    Projection {
        rows,
        unreadable,
        sealed_actors,
    }
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

    /// The act that closes a snapshot, as its author would have signed it.
    fn mark(floor: u64) -> RailAct {
        RailAct::Record {
            payload: snapshot_mark(floor).unwrap(),
        }
    }

    /// What the fold says `actor` still asserts, or `None` when it makes no
    /// claim at all — the two are different facts and this keeps them so.
    fn live_set(p: &Projection, k: &commonwealth_rail_core::SigningKey) -> Option<Vec<String>> {
        p.sealed_actors
            .get(&commonwealth_rail_core::actor_of(k))
            .map(|keys| keys.iter().cloned().collect())
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
    /// `(ts_unix, actor, seq, id)` — when a line was written DOWN. That is not the
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

    /// One actor, one key, two writes in one second: the later `seq` wins on
    /// every node. The old last term was the op id, a content hash — so the
    /// pair of values below is CHOSEN so that the earlier write's id sorts
    /// higher, which is exactly the case the id rule got backwards.
    #[test]
    fn two_writes_by_one_actor_in_one_second_fold_in_program_order() {
        let alex = key(1);
        let mut picked = None;
        for n in 0..64u8 {
            let first = signed(&alex, 100, 0, write("k", Some(&[b'a', n]), 777));
            let second = signed(&alex, 100, 1, write("k", Some(&[b'b', n]), 777));
            if first.id.as_str() > second.id.as_str() {
                picked = Some((first, second, vec![b'b', n]));
                break;
            }
        }
        let (first, second, expected) = picked.expect("a value pair whose ids sort against seq");
        let p = project(&admitted(&[first.clone(), second.clone()]));
        assert_eq!(p.rows.len(), 1);
        assert_eq!(
            p.rows[0].value.as_deref(),
            Some(&expected[..]),
            "the second write in program order must win: {:?}",
            p.rows[0]
        );
        let reversed = project(&admitted(&[second, first]));
        assert_eq!(p, reversed, "arrival order must not matter");
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

    /// **The two shapes on this vocabulary do not overlap.** The mark is read
    /// by field set exactly as a store write is, so the two parsers must
    /// refuse each other's acts — otherwise a key called `snap` or an extra
    /// field could turn a row into a claim about a whole live set.
    #[test]
    fn a_mark_is_not_a_store_write_and_a_store_write_is_not_a_mark() {
        let m = snapshot_mark(2_005).unwrap();
        assert_eq!(read_snapshot_mark(&m), Some(2_005));
        assert!(
            from_payload(&m).is_none(),
            "a mark must never project as a store row"
        );

        let w = to_payload("snap", Some(b"v"), 9).unwrap();
        assert!(
            read_snapshot_mark(&w).is_none(),
            "a key named `snap` is a key"
        );
        assert!(
            read_snapshot_mark(&Payload::new(json!({"snap": 1, "k": "a"})).unwrap()).is_none(),
            "a mark carries ONE field; anything else is another act"
        );
        assert!(read_snapshot_mark(&Payload::new(json!({"snap": -1})).unwrap()).is_none());
    }

    /// **A closed snapshot IS the actor's live set, and a retired key is not
    /// in it.** `gone` was written before the seal and never re-appended —
    /// which is what a tombstone this node published looks like once the seal
    /// that retired it lands. It is still a ROW (the retired op is on this
    /// disk until a prune takes it), and it is not in the live set. That gap
    /// between the two is the whole reconciliation.
    #[test]
    fn a_closed_snapshot_reports_the_actors_whole_live_set() {
        let alex = key(1);
        let a = admitted(&[
            signed(&alex, 100, 0, write("gone", Some(b"stale"), 100)),
            signed(&alex, 101, 1, write("kept", Some(b"v"), 101)),
            signed(&alex, 900, 2, RailAct::Seal),
            signed(&alex, 901, 3, write("kept", Some(b"v"), 101)),
            signed(&alex, 902, 4, mark(2)),
        ]);
        let p = project(&a);
        assert_eq!(
            live_set(&p, &alex).as_deref(),
            Some(&["kept".to_string()][..]),
            "the live set is what the snapshot re-appended, and nothing else"
        );
        assert!(
            !p.rows.iter().any(|r| r.key == "gone"),
            "an op below its author's floor is retired, whether or not this node \
             has pruned it off disk yet"
        );
        // …and that is why the live set has to be reported separately: the fold
        // no longer mentions `gone` at all, so nothing in `rows` would ever take
        // the store row this node wrote before the seal.
        assert_eq!(p.unreadable, 0, "the mark is this build's own vocabulary");
    }

    /// **A tombstone above the floor is not live either**, and this is the
    /// case where the two answers must agree: the row says delete, the live
    /// set says the author no longer asserts it.
    #[test]
    fn a_post_seal_tombstone_is_out_of_the_live_set() {
        let alex = key(1);
        let a = admitted(&[
            signed(&alex, 900, 0, RailAct::Seal),
            signed(&alex, 901, 1, write("k", Some(b"v"), 500)),
            signed(&alex, 902, 2, mark(0)),
            signed(&alex, 903, 3, write("k", None, 600)),
        ]);
        let p = project(&a);
        assert_eq!(live_set(&p, &alex).as_deref(), Some(&[][..]));
        assert_eq!(p.rows.len(), 1);
        assert_eq!(p.rows[0].value, None, "the row is the tombstone");
    }

    /// **A half-arrived snapshot claims NOTHING — and the cheap gates would
    /// both have claimed something.** The ring's pull is chunked, so this is
    /// the state a node is really in between two chunks. Note the second
    /// assertion: `is_complete()` is TRUE here, because the hole audit runs
    /// from the floor and the seal IS the floor. A completeness gate would
    /// have retired every row this actor owns (ARCH §18.2).
    #[test]
    fn a_snapshot_still_arriving_makes_no_claim_about_the_live_set() {
        let alex = key(1);
        let before = [
            signed(&alex, 100, 0, write("a", Some(b"1"), 100)),
            signed(&alex, 101, 1, write("b", Some(b"2"), 101)),
        ];
        // Chunk 1: the seal, and nothing above it yet.
        let a = admitted(&[
            before[0].clone(),
            before[1].clone(),
            signed(&alex, 900, 2, RailAct::Seal),
        ]);
        let p = project(&a);
        assert_eq!(live_set(&p, &alex), None, "no mark, no claim");
        assert!(
            a.is_complete(),
            "and `is_complete()` says yes — which is why it is not the gate"
        );

        // Chunk 2, still partial: one row of the snapshot, no mark.
        let a = admitted(&[
            before[0].clone(),
            before[1].clone(),
            signed(&alex, 900, 2, RailAct::Seal),
            signed(&alex, 901, 3, write("a", Some(b"1"), 100)),
        ]);
        assert_eq!(
            live_set(&project(&a), &alex),
            None,
            "one row above the floor is not a snapshot; the other row would              have been retired on the strength of it"
        );

        // The rest of it, and the claim is made.
        let a = admitted(&[
            before[0].clone(),
            before[1].clone(),
            signed(&alex, 900, 2, RailAct::Seal),
            signed(&alex, 901, 3, write("a", Some(b"1"), 100)),
            signed(&alex, 902, 4, write("b", Some(b"2"), 101)),
            signed(&alex, 903, 5, mark(2)),
        ]);
        assert_eq!(
            live_set(&project(&a), &alex),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    /// **A hole in the run below the mark withdraws the claim.** The mark
    /// alone would be believed by a node that ingested it out of order, or
    /// that could not parse one line of the snapshot — and the missing line is
    /// exactly the row that would then be retired.
    #[test]
    fn a_hole_under_the_mark_withdraws_the_claim() {
        let alex = key(1);
        let held = [
            signed(&alex, 900, 0, RailAct::Seal),
            signed(&alex, 901, 1, write("a", Some(b"1"), 100)),
            // seq 2 — the row for "b" — never arrived.
            signed(&alex, 903, 3, mark(0)),
        ];
        let a = admitted(&held);
        assert!(!a.is_complete(), "the hole is reported");
        assert_eq!(live_set(&project(&a), &alex), None);
    }

    /// **A mark for an OLDER seal closes nothing.** A node that has not
    /// compacted still holds the previous snapshot and its mark; reading that
    /// one as current would claim a live set two seals out of date.
    #[test]
    fn a_mark_from_an_earlier_seal_does_not_close_the_current_one() {
        let alex = key(1);
        let a = admitted(&[
            signed(&alex, 900, 0, RailAct::Seal),
            signed(&alex, 901, 1, write("a", Some(b"1"), 100)),
            signed(&alex, 902, 2, mark(0)),
            // The second seal. Its snapshot has not arrived.
            signed(&alex, 903, 3, RailAct::Seal),
        ]);
        assert_eq!(live_set(&project(&a), &alex), None);
    }

    /// **An actor that never sealed is ABSENT, not empty** (ARCH §18.3). The
    /// two mean opposite things to a reconciliation: absent is "I know nothing
    /// about their whole set", empty is "they assert nothing".
    #[test]
    fn an_actor_that_never_sealed_is_absent_rather_than_empty() {
        let (alex, bo) = (key(1), key(2));
        let a = admitted(&[
            signed(&alex, 100, 0, write("a", Some(b"1"), 100)),
            signed(&bo, 900, 0, RailAct::Seal),
            signed(&bo, 901, 1, mark(0)),
        ]);
        let p = project(&a);
        assert_eq!(live_set(&p, &alex), None, "alex has not sealed");
        assert_eq!(
            live_set(&p, &bo),
            Some(vec![]),
            "bo sealed with nothing live and says so — the K7 case, and the              one a `some op above the floor` gate cannot see"
        );
    }

    /// **One actor's seal says nothing about another's rows.** The per-actor
    /// fold is over that actor's OWN ops, so a key another node won is not in
    /// this one's live set — and the store matches on origin, so it is not
    /// this one's row either.
    #[test]
    fn a_sealed_actors_live_set_is_only_its_own_writes() {
        let (alex, bo) = (key(1), key(2));
        let a = admitted(&[
            signed(&bo, 100, 0, write("shared", Some(b"bo"), 500)),
            signed(&alex, 900, 0, RailAct::Seal),
            signed(&alex, 901, 1, write("mine", Some(b"alex"), 100)),
            signed(&alex, 902, 2, mark(0)),
        ]);
        let p = project(&a);
        assert_eq!(
            live_set(&p, &alex),
            Some(vec!["mine".to_string()]),
            "bo's key is not alex's to retire"
        );
        assert_eq!(value_of(&p, "shared").as_deref(), Some(&b"bo"[..]));
    }
}
