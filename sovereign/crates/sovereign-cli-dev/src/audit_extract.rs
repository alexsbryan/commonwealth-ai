// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn audit` LLM-backed extraction pass (Phase 7.3 gap E).
//!
//! When the user runs `svrn audit`, this module:
//!
//! 1. Reads `.sovereign/audit_state.toml` for `last_extracted_head`.
//! 2. Reads `git rev-parse HEAD` for the current head.
//! 3. If unchanged, no-ops (the extractor already ran for this state).
//! 4. Otherwise: builds `git diff <last>..<current>`, feeds it to the
//!    [`DiffDecisionExtractor`] with the supplied
//!    [`DecisionExtractorBackend`] (production:
//!    [`LocalLlmBackend`](sovereign_tools::notes::diff_extract_backend::LocalLlmBackend);
//!    tests: stub), persists the resulting decisions as
//!    `source='extracted'` notes, and advances
//!    `last_extracted_head` to the current head.
//!
//! ## Idempotency
//!
//! Two layers:
//!
//! - Head-equality short-circuit: re-running `svrn audit`
//!   against an unchanged tree does NO LLM work. Cheap.
//! - Body-content dedup: even if the head marker is forced
//!   forward (e.g. user deletes `.sovereign/audit_state.toml`),
//!   the persistence step skips notes whose body already exists
//!   under `source='extracted'` for the project. Repeated
//!   extractions on overlapping diffs don't double-count.
//!
//! ## Best-effort posture
//!
//! Every step that can fail does so quietly:
//!
//! - No git → skip with info-level log.
//! - No daemon configured → skip with info-level log.
//! - Backend errors → empty extraction Vec; the audit's
//!   `extracted` stream stays best-effort.
//!
//! The audit's "non-empty floor" is held up by the agent /
//! committed / observed streams; extraction is additive.

use std::path::{Path, PathBuf};

use corpus_engine_notes::{NoteScope, NoteSource, NoteStore};
use serde::{Deserialize, Serialize};
use sovereign_tools::notes::diff_extract::{
    DecisionExtraction, DecisionExtractorBackend, DiffDecisionExtractor, ExtractionRequest,
    MAX_DIFF_INPUT_BYTES,
};
use sovereign_tools::notes::diff_extract_backend::{LocalLlmBackend, LocalLlmConfig};

/// Path of the audit state file inside the project's
/// `.sovereign/` directory.
fn audit_state_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".sovereign").join("audit_state.toml")
}

/// Persisted state for the extraction pass. Currently just
/// `last_extracted_head` — future fields (per-feature heads, last
/// run timestamp, error-count for backoff) layer on with
/// `#[serde(default)]`.
///
/// Toml-shaped so an operator who wants to force a fresh run can
/// edit the file by hand.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditExtractState {
    /// Most recent commit hash the extractor processed. `None` for
    /// fresh repos or after a manual reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_extracted_head: Option<String>,
}

impl AuditExtractState {
    pub fn load(repo_root: &Path) -> Self {
        let path = audit_state_path(repo_root);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str(&text) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "audit_extract: malformed audit_state.toml; treating as empty"
                );
                Self::default()
            }
        }
    }

    pub fn save(&self, repo_root: &Path) -> std::io::Result<()> {
        let path = audit_state_path(repo_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(self)
            .unwrap_or_else(|_| "# audit_state.toml (serialise failed)\n".to_string());
        std::fs::write(&path, body)
    }
}

