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
pub mod render;
pub mod state;
pub mod synthesize;

use audit::{assess_claim, split_claims, ClaimAudit};
use budget::{SpendDecider, FAMILY_WEB_FETCH, FAMILY_WEB_SEARCH, KEY_FETCH_PAGES};
use containment::{strip_citation_spans, ContainmentConfig};
use fetch::fetch_round;
use icd::{
    AcquisitionPlan, AlignmentRecord, BudgetTotals, Charter, CharterValues, CustodyPolicy, Draft,
    EvidenceWindow, FailedSource, FetchFailure, FetchList, FetchedSource, Gap, GapList, LockRecord,
    Manifest, Plan, ReframeInput, ReframeRecord, ResidueRow, RoundRow, SourceLedger, Survey,
    TriageConfig, UrlConstraintPolicy, WindowChunk,
};
use render::{build_manifest, final_claims, not_covered, render_report, ManifestInput};
use state::{Event, RunLock, State};
use std::path::PathBuf;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSource {
    Mock,
    Corpus,
}

impl SearchSource {
    /// The ONE decider: a `&str` maps onto the closed set or refuses.
    pub fn parse(s: &str) -> Option<SearchSource> {
        match s {
            "mock" => Some(SearchSource::Mock),
            "corpus" => Some(SearchSource::Corpus),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SearchSource::Mock => "mock",
            SearchSource::Corpus => "corpus",
        }
    }
}

/// The budget ledger's key for the acquisition search — ONE decider
/// shared by the allowance map, the continue-to-web gate, and the
/// per-query spend: the source's key (`corpus` for the corpus source,
/// the web backend id for the mock). A second key derivation would let
/// the gate and the spend disagree — the shape that silently ended a
/// corpus-source run before it searched.
fn source_budget_key(source: SearchSource, web_backend: &str) -> String {
    match source {
        SearchSource::Mock => web_backend.to_string(),
        SearchSource::Corpus => "corpus".to_string(),
    }
}

/// Everything a run needs at launch. Values are frozen into the
/// charter at launch (FR-3); nothing here is re-read mid-run.
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub run_id: String,
    pub question: String,
    pub seed_id: Option<String>,
    pub run_dir: PathBuf,
    pub max_rounds: u32,
    pub code_set_k: usize,
    pub eps_quota: f64,
    pub evidence_window_max_chunks: usize,
    pub estate_corpus_ids: Vec<String>,
    pub web_backend: String,
    /// The acquisition search source (t1g rung 2): `Mock` (default) or
    /// `Corpus` — a closed set, decided once at launch.
    pub search_source: SearchSource,
    pub web_search_allowance: u32,
    pub web_fetch_allowance: u32,
    pub posture: ShardingPrivacy,
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
/// stripped, first 140 chars.
fn template_query(claim: &str) -> String {
    let stripped = strip_citation_spans(claim);
    stripped.trim().chars().take(140).collect()
}

