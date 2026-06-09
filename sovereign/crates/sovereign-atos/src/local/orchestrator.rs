// SPDX-License-Identifier: AGPL-3.0-or-later
//! `LocalAtosOrchestrator` — default [`AtosOrchestrator`] impl backed
//! by a pair of local [`FeatureStore`] / [`NoteStore`] handles.
//!
//! Pure orchestration: no terminal I/O (that belongs to the CLI),
//! no subprocess spawning beyond the stop-condition runner, no
//! rendering of user-facing banners. The methods compose existing
//! corpus-engine operations and expose the result as trait-level
//! types the caller can render however it likes.
//!
//! Pure text helpers live next door in [`super::helpers`]; this file
//! is the orchestration surface and the tests that exercise it
//! end-to-end against real SQLite tempdirs.

use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;

use corpus_engine_atos::{AtosRunRow, FeatureRow, FeatureState, FeatureStore, MilestoneRow};
use corpus_engine_notes::{NoteRow, NoteScope, NoteStore, ScopeFilter};

use super::helpers;
use crate::{
    AtosOrchestrator, Error, ReportSection, Result, RunContext, RunMode, StopOutcome,
    TeardownAction, TeardownReport,
};

const STDOUT_CAP_BYTES: usize = 8 * 1024;

/// The default orchestrator. Holds `Arc` handles to the two stores
/// and — optionally — an inference provider for Fast-slot
/// classification during teardown.
///
/// Constructing: use [`LocalAtosOrchestrator::new`] or the top-level
/// [`crate::local_orchestrator`] helper. The store handles are cheap
/// to clone; multiple orchestrator instances can share them.
pub struct LocalAtosOrchestrator {
    features: Arc<FeatureStore>,
    notes: Arc<NoteStore>,
    inference: Option<Arc<dyn sovereign_core::traits::InferenceProvider>>,
    /// Optional doc store. When attached,
    /// [`LocalAtosOrchestrator::render_and_write_report`] indexes each
    /// written report into FTS so `project_context` retrieval surfaces
    /// it alongside other markdown docs. None → reports stay on disk
    /// only (still the authoritative artifact).
    project_docs: Option<Arc<corpus_engine_notes::ProjectDocsStore>>,
    /// Repo root for computing relative paths during indexing. Defaults
    /// to CWD when the orchestrator isn't told otherwise.
    repo_root: Option<std::path::PathBuf>,
}

impl LocalAtosOrchestrator {
    pub fn new(features: Arc<FeatureStore>, notes: Arc<NoteStore>) -> Self {
        Self {
            features,
            notes,
            inference: None,
            project_docs: None,
            repo_root: None,
        }
    }

    /// Attach a Fast-slot-capable inference provider. When present,
    /// teardown's `--suggest` mode uses it; when absent, suggestions
    /// fall back to heuristics.
    pub fn with_inference(
        mut self,
        inference: Arc<dyn sovereign_core::traits::InferenceProvider>,
    ) -> Self {
        self.inference = Some(inference);
        self
    }

    /// Attach a `ProjectDocsStore` so written reports flow into the
    /// `project_context` FTS index. `repo_root` is used to compute
    /// stable relative paths; if absent, the store is still wired
    /// but paths will be absolute (still searchable, just noisier).
    pub fn with_project_docs(
        mut self,
        docs: Arc<corpus_engine_notes::ProjectDocsStore>,
        repo_root: std::path::PathBuf,
    ) -> Self {
        self.project_docs = Some(docs);
        self.repo_root = Some(repo_root);
        self
    }

    /// Access the feature store. Exposed so M3.3's charter parser and
    /// M3.7's teardown can do store-only operations without a wider
    /// API surface.
    pub fn features(&self) -> &Arc<FeatureStore> {
        &self.features
    }

    pub fn notes(&self) -> &Arc<NoteStore> {
        &self.notes
    }
}

