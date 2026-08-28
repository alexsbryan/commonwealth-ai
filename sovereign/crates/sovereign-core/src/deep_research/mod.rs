// SPDX-License-Identifier: AGPL-3.0-or-later
//! deep_research — the THIN local-only research loop (order
//! deep-research-t1a, R0-R9 + R11-thin).
//!
//! The controller drives the R11-thin state machine (`state.rs`): every
//! step writes its versioned ICD artifact into the run directory (FR-2);
//! the charter is frozen at launch (FR-3); abort is an input to every
//! state and lands on a truncated report (truncation declared); the
//! run-scoped lock refuses a second run against the same run dir (F19).
//!
//! The loop's provider boundary is `ResearchPort` (`estate.rs`),
//! implemented by the CLI verb; the loop itself never reaches into
//! corpus-engine, tools-base, or the network directly.

pub mod acquisition;
pub mod audit;
pub mod budget;
pub mod containment;
pub mod enrich;
pub mod estate;
pub mod fetch;
pub mod gym;
pub mod icd;
pub mod launch;
pub mod notes;
pub mod port;
pub mod render;
pub mod search;
pub mod state;
pub mod synthesize;

use audit::{assess_claim, split_claims, ClaimAudit};
use budget::{SpendDecider, FAMILY_WEB_FETCH, FAMILY_WEB_SEARCH, KEY_FETCH_PAGES};
use containment::{strip_citation_spans, ContainmentConfig};
use fetch::fetch_round;
use icd::{
    AcquisitionPlan, AlignmentRecord, BudgetTotals, Charter, CharterValues, CustodyPolicy, Draft,
    EmptyRound, EmptyRoundReason, EvidenceWindow, FailedSource, FetchFailure, FetchList,
    FetchedSource, Gap, GapList, LockRecord, Manifest, Plan, ReframeInput, ReframeRecord,
    ResidueRow, RoundRow, SourceLedger, Survey, TriageConfig, UrlConstraintPolicy, WindowChunk,
};
use render::{build_manifest, final_claims, not_covered, render_report, ManifestInput};
use serde::{Deserialize, Serialize};
use state::{Event, RunLock, State};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::oicp::ShardingPrivacy;
use crate::traits::InferenceProvider;

/// The acquisition's search source — a CLOSED set (rung 2 of the
/// acquisition ladder, order deep-research-t1g), decided ONCE at
/// launch by the CLI's `--search-source` flag (one decider; anything
/// else refuses loudly). Additive: `Mock` is the t1f default — the
/// deck's term-ranked surface; `Corpus` routes the SAME budget ledger
/// (web-search family, key `corpus`, same allowance) to the estate's
/// corpus-search surface. The source is recorded on every artifact
/// (SearchHit.engine `mock` | `corpus` — glassbox).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchSource {
    Mock,
    Corpus,
    /// The rung-3 variant (order deep-research-t2a): the real web
    /// leg through the ONE acquisition decider. Dispatches to the
    /// port's `web_search` identically to `Mock`; the port stamps
    /// web hits `Custody::PublicWeb`, and the run's consent grant
    /// gates the query egress at the boundary (default-deny).
    Web,
}

impl SearchSource {
    /// The ONE decider: a `&str` maps onto the closed set or refuses.
    pub fn parse(s: &str) -> Option<SearchSource> {
        match s {
            "mock" => Some(SearchSource::Mock),
            "corpus" => Some(SearchSource::Corpus),
            "web" => Some(SearchSource::Web),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SearchSource::Mock => "mock",
            SearchSource::Corpus => "corpus",
            SearchSource::Web => "web",
        }
    }
}

/// The budget ledger's key for the acquisition search — ONE decider
/// shared by the allowance map, the continue-to-web gate, and the
/// per-query spend: the source's key (`corpus` for the corpus source,
/// `web` for the web source, the web backend id for the mock). A
/// second key derivation would let the gate and the spend disagree —
/// the shape that silently ended a corpus-source run before it
/// searched.
fn source_budget_key(source: SearchSource, web_backend: &str) -> String {
    match source {
        SearchSource::Mock => web_backend.to_string(),
        SearchSource::Corpus => "corpus".to_string(),
        SearchSource::Web => "web".to_string(),
    }
}

/// Everything a run needs at launch. Values are frozen into the
/// charter at launch (FR-3); nothing here is re-read mid-run.
///
/// Serialize/Deserialize (order deep-research-t3a): the config rides
/// the resume checkpoint (`checkpoint.json`) — the resume restores
/// the run from the checkpoint's config and verifies the operator's
/// re-passed flags against it. The charter hash recomputed from this
/// config is the tamper check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunConfig {
    pub run_id: String,
    pub question: String,
    pub seed_id: Option<String>,
    pub run_dir: PathBuf,
    pub max_rounds: u32,
    pub code_set_k: usize,
    pub eps_quota: f64,
    /// drb1-t2 (fetch-then-judge): the post-fetch content admission
    /// floors — a fetched page admits when its content covers at least
    /// `content_coverage_floor` of the query's distinct terms OR
    /// carries a prose line of at least `prose_line_floor` chars
    /// (calibrated on the logged t7a flight; see
    /// `acquisition::DEFAULT_CONTENT_COVERAGE_FLOOR`). Serde-default:
    /// pre-t2 checkpoints resume with the production floors.
    #[serde(default = "acquisition::default_content_coverage_floor")]
    pub content_coverage_floor: f64,
    #[serde(default = "acquisition::default_prose_line_floor")]
    pub prose_line_floor: usize,
    pub evidence_window_max_chunks: usize,
    pub estate_corpus_ids: Vec<String>,
    pub web_backend: String,
    /// The acquisition search source (t1g rung 2): `Mock` (default) or
    /// `Corpus` — a closed set, decided once at launch.
    pub search_source: SearchSource,
    pub web_search_allowance: u32,
    pub web_fetch_allowance: u32,
    pub posture: ShardingPrivacy,
    /// The run's typed consent grant (order deep-research-t2a) —
    /// operator-issued once at launch (the CLI's `--consent <class>`),
    /// frozen into the charter (FR-3), carried by the port to the
    /// egress boundary, and recorded in the run manifest. `None` is
    /// default-deny: non-public-web egress refuses.
    pub consent: Option<crate::egress::ConsentGrant>,
    /// drb1-r1 Item 3: Optional caller overrides that can only tighten
    /// the charter's ceilings downward. Callers may specify lower values
    /// to constrain resource usage; higher values are clamped to the charter.
    #[serde(default)]
    pub max_rounds_override: Option<u32>,
    #[serde(default)]
    pub max_search_override: Option<u32>,
    #[serde(default)]
    pub max_fetch_override: Option<u32>,
}

/// The run's terminal report card.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub terminal_state: State,
    pub report_path: PathBuf,
    pub manifest: Manifest,
    /// Every artifact written to the run dir (the flight recorder).
    pub artifacts: Vec<String>,
}

/// Drive the loop to a terminal state. `abort` is polled at every state
/// entry; a set flag aborts from wherever the run is and lands on a
/// truncated report (truncation declared).
pub async fn run(
    config: RunConfig,
    port: Arc<dyn ResearchPort>,
    provider: Arc<dyn InferenceProvider>,
    abort: Arc<AtomicBool>,
) -> Result<RunOutcome, String> {
    Controller::start(config, port, provider, abort)
        .await?
        .drive()
        .await
}

/// The resume checkpoint (order deep-research-t3a) — the run's state
/// persisted after every completed round, the restore surface for
/// `--resume <run-id>`. Written by the controller at every round-push
/// site (drive's main body, the GAP-4 reframe branch, and the F4
/// acquisition-free continue), always with the machine at `Rounding` —
/// the invariant: `written_after_round
/// == rounds.len()`, checked at read. The envelope binds the checkpoint
/// to the run: run_id + charter_hash + the full config (the charter
/// hash recomputed from `config` at restore is the tamper check — a
/// checkpoint whose config does not hash to its own charter_hash is
/// refused).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCheckpoint {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub charter_hash: String,
    /// How many rounds had COMPLETED when the checkpoint was written.
    /// The resume continues at `written_after_round + 1`.
    pub written_after_round: u32,
    /// The frozen launch config (FR-3) — the resume's identity.
    pub config: RunConfig,
    // ── the restore-able controller state ────────────────────────────
    pub question: String,
    pub frontier: Vec<String>,
    pub frontier_question: Option<String>,
    pub figure_specifiers: Vec<String>,
    pub reframe_record: Option<ReframeRecord>,
    pub alignment_record: Option<AlignmentRecord>,
    pub re_plans: u32,
    pub residue: Vec<ResidueRow>,
    /// T7b: the rounds that added no evidence — the round-level state
    /// the verdict assembly had no reader for. Recorded at
    /// acquire_round (the moment the round window is final), restored
    /// on resume (serde-default — old checkpoints restore empty), and
    /// surfaced on the verdict set + report (the "No evidence fetched"
    /// section).
    #[serde(default)]
    pub empty_rounds: Vec<EmptyRound>,
    /// drb1-t2: the per-run source registry accumulated so far —
    /// restored on resume (serde-default: pre-t2 checkpoints restore
    /// empty, the resumed run's rows still land).
    #[serde(default)]
    pub source_registry: Vec<icd::SourceRegistryRow>,
    pub windows: Vec<EvidenceWindow>,
    pub prior_gap_texts: Vec<String>,
    pub prior_gaps: Vec<Gap>,
    pub web_refused: bool,
    pub window_capped: bool,
    pub search_calls: u32,
    pub rounds: Vec<RoundRow>,
    pub fetched_sources: Vec<FetchedSource>,
    pub failed_sources: Vec<FailedSource>,
    pub artifacts: Vec<String>,
    pub aborted_at_round: Option<u32>,
}

/// Resume a run interrupted after round N (order deep-research-t3a):
/// restore the controller from `checkpoint.json`, continue at N+1, with
/// the budget ledger restored from its journal (continuity — a resume
/// that can double-spend is refused, never weakened). Every refusal is
/// typed and names the defect: a missing run dir, an already-closed run
/// (manifest present — completed or aborted), a missing/malformed/
/// tampered checkpoint, a foreign or tampered ledger, a re-passed
/// config that does not match the checkpoint, and a live second run
/// (F19 — the stale lock file of a dead process is acquirable, the
/// operator's `--resume` is the visible act).
pub async fn resume(
    config: RunConfig,
    port: Arc<dyn ResearchPort>,
    provider: Arc<dyn InferenceProvider>,
    abort: Arc<AtomicBool>,
) -> Result<RunOutcome, String> {
    Controller::resume_start(config, port, provider, abort)
        .await?
        .drive()
        .await
}

/// Read + verify the run's checkpoint envelope (order
/// deep-research-t3a). The icd/version envelope and the
/// round-consistency invariant are checked here; the charter-hash,
/// config-identity, and ledger-continuity checks live in
/// `resume_start` (they need the caller's config). Public: the CLI
/// verb's `--resume` gate reads the checkpoint through the same
/// reader (one decider, one name).
pub fn read_checkpoint(run_dir: &Path) -> Result<RunCheckpoint, String> {
    let path = run_dir.join("checkpoint.json");
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "no checkpoint at {path:?} — the run never completed a round, nothing to resume ({e})"
        )
    })?;
    let cp: RunCheckpoint =
        serde_json::from_str(&raw).map_err(|e| format!("checkpoint malformed at {path:?}: {e}"))?;
    if cp.icd != "run_checkpoint" || cp.version != icd::ICD_VERSION {
        return Err(format!(
            "checkpoint at {path:?} is not a run checkpoint (icd {:?}, version {}) — \
             foreign or tampered",
            cp.icd, cp.version
        ));
    }
    if cp.written_after_round as usize != cp.rounds.len() {
        return Err(format!(
            "checkpoint at {path:?} is inconsistent: written_after_round {} but {} rounds \
             recorded — tampered",
            cp.written_after_round,
            cp.rounds.len()
        ));
    }
    Ok(cp)
}

/// Field-by-field config identity for the resume gate (order
/// deep-research-t3a). Returns the first mismatching field's name, or
/// None when the configs are identical. Explicit (names the field),
/// C-class, never a model.
fn config_mismatch(a: &RunConfig, b: &RunConfig) -> Option<&'static str> {
    if a.run_id != b.run_id {
        return Some("run_id");
    }
    if a.question != b.question {
        return Some("question");
    }
    if a.seed_id != b.seed_id {
        return Some("seed_id");
    }
    // `run_dir` is deliberately NOT compared (order deep-research-t3a,
    // measured red): it is the resume LOCATION, not an identity field —
    // the charter (the identity, FR-3) never included it. The
    // operator's `--resume <dir>` anchors the run at that dir even when
    // the dir is a faithful copy of the launch dir; `run_id` +
    // question + budget + charter fields are the identity.
    if a.max_rounds != b.max_rounds {
        return Some("max_rounds");
    }
    if a.code_set_k != b.code_set_k {
        return Some("code_set_k");
    }
    if a.eps_quota != b.eps_quota {
        return Some("eps_quota");
    }
    if a.evidence_window_max_chunks != b.evidence_window_max_chunks {
        return Some("evidence_window_max_chunks");
    }
    if a.estate_corpus_ids != b.estate_corpus_ids {
        return Some("estate_corpus_ids");
    }
    if a.web_backend != b.web_backend {
        return Some("web_backend");
    }
    if a.search_source != b.search_source {
        return Some("search_source");
    }
    if a.web_search_allowance != b.web_search_allowance {
        return Some("web_search_allowance");
    }
    if a.web_fetch_allowance != b.web_fetch_allowance {
        return Some("web_fetch_allowance");
    }
    if a.posture != b.posture {
        return Some("posture");
    }
    if a.consent != b.consent {
        return Some("consent");
    }
    None
}

/// How many claim judgements may be in flight at once.
///
/// The audit was strictly serial until 2026-08-24, and the measurement
/// that justified leaving it that way turned out not to hold: the
/// decision log for run dr-1787535219 records **1,351 inference calls,
/// 0 sheds and 0 errors** across a 63-minute compose+audit phase, so the
/// daemon was never contended. `sovereign-server`'s
/// `max_concurrent_turns` defaults to **4** — four slots, one of them
/// used.
///
/// Set to that ceiling and no higher. Past `max_concurrent_turns` the
/// REST path sheds `503 + Retry-After`, which would trade latency for
/// retries; `complete_with_shed_retry` would absorb it, but generating
/// sheds on purpose is not a speedup. If the daemon's ceiling is raised,
/// this is the one line that follows it.
///
/// Ordering is preserved by construction (`buffered`, not
/// `buffer_unordered`) — the verdict set and the gap list are built from
/// the audit sequence, so the output is byte-identical to the serial
/// loop's and only the wall clock changes.
///
/// SET TO 1 ON 2026-08-24, AND THE PREMISE ABOVE IS WHAT WAS WRONG.
/// "Four slots, one used" describes the REQUEST pipeline, not the GPU
/// underneath it. Measured over the 1,136 primary-slot calls of the task-69
/// flight, latency scales with co-residency and throughput does not move:
///
///   in flight   median latency   throughput
///       1            1.7s        0.59 calls/s
///       2            4.2s        0.48
///       3            5.8s        0.52
///       4            7.3s        0.55
///
/// Confirmed against a direct probe of one judge-shaped call on an idle
/// daemon: 1.97s solo, against a 5.8s median at three in flight. A 27B on
/// one GPU is saturated by a single request; concurrency buys nothing here
/// and costs three extra KV contexts on the largest slot. Keep the
/// `buffered` shape — it is what preserves order — and stop paying for
/// depth that does not exist. If the primary slot ever moves somewhere with
/// real parallelism, re-measure this table before raising it.
pub(crate) const AUDIT_CONCURRENCY: usize = 1;

