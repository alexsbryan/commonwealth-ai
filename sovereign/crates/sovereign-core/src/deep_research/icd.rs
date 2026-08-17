// SPDX-License-Identifier: AGPL-3.0-or-later
//! ICD artifact types — the boundary contracts.
//!
//! Every inter-component payload is a serialized, versioned artifact in
//! the run directory (FR-2). The field-level shapes are the contract
//! recorded in `research/deep-research/notes/icd-schemas.md`; this
//! module implements them verbatim, and `golden/` holds one fixture per
//! boundary as the qualification surface.
//!
//! Every artifact is `{icd, version}` + body. A parser that meets an
//! unknown `icd` or an unsupported `version` **refuses** — never
//! silently skips (§18.3).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The one supported artifact version (icd-schemas.md §0).
pub const ICD_VERSION: u32 = 1;

/// The four gate verdicts (§18.1) — never defaulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Passed,
    Failed,
    CouldNotJudge,
    NeverRan,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Passed => "passed",
            Verdict::Failed => "failed",
            Verdict::CouldNotJudge => "could-not-judge",
            Verdict::NeverRan => "never-ran",
        }
    }

    pub fn parse_wire(s: &str) -> Option<Verdict> {
        match s {
            "passed" => Some(Verdict::Passed),
            "failed" => Some(Verdict::Failed),
            "could-not-judge" => Some(Verdict::CouldNotJudge),
            "never-ran" => Some(Verdict::NeverRan),
            _ => None,
        }
    }
}

/// The gate action family from the custody reds: a claim resting on
/// unknown-provenance evidence must take a `refused_*` action (R-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateAction {
    CitationGrounded,
    AbstainedDecline,
    RewriteAnnotated,
    RefusedUnknownProvenance,
    /// GAP-2 — the corroboration floor (F22) capped the claim: its
    /// support set spans <2 distinct provenance origins. A cap, not a
    /// refusal — deliberately outside `is_refusal` (the R-3 reds stay
    /// `abstained_*` / `refused_*` only).
    CorroborationFloor,
}

impl GateAction {
    pub fn as_str(self) -> &'static str {
        match self {
            GateAction::CitationGrounded => "citation_grounded",
            GateAction::AbstainedDecline => "abstained_decline",
            GateAction::RewriteAnnotated => "rewrite_annotated",
            GateAction::RefusedUnknownProvenance => "refused_unknown_provenance",
            GateAction::CorroborationFloor => "corroboration_floor",
        }
    }

    /// The refusal family the R-3 red asserts on
    /// (`abstained_*` / `refused_*`).
    pub fn is_refusal(self) -> bool {
        matches!(
            self,
            GateAction::AbstainedDecline | GateAction::RefusedUnknownProvenance
        )
    }
}

// ---------------------------------------------------------------------------
// The envelope: {icd, version} + body. Unknown icd/version refuses.
// ---------------------------------------------------------------------------

/// The parsed form of any ICD artifact (the closed set of boundaries).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Artifact {
    Charter(Charter),
    Plan(Plan),
    Survey(Survey),
    GapList(GapList),
    FetchList(FetchList),
    SkipLedger(SkipLedger),
    BudgetLedger(BudgetLedger),
    EvidenceWindow(EvidenceWindow),
    Draft(Draft),
    VerdictSet(VerdictSet),
    Reframe(ReframeRecord),
    /// STEER 2 (directive 3c5d8b53): the pre-acquisition alignment
    /// record (alignment-1.json) — a question redirect at the
    /// alignment gate, the question-stewardship sibling of the
    /// re-frame.
    Alignment(AlignmentRecord),
    Manifest(Manifest),
}