#[async_trait]
impl AtosOrchestrator for LocalAtosOrchestrator {
    async fn provision_feature(&self, charter_md: &str) -> Result<FeatureRow> {
        let parsed = crate::charter::parse(charter_md)?;
        let (id, title) = helpers::extract_id_and_title(charter_md)?;

        // features.stop_condition remains empty at the feature level
        // from M3.3 onward — each milestone carries its own. M1/M2
        // features on disk keep their legacy feature-level
        // stop_condition; `run_stop_condition` will prefer the
        // milestone's value when present (M3.6 wires that).
        let feature = self
            .features
            .provision(&id, &title, &parsed.preamble_md, "", "")
            .await?;

        // Lift the charter-parsed auto-redteam opt-in into the row.
        // `false` is the default; only flip when the author explicitly
        // asked. We persist this as its own call rather than widening
        // `provision` so M1–M4 callsites stay untouched.
        if parsed.auto_redteam {
            self.features.set_auto_redteam(&id, true).await?;
        }

        // Seed one feature_milestones row per parsed milestone. Each
        // row's brief_md begins with a synthetic header so the
        // rendered brief reads like a self-contained document when
        // `atos next` pipes it into the driver.
        for spec in &parsed.milestones {
            let brief = helpers::compose_milestone_brief(spec);
            // Store the stop_condition at the end of brief_md for
            // M3.4's handoff path to pick up without a schema change.
            // A future migration can promote this to a real column.
            let brief_with_marker = if spec.stop_condition.is_empty() {
                brief
            } else {
                format!(
                    "{brief}\n\n<!-- atos:stop_condition:{} -->\n",
                    spec.stop_condition
                )
            };
            self.features
                .add_milestone(&id, spec.ordinal, &brief_with_marker)
                .await?;
        }

        Ok(feature)
    }

    async fn archive_feature(&self, feature_id: &str, reason: &str) -> Result<bool> {
        Ok(self.features.archive(feature_id, reason).await?)
    }

    async fn list_features(&self, include_archived: bool) -> Result<Vec<FeatureRow>> {
        Ok(self.features.list(include_archived).await?)
    }

    async fn get_feature(&self, feature_id: &str) -> Result<Option<FeatureRow>> {
        Ok(self.features.get(feature_id).await?)
    }

    async fn list_milestones(&self, feature_id: &str) -> Result<Vec<MilestoneRow>> {
        Ok(self.features.list_milestones(feature_id).await?)
    }

    async fn list_runs(&self, feature_id: &str) -> Result<Vec<AtosRunRow>> {
        Ok(self.features.list_runs_for_feature(feature_id).await?)
    }

    async fn next_milestone(
        &self,
        feature_id: &str,
        mode: RunMode,
    ) -> Result<Option<crate::PreparedBrief>> {
        let Some(_feature) = self.features.get(feature_id).await? else {
            return Err(Error::FeatureNotFound(feature_id.to_string()));
        };
        let milestones = self.features.list_milestones(feature_id).await?;
        if milestones.is_empty() {
            return Ok(None);
        }
        let runs = self.features.list_runs_for_feature(feature_id).await?;

        // Find the lowest-ordinal milestone whose latest `normal`-mode
        // run did not pass, or which has no normal runs at all.
        // Redteam runs don't count toward milestone progress — a team
        // may run the red team on a passing milestone multiple times
        // without unsealing it.
        let mut milestones_sorted = milestones.clone();
        milestones_sorted.sort_by_key(|m| m.ordinal);
        let mut target: Option<MilestoneRow> = None;
        for m in &milestones_sorted {
            let latest_normal = runs
                .iter()
                .filter(|r| r.milestone_id == m.id && r.mode == "normal")
                .max_by_key(|r| r.started_at);
            let passed = latest_normal.and_then(|r| r.stop_passed).unwrap_or(false);
            if !passed {
                target = Some(m.clone());
                break;
            }
        }
        let Some(milestone) = target else {
            return Ok(None);
        };

        // Compose the prior-milestone digest from feature-scoped notes
        // written before the target milestone started. Deterministic
        // concatenation so the handoff flow is testable without
        // inference; a future Fast-slot summarizer can swap in inside
        // `helpers::compose_prior_digest`.
        let prior_digest_md = if milestone.ordinal > 1 {
            helpers::compose_prior_digest(&self.notes, feature_id).await?
        } else {
            String::new()
        };

        let global_invariants_md = helpers::compose_global_invariants(&self.notes).await?;
        let stop_condition = helpers::extract_milestone_stop_condition(&milestone.brief_md);
        let charter_brief_md = helpers::strip_stop_condition_marker(&milestone.brief_md);

        Ok(Some(crate::PreparedBrief {
            feature_id: feature_id.to_string(),
            milestone_id: milestone.id.clone(),
            milestone_ordinal: milestone.ordinal,
            milestone_title: helpers::derive_milestone_title(&milestone.brief_md),
            charter_brief_md,
            stop_condition,
            prior_digest_md,
            global_invariants_md,
            mode,
        }))
    }

