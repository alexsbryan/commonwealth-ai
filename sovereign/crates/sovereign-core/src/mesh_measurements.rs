// SPDX-License-Identifier: AGPL-3.0-or-later
//! Measured throughput for one model on one mesh split — recorded, never
//! estimated.
//!
//! `svrn mesh plan` answers "will this model fit on my machines" from a GGUF
//! header parse alone: offline, no GPU, instant even on a 400 GB split. The
//! question it could not answer is "what will it feel like." This module is the
//! memory that lets it answer — and, just as importantly, the structure that
//! keeps it from *making the answer up*.
//!
//! ## The design rule: measure, don't predict
//!
//! Nothing here estimates. A record is written only after a real run against a
//! real placement, and it is served back only for the *exact* configuration it
//! was taken on. Where there is no record, the honest output is "not measured"
//! plus the command that would measure it — never an interpolation between
//! records that do exist.
//!
//! That rule is not caution for its own sake. `SCHEDULER_QUALITY.md` §4.5
//! priced the alternative against the simulator: a throughput number
//! extrapolated from a baseline probe onto a differently-sized model reads
//! −56%, doubles declined upgrades, and is filed **DO-NOT-BUILD**. The live
//! extrapolator is `throughput_factor` (`oicp-types/src/scoring.rs:384`), and it
//! is already wired end to end — the only thing keeping it dark is that nothing
//! populates `NodeCapabilities.benchmark`. See the guard test
//! `gossip_never_advertises_a_benchmark` in `sovereign-mesh`.
//!
//! **These records must never reach that field.** They are a different thing
//! for a different consumer: §4.5 measured a number's worth to the scheduler's
//! automatic ranked dispatch, through a clamp that only carries information
//! about nodes slower than the reference rate. These records are read by a
//! *person* deciding whether to add a machine, move the host role, or buy
//! hardware. That person has no clamp, and a number worth 0% to a ranker can be
//! decisive for them.
//!
//! Note what [`MeasurementRecord`] deliberately does **not** carry: any model
//! size. There is nothing here from which a size ratio could be computed, so
//! re-deriving the banned extrapolation would require first *adding a field* —
//! a reviewable act, rather than a one-line temptation.
//!
//! ## Why the key has exactly these five parts
//!
//! A cache key that is too coarse fabricates; one that is too fine never hits.
//! Each field of [`MeasurementKey`] is here because dropping it would serve a
//! real number for a configuration it was not measured on:
//!
//! | Field | Drop it and… |
//! |---|---|
//! | `model_fingerprint` | a Q4 number is shown for a Q8 plan |
//! | `placement_digest` | a 36/12 split's number is shown for a 24/24 plan |
//! | `host_hw_fingerprint` | one machine's number is shown on another's |
//! | `n_ctx` | an 8k number is shown for a 128k plan (decode rate tracks KV size) |
//! | `probe_version` | numbers taken by different methods are compared as equals |
//!
//! And what is deliberately *excluded*: RPC endpoint ports (DHCP churn would
//! make every lookup a miss), and the probe's prompt text and token counts
//! (they are protocol constants folded into `probe_version`, not key fields —
//! keying on them would drive the hit rate to zero).
//!
//! The GPU backend is folded *inside* `host_hw_fingerprint` rather than sitting
//! beside it, because a ROCm↔Vulkan swap shifts throughput materially on
//! identical silicon without changing the GPU's name — so it has to break the
//! key, not merely annotate it.
//!
//! ## Known blind spot
//!
//! Worker hardware is only as specific as the `node_key` each caller supplies.
//! A worker that swaps a GPU for a different one of the same capacity, while
//! keeping its name, produces a stale hit. The fix is to fold each worker's own
//! hardware fingerprint into its `node_key`; the shape here supports that
//! without a schema change, because `node_key` is an opaque caller-supplied
//! string.
//!
//! ## Storage
//!
//! `~/.sovereign/mesh-measurements.json`. `SOVEREIGN_MESH_MEASUREMENTS=<path>`
//! relocates it; `SOVEREIGN_MESH_MEASUREMENTS=0` disables reads and writes
//! entirely. Records are append-only per key, capped at [`MAX_RUNS_PER_KEY`]
//! (FIFO) so repeated runs make variance *visible* rather than averaging it
//! away — a thermally-throttled or link-jittery machine should look unstable,
//! not merely slow.
//!
//! There is no time-based expiry. A measurement does not rot: the hardware that
//! produced it is pinned in the key. What can change underneath it is the
//! inference engine, so every record stamps the build that took it and
//! [`lookup`] flags a mismatch as stale rather than discarding it. Silently
//! dropping a record would cost the operator a re-measurement for nothing;
//! silently refreshing it would spend twenty minutes they did not ask for.
//! Showing the age and letting them judge is the whole premise of the tool.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Bumped when the probe protocol changes in any way that makes new numbers
/// incomparable with old ones: the prompt, the token budget, the timing
/// formula, or the set of validity guards. Old records then stop matching
/// rather than being silently compared against numbers taken differently.
pub const PROBE_VERSION: u32 = 1;

