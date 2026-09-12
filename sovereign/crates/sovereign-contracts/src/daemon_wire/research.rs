// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deep research as a daemon JOB — the wire shapes of `/v1/research`
//! (sv-surface, 2026-09-11).
//!
//! Until this file the loop (`sovereign_core::deep_research::run`) was
//! linked into TWO hosts — the desktop's `dr_start` and the CLI verb — and
//! the daemon served no research route at all. These are the shapes the
//! daemon's `research_http` answers with and every client parses; the
//! desktop re-emits [`ResearchFrame`]s as its Tauri events byte for byte,
//! which is why the frame enum keeps the field names the Svelte store
//! already reads (`kind`, `run_id`, `elapsed_secs`, …).
//!
//! Every type here is serde over primitives. The ICD types the run dir is
//! written in (`deep_research::icd`) stay in `sovereign-core`: a client
//! reads the PROJECTION below, not the artifacts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Body of `POST /v1/research` — the union of what the two hosts pass to
/// `deep_research::launch::LaunchOptions`. The desktop sends the Ask
/// surface's fields (question, budget, consent, corpora, resume); the CLI
/// verb also names the triage knobs, the search source and the mock-deck
/// backend. Everything optional falls to `launch`'s defaults ON THE
/// DAEMON — no client re-decides a runtime default.
///
/// No `runs_base`: the run dir is the daemon's to mint (a client-named
/// directory on the serving process is a path the caller supplies, which
/// is ARCH principle 5's smell). The CLI's `--run-dir` is the one flag
/// this union does not carry, by that rule.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ResearchRequest {
    /// Required for a fresh run; ignored on a resume (the charter's
    /// question is the run's).
    pub question: String,
    pub max_rounds: Option<u32>,
    /// Estate corpus ids consulted BEFORE the web leg.
    pub corpora: Vec<String>,
    /// `"public-web"` | `"peer"` | `"personal"` — the typed release floor,
    /// parsed once on the daemon with `Custody::parse_wire`. Absent is
    /// default-deny.
    pub consent: Option<String>,
    /// Web-search allowance (queries).
    pub search: Option<u32>,
    /// Web-fetch allowance (pages).
    pub fetch: Option<u32>,
    /// Resume an interrupted run by id instead of launching one.
    pub resume_run_id: Option<String>,
    /// Triage: the code-set size. CLI `--code-set-k`.
    pub code_set_k: Option<usize>,
    /// Triage: the epsilon quota. CLI `--eps-quota`.
    pub eps_quota: Option<f64>,
    /// `"mock"` | `"corpus"` | `"web"`. CLI `--search-source`; the desktop
    /// leaves it to the daemon (`corpus`, or `mock` under a mock backend).
    pub search_source: Option<String>,
    /// `"auto"` | `"mock"`. CLI `--backend`; the desktop's demo override.
    pub backend: Option<String>,
    /// The deck directory a `mock` backend serves search/fetch from.
    pub mock_deck_dir: Option<String>,
}

/// Answer of `POST /v1/research` (202). Not [`super::IngestJobAck`]:
/// that ack is keyed by the corpus being ingested, and a research run has
/// no corpus — it has a RUN DIR, which the desktop's `started` event
/// announces, so that is the field this ack carries instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResearchJobAck {
    /// The run id (`dr-<unix>`), which is the job's identity — the run
    /// is what the job IS (ARCH principle 8: identity from essence).
    pub job_id: String,
    /// The run dir `launch::prepare` minted, real on disk when this is
    /// returned.
    pub run_dir: String,
    pub ok: bool,
    /// `GET /v1/research/{job_id}/progress` — always populated.
    pub progress_route: String,
}

/// One named gap from `gap-list-<round>.json` — the gate's compass output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchGap {
    pub id: String,
    pub text: String,
}

/// The budget ledger's spent/remaining, keyed by meter.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchBudget {
    pub spent: BTreeMap<String, u32>,
    pub remaining: BTreeMap<String, u32>,
}

/// The run's typed consent grant as recorded in the charter (absent =
/// default-deny: the web leg refused non-public-web payloads).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchConsent {
    pub release_floor: String,
    pub granted_at_unix: i64,
}

/// One frame of a research job's progress log, tagged on `kind`. The
/// daemon appends `started`, every CHANGED `live` snapshot of the run dir,
/// and one terminal frame (`report_ready` or `failed`). `heartbeat` is
/// never logged — a tick a second for an hour is a log nobody reads —
/// but it is a variant here so a client synthesises it from
/// [`ResearchProgress`]'s clocks with the ONE enum and emits it alongside
/// the logged frames unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResearchFrame {
    /// The run dir exists; polling begins from here.
    Started { run_id: String, run_dir: String },
    /// A changed snapshot of the run dir: current round, the gate's named
    /// gap list, the budget ledger, and the consent-grant status (from
    /// charter.json — live, not the close-time manifest).
    Live {
        round: Option<u32>,
        /// The charter's `max_rounds` — what the round number is OUT OF.
        /// `None` before the charter is readable.
        max_rounds: Option<u32>,
        stage: String,
        gaps: Vec<ResearchGap>,
        budget: ResearchBudget,
        consent: Option<ResearchConsent>,
    },
    /// Still being driven: this long in, this long since anything moved.
    Heartbeat {
        elapsed_secs: i64,
        quiet_secs: i64,
        stage: String,
    },
    /// Terminal: `report.md` exists — the checked report.
    ReportReady { report: ResearchReport },
    /// Terminal: the loop could not run, or ran and left no report.
    Failed { error: String },
}