    async fn begin_run(
        &self,
        feature_id: &str,
        milestone_id: &str,
        driver: &str,
        mode: RunMode,
    ) -> Result<RunContext> {
        // Move the feature to Active as a side effect of opening a
        // run. Previous state transitions (provisioned / paused) all
        // flow into active the moment work resumes — easier than
        // asking the CLI to remember.
        let _ = self
            .features
            .set_state(feature_id, FeatureState::Active)
            .await;

        // Lookup the milestone once here so the returned context
        // carries an ordinal for the CLI to print.
        let ordinal = self
            .features
            .list_milestones(feature_id)
            .await?
            .into_iter()
            .find(|m| m.id == milestone_id)
            .map(|m| m.ordinal)
            .ok_or_else(|| Error::MilestoneNotFound {
                feature_id: feature_id.to_string(),
                ordinal: -1,
            })?;

        let run = self
            .features
            .open_run_with_mode(feature_id, milestone_id, driver, mode.as_str())
            .await?;
        Ok(RunContext {
            run_id: run.id,
            feature_id: feature_id.to_string(),
            milestone_id: milestone_id.to_string(),
            milestone_ordinal: ordinal,
            driver: driver.to_string(),
            mode,
        })
    }

    async fn close_run(
        &self,
        run_id: &str,
        exit_code: i32,
        stop_passed: bool,
        stop_stdout: Option<&str>,
    ) -> Result<()> {
        let _ = self
            .features
            .close_run_with_stdout(run_id, exit_code as i64, stop_passed, stop_stdout)
            .await?;
        Ok(())
    }

    async fn run_stop_condition(&self, feature: &FeatureRow) -> Result<StopOutcome> {
        self.run_shell_command(&feature.stop_condition).await
    }

    async fn render_report(&self, feature_id: &str, section: ReportSection) -> Result<String> {
        let Some(feature) = self.features.get(feature_id).await? else {
            return Err(Error::FeatureNotFound(feature_id.to_string()));
        };
        let runs = self.list_runs(feature_id).await?;
        let milestones = self.list_milestones(feature_id).await?;
        crate::report::render(self.notes.as_ref(), &feature, &milestones, &runs, section).await
    }

    async fn promote_note(
        &self,
        note_id: &str,
        to: NoteScope,
        feature_id: Option<&str>,
        new_content: Option<&str>,
    ) -> Result<String> {
        Ok(self
            .notes
            .promote_note(note_id, to, feature_id, new_content)
            .await?)
    }

    async fn apply_teardown(
        &self,
        feature_id: &str,
        actions: Vec<TeardownAction>,
    ) -> Result<TeardownReport> {
        // M3.7 fills in the full flow (Fast-slot suggest,
        // confirmation gates, redteam-mode filtering). M3.1 does the
        // mechanical loop so the trait surface is real; CLI can call
        // it and see partial behavior.
        let mut report = TeardownReport::default();
        for action in actions {
            match action {
                TeardownAction::Promote {
                    ref note_id,
                    ref rewritten_content,
                } => {
                    let _ = self
                        .notes
                        .promote_note(
                            note_id,
                            NoteScope::Global,
                            None,
                            rewritten_content.as_deref(),
                        )
                        .await?;
                    report.promoted.push(note_id.clone());
                }
                TeardownAction::Archive { ref note_id } => {
                    report.archived.push(note_id.clone());
                }
                TeardownAction::Retire { ref note_id } => {
                    let _ = self.notes.delete_note(note_id).await?;
                    report.retired.push(note_id.clone());
                }
                TeardownAction::Skip { ref note_id } => {
                    report.skipped.push(note_id.clone());
                }
            }
        }
        // Freeze the feature. M3.7 will also write the
        // epistemic-report.md to disk — for M3.1 we just set state.
        let _ = self
            .features
            .set_state(feature_id, FeatureState::Completed)
            .await?;
        report.epistemic_report_md = self
            .render_report(feature_id, ReportSection::Epistemic)
            .await?;
        Ok(report)
    }

    async fn active_global_invariants(&self) -> Result<Vec<NoteRow>> {
        helpers::global_invariants_rows(&self.notes).await
    }
}

