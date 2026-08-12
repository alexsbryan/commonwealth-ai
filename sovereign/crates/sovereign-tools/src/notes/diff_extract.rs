// SPDX-License-Identifier: AGPL-3.0-or-later
//! Diff-based decision extractor (Phase 7.2).
//!
//! Reads the cumulative diff of files modified during a session
//! and asks an LLM (or a stub, in tests) to extract decisions and
//! reversals. Higher-confidence than the regex `response_mine` —
//! the diff is the actual code change, not assistant prose, so
//! the LLM has structural context.
//!
//! ## Pipeline
//!
//! 1. `git diff <session_start>..HEAD` → string. Caller fetches.
//! 2. Read existing notes for the session's feature scope. Lets
//!    the prompt notice contradictions ("the prior note said X
//!    but the diff implements not-X").
//! 3. Feed both into [`DecisionExtractorBackend::extract`]. The
//!    backend produces a list of [`DecisionExtraction`]s, each
//!    optionally with a `supersedes` link to a prior note id.
//! 4. Caller persists each as a `source='extracted'` note via
//!    `NoteStore::write_note_with_source(...)`.
//!
//! ## Why a trait, not a concrete LLM call here
//!
//! Backend selection (qwen-27B vs gemma-31B vs an external API)
//! and the inference adapter live elsewhere
//! (`sovereign-mesh::inference_adapter` etc.). This module owns
//! the prompt construction, output parsing, and contradiction-
//! detection contract. Having a small trait keeps unit tests
//! pure — the test suite covers prompt shape and parsing without
//! pulling a model into CI.
//!
//! Phase 7.3 wires a real backend at audit-assembly time
//! (`sovereign audit` runs the extractor lazily on the
//! cumulative diff).
//!
//! ## Token budget
//!
//! Spec calls for ~500 tokens per session. We don't enforce that
//! here — the backend is responsible for picking an appropriate
//! model size and trimming the diff if needed. The pure logic
//! caps the diff input at [`MAX_DIFF_INPUT_BYTES`] so a runaway
//! commit doesn't blow up the prompt accidentally.

use async_trait::async_trait;

use corpus_engine_notes::NoteRow;

/// Maximum diff text fed to the backend, in bytes. ~80 KB is
/// roughly the upper limit of a useful single-session diff
/// (anything larger usually means a non-targeted change set
/// where extraction would be noise anyway). Backends that can
/// handle more are free to ignore this and accept the full
/// diff via [`DecisionExtractorBackend::extract`] — the cap is
/// applied only at [`build_prompt`].
pub const MAX_DIFF_INPUT_BYTES: usize = 80_000;

/// Maximum number of decisions a single backend call may
/// surface. Keeps the audit's "Decisions" section human-readable
/// at the limit.
pub const MAX_EXTRACTIONS_PER_CALL: usize = 8;

/// One decision extracted from the diff. The body is intended
/// for direct insertion as a note; the kind is one of the
/// schema's accepted kinds (typically `decision` for committed
/// choices, `deviation` for changes that disagree with an
/// existing approval, `invariant` for newly-discovered constraints).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionExtraction {
    /// Note kind to use. Caller validates it's in the schema's
    /// accepted set; the backend produces strings, this struct
    /// passes them through.
    pub kind: String,
    /// Body of the note. Plain prose. The audit renderer wraps.
    pub body: String,
    /// If this extraction reverses a prior note, the prior
    /// note's id. Caller writes the new note with
    /// `supersedes = Some(this id)` so the audit's reversal
    /// display can render both.
    pub supersedes: Option<String>,
}