/// Read git's current HEAD. Returns `None` if the directory
/// isn't a git repo or `git` isn't on PATH.
fn read_git_head(repo_root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// `git diff <old>..<new>` capped at [`MAX_DIFF_INPUT_BYTES`] —
/// keeps the prompt within the model's context window even when
/// a giant range is in scope. Returns `None` on git failure.
///
/// `old` empty → `git diff <empty-tree-hash>..<new>` (everything
/// since the project's first commit; that's the right baseline
/// for the first extraction run). The empty-tree hash
/// `4b825dc642cb6eb9a060e54bf8d69288fbee4904` is a documented
/// SHA-1 git constant (the hash of an empty tree object) and
/// works on every SHA-1 repository without a separate `git
/// hash-object` shell-out. SHA-256 repositories — rare in 2026
/// — would fall back to `None` here, which the caller surfaces
/// as `skip_reason="git diff failed"`.
fn build_diff(repo_root: &Path, old: Option<&str>, new: &str) -> Option<String> {
    /// SHA-1 hash of the empty git tree object. Universal constant.
    const EMPTY_TREE_SHA1: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let old_arg = old
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| EMPTY_TREE_SHA1.to_string());

    let out = std::process::Command::new("git")
        .arg("diff")
        .arg(format!("{old_arg}..{new}"))
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.len() > MAX_DIFF_INPUT_BYTES {
        // Truncate at a safe char boundary near the cap. The
        // extractor's `build_prompt` ALSO caps; doing it here
        // cuts the bytes we have to ship over IPC into the
        // backend's HTTP body.
        let mut idx = MAX_DIFF_INPUT_BYTES;
        while idx > 0 && !text.is_char_boundary(idx) {
            idx -= 1;
        }
        text.truncate(idx);
        text.push_str("\n[diff truncated]\n");
    }
    Some(text)
}

/// Read the bodies of every active `source='extracted'` note in
/// the project's notes DB. Used to dedup before persisting a new
/// extraction — an LLM run on overlapping diffs commonly
/// re-derives the same decisions.
async fn existing_extracted_bodies(notes: &NoteStore) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let rows = match notes.read_notes(None, &[], &[], &[], 1000, false).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "audit_extract: read_notes failed for dedup set"
            );
            return out;
        }
    };
    for n in rows {
        if n.source == NoteSource::Extracted.as_str() {
            out.insert(n.content);
        }
    }
    out
}

/// Read recent decision/invariant notes — the existing-notes
/// context the extractor uses for contradiction detection.
async fn read_existing_decision_notes(
    notes: &NoteStore,
    limit: usize,
) -> Vec<corpus_engine_notes::NoteRow> {
    notes
        .read_notes(
            None,
            &[],
            &[],
            &["decision".to_string(), "invariant".to_string()],
            limit,
            false,
        )
        .await
        .unwrap_or_default()
}

/// Result summary returned by [`run`].
#[derive(Debug, Clone, Default)]
pub struct ExtractRunSummary {
    /// True when the run actually invoked the extractor (head
    /// changed AND backend was available). False when the
    /// short-circuit fired or the backend was unavailable.
    pub ran: bool,
    /// Hash the extractor processed up to. `None` when nothing ran.
    pub head: Option<String>,
    /// Number of new notes persisted (post-dedup).
    pub written: usize,
    /// Reason the run was skipped, if `ran == false`. Useful for
    /// the audit's section-prelude line ("extraction skipped:
    /// daemon unreachable").
    pub skip_reason: Option<&'static str>,
}

/// Orchestrate one extraction pass against the supplied backend.
/// Pure with respect to the supplied store + backend — no other
/// I/O than the git shell-out and the audit_state.toml file.
///
/// Returns the summary; errors are swallowed and surface as
/// `skip_reason`.
pub async fn run(
    repo_root: &Path,
    notes: &NoteStore,
    backend: &dyn DecisionExtractorBackend,
) -> ExtractRunSummary {
    let Some(current_head) = read_git_head(repo_root) else {
        return ExtractRunSummary {
            skip_reason: Some("not a git repo"),
            ..Default::default()
        };
    };

    let mut state = AuditExtractState::load(repo_root);
    if state.last_extracted_head.as_deref() == Some(current_head.as_str()) {
        return ExtractRunSummary {
            head: Some(current_head),
            skip_reason: Some("head unchanged since last extraction"),
            ..Default::default()
        };
    }

    let diff = match build_diff(
        repo_root,
        state.last_extracted_head.as_deref(),
        &current_head,
    ) {
        Some(d) if !d.is_empty() => d,
        Some(_) => {
            // Empty diff between two distinct heads — should be
            // rare but possible (e.g. an empty-merge commit).
            // Advance the marker so we don't keep retrying.
            state.last_extracted_head = Some(current_head.clone());
            let _ = state.save(repo_root);
            return ExtractRunSummary {
                head: Some(current_head),
                skip_reason: Some("empty diff range"),
                ..Default::default()
            };
        }
        None => {
            return ExtractRunSummary {
                head: Some(current_head),
                skip_reason: Some("git diff failed"),
                ..Default::default()
            };
        }
    };

    let existing_notes = read_existing_decision_notes(notes, 100).await;
    let request = ExtractionRequest {
        diff_text: diff,
        session_summary: Some(format!("audit extraction at HEAD {current_head}")),
        existing_notes,
    };

    let extractor = DiffDecisionExtractor::new(BackendRef(backend));
    let extractions = extractor.extract(&request).await;

    let dedup_seed = existing_extracted_bodies(notes).await;
    let mut written = 0_usize;
    for ext in extractions {
        if dedup_seed.contains(&ext.body) {
            continue;
        }
        if let Err(e) = persist_extracted(notes, &current_head, &ext).await {
            tracing::warn!(
                head = %current_head,
                error = %e,
                "audit_extract: failed to persist extracted note"
            );
            continue;
        }
        written += 1;
    }

    // Always advance the head marker after the run — even if
    // `written == 0`. The model decided there were no surfaceable
    // decisions in this range; we don't want to re-spend the
    // tokens next time. If the operator wants a fresh pass, they
    // delete `.sovereign/audit_state.toml`.
    state.last_extracted_head = Some(current_head.clone());
    if let Err(e) = state.save(repo_root) {
        tracing::warn!(
            error = %e,
            "audit_extract: could not save audit_state.toml; \
             next run will redo this range"
        );
    }

    ExtractRunSummary {
        ran: true,
        head: Some(current_head),
        written,
        skip_reason: None,
    }
}

