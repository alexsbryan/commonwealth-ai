//! sovereign-atos — the Agent Task Orchestration System library.
//!
//! Why this crate exists
//! =====================
//! M1 and M2 wired orchestration logic directly into
//! `sovereign-cli/src/atos_cmd.rs`. That made the CLI the only
//! transport. M3 extracts the logic so the same operations can be
//! invoked from any transport — the CLI today, a `/v1/atos/...`
//! endpoint in M4, an MCP tool vocabulary, anything.
//!
//! The [`AtosOrchestrator`] trait is the stable surface. The default
//! implementation [`LocalAtosOrchestrator`] talks to a local
//! [`FeatureStore`] / [`NoteStore`]; tests mock the trait.
//!
//! Dependency layer:
//!     stores (corpus-engine)
//!         ↓
//!     sovereign-atos  ← you are here
//!         ↓
//!     transports (sovereign-cli, future /v1, future MCP)
//!
//! The library does NOT depend on sovereign-tools, sovereign-mesh,
//! sovereign-inference, or sovereign-cli — that's enforced by the
//! Cargo.toml and enforces the layering at compile time.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use corpus_engine_atos::{AtosRunRow, FeatureRow, FeatureStore, MilestoneRow};
use corpus_engine_notes::{NoteRow, NoteStore};

pub mod approval;
pub mod charter;
pub mod local;
pub mod report;
pub mod session;

pub use charter::{parse as parse_charter, CharterParse, MilestoneSpec};
pub use local::LocalAtosOrchestrator;

// ─── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("feature not found: {0}")]
    FeatureNotFound(String),

    #[error("milestone not found for feature {feature_id}: ordinal {ordinal}")]
    MilestoneNotFound { feature_id: String, ordinal: i64 },

    #[error("run not found: {0}")]
    RunNotFound(String),

    #[error("charter parse error: {0}")]
    CharterParse(String),

    #[error("stop condition failed to run: {0}")]
    StopConditionSpawn(String),

    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),

    #[error("store error: {0}")]
    Store(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<corpus_engine::Error> for Error {
    fn from(e: corpus_engine::Error) -> Self {
        // Preserve InvalidInput mapping so CLI error classes stay
        // honest after the library boundary.
        match e {
            corpus_engine::Error::InvalidInput(s) => Error::InvalidInput(s),
            other => Error::Store(other.to_string()),
        }
    }
}

/// Same shape for `corpus-engine-atos::Error` after the ATOS state
/// carve-out (2026-05-23). FeatureStore + plan_items now return their
/// own narrow Error; bridge it here so the rest of sovereign-atos
/// can `?`-bubble through unchanged.
impl From<corpus_engine_atos::Error> for Error {
    fn from(e: corpus_engine_atos::Error) -> Self {
        match e {
            corpus_engine_atos::Error::InvalidInput(s) => Error::InvalidInput(s),
            other => Error::Store(other.to_string()),
        }
    }
}

/// Same shape for `corpus-engine-notes::Error` (NoteStore + project_docs
/// carved out 2026-05-23, step 3). Mirrors the atos bridge above.
impl From<corpus_engine_notes::Error> for Error {
    fn from(e: corpus_engine_notes::Error) -> Self {
        match e {
            corpus_engine_notes::Error::InvalidInput(s) => Error::InvalidInput(s),
            other => Error::Store(other.to_string()),
        }
    }
}

// ─── Types ────────────────────────────────────────────────────────────────────

/// Driver invocation mode. M3 introduces `Redteam` alongside the
/// default `Normal`. The mode is stored on `atos_runs.mode` and drives
/// tool filtering + report-renderer behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    Normal,
    Redteam,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Redteam => "redteam",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "normal" => Some(Self::Normal),
            "redteam" => Some(Self::Redteam),
            _ => None,
        }
    }
}

/// Which report artifact to render.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ReportSection {
    /// A single milestone's post-stop summary. Produced by
    /// `end-milestone` on PASS.
    Milestone(i64),
    /// The red team's findings for the active milestone. Appended
    /// rather than overwritten.
    RedTeam,
    /// The full feature wrap-up. Produced at teardown; freezes the
    /// feature state to `completed`.
    Epistemic,
    /// Ad-hoc full render without freezing.
    All,
}