/// Inputs the backend needs to do its job.
///
/// `existing_notes` is the set of `kind='decision' | 'invariant'`
/// notes that already exist for the feature scope, ordered
/// newest first. The backend uses these to detect
/// contradictions: a new decision that disagrees with an
/// existing one should set `supersedes = Some(prior.id)`.
#[derive(Debug, Clone)]
pub struct ExtractionRequest {
    /// `git diff <session_start>..HEAD` output. May be capped
    /// before reaching here — backends that want the full diff
    /// pull from the source themselves.
    pub diff_text: String,
    /// Optional summary line ("session-end at HEAD abc123…").
    /// Useful for the audit to say "extracted from session
    /// ending at <hash>" without re-fetching git state.
    pub session_summary: Option<String>,
    /// Notes already on file for the feature scope. Backend
    /// reads `id`, `kind`, and `content`; other columns are
    /// ignored.
    pub existing_notes: Vec<NoteRow>,
}

/// Trait for "I can read a diff and produce decisions." The
/// production implementation calls a local model via the
/// inference adapter; the test implementation is a hand-rolled
/// stub.
#[async_trait]
pub trait DecisionExtractorBackend: Send + Sync {
    /// Extract decisions from the supplied diff + existing-notes
    /// context. Errors are returned as a string so the caller
    /// (audit assembly) can log without dragging error types
    /// across crate boundaries.
    async fn extract(&self, request: &ExtractionRequest)
        -> Result<Vec<DecisionExtraction>, String>;
}

/// Build the focused-extraction prompt that the backend feeds to
/// its model. Pulled out into a free function so unit tests can
/// assert prompt shape (which is part of the contract — agents
/// reading this prompt should know what's being asked of them).
///
/// The prompt has three parts:
///
/// 1. Hard-coded preamble explaining the task and output format.
/// 2. Existing-notes context (each summarised to one line).
/// 3. The diff itself (capped at [`MAX_DIFF_INPUT_BYTES`]).
///
/// Output format requested: one JSON object per line, each with
/// `kind`, `body`, optional `supersedes_id`. Backends are
/// expected to constrain the model to that shape.
pub fn build_prompt(req: &ExtractionRequest) -> String {
    let mut s = String::new();
    s.push_str(
        "You are extracting decisions from a code diff for an audit. \
         Identify each meaningful decision the author appears to have \
         made. Skip mechanical changes (renames, formatting, \
         dependency bumps). For each decision, output one JSON object \
         on its own line with these keys:\n\
         \n\
         - kind: one of `decision`, `deviation`, `invariant`\n\
         - body: a single-sentence description of the decision\n\
         - supersedes_id: optional; the id of an existing note this \
         decision reverses\n\
         \n\
         Output at most ",
    );
    s.push_str(&MAX_EXTRACTIONS_PER_CALL.to_string());
    s.push_str(
        " decisions, ordered most-significant first. Output nothing \
         but the JSON objects (no prose, no markdown).\n",
    );
    if let Some(summary) = req.session_summary.as_deref() {
        s.push_str("\nSession: ");
        s.push_str(summary);
        s.push('\n');
    }
    if !req.existing_notes.is_empty() {
        s.push_str("\nExisting notes for this feature (newest first):\n");
        for n in &req.existing_notes {
            s.push_str("- id=");
            s.push_str(&n.id);
            s.push_str(" kind=");
            s.push_str(&n.kind);
            s.push_str(": ");
            // Trim multi-paragraph bodies down to the first line so
            // the prompt stays scannable.
            let first_line = n.content.lines().next().unwrap_or("").trim();
            s.push_str(first_line);
            s.push('\n');
        }
    }
    s.push_str("\nDiff:\n");
    let cap = req.diff_text.len().min(MAX_DIFF_INPUT_BYTES);
    s.push_str(&req.diff_text[..cap]);
    if cap < req.diff_text.len() {
        s.push_str("\n[diff truncated]\n");
    }
    s
}