// ─── Inherent helpers used by the CLI ───────────────────────────────────────

impl LocalAtosOrchestrator {
    /// Resolve the shell command that gates a given (feature,
    /// milestone) tuple. Precedence:
    ///
    /// 1. Per-milestone marker `<!-- atos:stop_condition:... -->` in
    ///    `feature_milestones.brief_md` (written by the charter
    ///    provisioner in M3.3).
    /// 2. `features.stop_condition` — the M1/M2 feature-level command,
    ///    kept for pre-M3.3 features.
    /// 3. Empty string — treated as a manual-review milestone by
    ///    [`Self::run_shell_command`] (returns `passed=true`).
    pub fn resolve_milestone_stop_condition(
        feature: &FeatureRow,
        milestone: &MilestoneRow,
    ) -> String {
        let per_milestone = helpers::extract_milestone_stop_condition(&milestone.brief_md);
        if !per_milestone.is_empty() {
            return per_milestone;
        }
        feature.stop_condition.clone()
    }

    /// Return the notes an operator should classify at teardown.
    ///
    /// Rules:
    /// - feature scope + `feature_id` match;
    /// - kind ∈ { decision, invariant, attempt, uncertainty,
    ///   postmortem_pointer } — the "promotable or retireable" set.
    ///   `redteam_finding` is NOT returned: those stay feature-scoped
    ///   and are rendered by the report renderer, not promoted into
    ///   the global corpus.
    /// - Notes written during a `mode=redteam` run are filtered out
    ///   so teammate-written red-team scratch doesn't reach global
    ///   scope by accident. (Red-team sessions should only call
    ///   `write_redteam_finding`, but if a cooperative policy slips,
    ///   this guards anyway.)
    pub async fn teardown_candidates(
        &self,
        feature_id: &str,
    ) -> Result<Vec<corpus_engine_notes::NoteRow>> {
        let filter = ScopeFilter {
            scopes: vec![NoteScope::Feature],
            feature_id: Some(feature_id.to_string()),
        };
        let notes = self
            .notes
            .read_notes_scoped(
                None,
                &[],
                &[],
                &[
                    "decision".to_string(),
                    "invariant".to_string(),
                    "attempt".to_string(),
                    "uncertainty".to_string(),
                    "postmortem_pointer".to_string(),
                ],
                500,
                false,
                &filter,
            )
            .await?;

        // Build the set of session_ids used by redteam runs, so we can
        // exclude notes authored during those runs.
        let runs = self.features.list_runs_for_feature(feature_id).await?;
        let redteam_sessions: std::collections::HashSet<String> = runs
            .iter()
            .filter(|r| r.mode == "redteam")
            .filter_map(|r| r.session_id.clone())
            .collect();
        // The WriteRedteamFindingTool writes with session_id="redteam"
        // as a literal — exclude that too.
        let mut filtered: Vec<corpus_engine_notes::NoteRow> = notes
            .into_iter()
            .filter(|n| n.session_id != "redteam")
            .filter(|n| !redteam_sessions.contains(&n.session_id))
            .collect();
        // Stable ordering: feature scope first, then kind, then
        // newest-first within kind.
        filtered.sort_by(|a, b| a.kind.cmp(&b.kind).then(b.created_at.cmp(&a.created_at)));
        Ok(filtered)
    }

