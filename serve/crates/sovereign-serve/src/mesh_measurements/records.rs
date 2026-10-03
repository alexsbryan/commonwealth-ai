// SPDX-License-Identifier: AGPL-3.0-or-later
//! Records and lookup: what a run measured, and what this machine has
//! measured before (split from `mesh_measurements.rs` at the move to serve).

use super::*;

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

impl RunConditions {
    /// Whether anything shared the GPU with the measured primary.
    pub fn had_co_residents(&self) -> bool {
        !self.co_resident_roles.is_empty()
    }

    /// Growth in daemon RSS across the run, when both ends were reported.
    ///
    /// Signed on purpose: a *drop* is as interesting as a climb, because it
    /// means a slot was evicted mid-run.
    pub fn rss_delta_mb(&self) -> Option<i64> {
        match (self.host_rss_mb_before, self.host_rss_mb_after) {
            (Some(a), Some(b)) => Some(b as i64 - a as i64),
            _ => None,
        }
    }

    /// One line an operator can read, or `None` when nothing was captured.
    ///
    /// Deliberately says "nothing else resident" rather than staying silent
    /// when the slot list is empty: absence of co-residents is a *finding*
    /// about the run, not absence of information about it.
    pub fn describe(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if self.co_resident_roles.is_empty() {
            parts.push("nothing else resident".to_string());
        } else {
            parts.push(format!(
                "also resident: {}",
                self.co_resident_roles.join(", ")
            ));
        }
        if let Some(rss) = self.host_rss_mb_before {
            match self.rss_delta_mb() {
                Some(d) if d != 0 => {
                    parts.push(format!("daemon rss {rss} MB ({d:+} MB over the run)"))
                }
                _ => parts.push(format!("daemon rss {rss} MB")),
            }
        }
        if let Some(up) = self.host_uptime_s {
            parts.push(format!("daemon up {}", human_duration(up)));
        }
        // Named, not counted: "2 workers" would not distinguish the LAN route
        // from the overlay route, which is the whole reason this is recorded.
        if !self.rpc_endpoints.is_empty() {
            parts.push(format!("rpc via {}", self.rpc_endpoints.join(", ")));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" · "))
        }
    }
}