impl Artifact {
    /// Parse + validate. Unknown `icd` or unsupported `version` refuses
    /// (never silently skips).
    pub fn parse(json: &str) -> Result<Artifact, String> {
        let envelope: Envelope =
            serde_json::from_str(json).map_err(|e| format!("artifact is not valid JSON: {e}"))?;
        if envelope.version != ICD_VERSION {
            return Err(format!(
                "unsupported icd version {} (this build supports {})",
                envelope.version, ICD_VERSION
            ));
        }
        match envelope.icd.as_str() {
            "charter" => Ok(Artifact::Charter(parse_body(json)?)),
            "plan" => Ok(Artifact::Plan(parse_body(json)?)),
            "survey" => Ok(Artifact::Survey(parse_body(json)?)),
            "gap_list" => Ok(Artifact::GapList(parse_body(json)?)),
            "fetch_list" => Ok(Artifact::FetchList(parse_body(json)?)),
            "skip_ledger" => Ok(Artifact::SkipLedger(parse_body(json)?)),
            "budget_ledger" => Ok(Artifact::BudgetLedger(parse_body(json)?)),
            "evidence_window" => Ok(Artifact::EvidenceWindow(parse_body(json)?)),
            "draft" => Ok(Artifact::Draft(parse_body(json)?)),
            "reframe" => Ok(Artifact::Reframe(parse_body(json)?)),
            "alignment" => Ok(Artifact::Alignment(parse_body(json)?)),
            "verdict_set" => Ok(Artifact::VerdictSet(parse_body(json)?)),
            "manifest" => Ok(Artifact::Manifest(parse_body(json)?)),
            other => Err(format!("unknown icd boundary: {other:?}")),
        }
    }

    pub fn icd_name(&self) -> &'static str {
        match self {
            Artifact::Charter(_) => "charter",
            Artifact::Plan(_) => "plan",
            Artifact::Survey(_) => "survey",
            Artifact::GapList(_) => "gap_list",
            Artifact::FetchList(_) => "fetch_list",
            Artifact::SkipLedger(_) => "skip_ledger",
            Artifact::BudgetLedger(_) => "budget_ledger",
            Artifact::EvidenceWindow(_) => "evidence_window",
            Artifact::Draft(_) => "draft",
            Artifact::VerdictSet(_) => "verdict_set",
            Artifact::Reframe(_) => "reframe",
            Artifact::Alignment(_) => "alignment",
            Artifact::Manifest(_) => "manifest",
        }
    }
}

fn parse_body<T: for<'de> Deserialize<'de>>(json: &str) -> Result<T, String> {
    serde_json::from_str(json).map_err(|e| format!("artifact body invalid: {e}"))
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    icd: String,
    version: u32,
}