    /// Write a rendered report to the per-feature directory on disk.
    /// Centralized here so every hook point (`end-milestone`,
    /// red-team completion, `teardown`) uses one filename convention
    /// and respects the same directory roots.
    ///
    /// Directory shape:
    ///
    /// ```text
    /// <cwd>/.sovereign/features/<feature-id>/
    ///     milestone-1.md       — per-milestone artifact (PASS only)
    ///     milestone-2.md
    ///     red-team.md          — accumulated red-team findings
    ///     epistemic-report.md  — final teardown artifact
    /// ```
    pub async fn render_and_write_report(
        &self,
        feature_id: &str,
        section: crate::ReportSection,
    ) -> Result<std::path::PathBuf> {
        let rendered = self.render_report(feature_id, section.clone()).await?;
        let dir = helpers::feature_dir(feature_id);
        std::fs::create_dir_all(&dir).map_err(Error::Io)?;
        let filename = match section {
            crate::ReportSection::Milestone(n) => format!("milestone-{n}.md"),
            crate::ReportSection::RedTeam => "red-team.md".into(),
            crate::ReportSection::Epistemic => "epistemic-report.md".into(),
            crate::ReportSection::All => "report.md".into(),
        };
        let path = dir.join(filename);
        std::fs::write(&path, rendered).map_err(Error::Io)?;

        // Index into project_context so a future agent turn can pull
        // "what did we learn on milestone-1?" via the same retrieval
        // path as any other markdown doc. Failure is logged and
        // ignored — the file on disk is the authoritative artifact;
        // the FTS index is a convenience layer.
        if let Some(store) = self.project_docs.as_ref() {
            let repo_root = self
                .repo_root
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| dir.clone()));
            match store.index_file(&path, &repo_root).await {
                Ok(n) => tracing::debug!(
                    path = %path.display(),
                    chunks = n,
                    "atos: indexed report into project_docs"
                ),
                Err(e) => tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "atos: project_docs index_file failed"
                ),
            }
        }

        Ok(path)
    }

    /// Run a specific shell command and capture its outcome. Used by
    /// `end-milestone` to execute the per-milestone stop condition
    /// resolved via [`Self::resolve_milestone_stop_condition`].
    pub async fn run_milestone_stop_condition(
        &self,
        feature: &FeatureRow,
        milestone: &MilestoneRow,
    ) -> Result<StopOutcome> {
        let cmd = Self::resolve_milestone_stop_condition(feature, milestone);
        self.run_shell_command(&cmd).await
    }

    /// Shared shell-runner. Used by the trait's `run_stop_condition`
    /// (feature-level) and [`Self::run_milestone_stop_condition`]
    /// (milestone-level). Single capture + truncation path.
    async fn run_shell_command(&self, cmd: &str) -> Result<StopOutcome> {
        if cmd.trim().is_empty() {
            // No stop command → operator wants manual review. Treat
            // as pass so the milestone doesn't block forever; the
            // review checklist still flags unverified invariants.
            return Ok(StopOutcome {
                passed: true,
                exit_code: 0,
                stdout: String::new(),
            });
        }
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| Error::StopConditionSpawn(e.to_string()))?;
        let mut combined = String::new();
        combined.push_str(&String::from_utf8_lossy(&output.stdout));
        if !output.stderr.is_empty() {
            combined.push_str("\n---stderr---\n");
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        if combined.len() > STDOUT_CAP_BYTES {
            combined.truncate(STDOUT_CAP_BYTES);
            combined.push_str("\n…(truncated)…");
        }
        Ok(StopOutcome {
            passed: output.status.success(),
            exit_code: output.status.code().unwrap_or(-1),
            stdout: combined,
        })
    }

    pub async fn provision_feature_parts(
        &self,
        id: &str,
        title: &str,
        charter_md: &str,
        sovereign_md: &str,
        stop_condition: &str,
    ) -> Result<FeatureRow> {
        Ok(self
            .features
            .provision(id, title, charter_md, sovereign_md, stop_condition)
            .await?)
    }

    /// Add a single milestone row. Matches the M1/M2 CLI flow where
    /// `start-milestone --brief <path>` appends milestones
    /// imperatively. M3.3 replaces this with charter-derived rows at
    /// provision time.
    pub async fn add_milestone(
        &self,
        feature_id: &str,
        ordinal: i64,
        brief_md: &str,
    ) -> Result<MilestoneRow> {
        Ok(self
            .features
            .add_milestone(feature_id, ordinal, brief_md)
            .await?)
    }

    /// Mark a milestone as started. Thin wrapper; surfaced for the CLI
    /// so it doesn't need a direct FeatureStore handle.
    pub async fn mark_milestone_started(&self, milestone_id: &str) -> Result<bool> {
        Ok(self.features.mark_started(milestone_id).await?)
    }

    /// Persist the compliance report JSON on the milestone row.
    pub async fn mark_milestone_ended(
        &self,
        milestone_id: &str,
        compliance_report_json: &str,
    ) -> Result<bool> {
        Ok(self
            .features
            .mark_ended(milestone_id, compliance_report_json)
            .await?)
    }

    /// Compute the next ordinal to use when adding a milestone
    /// imperatively (M1/M2 flow).
    pub async fn next_ordinal(&self, feature_id: &str) -> Result<i64> {
        Ok(self.features.next_ordinal(feature_id).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_orchestrator() -> LocalAtosOrchestrator {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let features = Arc::new(FeatureStore::open(&path.join("features.db")).unwrap());
        let notes = Arc::new(NoteStore::open(&path.join("notes.db")).unwrap());
        // leak so the DB survives the test body
        std::mem::forget(dir);
        LocalAtosOrchestrator::new(features, notes)
    }

    #[tokio::test]
    async fn provision_parts_round_trip() {
        let orc = make_orchestrator().await;
        let f = orc
            .provision_feature_parts("fx", "t", "c", "", "true")
            .await
            .unwrap();
        assert_eq!(f.id, "fx");
        let loaded = orc.get_feature("fx").await.unwrap().unwrap();
        assert_eq!(loaded.title, "t");
    }

    #[tokio::test]
    async fn archive_feature_round_trip() {
        let orc = make_orchestrator().await;
        orc.provision_feature_parts("fx", "t", "c", "", "")
            .await
            .unwrap();
        assert!(orc.archive_feature("fx", "done").await.unwrap());
        let active = orc.list_features(false).await.unwrap();
        assert!(active.iter().all(|f| f.id != "fx"));
        let all = orc.list_features(true).await.unwrap();
        assert!(all.iter().any(|f| f.id == "fx"));
    }

    #[tokio::test]
    async fn begin_and_close_run() {
        let orc = make_orchestrator().await;
        orc.provision_feature_parts("fx", "t", "c", "", "true")
            .await
            .unwrap();
        let m = orc.add_milestone("fx", 1, "brief").await.unwrap();
        let ctx = orc
            .begin_run("fx", &m.id, "claude", RunMode::Normal)
            .await
            .unwrap();
        assert_eq!(ctx.milestone_ordinal, 1);
        assert_eq!(ctx.driver, "claude");
        orc.close_run(&ctx.run_id, 0, true, None).await.unwrap();
        let runs = orc.list_runs("fx").await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].stop_passed, Some(true));
    }

    #[tokio::test]
    async fn stop_condition_captures_stdout() {
        let orc = make_orchestrator().await;
        let f = orc
            .provision_feature_parts("fx", "t", "c", "", "echo hello world")
            .await
            .unwrap();
        let outcome = orc.run_stop_condition(&f).await.unwrap();
        assert!(outcome.passed);
        assert_eq!(outcome.exit_code, 0);
        assert!(outcome.stdout.contains("hello world"));
    }

    #[tokio::test]
    async fn stop_condition_empty_passes_as_manual_review() {
        let orc = make_orchestrator().await;
        let f = orc
            .provision_feature_parts("fx", "t", "c", "", "")
            .await
            .unwrap();
        let outcome = orc.run_stop_condition(&f).await.unwrap();
        assert!(outcome.passed);
        assert!(outcome.stdout.is_empty());
    }

    // ── Charter-driven provisioning (M3.3) ───────────────────────────────

    const CHARTER_TWO_MS: &str = "# atos-version-flag — Add `--version` to atos