/// Compact duration for operator-facing condition lines.
pub(super) fn human_duration(secs: u64) -> String {
    match secs {
        s if s < 90 => format!("{s}s"),
        s if s < 5400 => format!("{}m", s / 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
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
    /// material, and `svrn mesh bench --history` shows them.
    pub fn records(&self) -> &[MeasurementRecord] {
        &self.records
    }
}

// ---------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------

/// What [`lookup`] serves back: the MEDIAN valid run for a key (by decode
/// rate), with the observed spread of run medians across every valid run
/// under it.
///
/// The headline policy is deliberate and was an explicit operator call
/// (2026-07-29, THE_NEXT_MONTH Item Three): "latest" let whichever run
/// happened most recently set the number a stranger is told, and a mean
/// would synthesise a run nobody ran. The median run is a run that actually
/// happened, and one outlier cannot set it — the case that forced the call
/// was a 122B split whose four runs sat at 7.75/8.38/8.53/11.08, where
/// "latest" plus a trial-extreme range quoted the band as 7.5–11.5.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasurementSummary {
    /// Headline decode rate — the median valid run's rate. Every other
    /// single-run field below comes from that same run, so the summary
    /// always describes one real run, never a composite.
    pub decode_tok_s: f64,
    /// Slowest valid run's rate under this key. A run's rate is already its
    /// median trial, so this is the observed floor across runs — NOT the
    /// slowest single trial, which would widen the band with within-run
    /// jitter the medians absorb.
    pub decode_tok_s_min: f64,
    /// Fastest valid run's rate under this key (observed ceiling, per above).
    pub decode_tok_s_max: f64,
    /// Median run's median time to first token.
    pub ttft_ms: f64,
    /// Median run's median inter-token latency.
    pub itl_p50_ms: f64,
    /// Median run's 95th-percentile inter-token latency.
    pub itl_p95_ms: f64,
    /// Median run's prefill rate, when the server reported one.
    pub prefill_tok_s: Option<f64>,
    /// Context length these numbers were taken at.
    pub n_ctx: u32,
    /// Backend these numbers were taken on.
    pub backend: Option<String>,
    /// Valid runs under this key.
    pub runs: u32,
    /// When the median run completed.
    pub measured_at: u64,
    /// Build that took the median run.
    pub measured_build: String,
    /// Whether that build differs from the one asking. Not a reason to hide the
    /// number — a reason to show it with a warning.
    pub stale: bool,
    /// Human-facing placement of the median run.
    pub placement_human: String,
    /// Human-facing model name of the median run.
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
///
/// A key whose [`link`](MeasurementKey::link) is [`LinkClass::Unknown`] never
/// matches, *including against a stored `Unknown`*. Two runs we could not
/// classify are not thereby the same run — that would be inferring an identity
/// from a shared absence of evidence, which is precisely the fabrication this
/// module exists to prevent. The caller reports "not measured" and
/// [`near_misses`] still names what *was* measured, so the operator sees the
/// number that exists and why it does not apply.
pub fn lookup(
    file: &MeasurementFile,
    key: &MeasurementKey,
    current_build: &str,
) -> Option<MeasurementSummary> {
    if key.link == LinkClass::Unknown {
        return None;
    }
    let mut valid: Vec<&MeasurementRecord> = file
        .records
        .iter()
        .filter(|r| &r.key == key && r.verdict.is_valid())
        .collect();
    if valid.is_empty() {
        return None;
    }

    // The median run by decode rate; ties break on recency so the pick is
    // deterministic. For an even count the LOWER middle is taken — the
    // conservative side, and still a run that actually happened (averaging
    // the two middles would quote a rate nobody measured). See the policy
    // note on [`MeasurementSummary`].
    valid.sort_by(|a, b| {
        a.decode_tok_s
            .partial_cmp(&b.decode_tok_s)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.measured_at.cmp(&b.measured_at))
    });
    let median = valid[(valid.len() - 1) / 2];

    Some(MeasurementSummary {
        decode_tok_s: median.decode_tok_s,
        decode_tok_s_min: valid.first().expect("non-empty").decode_tok_s,
        decode_tok_s_max: valid.last().expect("non-empty").decode_tok_s,
        ttft_ms: median.ttft_ms,
        itl_p50_ms: median.itl_p50_ms,
        itl_p95_ms: median.itl_p95_ms,
        prefill_tok_s: median.prefill_tok_s,
        n_ctx: median.key.n_ctx,
        backend: median.backend.clone(),
        runs: valid.len() as u32,
        measured_at: median.measured_at,
        measured_build: median.build.clone(),
        stale: median.build != current_build,
        placement_human: median.placement_human.clone(),
        model_name: median.model_name.clone(),
    })
}

/// One concrete way two configurations differ.
///
/// The facet vocabulary is shared with [`NearMiss::differs_by`], and a
/// `Difference` is strictly a *refinement* of it: the same facets, with the two
/// sides described where they can be. Nothing appears here that would not also
/// appear there, so a caller reading only the facet names is never told about a
/// difference the older surface hid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    /// Stable identifier: `"model"`, `"split"`, `"host-hardware"`,
    /// `"context"`, `"probe-version"`, `"link"`.
    pub facet: &'static str,
    /// What the other configuration had.
    ///
    /// `None` when that side kept no [`PlacementWitness`] — or kept one that
    /// does not account for its own key. The difference is real either way; what
    /// is missing is any honest description of it, and a placeholder here would
    /// be exactly the fabrication this module exists to prevent.
    pub theirs: Option<String>,
    /// What this configuration has, under the same rule.
    pub ours: Option<String>,
}

/// A configuration to compare: a key, plus the witness that explains it when one
/// was kept.
#[derive(Debug, Clone, Copy)]
pub struct Configuration<'a> {
    /// The identity.
    pub key: &'a MeasurementKey,
    /// The pre-image behind [`MeasurementKey::placement_digest`], when recorded.
    pub witness: Option<&'a PlacementWitness>,
}

impl<'a> Configuration<'a> {
    /// A configuration with no witness — all that a pre-2026-07-30 record, or a
    /// caller that has not built one, can offer.
    pub fn unwitnessed(key: &'a MeasurementKey) -> Self {
        Self { key, witness: None }
    }