/// Production entry point: load `~/.sovereign/config.toml`,
/// derive `(daemon_url, model_id)`, build a [`LocalLlmBackend`],
/// and call [`run`].
///
/// Returns the same [`ExtractRunSummary`] shape with
/// `skip_reason="no setup config"` if the config file is missing
/// or unreadable. The audit's other extraction streams (agent /
/// committed / observed / inferred-via-recover) keep the floor
/// non-empty even when this skips.
pub async fn run_with_default_backend(repo_root: &Path, notes: &NoteStore) -> ExtractRunSummary {
    let setup = match sovereign_core::setup_config::SetupConfig::load() {
        Ok(s) => s,
        Err(e) => {
            tracing::info!(
                error = %e,
                "audit_extract: no setup config; skipping LLM-backed extraction"
            );
            return ExtractRunSummary {
                skip_reason: Some("no setup config (~/.sovereign/config.toml)"),
                ..Default::default()
            };
        }
    };

    // Daemon URL — read the configured client port (defaults to
    // 9741) and bind to localhost. The daemon is local-only by
    // design, so we never reach over the network here.
    let daemon_url = format!("http://127.0.0.1:{}", setup.daemon.client_port);

    // Primary model id is the GGUF file stem — see
    // `setup_config::ModelsSection.primary` doc comment + the
    // slot manager's resolve-by-stem convention.
    let model_id = match setup
        .models
        .primary
        .file_stem()
        .and_then(|s| s.to_str())
        .map(String::from)
    {
        Some(s) => s,
        None => {
            return ExtractRunSummary {
                skip_reason: Some("primary model path has no file stem"),
                ..Default::default()
            };
        }
    };

    let config = LocalLlmConfig::for_daemon(daemon_url, model_id);
    let backend = match LocalLlmBackend::new(config) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "audit_extract: cannot build LocalLlmBackend; skipping"
            );
            return ExtractRunSummary {
                skip_reason: Some("backend construction failed"),
                ..Default::default()
            };
        }
    };
    run(repo_root, notes, &backend).await
}

/// Adapter so we can pass a `&dyn DecisionExtractorBackend` to
/// [`DiffDecisionExtractor::new`], which takes ownership of a
/// type implementing the trait. Avoids forcing callers to clone
/// or `Arc`-wrap their backend.
struct BackendRef<'a>(&'a dyn DecisionExtractorBackend);

#[async_trait::async_trait]
impl<'a> DecisionExtractorBackend for BackendRef<'a> {
    async fn extract(
        &self,
        request: &ExtractionRequest,
    ) -> Result<Vec<DecisionExtraction>, String> {
        self.0.extract(request).await
    }
}