Preamble for the feature.

## Milestones

### 1. Wire the flag

Implement the flag handler.

**Stop condition:** `cargo run -p sovereign-cli -- atos --version`

### 2. Regression test

Add a CLI test.

**Stop condition:** `cargo test -p sovereign-cli atos_version`
";

    #[tokio::test]
    async fn provision_feature_from_charter_seeds_milestones() {
        let orc = make_orchestrator().await;
        let f = orc.provision_feature(CHARTER_TWO_MS).await.unwrap();
        assert_eq!(f.id, "atos-version-flag");
        // pulldown-cmark delivers inline code stripped of its
        // backticks — we accept the stripped form as the title.
        assert_eq!(f.title, "Add --version to atos");
        assert_eq!(f.state, "provisioned");
        assert!(f.charter_md.contains("Preamble for the feature"));
        // Charter preamble stops before `## Milestones`.
        assert!(!f.charter_md.contains("## Milestones"));

        let milestones = orc.list_milestones(&f.id).await.unwrap();
        assert_eq!(milestones.len(), 2);
        assert_eq!(milestones[0].ordinal, 1);
        assert!(milestones[0].brief_md.contains("Wire the flag"));
        let stop = helpers::extract_milestone_stop_condition(&milestones[0].brief_md);
        assert_eq!(stop, "cargo run -p sovereign-cli -- atos --version");
        assert_eq!(milestones[1].ordinal, 2);
        let stop2 = helpers::extract_milestone_stop_condition(&milestones[1].brief_md);
        assert_eq!(stop2, "cargo test -p sovereign-cli atos_version");
    }

    #[tokio::test]
    async fn provision_feature_without_milestones_section_errors() {
        let orc = make_orchestrator().await;
        let err = orc
            .provision_feature("# only-a-title\n\nno milestones here.\n")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::CharterParse(_)));
    }

    #[tokio::test]
    async fn provision_feature_without_h1_title_errors() {
        let orc = make_orchestrator().await;
        let err = orc
            .provision_feature("## Milestones\n\n### 1. t\n\n**Stop condition:** `x`\n")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::CharterParse(_)));
    }

    // ── next_milestone selection (M3.4) ──────────────────────────────────

    #[tokio::test]
    async fn next_milestone_picks_first_when_no_runs() {
        let orc = make_orchestrator().await;
        orc.provision_feature(CHARTER_TWO_MS).await.unwrap();
        let brief = orc
            .next_milestone("atos-version-flag", RunMode::Normal)
            .await
            .unwrap()
            .expect("should have next milestone");
        assert_eq!(brief.milestone_ordinal, 1);
        assert_eq!(
            brief.stop_condition,
            "cargo run -p sovereign-cli -- atos --version"
        );
        assert!(brief.prior_digest_md.is_empty());
        assert_eq!(brief.mode, RunMode::Normal);
    }

    #[tokio::test]
    async fn next_milestone_skips_passed_milestones() {
        let orc = make_orchestrator().await;
        orc.provision_feature(CHARTER_TWO_MS).await.unwrap();
        let milestones = orc.list_milestones("atos-version-flag").await.unwrap();
        // Close out milestone 1 as passing.
        let ctx = orc
            .begin_run(
                "atos-version-flag",
                &milestones[0].id,
                "claude",
                RunMode::Normal,
            )
            .await
            .unwrap();
        orc.close_run(&ctx.run_id, 0, true, Some("stdout"))
            .await
            .unwrap();

        let brief = orc
            .next_milestone("atos-version-flag", RunMode::Normal)
            .await
            .unwrap()
            .expect("milestone 2 should be next");
        assert_eq!(brief.milestone_ordinal, 2);
    }

    #[tokio::test]
    async fn next_milestone_redoes_failed() {
        let orc = make_orchestrator().await;
        orc.provision_feature(CHARTER_TWO_MS).await.unwrap();
        let milestones = orc.list_milestones("atos-version-flag").await.unwrap();
        // Close out milestone 1 as FAILED.
        let ctx = orc
            .begin_run(
                "atos-version-flag",
                &milestones[0].id,
                "claude",
                RunMode::Normal,
            )
            .await
            .unwrap();
        orc.close_run(&ctx.run_id, 1, false, Some("failed"))
            .await
            .unwrap();

        let brief = orc
            .next_milestone("atos-version-flag", RunMode::Normal)
            .await
            .unwrap()
            .expect("milestone 1 should still be next on failure");
        assert_eq!(brief.milestone_ordinal, 1);
    }

    #[tokio::test]
    async fn next_milestone_returns_none_when_all_passed() {
        let orc = make_orchestrator().await;
        orc.provision_feature(CHARTER_TWO_MS).await.unwrap();
        let milestones = orc.list_milestones("atos-version-flag").await.unwrap();
        for m in &milestones {
            let ctx = orc
                .begin_run("atos-version-flag", &m.id, "claude", RunMode::Normal)
                .await
                .unwrap();
            orc.close_run(&ctx.run_id, 0, true, None).await.unwrap();
        }
        let brief = orc
            .next_milestone("atos-version-flag", RunMode::Normal)
            .await
            .unwrap();
        assert!(brief.is_none());
    }

    #[tokio::test]
    async fn next_milestone_redteam_runs_do_not_count_as_progress() {
        // A redteam run that "passes" on milestone 1 must not satisfy
        // the normal-mode gate — the team may redteam a passing
        // milestone multiple times without un-sealing it.
        let orc = make_orchestrator().await;
        orc.provision_feature(CHARTER_TWO_MS).await.unwrap();
        let milestones = orc.list_milestones("atos-version-flag").await.unwrap();
        let ctx = orc
            .begin_run(
                "atos-version-flag",
                &milestones[0].id,
                "claude",
                RunMode::Redteam,
            )
            .await
            .unwrap();
        orc.close_run(&ctx.run_id, 0, true, None).await.unwrap();
        let brief = orc
            .next_milestone("atos-version-flag", RunMode::Normal)
            .await
            .unwrap()
            .expect("milestone 1 still needs a normal run");
        assert_eq!(brief.milestone_ordinal, 1);
    }

    #[tokio::test]
    async fn prepared_brief_renders_with_charter_and_stop() {
        let orc = make_orchestrator().await;
        orc.provision_feature(CHARTER_TWO_MS).await.unwrap();
        let brief = orc
            .next_milestone("atos-version-flag", RunMode::Normal)
            .await
            .unwrap()
            .unwrap();
        let rendered = brief.render();
        assert!(rendered.contains("# Milestone 1 — Wire the flag"));
        assert!(rendered.contains("**Stop condition:**"));
        assert!(rendered.contains("Implement the flag handler"));
        // No stray stop-condition marker (it lives in the header only).
        assert!(!rendered.contains("<!-- atos:stop_condition:"));
    }

    #[tokio::test]
    async fn stop_condition_nonzero_fails() {
        let orc = make_orchestrator().await;
        let f = orc
            .provision_feature_parts("fx", "t", "c", "", "exit 7")
            .await
            .unwrap();
        let outcome = orc.run_stop_condition(&f).await.unwrap();
        assert!(!outcome.passed);
        assert_eq!(outcome.exit_code, 7);
    }

    // ── M5.4: reports indexed into ProjectDocsStore ──────────────────────

    #[tokio::test]
    async fn written_report_is_searchable_via_project_docs() {
        // The orchestrator writes milestone-N.md into
        // <repo_root>/.sovereign/features/<id>/ and — when wired —
        // indexes it into ProjectDocsStore. We verify the end-to-end:
        // render → write → index → search returns the artifact.
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().to_path_buf();

        // Features+notes under .sovereign/ so the report write path
        // lands in repo_root/.sovereign/features/fx/milestone-1.md.
        let sovereign_dir = repo_root.join(".sovereign");
        std::fs::create_dir_all(&sovereign_dir).unwrap();
        let features = Arc::new(FeatureStore::open(&sovereign_dir.join("features.db")).unwrap());
        let notes = Arc::new(NoteStore::open(&sovereign_dir.join("notes.db")).unwrap());
        let docs = Arc::new(
            corpus_engine_notes::ProjectDocsStore::open(&sovereign_dir.join("project_docs.db"))
                .unwrap(),
        );
        let orc = LocalAtosOrchestrator::new(features, notes)
            .with_project_docs(Arc::clone(&docs), repo_root.clone());

        // Seed: feature + one milestone + one passing run so the
        // report renderer has material to format. The feature id
        // carries a unique token ("permafrost") so the rendered
        // header includes it and FTS can find it on search.
        orc.provision_feature_parts("permafrost", "Title", "Charter.", "", "true")
            .await
            .unwrap();
        let m = orc.add_milestone("permafrost", 1, "Brief").await.unwrap();
        let ctx = orc
            .begin_run("permafrost", &m.id, "claude", RunMode::Normal)
            .await
            .unwrap();
        orc.close_run(&ctx.run_id, 0, true, Some("ok"))
            .await
            .unwrap();

        // The test runs with CWD = workspace, not the scratch repo.
        // render_and_write_report uses `feature_dir(feature_id)`
        // which resolves relative to cwd, so we must write relative
        // to repo_root ourselves via env::set_current_dir. Cheapest
        // approach: sandbox in repo_root for the duration of the
        // write.
        let prior = std::env::current_dir().unwrap();
        std::env::set_current_dir(&repo_root).unwrap();
        let written = orc
            .render_and_write_report("permafrost", ReportSection::Milestone(1))
            .await
            .unwrap();
        std::env::set_current_dir(prior).unwrap();

        assert!(written.exists(), "report should be written to disk");

        // Search via ProjectDocsStore — the unique token must find
        // the indexed chunk.
        let hits = docs.search("permafrost", 5).await.unwrap();
        assert!(
            hits.iter().any(|h| h.file_path.contains("milestone-1.md")),
            "project_docs search should surface the indexed report; got: {hits:#?}"
        );
    }
}
