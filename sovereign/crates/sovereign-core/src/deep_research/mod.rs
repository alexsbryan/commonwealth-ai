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
    AcquisitionPlan, BudgetTotals, Charter, CharterValues, CustodyPolicy, Draft, EvidenceWindow,
    FailedSource, FetchFailure, FetchList, FetchedSource, Gap, GapList, LockRecord, Manifest, Plan,
    RoundRow, SourceLedger, Survey, TriageConfig, UrlConstraintPolicy, WindowChunk,
};
use render::{build_manifest, final_claims, not_covered, render_report, ManifestInput};
use state::{Event, RunLock, State};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::oicp::ShardingPrivacy;
use crate::traits::InferenceProvider;

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

/// The deterministic gap→query template (the audit's `query_for`).
fn template_query(claim: &str) -> String {
    let stripped = strip_citation_spans(claim);
    stripped.trim().chars().take(140).collect()
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
            windows: Vec::new(),
            prior_gap_texts: Vec::new(),
            prior_gaps: Vec::new(),
            web_refused: false,
            window_capped: false,
            search_calls: 0,
            rounds: Vec::new(),
            fetched_sources: Vec::new(),
            failed_sources: Vec::new(),
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
                    content: hit.snippet.clone(),
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
            &self.config.question,
            &template_query,
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
            self.config.question,
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

        // Planning: the plan ICD.
        if aborted(&self.abort) {
            self.aborted_at_round = Some(0);
            return self.land_aborted().await;
        }
        let plan = Plan {
            icd: "plan".to_string(),
            version: icd::ICD_VERSION,
            run_id: self.config.run_id.clone(),
            charter_hash: self.charter_hash.clone(),
            rounds_planned: self.config.max_rounds,
            estate_first: true,
            network_after_estate: true,
            acquisition: AcquisitionPlan {
                queries_preplanned: Vec::new(),
                source: "gap-template".to_string(),
            },
        };
        self.write_artifact("plan.json", &plan)?;
        self.step(Event::PlanWritten)?;

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
                    &self.config.question,
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
                &self.config.question,
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

            let continue_to_web = !self.web_refused
                && self
                    .decider
                    .remaining(FAMILY_WEB_SEARCH, &self.config.web_backend)
                    > 0
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
        let mut fetch_list =
            acquisition::form_queries(&self.config.run_id, &self.charter_hash, round, &gaps);

        // R4 search through the ONE decider (web-search half). A
        // refused query spends nothing and is journaled in the budget
        // ledger — the ledger is the record.
        let mut all_hits = Vec::new();
        for query in &fetch_list.queries {
            let verdict = self
                .decider
                .allow(FAMILY_WEB_SEARCH, &self.config.web_backend, 1, now_unix())
                .await?;
            if !verdict.allowed() {
                continue;
            }
            self.search_calls += 1;
            let hits = self
                .port
                .web_search(&self.config.web_backend, &query.text, 10)
                .await
                .map_err(|e| format!("web search: {e}"))?;
            for h in hits {
                all_hits.push(icd::SearchHit {
                    id: h.id.clone(),
                    query_id: query.id.clone(),
                    url: h.url,
                    title: h.title,
                    snippet: h.snippet,
                    engine: self.config.web_backend.clone(),
                    score: h.score,
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
        // failures recorded absent per-source (F17).
        let mut window = fetch_round(
            self.port.as_ref(),
            &mut self.decider,
            &self.config.run_id,
            &self.charter_hash,
            round,
            &fetch_list,
            &triaged.ranked,
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
        let report = render_report(&self.config.question, &claims, &self.config.run_id);
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
                self.config.question
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
        format!("{FAMILY_WEB_SEARCH}:{}", config.web_backend),
        config.web_search_allowance,
    );
    m.insert(
        format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"),
        config.web_fetch_allowance,
    );
    m
}

pub use estate::ResearchPort;

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
}