    /// The witness, but only if it accounts for this key's digest.
    ///
    /// The faithfulness check is applied here, at the point of *use*, rather
    /// than left as something a caller may remember to call. A witness built
    /// from different inputs than the key beside it describes some other
    /// configuration; quoting it would be worse than saying nothing, so it is
    /// treated as absent.
    fn faithful(&self) -> Option<&'a PlacementWitness> {
        self.witness
            .filter(|w| w.explains(&self.key.placement_digest))
    }

    fn split_description(&self) -> Option<String> {
        Some(self.faithful()?.describe_split())
    }

    fn host_description(&self) -> Option<String> {
        Some(
            self.faithful()?
                .machine_with_hw(self.key.host_hw_fingerprint)?
                .describe(),
        )
    }
}

/// Every way `theirs` differs from `ours`, described where it can be.
///
/// The one primitive behind both surfaces that need it: [`near_misses`], which
/// compares a stored record against a plan, and any caller asking the question
/// the store could not answer before a witness existed — *why did these two runs
/// of mine land under different keys?*
///
/// Facet order is by how much it should change a reader's mind, not
/// alphabetical: what model, then how it was split, then on what, then the
/// settings.
pub fn differences(theirs: Configuration<'_>, ours: Configuration<'_>) -> Vec<Difference> {
    let mut out = Vec::new();
    let (t, o) = (theirs.key, ours.key);

    if t.model_fingerprint != o.model_fingerprint {
        out.push(Difference {
            facet: "model",
            theirs: Some(t.model_fingerprint.clone()),
            ours: Some(o.model_fingerprint.clone()),
        });
    }
    if t.placement_digest != o.placement_digest {
        out.push(Difference {
            facet: "split",
            theirs: theirs.split_description(),
            ours: ours.split_description(),
        });
    }
    if t.host_hw_fingerprint != o.host_hw_fingerprint {
        out.push(Difference {
            facet: "host-hardware",
            theirs: theirs.host_description(),
            ours: ours.host_description(),
        });
    }
    if t.n_ctx != o.n_ctx {
        out.push(Difference {
            facet: "context",
            theirs: Some(t.n_ctx.to_string()),
            ours: Some(o.n_ctx.to_string()),
        });
    }
    if t.probe_version != o.probe_version {
        out.push(Difference {
            facet: "probe-version",
            theirs: Some(t.probe_version.to_string()),
            ours: Some(o.probe_version.to_string()),
        });
    }
    if t.link != o.link {
        out.push(Difference {
            facet: "link",
            theirs: Some(t.link.as_str().to_string()),
            ours: Some(o.link.as_str().to_string()),
        });
    }
    out
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
    /// `"probe-version"`, `"link"`.
    ///
    /// Derived from [`Self::detail`] rather than computed alongside it, so the
    /// two can never disagree about what differs.
    pub differs_by: Vec<&'static str>,
    /// The same differences, with both sides described wherever a
    /// [`PlacementWitness`] allows it.
    ///
    /// This is the surface that carries the weight once a measurement can come
    /// from a machine the reader has never seen: an exact key hit pins the
    /// silicon *and* the split, so a stranger will almost never get one, and
    /// "differs by: split, host-hardware" gives them nothing to judge with.
    /// One entry per element of `differs_by`, in the same order.
    pub detail: Vec<Difference>,
    /// Which machine took this, when it was not this one.
    ///
    /// `None` is the local store — the reader's own past run. `Some(name)` names
    /// the peer whose daemon gossiped it. The distinction is not cosmetic and it
    /// is not presentational: a local near miss is evidence about hardware the
    /// reader controls and can re-measure, a peer's is evidence about hardware
    /// they have never seen and cannot check. Rendering the two identically
    /// would let a stranger's number pass for something the reader had measured,
    /// which is the same failure as an extrapolation, just sourced differently.
    pub taken_by: Option<String>,

    /// One line describing what else was running when this was taken, from
    /// [`RunConditions::describe`]. `None` when the record predates conditions.
    ///
    /// Carried here because this is where a number the reader cannot check gets
    /// offered to them. A peer's rate on a box with three other models resident
    /// and its RSS climbing is a different claim from the same rate on a quiet
    /// one, and a reader shown only the rate is in exactly the position that
    /// produced a false 43% variance on this fleet: comparing two numbers
    /// without being told they were taken under different loads.
    pub conditions: Option<String>,
}

