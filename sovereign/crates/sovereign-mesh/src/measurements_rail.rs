// SPDX-License-Identifier: AGPL-3.0-or-later
//! `mesh-measurements` on the ring rail — the namespace's journal vocabulary.
//!
//! # Why this namespace moved off the gossip store
//!
//! A measurement is a fact somebody recorded, and the gossip KV store could
//! not keep one. `MeshStore` is `in_memory()` in production — a wire buffer,
//! not storage — so every record this node had ever published evaporated on
//! restart, and the repair was a boot step that re-uploaded the whole local
//! file into the buffer on every start. The rail is an append-only journal on
//! disk that converges by anti-entropy, which is what the data actually
//! wanted: a record is written once and stays written.
//!
//! It is also the one namespace that fits the rail today. Records are capped
//! per configuration by `MAX_RUNS_PER_KEY`, so the journal never has to
//! forget — which is the property the other candidates lack.
//!
//! # ONE namespace name, ONE wire form
//!
//! The namespace is [`MEASUREMENTS_APP_ID`](mm::MEASUREMENTS_APP_ID), reused
//! verbatim. Minting a second spelling for the rail side would be two answers
//! to what this data is called (ARCH §10.6).
//!
//! The bytes are [`mm::to_wire`]'s, unchanged, and they are read back by
//! [`mm::from_wire`]. That keeps the schema-version check un-skippable and
//! keeps "an invalid run does not travel" true on the rail exactly as it was
//! on the wire — one door, not two.
//!
//! # Why the record rides as a STRING and not as an object
//!
//! [`Payload`] refuses any fractional number, and it is right to: a `Value`
//! has no bytes, only a serializer whose spelling of `100.0` is a library
//! choice, and every node must derive identical bytes from identical facts or
//! the signatures stop verifying. A [`MeasurementRecord`](mm::MeasurementRecord)
//! is nine `f64`s — `decode_tok_s`, `ttft_ms`, the two latency percentiles —
//! so it cannot be a payload object at all, and
//! `a_measurement_record_cannot_be_a_rail_payload_directly` pins that.
//!
//! The wrapper is not a way around the rule; it satisfies it more directly
//! than the alternative would. A JSON string has exactly one spelling, and
//! the bytes inside it are the author's own — produced once by [`mm::to_wire`]
//! and never re-serialized by a reader — so the re-serialization hazard the
//! rule exists to prevent is not merely checked, it is absent.
//!
//! The alternative was quantizing every rate into an integer unit, and this
//! module's own `wire_key` has already priced that: a rate that passes through
//! JSON can return one ULP away from what went in, so a second integer
//! spelling of a rate is a second identity for one measurement. One
//! representation, and it is the one both the file and the wire already use.

use commonwealth_rail::{
    Admission, Ed25519Verifier, Op, Payload, Person, RailAct, RingJournal, RingSigner, Roster,
    SignedOp,
};
use sovereign_core::mesh_measurements as mm;

/// What a `mesh-measurements` journal line says it is. Present so a reader —
/// `svrn ring log`, a future second act on this namespace — can tell the act
/// apart without decoding it, which is what `kind` is for on every rail app.
const PAYLOAD_KIND: &str = "mesh-measurement";

/// The field carrying [`mm::to_wire`]'s bytes. See the module docs for why
/// it is a string.
const PAYLOAD_WIRE: &str = "wire";

/// Wrap one record as a rail payload, or say why it cannot travel.
///
/// The refusals are sentences because they reach an operator through
/// `POST /v1/mesh/measurements`'s `refused` field, and "the gossip buffer
/// rejected the write" was never something a person could act on.
pub fn to_payload(record: &mm::MeasurementRecord) -> Result<Payload, String> {
    let Some(bytes) = mm::to_wire(record) else {
        return Err("an invalid run does not travel".to_string());
    };
    let wire = String::from_utf8(bytes)
        .map_err(|_| "this run could not be encoded as a journal line".to_string())?;
    Payload::new(serde_json::json!({ "kind": PAYLOAD_KIND, PAYLOAD_WIRE: wire }))
        .map_err(|e| e.to_string())
}