/// Bumped when the on-disk layout changes incompatibly. A file at a different
/// version is discarded wholesale.
const SCHEMA_VERSION: u32 = 1;

/// Per-key run cap. Keeps variance visible without unbounded growth.
pub const MAX_RUNS_PER_KEY: usize = 8;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Proof that a real, present host was identified — the token
/// [`MeasurementKey::for_plan`] requires.
///
/// This exists to make one rule structural instead of conventional: **a
/// hypothetical mesh can never match a measurement.** `svrn mesh plan
/// --devices 64,32,32` describes hardware that is not here, so there is no host
/// to fingerprint and no measurement that could honestly apply to it. Rather
/// than rely on a runtime `if` that a later refactor could drop, the key simply
/// cannot be *constructed* without this value, and the only way to obtain one
/// is [`HostIdentity::from_live_mesh`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostIdentity(u64);

impl HostIdentity {
    /// Identify the host from a fingerprint read off the live mesh.
    ///
    /// `None` when the host advertises no fingerprint — an older peer, or a
    /// node whose hardware detection came up empty. A caller that gets `None`
    /// must report "not measured" rather than substituting a placeholder:
    /// a shared default would collide every unidentified host into one key.
    pub fn from_live_mesh(hw_fingerprint: Option<u64>) -> Option<Self> {
        hw_fingerprint.map(Self)
    }

    /// The underlying fingerprint, for display and JSON emission.
    pub fn fingerprint(self) -> u64 {
        self.0
    }
}

/// Stable hash of one machine's hardware.
///
/// Small on purpose — this needs equality against a previously recorded value,
/// not cryptographic uniqueness. `gpus` is `(name, vram_gb, backend)`, and the
/// backend string is part of the hash because the same silicon driven through
/// different backends is, for throughput purposes, different hardware.
///
/// Order-independent across GPUs: the same two cards enumerated in either order
/// hash identically, so a driver-order change does not invalidate a record.
pub fn hardware_fingerprint(
    cpu_cores: u32,
    system_ram_gb: u32,
    gpus: &[(String, u32, String)],
) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Hash each GPU independently, then combine with a commutative fold so
    // enumeration order cannot change the result.
    let mut gpu_mix: u64 = 0;
    for (name, vram_gb, backend) in gpus {
        let mut g = DefaultHasher::new();
        name.hash(&mut g);
        vram_gb.hash(&mut g);
        backend.hash(&mut g);
        gpu_mix ^= g.finish();
    }

    let mut h = DefaultHasher::new();
    cpu_cores.hash(&mut h);
    system_ram_gb.hash(&mut h);
    gpus.len().hash(&mut h);
    gpu_mix.hash(&mut h);
    h.finish()
}

/// Fingerprint a model from its GGUF tensor table — `"mf1:<16 hex>"`.
///
/// `sizes` is the `(tensor_name, layer, nbytes)` table that `mesh plan` already
/// parses; only name and byte count participate. Properties that matter:
///
/// - **Order-independent.** The table is sorted before hashing, so two reads of
///   the same file agree regardless of enumeration order.
/// - **Quantisation-sensitive.** Byte counts are hashed, so Q4 and Q8 of the
///   same model are different models here — which is the point.
/// - **Rename-proof.** Nothing about the file's path or name is included.
/// - **Free.** This is a header parse the caller has already done.
pub fn model_fingerprint(sizes: &[(String, Option<u32>, u64)], block_count: u32) -> String {
    let mut rows: Vec<(&str, u64)> = sizes.iter().map(|(n, _, b)| (n.as_str(), *b)).collect();
    rows.sort_unstable();

    let mut h = Sha256::new();
    h.update(b"mf1");
    h.update(block_count.to_le_bytes());
    h.update((rows.len() as u64).to_le_bytes());
    for (name, nbytes) in &rows {
        h.update(name.as_bytes());
        h.update([0u8]); // delimiter: "ab"+"c" must not collide with "a"+"bc"
        h.update(nbytes.to_le_bytes());
    }
    format!("mf1:{}", hex16(&h.finalize()))
}