/// drb1-t5: the composed-report deliverable. DEFAULT OFF — a shipped
/// default-off switch carries its `sovereign/DEFAULTS_LEDGER.md` row and
/// its `quality/env-flags.toml` entry.
fn composed_report_enabled() -> bool {
    std::env::var("SOVEREIGN_DR_COMPOSED_REPORT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// drb1-r4: hand the writer distilled FINDINGS instead of retrieved
/// passages (`deep_research::notes`). Default OFF, and it composes with
/// the composed-report switch rather than replacing it: notes feed
/// `compose_report`'s sections, so with the composed report off there is
/// no writer to feed and this flag does nothing. Its
/// `sovereign/DEFAULTS_LEDGER.md` row and `quality/env-flags.toml` entry
/// carry the cost and the reversal condition.
/// drb1-r5: plan the report's OUTLINE instead of writing one section per
/// search query. Default OFF. Requires SOVEREIGN_DR_COMPOSED_REPORT=1 —
/// there is no outline without a composed deliverable to structure. Row and
/// reversal condition in `sovereign/DEFAULTS_LEDGER.md`.
fn report_outline_enabled() -> bool {
    std::env::var("SOVEREIGN_DR_REPORT_OUTLINE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn research_notes_enabled() -> bool {
    std::env::var("SOVEREIGN_DR_RESEARCH_NOTES")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// drb1-r7: the section writer sees GRADED evidence and is told how to use the
/// grade, instead of a flat list it must weigh by itself.
///
/// On-form: `SOVEREIGN_DR_WRITER_CONTRACT_V2=1` (also `true`,
/// case-insensitive); every other value, including unset, keeps the flat
/// evidence block and the v1 contract. Row and reversal condition in
/// `sovereign/DEFAULTS_LEDGER.md`.
///
/// Why it exists: AIQ's writer prompt steers synthesis by a per-note
/// `evidence_judgment` — high-scoring notes are anchors, medium ones support
/// and nuance, low ones are for gaps and caveats
/// (`deep_researcher/prompts/writer.j2`, "Synthesis Contract"). We already
/// COMPUTE that grade on both evidence paths and then discard it: `Finding`
/// carries `usefulness` (0-100) and `notes::findings_block` sorts by it before
/// dropping it, and `synthesize::rank_passages` returns passages in descending
/// cosine order which the evidence block then flattens. The writer is handed an
/// ordered list with the ordering unexplained. This surfaces the grade it
/// already earned and states the obligation that goes with it.
fn writer_contract_v2_enabled() -> bool {
    std::env::var("SOVEREIGN_DR_WRITER_CONTRACT_V2")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// drb1-r8: the deliverable carries the ARCHITECTURE of a report rather than
/// the shape of the pipeline that produced it. Default OFF. Requires
/// `SOVEREIGN_DR_COMPOSED_REPORT=1`; composes with `SOVEREIGN_DR_REPORT_OUTLINE`
/// (that flag decides WHICH sections exist, this one decides what surrounds
/// them). Row and reversal condition in `sovereign/DEFAULTS_LEDGER.md`.
///
/// Diagnosed, not guessed: across 25 scored task-69 judge records the RACE
/// criteria we lose most are `Breadth and Depth of MCP Protocol Description`
/// (ours 6.12 vs the reference's 9.36, in 25 of 25 draws), `Logical Structure
/// and Coherent Flow of Argumentation` (6.60 vs 9.50) and `Formatting, Layout,
/// and Typographical Consistency` (6.98 vs 9.48). The deliverable's H1 was the
/// user's raw prompt sentence, it opened with no summary, closed on internal
/// furniture, and capped at 8 sections — so a question naming two subjects
/// could not afford a section for the second one.
/// drb1-r9: how much of the evidence window one section's writer may see.
/// Default OFF. Requires `SOVEREIGN_DR_COMPOSED_REPORT=1`. Independent of
/// `SOVEREIGN_DR_REPORT_ARCHITECTURE` on purpose — that one decides the
/// deliverable's SHAPE, this one decides how much evidence is behind each
/// part of it, and they must be separable in a measurement.
///
/// Measured on the task-69 control flight `dr-1787742429`: a 1,060,308-char
/// evidence window, of which one section's writer saw 11,200 chars (1.06%)
/// and the whole eight-section report saw 8.5%. MCP material sits in 40 of
/// the window's 46 chunks and the deliverable still has no MCP section.
/// Row and reversal condition in `sovereign/DEFAULTS_LEDGER.md`.
fn report_section_evidence_enabled() -> bool {
    std::env::var("SOVEREIGN_DR_REPORT_SECTION_EVIDENCE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn report_architecture_enabled() -> bool {
    std::env::var("SOVEREIGN_DR_REPORT_ARCHITECTURE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Whether each section's writer is shown the report's outline and its own
/// place in it.
///
/// Deliberately SEPARATE from `SOVEREIGN_DR_REPORT_ARCHITECTURE`: that flag
/// decides the deliverable's SHAPE (title, executive summary, closing), this
/// one decides whether a section knows it is part of one. An arm that moves
/// both cannot tell them apart.
///
/// Why it exists, measured rather than reasoned (2026-08-27, bed
/// `dr-1787807617`): readability is the only RACE dimension still behind the
/// reference, and the judge's objection is that the report "reads like a
/// collection of research findings rather than a single narrative arc". The
/// obvious cause — too many sections — was flown and REFUTED: cutting the
/// outline from 20 sections to 10 left Logical Structure at exactly 8.5 and
/// cost 0.61 overall. The remaining candidate is in the prompt: a section
/// writer is handed the question, its own sub-question, and its evidence, and
/// NOTHING about the sections around it, so every section is composed in
/// isolation no matter how many there are.
fn section_context_enabled() -> bool {
    std::env::var("SOVEREIGN_DR_SECTION_CONTEXT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn aborted(flag: &AtomicBool) -> bool {
    flag.load(Ordering::Relaxed)
}

/// FNV-1a 64 — a stable charter hash across processes and std versions
/// (the charter hash links every artifact in the run).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The deterministic gap→query template (the audit's `query_for` for
/// gaps the floor did NOT cap): the claim's prose, citation spans
/// stripped, non-question figure runs removed (order deep-research-t2c
/// — the strip-3c anti-leak: the query never echoes the estate's
/// figures; the question's own specifiers are the only figure tokens
/// allowed to ride), first 140 chars.
fn template_query(claim: &str, question_specifiers: &[String]) -> String {
    let stripped = strip_disallowed_figures(&strip_citation_spans(claim), question_specifiers);
    stripped.trim().chars().take(140).collect()
}

/// The one gap→query decider (t1d fix 3 — second-origin; t1e —
/// figure-hunting; t2c — strip-3c anti-leak): when the floor capped
/// the claim (its corroboration record fails the floor), the query is
/// a FACT query — the claim's figures plus its content words — so the
/// next round targets the missing second origin by the fact it must
/// carry. A claim the floor did not cap keeps the prose template — and
/// when that template carries no figure specifier, the question's own
/// specifiers are folded in (t1e: a thematic claim's follow-up query
/// still hunts the figures the question implies; the numbers never
/// silently drop out of the acquisition). On BOTH shapes the query
/// carries no figure tokens beyond the question's own (t2c: the estate
/// figures a survey answer quoted must never echo into the next
/// round's query — the measured t1h g2 leak, "30 100 last years trend
/// ..."). Structural, not remembered: the record chooses the floor
/// shape, the specifier presence chooses the fold-in.
fn gap_query_for(
    claim: &str,
    corroboration: Option<&icd::CorroborationRecord>,
    question_specifiers: &[String],
) -> String {
    let floor_capped = corroboration.map(|c| !c.passes_floor).unwrap_or(false);
    if floor_capped {
        fact_query(claim, question_specifiers)
    } else {
        acquisition::figure_hunt_query(
            template_query(claim, question_specifiers),
            question_specifiers,
        )
    }
}

/// The floor-capped gap's FACT query: the claim's figures first (the
/// fact's identity — a second origin must carry the same numbers),
/// then its content words (the subject). Deterministic C-class, no
/// model; capped at 200 chars. The t1c battery measured the template's
/// deadlock this replaces: a long claim's figure sits beyond the
/// 140-char cut, the follow-up query missed the very number the floor
/// demanded, and the missing origin could never surface (R-12: 0/12
/// on the v0 single-origin decks).
fn fact_query(claim: &str, question_specifiers: &[String]) -> String {
    let stripped = strip_citation_spans(claim);
    let mut parts: Vec<String> = Vec::new();
    for f in figure_tokens(&stripped) {
        // The claim's figures ride ONLY when the question's own
        // specifiers carry them (order deep-research-t2c — the
        // strip-3c anti-leak: a second origin must carry the same
        // numbers, but never the estate's echo — the t1h g2 shape,
        // "30 100 last years trend ...", was the estate's own
        // figures in the round-1 query).
        if question_specifiers.contains(&f) && !parts.contains(&f) {
            parts.push(f);
        }
    }
    for w in stripped.split_whitespace() {
        let word = w.trim_matches(|c: char| !c.is_alphanumeric());
        let lower = word.to_ascii_lowercase();
        if word.chars().count() >= 3
            && !is_query_stopword(&lower)
            // Digit-carrying words ARE figure tokens by the ONE
            // decider — never content words (the t1h "100" entered
            // the FACT query as a 3-char content word).
            && figure_tokens(word).is_empty()
            && !parts.iter().any(|p| p.to_ascii_lowercase() == lower)
        {
            parts.push(word.to_string());
        }
    }
    let mut query = String::new();
    for part in parts {
        if query.chars().count() + part.chars().count() + 1 > 200 {
            break;
        }
        if !query.is_empty() {
            query.push(' ');
        }
        query.push_str(&part);
    }
    query
}

/// One maximal figure run — digits plus adjacent ratio/currency
/// punctuation (`$ % : / ,`), trailing sentence separators trimmed —
/// with its BYTE span in the source text (order deep-research-t2c:
/// the anti-leak strip needs the spans; multibyte-safe, the measured
/// estate_snippet precedent). The ONE run finder: `figure_tokens` and
/// `strip_disallowed_figures` both read it.
struct FigureRun {
    token: String,
    start: usize,
    end: usize,
}

/// C-class figure runs with byte spans. Token semantics are unchanged
/// from the pre-t2c `figure_tokens`: every maximal run of digits plus
/// adjacent ratio/currency punctuation, trailing sentence separators
/// popped. Deterministic, no model.
fn figure_runs(s: &str) -> Vec<FigureRun> {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].1.is_ascii_digit() {
            let start_byte = chars[i].0;
            let start_char = i;
            while i < chars.len()
                && (chars[i].1.is_ascii_digit()
                    || matches!(chars[i].1, '$' | '%' | '.' | ':' | '/' | ','))
            {
                i += 1;
            }
            let end_byte = if i < chars.len() { chars[i].0 } else { s.len() };
            let mut token: String = chars[start_char..i].iter().map(|(_, c)| *c).collect();
            while token.ends_with(['.', ',']) {
                token.pop();
            }
            out.push(FigureRun {
                token,
                start: start_byte,
                end: end_byte,
            });
        } else {
            i += 1;
        }
    }
    out
}

/// One entry in the word→digit table (order deep-research-t6d): a
/// spelled number word's digit value, and whether it is a scale word
/// (hundred, thousand — multiplicative).
struct NumberWord {
    value: u64,
    scale: bool,
}

/// The word→digit inversion (order deep-research-t6d, pre-registered):
/// every word in the adversarial generator's NUMBER_WORDS/ORDINAL_WORDS
/// (sovereign-eval/src/flywheel/generators/adversarial.rs:588) maps to
/// its digit value — closed with the units/teens/tens ranges the
/// generator's words imply: the battery's frozen shapes carry
/// "seventeen" and "ninety-five", which the literal arrays lack.
/// Ordinals share the cardinals' values ("twentieth" → 20). Matched
/// case-insensitively, whole-word (the generator's find_word_ci
/// convention). Deterministic, no model.
fn number_word_value(w: &str) -> Option<NumberWord> {
    let (value, scale) = match w.to_ascii_lowercase().as_str() {
        "one" | "first" => (1, false),
        "two" | "second" => (2, false),
        "three" | "third" => (3, false),
        "four" | "fourth" => (4, false),
        "five" | "fifth" => (5, false),
        "six" | "sixth" => (6, false),
        "seven" | "seventh" => (7, false),
        "eight" | "eighth" => (8, false),
        "nine" | "ninth" => (9, false),
        "ten" | "tenth" => (10, false),
        "eleven" | "eleventh" => (11, false),
        "twelve" | "twelfth" => (12, false),
        "thirteen" | "thirteenth" => (13, false),
        "fourteen" | "fourteenth" => (14, false),
        "fifteen" | "fifteenth" => (15, false),
        "sixteen" | "sixteenth" => (16, false),
        "seventeen" | "seventeenth" => (17, false),
        "eighteen" | "eighteenth" => (18, false),
        "nineteen" | "nineteenth" => (19, false),
        "twenty" | "twentieth" => (20, false),
        "thirty" | "thirtieth" => (30, false),
        "forty" | "fortieth" => (40, false),
        "fifty" | "fiftieth" => (50, false),
        "sixty" | "sixtieth" => (60, false),
        "seventy" | "seventieth" => (70, false),
        "eighty" | "eightieth" => (80, false),
        "ninety" | "ninetieth" => (90, false),
        "hundred" => (100, true),
        "thousand" => (1000, true),
        _ => return None,
    };
    Some(NumberWord { value, scale })
}

/// Is this word a number word or a hyphen-compound of number words
/// ("fifty-eight", "twenty-first")? A hyphen-compound counts only if
/// EVERY hyphen-separated part is a number word — "state-of-the-art"
/// is not a figure.
fn is_number_word(w: &str) -> bool {
    w.split('-').all(|part| number_word_value(part).is_some())
}

/// The word-number class decoder (order deep-research-t6d,
/// pre-registered): replace spelled-out figures with their digit forms
/// so the digit-run extractor reads word forms and digit forms
/// identically ("twenty percent" → "20%", "ninety-five over twenty" →
/// "95/20"). Applied at the ONE choke point, inside `figure_tokens`;
/// every consumer (the witness, the fold identity, the figure
/// inventory, the question specifiers, the fact query) inherits it.
///
/// Structure, never remembered: number phrases evaluate by the
/// standard composition (tens+unit compounds, scale words, "and"
/// connectors); the unit words are guarded — "percent"/"per cent"
/// convert to "%" only after a figure, "point" to "." and "over" to
/// "/" only BETWEEN two figure phrases (the prepositional "grew over
/// twenty percent" is not a ratio); "times" is never mapped (the
/// digit run already reads "17.5 times" as "17.5"). Everything
/// unconverted is carried byte-for-byte; the output feeds only the
/// token extraction, never user-visible text.
fn normalize_word_figures(s: &str) -> String {
    // Word tokens: alphanumeric + apostrophe + hyphen runs with byte
    // spans (the subject-split convention).
    let mut words: Vec<(&str, usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if c.is_alphanumeric() || c == '\'' || c == '-' {
            start.get_or_insert(i);
        } else if let Some(st) = start.take() {
            words.push((&s[st..i], st, i));
        }
    }
    if let Some(st) = start.take() {
        words.push((&s[st..s.len()], st, s.len()));
    }

    let mut out = String::with_capacity(s.len());
    let mut prev_figure = false;
    let mut skip_sep = false;
    let mut cursor = 0usize;
    let mut i = 0usize;
    while i < words.len() {
        let (w, wst, wen) = words[i];
        let sep = &s[cursor..wst];
        let out_before_sep = out.len();
        if !skip_sep {
            out.push_str(sep);
        }
        skip_sep = false;
        cursor = wen;
        let lower = w.to_ascii_lowercase();

        // The two-word "per cent" unit.
        let per_cent = lower == "per"
            && words
                .get(i + 1)
                .is_some_and(|(n, _, _)| n.eq_ignore_ascii_case("cent"));

        // "%" absorbs the whitespace before the unit.
        if (lower == "percent" || per_cent) && prev_figure {
            out.truncate(out_before_sep);
            out.push('%');
            prev_figure = true;
            i += if per_cent { 2 } else { 1 };
            continue;
        }

        // Decimal / ratio markers: structural — only BETWEEN two
        // figure phrases ("fifty-eight point one", "95 over 20"), and
        // the whitespace on both sides is absorbed so the digit-run
        // extractor reads one run ("58.1%", "95/20").
        if (lower == "point" || lower == "over") && prev_figure {
            let next_is_figure = words.get(i + 1).is_some_and(|(n, _, _)| {
                n.chars().next().is_some_and(|c| c.is_ascii_digit()) || is_number_word(n)
            });
            if next_is_figure {
                out.truncate(out_before_sep);
                out.push(if lower == "point" { '.' } else { '/' });
                prev_figure = true;
                skip_sep = true;
                i += 1;
                continue;
            }
        }

        if is_number_word(w) {
            // The maximal phrase run: consecutive number words with
            // optional "and" connectors. The word's own leading
            // separator stays (word forms glue to their prose exactly
            // like digit forms — "affected twenty percent" reads as
            // "affected 20%", never "affected20%"); the separators
            // BETWEEN the absorbed words are consumed by advancing
            // the cursor past the run's last word ("one hundred"
            // never re-emits "hundred" as prose).
            let mut j = i;
            let mut parts: Vec<(u64, bool)> = Vec::new();
            while j < words.len() {
                let (nw, _, _) = words[j];
                if is_number_word(nw) {
                    for part in nw.split('-') {
                        let nwv = number_word_value(part).expect("is_number_word checked");
                        parts.push((nwv.value, nwv.scale));
                    }
                    j += 1;
                } else if nw.eq_ignore_ascii_case("and") && !parts.is_empty() {
                    j += 1; // connector — absorbed into the span
                } else {
                    break;
                }
            }
            let mut total = 0u64;
            let mut current = 0u64;
            for (v, scale) in &parts {
                if *scale {
                    current = if current == 0 { 1 } else { current } * v;
                    total += current;
                    current = 0;
                } else {
                    current += v;
                }
            }
            total += current;
            out.push_str(&total.to_string());
            prev_figure = true;
            cursor = words[j - 1].2;
            i = j;
            continue;
        }

        // A digit-carrying word is a figure run already — pushed
        // verbatim; everything else is prose.
        out.push_str(w);
        prev_figure = w.chars().any(|c| c.is_ascii_digit());
        i += 1;
    }
    if !skip_sep {
        out.push_str(&s[cursor..]);
    }
    out
}

/// C-class figure tokens for the fact query: every maximal run of
/// digits plus adjacent ratio/currency punctuation (`$ % . : / ,`),
/// trailing sentence separators trimmed. Word-form figures (order
/// deep-research-t6d — the word-number class) are normalized to their
/// digit forms first, so "twenty percent" yields the same token as
/// "20%" and the witness, fold identity, figure inventory, question
/// specifiers, and fact query read word and digit forms identically.
/// Deterministic, no model.
/// Cosine similarity — the ONE implementation the deep-research module
/// uses (§10.6). Returns -1.0 on a dimension mismatch or a
/// zero-magnitude vector so a degenerate embedding can never be
/// mistaken for a near match; callers treat a ZERO-LENGTH input as
/// unavailability, not as a low score (§18.3).
/// Strip C0 control bytes (keeping tab/newline/CR) before text reaches
/// the embedding tokenizer. Measured 2026-08-22: a PDF-extracted chunk
/// in the DRB-I estate carried an interior NUL, and the embed backend
/// refused the WHOLE batch — "Embed tokenization failed: input contains
/// an interior NUL at byte 785". One bad byte in one passage takes down
/// every passage batched with it, so the scrub belongs at the boundary,
/// not at the call sites that happen to remember.
pub(crate) fn scrub_control(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
        .collect()
}

pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return -1.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 {
        return -1.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

fn figure_tokens(s: &str) -> Vec<String> {
    figure_runs(&normalize_word_figures(s))
        .into_iter()
        .map(|r| r.token)
        .collect()
}

/// The gap-ledger fold's ONE identity decider (order deep-research-t6c,
/// pre-registered in adversarial/pre-registration.md): a gap claim's
/// fact identity = (figure tokens minus the QUESTION's own specifiers,
/// content-word subjects). Citation spans are stripped first — the
/// "[Source: ev-1]" digits are the evidence's ids, never the claim's
/// figures (the spurious "1" measured in the fold simulation). Two gap
/// texts are one fact when their figures intersect AND their subjects
/// intersect, or both are figureless with ≥2 shared subjects; the fold
/// rule lives beside the decider in audit.rs. Deterministic C-class,
/// no model. Recomputed per round — stateless, no ledger to corrupt
/// (§7.6: code-enforced, never model-judged).
pub(crate) fn gap_identity(
    claim: &str,
    question_specifiers: &[String],
) -> (Vec<String>, Vec<String>) {
    let stripped = strip_citation_spans(claim);
    let figures: Vec<String> = figure_tokens(&stripped)
        .into_iter()
        .filter(|t| !question_specifiers.iter().any(|s| s == t))
        .collect();
    let mut subjects: Vec<String> = stripped
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| {
            let wl = w.to_lowercase();
            wl.len() >= 3 && !is_query_stopword(&wl) && !wl.chars().any(|c| c.is_ascii_digit())
        })
        .map(|w| w.to_lowercase())
        .collect();
    subjects.sort();
    subjects.dedup();
    (figures, subjects)
}

/// The strip-3c anti-leak decider (order deep-research-t2c; word-form
/// closure, order deep-research-t6d): remove every figure `text`
/// carries whose token is NOT in `allowed` — the QUESTION's own figure
/// specifiers, never bank vocabulary — each replaced by a single space
/// (seams collapsed). Word-form figures are normalized to their digit
/// forms first, so "four" strips exactly like "4": a word-figure claim
/// never keeps a figure specifier the question did not allow, the
/// estate's spelled-out echo ("one hundred cities") never leaks into
/// the query, and the t1e fold-in still fires for a claim whose only
/// figure was a stripped word. Both gap-query shapes read it;
/// deterministic C-class, no model.
fn strip_disallowed_figures(text: &str, allowed: &[String]) -> String {
    let normalized = normalize_word_figures(text);
    let mut out = String::with_capacity(normalized.len());
    let mut cursor = 0;
    for run in figure_runs(&normalized) {
        if !allowed.iter().any(|a| a == &run.token) {
            out.push_str(&normalized[cursor..run.start]);
            out.push(' ');
        }
        cursor = run.end;
    }
    out.push_str(&normalized[cursor..]);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The fact query's minimal stopword set: English function words the
/// query does not need. Deterministic and small — the figures carry
/// the fact's identity, the content words carry the subject.
fn is_query_stopword(w: &str) -> bool {
    matches!(
        w,
        "the"
            | "and"
            | "was"
            | "were"
            | "for"
            | "with"
            | "from"
            | "that"
            | "this"
            | "its"
            | "are"
            | "had"
            | "has"
            | "but"
            | "not"
            | "into"
            | "over"
            | "after"
            | "between"
            | "than"
            | "their"
            | "which"
            | "what"
            | "when"
            | "where"
            | "who"
            | "how"
            | "did"
            | "why"
            | "been"
            | "being"
            | "will"
            | "would"
            | "about"
            | "more"
            | "most"
            | "some"
            | "such"
            | "also"
            | "then"
            | "there"
            | "these"
            | "those"
            | "upon"
            | "while"
            | "every"
            | "each"
            | "still"
            | "even"
            | "only"
            | "much"
            | "many"
    )
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The per-run controller: state machine + artifact writes.
struct Controller {
    config: RunConfig,
    port: Arc<dyn ResearchPort>,
    provider: Arc<dyn InferenceProvider>,
    abort: Arc<AtomicBool>,
    charter: Charter,
    charter_hash: String,
    tau: f64,
    containment: ContainmentConfig,
    decider: SpendDecider,
    lock: RunLock,
    state: State,
    /// The run's CURRENT question — the launch question, replaced by
    /// the reframed question at the GAP-4 re-frame (the survey, the
    /// drafts, the empty-window queries, and the report all read THIS,
    /// never a stale config field).
    question: String,
    /// t1d fix 2 (breadth): the acquisition frontier — the
    /// sub-question decomposition of the CURRENT question, computed ONCE
    /// per question text (a redirect/reframe re-computes; a re-plan of
    /// the same question never re-spends). The plan's
    /// `queries_preplanned` records it; the round-1 acquisition asks it
    /// (METHODOLOGY.md: "the sub-question list is the search frontier").
    /// The frontier is figure-hunted (t1e): every sub-question carries
    /// figure specifiers — the question's own, folded in when the draft
    /// left a sub-question specifier-less.
    frontier: Vec<String>,
    /// The question the frontier was computed for.
    frontier_question: Option<String>,
    /// t1e (figure-hunting): the CURRENT question's own figure
    /// specifiers — its digit runs + its measure-family words (the
    /// generic "what measures and numbers does this question imply?",
    /// shape from the question's own text, never bank-derived).
    /// Recorded on the plan artifact (glassbox) and folded into
    /// frontier sub-questions and gap queries that carry none.
    figure_specifiers: Vec<String>,
    /// GAP-4: the staged re-frame input (`<run_dir>/reframe-input.json`,
    /// read at start), None when no re-frame was staged.
    reframe_input: Option<ReframeInput>,
    /// GAP-4: the reframe record once written (reframe-1.json + the
    /// manifest's `reframe`). Set exactly once — the enumerated
    /// re-frame fires at most once per run (FR-1).
    reframe_record: Option<ReframeRecord>,
    /// STEER 2: the alignment record once written (alignment-1.json +
    /// the manifest's `alignment`). Set exactly once — a redirect
    /// fires at most once per run: the staged input is CONSUMED on
    /// the first redirect, so every later plan passes without
    /// re-prompting the operator.
    alignment_record: Option<AlignmentRecord>,
    /// STEER 2: how many plans have been written. plan.json is the
    /// initial plan; re-plan 1 is plan-2.json (golden preserving);
    /// re-plan N is plan-{N+1}.json. Every plan passes the alignment
    /// gate (Align) before any acquisition.
    re_plans: u32,
    /// GAP-3: the epistemic residue — every query the loop executed
    /// that returned no evidence, collected in acquire_round, rendered
    /// as report content and carried on the manifest.
    residue: Vec<ResidueRow>,
    /// T7b: the rounds that added no evidence — recorded at
    /// acquire_round (the moment the round window is final), carried on
    /// the checkpoint (resume-safe), surfaced on the verdict set and
    /// the report.
    empty_rounds: Vec<EmptyRound>,
    /// The windows accumulated so far (the estate window first).
    windows: Vec<EvidenceWindow>,
    /// The still-open gap claim texts — the strict-subset identity
    /// (stable claim texts re-audited against each new window).
    prior_gap_texts: Vec<String>,
    /// drb1-t2 (AIQ §1.4): the per-run source registry — every FETCHED
    /// source (window-admitted or content-refused), url + title + type
    /// + round + admitted. Written as `source-registry.json` at
    /// finish; the T3 writer's citation whitelist surface. Failed
    /// fetches stay on the manifest's failed list (they produced no
    /// source).
    source_registry: Vec<icd::SourceRegistryRow>,
    /// The current round's gaps AS THE GAP LIST RECORDED THEM — the
    /// compass's output. The acquisition leg queries
    /// `gap.actionable_query` (the question for empty-window gaps,
    /// icd-schemas.md §4), never a re-derivation from the claim text:
    /// re-deriving lost the empty-window substitution and sent the
    /// abstention text to the search engine (demo run dr-1786720584).
    prior_gaps: Vec<Gap>,
    /// F16: the estate precondition failed — the web leg is refused.
    web_refused: bool,
    window_capped: bool,
    search_calls: u32,
    rounds: Vec<RoundRow>,
    fetched_sources: Vec<FetchedSource>,
    failed_sources: Vec<FailedSource>,
    artifacts: Vec<String>,
    aborted_at_round: Option<u32>,
    /// Order deep-research-t3a: the round count a resume restored from
    /// (`None` on a fresh run). `drive()` skips the launch head
    /// (charter/plan/align — the checkpoint verified them) and the
    /// completed rounds when `Some`.
    resumed_after_round: Option<u32>,
    /// T6c REV-2 (pre-registered): the degenerate-draft guard's retry
    /// budget — at most ONE shape-constrained re-draft per flight
    /// segment. A resume re-flights only the un-written rounds and
    /// rebuilds without the checkpoint's flag (the checkpoint is
    /// ICD-frozen — no ICD change): the budget is per segment, and the
    /// guard's contract (no infinite loop per draft call) holds either
    /// way. The retry record = the draft-{round}-degenerate.json
    /// artifact + tracing; RoundRow is unchanged.
    draft_retried: bool,
}

impl Controller {
    async fn start(
        config: RunConfig,
        port: Arc<dyn ResearchPort>,
        provider: Arc<dyn InferenceProvider>,
        abort: Arc<AtomicBool>,
    ) -> Result<Controller, String> {
        // Initializing: the run dir + the run-scoped lock (F19).
        std::fs::create_dir_all(&config.run_dir)
            .map_err(|e| format!("run dir {}: {e}", config.run_dir.display()))?;
        let lock = RunLock::acquire(&config.run_dir, &config.run_id)?;
        // The controller is born with its REAL identity: charter →
        // hash → allowance → decider, derived from the config before
        // the struct exists. There is deliberately no placeholder
        // charter or decider — a placeholder decider once journaled an
        // empty ledger to the process CWD
        // (`PathBuf::new().join("budget-ledger.json")`), and the demo
        // measured the leak as a stray `budget-ledger.json` in the
        // repo root with an empty run_id (run dr-1786720828). Every
        // artifact this loop writes lives in the run dir, from birth.
        let charter = build_charter(&config);
        charter.validate()?;
        let charter_hash = hash_charter(&charter);
        let tau = audit::run_tau();
        let containment = ContainmentConfig {
            extraction_max_tokens: charter.charter.containment.extraction_max_tokens,
            specifics_max: charter.charter.containment.specifics_max,
        };
        let decider = SpendDecider::new(
            &config.run_id,
            &charter_hash,
            allowance_map(&config),
            &config.run_dir.join("budget-ledger.json"),
        )?;
        // GAP-4: the staged re-frame input — a typed file the launcher
        // writes into the run dir BEFORE the run (the CLI's --reframe).
        // Read at start; malformed refuses the run loudly; absent is
        // None (a run that never re-frames behaves exactly as before).
        let question = config.question.clone();
        let reframe_input = match std::fs::read_to_string(config.run_dir.join("reframe-input.json")) {
            Ok(json) => Some(
                serde_json::from_str::<ReframeInput>(&json).map_err(|e| {
                    format!(
                        "reframe-input.json in {} is malformed: {e} — a staged re-frame is a typed input, never silently ignored",
                        config.run_dir.display()
                    )
                })?,
            ),
            Err(_) => None,
        };
        let mut ctl = Controller {
            config,
            port,
            provider,
            abort,
            charter,
            charter_hash,
            tau,
            containment,
            decider,
            lock,
            state: State::Initializing,
            question,
            reframe_input,
            reframe_record: None,
            alignment_record: None,
            re_plans: 0,
            residue: Vec::new(),
            source_registry: Vec::new(),
            empty_rounds: Vec::new(),
            windows: Vec::new(),
            prior_gap_texts: Vec::new(),
            prior_gaps: Vec::new(),
            web_refused: false,
            window_capped: false,
            search_calls: 0,
            rounds: Vec::new(),
            fetched_sources: Vec::new(),
            failed_sources: Vec::new(),
            frontier: Vec::new(),
            frontier_question: None,
            figure_specifiers: Vec::new(),
            artifacts: Vec::new(),
            aborted_at_round: None,
            resumed_after_round: None,
            draft_retried: false,
        };
        Ok(ctl)
    }

    /// Order deep-research-t3a: restore an interrupted run. The verb
    /// rebuilds the port from the checkpoint's config + the launch
    /// sidecar and calls [`resume`]; this is the authoritative gate —
    /// every refusal is typed, and the ledger-continuity refusal (a
    /// resume that could double-spend budget) is the order's
    /// not-worth-continuing-if line.
    async fn resume_start(
        config: RunConfig,
        port: Arc<dyn ResearchPort>,
        provider: Arc<dyn InferenceProvider>,
        abort: Arc<AtomicBool>,
    ) -> Result<Controller, String> {
        if !config.run_dir.exists() {
            return Err(format!(
                "nothing to resume: run dir {} does not exist",
                config.run_dir.display()
            ));
        }
        // An already-closed run refuses: a completed or gracefully
        // aborted run is terminal (manifest present). A SIGKILLed run
        // has NO manifest — that is the resumable shape.
        let manifest_path = config.run_dir.join("manifest.json");
        if manifest_path.exists() {
            let state = std::fs::read_to_string(&manifest_path)
                .ok()
                .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
                .and_then(|v| {
                    v.get("terminal_state")
                        .and_then(|t| t.as_str().map(String::from))
                })
                .unwrap_or_else(|| "unknown".to_string());
            return Err(format!(
                "nothing to resume: the run already closed (terminal state {state}) — \
                 a completed or aborted run is not resumable"
            ));
        }
        let cp = read_checkpoint(&config.run_dir)?;
        // The charter hash recomputed from the checkpoint's config is
        // the config's tamper check (FR-3: the charter derives from
        // the config alone).
        let charter = build_charter(&cp.config);
        charter.validate()?;
        if hash_charter(&charter) != cp.charter_hash {
            return Err(
                "checkpoint tampered: the run's config does not hash to the checkpoint's \
                 charter_hash — a modified checkpoint is refused, never silently restored"
                    .to_string(),
            );
        }
        // The re-passed config must BE the checkpoint's config — field
        // by field, naming the first mismatch (the verb rebuilds the
        // config from the checkpoint, so this is the backstop against
        // a caller error or a tampered re-pass).
        if let Some(field) = config_mismatch(&config, &cp.config) {
            return Err(format!(
                "resume mismatch: {field} does not match the checkpoint — \
                 a run resumes with the state it was interrupted with, never a modified one"
            ));
        }
        // Ledger continuity: the decider is restored from the run's
        // journal — the entries replay spent/remaining (the journal is
        // the record), and a foreign or tampered ledger refuses. A
        // resume that can double-spend budget is worse than none.
        let decider = SpendDecider::restore(
            &cp.run_id,
            &cp.charter_hash,
            &allowance_map(&cp.config),
            &config.run_dir.join("budget-ledger.json"),
        )?;
        // The stale-lock-tolerant re-acquisition: a SIGKILLed run's
        // lock file remains (Drop never ran) — the flock discriminates
        // a live second run (refuses, F19) from a dead process's stale
        // file (acquirable — `--resume` is the visible act).
        let lock = RunLock::acquire(&config.run_dir, &config.run_id)?;
        let tau = audit::run_tau();
        let containment = ContainmentConfig {
            extraction_max_tokens: charter.charter.containment.extraction_max_tokens,
            specifics_max: charter.charter.containment.specifics_max,
        };
        // The staged re-frame input is re-read like start() (the loop
        // never consumes the file; the restored reframe_record
        // prevents a second fire — at most once per run, FR-1).
        let reframe_input = match std::fs::read_to_string(config.run_dir.join("reframe-input.json"))
        {
            Ok(json) => Some(serde_json::from_str::<ReframeInput>(&json).map_err(|e| {
                format!(
                    "reframe-input.json in {} is malformed: {e}",
                    config.run_dir.display()
                )
            })?),
            Err(_) => None,
        };
        let ctl = Controller {
            config,
            port,
            provider,
            abort,
            charter,
            charter_hash: cp.charter_hash.clone(),
            tau,
            containment,
            decider,
            lock,
            state: State::Rounding, // the checkpoint invariant: written at Rounding
            question: cp.question.clone(),
            reframe_input,
            reframe_record: cp.reframe_record.clone(),
            alignment_record: cp.alignment_record.clone(),
            re_plans: cp.re_plans,
            residue: cp.residue.clone(),
            empty_rounds: cp.empty_rounds.clone(),
            source_registry: cp.source_registry.clone(),
            windows: cp.windows.clone(),
            prior_gap_texts: cp.prior_gap_texts.clone(),
            prior_gaps: cp.prior_gaps.clone(),
            web_refused: cp.web_refused,
            window_capped: cp.window_capped,
            search_calls: cp.search_calls,
            rounds: cp.rounds.clone(),
            fetched_sources: cp.fetched_sources.clone(),
            failed_sources: cp.failed_sources.clone(),
            frontier: cp.frontier.clone(),
            frontier_question: cp.frontier_question.clone(),
            figure_specifiers: cp.figure_specifiers.clone(),
            artifacts: cp.artifacts.clone(),
            aborted_at_round: cp.aborted_at_round,
            resumed_after_round: Some(cp.written_after_round),
            draft_retried: false,
        };
        tracing::info!(
            target: "deep_research",
            run_id = %ctl.config.run_id,
            resumed_after_round = cp.written_after_round,
            "resume: state restored from checkpoint.json — continuing at round {}",
            cp.written_after_round + 1
        );
        Ok(ctl)
    }

    /// Order deep-research-t3a: persist the run's state after a
    /// completed round — the resume surface. Written only at the two
    /// round-push sites in drive(), where the machine is at `Rounding`
    /// (the invariant `read_checkpoint` checks). Atomic: tmp + rename,
    /// so a crash mid-write never leaves a half checkpoint (a torn
    /// checkpoint is worse than none — it would restore a lie).
    fn write_checkpoint(&mut self) -> Result<(), String> {
        let cp = RunCheckpoint {
            icd: "run_checkpoint".to_string(),
            version: icd::ICD_VERSION,
            run_id: self.config.run_id.clone(),
            charter_hash: self.charter_hash.clone(),
            written_after_round: self.rounds.len() as u32,
            config: self.config.clone(),
            question: self.question.clone(),
            frontier: self.frontier.clone(),
            frontier_question: self.frontier_question.clone(),
            figure_specifiers: self.figure_specifiers.clone(),
            reframe_record: self.reframe_record.clone(),
            alignment_record: self.alignment_record.clone(),
            re_plans: self.re_plans,
            residue: self.residue.clone(),
            empty_rounds: self.empty_rounds.clone(),
            source_registry: self.source_registry.clone(),
            windows: self.windows.clone(),
            prior_gap_texts: self.prior_gap_texts.clone(),
            prior_gaps: self.prior_gaps.clone(),
            web_refused: self.web_refused,
            window_capped: self.window_capped,
            search_calls: self.search_calls,
            rounds: self.rounds.clone(),
            fetched_sources: self.fetched_sources.clone(),
            failed_sources: self.failed_sources.clone(),
            artifacts: self.artifacts.clone(),
            aborted_at_round: self.aborted_at_round,
        };
        let json =
            serde_json::to_string_pretty(&cp).map_err(|e| format!("checkpoint serialize: {e}"))?;
        let path = self.config.run_dir.join("checkpoint.json");
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json).map_err(|e| format!("checkpoint write {tmp:?}: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("checkpoint commit {path:?}: {e}"))?;
        if !self.artifacts.iter().any(|a| a == "checkpoint.json") {
            self.artifacts.push("checkpoint.json".to_string());
        }
        Ok(())
    }

    /// The decider's journal path is ALWAYS the run dir's
    /// `budget-ledger.json` — see `start()`'s note: a placeholder
    /// decider once journaled to the process CWD, and the demo
    /// measured the leak (dr-1786720828).
    fn journal_path(&self) -> PathBuf {
        self.config.run_dir.join("budget-ledger.json")
    }

    /// STEER 2: write the current plan under the run's re-plan naming —
    /// plan.json (the launch plan), then plan-2.json (re-plan 1),
    /// plan-3.json (re-plan 2), ... — and count it. Every plan write
    /// drives the SAME PlanWritten row: each plan passes the alignment
    /// gate (Align) before any acquisition spend.
    fn write_plan_artifact(&mut self) -> Result<PathBuf, String> {
        let name = if self.re_plans == 0 {
            "plan.json".to_string()
        } else {
            format!("plan-{}.json", self.re_plans + 1)
        };
        self.re_plans += 1;
        self.write_artifact(&name, &self.build_plan())
    }

    /// STEER 2 (directive 3c5d8b53): the pre-acquisition alignment
    /// gate — called after EVERY PlanWritten (the launch plan and
    /// every re-plan). Shown the plan, the port decides: Proceed opens
    /// the rounds; a Redirect records the redirect (alignment-1.json +
    /// the manifest's `alignment`), re-enters Planning, and writes the
    /// re-plan through the SAME PlanWritten row — ONE enumerated
    /// re-plan transition (FR-1), the question-stewardship sibling of
    /// the mid-run re-frame. The staged input is consumed on the first
    /// redirect, so later re-plans pass without re-prompting.
    async fn align_plan(&mut self) -> Result<(), String> {
        loop {
            let decision = self
                .port
                .alignment_decision(&self.build_plan(), &self.config.run_dir)
                .await?;
            match decision {
                AlignmentDecision::Proceed => {
                    self.step(Event::AlignProceed)?; // → Rounding
                    return Ok(());
                }
                AlignmentDecision::Redirect { question, reason } => {
                    let original_question = self.question.clone();
                    let record = AlignmentRecord {
                        icd: "alignment".to_string(),
                        version: icd::ICD_VERSION,
                        run_id: self.config.run_id.clone(),
                        charter_hash: self.charter_hash.clone(),
                        round: 0,
                        original_question,
                        redirected_question: question.clone(),
                        reason,
                        trigger: "pre-acquisition alignment: the operator redirected the \
                                  question at the gate, before any acquisition spend"
                            .to_string(),
                    };
                    self.write_artifact("alignment-1.json", &record)?;
                    self.alignment_record = Some(record);
                    self.question = question;
                    self.step(Event::AlignRedirect)?; // → Planning
                    self.ensure_frontier().await?; // the redirected question's frontier
                    self.write_plan_artifact()?; // plan-2.json (re-plan 1)
                    self.step(Event::PlanWritten)?; // → Align — the re-plan passes the gate
                }
            }
        }
    }

    /// t1d fix 2 (breadth): compute the acquisition frontier for the
    /// CURRENT question — once per question text (a redirect/reframe
    /// changes the question and re-computes; a re-plan of the same
    /// question never re-spends model tokens). The plan's
    /// queries_preplanned carries it; the round-1 acquisition asks it.
    ///
    /// t1e (figure-hunting): the frontier then passes through the
    /// figure-hunt fold-in — every sub-question carries figure
    /// specifiers (the question's own digits + measure words folded in
    /// when the draft left a sub-question specifier-less), and the
    /// question's specifiers are recorded on the Controller (the plan
    /// artifact records them — glassbox). The step is generic SHAPE:
    /// the question's own text, never the bank's keys.
    async fn ensure_frontier(&mut self) -> Result<(), String> {
        if self.frontier_question.as_deref() == Some(self.question.as_str()) {
            return Ok(());
        }
        let subs = self.port.plan_subquestions(&self.question).await?;
        let subs = acquisition::figure_hunt_frontier(subs, &self.question);
        let specs = acquisition::figure_specifiers(&self.question);
        tracing::debug!(
            target: "deep_research",
            question = %self.question,
            sub_questions = subs.len(),
            figure_specifiers = specs.len(),
            "plan: acquisition frontier computed (figure-hunted)"
        );
        for q in &subs {
            tracing::debug!(
                target: "deep_research",
                question = %self.question,
                carries_specifier = acquisition::has_figure_specifier(q),
                sub_question = %q,
                "plan: frontier sub-question specifier presence"
            );
        }
        self.frontier = subs;
        self.figure_specifiers = specs;
        self.frontier_question = Some(self.question.clone());
        Ok(())
    }

    /// The plan ICD — the launch plan (plan.json) and the GAP-4
    /// re-plan (plan-2.json) are the same artifact shape; the reframe
    /// record names which question the re-plan serves. The acquisition
    /// frontier (t1d fix 2) is recorded as queries_preplanned; the
    /// source names who formed it — "gap-template" when no frontier was
    /// provided (pre-fix behavior), "plan-subquestions" when the plan
    /// carries the decomposed frontier.
    fn build_plan(&self) -> Plan {
        Plan {
            icd: "plan".to_string(),
            version: icd::ICD_VERSION,
            run_id: self.config.run_id.clone(),
            charter_hash: self.charter_hash.clone(),
            rounds_planned: self.config.max_rounds,
            estate_first: true,
            network_after_estate: true,
            acquisition: AcquisitionPlan {
                queries_preplanned: self.frontier.clone(),
                source: if self.frontier.is_empty() {
                    "gap-template".to_string()
                } else {
                    "plan-subquestions".to_string()
                },
                figure_specifiers: self.figure_specifiers.clone(),
            },
        }
    }

    /// The one transition entry point: any drive mistake (a pair not in
    /// the table) is an error, never a silent skip.
    fn step(&mut self, event: Event) -> Result<(), String> {
        let from = self.state;
        let to = State::transition(from, event).ok_or_else(|| {
            format!(
                "state machine: no transition for ({}, {:?})",
                from.as_str(),
                event
            )
        })?;
        self.state = to;
        Ok(())
    }

    fn write_artifact<T: serde::Serialize>(
        &mut self,
        name: &str,
        body: &T,
    ) -> Result<PathBuf, String> {
        let path = self.config.run_dir.join(name);
        let json = serde_json::to_string_pretty(body)
            .map_err(|e| format!("artifact {name} serialize: {e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("artifact {name} write: {e}"))?;
        self.artifacts.push(name.to_string());
        Ok(path)
    }

    /// The estate window (round 1's evidence): custody `personal`,
    /// content from the survey snippets. Estate-first: no network has
    /// been consulted.
    fn estate_window(&self, survey: &Survey) -> EvidenceWindow {
        let mut chunks = Vec::new();
        // ONE HANDLE PER HIT, NOT PER QUERY. This counter used to be the
        // QUERY index (`i + 1`), so every hit a query returned collapsed onto
        // the same `estate-N`. Measured 2026-08-27 on bed dr-1787807617:
        // `estate-1` covered EIGHT chunks and `estate-2` six — 12 of the
        // window's 62 chunks were structurally uncitable, because a writer has
        // no way to name the seventh hit of query 1 apart from the first.
        //
        // The waste was the smaller half of it. `number_citations` resolves a
        // handle with `.find(|c| c.id == id)`, so EVERY `[estate-1]` rendered
        // the FIRST hit's URL — a claim drawn from hit 5 shipped with hit 1's
        // source in the Sources list. Silent mis-attribution in the one
        // contract this pipeline exists to keep (§7.5: identity from essence,
        // never a counter that is not unique in the scope the id is used in —
        // and that scope is the merged WINDOW, not the query).
        let mut hit_index = 0usize;
        for q in survey.searched.iter() {
            for hit in &q.hits {
                hit_index += 1;
                let locator = hit
                    .url
                    .clone()
                    .unwrap_or_else(|| format!("estate:{}:{}", hit.corpus_id, hit.chunk_id));
                chunks.push(WindowChunk {
                    id: format!("estate-{hit_index}"),
                    locator: locator.clone(),
                    source_url: locator,
                    custody: "personal".to_string(),
                    provenance_class: "known".to_string(),
                    // The BODY over the snippet cut (t1h — the corpus
                    // leg's boundary: the term-centered 600-char
                    // snippet can miss the digits; the admitted
                    // chunk's full content drafts).
                    content: hit.content.clone().unwrap_or_else(|| hit.snippet.clone()),
                    ingested_into: None,
                    tags: Vec::new(),
                });
            }
        }
        let custody = fetch::derive_custody(&chunks);
        EvidenceWindow {
            icd: "evidence_window".to_string(),
            version: icd::ICD_VERSION,
            run_id: self.config.run_id.clone(),
            charter_hash: self.charter_hash.clone(),
            round: survey.round,
            chunks,
            fetch_failures: Vec::new(),
            dedup_refused: Vec::new(),
            content_refused: Vec::new(),
            derived_custody: custody,
        }
    }

    /// Merge the accumulated windows: dedup by source URL (first wins),
    /// capped at the charter's window cap. Capping is declared — the
    /// flag surfaces in the manifest's `truncation_declared`.
    /// Evidence handles that name more than one chunk in the same window.
    ///
    /// A window id is a WINDOW-LOCAL LABEL, and its whole correctness requirement
    /// is that it is unique in the scope it is resolved against. `number_citations`
    /// resolves with `.find(|c| c.id == id)` and the audit matches the same way, so
    /// a repeated id does not fail loudly — it silently binds every citation to the
    /// FIRST chunk carrying that id, shipping the wrong source URL for the rest.
    ///
    /// Both minting sites have produced collisions: `estate_window` numbered by
    /// QUERY rather than by hit (fixed), and `fetch_round` restarts its counter
    /// each round (`let mut index = 0usize`, fetch.rs:301), so round 2's `ev-1`
    /// collides with round 1's. Measured on bed dr-1787807617: 7 ids covering 24
    /// of 62 chunks.
    pub(crate) fn duplicate_window_ids(chunks: &[WindowChunk]) -> Vec<(String, usize)> {
        let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for c in chunks {
            *counts.entry(c.id.as_str()).or_insert(0) += 1;
        }
        counts
            .into_iter()
            .filter(|(_, n)| *n > 1)
            .map(|(id, n)| (id.to_string(), n))
            .collect()
    }

    fn merge_windows(&mut self) -> EvidenceWindow {
        let cap = self.config.evidence_window_max_chunks;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut chunks: Vec<WindowChunk> = Vec::new();
        let mut failures: Vec<FetchFailure> = Vec::new();
        let mut refused: Vec<String> = Vec::new();
        let mut capped = false;
        for w in &self.windows {
            for c in &w.chunks {
                if !seen.insert(c.source_url.clone()) {
                    continue;
                }
                if chunks.len() >= cap {
                    capped = true;
                    continue;
                }
                chunks.push(c.clone());
            }
            failures.extend(w.fetch_failures.iter().cloned());
            for u in &w.dedup_refused {
                if !refused.contains(u) {
                    refused.push(u.clone());
                }
            }
        }
        self.window_capped = capped;
        // GLASSBOX, because the failure is silent by construction: a repeated
        // handle binds every citation to the first chunk carrying it, so the
        // deliverable ships a wrong source URL and nothing errors. Warned
        // rather than refused — a flight that already acquired its evidence
        // should still land, NAMED, rather than be thrown away (§18.3).
        let dupes = Self::duplicate_window_ids(&chunks);
        if !dupes.is_empty() {
            let shadowed: usize = dupes.iter().map(|(_, n)| n - 1).sum();
            tracing::warn!(
                target: "deep_research",
                duplicate_ids = dupes.len(),
                shadowed_chunks = shadowed,
                worst = ?dupes.iter().max_by_key(|(_, n)| *n),
                "merge_windows: evidence handles are NOT unique in this window — \
                 every citation to a repeated id resolves to the FIRST chunk, so \
                 those claims will carry the wrong source URL"
            );
        }
        let custody = fetch::derive_custody(&chunks);
        EvidenceWindow {
            icd: "evidence_window".to_string(),
            version: icd::ICD_VERSION,
            run_id: self.config.run_id.clone(),
            charter_hash: self.charter_hash.clone(),
            round: self.windows.len() as u32,
            chunks,
            fetch_failures: failures,
            dedup_refused: refused,
            content_refused: Vec::new(),
            derived_custody: custody,
        }
    }

    fn audit_chunks(window: &EvidenceWindow) -> Vec<audit::AuditChunk> {
        window
            .chunks
            .iter()
            .map(|c| audit::AuditChunk {
                id: c.id.clone(),
                content: c.content.clone(),
                custody_known: c.provenance_class == "known",
                source_url: c.source_url.clone(),
            })
            .collect()
    }

    /// Audit one draft's claims plus the prior rounds' still-open gap
    /// claims (stable claim texts — the strict-subset identity) against
    /// the current window.
    async fn audit_pass(
        &self,
        draft: &Draft,
        window: &EvidenceWindow,
    ) -> Result<(Vec<ClaimAudit>, GapList), String> {
        let chunks = Self::audit_chunks(window);

        // Dedup FIRST, then judge. The `seen` set is the strict-subset
        // identity and it must be applied in source order — draft claims
        // before prior-gap claims — so the deduped list is built here and
        // the judging below reads it, rather than the two being
        // interleaved.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut to_judge: Vec<String> = Vec::new();
        for claim in split_claims(&draft.text) {
            let key = claim.trim().to_string();
            if seen.insert(key.clone()) {
                to_judge.push(key);
            }
        }
        for gap_text in &self.prior_gap_texts {
            if seen.insert(gap_text.clone()) {
                to_judge.push(gap_text.clone());
            }
        }

        // Judge with bounded overlap. `buffered` — NOT
        // `buffer_unordered` — because the audit order is the verdict
        // set's order and the gap list is built from it: an unordered
        // join would reshuffle the deliverable. `buffered` keeps the
        // output sequence identical to the serial loop's while letting
        // AUDIT_CONCURRENCY judgements be in flight at once, so this is
        // a pure latency change with byte-identical output.
        use futures::StreamExt as _;
        // One span index for the whole pass — the spans belong to the
        // chunks, not to the claims (see `audit::SpanCache`).
        let span_cache = audit::SpanCache::default();
        let audits: Vec<ClaimAudit> =
            futures::stream::iter(to_judge.into_iter().map(|key| {
                let provider = &self.provider;
                let chunks = &chunks;
                let containment = &self.containment;
                let posture = self.config.posture;
                let tau = self.tau;
                let spans = &span_cache;
                async move {
                    assess_claim(provider, &key, chunks, containment, posture, tau, spans).await
                }
            }))
            .buffered(AUDIT_CONCURRENCY)
            .collect()
            .await;
        let gap_list = audit::build_gap_list(
            &self.config.run_id,
            &self.charter_hash,
            draft.round,
            &audits,
            &self.prior_gap_texts,
            &self.question,
            &self.figure_specifiers,
            &|claim, corroboration| gap_query_for(claim, corroboration, &self.figure_specifiers),
        );
        Ok((audits, gap_list))
    }

    /// The abort landing: a truncated report from whatever exists —
    /// the last gap list rendered as open questions, truncation
    /// declared.
    async fn land_aborted(&mut self) -> Result<RunOutcome, String> {
        // The interrupted state is captured BEFORE the machine steps —
        // the report names where the run was when the abort landed.
        let interrupted = self.state;
        let mut report = format!(
            "# {}\n\n**ABORTED** — interrupted at state `{}` (round {}). The run closed with \
             truncation declared: claims not audited before the abort are not evaluated.\n\n",
            self.question,
            interrupted.as_str(),
            self.aborted_at_round.unwrap_or(0)
        );
        if !self.prior_gap_texts.is_empty() {
            report.push_str("## Open questions at abort\n\n");
            for gap in &self.prior_gap_texts {
                report.push_str(&format!("- **[could-not-judge]** {gap}\n"));
            }
            report.push('\n');
        }
        let report_path = self.config.run_dir.join("report.md");
        std::fs::write(&report_path, report).map_err(|e| format!("abort report write: {e}"))?;
        self.artifacts.push("report.md".to_string());
        // drb1-t2: an aborted run's registry still lands — the sources
        // fetched before the abort are real acquisitions.
        let registry = icd::SourceRegistry {
            icd: "source_registry".to_string(),
            version: icd::ICD_VERSION,
            run_id: self.config.run_id.clone(),
            charter_hash: self.charter_hash.clone(),
            sources: self.source_registry.clone(),
        };
        self.write_artifact("source-registry.json", &registry)?;
        // The abort landing goes through the machine: Abort (from
        // whatever state — the row exists for every state) → Aborted →
        // AbortRendered → DonePartial.
        self.step(Event::Abort)?;
        self.step(Event::AbortRendered)?;
        self.close_manifest(State::DonePartial, true)
    }

    /// Write the manifest + release the lock.
    fn close_manifest(
        &mut self,
        terminal: State,
        truncation_declared: bool,
    ) -> Result<RunOutcome, String> {
        let ledger = self.decider.snapshot();
        let mut not_covered = self.prior_gap_texts.clone();
        if self.web_refused {
            not_covered.push(
                "estate precondition failed (F16): the estate is not searchable; the web leg was refused"
                    .to_string(),
            );
        }
        let mut manifest = build_manifest(ManifestInput {
            run_id: self.config.run_id.clone(),
            charter_hash: self.charter_hash.clone(),
            terminal_state: terminal.as_str().to_string(),
            aborted_at_round: self.aborted_at_round,
            truncation_declared,
            rounds: self.rounds.clone(),
            sources: SourceLedger {
                fetched: self.fetched_sources.clone(),
                failed: self.failed_sources.clone(),
            },
            budget: BudgetTotals {
                spent: ledger.spent.clone(),
                remaining: ledger.remaining.clone(),
            },
            not_covered,
            reframe: self.reframe_record.clone(),
            alignment: self.alignment_record.clone(),
            residue: self.residue.clone(),
            consent: self.config.consent.clone(),
            lock: LockRecord {
                id: self.lock.id.clone(),
                acquired_at_unix: self.lock.acquired_at_unix,
                released_at_unix: None,
            },
        });
        manifest.lock.released_at_unix = Some(self.lock.release());
        self.write_artifact("manifest.json", &manifest)?;
        Ok(RunOutcome {
            terminal_state: terminal,
            report_path: self.config.run_dir.join("report.md"),
            manifest,
            artifacts: self.artifacts.clone(),
        })
    }

    /// The main drive — every state change through `State::transition`.
    async fn drive(&mut self) -> Result<RunOutcome, String> {
        // Initializing → Planning: the frozen charter (FR-3). A resumed
        // run skips the launch head entirely — the checkpoint verified
        // the charter, the plan artifacts are on disk, and the machine
        // was restored at Rounding (order deep-research-t3a).
        let resumed_after = self.resumed_after_round;
        if resumed_after.is_none() {
            if aborted(&self.abort) {
                self.aborted_at_round = Some(0);
                return self.land_aborted().await;
            }
            let charter = self.charter.clone();
            self.write_artifact("charter.json", &charter)?;
            self.step(Event::CharterWritten)?;

            // Planning: the plan ICD. STEER 2: the launch plan passes the
            // pre-acquisition alignment gate — shown the plan and its
            // acceptance shapes, the port confirms (or redirects the
            // question, re-planning through the SAME PlanWritten row)
            // BEFORE any acquisition spend.
            if aborted(&self.abort) {
                self.aborted_at_round = Some(0);
                return self.land_aborted().await;
            }
            self.ensure_frontier().await?; // t1d fix 2: the launch frontier
            self.write_plan_artifact()?; // plan.json
            self.step(Event::PlanWritten)?; // → Align
            self.align_plan().await?; // Proceed → Rounding; a redirect re-plans through the same row
        }

        // The round loop. A resumed run continues at
        // `written_after_round + 1` — the checkpoint restored the
        // controller state and the budget ledger, so the completed
        // rounds are never re-run (no re-spend of drafts or searches).
        let max_rounds = self.config.max_rounds;
        let first_round = resumed_after.unwrap_or(0) + 1;
        for round in first_round..=max_rounds {
            self.step(Event::RoundStarted)?; // → Surveying
            if aborted(&self.abort) {
                self.aborted_at_round = Some(round);
                return self.land_aborted().await;
            }

            // Surveying: the estate, round 1 only (estate-first, F16).
            if round == 1 {
                let listing = self
                    .port
                    .estate_listing(&self.config.estate_corpus_ids)
                    .await?;
                let precondition = listing.precondition("estate corpora listed at round 1");
                let survey = estate::survey_estate(
                    self.port.as_ref(),
                    &self.config.run_id,
                    &self.charter_hash,
                    round,
                    &self.question,
                    &listing,
                    &self.config.estate_corpus_ids,
                    8,
                )
                .await?;
                self.web_refused = !precondition.estate_searchable;
                self.write_artifact(&format!("survey-{round}.json"), &survey)?;
                self.windows.push(self.estate_window(&survey));
            }
            self.step(Event::SurveyComplete)?; // → Auditing

            // Auditing: draft (estate round-1 / merged later), then the
            // composed gate over its claims + the prior gaps.
            if aborted(&self.abort) {
                self.aborted_at_round = Some(round);
                return self.land_aborted().await;
            }
            let window = self.merge_windows();
            let mut draft = synthesize::draft_round(
                self.port.as_ref(),
                &self.config.run_id,
                &self.charter_hash,
                round,
                &self.question,
                &window,
                &self.prior_gap_texts,
                false,
            )
            .await?;
            // T6c REV-2 (pre-registered): the degenerate-draft guard.
            // The seed-07 corruption class (inner monologue, date
            // spirals, 12.8 "**" per 1k chars) flooded the gap ledger
            // 2 -> 38 in rev 1. ONE shape-constrained re-draft per
            // flight segment; the degenerate original is preserved as
            // draft-{round}-degenerate.json (never silently
            // substituted — §18.3), the retry is glassbox.
            if synthesize::draft_is_degenerate(&draft.text) && !self.draft_retried {
                tracing::warn!(
                    target: "deep_research",
                    run_id = %self.config.run_id,
                    round,
                    chars = draft.text.len(),
                    "degenerate draft detected; one shape-constrained re-draft"
                );
                self.write_artifact(&format!("draft-{round}-degenerate.json"), &draft)?;
                self.draft_retried = true;
                draft = synthesize::draft_round(
                    self.port.as_ref(),
                    &self.config.run_id,
                    &self.charter_hash,
                    round,
                    &self.question,
                    &window,
                    &self.prior_gap_texts,
                    true,
                )
                .await?;
            }
            if round == 1 {
                // The survey's estate_answer is the round-1 draft (the
                // estate alone answered).
                let path = self.config.run_dir.join("survey-1.json");
                let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                let mut survey: Survey =
                    serde_json::from_str(&json).map_err(|e| format!("survey-1 re-read: {e}"))?;
                survey.estate_answer = draft.text.clone();
                self.write_artifact("survey-1.json", &survey)?;
            }
            self.write_artifact(&format!("draft-{round}.json"), &draft)?;

            let (audits, gap_list) = self.audit_pass(&draft, &window).await?;
            let gaps_before = self.prior_gap_texts.len();
            self.write_artifact(&format!("gap-list-{round}.json"), &gap_list)?;
            self.prior_gap_texts = gap_list.gaps.iter().map(|g| g.text.clone()).collect();
            self.prior_gaps = gap_list.gaps.clone();
            let gaps_after = self.prior_gap_texts.len();

            if self.prior_gap_texts.is_empty() {
                // NoNewGaps: the loop's questions are answered — the
                // last draft is the final one.
                return self
                    .finish(&draft, round, gaps_before, gaps_after, false)
                    .await;
            }

            // GAP-4: the structural-surprise re-frame (FR-1). A staged
            // reframe-input.json fires the ONE enumerated re-plan
            // transition when the loop is spinning: round >= 2, the gap
            // list unchanged and still open, and the last acquire round
            // fetched nothing. The reframe record + the re-plan
            // (plan-2.json, through the SAME PlanWritten row) are
            // written here; the reframed question drives every later
            // draft, gap query, and the report. At most once per run. A
            // run without a staged input cannot fire the trigger — the
            // loop behaves exactly as before.
            if let Some(reframe) = self.reframe_input.clone() {
                let spinning = round >= 2
                    && gaps_after > 0
                    && gaps_after == gaps_before
                    && self.rounds.last().map(|r| r.fetched == 0).unwrap_or(false);
                if spinning && self.reframe_record.is_none() {
                    let original_question = self.question.clone();
                    self.step(Event::ReframeRequested)?; // → Reframing
                    let record = ReframeRecord {
                        icd: "reframe".to_string(),
                        version: icd::ICD_VERSION,
                        run_id: self.config.run_id.clone(),
                        charter_hash: self.charter_hash.clone(),
                        round,
                        original_question,
                        reframed_question: reframe.question.clone(),
                        reason: reframe.reason.clone(),
                        trigger: "structural surprise: the last acquire round fetched nothing \
                                  and the gap list is unchanged (spinning)"
                            .to_string(),
                    };
                    self.write_artifact("reframe-1.json", &record)?;
                    self.reframe_record = Some(record);
                    self.question = reframe.question;
                    // The reframe round is a real round in the ledger —
                    // it searched nothing and fetched nothing.
                    self.rounds.push(RoundRow {
                        round,
                        gaps_before,
                        gaps_after,
                        fetched: 0,
                        search_calls: 0,
                    });
                    self.step(Event::ReframeWritten)?; // → Planning
                    self.ensure_frontier().await?; // the reframed question's frontier
                    self.write_plan_artifact()?; // plan-2.json (re-plan 1)
                    self.step(Event::PlanWritten)?; // → Align — the re-plan passes the alignment gate
                    self.align_plan().await?; // Proceed → Rounding; a second redirect re-plans again
                                              // T3a: the reframe round is a real round in the
                                              // ledger, so the resume checkpoint lands here too —
                                              // a crash mid-reframe re-fires the branch from
                                              // reframe-input.json (idempotent; it searched and
                                              // fetched nothing).
                    self.write_checkpoint()?;
                    continue; // the reframed question drives the next round
                }
            }

            let continue_to_web = !self.web_refused
                && self.decider.remaining(
                    FAMILY_WEB_SEARCH,
                    &source_budget_key(self.config.search_source, &self.config.web_backend),
                ) > 0
                && self.decider.remaining(FAMILY_WEB_FETCH, KEY_FETCH_PAGES) > 0;
            if !continue_to_web {
                // drb1-r1 Item 1 (F4 stop rule): If gaps are growing AND round budget
                // remains, consume one more round instead of stopping early.
                let gaps_growing = gaps_after > gaps_before;
                let round_budget_remains = round < max_rounds;

                if gaps_growing && round_budget_remains {
                    tracing::debug!(
                        target: "deep_research",
                        run_id = %self.config.run_id,
                        round,
                        gaps_before,
                        gaps_after,
                        max_rounds,
                        "drb1-r1 F4: gaps growing with round budget remaining — continuing to next round"
                    );
                    // drb1-r2c: the continue must SEQUENCE the machine —
                    // the round audited (the machine is at Auditing) and
                    // the acquisition leg cannot run here, so the
                    // enumerated skip returns it to Rounding for the next
                    // RoundStarted. The bare `continue` of drb1-r1 left
                    // the machine at Auditing and the next round errored
                    // ("no transition for (auditing, RoundStarted)") —
                    // watched red in gym_deck::
                    // unsearchable_estate_refuses_the_web_leg.
                    self.step(Event::AcquisitionSkipped)?; // → Rounding
                                                           // The consumed round is a real round in the ledger —
                                                           // it drafted and audited, searched nothing, fetched
                                                           // nothing (the reframe branch's row shape).
                    self.rounds.push(RoundRow {
                        round,
                        gaps_before,
                        gaps_after,
                        fetched: 0,
                        search_calls: 0,
                    });
                    // T3a: the resume checkpoint lands at EVERY
                    // round-push site — the machine is at Rounding,
                    // written_after_round == rounds.len(). A SIGKILL from
                    // here forward resumes at round + 1.
                    self.write_checkpoint()?;
                    continue;
                }

                // BudgetExhausted (or the F16 refusal): done-partial.
                return self
                    .finish(&draft, round, gaps_before, gaps_after, true)
                    .await;
            }
            self.step(Event::GapCycle)?; // → Querying
            let search_before = self.search_calls;
            if let Err(e) = self.acquire_round(round).await {
                // An abort mid-leg lands the truncated report — never an
                // Err that abandons the run dir without a manifest.
                if aborted(&self.abort) {
                    self.aborted_at_round = Some(round);
                    return self.land_aborted().await;
                }
                return Err(e);
            }
            let fetched = self.windows.last().map(|w| w.chunks.len()).unwrap_or(0);
            self.rounds.push(RoundRow {
                round,
                gaps_before,
                gaps_after,
                fetched,
                search_calls: self.search_calls - search_before,
            });
            // T3a: the resume checkpoint lands at EVERY round-push site
            // (here + the reframe branch + the F4 acquisition-free
            // continue) — the machine is at Rounding,
            // written_after_round == rounds.len(), and the round's
            // artifacts (evidence-window-N.json etc.) are on disk. A
            // SIGKILL from here forward resumes at round + 1.
            self.write_checkpoint()?;
            // acquire_round returns with the loop state at Rounding.
        }

        // max_rounds reached with gaps still open: done-partial.
        let draft = self.last_draft();
        self.finish(
            &draft,
            max_rounds,
            self.prior_gap_texts.len(),
            self.prior_gap_texts.len(),
            true,
        )
        .await
    }

    /// The R4+R5+R6 leg for one round: form queries, search through
    /// the decider, triage, fetch, enrich. Returns with the machine at
    /// Rounding (the loop's next-round state).
    async fn acquire_round(&mut self, round: u32) -> Result<EvidenceWindow, String> {
        if aborted(&self.abort) {
            self.aborted_at_round = Some(round);
            return Err("aborted".to_string());
        }
        // The compass's output drives R4: the gaps AS THE GAP LIST
        // RECORDED them, queries included (the empty-window gap's
        // query is the question — audit::build_gap_list). Re-deriving
        // the query from the claim text here would send the
        // abstention text to the search engine (the defect the demo
        // measured in dr-1786720584).
        let gaps = self.prior_gaps.clone();
        // t1d fix 2 (breadth): the acquisition frontier joins the
        // round-1 queries only — the initial acquisition asks the whole
        // frontier (the plan's queries_preplanned); rounds 2+ are
        // gap-targeted follow-ups.
        let frontier: &[String] = if round == 1 { &self.frontier } else { &[] };
        // AIQ planner rule 3, ported (acquisition tune 2026-08-24): the
        // gaps are reformulated into SELF-CONTAINED search queries before
        // they are minted. Round 1's queries were already model-formed
        // (`plan_subquestions`) and are the ones that retrieve well; the
        // gap rounds were a string template over draft prose and are the
        // ones that collapse. This gives them the same surface.
        //
        // A refusal is NOT a failure of the round — it falls back to the
        // deterministic template, which is exactly the prior behaviour,
        // and the fallback is recorded on every query's `formed_by`
        // ("gap-template" vs "gap-model") rather than being silent.
        let gap_texts: Vec<String> = gaps.iter().map(|g| g.text.clone()).collect();
        let reformed: Option<Vec<String>> = if gap_texts.is_empty() {
            None
        } else {
            match self.port.gap_queries(&self.question, &gap_texts).await {
                Ok(q) => Some(q),
                Err(e) => {
                    tracing::warn!(
                        target: "deep_research",
                        run_id = %self.config.run_id,
                        round,
                        gaps = gap_texts.len(),
                        error = %e,
                        "acquisition: gap-query reformulation refused —                          falling back to the deterministic template                          (recorded as formed_by=gap-template)"
                    );
                    None
                }
            }
        };
        let mut fetch_list = acquisition::form_queries(
            &self.config.run_id,
            &self.charter_hash,
            round,
            &gaps,
            frontier,
            reformed.as_deref(),
        );

        // R4 search through the ONE decider (web-search family). The
        // SOURCE is a closed set decided once at launch (t1g rung 2):
        // Mock — the deck's term-ranked surface; Corpus — the estate's
        // corpus-search surface; Web (rung 3, order deep-research-t2a)
        // — the real web leg through the port, routed identically to
        // Mock (the port's web_search carries the run's consent grant
        // to the egress boundary). Same ledger, same allowance — the
        // protocol is unchanged, only the source routes differently. A
        // refused query spends nothing and is journaled in the budget
        // ledger — the ledger is the record.
        let source_key = source_budget_key(self.config.search_source, &self.config.web_backend);
        // drb1-r2b (order drb1-r2b, campaign drb1-race): the
        // round-allowance split — the gap round keeps its ammunition.
        // Round 1 asks its own gaps plus the whole frontier and can
        // form more queries than the search allowance holds; without a
        // split it exhausts the meter and the between-rounds gate then
        // refuses the gap round entry (the seed-02 shape: round-1
        // search_calls 12/12, round-2 search_calls 0, gaps flat). The
        // round's query list is truncated to its fair share BEFORE the
        // search loop, so the fetch list records exactly the queries
        // the round executed (the residue's "searched but absent" stays
        // exact) and the decider still journals only real asks —
        // spent/remaining are the truth, never a mask. The FINAL round
        // (rounds_left == 1) keeps the whole remaining allowance: the
        // R1 consume-the-remaining-budget stop rule still ends the run
        // with everything spent where it should.
        let rounds_left = self
            .config
            .max_rounds
            .saturating_sub(round)
            .saturating_add(1);
        let search_cap = budget::round_allowance_cap(
            self.decider.remaining(FAMILY_WEB_SEARCH, &source_key),
            rounds_left,
        ) as usize;
        if fetch_list.queries.len() > search_cap {
            tracing::debug!(
                target: "deep_research",
                run_id = %self.config.run_id,
                round,
                formed = fetch_list.queries.len(),
                cap = search_cap,
                rounds_left,
                "drb1-r2b: round-allowance split holds queries back for the later rounds"
            );
            fetch_list.queries.truncate(search_cap);
        }
        // R4 the search walk itself — waves through the ONE decider
        // (`search::search_round`, the sibling of the R6 fetch walk).
        let leg = search::SearchPolicy {
            source: self.config.search_source,
            source_key: source_key.clone(),
            web_backend: self.config.web_backend.clone(),
            estate_corpus_ids: self.config.estate_corpus_ids.clone(),
        };
        let searched = search::search_round(
            self.port.as_ref(),
            &mut self.decider,
            round,
            &fetch_list.queries,
            now_unix(),
            &leg,
        )
        .await?;
        self.search_calls += searched.calls;
        self.residue.extend(searched.residue);
        let all_hits = searched.hits;

        // R5 triage: ranker never excluder (code-set K + ε-quota; the
        // skip ledger is the F25 record).
        let triaged = acquisition::triage_hits(
            &self.config.run_id,
            &self.charter_hash,
            round,
            all_hits,
            self.config.code_set_k,
            self.config.eps_quota,
        );
        acquisition::attach_hits(&mut fetch_list, triaged.ranked.clone());
        fetch_list.triage = triaged.outcome.clone();
        self.write_artifact(&format!("fetch-list-{round}.json"), &fetch_list)?;
        self.write_artifact(&format!("skip-ledger-{round}.json"), &triaged.skip_ledger)?;
        self.step(Event::QueriesFormed)?; // → Triage
        self.step(Event::TriageComplete)?; // → Fetching
                                           // NOTE (drb1-t2): the skip ledger written above is the TRIAGE
                                           // record (every row not in the K ∪ ε tiers, plus demoted
                                           // noise) — written before the fetch leg runs. Under permissive
                                           // triage the walk may FETCH below-tier rows within budget, so
                                           // the ledger is REWRITTEN after the fetch with the fetched
                                           // urls removed (the ledger is the not-fetched record; a row
                                           // the loop fetched must not carry a skip row). The rewrite
                                           // lands beside the evidence window below.

        // R6 fetch through the decider; custody stamped by code;
        // failures recorded absent per-source (F17). Dedup: the URLs
        // fetched by prior rounds are refused (t1d fix 1 — a round-2
        // fetch of an already-fetched URL is refused, no re-spend).
        //
        // drb1-t2 (fetch-then-judge): the walk queue is the round's
        // FULL non-noise candidate list (permissive triage — noise
        // demoted, budget deciding the walk depth), bounded by the
        // round's fetch share (the r2b split over the fetch family —
        // the gap rounds keep their ammunition; the decider's global
        // allowance still binds underneath), with same-query fallback
        // promotion past failures and content admission post-fetch.
        let rounds_left = self
            .config
            .max_rounds
            .saturating_sub(round)
            .saturating_add(1);
        let round_fetch_cap = budget::round_allowance_cap(
            self.decider.remaining(FAMILY_WEB_FETCH, KEY_FETCH_PAGES),
            rounds_left,
        ) as usize;
        let fetch_policy = fetch::FetchPolicy {
            round_fetch_cap,
            content_coverage_floor: self.config.content_coverage_floor,
            prose_line_floor: self.config.prose_line_floor,
        };
        let already_fetched: Vec<String> =
            self.fetched_sources.iter().map(|s| s.url.clone()).collect();
        let out = fetch_round(
            self.port.as_ref(),
            &mut self.decider,
            &self.config.run_id,
            &self.charter_hash,
            round,
            &fetch_list,
            &triaged.candidates,
            &already_fetched,
            now_unix(),
            &fetch_policy,
        )
        .await?;
        let mut window = out.window;
        self.source_registry.extend(out.registry_rows);
        // drb1-t2: the post-fetch ledger rewrite — a below-tier row the
        // walk FETCHED (within budget) must not carry a skip row; the
        // window's chunks and content_refused are its fetch record.
        // The pre-fetch ledger (written at triage time) named it
        // below-cut; this rewrite is the honest final record.
        {
            let mut skip_ledger = triaged.skip_ledger;
            let fetched_now: std::collections::HashSet<String> = window
                .chunks
                .iter()
                .map(|c| c.source_url.clone())
                .chain(window.content_refused.iter().map(|r| r.url.clone()))
                .collect();
            let before = skip_ledger.entries.len();
            skip_ledger
                .entries
                .retain(|e| !fetched_now.contains(&e.url));
            if before != skip_ledger.entries.len() {
                tracing::debug!(
                    target: "deep_research",
                    run_id = %self.config.run_id,
                    round,
                    removed = before - skip_ledger.entries.len(),
                    "drb1-t2: skip ledger rewritten post-fetch (below-tier rows the walk fetched)"
                );
                self.write_artifact(&format!("skip-ledger-{round}.json"), &skip_ledger)?;
            }
        }
        self.step(Event::FetchComplete)?; // → Enriching
        for f in &window.fetch_failures {
            self.failed_sources.push(FailedSource {
                url: f.url.clone(),
                error: f.error.clone(),
            });
        }

        // R7 enrich: derived tags + the custody join.
        //
        // The join reads `candidates`, NOT `ranked` (acquisition tune,
        // 2026-08-24). `candidates` IS the walk queue the fetch leg was
        // handed, so every chunk in the window came from a row in it;
        // `ranked` is only the K ∪ ε tier, and drb1-t2 decoupled the two
        // when it made the walk permissive. Joining on the tier dropped
        // the title of every chunk fetched past rank K + ε, and
        // `derive_tags` answers an empty title with its no-signal
        // fallback — the chunk's sole tag becomes its own URL. Latent at
        // the DRB-I settings (round cap 4 < the 6-row tier) and live the
        // moment the fetch allowance rises. Red:
        // `below_tier_chunks_keep_their_source_title_in_enrichment`.
        let titles: Vec<(String, String)> = window
            .chunks
            .iter()
            .map(|c| {
                let t = triaged
                    .candidates
                    .iter()
                    .find(|h| h.url == c.source_url)
                    .map(|h| h.title.clone())
                    .unwrap_or_default();
                (c.id.clone(), t)
            })
            .collect();
        enrich::enrich_window(&mut window, &titles);
        for c in &window.chunks {
            self.fetched_sources.push(FetchedSource {
                url: c.source_url.clone(),
                custody: c.custody.clone(),
                ingested_into: c.ingested_into.clone(),
            });
        }
        // T7b (order deep-research-t7b, pre-registered): the round
        // window is final NOW — record the round-level empty state
        // before it ships (glassbox: a no-evidence round is visible in
        // the run log and on the verdict surface, never silently).
        if let Some(reason) = audit::empty_round_reason(&window) {
            self.empty_rounds.push(EmptyRound { round, reason });
            let reason_s = reason.as_str();
            tracing::warn!(
                target: "deep_research",
                run_id = %self.config.run_id,
                round,
                reason = reason_s,
                "round {round} added no evidence: {reason_s}"
            );
        }
        self.write_artifact(&format!("evidence-window-{round}.json"), &window)?;
        self.windows.push(window.clone());
        self.step(Event::EnrichComplete)?; // → Rounding
        Ok(window)
    }

    /// The final pass: the verdict set, report, manifest — from the
    /// last draft and the merged window. Drives the terminal chain
    /// through the state machine.
    async fn finish(
        &mut self,
        draft: &Draft,
        round: u32,
        gaps_before: usize,
        gaps_after: usize,
        truncated: bool,
    ) -> Result<RunOutcome, String> {
        if aborted(&self.abort) {
            self.aborted_at_round = Some(round);
            return self.land_aborted().await;
        }
        let window = self.merge_windows();

        // drb1-t5: the COMPOSED deliverable — one section per planned
        // sub-question plus a closing synthesis, written over the whole
        // merged window (AIQ §1.6/§6.3). Default OFF; the flight and the
        // ledger row name it. When it composes, the gate runs over the
        // composed text, so the audited artefact and the delivered
        // artefact are the same document — never two.
        // drb1-r4: one researcher worker per sub-question, reading the
        // WHOLE merged window, returning findings whose evidence ids the
        // parser resolved against that window. Gathered only when the
        // composed report will actually consume them.
        let notes: Vec<icd::ResearchNote> = if composed_report_enabled() && research_notes_enabled()
        {
            let n = notes::gather(&*self.port, &self.frontier, &window).await;
            let admitted: usize = n.iter().map(|x| x.findings.len()).sum();
            let refused: usize = n.iter().map(|x| x.refused.len()).sum();
            tracing::info!(
                target: "deep_research",
                run_id = %self.config.run_id,
                sub_questions = n.len(),
                findings = admitted,
                refused,
                window_chunks = window.chunks.len(),
                "research workers distilled the window"
            );
            self.write_artifact(
                &format!("research-notes-{round}.json"),
                &icd::ResearchNotes {
                    icd: "research_notes".to_string(),
                    version: icd::ICD_VERSION,
                    run_id: self.config.run_id.clone(),
                    charter_hash: self.charter_hash.clone(),
                    round,
                    notes: n.clone(),
                },
            )?;
            n
        } else {
            Vec::new()
        };

        // drb1-r5: WHICH list becomes the deliverable's sections. The
        // frontier is tuned for retrieval; an outline is planned for the
        // reader. A refused or unusable outline falls back to the frontier
        // and says so — never silently (§18.3).
        let sections: Vec<String> = if composed_report_enabled() && report_outline_enabled() {
            match synthesize::plan_outline(&*self.port, &self.question, &window).await {
                Ok(o) => {
                    tracing::info!(
                        target: "deep_research",
                        run_id = %self.config.run_id,
                        sections = o.len(),
                        frontier = self.frontier.len(),
                        "report outline planned — sections come from the outline, not the \
                         search frontier"
                    );
                    o
                }
                Err(e) => {
                    tracing::warn!(
                        target: "deep_research",
                        run_id = %self.config.run_id,
                        error = %e,
                        "outline unavailable — sections fall back to the search frontier \
                         (named, never silent)"
                    );
                    self.frontier.clone()
                }
            }
        } else {
            self.frontier.clone()
        };

        // FREEZE THE WRITER'S INPUTS. Written at the boundary, before the
        // call, so `tests/compose_replay.rs` can re-run the production
        // `compose_report` against identical evidence in ~12 minutes instead
        // of paying the ~96-minute flight that surrounds it. Sibling to
        // `arms/bed-binder/bed.json`, which does the same for the audit.
        // Unconditional: a run that cannot be replayed is a measurement we
        // can only repeat by rebuying its acquisition and its audit.
        if composed_report_enabled() {
            let (want, cap) = synthesize::section_evidence_budget();
            let compose_input = icd::ComposeInput {
                icd: "compose_input".to_string(),
                version: icd::ICD_VERSION,
                run_id: self.config.run_id.clone(),
                charter_hash: self.charter_hash.clone(),
                question: self.question.clone(),
                window: window.clone(),
                sections: sections.clone(),
                notes: notes.clone(),
                section_passages: want,
                per_source_cap: cap,
            };
            self.write_artifact("compose-input.json", &compose_input)?;
        }

        let composed = if composed_report_enabled() {
            match synthesize::compose_report(
                &*self.port,
                &self.question,
                &window,
                &sections,
                &notes,
            )
            .await
            {
                Ok(md) => Some(md),
                Err(e) => {
                    tracing::warn!(
                        target: "deep_research",
                        error = %e,
                        "composed report unavailable — falling back to the claim-ledger render (named, never silent)"
                    );
                    None
                }
            }
        } else {
            None
        };

        let audit_target;
        let audit_ref = match &composed {
            Some(md) => {
                audit_target = Draft {
                    text: md.clone(),
                    ..draft.clone()
                };
                &audit_target
            }
            None => draft,
        };
        let (audits, _) = self.audit_pass(audit_ref, &window).await?;
        let claims = final_claims(&audits, &window);
        let verdict_set = icd::VerdictSet {
            icd: "verdict_set".to_string(),
            version: icd::ICD_VERSION,
            run_id: self.config.run_id.clone(),
            charter_hash: self.charter_hash.clone(),
            claims: claims.clone(),
            empty_rounds: self.empty_rounds.clone(),
        };
        self.write_artifact("verdict-set.json", &verdict_set)?;
        // drb1-t2 (AIQ §1.4): the per-run source registry — every
        // fetched source (window-admitted or content-refused), the T3
        // writer's citation whitelist surface. Written at finish and
        // on the checkpoint path (the registry rides the checkpoint,
        // so a resumed run appends rather than truncates).
        let registry = icd::SourceRegistry {
            icd: "source_registry".to_string(),
            version: icd::ICD_VERSION,
            run_id: self.config.run_id.clone(),
            charter_hash: self.charter_hash.clone(),
            sources: self.source_registry.clone(),
        };
        self.write_artifact("source-registry.json", &registry)?;
        self.prior_gap_texts = not_covered(&claims);
        let report = match &composed {
            Some(md) => {
                let (numbered, _) = synthesize::number_citations(md, &window);
                render::annotate_composed(&numbered, &claims)
            }
            None => render_report(
                &self.question,
                &claims,
                &self.config.run_id,
                self.reframe_record.as_ref(),
                self.alignment_record.as_ref(),
                &self.residue,
                &self.empty_rounds,
            ),
        };
        let report_path = self.config.run_dir.join("report.md");
        std::fs::write(&report_path, report).map_err(|e| format!("report write: {e}"))?;
        self.artifacts.push("report.md".to_string());

        // The terminal row is the round this finish closes. A finish
        // called mid-round (NoNewGaps / BudgetExhausted) pushes; the
        // TAIL call (max_rounds reached, gaps still open) lands after
        // the final round's loop row — pushing again would duplicate
        // the round (measured in the t3a resume tests: a high-budget
        // 3-round flight recorded rounds [1,2,3,3]). One row per
        // round, always.
        if self.rounds.last().map(|r| r.round) != Some(round) {
            self.rounds.push(RoundRow {
                round,
                gaps_before,
                gaps_after,
                fetched: window.chunks.len(),
                search_calls: 0,
            });
        }

        // The terminal chain: Auditing → Synthesizing → Rendering → Done.
        let event = if self.prior_gap_texts.is_empty() && !truncated {
            Event::NoNewGaps
        } else {
            Event::BudgetExhausted
        };
        self.step(event)?; // → Synthesizing
        self.step(Event::DraftReady)?; // → Rendering
        let terminal = if self.prior_gap_texts.is_empty() && !truncated && !self.web_refused {
            self.step(Event::ReportRendered)?; // → Done
            State::Done
        } else {
            self.step(Event::ReportRenderedPartial)?; // → DonePartial
            State::DonePartial
        };
        self.close_manifest(
            terminal,
            truncated || self.window_capped || !self.prior_gap_texts.is_empty() || self.web_refused,
        )
    }

    /// The last written draft, or an honest empty one (a run that
    /// never drafted).
    fn last_draft(&self) -> Draft {
        let dir = &self.config.run_dir;
        let mut best: Option<(u32, Draft)> = None;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(rest) = name.strip_prefix("draft-") {
                    if let Some(round_str) = rest.strip_suffix(".json") {
                        if let Ok(r) = round_str.parse::<u32>() {
                            if let Ok(json) = std::fs::read_to_string(e.path()) {
                                if let Ok(d) = serde_json::from_str::<Draft>(&json) {
                                    if best.as_ref().map(|(br, _)| r > *br).unwrap_or(true) {
                                        best = Some((r, d));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        best.map(|(_, d)| d).unwrap_or_else(|| Draft {
            icd: "draft".to_string(),
            version: icd::ICD_VERSION,
            run_id: self.config.run_id.clone(),
            charter_hash: self.charter_hash.clone(),
            round: 0,
            provider: "port:draft".to_string(),
            url_constraint: UrlConstraintPolicy {
                enabled: true,
                layer: "sovereign-inference:UrlAllowlistConstraint".to_string(),
            },
            text: format!(
                "No draft was produced for {} before the run closed.",
                self.question
            ),
            citations: Vec::new(),
        })
    }
}

/// The charter, derived from the config alone (FR-3: thresholds frozen
/// at launch). A free function so the controller can be born with its
/// real identity — see [`Controller::start`].
///
/// drb1-r1 Item 3: Applies caller overrides with downward-only clamping.
fn build_charter(config: &RunConfig) -> Charter {
    // Apply overrides with downward-only clamping (callers can only tighten,
    // never raise above the charter's configured ceilings).
    let max_rounds = if let Some(override_val) = config.max_rounds_override {
        let clamped = override_val.min(config.max_rounds);
        if clamped != config.max_rounds {
            tracing::debug!(
                target: "deep_research",
                from = override_val,
                to = clamped,
                charter_max = config.max_rounds,
                "caller tightened max_rounds (clamped to charter ceiling)"
            );
        }
        clamped
    } else {
        config.max_rounds
    };

    let web_search_queries = if let Some(override_val) = config.max_search_override {
        let clamped = override_val.min(config.web_search_allowance);
        if clamped != config.web_search_allowance {
            tracing::debug!(
                target: "deep_research",
                from = override_val,
                to = clamped,
                charter_max = config.web_search_allowance,
                "caller tightened max_search (clamped to charter ceiling)"
            );
        }
        clamped
    } else {
        config.web_search_allowance
    };

    let web_fetch_pages = if let Some(override_val) = config.max_fetch_override {
        let clamped = override_val.min(config.web_fetch_allowance);
        if clamped != config.web_fetch_allowance {
            tracing::debug!(
                target: "deep_research",
                from = override_val,
                to = clamped,
                charter_max = config.web_fetch_allowance,
                "caller tightened max_fetch (clamped to charter ceiling)"
            );
        }
        clamped
    } else {
        config.web_fetch_allowance
    };

    Charter {
        icd: "charter".to_string(),
        version: icd::ICD_VERSION,
        run_id: config.run_id.clone(),
        question: config.question.clone(),
        seed_id: config.seed_id.clone(),
        created_at_unix: now_unix(),
        charter: CharterValues {
            max_rounds,
            evidence_window_max_chunks: config.evidence_window_max_chunks,
            containment: icd::ContainmentConfig {
                trigger: "judge-supported".to_string(),
                extraction_max_tokens: 32,
                specifics_max: 4,
            },
            triage: TriageConfig {
                code_set_k: config.code_set_k,
                eps_quota: config.eps_quota,
                content_coverage_floor: config.content_coverage_floor,
                prose_line_floor: config.prose_line_floor,
            },
            budget: icd::BudgetAllowance {
                web_search_queries,
                web_fetch_pages,
            },
            custody: CustodyPolicy {
                stamp_required: true,
                unknown_refuses: true,
            },
            url_constraint: UrlConstraintPolicy {
                enabled: true,
                layer: "sovereign-inference:UrlAllowlistConstraint".to_string(),
            },
            consent: config.consent.clone(),
        },
        frozen: true,
    }
}

fn hash_charter(charter: &Charter) -> String {
    // The wall clock must not leak into the identity hash (order
    // deep-research-t3a, measured red dr-1786979612): the charter is
    // rebuilt from the checkpoint's config at `--resume`, and a
    // `created_at_unix` field would make the hash differ from the
    // launch-time value whenever a second ticks between launch and
    // resume — an honest resume would always refuse as "tampered".
    // The identity is the config-derived content only; the timestamp
    // is a record, not an identity (regression test
    // charter_hash_is_time_independent).
    let mut hashed = charter.clone();
    hashed.created_at_unix = 0;
    let json = serde_json::to_string(&hashed).unwrap_or_default();
    format!("{:016x}", fnv1a(json.as_bytes()))
}

fn allowance_map(config: &RunConfig) -> std::collections::HashMap<String, u32> {
    let mut m = std::collections::HashMap::new();
    m.insert(
        format!(
            "{FAMILY_WEB_SEARCH}:{}",
            source_budget_key(config.search_source, &config.web_backend)
        ),
        config.web_search_allowance,
    );
    m.insert(
        format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"),
        config.web_fetch_allowance,
    );
    m
}

pub use estate::{AlignmentDecision, ResearchPort};

#[cfg(test)]
mod tests {
    use super::estate::{EstateListing, PortHit};
    use super::*;
    use crate::types::{CompletionResponse, Depth, ProviderCapabilities};
    use futures::Stream;
    use sovereign_contracts::types::{CompletionRequest, Speed};
    use std::pin::Pin;

    fn wc(id: &str, url: &str) -> WindowChunk {
        WindowChunk {
            id: id.to_string(),
            locator: url.to_string(),
            source_url: url.to_string(),
            custody: "public-web".to_string(),
            provenance_class: "known".to_string(),
            content: "body".to_string(),
            ingested_into: None,
            tags: Vec::new(),
        }
    }

    #[test]
    fn a_repeated_evidence_handle_is_reported_not_silently_resolved() {
        // The failure this catches is silent BY CONSTRUCTION: `number_citations`
        // resolves a handle with `.find(|c| c.id == id)`, so a repeated id binds
        // every citation to the FIRST chunk and the deliverable ships the wrong
        // source URL for the rest. Nothing errors, and the report reads fine.
        //
        // Shape taken from the real bed dr-1787807617, where `estate_window`
        // numbered by QUERY: estate-1 covered eight chunks.
        let chunks = vec![
            wc("estate-1", "https://a.example/one"),
            wc("estate-1", "https://b.example/two"),
            wc("estate-1", "https://c.example/three"),
            wc("ev-2", "https://d.example/round1"),
            wc("ev-2", "https://e.example/round2"),
            wc("ev-9", "https://f.example/unique"),
        ];
        let dupes = Controller::duplicate_window_ids(&chunks);
        assert_eq!(
            dupes,
            vec![("estate-1".to_string(), 3), ("ev-2".to_string(), 2)],
            "both collision families are named, with their multiplicity"
        );
        let shadowed: usize = dupes.iter().map(|(_, n)| n - 1).sum();
        assert_eq!(
            shadowed, 3,
            "three chunks cannot be cited apart from another"
        );
    }

    #[test]
    fn unique_handles_report_nothing() {
        // Watch-it-fail: make the counter query-scoped again and this fires.
        let chunks = vec![
            wc("ev-1", "https://a.example/1"),
            wc("ev-2", "https://b.example/2"),
            wc("estate-1", "https://c.example/3"),
            wc("estate-2", "https://d.example/4"),
        ];
        assert!(Controller::duplicate_window_ids(&chunks).is_empty());
    }

    /// A port that answers "nothing" — start() never reaches the
    /// network, so defaults are honest.
    struct NoopPort;
    /// The audit judges claims with bounded overlap (`AUDIT_CONCURRENCY`)
    /// rather than strictly serially. The whole safety argument for that
    /// is ORDER: the verdict set and the gap list are built from the
    /// audit sequence, so `buffered` (ordered) is correct and
    /// `buffer_unordered` would silently reshuffle the deliverable.
    ///
    /// This pins the property directly on the combinator, at the
    /// concurrency the audit actually uses, with per-item delays that
    /// GUARANTEE out-of-order completion — item 0 sleeps longest, so a
    /// reshuffling combinator cannot pass by luck. Watched red with
    /// `buffer_unordered` substituted: the assertion fails on the first
    /// element.
    #[tokio::test]
    async fn buffered_preserves_order_even_when_later_items_finish_first() {
        use futures::StreamExt as _;
        let n = AUDIT_CONCURRENCY * 3;
        let out: Vec<usize> = futures::stream::iter((0..n).map(|i| async move {
            // Item 0 waits longest; the last waits least.
            tokio::time::sleep(std::time::Duration::from_millis(((n - i) * 4) as u64)).await;
            i
        }))
        .buffered(AUDIT_CONCURRENCY)
        .collect()
        .await;
        assert_eq!(
            out,
            (0..n).collect::<Vec<_>>(),
            "the audit's combinator must yield in source order — the verdict set and \
             the gap list are built from this sequence"
        );
    }

    #[async_trait::async_trait]
    impl ResearchPort for NoopPort {
        async fn estate_listing(&self, _ids: &[String]) -> Result<EstateListing, String> {
            Ok(EstateListing {
                corpora: Vec::new(),
            })
        }
        async fn estate_search(
            &self,
            _ids: &[String],
            _q: &str,
            _l: usize,
        ) -> Result<Vec<PortHit>, String> {
            Ok(Vec::new())
        }
        async fn web_search(&self, _b: &str, _q: &str, _l: usize) -> Result<Vec<PortHit>, String> {
            Ok(Vec::new())
        }
        async fn web_fetch(&self, _u: &str) -> Result<String, String> {
            Ok(String::new())
        }
        async fn terminal_poll(&self) -> Result<(), String> {
            Ok(())
        }
        async fn draft(
            &self,
            _leg: crate::deep_research::estate::DraftLeg,
            _p: &str,
            _s: Option<&str>,
            _a: &[String],
        ) -> Result<String, String> {
            Ok(String::new())
        }
    }

    /// The same minimal stub the grounding tests use — start() never
    /// asks the provider anything. (`crate::error::Result`, the crate's
    /// 1-parameter alias — the port trait uses std's 2-parameter
    /// `Result<T, String>`.)
    struct NoProvider;
    #[async_trait::async_trait]
    impl InferenceProvider for NoProvider {
        async fn complete(
            &self,
            _r: &CompletionRequest,
        ) -> crate::error::Result<CompletionResponse> {
            Ok(CompletionResponse {
                text: "no".into(),
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "test".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> crate::error::Result<Pin<Box<dyn Stream<Item = crate::error::Result<String>> + Send>>>
        {
            unimplemented!()
        }
        async fn embed(&self, _t: &str) -> crate::error::Result<Vec<f32>> {
            Ok(vec![])
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    fn demo_config(run_dir: PathBuf) -> RunConfig {
        RunConfig {
            run_id: "dr-start-test".to_string(),
            question: "When did the Apollo 11 mission land on the Moon?".to_string(),
            seed_id: None,
            run_dir,
            max_rounds: 3,
            code_set_k: 3,
            eps_quota: 0.1,
            content_coverage_floor: acquisition::DEFAULT_CONTENT_COVERAGE_FLOOR,
            prose_line_floor: acquisition::DEFAULT_PROSE_LINE_FLOOR,
            evidence_window_max_chunks: 20,
            estate_corpus_ids: Vec::new(),
            web_backend: "duckduckgo".to_string(),
            search_source: SearchSource::Mock,
            web_search_allowance: 4,
            web_fetch_allowance: 4,
            posture: ShardingPrivacy::LocalOnly,
            consent: None,
            max_rounds_override: None,
            max_search_override: None,
            max_fetch_override: None,
        }
    }

    /// The ledger is a run-scoped artifact: it must land in the run
    /// dir and carry the run's identity from birth. Watched failure:
    /// the demo's first measurement left a stray `budget-ledger.json`
    /// in the process CWD with an EMPTY run_id (repo root, run
    /// dr-1786720828) — a placeholder decider journaled to
    /// `PathBuf::new().join("budget-ledger.json")` before the real
    /// decider replaced it.
    #[tokio::test]
    async fn start_journals_the_budget_ledger_only_inside_the_run_dir() {
        let dir = std::env::temp_dir().join(format!("dr-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let run_dir = dir.join("runs").join("dr-start-test");
        // A stray from an earlier buggy run must not masquerade as a
        // clean CWD — remove it visibly before the measurement.
        let cwd_leak = PathBuf::from("budget-ledger.json");
        if cwd_leak.exists() {
            std::fs::remove_file(&cwd_leak).expect("clear pre-existing CWD stray");
        }
        let ctl = Controller::start(
            demo_config(run_dir.clone()),
            Arc::new(NoopPort),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("controller start");
        let ledger_path = run_dir.join("budget-ledger.json");
        let ledger: super::icd::BudgetLedger =
            serde_json::from_str(&std::fs::read_to_string(&ledger_path).unwrap_or_else(|e| {
                panic!("ledger must exist in the run dir {ledger_path:?}: {e}")
            }))
            .expect("ledger parses");
        assert_eq!(
            ledger.run_id, "dr-start-test",
            "the ledger carries the run's identity from birth"
        );
        assert!(
            !ledger.charter_hash.is_empty(),
            "the ledger carries the charter hash"
        );
        assert!(
            !cwd_leak.exists(),
            "no budget-ledger.json may appear outside the run dir (the CWD leak the demo measured)"
        );
        assert_eq!(ctl.state, State::Initializing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// RED-first (order deep-research-t1d fix 2 — breadth): round-1
    /// queries cover every deck hit for the v1-shaped question.
    ///
    /// The HEAD failure shape (measured in the t1c battery,
    /// dr-1786748480): round 1 asked ONLY the question — the
    /// empty-window gap's query — so deck hits whose match tokens sit
    /// outside the question text never reached the window (4 of 11 v1
    /// hits). The fixed loop joins the plan's acquisition frontier
    /// (the scripted sub-questions here — the mock's plan_subquestions
    /// surface) to the round-1 query set, and every deck hit must be
    /// covered. Watch-it-fail: on the pre-fix code, round-1 queries
    /// are 8 identical copies of the question and the coverage
    /// assertion fails 0 of 8.
    #[tokio::test]
    async fn round1_queries_cover_every_deck_hit() {
        use super::gym::{Deck, MockBackendImpl, MockDraftSurface};

        let frontier_lines = vec![
            "Which cities show the highest Gini index of income inequality?",
            "How did New Orleans rank on the 80/20 income ratio?",
            "What does the Case-Shiller index say about home prices since 2000?",
            "How did the white share of urban cores change after 2000?",
            "How did manufacturing employment shift between 1979 and the pandemic?",
            "What is the price-to-income ratio in California?",
            "Did economic mobility worsen for low-income families?",
            "How did poverty rates change in gentrifying neighborhoods?",
        ];
        let token_lines = [
            (
                "Gini",
                "The Gini index of income inequality in New York rose from 0.50 in 1980 to 0.55 in 2024.",
            ),
            (
                "New Orleans",
                "New Orleans ranked among the highest on the 80/20 income ratio in the 2010s.",
            ),
            (
                "Case-Shiller",
                "The Case-Shiller index shows home prices in San Francisco quadrupled since 2000.",
            ),
            (
                "white share",
                "The white share of urban cores fell in every decade after 2000.",
            ),
            (
                "manufacturing",
                "Manufacturing employment shifted from the Northeast to the South between 1979 and the pandemic.",
            ),
            (
                "price-to-income",
                "The price-to-income ratio in California doubled from 1990 to 2024.",
            ),
            (
                "mobility",
                "Economic mobility worsened for low-income families after 1980.",
            ),
            (
                "poverty",
                "Poverty rates fell in gentrifying neighborhoods as rents rose.",
            ),
        ];
        let mut deck_toml = String::from(
            "version = 1\n\
             [[corpus]]\n\
             corpus_id = \"cities\"\n\
             kind = \"documents\"\n\
             chunks_count = 8\n\
             searchable = true\n\
             custody = \"personal\"\n",
        );
        let mut bodies: Vec<(String, String)> = Vec::new();
        for (i, (token, fact)) in token_lines.iter().enumerate() {
            deck_toml.push_str(&format!(
                "[[hit]]\n\
                 match = [\"{token}\"]\n\
                 url = \"https://gym.example/city{i}\"\n\
                 title = \"city page {i}\"\n\
                 snippet = \"About {token}.\"\n\
                 body = \"city{i}.md\"\n"
            ));
            bodies.push((format!("city{i}.md"), fact.to_string()));
        }
        let body_refs: Vec<(&str, &str)> = bodies
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let deck = Deck::parse(&deck_toml, &body_refs).expect("breadth deck builds");

        let port = MockBackendImpl::new(
            deck.clone(),
            MockDraftSurface::Scripted(frontier_lines.join("\n")),
        );
        let dir = std::env::temp_dir().join(format!("dr-breadth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let run_dir = dir.join("runs").join("dr-breadth");
        let mut cfg = demo_config(run_dir.clone());
        cfg.web_backend = MockBackendImpl::BACKEND_ID.to_string();
        cfg.web_search_allowance = 40;
        cfg.web_fetch_allowance = 40;
        // The question text must not contain any hit's match token —
        // the red shape: the pre-fix round-1 query (the question) covers
        // zero of the eight hits.
        cfg.question =
            "How did American cities change across four decades (1980 to 2024)?".to_string();
        cfg.run_id = "dr-breadth".to_string();

        run(
            cfg,
            Arc::new(port),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("the loop drives to a terminal state");

        // The plan records the acquisition frontier — the round-1 query
        // set the loop promises.
        let plan: super::icd::Plan = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join("plan.json")).expect("plan.json exists"),
        )
        .expect("plan.json parses");
        assert_eq!(
            plan.acquisition.queries_preplanned, frontier_lines,
            "plan must record the decomposed acquisition frontier verbatim"
        );
        assert_eq!(
            plan.acquisition.source, "plan-subquestions",
            "the frontier's source names the decomposition, not the gap template"
        );

        // Round-1 queries cover every deck hit — the fix-2 invariant.
        // drb1-r2b: under the round-allowance split round 1 EXECUTES
        // its fair share of the allowance, not the whole formed set —
        // ceil(40/3) = 14 of the 16 formed queries (8 audit-gap +
        // 8 frontier; the gaps outrank the frontier in the executed
        // set, so 6 of 8 frontier queries run in round 1 and the rest
        // are covered by the gap queries below). The FULL frontier
        // stays recorded on the plan (the assertion above).
        let fetch_list: super::icd::FetchList = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join("fetch-list-1.json"))
                .expect("fetch-list-1.json exists"),
        )
        .expect("fetch-list-1.json parses");
        let frontier_queries: Vec<&str> = fetch_list
            .queries
            .iter()
            .filter(|q| q.formed_by == "plan-subquestion")
            .map(|q| q.text.as_str())
            .collect();
        assert_eq!(
            fetch_list.queries.len(),
            14,
            "round 1 executes ceil(allowance/max_rounds) queries — the \
             round-allowance split (order drb1-r2b)"
        );
        assert_eq!(
            frontier_queries.len(),
            6,
            "the round's own gaps outrank the frontier in the executed set"
        );
        for (i, hit) in deck.hits.iter().enumerate() {
            let covered = fetch_list
                .queries
                .iter()
                .any(|q| deck.query_matches(i, &q.text));
            assert!(
                covered,
                "deck hit {i} ({}) unreached by round-1 queries — \
                 round-1 queries must cover every deck hit",
                hit.url
            );
        }
        assert!(
            !fetch_list.search_hits.is_empty(),
            "round-1 hits must reach the fetch list"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// RED-first (acquisition tune, 2026-08-24): every chunk that
    /// reaches the evidence window carries its SOURCE TITLE into
    /// enrichment — including a chunk fetched from below the K ∪ ε
    /// tier.
    ///
    /// THE SHAPE. drb1-t2 made the fetch walk queue `triaged.candidates`
    /// (every non-noise ranked row, budget deciding the depth) but left
    /// the R7 title join reading `triaged.ranked` (the K ∪ ε tier, 6
    /// rows at the production defaults). The two disagree the moment
    /// `round_fetch_cap` exceeds the tier — the walk fetches rank 7 and
    /// beyond, the join misses them, `enrich_window` receives an empty
    /// title, and `derive_tags` falls back to its no-signal branch: the
    /// chunk's only tag becomes the raw source URL. Silent — the chunk
    /// is in the window, the run is green, and the enrichment that the
    /// audit and the compose legs read is degraded for exactly the
    /// sources the permissive walk was built to reach.
    ///
    /// WHY IT IS LATENT AND WHY IT MATTERS NOW. At the DRB-I settings
    /// (`web_fetch_allowance` 12, `max_rounds` 3) the round cap is
    /// ceil(12/3) = 4, which is BELOW the 6-row tier, so the walk never
    /// gets there and the bug cannot fire. Every measured lever for
    /// closing the acquisition gap raises that allowance, and the first
    /// one that pushes the round cap past 6 activates this. It is fixed
    /// ahead of the raise, not after it.
    ///
    /// WATCH IT FAIL: with the join on `ranked`, the eight deck hits
    /// fetch under a cap of ceil(40/3) = 14 and the rows past the tier
    /// arrive tagged `["https://gym.example/cityN"]` instead of
    /// `["city", "page"]`.
    #[tokio::test]
    async fn below_tier_chunks_keep_their_source_title_in_enrichment() {
        use super::gym::{Deck, MockBackendImpl, MockDraftSurface};

        // Eight hits, each reachable by its own query — the frontier
        // fans wide enough that the walk must go past rank 6.
        let frontier_lines: Vec<String> = (0..8)
            .map(|i| format!("What does source {i} report about topic t{i}?"))
            .collect();
        let mut deck_toml = String::from("version = 1\n");
        let mut bodies: Vec<(String, String)> = Vec::new();
        for i in 0..8 {
            deck_toml.push_str(&format!(
                "[[hit]]\n\
                 match = [\"t{i}\"]\n\
                 url = \"https://gym.example/city{i}\"\n\
                 title = \"city page {i}\"\n\
                 snippet = \"About topic t{i}.\"\n\
                 body = \"city{i}.md\"\n"
            ));
            bodies.push((
                format!("city{i}.md"),
                format!("Source {i} reports that topic t{i} rose by {i}0 percent."),
            ));
        }
        let body_refs: Vec<(&str, &str)> = bodies
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let deck = Deck::parse(&deck_toml, &body_refs).expect("title-join deck builds");

        let port = MockBackendImpl::new(
            deck.clone(),
            MockDraftSurface::Scripted(frontier_lines.join("\n")),
        );
        let dir = std::env::temp_dir().join(format!("dr-titlejoin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let run_dir = dir.join("runs").join("dr-titlejoin");
        let mut cfg = demo_config(run_dir.clone());
        cfg.web_backend = MockBackendImpl::BACKEND_ID.to_string();
        // The allowance that puts the round cap (ceil(40/3) = 14) past
        // the K ∪ ε tier (5 + ceil(5 * 0.1) = 6) — the condition the
        // bug needs, and the condition every acquisition raise creates.
        cfg.web_search_allowance = 40;
        cfg.web_fetch_allowance = 40;
        cfg.question = "What changed across the eight sources?".to_string();
        cfg.run_id = "dr-titlejoin".to_string();

        run(
            cfg,
            Arc::new(port),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("the loop drives to a terminal state");

        let window: super::icd::EvidenceWindow = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join("evidence-window-1.json"))
                .expect("evidence-window-1.json exists"),
        )
        .expect("evidence-window-1.json parses");
        assert!(
            window.chunks.len() > super::acquisition::DEFAULT_CODE_SET_K + 1,
            "the instrument needs a walk PAST the tier — {} chunks is not \
             past the {}-row K ∪ ε tier, so this test cannot see the bug \
             it exists to catch (§18.1: validate the instrument first)",
            window.chunks.len(),
            super::acquisition::DEFAULT_CODE_SET_K + 1
        );
        for chunk in &window.chunks {
            assert_ne!(
                chunk.tags,
                vec![chunk.source_url.clone()],
                "chunk {} ({}) carries the no-signal tag fallback — its \
                 title never reached enrichment, so the R7 join missed a \
                 row the fetch walk admitted",
                chunk.id,
                chunk.source_url
            );
            assert!(
                chunk.tags.iter().any(|t| t == "city" || t == "page"),
                "chunk {} ({}) lost its source title in enrichment: tags {:?}",
                chunk.id,
                chunk.source_url,
                chunk.tags
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// RED-first (order deep-research-t1g — rung 2, corpus search): a
    /// corpus carrying a deck's facts is retrievable by the loop's
    /// acquisition through the CORPUS source — a concept query (no
    /// figure, no bank vocabulary) must retrieve the value-bearing
    /// chunk AND its content must reach the evidence window, with the
    /// chunk's estate custody kept (never re-stamped public-web).
    ///
    /// The HEAD failure shape (measured in the t1f battery): the mock's
    /// estate leg answers the decked empty — zero hits — so an
    /// acquisition routed to the corpus source retrieves nothing and
    /// the evidence window stays empty. Watch-it-fail: on the
    /// pre-wiring shape (the corpus surface unread, the dispatch
    /// routed but estate_search still answering the decked empty)
    /// round-1 search hits are zero, no `estate:` hit exists, and the
    /// value-bearing chunk never reaches the window.
    #[tokio::test]
    async fn corpus_source_retrieves_value_bearing_chunk_into_window() {
        use super::gym::{CorpusEmbed, CorpusSurface, Deck, MockBackendImpl, MockDraftSurface};
        use corpus_engine::index::{InsertChunk, InsertCodeMeta};
        use corpus_engine::CorpusIndex;

        const EMBED_DIM: usize = 8;
        fn embedding(seed: f32) -> Vec<f32> {
            // Deterministic seeded embeddings — the corpus-engine
            // tests' precedent (sharding_round_trip_e2e.rs): the FTS
            // leg does the lexical work; the vector leg is stable.
            (0..EMBED_DIM).map(|i| seed + i as f32 * 0.1).collect()
        }
        struct FakeEmbed;
        #[async_trait::async_trait]
        impl CorpusEmbed for FakeEmbed {
            async fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
                let seed = text.bytes().fold(0f32, |a, b| a + b as f32) % 100.0;
                Ok(embedding(seed))
            }
        }

        // The fixture corpus: the deck's facts as chunks — one
        // value-bearing (a distinctive figure), one value-bearing at
        // 2-digit scale, one unrelated.
        let dir = std::env::temp_dir().join(format!("dr-corpus-source-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let index = CorpusIndex::create(
            &dir,
            "dr-fixture",
            "Rung-2 Fixture",
            "test-embed",
            EMBED_DIM,
            true,
            "MIT",
        )
        .await
        .expect("fixture corpus creates");
        let rows = [
            (
                "New York City's Gini coefficient of income inequality reached 0.5469 in \
                 2019 — the highest of any large American city.",
                "NYC inequality",
            ),
            (
                "Seattle's price-to-income ratio stood at 7.87 in 2024, among the steepest \
                 in the nation.",
                "Seattle affordability",
            ),
            (
                "The municipal zoning commission voted on a parks bond on Tuesday.",
                "Distractor",
            ),
        ];
        let payload: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(i, (content, title))| {
                (
                    InsertChunk {
                        content: content.to_string(),
                        title: Some(title.to_string()),
                        url: None,
                        metadata: None,
                        content_hash: None,
                        source_doc_id: None,
                        source_file: None,
                        code: InsertCodeMeta::default(),
                        unit_id: None,
                    },
                    embedding(i as f32),
                )
            })
            .collect();
        index
            .insert_batch(&payload)
            .await
            .expect("fixture chunks insert");
        index
            .build_indexes(true, true, None)
            .await
            .expect("fixture indexes build");
        index.mark_indexes_built().expect("indexes marked built");
        index
            .mark_ingestion_complete()
            .expect("ingestion marked complete");

        // The mock: the deck declares the corpus LISTED and searchable
        // (F16's shape) but carries no web hits — the acquisition's
        // corpus source serves the estate leg from the fixture corpus.
        let deck_toml = format!(
            "version = 1\n\
             [[corpus]]\n\
             corpus_id = \"dr-fixture\"\n\
             kind = \"documents\"\n\
             chunks_count = 3\n\
             searchable = true\n\
             custody = \"personal\"\n"
        );
        let deck = Deck::parse(&deck_toml, &[]).expect("corpus deck builds");
        // The scripted frontier and question carry NO digits at all —
        // the concept shape: the queries must not name any figure.
        let frontier_lines = vec![
            "How unequal is New York's largest city?",
            "How expensive are homes in American cities?",
            "What happened to housing affordability for renters?",
        ];
        let port = MockBackendImpl::with_corpus(
            deck,
            MockDraftSurface::Scripted(frontier_lines.join("\n")),
            CorpusSurface {
                indexes: vec![index],
                embed: Box::new(FakeEmbed),
            },
        );

        let run_dir = dir.join("runs").join("dr-corpus-source");
        let mut cfg = demo_config(run_dir.clone());
        cfg.web_backend = MockBackendImpl::BACKEND_ID.to_string();
        cfg.search_source = SearchSource::Corpus;
        cfg.estate_corpus_ids = vec!["dr-fixture".to_string()];
        cfg.question = "How did income inequality and housing affordability change in \
                        American cities over recent decades?"
            .to_string();
        cfg.run_id = "dr-corpus-source".to_string();

        run(
            cfg,
            Arc::new(port),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("the loop drives to a terminal state");

        // Round-1 queries name no figure — the concept shape.
        let fetch_list: super::icd::FetchList = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join("fetch-list-1.json"))
                .expect("fetch-list-1.json exists"),
        )
        .expect("fetch-list-1.json parses");
        let value_runs: Vec<String> = fetch_list
            .queries
            .iter()
            .flat_map(|q| {
                q.text
                    .split(|c: char| !c.is_ascii_digit())
                    .filter(|d| d.len() >= 3)
            })
            .map(|d| d.to_string())
            .collect();
        assert!(
            value_runs.is_empty(),
            "round-1 queries must carry no value-shaped digit runs (the concept shape): {value_runs:?}"
        );

        // The corpus source served the round: hits are engine "corpus"
        // with chunk-level estate locators (the dedup fix).
        assert!(
            !fetch_list.search_hits.is_empty(),
            "round-1 search hits must exist — the corpus source answered the acquisition"
        );
        for h in &fetch_list.search_hits {
            assert_eq!(
                h.engine, "corpus",
                "a corpus-source hit must record its engine: {}",
                h.url
            );
        }
        let estate_hits: Vec<_> = fetch_list
            .search_hits
            .iter()
            .filter(|h| h.url.starts_with("estate:dr-fixture:") && h.url.matches(':').count() == 2)
            .collect();
        assert!(
            !estate_hits.is_empty(),
            "corpus hits must carry chunk-level estate:<corpus>:<chunk> locators — \
             a corpus-level-only locator collapses the window's dedup-by-url"
        );

        // The value-bearing chunk's CONTENT reached the evidence
        // window, with the estate's custody kept.
        let window: super::icd::EvidenceWindow = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join("evidence-window-1.json"))
                .expect("evidence-window-1.json exists"),
        )
        .expect("evidence-window-1.json parses");
        let joined: String = window
            .chunks
            .iter()
            .map(|c| c.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("0.5469"),
            "the value-bearing chunk's content must reach the evidence window (concept \
             query -> value chunk). Window: {}",
            joined.chars().take(300).collect::<String>()
        );
        assert!(
            window.chunks.iter().any(|c| c.custody == "personal"),
            "an estate chunk's window custody must stay personal — never re-stamped \
             public-web: {:?}",
            window
                .chunks
                .iter()
                .map(|c| (c.source_url.clone(), c.custody.clone()))
                .collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// RED-first (order deep-research-t1e — figure-hunting): for a
    /// question whose own text implies figures, the plan artifact's
    /// sub-questions must carry figure specifiers.
    ///
    /// The HEAD failure shape (measured in the t1d battery,
    /// dr-1786754967): the daemon's draft sub-questions were THEMATIC —
    /// "How did income inequality trends evolve in American
    /// metropolitan areas between 1980 and 2024?" — no measure named,
    /// so the figure-specific deck hits (Gini 0.5469, the 7.87 ratio,
    /// white share, manufacturing jobs, Case-Shiller 325.78) never
    /// entered the evidence window and their keys were unreachable by
    /// any downstream fix. The fixed loop figure-hunts the frontier:
    /// every sub-question carries a figure specifier (a digit or a
    /// measure word) — the question's own specifiers folded in when
    /// the draft left one bare. SHAPE test: the fixture question's own
    /// text implies figures (digit tokens 1980/2024 + the measure word
    /// income); the scripted sub-questions are deliberately
    /// specifier-less; the plan artifact must carry specifiers in
    /// every sub-question and record the question's specifiers
    /// (glassbox). No bank vocabulary anywhere — the shape is generic.
    /// Watch-it-fail: on the pre-fix shape (fold-in disabled) the
    /// scripted lines pass through untouched and the specifier
    /// assertions fail.
    #[tokio::test]
    async fn plan_subquestions_carry_figure_specifiers() {
        use super::gym::{Deck, MockBackendImpl, MockDraftSurface};

        // Thematic, specifier-less sub-questions — the HEAD shape the
        // draft produced on the v1 question (measured, dr-1786754967).
        let frontier_lines = vec![
            "What were the primary drivers of the change in American cities?",
            "How did American cities evolve over time?",
        ];
        // The fixture question's OWN text implies figures: digit
        // tokens (1980, 2024) and a measure word (income).
        let question = "How did income inequality and housing affordability evolve \
                        across US cities from 1980 to 2024?";

        let mut deck_toml = String::from(
            "version = 1\n\
             [[corpus]]\n\
             corpus_id = \"cities\"\n\
             kind = \"documents\"\n\
             chunks_count = 2\n\
             searchable = true\n\
             custody = \"personal\"\n",
        );
        let mut bodies: Vec<(String, String)> = Vec::new();
        for (i, (token, fact)) in [
            ("income", "Income inequality in cities rose steadily."),
            ("price", "Home prices rose faster than incomes."),
        ]
        .iter()
        .enumerate()
        {
            deck_toml.push_str(&format!(
                "[[hit]]\n\
                 match = [\"{token}\"]\n\
                 url = \"https://gym.example/fh{i}\"\n\
                 title = \"city page {i}\"\n\
                 snippet = \"About {token}.\"\n\
                 body = \"fh{i}.md\"\n"
            ));
            bodies.push((format!("fh{i}.md"), fact.to_string()));
        }
        let body_refs: Vec<(&str, &str)> = bodies
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let deck = Deck::parse(&deck_toml, &body_refs).expect("figure-hunt deck builds");

        let port = MockBackendImpl::new(
            deck.clone(),
            MockDraftSurface::Scripted(frontier_lines.join("\n")),
        );
        let dir = std::env::temp_dir().join(format!("dr-fh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let run_dir = dir.join("runs").join("dr-fh");
        let mut cfg = demo_config(run_dir.clone());
        cfg.web_backend = MockBackendImpl::BACKEND_ID.to_string();
        cfg.web_search_allowance = 12;
        cfg.web_fetch_allowance = 12;
        cfg.question = question.to_string();
        cfg.run_id = "dr-fh".to_string();

        run(
            cfg,
            Arc::new(port),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("the loop drives to a terminal state");

        let plan: super::icd::Plan = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join("plan.json")).expect("plan.json exists"),
        )
        .expect("plan.json parses");
        // Glassbox: the plan records the question's own specifiers.
        assert_eq!(
            plan.acquisition.figure_specifiers,
            vec!["1980".to_string(), "2024".to_string(), "income".to_string()],
            "the plan must record the question's own figure specifiers"
        );
        // The plan's sub-questions carry figure specifiers — the
        // figure-hunted frontier.
        for q in &plan.acquisition.queries_preplanned {
            assert!(
                acquisition::has_figure_specifier(q),
                "the plan's sub-question must carry a figure specifier \
                 (a digit or a measure word): {q:?}"
            );
        }
        assert_eq!(
            plan.acquisition.queries_preplanned[0],
            "What were the primary drivers of the change in American cities? (1980, 2024, income)",
            "the specifier-less sub-question gets the question's specifiers folded in"
        );
        assert_eq!(
            plan.acquisition.queries_preplanned[1],
            "How did American cities evolve over time? (1980, 2024, income)",
            "every specifier-less sub-question gets the fold-in"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// RED-first (order deep-research-t1e — R4 query forming): a gap
    /// query whose claim carries no figure specifier gets the
    /// question's own specifiers folded in — a thematic claim's
    /// follow-up query still hunts the figures the question implies;
    /// the numbers never silently drop out of the acquisition.
    /// A claim that already carries a specifier keeps its own shape,
    /// and the floor-capped FACT query is unchanged (t1d fix 3 — the
    /// second-origin target keeps the claim's figures). Watch-it-fail:
    /// on the pre-fix shape (no fold-in) the query is the bare prose
    /// template and the specifier assertion fails.
    #[test]
    fn gap_query_folds_in_question_specifiers() {
        let specs = ["1980".to_string(), "2024".to_string(), "income".to_string()];
        let thematic_claim =
            "Cities gentrified across the four decades, driven by economic factors.";
        let q = gap_query_for(thematic_claim, None, &specs);
        assert!(
            q.contains("1980") && q.contains("income"),
            "the figure-less claim's query must carry the question's specifiers: {q:?}"
        );
        assert_eq!(
            q,
            format!(
                "{} (1980, 2024, income)",
                template_query(thematic_claim, &specs)
            ),
            "the fold-in appends the question's specifiers to the prose template"
        );
        // A claim that already carries a figure keeps its own shape —
        // its estate figures stripped (t2c: "0.5469" and "2013" are
        // not the question's, so they never echo into the query; the
        // measure word "index" keeps the claim's own shape, no
        // fold-in).
        let figure_claim = "The Gini index in New York reached 0.5469 by 2013.";
        assert_eq!(
            gap_query_for(figure_claim, None, &specs),
            template_query(figure_claim, &specs),
            "a figure-bearing claim's query stands as formed, figures stripped"
        );
        assert!(
            !gap_query_for(figure_claim, None, &specs).contains("0.5469"),
            "a disallowed estate figure never echoes into the query"
        );
        // The floor-capped FACT query is unchanged in shape (fix 3) —
        // the allowed figures ride, not the fold-in.
        let record = icd::CorroborationRecord {
            origins: vec!["https://gym.example/one".to_string()],
            support_chunks: 1,
            floor: 2,
            passes_floor: false,
        };
        assert_eq!(
            gap_query_for(thematic_claim, Some(&record), &specs),
            fact_query(thematic_claim, &specs),
            "the floor-capped gap keeps the fact query — its figures ride, not the fold-in"
        );
        // No specifiers on the question → no fold-in anywhere.
        assert_eq!(
            gap_query_for(thematic_claim, None, &[]),
            template_query(thematic_claim, &[]),
            "a question with no specifiers folds nothing in"
        );
    }

    /// RED-first (order deep-research-t6d — the word-number class,
    /// pre-registered): the strict-shape re-draft spelled every figure
    /// as words (battery #4's v1 flight — loop_gap_trace [1, 21, 26],
    /// 40/40 could-not-judge, P4-v1 5/16) and the digit-only decider
    /// went blind. The fix: a word→digit table inverted from the
    /// adversarial generator's NUMBER_WORDS/ORDINAL_WORDS
    /// (sovereign-eval/src/flywheel/generators/adversarial.rs:588),
    /// applied inside figure_tokens — word-form text must yield the
    /// SAME figure tokens as its digit form. Watch-it-fail at HEAD:
    /// every pair below tokenizes differently (the word side blind or
    /// the unit word dropped).
    #[test]
    fn word_figures_tokenize_like_their_digit_forms() {
        for (word_form, digit_form) in [
            ("twenty percent", "20%"),
            ("fifty-eight point one percent", "58.1%"),
            ("seventeen point five times", "17.5"),
            ("ninety-five over twenty", "95/20"),
            ("eight point five", "8.5"),
            // The in-tree fixture shape (synthesize.rs:730 — the
            // complete-sentence-bullet test's "8 percent of all
            // reviewed neighborhoods").
            ("8 percent of all reviewed neighborhoods", "8%"),
            ("20 per cent", "20%"),
            ("one hundred twenty", "120"),
            ("twenty-first", "21"),
            ("two thousand", "2000"),
        ] {
            assert_eq!(
                figure_tokens(word_form),
                figure_tokens(digit_form),
                "the word form {word_form:?} must tokenize like {digit_form:?}"
            );
        }
    }

    /// RED-first (t6d — guard: NO false conversions). The unit
    /// mappings are structural: the prepositional "over" is not a
    /// ratio and "point" between non-figures is not a decimal. Green
    /// at HEAD, must stay green — the class fix must not invent
    /// figures.
    #[test]
    fn word_figure_guards_do_not_fabricate_tokens() {
        assert_eq!(
            figure_tokens("spending increased by over 20 percent last year"),
            figure_tokens("spending increased by over 20% last year"),
            "the prepositional 'over' must not become a ratio slash"
        );
        assert!(
            figure_tokens("the point of the study was the eviction rate").is_empty(),
            "a prose 'point' between non-figures is not a decimal"
        );
        assert_eq!(
            figure_tokens("8 point five percent"),
            figure_tokens("8.5%"),
            "a digit-run on the left still closes the decimal"
        );
    }

    /// RED-first (t6d — inversion completeness): every word in the
    /// adversarial generator's NUMBER_WORDS/ORDINAL_WORDS
    /// (sovereign-eval adversarial.rs:588) must tokenize to its digit
    /// value — the generator's vocabulary and the decider's
    /// vocabulary are the same set, embedded here verbatim from the
    /// generator. Watch-it-fail at HEAD: the word-only forms
    /// ("twelve", "twentieth", "hundred", "thousand") yield no tokens
    /// at all.
    #[test]
    fn adversarial_number_words_invert_to_figures() {
        let cardinals = [
            ("one", "1"),
            ("two", "2"),
            ("three", "3"),
            ("four", "4"),
            ("five", "5"),
            ("six", "6"),
            ("seven", "7"),
            ("eight", "8"),
            ("nine", "9"),
            ("ten", "10"),
            ("eleven", "11"),
            ("twelve", "12"),
            ("twenty", "20"),
            ("thirty", "30"),
            ("forty", "40"),
            ("fifty", "50"),
            ("hundred", "100"),
            ("thousand", "1000"),
        ];
        let ordinals = [
            ("first", "1"),
            ("second", "2"),
            ("third", "3"),
            ("fourth", "4"),
            ("fifth", "5"),
            ("sixth", "6"),
            ("seventh", "7"),
            ("eighth", "8"),
            ("ninth", "9"),
            ("tenth", "10"),
            ("eleventh", "11"),
            ("twelfth", "12"),
            ("thirteenth", "13"),
            ("fourteenth", "14"),
            ("fifteenth", "15"),
            ("sixteenth", "16"),
            ("seventeenth", "17"),
            ("eighteenth", "18"),
            ("nineteenth", "19"),
            ("twentieth", "20"),
        ];
        for (word, digits) in cardinals.iter().chain(ordinals.iter()) {
            assert_eq!(
                figure_tokens(word),
                vec![digits.to_string()],
                "the adversarial vocabulary word {word:?} must invert to {digits:?}"
            );
        }
    }

    /// RED-first (t6d — inheritance into the question specifiers): the
    /// acquisition specifiers read the SAME decider; a question
    /// spelling a figure in words must yield the same FIGURE specifier
    /// as the digit form. Watch-it-fail at HEAD: "twenty percent"
    /// yields "20", not "20%". (The raw-text measure-word pass is
    /// orthogonal and pre-existing: the word form's "percent" is a
    /// MEASURE_WORDS specifier, the digit form's "%" is not — the
    /// figure specifier itself is identical, which is this order's
    /// contract.)
    #[test]
    fn word_figures_inherit_into_question_specifiers() {
        let word_q = "Why did spending on cloud security rise by twenty percent in 2025?";
        let digit_q = "Why did spending on cloud security rise by 20% in 2025?";
        let word_specs = acquisition::figure_specifiers(word_q);
        let digit_specs = acquisition::figure_specifiers(digit_q);
        assert!(
            word_specs.iter().any(|s| s == "20%"),
            "the question's word-form figure must read as its digit form: {word_specs:?}"
        );
        assert!(
            digit_specs.iter().any(|s| s == "20%"),
            "the digit-form question carries the same figure: {digit_specs:?}"
        );
        assert!(
            word_specs.iter().any(|s| s == "2025") && digit_specs.iter().any(|s| s == "2025"),
            "both forms carry the era year"
        );
    }

    /// RED-first (t6d — inheritance into the fold identity): a claim
    /// spelling its figure in words must carry the same identity
    /// figures as the digit-form claim. Watch-it-fail at HEAD: the
    /// word-form claim's figures come back without the unit (the
    /// 40/40 could-not-judge blindness).
    #[test]
    fn word_figures_inherit_into_gap_identity() {
        let word_claim =
            "Gentrification affected fifty-eight point one percent of eligible tracts.";
        let digit_claim = "Gentrification affected 58.1% of eligible tracts.";
        let (word_figs, _) = gap_identity(word_claim, &[]);
        let (digit_figs, _) = gap_identity(digit_claim, &[]);
        assert_eq!(
            word_figs, digit_figs,
            "word-form and digit-form claims share one figure identity"
        );
        assert!(
            word_figs.iter().any(|f| f == "58.1%"),
            "the word-form claim's figure must read as 58.1%: {word_figs:?}"
        );
    }

    /// RED-first (order deep-research-t6d — the strip-3c anti-leak in
    /// word form): the t2c strip decider read digit runs only, so a
    /// word-figure claim's figure bypassed the strip — "four" survived
    /// in the template, the fold-in guard saw a figure specifier, and
    /// the question's era years silently dropped out of the follow-up
    /// query (the t1e numbers-drop-out failure mode, re-opened in word
    /// form; the estate's spelled-out echo "one hundred cities" leaked
    /// the same way). Word-form and digit-form claims must produce
    /// IDENTICAL templates, queries, and carried figure sets.
    #[test]
    fn word_figures_strip_and_fold_like_digit_forms() {
        // A word figure the question does NOT allow is stripped, and
        // the claim becomes specifier-less — the t1e fold-in fires
        // exactly as it does for the digit form.
        let specs = ["1980".to_string(), "2024".to_string()];
        let word_claim = "Cities gentrified across the four decades, driven by economic factors.";
        let digit_claim = "Cities gentrified across the 4 decades, driven by economic factors.";
        assert_eq!(
            template_query(word_claim, &specs),
            template_query(digit_claim, &specs),
            "the word-form template strips to the digit-form template"
        );
        let word_q = gap_query_for(word_claim, None, &specs);
        let digit_q = gap_query_for(digit_claim, None, &specs);
        assert_eq!(
            word_q, digit_q,
            "the word-form and digit-form follow-up queries are identical"
        );
        assert!(
            word_q.contains("1980") && word_q.contains("2024"),
            "the word-figure claim's query still hunts the era years: {word_q:?}"
        );
        // A word figure the question DOES allow rides — and the two
        // forms still agree (the claim keeps its own shape).
        let with_four = ["4".to_string(), "1980".to_string(), "2024".to_string()];
        assert_eq!(
            gap_query_for(word_claim, None, &with_four),
            gap_query_for(digit_claim, None, &with_four),
            "an allowed word figure rides exactly like its digit form"
        );
        // The estate's spelled-out echo never leaks — the word form
        // strips to the digit form's template.
        let estate =
            "researcher Martin analyzed data from the nation's one hundred largest cities.";
        let estate_digit = "researcher Martin analyzed data from the nation's 100 largest cities.";
        let stripped = strip_disallowed_figures(estate, &["1980".to_string()]);
        assert_eq!(
            stripped,
            strip_disallowed_figures(estate_digit, &["1980".to_string()]),
            "the spelled-out estate figure strips like the digit"
        );
        assert!(
            !stripped.contains('1') && !stripped.contains("hundred"),
            "the estate's spelled-out figure is gone from the template: {stripped:?}"
        );
        assert!(
            figure_tokens(&gap_query_for(estate, None, &specs))
                .iter()
                .all(|f| specs.contains(f)),
            "the estate's word figure never rides in the query"
        );
    }

    /// RED-first (order deep-research-t2c — the strip-3c query-side
    /// leak, Instrument 2): a gap claim carrying figures QUOTED FROM
    /// THE ESTATE's admitted chunk must not echo them into the next
    /// round's gap query. The measured shape (t1h v1 flight
    /// dr-1786933992, g2): the survey answer's claim carried "100"
    /// from the admitted estate chunk, and round-1's gap-template
    /// query echoed it verbatim ("30 100 last years trend become
    /// major concern urban planners ..."). Watch-it-fail at HEAD:
    /// both gap shapes carry the estate's figures. After the fix the
    /// query carries no figure tokens beyond the QUESTION's own (the
    /// allowed set — the question's era years), on BOTH shapes: the
    /// floor-capped FACT query and the prose template.
    #[test]
    fn gap_query_does_not_echo_estate_figures() {
        // The DEMO-7 measured claim shape — "the nation's largest 100
        // cities" is the estate's own admitted figure (the survey
        // answer quoted it); "30" is the claim's other estate figure.
        let claim = "Over the last 30 years, this trend has become a major concern for urban \
                     planners, while researcher Richard Martin analyzed data from the nation's \
                     largest 100 cities to track these changes.";
        let question = "How did American cities change across four decades (1980-2024)?";
        let specs = acquisition::figure_specifiers(question);
        assert_eq!(
            specs,
            ["4".to_string(), "1980".to_string(), "2024".to_string()],
            "the allowed set is the question's own figure tokens — the era years, and the \
             word figure four (order deep-research-t6d: a spelled-out number word IS a figure)"
        );
        // Both gap shapes: the floor-capped FACT query and the prose
        // template.
        let floor_capped = icd::CorroborationRecord {
            origins: vec!["estate:dr-demo6-v1:33".to_string()],
            support_chunks: 1,
            floor: 2,
            passes_floor: false,
        };
        for q in [
            gap_query_for(claim, Some(&floor_capped), &specs),
            gap_query_for(claim, None, &specs),
        ] {
            let carried = figure_tokens(&q);
            assert!(
                carried.iter().all(|f| specs.contains(f)),
                "the gap query echoes a figure the question does not carry \
                 (the strip-3c leak): carried {carried:?} in query {q:?}"
            );
        }
        // The leak's exact measured shape is gone: "100" never rides
        // the query.
        let template = gap_query_for(claim, None, &specs);
        assert!(
            !template.contains("100"),
            "the estate's quoted figure must not echo into the query: {template:?}"
        );
    }

    /// RED-first (order deep-research-t1d fix 3 — second-origin): when
    /// the floor caps a claim, the next round's gap query must target
    /// the claim's FACT — the figure the second origin must carry —
    /// not the first 140 characters of prose. Watch-it-fail at HEAD
    /// (t1d): the figure sits beyond the template's 140-char cut, so
    /// the query misses the very number the floor demanded (the t1c
    /// R-12 measurement: 0/12 on v0 single-origin decks — the
    /// follow-up query could never surface the missing second origin).
    /// Contract merged at t2c (the strip-3c instrument): the query
    /// carries the claim's figures ONLY when the question's own
    /// specifiers carry them — the allowed "2024" rides (beyond the
    /// 140-char cut, so the FACT shape still proves itself), the
    /// estate's "0.55" never echoes.
    #[test]
    fn floor_capped_gap_query_targets_the_missing_origin_fact() {
        let claim = format!(
            "{} The Gini index of income inequality in New York rose to 0.55 by 2024.",
            "A long background clause that carries no load-bearing figure. ".repeat(6)
        );
        assert!(
            claim.chars().count() > 140,
            "the fixture's figure must sit beyond the template's 140-char cut"
        );
        // The question's own specifiers — derived exactly as the loop
        // derives them (acquisition::figure_specifiers).
        let question = "How did the Gini index change by 2024?";
        let specs = acquisition::figure_specifiers(question);
        assert!(
            specs.iter().any(|s| s == "2024") && !specs.iter().any(|s| s == "0.55"),
            "the fixture's allowed set carries the question's year, never the estate's figure"
        );
        let audit = audit::ClaimAudit {
            claim: claim.clone(),
            verdict: super::icd::Verdict::CouldNotJudge,
            action: super::icd::GateAction::CorroborationFloor,
            witness: super::icd::WitnessRecord::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: Some(
                "corroboration floor: 1 supporting chunk from 1 distinct origin".to_string(),
            ),
            corroboration: Some(super::icd::CorroborationRecord {
                origins: vec!["https://gym.example/one".to_string()],
                support_chunks: 1,
                floor: 2,
                passes_floor: false,
            }),
        };
        let gap_list = audit::build_gap_list(
            "run",
            "hash",
            2,
            &[audit],
            &[],
            "question?",
            &specs,
            &|c, corr| gap_query_for(c, corr, &specs),
        );
        assert_eq!(gap_list.gaps.len(), 1);
        let gap = &gap_list.gaps[0];
        assert!(
            gap.actionable_query.contains("2024"),
            "the floor-capped gap's query must carry the ALLOWED figure \
             (beyond the 140-char prose cut) so the next round can target \
             the missing second origin: {:?}",
            gap.actionable_query
        );
        assert!(
            gap.actionable_query.contains("Gini"),
            "the fact query keeps the claim's subject content words: {:?}",
            gap.actionable_query
        );
        assert!(
            !gap.actionable_query.contains("0.55"),
            "the estate's figure never echoes into the query (strip-3c): {:?}",
            gap.actionable_query
        );
        let cap = gap
            .corroboration
            .as_ref()
            .expect("the gap carries the floor's corroboration record");
        assert!(!cap.passes_floor && cap.origins == ["https://gym.example/one"]);
        // A claim the floor did not cap keeps the prose template — the
        // fact query is the floor's shape, not a global rewrite.
        let plain = audit::ClaimAudit {
            claim: claim.clone(),
            verdict: super::icd::Verdict::CouldNotJudge,
            action: super::icd::GateAction::AbstainedDecline,
            witness: super::icd::WitnessRecord::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: Some("judge failed to run".to_string()),
            corroboration: None,
        };
        let gap_list = audit::build_gap_list(
            "run",
            "hash",
            2,
            &[plain],
            &[],
            "question?",
            &specs,
            &|c, corr| gap_query_for(c, corr, &specs),
        );
        assert_eq!(
            gap_list.gaps[0].actionable_query,
            acquisition::figure_hunt_query(template_query(&claim, &specs), &specs),
            "a claim the floor did not cap keeps the prose template — \
             the fold-in rides it as it always has (t1e)"
        );
    }

    // ── T3a resume surface (order deep-research-t3a) ─────────────────

    /// A small deck-backed port for the resume flights: one hit matched
    /// by a query term, drafts scripted. Rounds iterate while the
    /// audit keeps abstaining (NoProvider), so a high-budget flight
    /// reaches the round loop's terminal tail.
    fn resume_deck_port() -> super::gym::MockBackendImpl {
        use super::gym::{Deck, MockBackendImpl, MockDraftSurface};
        let deck_toml = "version = 1\n\
             [[corpus]]\n\
             corpus_id = \"resume-city\"\n\
             kind = \"documents\"\n\
             chunks_count = 1\n\
             searchable = true\n\
             custody = \"personal\"\n\
             [[hit]]\n\
             match = [\"Gini\"]\n\
             url = \"https://gym.example/resume-city\"\n\
             title = \"resume city page\"\n\
             snippet = \"About Gini.\"\n\
             body = \"resume-city.md\"\n";
        let body = "The Gini index of income inequality in New York rose from 0.50 in 1980 to \
                    0.55 in 2024.";
        let deck = Deck::parse(deck_toml, &[("resume-city.md", body)]).expect("deck builds");
        MockBackendImpl::new(
            deck,
            MockDraftSurface::Scripted(
                "Which cities show the highest Gini index of income inequality?".to_string(),
            ),
        )
    }

    /// The SIGKILL artifact state: a completed flight's run dir with
    /// the TERMINAL chain removed (manifest/verdict-set/report) — what
    /// a kill mid-flight leaves: checkpoint + ledger + windows on
    /// disk, NO manifest (the resumable shape; a manifest marks the
    /// run terminal and refuses).
    fn simulate_kill(run_dir: &Path) {
        for name in ["manifest.json", "verdict-set.json", "report.md"] {
            let _ = std::fs::remove_file(run_dir.join(name));
        }
    }

    /// Flight 1 of the resume pair: a deck-backed mock flight to
    /// completion, then the SIGKILL artifact state. Returns the
    /// (config, run_dir).
    async fn resume_flight(dir: &std::path::Path) -> (RunConfig, PathBuf) {
        let run_dir = dir.join("runs").join("dr-resume-flight");
        let mut cfg = demo_config(run_dir.clone());
        cfg.web_backend = super::gym::MockBackendImpl::BACKEND_ID.to_string();
        cfg.web_search_allowance = 40;
        cfg.web_fetch_allowance = 40;
        cfg.run_id = "dr-resume-flight".to_string();
        cfg.question = "How did income inequality change in American cities?".to_string();
        run(
            cfg.clone(),
            Arc::new(resume_deck_port()),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("flight 1 completes");
        simulate_kill(&run_dir);
        (cfg, run_dir)
    }

    #[tokio::test]
    async fn checkpoint_round_trips_with_the_invariant() {
        let dir = std::env::temp_dir().join(format!("dr-cp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let run_dir = dir.join("runs").join("dr-cp");
        let mut cfg = demo_config(run_dir.clone());
        cfg.web_backend = super::gym::MockBackendImpl::BACKEND_ID.to_string();
        cfg.web_search_allowance = 40;
        cfg.web_fetch_allowance = 40;
        cfg.run_id = "dr-cp".to_string();
        cfg.question = "How did income inequality change in American cities?".to_string();
        run(
            cfg.clone(),
            Arc::new(resume_deck_port()),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("flight completes");

        let cp = read_checkpoint(&run_dir).expect("checkpoint readable after a completed flight");
        assert_eq!(cp.icd, "run_checkpoint");
        assert_eq!(cp.version, super::icd::ICD_VERSION);
        assert_eq!(
            cp.written_after_round as usize,
            cp.rounds.len(),
            "the checkpoint invariant: written_after_round == rounds.len()"
        );
        assert_eq!(cp.run_id, "dr-cp");
        assert_eq!(cp.config.run_id, cfg.run_id);
        assert_eq!(cp.config.question, cfg.question);
        assert_eq!(cp.config.max_rounds, cfg.max_rounds);
        // The manifest's round rows are one-per-round — the tail finish
        // never duplicates the final round (measured: a high-budget
        // 3-round flight recorded [1,2,3,3] before the guard).
        let manifest: super::icd::Manifest = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join("manifest.json")).expect("manifest exists"),
        )
        .expect("manifest parses");
        let rounds: Vec<u32> = manifest.rounds.iter().map(|r| r.round).collect();
        assert_eq!(rounds, (1..=rounds.len() as u32).collect::<Vec<u32>>());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn resume_continues_at_n_plus_one_with_ledger_continuity() {
        let dir = std::env::temp_dir().join(format!("dr-resume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (cfg, run_dir) = resume_flight(&dir).await;
        let cp1 = read_checkpoint(&run_dir).expect("flight-1 checkpoint");
        let ledger1: super::icd::BudgetLedger = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join("budget-ledger.json")).expect("ledger 1"),
        )
        .expect("ledger 1 parses");

        let outcome2 = resume(
            cfg.clone(),
            Arc::new(resume_deck_port()),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("resume drives to a terminal state");
        let ledger2: super::icd::BudgetLedger = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join("budget-ledger.json")).expect("ledger 2"),
        )
        .expect("ledger 2 parses");

        // Ledger continuity — the double-spend discriminator: a restart
        // would RE-CREATE the ledger (a new decider journals a fresh
        // empty one); a resume replays the entries and appends. Spent
        // never decreases, remaining never increases.
        assert_eq!(ledger2.run_id, ledger1.run_id);
        assert_eq!(ledger2.allowance, ledger1.allowance);
        assert!(
            ledger2.entries.len() >= ledger1.entries.len(),
            "the restored ledger must carry flight-1's journal entries — a restart would reset them"
        );
        for (meter, spent1) in &ledger1.spent {
            assert!(
                ledger2.spent.get(meter).copied().unwrap_or(0) >= *spent1,
                "no spend is ever forgotten across a resume ({meter})"
            );
        }
        for (meter, remaining1) in &ledger1.remaining {
            assert!(
                ledger2.remaining.get(meter).copied().unwrap_or(0) <= *remaining1,
                "no allowance is ever re-minted across a resume ({meter})"
            );
        }

        // Rounds advance, never duplicate, never skip.
        let rounds = &outcome2.manifest.rounds;
        assert!(!rounds.is_empty(), "the resumed run records its rounds");
        let mut seen = std::collections::BTreeSet::new();
        for (i, r) in rounds.iter().enumerate() {
            assert_eq!(
                r.round as usize,
                i + 1,
                "rounds stay contiguous — no re-run, no skip"
            );
            assert!(seen.insert(r.round), "no round is ever executed twice");
        }
        assert!(
            rounds.len() >= cp1.rounds.len(),
            "the resumed run adds rounds, never forgets them"
        );
        // The checkpoint advanced (or the run finished the tail) and
        // the invariant holds on the restored checkpoint too.
        let cp2 = read_checkpoint(&run_dir).expect("checkpoint readable after resume");
        assert_eq!(cp2.written_after_round as usize, cp2.rounds.len());
        assert!(
            cp2.written_after_round >= cp1.written_after_round,
            "the resume never rewinds the round count"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn resume_refuses_a_terminal_run() {
        let dir = std::env::temp_dir().join(format!("dr-resume-term-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let run_dir = dir.join("runs").join("dr-resume-term");
        let mut cfg = demo_config(run_dir.clone());
        cfg.web_backend = super::gym::MockBackendImpl::BACKEND_ID.to_string();
        cfg.run_id = "dr-resume-term".to_string();
        run(
            cfg.clone(),
            Arc::new(resume_deck_port()),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("flight completes (manifest present)");
        let e = resume(
            cfg.clone(),
            Arc::new(resume_deck_port()),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect_err("a completed run is terminal — resume must refuse");
        assert!(e.contains("already closed"), "{e}");
        assert!(e.contains("terminal state"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn resume_refuses_a_mismatched_config() {
        let dir = std::env::temp_dir().join(format!("dr-resume-mismatch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (cfg, _run_dir) = resume_flight(&dir).await;
        let mut tampered = cfg.clone();
        tampered.question = "A different question entirely?".to_string();
        let e = resume(
            tampered,
            Arc::new(resume_deck_port()),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect_err("a modified config must refuse");
        assert!(e.contains("resume mismatch"), "{e}");
        assert!(e.contains("question"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn resume_refuses_a_tampered_checkpoint() {
        let dir = std::env::temp_dir().join(format!("dr-resume-tamper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (cfg, run_dir) = resume_flight(&dir).await;

        // A checkpoint whose charter_hash no longer matches its own
        // config refuses (FR-3: the charter derives from the config —
        // a modified checkpoint is never silently restored).
        let path = run_dir.join("checkpoint.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("checkpoint on disk"))
                .expect("checkpoint parses");
        v["charter_hash"] = serde_json::json!("deadbeef");
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).expect("tamper written");
        let e = resume(
            cfg.clone(),
            Arc::new(resume_deck_port()),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect_err("a tampered charter_hash must refuse");
        assert!(e.contains("tampered"), "{e}");

        // A checkpoint whose round count disagrees with its own rounds
        // list refuses at the ENVELOPE (read_checkpoint's invariant) —
        // fresh flight in a fresh dir for an honest checkpoint.
        let dir2 = std::env::temp_dir().join(format!("dr-resume-tamper2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir2);
        let (_cfg2, run_dir2) = resume_flight(&dir2).await;
        let path2 = run_dir2.join("checkpoint.json");
        let mut v2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path2).expect("checkpoint 2 on disk"))
                .expect("checkpoint 2 parses");
        v2["written_after_round"] = serde_json::json!(7); // 7 rounds recorded? no — 3
        std::fs::write(&path2, serde_json::to_string(&v2).unwrap()).expect("tamper 2 written");
        let e = read_checkpoint(&run_dir2).expect_err("an inconsistent checkpoint must refuse");
        assert!(e.contains("inconsistent"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// The regression pin for the measured red dr-1786979612 (order
    /// deep-research-t3a): the launch-time charter hash used to include
    /// `created_at_unix`, so a resume a second later rebuilt the charter
    /// with a fresh timestamp, the hash differed, and an HONEST resume
    /// refused as "tampered" (the demo flight measured it; the unit
    /// tests were only passing because their flights were same-second
    /// fast). The identity hash must be a pure function of the config.
    #[test]
    fn charter_hash_is_time_independent() {
        let mut cfg = demo_config(std::env::temp_dir().join("dr-hash-time"));
        cfg.question = "How did income inequality change in American cities?".to_string();
        let first = hash_charter(&build_charter(&cfg));
        std::thread::sleep(std::time::Duration::from_secs(2));
        let second = hash_charter(&build_charter(&cfg));
        assert_eq!(
            first, second,
            "the charter hash changed across a 2s gap — a wall-clock field \
             (created_at_unix) leaks into the identity hash"
        );
    }

    fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let target = dst.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_dir(&entry.path(), &target)?;
            } else {
                std::fs::copy(entry.path(), target)?;
            }
        }
        Ok(())
    }

    /// The regression pin for the measured red (order
    /// deep-research-t3a): `--resume <dir>` must anchor the run at the
    /// NAMED dir. A faithful copy of a killed run dir resumes into the
    /// COPY (its own checkpoint read, its own state written, the
    /// ORIGINAL untouched); a tampered copy refuses at the hash before
    /// any state write. Before the fix the core anchored on
    /// cp.config.run_dir (the LAUNCH dir): a copy's resume resumed and
    /// closed the ORIGINAL run, and a tampered copy's deadbeef
    /// checkpoint was never even read.
    #[tokio::test]
    async fn resume_of_a_copy_anchors_at_the_named_dir() {
        let dir = std::env::temp_dir().join(format!("dr-resume-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (cfg, run_dir) = resume_flight(&dir).await;

        // A faithful copy: same killed shape, same charter_hash.
        let copy = dir.join("runs").join("dr-resume-copy");
        copy_dir(&run_dir, &copy).expect("run dir copied");
        let mut cfg_copy = cfg.clone();
        cfg_copy.run_dir = copy.clone();

        // The copy resumes and closes IN THE COPY; the original stays
        // in the killed shape (this is the anchor property).
        resume(
            cfg_copy,
            Arc::new(resume_deck_port()),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("a faithful copy resumes");
        assert!(copy.join("manifest.json").exists(), "the COPY closed");
        assert!(
            !run_dir.join("manifest.json").exists(),
            "the ORIGINAL must stay untouched — the resume anchored at the named dir, \
             not at the checkpoint's launch dir"
        );

        // A TAMPERED copy refuses at the hash before any state write.
        let tampered = dir.join("runs").join("dr-resume-copy-tampered");
        copy_dir(&run_dir, &tampered).expect("tampered copy made");
        let path = tampered.join("checkpoint.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["charter_hash"] = serde_json::json!("deadbeef");
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        let mut cfg_tampered = cfg;
        cfg_tampered.run_dir = tampered.clone();
        let e = resume(
            cfg_tampered,
            Arc::new(resume_deck_port()),
            Arc::new(NoProvider),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect_err("a tampered copy must refuse");
        assert!(e.contains("tampered"), "{e}");
        assert!(
            !tampered.join("manifest.json").exists(),
            "a refused resume writes no state"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
