// SPDX-License-Identifier: AGPL-3.0-or-later
//! Records: what a run measured, and what this machine has measured before (split from `mesh_measurements.rs` at the move to serve).

use super::*;

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// Whether a run is fit to be served back.
///
/// A run that tripped a validity guard is still *written* — a discarded failure
/// teaches nobody anything, and silently dropping them would turn the tool into
/// retry-until-lucky. It simply never travels ([`to_wire`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// Every guard passed; this number may be shown.
    Valid,
    /// At least one guard tripped. `problems` is operator-facing prose.
    Invalid {
        /// What went wrong, one entry per tripped guard.
        problems: Vec<String>,
    },
}

impl Verdict {
    /// Whether this run may be served back to a reader.
    pub fn is_valid(&self) -> bool {
        matches!(self, Verdict::Valid)
    }
}

/// What else was true of this machine while the run was taken.
///
/// [`PlacementWitness`] explains *what* a run measured. This explains the
/// *conditions it measured under* — the half that was missing when two runs
/// under one key came back 43% apart and nothing recorded could say why. Every
/// field here is something that can differ between two runs of an identical
/// configuration, which is exactly the class of thing the key cannot hold.
///
/// **Never hashed, never part of [`MeasurementKey`].** Conditions are not
/// identity. Keying on them would give every run a unique unmatched key and
/// destroy the ability to compare runs of the same configuration at all — which
/// is the entire point of the store. So a busy run and a quiet run land under
/// one key, both are kept, and the reader is told which was which.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunConditions {
    /// Slot roles reported resident alongside the measured primary, sorted.
    ///
    /// The primary itself is excluded — it is what was measured, not something
    /// competing with it. So `["embed", "fast"]` means two other models held
    /// memory and could take GPU time during the run.
    ///
    /// Recorded because slot co-residency was the leading suspect for the 43%
    /// spread and could not be checked: it had to be *recalled* rather than
    /// read, and recall could not distinguish "resident in both runs" (a
    /// constant, which cannot explain a difference) from "resident in one".
    pub co_resident_roles: Vec<String>,

    /// Daemon resident-set size in MB at the start of the run, when `/status`
    /// reported it.
    pub host_rss_mb_before: Option<u64>,
    /// The same at the end. A large climb across a short run means something
    /// else on this box was growing while the number was being taken.
    pub host_rss_mb_after: Option<u64>,

    /// Daemon uptime in seconds when the run started.
    ///
    /// Distinguishes a measurement taken on a long-settled daemon from one
    /// taken minutes after a restart, when caches are cold and the supervisor
    /// may still be reconciling.
    pub host_uptime_s: Option<u64>,

    /// Wall-clock seconds spanned by the whole run, first trial to last.
    ///
    /// Not a performance figure — a cross-check. Two runs of the same trial
    /// count whose spans differ sharply were not taken under the same load.
    pub run_span_s: Option<f64>,

    /// The `host:port` addresses ggml actually dialled for each remote worker
    /// carrying weight, in placement order.
    ///
    /// [`LinkClass`] answers only "was the authority loopback", so a `direct`
    /// record says nothing about *which* route the tensors took. A peer
    /// routinely advertises several — a LAN address and an overlay address, say
    /// — and those have different latency floors and degrade differently under
    /// load. Two runs of one configuration can therefore differ by route while
    /// keying identically, which is exactly the unexplainable-spread failure the
    /// rest of this struct exists to close.
    ///
    /// Empty both for a local load and for records written before this was
    /// captured. **Absence is not loopback**: a reader must not infer a route
    /// from an empty list, only that none was recorded.
    #[serde(default)]
    pub rpc_endpoints: Vec<String>,
}