/// Parse the backend's line-delimited JSON output back into
/// [`DecisionExtraction`]s. Tolerates blank lines and stray
/// non-JSON lines (skipped with a warning logged at trace level
/// — never bubbled up). Caps at [`MAX_EXTRACTIONS_PER_CALL`].
pub fn parse_extractions(raw: &str) -> Vec<DecisionExtraction> {
    let mut out = Vec::new();
    for line in raw.lines() {
        if out.len() >= MAX_EXTRACTIONS_PER_CALL {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            tracing::trace!(line = %trimmed, "diff_extract: ignoring non-JSON output line");
            continue;
        };
        let Some(kind) = value.get("kind").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(body) = value.get("body").and_then(|v| v.as_str()) else {
            continue;
        };
        if body.trim().is_empty() {
            continue;
        }
        let supersedes = value
            .get("supersedes_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        out.push(DecisionExtraction {
            kind: kind.to_string(),
            body: body.to_string(),
            supersedes,
        });
    }
    out
}

/// Adapter that ties a backend to the `ExtractionRequest` →
/// `Vec<DecisionExtraction>` flow. Production audits hold one
/// of these; tests construct ad-hoc with a stub backend.
pub struct DiffDecisionExtractor<B: DecisionExtractorBackend> {
    backend: B,
}