/// Read a record back off a journal line, or `None` if this line is not one
/// we can read.
///
/// Never fails loudly, for the reason [`mm::from_wire`] does not: a peer on a
/// different schema must not cost the reader every other peer's
/// measurements. The count of these is reported to the caller instead.
pub fn from_payload(payload: &Payload) -> Option<mm::MeasurementRecord> {
    let obj = payload.as_value().as_object()?;
    if obj.get("kind")?.as_str()? != PAYLOAD_KIND {
        return None;
    }
    mm::from_wire(obj.get(PAYLOAD_WIRE)?.as_str()?.as_bytes())
}

/// Sign one locally-taken record onto this node's journal.
///
/// `roster` is derived from mesh membership
/// ([`MeshRoster`](crate::ring_roster::MeshRoster)) and is what
/// [`RingJournal::append`] checks our own key against. A node that cannot
/// place its own key is REFUSED here rather than allowed to write ops every
/// peer would report as `UnknownSigner` — and the refusal is a sentence, not
/// a silent drop. Nothing is lost by refusing: `svrn mesh bench` writes
/// `~/.svrnmesh/mesh-measurements.json` before it ever POSTs, and
/// [`republish`] carries the file onto the journal once an identity exists.
pub fn publish(
    journal: &RingJournal,
    signer: &dyn RingSigner,
    roster: &Roster,
    record: &mm::MeasurementRecord,
) -> Result<Op<SignedOp>, String> {
    let payload = to_payload(record)?;
    journal
        .append(RailAct::Record { payload }, signer, roster)
        .map_err(|e| e.to_string())
}

/// One measurement a peer put on this journal.
#[derive(Debug, Clone)]
pub struct RailMeasurement {
    /// The signing public key — the only field on the line a writer cannot
    /// forge for someone else (ARCH §18.1).
    pub actor: String,
    /// Who the roster says that key is. Resolved by admission, never read out
    /// of the payload the publisher controls.
    pub person: Person,
    pub record: mm::MeasurementRecord,
}

/// What one read of the journal found.
#[derive(Debug, Clone, Default)]
pub struct RailMeasurements {
    /// Newest first.
    pub found: Vec<RailMeasurement>,
    /// Admitted lines this build could not read as a measurement, usually a
    /// peer on an incompatible schema.
    pub unreadable: usize,
    /// Everything admission could not account for — an unplaceable signer, a
    /// hole in a peer's sequence, a torn line. Reported rather than swallowed:
    /// an empty `found` beside a non-zero `gaps` is a very different fact from
    /// an empty `found` beside a quiet ring (ARCH §18.3).
    pub gaps: usize,
}

/// Read an admission as measurements, dropping the ops `exclude_actor`
/// signed and capping each publisher's history per configuration at
/// [`mm::MAX_RUNS_PER_KEY`] — the depth their own file keeps.
///
/// Our own runs live in `~/.svrnmesh/mesh-measurements.json`, which is the
/// authoritative copy; returning them here would show the operator their own
/// measurement wearing their own node name, as though a stranger had
/// confirmed it. `None` excludes nothing — the diagnostic path.
pub fn read(admission: &Admission, exclude_actor: Option<&str>) -> RailMeasurements {
    let mut out = RailMeasurements {
        gaps: admission.gaps.len(),
        ..Default::default()
    };
    for op in admission.applied() {
        if exclude_actor == Some(op.actor.as_str()) {
            continue;
        }
        let Some(payload) = op.payload.as_ref() else {
            continue;
        };
        match from_payload(payload) {
            Some(record) => out.found.push(RailMeasurement {
                actor: op.actor.clone(),
                person: op.person.clone(),
                record,
            }),
            None => out.unreadable += 1,
        }
    }
    out.found
        .sort_by_key(|m| std::cmp::Reverse(m.record.measured_at));

    // The publisher's own file keeps at most `MAX_RUNS_PER_KEY` runs per
    // configuration, FIFO, so repeated runs make variance visible without
    // unbounded growth. A journal has no such cap — it is append-only and
    // forgets nothing — so the cap is applied HERE, per publisher per
    // configuration, against the SAME constant (ARCH §10.6). Without it a
    // reader would see deeper history than the publisher's own
    // `mesh bench --history` shows, and the gap would widen every time
    // anybody re-benched a configuration they had already measured.
    //
    // This is retention, not a refusal: what is dropped is the OLDEST runs of
    // a configuration whose newest eight are all here, so it is never the
    // difference between an answer and no answer. It is traced rather than
    // counted into `unreadable`, which is reserved for lines this node could
    // not read — calling a deliberate cap an unreadable line would report a
    // subset where the answer is complete.
    //
    // Linear rather than hashed: `MeasurementKey` already answers equality
    // and a ring holds a handful of configurations, so a second, hashable
    // spelling of the key would be an identity to keep in sync for nothing
    // (ARCH §7.5).
    let mut depth: Vec<(String, mm::MeasurementKey, usize)> = Vec::new();
    let mut retired = 0usize;
    let mut kept = Vec::with_capacity(out.found.len());
    for m in out.found {
        match depth
            .iter_mut()
            .find(|(actor, key, _)| actor == &m.actor && key == &m.record.key)
        {
            Some((_, _, n)) if *n >= mm::MAX_RUNS_PER_KEY => {
                retired += 1;
                continue;
            }
            Some((_, _, n)) => *n += 1,
            None => depth.push((m.actor.clone(), m.record.key.clone(), 1)),
        }
        kept.push(m);
    }
    out.found = kept;

    tracing::debug!(
        namespace = mm::MEASUREMENTS_APP_ID,
        held = admission.held,
        found = out.found.len(),
        // Older than the per-configuration depth the publisher's own file
        // keeps. Named on the event a reader already looks at when asking why
        // a run they took is not in the list (ARCH §9.1).
        retired,
        unreadable = out.unreadable,
        gaps = out.gaps,
        "mesh-measurements: read the ring journal"
    );
    out
}