/// One device's share of a placement, as the digest sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementShard {
    /// Stable identity of the machine holding this share.
    ///
    /// Callers pass a mesh member *name* where the RPC endpoint resolves to a
    /// known peer, and an endpoint host with the port dropped otherwise. Ports
    /// must not appear: they churn across restarts and would make every lookup
    /// a miss.
    pub node_key: String,
    /// Inclusive block range this device holds, or `None` if it holds none.
    pub blocks: Option<(u32, u32)>,
    /// Whether this device carries the output head.
    pub holds_output: bool,
}

/// Fingerprint a placement — `"pd1:<16 hex>"`.
///
/// `mode` distinguishes a single-machine load from a split one, so the same
/// model measured solo and measured distributed are never confused. Shards are
/// sorted by `node_key`, making the digest independent of the order the mesh
/// happened to enumerate its members.
pub fn placement_digest(mode: &str, total_blocks: u32, shards: &[PlacementShard]) -> String {
    let mut sorted: Vec<&PlacementShard> = shards.iter().collect();
    sorted.sort_unstable_by(|a, b| a.node_key.cmp(&b.node_key));

    let mut h = Sha256::new();
    h.update(b"pd1");
    h.update(mode.as_bytes());
    h.update([0u8]);
    h.update(total_blocks.to_le_bytes());
    h.update((sorted.len() as u64).to_le_bytes());
    for s in sorted {
        h.update(s.node_key.as_bytes());
        h.update([0u8]);
        match s.blocks {
            Some((lo, hi)) => {
                h.update([1u8]);
                h.update(lo.to_le_bytes());
                h.update(hi.to_le_bytes());
            }
            None => h.update([0u8]),
        }
        h.update([u8::from(s.holds_output)]);
    }
    format!("pd1:{}", hex16(&h.finalize()))
}

fn hex16(digest: &[u8]) -> String {
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// The key
// ---------------------------------------------------------------------------

/// The identity of a measurable configuration. Two runs share a key only if
/// they are genuinely the same thing measured twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeasurementKey {
    /// See [`PROBE_VERSION`].
    pub probe_version: u32,
    /// See [`model_fingerprint`].
    pub model_fingerprint: String,
    /// See [`placement_digest`].
    pub placement_digest: String,
    /// See [`hardware_fingerprint`], for the host.
    pub host_hw_fingerprint: u64,
    /// Context length the measurement was taken at. Decode rate is a function
    /// of KV size, so 8k and 128k are not the same measurement.
    pub n_ctx: u32,
}

impl MeasurementKey {
    /// Build the key for a plan against a real, present mesh.
    ///
    /// Requires a [`HostIdentity`], which is what bars a `--devices`
    /// hypothetical from ever matching a record. Always stamps the current
    /// [`PROBE_VERSION`]: a caller cannot ask for a number taken by a
    /// superseded method.
    pub fn for_plan(
        host: HostIdentity,
        model_fingerprint: String,
        placement_digest: String,
        n_ctx: u32,
    ) -> Self {
        Self {
            probe_version: PROBE_VERSION,
            model_fingerprint,
            placement_digest,
            host_hw_fingerprint: host.fingerprint(),
            n_ctx,
        }
    }
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// Whether a run is fit to be served back.
///
/// A run that tripped a validity guard is still *written* — a discarded failure
/// teaches nobody anything, and silently dropping them would turn the tool into
/// retry-until-lucky. It is simply never returned by [`lookup`].
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
    /// Build that took the measurement. A mismatch marks a lookup stale.
    pub build: String,
    /// Engine-reported backend, for display and for spotting a payload/key
    /// disagreement.
    pub backend: Option<String>,
    /// Measured round-trip time to the furthest worker, when distributed.
    pub link_rtt_ms: Option<f64>,

    /// Whether this run may be served back.
    pub verdict: Verdict,
}

/// The on-disk file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasurementFile {
    schema_version: u32,
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
    /// material, and `svrn mesh bench --history` shows them.
    pub fn records(&self) -> &[MeasurementRecord] {
        &self.records
    }
}