impl<B: DecisionExtractorBackend> DiffDecisionExtractor<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// Run the extractor end-to-end. Returns an empty Vec on
    /// backend failure (logged at warn level) — the audit's
    /// `extracted` source is best-effort; other extraction
    /// streams (response mining, commit harvest, observed
    /// patterns) hold the floor.
    pub async fn extract(&self, request: &ExtractionRequest) -> Vec<DecisionExtraction> {
        match self.backend.extract(request).await {
            Ok(items) => items.into_iter().take(MAX_EXTRACTIONS_PER_CALL).collect(),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "diff_extract: backend extraction failed; returning empty"
                );
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine_notes::{NoteScope, NoteSource};

    /// Helper: a NoteRow stub with the fields `build_prompt` reads.
    fn note(id: &str, kind: &str, body: &str) -> NoteRow {
        NoteRow {
            id: id.into(),
            kind: kind.into(),
            content: body.into(),
            symbols: Vec::new(),
            files: Vec::new(),
            session_id: "test".into(),
            created_at: "1970-01-01T00:00:00Z".into(),
            tool_name: None,
            retired_at: None,
            retired_by: None,
            scope: NoteScope::Global.as_str().to_string(),
            feature_id: None,
            promoted_from: None,
            related_entity: None,
            source: NoteSource::Agent.as_str().to_string(),
            supersedes: None,
            payload_json: None,
            origin_node_id: None,
            received_at: None,
            sent_at: None,
        }
    }

    /// Stub backend used in tests: returns whatever `Vec` the
    /// caller stored at construction time. Lets each test drive
    /// the parser's downstream behaviour without LLM I/O.
    struct StubBackend(pub Result<Vec<DecisionExtraction>, String>);

    #[async_trait]
    impl DecisionExtractorBackend for StubBackend {
        async fn extract(
            &self,
            _request: &ExtractionRequest,
        ) -> Result<Vec<DecisionExtraction>, String> {
            self.0.clone()
        }
    }

    /// `build_prompt` includes the preamble's instruction to emit
    /// JSON-per-line and lists existing notes one-per-line.
    #[test]
    fn build_prompt_includes_preamble_existing_notes_and_diff() {
        let req = ExtractionRequest {
            diff_text: "+let x = 1;".into(),
            session_summary: Some("session ending at abc123".into()),
            existing_notes: vec![note("n1", "decision", "Use BTreeMap")],
        };
        let prompt = build_prompt(&req);
        assert!(prompt.contains("JSON object"));
        assert!(prompt.contains("session ending at abc123"));
        assert!(prompt.contains("id=n1"));
        assert!(prompt.contains("Use BTreeMap"));
        assert!(prompt.contains("+let x = 1;"));
    }

    /// Multi-paragraph existing notes are summarised to first
    /// line so the prompt stays scannable.
    #[test]
    fn build_prompt_trims_existing_note_to_first_line() {
        let req = ExtractionRequest {
            diff_text: String::new(),
            session_summary: None,
            existing_notes: vec![note(
                "n1",
                "decision",
                "First line decision summary.\n\nLong second paragraph that \
                 should NOT appear in the prompt context.",
            )],
        };
        let prompt = build_prompt(&req);
        assert!(prompt.contains("First line decision summary."));
        assert!(!prompt.contains("Long second paragraph"));
    }

    /// Diff input bigger than the cap is truncated with a marker.
    #[test]
    fn build_prompt_caps_diff_input_at_max_bytes() {
        let big_diff = "x".repeat(MAX_DIFF_INPUT_BYTES + 100);
        let req = ExtractionRequest {
            diff_text: big_diff,
            session_summary: None,
            existing_notes: Vec::new(),
        };
        let prompt = build_prompt(&req);
        assert!(prompt.contains("[diff truncated]"));
        assert!(
            prompt.len() < MAX_DIFF_INPUT_BYTES + 1024,
            "prompt should be cap + small overhead, got {}",
            prompt.len()
        );
    }

    /// `parse_extractions` reads JSON-per-line and tolerates
    /// non-JSON lines + blank lines. Caps at MAX.
    #[test]
    fn parse_extractions_reads_json_per_line() {
        let raw = "\
            {\"kind\":\"decision\",\"body\":\"switch to async\"}\n\
            \n\
            random non-json line\n\
            {\"kind\":\"deviation\",\"body\":\"reduces strict ordering\",\"supersedes_id\":\"n42\"}\n";
        let out = parse_extractions(raw);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].kind, "decision");
        assert_eq!(out[0].body, "switch to async");
        assert_eq!(out[0].supersedes, None);
        assert_eq!(out[1].kind, "deviation");
        assert_eq!(out[1].supersedes.as_deref(), Some("n42"));
    }

    /// `parse_extractions` honours the cap.
    #[test]
    fn parse_extractions_honours_cap() {
        let mut raw = String::new();
        for i in 0..(MAX_EXTRACTIONS_PER_CALL + 5) {
            raw.push_str(&format!(
                "{{\"kind\":\"decision\",\"body\":\"thing {i}\"}}\n"
            ));
        }
        let out = parse_extractions(&raw);
        assert_eq!(out.len(), MAX_EXTRACTIONS_PER_CALL);
    }

    /// Rows missing required fields are skipped (not an error).
    #[test]
    fn parse_extractions_skips_invalid_rows() {
        let raw = "\
            {\"kind\":\"decision\"}\n\
            {\"body\":\"missing kind\"}\n\
            {\"kind\":\"decision\",\"body\":\"\"}\n\
            {\"kind\":\"decision\",\"body\":\"valid\"}\n";
        let out = parse_extractions(raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].body, "valid");
    }

    /// Empty `supersedes_id` is treated the same as absent.
    #[test]
    fn empty_supersedes_id_is_normalised_to_none() {
        let raw = "{\"kind\":\"decision\",\"body\":\"x\",\"supersedes_id\":\"\"}\n";
        let out = parse_extractions(raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].supersedes, None);
    }

    /// End-to-end with the stub backend: extractor returns what
    /// the backend supplied, capped.
    #[tokio::test]
    async fn extractor_round_trips_through_stub_backend() {
        let backend = StubBackend(Ok(vec![DecisionExtraction {
            kind: "decision".into(),
            body: "use atomic rename".into(),
            supersedes: None,
        }]));
        let extractor = DiffDecisionExtractor::new(backend);
        let req = ExtractionRequest {
            diff_text: "+ rename(temp, final);".into(),
            session_summary: None,
            existing_notes: Vec::new(),
        };
        let out = extractor.extract(&req).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].body, "use atomic rename");
    }

    /// Backend failure yields an empty Vec — `extracted` source is
    /// best-effort; other extraction streams hold the floor.
    #[tokio::test]
    async fn backend_error_yields_empty_vec() {
        let backend = StubBackend(Err("model unavailable".into()));
        let extractor = DiffDecisionExtractor::new(backend);
        let req = ExtractionRequest {
            diff_text: String::new(),
            session_summary: None,
            existing_notes: Vec::new(),
        };
        let out = extractor.extract(&req).await;
        assert!(out.is_empty());
    }
}