/// Persist one extraction as a `source='extracted'` note. The
/// `head` is recorded as `related_entity` so the audit's reversal
/// display can correlate the note with the commit range it came
/// from.
async fn persist_extracted(
    notes: &NoteStore,
    head: &str,
    ext: &DecisionExtraction,
) -> Result<(), String> {
    notes
        .write_note_with_source(
            &ext.kind,
            &ext.body,
            Vec::new(),
            Vec::new(),
            "audit-extract",
            NoteScope::Global,
            None,
            Some(head),
            NoteSource::Extracted,
            ext.supersedes.as_deref(),
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// Stub backend that returns whatever the test installed.
    struct StubBackend(Vec<DecisionExtraction>);

    #[async_trait]
    impl DecisionExtractorBackend for StubBackend {
        async fn extract(
            &self,
            _request: &ExtractionRequest,
        ) -> Result<Vec<DecisionExtraction>, String> {
            Ok(self.0.clone())
        }
    }

    /// Helper: init a tempdir as a git repo with one commit so
    /// `git rev-parse HEAD` returns a valid hash.
    fn init_git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "t@e.com"],
            vec!["config", "user.name", "T"],
        ] {
            assert!(std::process::Command::new("git")
                .args(&args)
                .current_dir(r)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(r.join("README.md"), b"# r\n").unwrap();
        assert!(std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(r)
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args([
                "commit",
                "-m",
                "baseline commit one two three four five six"
            ])
            .current_dir(r)
            .status()
            .unwrap()
            .success());
        dir
    }

    /// Append a second commit so HEAD changes and a diff range exists.
    fn append_commit(repo: &Path) {
        std::fs::write(repo.join("README.md"), b"# r\n# second\n").unwrap();
        assert!(std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args([
                "commit",
                "-m",
                "Switch storage to async channels for ingest"
            ])
            .current_dir(repo)
            .status()
            .unwrap()
            .success());
    }

    /// Happy path: backend produces decisions, they land as
    /// extracted notes, the head marker advances.
    #[tokio::test]
    async fn run_persists_extracted_notes_and_advances_head() {
        let dir = init_git_repo();
        let notes_db = dir.path().join(".sovereign").join("notes.db");
        std::fs::create_dir_all(notes_db.parent().unwrap()).unwrap();
        let notes = NoteStore::open(&notes_db).unwrap();

        let backend = StubBackend(vec![
            DecisionExtraction {
                kind: "decision".into(),
                body: "switch to async channels".into(),
                supersedes: None,
            },
            DecisionExtraction {
                kind: "deviation".into(),
                body: "drops strict ordering".into(),
                supersedes: None,
            },
        ]);
        let summary = run(dir.path(), &notes, &backend).await;
        assert!(summary.ran);
        assert_eq!(summary.written, 2);
        assert!(summary.head.is_some());

        // Verify both rows exist with source='extracted'.
        let rows = notes
            .read_notes(None, &[], &[], &[], 100, false)
            .await
            .unwrap();
        let extracted: Vec<_> = rows
            .iter()
            .filter(|n| n.source == NoteSource::Extracted.as_str())
            .collect();
        assert_eq!(extracted.len(), 2);

        // Audit state file written.
        let state_path = audit_state_path(dir.path());
        assert!(state_path.exists());
        let state = AuditExtractState::load(dir.path());
        assert_eq!(
            state.last_extracted_head.as_deref(),
            summary.head.as_deref()
        );
    }

    /// Re-running with the same head short-circuits — no backend
    /// call, no notes written.
    #[tokio::test]
    async fn run_short_circuits_when_head_unchanged() {
        let dir = init_git_repo();
        let notes_db = dir.path().join(".sovereign").join("notes.db");
        std::fs::create_dir_all(notes_db.parent().unwrap()).unwrap();
        let notes = NoteStore::open(&notes_db).unwrap();

        // First pass writes one note + advances head.
        let backend = StubBackend(vec![DecisionExtraction {
            kind: "decision".into(),
            body: "first run decision".into(),
            supersedes: None,
        }]);
        let s1 = run(dir.path(), &notes, &backend).await;
        assert!(s1.ran);
        assert_eq!(s1.written, 1);

        // Second pass with the SAME head: short-circuit. Even
        // though the stub backend would happily produce a row,
        // we never reach it.
        let s2 = run(dir.path(), &notes, &backend).await;
        assert!(!s2.ran, "should have short-circuited");
        assert_eq!(s2.skip_reason, Some("head unchanged since last extraction"));
        assert_eq!(s2.written, 0);
    }

    /// Body-content dedup: even if the head marker is forced
    /// forward by adding a new commit, an extraction whose body
    /// duplicates a prior one is skipped.
    #[tokio::test]
    async fn run_dedups_by_body_when_extractor_repeats() {
        let dir = init_git_repo();
        let notes_db = dir.path().join(".sovereign").join("notes.db");
        std::fs::create_dir_all(notes_db.parent().unwrap()).unwrap();
        let notes = NoteStore::open(&notes_db).unwrap();

        let body = "switch to async channels";
        let backend = StubBackend(vec![DecisionExtraction {
            kind: "decision".into(),
            body: body.into(),
            supersedes: None,
        }]);

        // First run.
        let s1 = run(dir.path(), &notes, &backend).await;
        assert_eq!(s1.written, 1);

        // Force a new head + run again with the SAME extraction.
        append_commit(dir.path());
        let s2 = run(dir.path(), &notes, &backend).await;
        assert!(s2.ran);
        assert_eq!(s2.written, 0, "duplicate body should have been deduped");

        // Total extracted notes is still 1.
        let rows = notes
            .read_notes(None, &[], &[], &[], 100, false)
            .await
            .unwrap();
        assert_eq!(
            rows.iter()
                .filter(|n| n.source == NoteSource::Extracted.as_str())
                .count(),
            1
        );
    }

    /// Backend returns an empty Vec → run is "successful", marker
    /// advances, no notes land.
    #[tokio::test]
    async fn empty_extractions_still_advance_head_marker() {
        let dir = init_git_repo();
        let notes_db = dir.path().join(".sovereign").join("notes.db");
        std::fs::create_dir_all(notes_db.parent().unwrap()).unwrap();
        let notes = NoteStore::open(&notes_db).unwrap();

        let backend = StubBackend(vec![]);
        let s1 = run(dir.path(), &notes, &backend).await;
        assert!(s1.ran);
        assert_eq!(s1.written, 0);
        let head_after_first = s1.head.clone().unwrap();

        // Second run with same head → short-circuit (proves the
        // marker advanced even on zero-write run).
        let s2 = run(dir.path(), &notes, &backend).await;
        assert!(!s2.ran);
        assert_eq!(s2.skip_reason, Some("head unchanged since last extraction"));
        assert_eq!(s2.head.as_deref(), Some(head_after_first.as_str()));
    }

    /// Running outside a git repo yields a clean skip with the
    /// "not a git repo" reason.
    #[tokio::test]
    async fn run_outside_git_repo_skips() {
        let dir = tempfile::tempdir().unwrap();
        let notes_db = dir.path().join(".sovereign").join("notes.db");
        std::fs::create_dir_all(notes_db.parent().unwrap()).unwrap();
        let notes = NoteStore::open(&notes_db).unwrap();

        let backend = StubBackend(vec![]);
        let s = run(dir.path(), &notes, &backend).await;
        assert!(!s.ran);
        assert_eq!(s.skip_reason, Some("not a git repo"));
    }

    /// `AuditExtractState` round-trips through TOML with a
    /// stable shape so an operator can edit the file by hand.
    #[test]
    fn audit_extract_state_round_trips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AuditExtractState::default();
        state.last_extracted_head = Some("abc123".into());
        std::fs::create_dir_all(dir.path().join(".sovereign")).unwrap();
        state.save(dir.path()).unwrap();

        let loaded = AuditExtractState::load(dir.path());
        assert_eq!(loaded.last_extracted_head.as_deref(), Some("abc123"));

        // Hand-readable: file contains the field name.
        let raw = std::fs::read_to_string(audit_state_path(dir.path())).unwrap();
        assert!(raw.contains("last_extracted_head"));
    }

    /// Default state (file missing) loads cleanly.
    #[test]
    fn audit_extract_state_load_missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let state = AuditExtractState::load(dir.path());
        assert!(state.last_extracted_head.is_none());
    }

    /// Malformed TOML doesn't panic — falls back to default with
    /// a warn-level log. (We just verify it doesn't crash.)
    #[test]
    fn audit_extract_state_load_malformed_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".sovereign")).unwrap();
        std::fs::write(audit_state_path(dir.path()), b"= = invalid = =").unwrap();
        let state = AuditExtractState::load(dir.path());
        assert!(state.last_extracted_head.is_none());
    }
}