/// What [`republish`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Republished {
    pub appended: usize,
    pub already_held: usize,
    /// Runs [`mm::to_wire`] refuses, or that this node cannot author.
    pub withheld: usize,
}

/// Put every local record that is not already on the journal onto it.
///
/// The rail replaces the old boot step rather than repeating it. That step
/// existed because the gossip buffer was in memory and lost this node's whole
/// history on every restart; a journal on disk does not, so this is a
/// **migration and a repair**, not a periodic upload:
///
/// - the records a node filed before this namespace moved to the rail reach
///   the ring the first time it boots on this build, and
/// - a run published while this node could not place its own key (see
///   [`publish`]) lands the moment an identity exists — which is what stops
///   the refusal from being a quiet loss.
///
/// Idempotent by content, not by op id: an op id folds in `seq` and the
/// timestamp, so re-appending the same record would mint a new line every
/// boot. What is compared is [`mm::wire_key`], which is derived from the
/// record itself.
pub fn republish(
    journal: &RingJournal,
    signer: &dyn RingSigner,
    roster: &Roster,
    records: &[mm::MeasurementRecord],
) -> Republished {
    let mut out = Republished::default();
    if records.is_empty() {
        return out;
    }
    let mine = signer.actor();
    let held: std::collections::BTreeSet<String> = match journal.admit(roster, &Ed25519Verifier) {
        Ok(admission) => admission
            .ops
            .iter()
            .filter(|o| o.actor == mine)
            .filter_map(|o| o.payload.as_ref())
            .filter_map(from_payload)
            .map(|r| mm::wire_key(&r))
            .collect(),
        Err(e) => {
            // Refusing to guess. Appending against an unreadable journal would
            // duplicate every record this node holds, every boot.
            tracing::warn!(error = %e, "mesh-measurements: journal unreadable, republish skipped");
            return out;
        }
    };
    for record in records {
        if held.contains(&mm::wire_key(record)) {
            out.already_held += 1;
            continue;
        }
        match publish(journal, signer, roster, record) {
            Ok(_) => out.appended += 1,
            Err(why) => {
                out.withheld += 1;
                tracing::debug!(why, "mesh-measurements: a local record stayed home");
            }
        }
    }
    if out.appended > 0 || out.withheld > 0 {
        tracing::info!(
            appended = out.appended,
            already_held = out.already_held,
            withheld = out.withheld,
            total = records.len(),
            "mesh-measurements: local history reconciled onto the ring journal"
        );
    }
    out
}

#[cfg(test)]
pub(crate) mod tests;