/// A composed brief handed to a driver subprocess. The library decides
/// the shape (charter brief + prior-milestone digest + global
/// invariants); the CLI just pipes `.render()` into stdin.
#[derive(Debug, Clone)]
pub struct PreparedBrief {
    pub feature_id: String,
    pub milestone_id: String,
    pub milestone_ordinal: i64,
    pub milestone_title: String,
    pub charter_brief_md: String,
    pub stop_condition: String,
    /// Digest of feature-scoped notes from the prior milestone, or
    /// empty on milestone 1.
    pub prior_digest_md: String,
    /// Global invariants to restate at the top of every session —
    /// keeps agents honest about codebase-wide rules even when their
    /// context is cold.
    pub global_invariants_md: String,
    /// Mode this brief is being composed for. Red-team briefs skip the
    /// prior_digest and replace it with the charter's Invariants
    /// section (cooperative filtering).
    pub mode: RunMode,
}

impl PreparedBrief {
    /// Render the full brief as markdown. This is what gets piped into
    /// the driver subprocess's stdin.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# Milestone {} — {}\n\n",
            self.milestone_ordinal, self.milestone_title
        ));
        out.push_str(&format!(
            "**Feature:** {}\n**Stop condition:** `{}`\n**Mode:** {}\n\n",
            self.feature_id,
            self.stop_condition,
            self.mode.as_str()
        ));
        out.push_str(&self.charter_brief_md);
        out.push_str("\n\n");

        if !self.global_invariants_md.trim().is_empty() {
            out.push_str("## Invariants to respect\n\n");
            out.push_str(&self.global_invariants_md);
            out.push_str("\n\n");
        }
        if !self.prior_digest_md.trim().is_empty() {
            out.push_str("## Prior milestone digest\n\n");
            out.push_str(&self.prior_digest_md);
            out.push_str("\n\n");
        }
        out
    }
}

/// Per-note decision taken by `teardown`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeardownAction {
    /// Copy the note to global scope via [`NoteStore::promote_note`].
    Promote {
        note_id: String,
        rewritten_content: Option<String>,
    },
    /// Leave the note at feature scope. Archival happens at the
    /// feature level so feature-scoped notes naturally stop being
    /// injected once `feature.state = 'archived'`.
    Archive { note_id: String },
    /// Delete the note outright — scratch work, superseded decisions.
    Retire { note_id: String },
    /// Operator explicitly skipped this note. No-op; recorded in the
    /// report so review is auditable.
    Skip { note_id: String },
}