/// One completed measurement run.
///
/// Deliberately carries **no model size**. See the module docs: without a size
/// there is no ratio to extrapolate by, so the banned size-law estimate cannot
/// be reconstructed from this data without someone first adding a field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasurementRecord {
    /// The configuration this run measured.
    pub key: MeasurementKey,

    /// Median steady-state decode rate across the run's trials.
    pub decode_tok_s: f64,
    /// Slowest trial in the run.
    pub decode_tok_s_min: f64,
    /// Fastest trial in the run.
    pub decode_tok_s_max: f64,
    /// Median time to first content token.
    pub ttft_ms: f64,
    /// Median inter-token latency.
    pub itl_p50_ms: f64,
    /// 95th-percentile inter-token latency — where link jitter shows up.
    pub itl_p95_ms: f64,
    /// Prefill rate, present only when the server reported real prompt-token
    /// counts. `None` renders as "n/a", never as an estimate from string
    /// length.
    pub prefill_tok_s: Option<f64>,
    /// Seconds spent loading the model, when this run paid for a cold load.
    pub cold_load_s: Option<f64>,
    /// Timed trials contributing to this run (warm-up excluded).
    pub trials: u32,
    /// Content frames observed, summed across trials.
    pub content_frames: u32,

    /// Human-facing model name. Provenance only — never keyed on.
    pub model_name: String,
    /// Human-facing placement, e.g. `"36 local + 12 @beefymac"`.
    pub placement_human: String,
    /// Machines holding blocks.
    pub nodes: u32,
    /// Network hops per token (`nodes - 1` for a single-stream pipeline).
    pub hops: u32,
    /// Unix seconds at which the run completed.
    pub measured_at: u64,
    /// Build that took the measurement.
    pub build: String,
    /// Engine-reported backend, for display and for spotting a payload/key
    /// disagreement.
    pub backend: Option<String>,
    /// Measured round-trip time to the furthest worker, when distributed.
    pub link_rtt_ms: Option<f64>,

    /// Whether this run may be served back.
    pub verdict: Verdict,

    /// The inputs behind [`MeasurementKey::placement_digest`], so this record can
    /// explain itself to a reader who did not run it. See [`PlacementWitness`].
    ///
    /// `None` for a record written before 2026-07-30, when only the hash was
    /// kept. Those records still serve exact hits perfectly well — the witness is
    /// explanatory, not part of the identity — so unlike the v1→v2 change
    /// (see [`SCHEMA_VERSION`], where the missing field was a *key* field and
    /// keeping the rows would have kept rows that could never match) they are
    /// preserved rather than discarded. The cost of keeping them is that they
    /// cannot say what they measured beyond `placement_human`, and the surfaces
    /// that read a witness say so instead of guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<PlacementWitness>,

    /// What else was true of this machine while the run was taken. See
    /// [`RunConditions`].
    ///
    /// `None` for a record written before 2026-07-30. Kept rather than
    /// discarded for the same reason as a witness-less record: conditions are
    /// explanatory, not part of the identity, so an old row still serves an
    /// exact hit perfectly well. The cost of keeping it is that it cannot say
    /// what else was running — and the surfaces that read conditions say
    /// exactly that instead of implying the box was quiet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<RunConditions>,
}

/// The on-disk file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasurementFile {
    pub(super) schema_version: u32,
    records: Vec<MeasurementRecord>,
}

impl Default for MeasurementFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            records: Vec::new(),
        }
    }
}

impl MeasurementFile {
    /// An empty file at the current schema.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every record, newest last. Includes invalid runs — they are glassbox
    /// material.
    pub fn records(&self) -> &[MeasurementRecord] {
        &self.records
    }
}

/// Append a run, evicting the oldest under the same key past
/// [`MAX_RUNS_PER_KEY`].
///
/// Runs accumulate rather than overwrite so that repeated measurement shows
/// spread. Eviction is per key, so a busy configuration cannot push another
/// configuration's history out.
pub fn record(file: &mut MeasurementFile, rec: MeasurementRecord) {
    let key = rec.key.clone();
    file.records.push(rec);

    let mut idx: Vec<usize> = file
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.key == key)
        .map(|(i, _)| i)
        .collect();

    while idx.len() > MAX_RUNS_PER_KEY {
        // Oldest by measured_at; ties break on insertion order.
        let victim_pos = idx
            .iter()
            .enumerate()
            .min_by_key(|(_, &i)| (file.records[i].measured_at, i))
            .map(|(pos, _)| pos)
            .expect("idx is non-empty inside the loop");
        let victim = idx.remove(victim_pos);
        file.records.remove(victim);
        for i in idx.iter_mut() {
            if *i > victim {
                *i -= 1;
            }
        }
    }
}