impl NearMiss {
    /// Whether this measured *exactly* the configuration that was asked about.
    ///
    /// Only reachable for a peer's record. A local one is filtered out of
    /// [`near_misses`] by construction, because an exact local hit is what
    /// [`lookup`] is for. A peer's is kept, because a key is a claim about a
    /// configuration and not about a filesystem: someone with the same silicon,
    /// split, link and context measured the thing being asked about, and
    /// discarding that because it arrived over the network would throw away the
    /// most informative record travel can deliver.
    ///
    /// It is still not served as the answer — [`lookup`] reads local records
    /// only, so `mesh plan` continues to say "not measured *here*" and offers
    /// this beside it, attributed. Named rather than left as an empty-vec test
    /// so the branch reads as a decision at the call site.
    pub fn is_exact(&self) -> bool {
        self.detail.is_empty()
    }
}

/// Valid measurements of the same model in other configurations, newest first,
/// drawn from the local store **and** from whatever peers have gossiped.
///
/// Restricted to the same `model_fingerprint`: a different model's number is
/// not a near miss, it is an unrelated fact.
///
/// The two sources are treated alike in every respect but one — each result
/// carries [`NearMiss::taken_by`], so a peer's number can never be read as the
/// reader's own. They are ranked together by recency rather than kept in
/// separate lists, because the question ("what is the closest thing anyone has
/// actually measured?") does not care which disk the answer sat on.
///
/// Local records matching `key` exactly are excluded — that is a hit, not a
/// near miss, and [`lookup`] serves it. Peer records matching exactly are
/// **kept**, with an empty `detail`; see [`NearMiss::is_exact`] for why.
///
/// `peers` may be empty, which is the whole behaviour on a solo node and the
/// behaviour on any node whose daemon is not reachable. Nothing here needs the
/// mesh to be up; a missing peer half is silently a smaller answer, never an
/// error, because the local half is the part the operator can act on today.
///
/// `ours` is the caller's own [`PlacementWitness`], when it has one. Passing
/// `None` costs nothing that was ever there — the facets are still named — but
/// it does mean every `split` and `host-hardware` difference comes back
/// undescribed, because describing a difference needs both sides. That cost
/// lands hardest on the peer half, where those two facets are exactly what
/// differs.
pub fn near_misses(
    file: &MeasurementFile,
    peers: &[ForeignRecord],
    key: &MeasurementKey,
    ours: Option<&PlacementWitness>,
) -> Vec<NearMiss> {
    let mine = Configuration { key, witness: ours };
    let describe = |r: &MeasurementRecord, taken_by: Option<String>| {
        let detail = differences(
            Configuration {
                key: &r.key,
                witness: r.witness.as_ref(),
            },
            mine,
        );
        NearMiss {
            placement_human: r.placement_human.clone(),
            decode_tok_s: r.decode_tok_s,
            measured_at: r.measured_at,
            differs_by: detail.iter().map(|d| d.facet).collect(),
            detail,
            taken_by,
            conditions: r.conditions.as_ref().and_then(|c| c.describe()),
        }
    };

    let mut out: Vec<NearMiss> = file
        .records
        .iter()
        .filter(|r| r.verdict.is_valid())
        .filter(|r| r.key.model_fingerprint == key.model_fingerprint)
        .filter(|r| &r.key != key)
        .map(|r| describe(r, None))
        .collect();

    out.extend(
        peers
            .iter()
            .filter(|f| f.record.verdict.is_valid())
            .filter(|f| f.record.key.model_fingerprint == key.model_fingerprint)
            .map(|f| describe(&f.record, Some(f.describe_origin()))),
    );

    // Recency first; an exact peer hit outranks an older near one at equal
    // timestamps, since it is strictly more informative.
    out.sort_by(|a, b| {
        b.measured_at
            .cmp(&a.measured_at)
            .then_with(|| a.detail.len().cmp(&b.detail.len()))
    });
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