// ---------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------

/// What [`lookup`] serves back: the latest valid run for a key, with the spread
/// across every valid run under it.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasurementSummary {
    /// Headline decode rate — the latest valid run's median.
    pub decode_tok_s: f64,
    /// Slowest trial across every valid run under this key.
    pub decode_tok_s_min: f64,
    /// Fastest trial across every valid run under this key.
    pub decode_tok_s_max: f64,
    /// Latest run's median time to first token.
    pub ttft_ms: f64,
    /// Latest run's median inter-token latency.
    pub itl_p50_ms: f64,
    /// Latest run's 95th-percentile inter-token latency.
    pub itl_p95_ms: f64,
    /// Latest run's prefill rate, when the server reported one.
    pub prefill_tok_s: Option<f64>,
    /// Context length these numbers were taken at.
    pub n_ctx: u32,
    /// Backend these numbers were taken on.
    pub backend: Option<String>,
    /// Valid runs under this key.
    pub runs: u32,
    /// When the latest valid run completed.
    pub measured_at: u64,
    /// Build that took the latest valid run.
    pub measured_build: String,
    /// Whether that build differs from the one asking. Not a reason to hide the
    /// number — a reason to show it with a warning.
    pub stale: bool,
    /// Human-facing placement of the latest valid run.
    pub placement_human: String,
    /// Human-facing model name of the latest valid run.
    pub model_name: String,
}

/// The measured numbers for exactly this configuration, or `None`.
///
/// Exact match on the whole key, and invalid runs are skipped. There is no
/// nearest-neighbour fallback and no interpolation: a caller that gets `None`
/// must say "not measured", because any number it could synthesise here would
/// describe a configuration nobody ran.
///
/// `current_build` is passed in rather than read from the environment so this
/// stays a pure function — the whole module is testable without a filesystem,
/// a daemon, or a GPU.
pub fn lookup(
    file: &MeasurementFile,
    key: &MeasurementKey,
    current_build: &str,
) -> Option<MeasurementSummary> {
    let valid: Vec<&MeasurementRecord> = file
        .records
        .iter()
        .filter(|r| &r.key == key && r.verdict.is_valid())
        .collect();

    let latest = valid.iter().max_by_key(|r| r.measured_at)?;

    let min = valid
        .iter()
        .map(|r| r.decode_tok_s_min)
        .fold(f64::INFINITY, f64::min);
    let max = valid
        .iter()
        .map(|r| r.decode_tok_s_max)
        .fold(f64::NEG_INFINITY, f64::max);

    Some(MeasurementSummary {
        decode_tok_s: latest.decode_tok_s,
        decode_tok_s_min: min,
        decode_tok_s_max: max,
        ttft_ms: latest.ttft_ms,
        itl_p50_ms: latest.itl_p50_ms,
        itl_p95_ms: latest.itl_p95_ms,
        prefill_tok_s: latest.prefill_tok_s,
        n_ctx: latest.key.n_ctx,
        backend: latest.backend.clone(),
        runs: valid.len() as u32,
        measured_at: latest.measured_at,
        measured_build: latest.build.clone(),
        stale: latest.build != current_build,
        placement_human: latest.placement_human.clone(),
        model_name: latest.model_name.clone(),
    })
}

/// A measurement of the same model in a *different* configuration.
///
/// This exists so the tool can say "the split you are proposing has not been
/// measured; the one you are running measured 14.1 tok/s" — which is exactly
/// how an operator decides whether to move the host role. It names the other
/// configuration and its number; it does **not** combine, scale, or interpolate
/// them toward the configuration that was asked about.
#[derive(Debug, Clone, PartialEq)]
pub struct NearMiss {
    /// Human-facing placement of the configuration that *was* measured.
    pub placement_human: String,
    /// Its measured decode rate.
    pub decode_tok_s: f64,
    /// When it was measured.
    pub measured_at: u64,
    /// Which parts of the key differ from the one asked about. Stable
    /// identifiers: `"split"`, `"host-hardware"`, `"context"`,
    /// `"probe-version"`.
    pub differs_by: Vec<&'static str>,
}