// ---------------------------------------------------------------------------
// §1 charter.json — frozen at launch (FR-3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Charter {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub question: String,
    #[serde(default)]
    pub seed_id: Option<String>,
    pub created_at_unix: i64,
    pub charter: CharterValues,
    pub frozen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharterValues {
    pub max_rounds: u32,
    pub evidence_window_max_chunks: usize,
    pub containment: ContainmentConfig,
    pub triage: TriageConfig,
    pub budget: BudgetAllowance,
    pub custody: CustodyPolicy,
    pub url_constraint: UrlConstraintPolicy,
    /// The run's consent grant, frozen into the charter at launch
    /// (order deep-research-t2a, Instrument 2 — FR-3). Absent when the
    /// operator gave no `--consent` (default-deny): the web leg then
    /// refuses non-public-web payloads. Serialized only when present —
    /// a no-grant run's charter is byte-identical to the pre-t2a
    /// shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consent: Option<crate::egress::ConsentGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainmentConfig {
    /// The witness fires on judge-supported claims only (gate-redesign.md §2).
    pub trigger: String,
    pub extraction_max_tokens: u32,
    pub specifics_max: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageConfig {
    pub code_set_k: usize,
    pub eps_quota: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetAllowance {
    pub web_search_queries: u32,
    pub web_fetch_pages: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustodyPolicy {
    pub stamp_required: bool,
    pub unknown_refuses: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UrlConstraintPolicy {
    pub enabled: bool,
    pub layer: String,
}

// ---------------------------------------------------------------------------
// §2 plan.json
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub charter_hash: String,
    pub rounds_planned: u32,
    pub estate_first: bool,
    pub network_after_estate: bool,
    pub acquisition: AcquisitionPlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcquisitionPlan {
    pub queries_preplanned: Vec<String>,
    pub source: String,
    /// The question's own figure specifiers (t1e — glassbox): the
    /// answer to "what measures and numbers does this question imply?"
    /// read from the question's own text (its digit runs + its
    /// measure-family words). Recorded so the plan artifact shows the
    /// figure-hunting shape the frontier was checked against; the
    /// scorer measures figure-token presence in the plan from this.
    /// Empty on artifacts that predate the field (additive).
    #[serde(default)]
    pub figure_specifiers: Vec<String>,
}

// ---------------------------------------------------------------------------
// §3 survey-<round>.json — R2 estate survey
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Survey {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub charter_hash: String,
    pub round: u32,
    pub estate_precondition: EstatePrecondition,
    pub estate_corpora: Vec<CorpusEntry>,
    pub searched: Vec<SurveyQuery>,
    #[serde(default)]
    pub estate_answer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EstatePrecondition {
    /// F16 assert: the estate was asked and is searchable before any
    /// network call. The loop refuses R4 while `asserted` is false.
    pub asserted: bool,
    pub estate_searchable: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusEntry {
    pub corpus_id: String,
    pub kind: String,
    pub chunks_count: i64,
    pub searchable: bool,
    #[serde(default)]
    pub custody: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurveyQuery {
    pub query: String,
    #[serde(default)]
    pub hits: Vec<SurveyHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurveyHit {
    pub chunk_id: String,
    pub corpus_id: String,
    pub score: f64,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub custody: Option<String>,
    /// The chunk's content as the estate returned it (the round-1
    /// drafting window is built from these snippets).
    #[serde(default)]
    pub snippet: String,
    /// The chunk's BODY as the estate returned it (t1h — the estate
    /// window's drafting surface prefers the body over the term-
    /// centered snippet cut; None on artifacts predating the field).
    #[serde(default)]
    pub content: Option<String>,
}

// ---------------------------------------------------------------------------
// §4 gap-list-<round>.json — R3, the compass output
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapList {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub charter_hash: String,
    pub round: u32,
    pub claims: Vec<ClaimVerdict>,
    pub gaps: Vec<Gap>,
    #[serde(default)]
    pub empty_evidence_windows: Vec<EmptyWindow>,
    pub strict_subset_of_prior: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimVerdict {
    pub id: String,
    pub text: String,
    pub verdict: Verdict,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub witness: WitnessRecord,
    pub action: GateAction,
    #[serde(default)]
    pub empty_evidence_window: bool,
    /// GAP-2 — the corroboration floor's record, when the claim reached
    /// the floor (the gate's own accounting, verdict-visible).
    #[serde(default)]
    pub corroboration: Option<CorroborationRecord>,
}

/// GAP-2 — the corroboration floor's verdict-visible record (FMEA F22,
/// the two-source rule). C-class: `origins` are the distinct source_urls
/// among the supporting chunks, counted never chunked. The record is on
/// every claim that reached the floor — both the cap and the pass carry
/// it, so `passes_floor` is the gate's own answer, never a default.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct CorroborationRecord {
    pub origins: Vec<String>,
    pub support_chunks: usize,
    pub floor: usize,
    pub passes_floor: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WitnessRecord {
    #[serde(default)]
    pub ran: bool,
    #[serde(default)]
    pub specifics: Vec<String>,
    #[serde(default)]
    pub all_absent: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gap {
    pub id: String,
    pub text: String,
    pub actionable_query: String,
    #[serde(default)]
    pub from_claim_id: Option<String>,
    /// t1d fix 3 (second-origin): the corroboration record when the
    /// floor capped the claim — the gap's query is then a FACT query
    /// (the claim's figures + content words, not the prose cut), so
    /// the next round targets the capped claim's missing origin. The
    /// record rides the gap into the fetch list (icd-schemas.md §4).
    #[serde(default)]
    pub corroboration: Option<CorroborationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmptyWindow {
    pub claim_id: String,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// §5 fetch-list-<round>.json — R4 + R5
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchList {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub charter_hash: String,
    pub round: u32,
    pub queries: Vec<FormedQuery>,
    pub search_hits: Vec<SearchHit>,
    pub triage: TriageOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormedQuery {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub from_gap_id: Option<String>,
    pub formed_by: String,
    pub provider: String,
    /// t1d fix 3 (second-origin): the floor's corroboration record
    /// when the query targets a floor-capped claim's missing origin —
    /// the fetch list is self-describing (why this query, and which
    /// origins the floor counted). Absent for preplanned and
    /// non-capped queries.
    #[serde(default)]
    pub corroboration: Option<CorroborationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub id: String,
    pub query_id: String,
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub snippet: String,
    /// The hit's BODY as the surface returned it (t1h — the corpus
    /// leg's triage boundary: titles are digit-free document names and
    /// snippets are term-centered 600-char cuts, so the body is the
    /// figure-bearing decider's only view of the digits). None on web
    /// hits and on artifacts predating the field (additive, never a
    /// schema break).
    #[serde(default)]
    pub content: Option<String>,
    /// The source that produced the hit — the closed set
    /// `mock` | `corpus` (the acquisition source dispatch, t1g rung 2;
    /// the web-leg backends previously recorded here are `mock`'s id).
    pub engine: String,
    /// The backend's relevance score — the triage ranker's input
    /// (R5: ranker, never excluder).
    #[serde(default)]
    pub score: f64,
    /// The port's custody stamp, carried through to the window chunk
    /// (t1g rung 2: an estate hit stays `personal`, never re-stamped
    /// public-web at fetch). Empty on artifacts predating the field.
    #[serde(default)]
    pub custody: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageOutcome {
    pub code_set_k: Vec<String>,
    pub eps_admits: Vec<String>,
    pub below_cut: Vec<String>,
    pub threshold: f64,
    pub eps_quota: f64,
    /// The admission rule that ranked the round's hits (t1e): the one
    /// decider's name, recorded on the artifact — "score-then-figure-
    /// bearing" (ties break on figure-bearing-ness before insertion
    /// order, so the K-cut does not silently exclude the hits the
    /// figures live in). Defaults to the legacy name on artifacts that
    /// predate the rule (additive, never a schema break).
    #[serde(default = "default_admission_rule")]
    pub admission_rule: String,
}

fn default_admission_rule() -> String {
    "score-then-insertion".to_string()
}

// ---------------------------------------------------------------------------
// §6 skip-ledger-<round>.json — F25
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkipLedger {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub charter_hash: String,
    pub round: u32,
    pub entries: Vec<SkipEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkipEntry {
    pub url: String,
    pub title: String,
    pub score: f64,
    pub rank: usize,
    pub reason: String,
    pub decision: String,
}

// ---------------------------------------------------------------------------
// §7 budget-ledger.json — the one decider's journal
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetLedger {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub charter_hash: String,
    pub allowance: HashMap<String, u32>,
    pub entries: Vec<BudgetEntry>,
    pub spent: HashMap<String, u32>,
    pub remaining: HashMap<String, u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetEntry {
    pub family: String,
    pub key: String,
    pub units: u32,
    pub at_unix: i64,
    pub decision: String,
    #[serde(default)]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// §8 evidence-window-<round>.json — R6, custody-stamped
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceWindow {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub charter_hash: String,
    pub round: u32,
    pub chunks: Vec<WindowChunk>,
    #[serde(default)]
    pub fetch_failures: Vec<FetchFailure>,
    /// URLs refused as already-fetched (order deep-research-t1d fix 1:
    /// a round-2 fetch of an already-fetched URL is refused). Refusals
    /// are NOT fetch failures — the source was acquired (a prior round
    /// or earlier this round); they are the dedup record, and refused
    /// fetches spend no budget.
    #[serde(default)]
    pub dedup_refused: Vec<String>,
    pub derived_custody: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowChunk {
    pub id: String,
    pub locator: String,
    pub source_url: String,
    pub custody: String,
    pub provenance_class: String,
    pub content: String,
    #[serde(default)]
    pub ingested_into: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchFailure {
    pub url: String,
    pub error: String,
    pub absent: bool,
}

// ---------------------------------------------------------------------------
// §9 draft-<round>.json — R8
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub charter_hash: String,
    pub round: u32,
    pub provider: String,
    pub url_constraint: UrlConstraintPolicy,
    pub text: String,
    #[serde(default)]
    pub citations: Vec<DraftCitation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftCitation {
    pub evidence_id: String,
    pub url: String,
    #[serde(default)]
    pub custody: Option<String>,
}

// ---------------------------------------------------------------------------
// §10 verdict-set.json — R9 claim splitter
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerdictSet {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub charter_hash: String,
    pub claims: Vec<FinalClaim>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalClaim {
    pub id: String,
    pub text: String,
    pub verdict: Verdict,
    pub status: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub citations: Vec<ClaimCitation>,
    #[serde(default)]
    pub flag: Option<String>,
    /// GAP-2 — the gate's corroboration record, verdict-visible on the
    /// final claim (spec: "the gate's corroboration record").
    #[serde(default)]
    pub corroboration: Option<CorroborationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimCitation {
    pub evidence_id: String,
    pub url: String,
    pub chunk_id: String,
}

// ---------------------------------------------------------------------------
// §12 manifest.json — run close
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub charter_hash: String,
    pub terminal_state: String,
    #[serde(default)]
    pub aborted_at_round: Option<u32>,
    pub truncation_declared: bool,
    #[serde(default)]
    pub rounds: Vec<RoundRow>,
    pub sources: SourceLedger,
    pub budget: BudgetTotals,
    #[serde(default)]
    pub not_covered: Vec<String>,
    /// GAP-4: the reframe record, when the run re-framed its question
    /// (structural surprise). Absent on a run that never re-framed.
    #[serde(default)]
    pub reframe: Option<ReframeRecord>,
    /// STEER 2: the alignment record, when the pre-acquisition gate
    /// redirected the question. Absent on a run that aligned (or that
    /// never consulted a port that redirects).
    #[serde(default)]
    pub alignment: Option<AlignmentRecord>,
    /// GAP-3: the epistemic residue — every query the loop executed and
    /// that returned no evidence, as report content ("we looked for X
    /// and found no evidence either way"). Empty on a run where every
    /// search found something (the section then renders nothing).
    #[serde(default)]
    pub residue: Vec<ResidueRow>,
    /// The run-scoped consent grant (order deep-research-t2a) — the
    /// manifest record of the operator's typed release for this run
    /// (default-deny: absent on a run that released nothing).
    #[serde(default)]
    pub consent: Option<crate::egress::ConsentGrant>,
    pub lock: LockRecord,
}

/// GAP-3: one searched-but-absent row — a query the loop executed that
/// returned zero results. The report renders the residue as a first-
/// class section; publication-bias awareness, generalizing the
/// manifest's "what was NOT covered" to named queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResidueRow {
    /// The query as the loop executed it.
    pub query: String,
    /// The acquire round that searched it.
    pub round: u32,
}

/// GAP-4: the staged re-frame input — `<run_dir>/reframe-input.json`,
/// written by the launcher (the CLI's `--reframe`) BEFORE the run
/// opens the dir. The loop reads it at start; a malformed input
/// refuses the run loudly. The question is the operator's re-framing;
/// the reason is theirs to state (both recorded in reframe-1.json).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReframeInput {
    pub question: String,
    #[serde(default)]
    pub reason: String,
}

/// GAP-4: the reframe record written at the Reframing state
/// (`reframe-1.json`) and carried on the manifest — the typed
/// hermeneutic move, on the record: when it fired, what the question
/// became, and why (the operator's stated reason).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReframeRecord {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub charter_hash: String,
    /// The round whose audit fired the trigger.
    pub round: u32,
    pub original_question: String,
    pub reframed_question: String,
    pub reason: String,
    /// Why the loop decided the surprise was structural: the last
    /// acquire round fetched nothing AND the gap list was unchanged.
    pub trigger: String,
}

/// STEER 2 (directive 3c5d8b53): the alignment record written at the
/// Align state (`alignment-1.json`) and carried on the manifest — the
/// pre-acquisition redirect, on the record: what the question was,
/// what it became, and why (the operator's stated reason). The
/// question-stewardship sibling of `ReframeRecord` — same shape,
/// distinct move (pre-run operator redirect vs mid-run structural
/// surprise). `round` is always 0: the redirect fires before any
/// acquisition round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlignmentRecord {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub charter_hash: String,
    /// Always 0 — the alignment gate fires before any acquisition.
    pub round: u32,
    pub original_question: String,
    pub redirected_question: String,
    pub reason: String,
    /// Why the gate decided the redirect: the operator's call at the
    /// pre-acquisition alignment.
    pub trigger: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundRow {
    pub round: u32,
    pub gaps_before: usize,
    pub gaps_after: usize,
    pub fetched: usize,
    pub search_calls: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceLedger {
    #[serde(default)]
    pub fetched: Vec<FetchedSource>,
    #[serde(default)]
    pub failed: Vec<FailedSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchedSource {
    pub url: String,
    pub custody: String,
    #[serde(default)]
    pub ingested_into: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedSource {
    pub url: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetTotals {
    pub spent: HashMap<String, u32>,
    pub remaining: HashMap<String, u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockRecord {
    pub id: String,
    pub acquired_at_unix: i64,
    #[serde(default)]
    pub released_at_unix: Option<i64>,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

impl Charter {
    pub fn validate(&self) -> Result<(), String> {
        if self.icd != "charter" || self.version != ICD_VERSION {
            return Err(format!(
                "charter envelope mismatch: {}/{}",
                self.icd, self.version
            ));
        }
        if !self.frozen {
            return Err("charter must be frozen at launch (FR-3)".to_string());
        }
        if self.charter.max_rounds == 0 {
            return Err("charter max_rounds must be ≥ 1".to_string());
        }
        if self.charter.containment.trigger != "judge-supported" {
            return Err(format!(
                "containment trigger must be \"judge-supported\", got {:?}",
                self.charter.containment.trigger
            ));
        }
        if self.charter.custody.unknown_refuses == false && self.charter.custody.stamp_required {
            // stamp_required + unknown_refuses is the fail-closed posture; a
            // charter that requires stamps but does not refuse unknowns is
            // incoherent.
            return Err(
                "custody policy incoherent: stamps required but unknowns do not refuse".to_string(),
            );
        }
        if !(0.0..=1.0).contains(&self.charter.triage.eps_quota) {
            return Err("eps_quota must be in [0, 1]".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_wire_spellings() {
        assert_eq!(Verdict::Passed.as_str(), "passed");
        assert_eq!(Verdict::CouldNotJudge.as_str(), "could-not-judge");
        assert_eq!(
            Verdict::parse_wire("could-not-judge"),
            Some(Verdict::CouldNotJudge)
        );
        assert_eq!(Verdict::parse_wire("passes"), None);
    }

    #[test]
    fn gate_action_refusal_family() {
        assert!(GateAction::AbstainedDecline.is_refusal());
        assert!(GateAction::RefusedUnknownProvenance.is_refusal());
        assert!(!GateAction::CitationGrounded.is_refusal());
        assert!(GateAction::RefusedUnknownProvenance
            .as_str()
            .starts_with("refused"));
    }
}