/// The one gap→query decider (t1d fix 3 — second-origin; t1e —
/// figure-hunting): when the floor capped the claim (its
/// corroboration record fails the floor), the query is a FACT query —
/// the claim's figures plus its content words — so the next round
/// targets the missing second origin by the fact it must carry. A
/// claim the floor did not cap keeps the prose template — and when
/// that template carries no figure specifier, the question's own
/// specifiers are folded in (t1e: a thematic claim's follow-up query
/// still hunts the figures the question implies; the numbers never
/// silently drop out of the acquisition). Structural, not remembered:
/// the record chooses the floor shape, the specifier presence chooses
/// the fold-in.
fn gap_query_for(
    claim: &str,
    corroboration: Option<&icd::CorroborationRecord>,
    question_specifiers: &[String],
) -> String {
    let floor_capped = corroboration.map(|c| !c.passes_floor).unwrap_or(false);
    if floor_capped {
        fact_query(claim)
    } else {
        acquisition::figure_hunt_query(template_query(claim), question_specifiers)
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
fn fact_query(claim: &str) -> String {
    let stripped = strip_citation_spans(claim);
    let mut parts: Vec<String> = Vec::new();
    for f in figure_tokens(&stripped) {
        if !parts.contains(&f) {
            parts.push(f);
        }
    }
    for w in stripped.split_whitespace() {
        let word = w.trim_matches(|c: char| !c.is_alphanumeric());
        let lower = word.to_ascii_lowercase();
        if word.chars().count() >= 3
            && !is_query_stopword(&lower)
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

/// C-class figure tokens for the fact query: every maximal run of
/// digits plus adjacent ratio/currency punctuation (`$ % . : / ,`),
/// trailing sentence separators trimmed. Deterministic, no model.
fn figure_tokens(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || matches!(chars[i], '$' | '%' | '.' | ':' | '/' | ','))
            {
                i += 1;
            }
            let mut token: String = chars[start..i].iter().collect();
            while token.ends_with(['.', ',']) {
                token.pop();
            }
            out.push(token);
        } else {
            i += 1;
        }
    }
    out
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
    /// The windows accumulated so far (the estate window first).
    windows: Vec<EvidenceWindow>,
    /// The still-open gap claim texts — the strict-subset identity
    /// (stable claim texts re-audited against each new window).
    prior_gap_texts: Vec<String>,
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
        };
        Ok(ctl)
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
        for (i, q) in survey.searched.iter().enumerate() {
            for hit in &q.hits {
                let locator = hit
                    .url
                    .clone()
                    .unwrap_or_else(|| format!("estate:{}:{}", hit.corpus_id, hit.chunk_id));
                chunks.push(WindowChunk {
                    id: format!("estate-{}", i + 1),
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
            derived_custody: custody,
        }
    }

    /// Merge the accumulated windows: dedup by source URL (first wins),
    /// capped at the charter's window cap. Capping is declared — the
    /// flag surfaces in the manifest's `truncation_declared`.
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
        let mut audits = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for claim in split_claims(&draft.text) {
            let key = claim.trim().to_string();
            if seen.insert(key.clone()) {
                audits.push(
                    assess_claim(
                        &self.provider,
                        &key,
                        &chunks,
                        &self.containment,
                        self.config.posture,
                        self.tau,
                    )
                    .await,
                );
            }
        }
        for gap_text in &self.prior_gap_texts {
            if seen.insert(gap_text.clone()) {
                audits.push(
                    assess_claim(
                        &self.provider,
                        gap_text,
                        &chunks,
                        &self.containment,
                        self.config.posture,
                        self.tau,
                    )
                    .await,
                );
            }
        }
        let gap_list = audit::build_gap_list(
            &self.config.run_id,
            &self.charter_hash,
            draft.round,
            &audits,
            &self.prior_gap_texts,
            &self.question,
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
        // Initializing → Planning: the frozen charter (FR-3).
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

        // The round loop.
        let max_rounds = self.config.max_rounds;
        for round in 1..=max_rounds {
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
            let draft = synthesize::draft_round(
                self.port.as_ref(),
                &self.config.run_id,
                &self.charter_hash,
                round,
                &self.question,
                &window,
                &self.prior_gap_texts,
            )
            .await?;
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
        let mut fetch_list = acquisition::form_queries(
            &self.config.run_id,
            &self.charter_hash,
            round,
            &gaps,
            frontier,
        );

        // R4 search through the ONE decider (web-search family). The
        // SOURCE is a closed set decided once at launch (t1g rung 2):
        // Mock — the deck's term-ranked surface; Corpus — the estate's
        // corpus-search surface. Same ledger, same allowance — the
        // protocol is unchanged, only the source routes differently. A
        // refused query spends nothing and is journaled in the budget
        // ledger — the ledger is the record.
        let source_key = source_budget_key(self.config.search_source, &self.config.web_backend);
        let mut all_hits = Vec::new();
        for query in &fetch_list.queries {
            let verdict = self
                .decider
                .allow(FAMILY_WEB_SEARCH, &source_key, 1, now_unix())
                .await?;
            if !verdict.allowed() {
                continue;
            }
            self.search_calls += 1;
            let hits = match self.config.search_source {
                SearchSource::Mock => self
                    .port
                    .web_search(&self.config.web_backend, &query.text, 10)
                    .await
                    .map_err(|e| format!("web search: {e}"))?,
                SearchSource::Corpus => self
                    .port
                    .estate_search(&self.config.estate_corpus_ids, &query.text, 10)
                    .await
                    .map_err(|e| format!("corpus search: {e}"))?,
            };
            if hits.is_empty() {
                // GAP-3: a searched-but-absent query is report content
                // — the residue records it here, at the moment the
                // empty result is known (never reconstructed later
                // from the triage ledger, where the absence is lost).
                self.residue.push(ResidueRow {
                    query: query.text.clone(),
                    round,
                });
            }
            for h in hits {
                all_hits.push(icd::SearchHit {
                    id: h.id.clone(),
                    query_id: query.id.clone(),
                    url: h.url,
                    title: h.title,
                    snippet: h.snippet,
                    // The body carries through (t1h — the triage
                    // decider reads it over the snippet cut).
                    content: h.content,
                    engine: self.config.search_source.as_str().to_string(),
                    score: h.score,
                    custody: h.custody.as_str().to_string(),
                });
            }
        }

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

        // R6 fetch through the decider; custody stamped by code;
        // failures recorded absent per-source (F17). Dedup: the URLs
        // fetched by prior rounds are refused (t1d fix 1 — a round-2
        // fetch of an already-fetched URL is refused, no re-spend).
        let already_fetched: Vec<String> =
            self.fetched_sources.iter().map(|s| s.url.clone()).collect();
        let mut window = fetch_round(
            self.port.as_ref(),
            &mut self.decider,
            &self.config.run_id,
            &self.charter_hash,
            round,
            &fetch_list,
            &triaged.ranked,
            &already_fetched,
            now_unix(),
        )
        .await?;
        self.step(Event::FetchComplete)?; // → Enriching
        for f in &window.fetch_failures {
            self.failed_sources.push(FailedSource {
                url: f.url.clone(),
                error: f.error.clone(),
            });
        }

        // R7 enrich: derived tags + the custody join.
        let titles: Vec<(String, String)> = window
            .chunks
            .iter()
            .map(|c| {
                let t = triaged
                    .ranked
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
        let (audits, _) = self.audit_pass(draft, &window).await?;
        let claims = final_claims(&audits, &window);
        let verdict_set = icd::VerdictSet {
            icd: "verdict_set".to_string(),
            version: icd::ICD_VERSION,
            run_id: self.config.run_id.clone(),
            charter_hash: self.charter_hash.clone(),
            claims: claims.clone(),
        };
        self.write_artifact("verdict-set.json", &verdict_set)?;
        self.prior_gap_texts = not_covered(&claims);
        let report = render_report(
            &self.question,
            &claims,
            &self.config.run_id,
            self.reframe_record.as_ref(),
            self.alignment_record.as_ref(),
            &self.residue,
        );
        let report_path = self.config.run_dir.join("report.md");
        std::fs::write(&report_path, report).map_err(|e| format!("report write: {e}"))?;
        self.artifacts.push("report.md".to_string());

        self.rounds.push(RoundRow {
            round,
            gaps_before,
            gaps_after,
            fetched: window.chunks.len(),
            search_calls: 0,
        });

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
fn build_charter(config: &RunConfig) -> Charter {
    Charter {
        icd: "charter".to_string(),
        version: icd::ICD_VERSION,
        run_id: config.run_id.clone(),
        question: config.question.clone(),
        seed_id: config.seed_id.clone(),
        created_at_unix: now_unix(),
        charter: CharterValues {
            max_rounds: config.max_rounds,
            evidence_window_max_chunks: config.evidence_window_max_chunks,
            containment: icd::ContainmentConfig {
                trigger: "judge-supported".to_string(),
                extraction_max_tokens: 32,
                specifics_max: 4,
            },
            triage: TriageConfig {
                code_set_k: config.code_set_k,
                eps_quota: config.eps_quota,
            },
            budget: icd::BudgetAllowance {
                web_search_queries: config.web_search_allowance,
                web_fetch_pages: config.web_fetch_allowance,
            },
            custody: CustodyPolicy {
                stamp_required: true,
                unknown_refuses: true,
            },
            url_constraint: UrlConstraintPolicy {
                enabled: true,
                layer: "sovereign-inference:UrlAllowlistConstraint".to_string(),
            },
        },
        frozen: true,
    }
}

fn hash_charter(charter: &Charter) -> String {
    let json = serde_json::to_string(charter).unwrap_or_default();
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

    /// A port that answers "nothing" — start() never reaches the
    /// network, so defaults are honest.
    struct NoopPort;
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
        async fn draft(&self, _p: &str, _s: Option<&str>, _a: &[String]) -> Result<String, String> {
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
            evidence_window_max_chunks: 20,
            estate_corpus_ids: Vec::new(),
            web_backend: "duckduckgo".to_string(),
            search_source: SearchSource::Mock,
            web_search_allowance: 4,
            web_fetch_allowance: 4,
            posture: ShardingPrivacy::LocalOnly,
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
            frontier_queries.len(),
            8,
            "round 1 must carry the full acquisition frontier as queries"
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
            format!("{} (1980, 2024, income)", template_query(thematic_claim)),
            "the fold-in appends the question's specifiers to the prose template"
        );
        // A claim that already carries a figure keeps its own shape.
        let figure_claim = "The Gini index in New York reached 0.5469 by 2013.";
        assert_eq!(
            gap_query_for(figure_claim, None, &specs),
            template_query(figure_claim),
            "a figure-bearing claim's query stands as formed"
        );
        // The floor-capped FACT query is unchanged (fix 3).
        let record = icd::CorroborationRecord {
            origins: vec!["https://gym.example/one".to_string()],
            support_chunks: 1,
            floor: 2,
            passes_floor: false,
        };
        assert_eq!(
            gap_query_for(thematic_claim, Some(&record), &specs),
            fact_query(thematic_claim),
            "the floor-capped gap keeps the fact query — its figures ride, not the fold-in"
        );
        // No specifiers on the question → no fold-in anywhere.
        assert_eq!(
            gap_query_for(thematic_claim, None, &[]),
            template_query(thematic_claim),
            "a question with no specifiers folds nothing in"
        );
    }

    /// RED-first (order deep-research-t1d fix 3 — second-origin): when
    /// the floor caps a claim, the next round's gap query must target
    /// the claim's FACT — the figure the second origin must carry —
    /// not the first 140 characters of prose. Watch-it-fail at HEAD:
    /// the figure sits beyond the template's 140-char cut, so the
    /// query misses the very number the floor demanded (the t1c R-12
    /// measurement: 0/12 on v0 single-origin decks — the follow-up
    /// query could never surface the missing second origin).
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
        let gap_list =
            audit::build_gap_list("run", "hash", 2, &[audit], &[], "question?", &|c, corr| {
                gap_query_for(c, corr, &[])
            });
        assert_eq!(gap_list.gaps.len(), 1);
        let gap = &gap_list.gaps[0];
        assert!(
            gap.actionable_query.contains("0.55"),
            "the floor-capped gap's query must carry the claim's figure \
             (beyond the 140-char prose cut) so the next round can target \
             the missing second origin: {:?}",
            gap.actionable_query
        );
        assert!(
            gap.actionable_query.contains("Gini"),
            "the fact query keeps the claim's subject content words: {:?}",
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
        let gap_list =
            audit::build_gap_list("run", "hash", 2, &[plain], &[], "question?", &|c, corr| {
                gap_query_for(c, corr, &[])
            });
        assert_eq!(
            gap_list.gaps[0].actionable_query,
            template_query(&claim),
            "a claim the floor did not cap keeps the prose template"
        );
    }
}