/// Valid measurements of the same model in other configurations, newest first.
///
/// Restricted to the same `model_fingerprint`: a different model's number is
/// not a near miss, it is an unrelated fact. Returns an empty vec when the key
/// matches exactly (that is a hit, not a near miss — call [`lookup`]).
pub fn near_misses(file: &MeasurementFile, key: &MeasurementKey) -> Vec<NearMiss> {
    let mut out: Vec<NearMiss> = file
        .records
        .iter()
        .filter(|r| r.verdict.is_valid())
        .filter(|r| r.key.model_fingerprint == key.model_fingerprint)
        .filter(|r| &r.key != key)
        .map(|r| {
            let mut differs_by = Vec::new();
            if r.key.placement_digest != key.placement_digest {
                differs_by.push("split");
            }
            if r.key.host_hw_fingerprint != key.host_hw_fingerprint {
                differs_by.push("host-hardware");
            }
            if r.key.n_ctx != key.n_ctx {
                differs_by.push("context");
            }
            if r.key.probe_version != key.probe_version {
                differs_by.push("probe-version");
            }
            NearMiss {
                placement_human: r.placement_human.clone(),
                decode_tok_s: r.decode_tok_s,
                measured_at: r.measured_at,
                differs_by,
            }
        })
        .collect();
    out.sort_unstable_by(|a, b| b.measured_at.cmp(&a.measured_at));
    out
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

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Resolved store path, or `None` when disabled.
///
/// `SOVEREIGN_MESH_MEASUREMENTS=0` turns the whole mechanism off: lookups miss
/// and nothing is written, which is the escape hatch for a machine that should
/// not keep this history.
pub fn store_path() -> Option<PathBuf> {
    match std::env::var("SOVEREIGN_MESH_MEASUREMENTS") {
        Ok(v) if v == "0" => None,
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v)),
        _ => dirs::home_dir().map(|h| h.join(".sovereign").join("mesh-measurements.json")),
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sizes() -> Vec<(String, Option<u32>, u64)> {
        vec![
            ("blk.0.attn_q.weight".into(), Some(0), 1_000),
            ("blk.1.ffn_gate.weight".into(), Some(1), 2_000),
            ("output.weight".into(), None, 3_000),
        ]
    }

    fn shards() -> Vec<PlacementShard> {
        vec![
            PlacementShard {
                node_key: "beefymac".into(),
                blocks: Some((0, 11)),
                holds_output: false,
            },
            PlacementShard {
                node_key: "ruggedfox".into(),
                blocks: Some((12, 47)),
                holds_output: true,
            },
        ]
    }

    fn key() -> MeasurementKey {
        MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("distributed", 48, &shards()),
            32_768,
        )
    }

    fn rec_at(k: MeasurementKey, at: u64, tok_s: f64, verdict: Verdict) -> MeasurementRecord {
        MeasurementRecord {
            key: k,
            decode_tok_s: tok_s,
            decode_tok_s_min: tok_s - 0.2,
            decode_tok_s_max: tok_s + 0.1,
            ttft_ms: 910.0,
            itl_p50_ms: 71.0,
            itl_p95_ms: 79.0,
            prefill_tok_s: None,
            cold_load_s: Some(112.3),
            trials: 3,
            content_frames: 256,
            model_name: "Qwen3.5-122B".into(),
            placement_human: "36 local + 12 @beefymac".into(),
            nodes: 2,
            hops: 1,
            measured_at: at,
            build: "0.10.0".into(),
            backend: Some("vulkan".into()),
            link_rtt_ms: Some(0.4),
            verdict,
        }
    }

    // --- key discrimination: too-coarse failures -------------------------

    #[test]
    fn key_changes_when_split_changes() {
        let a = placement_digest("distributed", 48, &shards());
        let mut moved = shards();
        moved[0].blocks = Some((0, 17));
        moved[1].blocks = Some((18, 47));
        let b = placement_digest("distributed", 48, &moved);
        assert_ne!(a, b, "a 36/12 split must not match a 30/18 one");
    }

    #[test]
    fn key_changes_when_worker_identity_changes() {
        let a = placement_digest("distributed", 48, &shards());
        let mut other = shards();
        other[0].node_key = "someone-elses-mac".into();
        let b = placement_digest("distributed", 48, &other);
        assert_ne!(
            a, b,
            "the same split on a different peer is a different measurement"
        );
    }

    #[test]
    fn key_changes_between_solo_and_distributed() {
        let solo = placement_digest("local", 48, &shards());
        let dist = placement_digest("distributed", 48, &shards());
        assert_ne!(solo, dist);
    }

    #[test]
    fn key_changes_when_host_hardware_changes() {
        let a = hardware_fingerprint(32, 128, &[("Radeon 8060S".into(), 128, "vulkan".into())]);
        let b = hardware_fingerprint(32, 128, &[("RTX 4090".into(), 24, "cuda".into())]);
        assert_ne!(a, b);
    }

    #[test]
    fn key_changes_when_backend_changes_on_identical_silicon() {
        let vulkan =
            hardware_fingerprint(32, 128, &[("Radeon 8060S".into(), 128, "vulkan".into())]);
        let rocm = hardware_fingerprint(32, 128, &[("Radeon 8060S".into(), 128, "rocm".into())]);
        assert_ne!(
            vulkan, rocm,
            "same GPU under a different backend runs at a different rate — the key must break"
        );
    }

    #[test]
    fn key_changes_when_ctx_changes() {
        let small = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("distributed", 48, &shards()),
            8_192,
        );
        assert_ne!(small, key());
    }

    #[test]
    fn model_fingerprint_is_quant_sensitive() {
        let mut requantised = sizes();
        requantised[0].2 = 1_500;
        assert_ne!(
            model_fingerprint(&sizes(), 48),
            model_fingerprint(&requantised, 48)
        );
    }

    // --- key stability: too-fine failures --------------------------------

    #[test]
    fn model_fingerprint_is_order_independent() {
        let mut permuted = sizes();
        permuted.reverse();
        assert_eq!(
            model_fingerprint(&sizes(), 48),
            model_fingerprint(&permuted, 48)
        );
    }

    #[test]
    fn placement_digest_is_order_independent() {
        let mut permuted = shards();
        permuted.reverse();
        assert_eq!(
            placement_digest("distributed", 48, &shards()),
            placement_digest("distributed", 48, &permuted)
        );
    }

    #[test]
    fn hardware_fingerprint_is_gpu_order_independent() {
        let a = hardware_fingerprint(
            32,
            128,
            &[
                ("RTX 4090".into(), 24, "cuda".into()),
                ("Radeon 8060S".into(), 128, "vulkan".into()),
            ],
        );
        let b = hardware_fingerprint(
            32,
            128,
            &[
                ("Radeon 8060S".into(), 128, "vulkan".into()),
                ("RTX 4090".into(), 24, "cuda".into()),
            ],
        );
        assert_eq!(
            a, b,
            "driver enumeration order must not invalidate a record"
        );
    }

    #[test]
    fn model_fingerprint_is_stable_across_repeated_reads() {
        assert_eq!(
            model_fingerprint(&sizes(), 48),
            model_fingerprint(&sizes(), 48)
        );
    }

    // --- the hypothetical bar --------------------------------------------

    #[test]
    fn an_unidentified_host_yields_no_identity() {
        assert!(
            HostIdentity::from_live_mesh(None).is_none(),
            "without a host fingerprint there is no key, so `--devices` cannot match a record"
        );
    }

    // --- lookup ----------------------------------------------------------

    #[test]
    fn lookup_returns_none_for_an_unmeasured_key() {
        let f = MeasurementFile::new();
        assert!(lookup(&f, &key(), "0.10.0").is_none());
    }

    #[test]
    fn lookup_ignores_invalid_runs_under_the_exact_key() {
        let mut f = MeasurementFile::new();
        record(
            &mut f,
            rec_at(
                key(),
                100,
                14.1,
                Verdict::Invalid {
                    problems: vec!["peer went offline mid-run".into()],
                },
            ),
        );
        assert!(
            lookup(&f, &key(), "0.10.0").is_none(),
            "a run that tripped a guard is kept for glassbox but must never be served"
        );
    }

    #[test]
    fn lookup_reports_the_latest_run_and_the_spread_across_all_of_them() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key(), 100, 13.0, Verdict::Valid));
        record(&mut f, rec_at(key(), 200, 14.1, Verdict::Valid));
        let s = lookup(&f, &key(), "0.10.0").expect("two valid runs");
        assert_eq!(s.runs, 2);
        assert!(
            (s.decode_tok_s - 14.1).abs() < 1e-9,
            "headline is the latest run"
        );
        assert!(
            (s.decode_tok_s_min - 12.8).abs() < 1e-9,
            "min spans every run"
        );
        assert!(
            (s.decode_tok_s_max - 14.2).abs() < 1e-9,
            "max spans every run"
        );
        assert!(!s.stale);
    }

    #[test]
    fn a_record_from_another_build_is_served_but_flagged_stale() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));
        let s = lookup(&f, &key(), "0.11.0").expect("still served");
        assert!(
            s.stale,
            "a build change is a warning, not a reason to hide the number"
        );
        assert_eq!(s.measured_build, "0.10.0");
    }

    // --- near misses ------------------------------------------------------

    #[test]
    fn a_near_miss_names_the_other_config_and_carries_no_rate_for_ours() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));

        let mut moved = shards();
        moved[0].blocks = Some((0, 17));
        moved[1].blocks = Some((18, 47));
        let asked = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("distributed", 48, &moved),
            32_768,
        );

        assert!(
            lookup(&f, &asked, "0.10.0").is_none(),
            "the configuration asked about was never measured"
        );
        let misses = near_misses(&f, &asked);
        assert_eq!(misses.len(), 1);
        assert_eq!(misses[0].differs_by, vec!["split"]);
        assert!((misses[0].decode_tok_s - 14.1).abs() < 1e-9);
    }

    #[test]
    fn a_different_model_is_not_a_near_miss() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));

        let mut other_model = sizes();
        other_model[0].2 = 9_999;
        let asked = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&other_model, 48),
            placement_digest("distributed", 48, &shards()),
            32_768,
        );
        assert!(near_misses(&f, &asked).is_empty());
    }

    #[test]
    fn an_exact_hit_is_not_also_reported_as_a_near_miss() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));
        assert!(near_misses(&f, &key()).is_empty());
    }

    #[test]
    fn an_invalid_run_is_not_a_near_miss_either() {
        let mut f = MeasurementFile::new();
        record(
            &mut f,
            rec_at(
                key(),
                100,
                14.1,
                Verdict::Invalid {
                    problems: vec!["only 10 content frames".into()],
                },
            ),
        );
        let mut moved = shards();
        moved[0].blocks = Some((0, 17));
        let asked = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("distributed", 48, &moved),
            32_768,
        );
        assert!(near_misses(&f, &asked).is_empty());
    }

    // --- record retention -------------------------------------------------

    #[test]
    fn runs_accumulate_then_evict_oldest_first_per_key() {
        let mut f = MeasurementFile::new();
        for i in 0..(MAX_RUNS_PER_KEY as u64 + 3) {
            record(&mut f, rec_at(key(), 100 + i, 14.0, Verdict::Valid));
        }
        assert_eq!(f.records().len(), MAX_RUNS_PER_KEY);
        let oldest = f.records().iter().map(|r| r.measured_at).min().unwrap();
        assert_eq!(oldest, 103, "the three oldest runs were evicted");
    }

    #[test]
    fn eviction_is_scoped_to_one_key() {
        let mut f = MeasurementFile::new();
        let other = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("local", 48, &shards()),
            32_768,
        );
        record(&mut f, rec_at(other.clone(), 1, 11.7, Verdict::Valid));
        for i in 0..(MAX_RUNS_PER_KEY as u64 + 3) {
            record(&mut f, rec_at(key(), 100 + i, 14.0, Verdict::Valid));
        }
        assert!(
            lookup(&f, &other, "0.10.0").is_some(),
            "a busy configuration must not push another one's history out"
        );
    }

    // --- persistence ------------------------------------------------------

    #[test]
    fn a_store_round_trips() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));
        let json = serde_json::to_string(&f).unwrap();
        let back = parse(&json);
        assert_eq!(
            lookup(&back, &key(), "0.10.0"),
            lookup(&f, &key(), "0.10.0")
        );
    }

    #[test]
    fn a_store_from_an_incompatible_schema_is_discarded_not_misread() {
        let json = r#"{"schema_version":9999,"records":[]}"#;
        assert!(parse(json).records().is_empty());
    }

    #[test]
    fn an_unreadable_store_degrades_to_empty_rather_than_failing() {
        assert!(parse("{ this is not json").records().is_empty());
    }

    #[test]
    fn an_invalid_verdict_round_trips_with_its_problems() {
        let r = rec_at(
            key(),
            100,
            0.0,
            Verdict::Invalid {
                problems: vec!["served by the wrong model".into()],
            },
        );
        let back: MeasurementRecord =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.verdict, r.verdict);
    }
}