/// Answer of `GET /v1/research/{job_id}/progress?after=N` — the frames
/// from the caller's cursor on, plus the clocks a heartbeat is made of.
/// Not `ClusterProgressView`: that view is keyed by corpus and carries no
/// clocks, and a research run has neither a corpus nor a frame per tick.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResearchProgress {
    pub job_id: String,
    pub frames: Vec<ResearchFrame>,
    /// The cursor to send next: one past the last frame here.
    pub next: usize,
    /// `true` iff the terminal frame has been appended.
    pub finished: bool,
    /// Seconds since THIS leg started (a resumed run's charter carries the
    /// original birth, which is not what the operator is watching).
    pub elapsed_secs: i64,
    /// Seconds since the run dir last changed — measured, not inferred.
    pub quiet_secs: i64,
    /// The last observed stage; `"planning"` only before anything has ever
    /// been observed.
    pub stage: String,
}

/// Answer of `POST /v1/research/{job_id}/abort`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchAbortAck {
    pub job_id: String,
    /// The job was live and its abort flag is now raised. The loop polls
    /// it at every state entry and lands on a truncated report with the
    /// truncation declared.
    pub aborted: bool,
}

/// Answer of `GET /v1/research/capabilities`. Named affordances, not
/// scraped `--help` tokens; `error` names why research cannot run on this
/// daemon when it cannot (no models configured, say). Absence reported,
/// never defaulted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchCapabilities {
    pub flags: Vec<String>,
    pub error: Option<String>,
}

/// One prior run on the shelf (`GET /v1/research/runs`), read from its
/// run dir's charter (live facts) and manifest (close facts).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResearchRunSummary {
    pub run_id: String,
    pub question: Option<String>,
    pub created_at_unix: Option<i64>,
    /// The manifest's close-time state, or `None` when there is no
    /// manifest. Read WITH `live`: live is "running", absent-and-not-live
    /// is genuinely interrupted — absence is reported, never defaulted.
    pub terminal_state: Option<String>,
    /// Is the daemon driving this run right now? The job registry is the
    /// one decider.
    pub live: bool,
    pub rounds: usize,
    pub report_present: bool,
    pub consent: Option<ResearchConsent>,
}

/// One run the daemon is driving right now (`GET /v1/research/active`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResearchActiveRun {
    pub run_id: String,
    pub question: Option<String>,
    pub started_at_unix: i64,
}

/// The checked report + its verdict dimensions
/// (`GET /v1/research/runs/{run_id}/report`), rendered from the loop's
/// own artifacts — never re-invented.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResearchReport {
    pub run_id: String,
    pub question: String,
    pub terminal_state: String,
    pub report_md: String,
    pub claims: Vec<ResearchClaim>,
    pub not_covered: Vec<String>,
    pub residue: Vec<ResearchResidueRow>,
    pub reframe: Option<ResearchReframe>,
    pub alignment: Option<ResearchAlignment>,
    pub budget: ResearchBudget,
    pub rounds: Vec<ResearchRoundRow>,
    pub consent: Option<ResearchConsent>,
    pub constitution: ResearchConstitution,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResearchClaim {
    pub id: String,
    pub text: String,
    pub verdict: String,
    pub status: String,
    pub citations: Vec<ResearchCitation>,
    pub corroboration: Option<ResearchCorroboration>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchCitation {
    pub evidence_id: String,
    pub url: String,
    pub chunk_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchCorroboration {
    pub origins: Vec<String>,
    pub support_chunks: usize,
    pub floor: usize,
    pub passes_floor: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchResidueRow {
    pub query: String,
    pub round: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchReframe {
    pub round: u32,
    pub original_question: String,
    pub reframed_question: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchAlignment {
    pub round: u32,
    pub original_question: String,
    pub redirected_question: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchRoundRow {
    pub round: u32,
    pub gaps_before: usize,
    pub gaps_after: usize,
    pub fetched: usize,
    pub search_calls: u32,
}

/// The (g) constitution position: zero untraced figures in [passed].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchConstitution {
    pub passed_claims: usize,
    /// Every untraced-figure violation, naming the claim. Empty = holds.
    pub violations: Vec<String>,
    /// [passed] claims whose evidence ids resolved to no window chunk —
    /// the check could not run on them; reported, never defaulted.
    pub unresolved: usize,
}
