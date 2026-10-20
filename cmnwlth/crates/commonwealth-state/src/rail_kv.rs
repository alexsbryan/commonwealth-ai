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

/// How many of a node's own ops may stand above its last seal before its
/// pump seals again.
///
/// ONE constant for every namespace a pump seals, KV and measurements alike,
/// read by both of cw-rails' sealers — the kv pump's and the plane seal's
/// (`commonwealth_rails::plane_seal`, the daemon's until pb-mesh-exit-mesh)
/// (ARCH §10.6). Two thousand ops is roughly 1.2 MB of journal at
/// the measured ~594-byte line, held by every node in the mesh — small enough
/// that a seal is rare (a household writes on the order of 3,500 ops a year)
/// and large enough that the seal's own cost, one admission plus one
/// snapshot, is amortised over a long stretch of ordinary writes.
pub const SEAL_AFTER_OWN_OPS: usize = 2_000;

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
#[path = "tests/rail_kv.rs"]
mod tests;