impl TeardownAction {
    pub fn note_id(&self) -> &str {
        match self {
            Self::Promote { note_id, .. } => note_id,
            Self::Archive { note_id } => note_id,
            Self::Retire { note_id } => note_id,
            Self::Skip { note_id } => note_id,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TeardownReport {
    pub promoted: Vec<String>,
    pub archived: Vec<String>,
    pub retired: Vec<String>,
    pub skipped: Vec<String>,
    pub epistemic_report_md: String,
}

/// Return value from [`AtosOrchestrator::run_stop_condition`].
#[derive(Debug, Clone)]
pub struct StopOutcome {
    pub passed: bool,
    pub exit_code: i32,
    /// Captured stdout (bounded at 8KB) so the renderer can include
    /// test output in the milestone-<n>.md artifact.
    pub stdout: String,
}

/// Handle returned from [`AtosOrchestrator::begin_run`]. Callers
/// export `run_id` as `ATOS_RUN_ID` to the driver subprocess.
#[derive(Debug, Clone)]
pub struct RunContext {
    pub run_id: String,
    pub feature_id: String,
    pub milestone_id: String,
    pub milestone_ordinal: i64,
    pub driver: String,
    pub mode: RunMode,
}

// ─── Trait ────────────────────────────────────────────────────────────────────

/// The orchestrator surface. Every transport — CLI today, `/v1` in
/// M4, MCP-tool vocabulary later — calls these methods; the methods
/// never care which transport is driving.
#[async_trait]
pub trait AtosOrchestrator: Send + Sync {
    /// Create a feature row from a charter markdown document.
    ///
    /// The charter MUST contain `# <id> — <title>` as the first-level
    /// heading (id is the slug before the first `—` / `--` / colon,
    /// title is whatever follows) and a `## Milestones` section whose
    /// `### N. Title` subsections carry the per-milestone briefs and
    /// `**Stop condition:**` markers. See [`charter::parse`] for the
    /// full format.
    ///
    /// On success, returns the [`FeatureRow`] and commits one
    /// `feature_milestones` row per charter milestone in document
    /// order. The feature state is `provisioned`.
    async fn provision_feature(&self, charter_md: &str) -> Result<FeatureRow>;

    async fn archive_feature(&self, feature_id: &str, reason: &str) -> Result<bool>;

    async fn list_features(&self, include_archived: bool) -> Result<Vec<FeatureRow>>;

    async fn get_feature(&self, feature_id: &str) -> Result<Option<FeatureRow>>;

    async fn list_milestones(&self, feature_id: &str) -> Result<Vec<MilestoneRow>>;

    async fn list_runs(&self, feature_id: &str) -> Result<Vec<AtosRunRow>>;

    /// Resolve the next milestone an operator should work on. Returns
    /// `None` when every milestone has a passing run.
    ///
    /// Rules:
    /// - Pick the lowest-ordinal milestone whose latest `mode='normal'`
    ///   run did not pass (or has no runs).
    /// - The returned [`PreparedBrief`] includes the charter brief +
    ///   prior-milestone digest + active global invariants; the CLI
    ///   calls `.render()` to produce what it pipes into the driver.
    /// - `mode` selects the brief shape: `Normal` composes the full
    ///   handoff; `Redteam` replaces the digest with invariants-only
    ///   framing (M3.5).
    async fn next_milestone(
        &self,
        feature_id: &str,
        mode: RunMode,
    ) -> Result<Option<PreparedBrief>>;

    /// Open a new run. Feature is moved to `Active` if not already.
    async fn begin_run(
        &self,
        feature_id: &str,
        milestone_id: &str,
        driver: &str,
        mode: RunMode,
    ) -> Result<RunContext>;

    /// Close a run. Writes exit_code + stop_passed + optional
    /// captured stdout from the stop_condition. No-op if the run id
    /// is unknown. `stop_stdout` is `None` for provisional closes
    /// (driver subprocess exited but end-milestone hasn't computed
    /// the real verdict yet); `Some` for final closes.
    async fn close_run(
        &self,
        run_id: &str,
        exit_code: i32,
        stop_passed: bool,
        stop_stdout: Option<&str>,
    ) -> Result<()>;

    /// Run a feature's stop condition and capture the outcome.
    async fn run_stop_condition(&self, feature: &FeatureRow) -> Result<StopOutcome>;

    /// Render a markdown report for the given feature. `ReportSection`
    /// picks the view (per-milestone, red team, or full epistemic).
    async fn render_report(&self, feature_id: &str, section: ReportSection) -> Result<String>;

    /// Promote a note to a new scope via [`NoteStore::promote_note`].
    async fn promote_note(
        &self,
        note_id: &str,
        to: corpus_engine_notes::NoteScope,
        feature_id: Option<&str>,
        new_content: Option<&str>,
    ) -> Result<String>;

    /// Apply a batch of teardown decisions. The operator (or the
    /// Fast-slot suggest pass) decides the actions; this method
    /// executes them atomically enough that a mid-batch failure leaves
    /// a partial-but-consistent state.
    async fn apply_teardown(
        &self,
        feature_id: &str,
        actions: Vec<TeardownAction>,
    ) -> Result<TeardownReport>;

    /// Pull active global invariants — the subset every fresh session
    /// should see. Used by [`PreparedBrief`] composition.
    async fn active_global_invariants(&self) -> Result<Vec<NoteRow>>;
}

// ─── Public re-exports ───────────────────────────────────────────────────────

pub use corpus_engine_atos::FeatureRow as _FeatureRow;
/// Expose the `Arc<FeatureStore>` + `Arc<NoteStore>` handles so
/// transports that construct a `LocalAtosOrchestrator` don't need a
/// direct corpus-engine import just for those types.
pub use corpus_engine_notes::{NoteScope, NoteStore as _NoteStore};

/// Convenience constructor that wires a store pair into the default
/// orchestrator. The optional `InferenceProvider` drives the Fast-slot
/// classification hint in teardown.
pub fn local_orchestrator(
    features: Arc<FeatureStore>,
    notes: Arc<NoteStore>,
) -> LocalAtosOrchestrator {
    LocalAtosOrchestrator::new(features, notes)
}
