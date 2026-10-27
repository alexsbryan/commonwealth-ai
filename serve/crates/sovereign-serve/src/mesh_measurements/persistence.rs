// SPDX-License-Identifier: AGPL-3.0-or-later
//! The local file and the wire form a record travels in (split from
//! `mesh_measurements.rs` at the move to serve).

use super::*;

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Resolved store path, or `None` when disabled.
///
/// `SOVEREIGN_MESH_MEASUREMENTS=0` turns the whole mechanism off: nothing is
/// read or written, which is the escape hatch for a machine that should
/// not keep this history.
pub fn store_path() -> Option<PathBuf> {
    match std::env::var("SOVEREIGN_MESH_MEASUREMENTS") {
        Ok(v) if v == "0" => None,
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v)),
        _ => Some(sovereign_contracts::rebrand::svrnmesh_root().join("mesh-measurements.json")),
    }
}

/// Parse a store, discarding one written by an incompatible schema.
///
/// Never fails: an unreadable or superseded file is treated as an empty one,
/// because losing measurement history is an inconvenience while refusing to
/// plan is a broken command.
pub fn parse(contents: &str) -> MeasurementFile {
    match serde_json::from_str::<MeasurementFile>(contents) {
        Ok(f) if f.schema_version == SCHEMA_VERSION => f,
        Ok(f) => {
            tracing::debug!(
                found = f.schema_version,
                expected = SCHEMA_VERSION,
                "mesh-measurements: discarding store written by an incompatible schema"
            );
            MeasurementFile::new()
        }
        Err(e) => {
            tracing::debug!(error = %e, "mesh-measurements: unreadable store — starting empty");
            MeasurementFile::new()
        }
    }
}

/// Read the store from disk. Empty when disabled, absent, or unreadable.
pub fn load() -> MeasurementFile {
    let Some(path) = store_path() else {
        return MeasurementFile::new();
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => parse(&s),
        Err(_) => MeasurementFile::new(),
    }
}

/// Write the store to disk. No-op when disabled.
pub fn save(file: &MeasurementFile) -> std::io::Result<()> {
    let Some(path) = store_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, body)
}

// ---------------------------------------------------------------------------
// Travel
// ---------------------------------------------------------------------------
//
// The durable local file is authoritative; the ring journal carries copies.
// Publication is idempotent by content — `wire_key` is derived from the
// record, so republishing the same record is recognised rather than
// accumulated (`crate::measurements_rail::republish`). Peer records are never
// merged into [`MeasurementFile`].

/// A record on the wire, versioned so a future incompatible change is *dropped*
/// by an older reader rather than half-understood.
///
/// Private: the only way to produce these bytes is [`to_wire`] and the only way
/// to read them is [`from_wire`], so the version check cannot be skipped by a
/// caller who forgot it existed. This is the same discipline [`parse`] applies
/// to the file, applied to the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MeasurementEnvelope {
    schema_version: u32,
    record: MeasurementRecord,
}

/// Serialize a record for publication, or `None` if it must not travel.
///
/// Refuses an [`Verdict::Invalid`] run. A failed run is glassbox material *for
/// the operator who ran it* — it says their machine could not do the thing.
/// On a peer it is only noise, and worse,
/// it is noise a reader could mistake for a capability claim about hardware they
/// were considering. The local file keeps every run; the wire carries only the
/// ones that mean something to a stranger.
pub fn to_wire(record: &MeasurementRecord) -> Option<Vec<u8>> {
    if !record.verdict.is_valid() {
        return None;
    }
    serde_json::to_vec(&MeasurementEnvelope {
        schema_version: SCHEMA_VERSION,
        record: record.clone(),
    })
    .ok()
}

/// Read a record published by a peer, or `None` if it cannot be trusted as one.
///
/// Never fails loudly: a peer on a different schema, or a corrupt entry, yields
/// `None` and is skipped. One unreadable entry must not cost the reader every
/// other peer's measurements, and there is nothing an operator could do about a
/// remote node's version anyway.
pub fn from_wire(bytes: &[u8]) -> Option<MeasurementRecord> {
    let env: MeasurementEnvelope = serde_json::from_slice(bytes).ok()?;
    if env.schema_version != SCHEMA_VERSION {
        tracing::debug!(
            found = env.schema_version,
            expected = SCHEMA_VERSION,
            "mesh-measurements: dropping a peer record written by an incompatible schema"
        );
        return None;
    }
    Some(env.record)
}

/// The KV key a record publishes under.
///
/// Derived entirely from the record, so publishing the same record twice is a
/// no-op rather than a duplicate — which is what makes the boot republish safe
/// to run on every start.
///
/// `measured_at` leads, zero-padded, so that lexicographic order is
/// chronological order: a raw `scan` of the namespace reads oldest-to-newest
/// without decoding anything, and a date prefix is a usable scan filter. The
/// hash tail is over the key fields *and* the headline rate, so two runs of the
/// same configuration in the same second stay distinct instead of one silently
/// replacing the other.
///
/// The rate enters the hash **quantized to a thousandth of a token per second**,
/// and that is load-bearing rather than tidy. `serde_json` is built here without
/// its `float_roundtrip` feature, so an `f64` can come back from JSON one ULP
/// away from what went in — and a record passes through JSON twice, once to the
/// local file and once to the wire. Hashing the raw bits would therefore let the
/// same measurement compute two different keys depending on which copy you held,
/// and the boot republish would leave an orphan entry behind that LWW could
/// never overwrite. A thousandth of a token per second is far below anything a
/// reader could act on, so the tolerance costs nothing; two runs closer together
/// than that in the same second are the same measurement, and keeping one is
/// right.
pub fn wire_key(record: &MeasurementRecord) -> String {
    let k = &record.key;
    let mut h = Sha256::new();
    h.update(k.model_fingerprint.as_bytes());
    h.update([0u8]);
    h.update(k.placement_digest.as_bytes());
    h.update([0u8]);
    h.update(k.host_hw_fingerprint.to_le_bytes());
    h.update(k.n_ctx.to_le_bytes());
    h.update(k.probe_version.to_le_bytes());
    h.update(k.link.as_str().as_bytes());
    h.update([0u8]);
    h.update(record.measured_at.to_le_bytes());
    h.update(((record.decode_tok_s * 1000.0).round() as i64).to_le_bytes());
    format!("{:010}/{}", record.measured_at, hex16(&h.finalize()))
}
