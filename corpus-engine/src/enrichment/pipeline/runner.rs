//! Per-phase runner — glues the `Pipeline` trait, `ExemplarBank`,
//! `PhaseCache`, `RunOutputWriter`, and injected `EmbedFn` +
//! `ChatCompletionFn` into an executor the CLI calls per subcommand.
//!
//! Landing 2 implements phase 1 (per-chapter question extraction).
//! Subsequent phases land incrementally; each `phase_N_*` method is
//! additive and does not break the others.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::exemplar_bank::{Exemplar, ExemplarBank};
use super::phase_cache::PhaseCache;
use super::run_output::RunOutputWriter;
use super::trait_def::Pipeline;
use super::atlas::{SectionExtraction, SeedEntities, SeedOrigin, SeedStrategy};
use super::types::*;
use super::vector_clustering::cluster_vectors;
use crate::error::{Error, Result};
use crate::types::EmbedFn;

/// Which chapters the developer wants phase 1 to run against.
#[derive(Debug, Clone)]
pub enum ChapterSelection {
    /// Just the given chapter IDs. Output lands in `runs/` but the
    /// cache is NOT updated — subset runs are diagnostic, not ground
    /// truth for phases 2+.
    Subset(Vec<String>),
    /// Every chapter in the manifest. Overwrites the cache in one
    /// shot.
    Full,
    /// A recovery run targeting chapter IDs that failed in a prior
    /// pass. Semantically a subset, but successful results are merged
    /// into the existing cache (matching chapters overwritten, those
    /// chapters dropped from cached failures). Distinguishes
    /// retry-of-failed from ad-hoc diagnostic subsets so the operator
    /// doesn't have to hand-merge the run file after a successful
    /// retry.
    RetryFailed(Vec<String>),
}

impl ChapterSelection {
    pub fn mode_label(&self) -> &'static str {
        match self {
            Self::Subset(_) => "subset",
            Self::Full => "full",
            Self::RetryFailed(_) => "retry",
        }
    }

    /// True when a successful run should *overwrite* the cache in
    /// full (`Full` only).
    pub fn should_update_cache(&self) -> bool {
        matches!(self, Self::Full)
    }

    /// True when a successful run should *merge* into the existing
    /// cache (replace matching chapters, drop matching failures)
    /// rather than overwriting it.
    pub fn should_merge_into_cache(&self) -> bool {
        matches!(self, Self::RetryFailed(_))
    }
}

// ── Checkpoint (per-chapter, append-only) ────────────────────
//
// `_checkpoint.jsonl` is JSONL: one [`Phase1CheckpointEntry`] per
// line, appended immediately after each chapter completes (success
// or failure). Crash-resilience for long-running Phase 1 runs
// (Wikipedia-scale Tier-2 takes hours; a daemon/host crash mid-run
// would otherwise lose every successful chapter).
//
// Resume reads the file, builds a set of chapter ids already
// processed, and the CLI passes a `Subset` selection of the
// remainder. After enough invocations to cover every chapter, the
// `--finalize` path reads the JSONL once more and writes a
// canonical `Phase1Output` run-file from it.

/// One JSONL row in `_checkpoint.jsonl`. `Success` carries the full
/// `ExtractedQuestion` so a subsequent `--finalize` can rebuild the
/// run-file from the checkpoint alone — no need to keep prior
/// run-files alive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Phase1CheckpointEntry {
    Success {
        chapter_id: String,
        extracted: ExtractedQuestion,
    },
    Failure {
        chapter_id: String,
        failure: Phase1Failure,
    },
}

impl Phase1CheckpointEntry {
    pub fn chapter_id(&self) -> &str {
        match self {
            Self::Success { chapter_id, .. } => chapter_id,
            Self::Failure { chapter_id, .. } => chapter_id,
        }
    }
}

/// Read every entry from a Phase-1 checkpoint file. Empty / missing
/// file returns `Ok(Vec::new())` — both mean "nothing processed yet".
/// Malformed lines abort with an error rather than silently skipping
/// (a corrupted checkpoint should not produce a quietly-incomplete
/// resume).
pub fn read_phase1_checkpoint(path: &Path) -> Result<Vec<Phase1CheckpointEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Database(format!("read checkpoint {}: {e}", path.display())))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: Phase1CheckpointEntry = serde_json::from_str(line).map_err(|e| {
            Error::Serialization(format!(
                "checkpoint {} line {}: {e}",
                path.display(),
                i + 1
            ))
        })?;
        out.push(entry);
    }
    Ok(out)
}

/// Aggregate a checkpoint into `(extracted, failures)` deduped by
/// chapter_id with last-write-wins semantics. Used by `--finalize`
/// when reconstructing a canonical `Phase1Output`.
pub fn collapse_phase1_checkpoint(
    entries: Vec<Phase1CheckpointEntry>,
) -> (Vec<ExtractedQuestion>, Vec<Phase1Failure>) {
    let mut by_id_success: HashMap<String, ExtractedQuestion> = HashMap::new();
    let mut by_id_failure: HashMap<String, Phase1Failure> = HashMap::new();
    for entry in entries {
        match entry {
            Phase1CheckpointEntry::Success { chapter_id, extracted } => {
                by_id_failure.remove(&chapter_id);
                by_id_success.insert(chapter_id, extracted);
            }
            Phase1CheckpointEntry::Failure { chapter_id, failure } => {
                if !by_id_success.contains_key(&chapter_id) {
                    by_id_failure.insert(chapter_id, failure);
                }
            }
        }
    }
    let mut extracted: Vec<ExtractedQuestion> = by_id_success.into_values().collect();
    extracted.sort_by(|a, b| a.chapter_id.cmp(&b.chapter_id));
    let mut failures: Vec<Phase1Failure> = by_id_failure.into_values().collect();
    failures.sort_by(|a, b| a.chapter_id.cmp(&b.chapter_id));
    (extracted, failures)
}

/// Build the set of chapter ids the checkpoint considers processed
/// (success + failure). Resume callers feed this into the
/// `ChapterSelection` filter.
pub fn checkpoint_processed_ids(entries: &[Phase1CheckpointEntry]) -> HashSet<String> {
    entries
        .iter()
        .map(|e| e.chapter_id().to_string())
        .collect()
}

/// Append one entry to the checkpoint file. Atomic at the line
/// level (`writeln!` + `\n`), so a crash mid-write at most truncates
/// the in-flight line — the reader skips empty lines and aborts on
/// malformed ones.
fn append_phase1_checkpoint(path: &Path, entry: &Phase1CheckpointEntry) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::Database(format!(
                "create checkpoint parent {}: {e}",
                parent.display()
            ))
        })?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| Error::Database(format!("open checkpoint {}: {e}", path.display())))?;
    let line = serde_json::to_string(entry).map_err(|e| {
        Error::Serialization(format!("serialise checkpoint entry: {e}"))
    })?;
    writeln!(f, "{line}").map_err(|e| {
        Error::Database(format!("write checkpoint {}: {e}", path.display()))
    })?;
    Ok(())
}

/// Streaming progress events for phase 1.
#[derive(Debug, Clone)]
pub enum Phase1Progress<'a> {
    Start {
        total: usize,
        exemplars_loaded: usize,
    },
    ChapterStart {
        i: usize,
        total: usize,
        chapter_id: &'a str,
    },
    ChapterDone {
        chapter_id: &'a str,
        question_count: usize,
    },
    ChapterFailed {
        chapter_id: &'a str,
        reason: &'a str,
    },
    Done {
        produced: usize,
        failed: usize,
        run_path: &'a Path,
    },
}

/// Outcome of one phase-1 call.
#[derive(Debug, Clone)]
pub struct Phase1RunResult {
    pub output: Phase1Output,
    pub run_path: PathBuf,
    pub cache_updated: bool,
    pub failures: Vec<Phase1Failure>,
}

/// Cap on how much of a failing model response we carry forward.
///
/// We capture the *post-thinking* content — the substring the parser
/// actually tried to deserialize — not the reasoning preamble (see
/// `truncate_response_head`). 2 KB comfortably fits a full atlas JSON
/// response (typical size 0.8–1.6 KB) plus fenced markers, while
/// keeping a run file small even when dozens of chapters fail.
const FAILURE_HEAD_CHAR_CAP: usize = 2048;

/// Below this word count, a chapter is almost certainly a `Part`
/// heading or other front-matter section the regex picked up but
/// that has no body for the model to think about. Sending such a
/// chapter to chat produces either a refusal or a schema-template
/// echo — both wasted roundtrips. A real chapter, even a terse one,
/// comfortably clears this bar.
const MIN_PHASE1_CHAPTER_WORDS: usize = 40;

/// Output-token budget used for seed-threaded Phase 1 calls. The
/// seed block enlarges the prompt by a few hundred tokens of input,
/// but the real cost is in the reasoning trace the model produces
/// in response — it swells to match the new context, and the
/// six-facet JSON output starts to truncate on the standard
/// 16384 cap (observed in Landing 1 smoke test as `parse_drift` on
/// 3/5 chapters). 24576 gives the JSON enough headroom to finish.
/// Only applied when the pipeline has a seed to thread AND the
/// runner has a token-aware chat closure configured.
const PHASE1_SEED_OUTPUT_BUDGET: u32 = 24576;

/// Count whitespace-separated tokens. Good-enough proxy for "words";
/// the threshold only needs to be in the right order of magnitude.
fn approx_word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Classify a Phase 1 parse failure into a `PhaseFailureKind` the
/// CLI can route on. Structured, not string-sniffed — the raw model
/// response is the source of truth for whether the failure was a
/// think-truncation (unclosed `<think>` → no answer), schema drift
/// (JSON parsed at the envelope level but failed validation), empty
/// extraction (well-formed envelope, zero atoms), or generic parse
/// drift (malformed JSON after the thinking preamble).
///
/// The `reason` string is untouched; callers keep it for display
/// while the enum drives recovery decisions.
fn classify_phase1_parse_failure(response: &str, err: &Error) -> PhaseFailureKind {
    // The cheapest-and-strongest signal first: did the model ever
    // close its <think> block? If not, nothing past the preamble
    // could have landed — that's specifically a think-truncation.
    if is_truncated_thinking_response(response) {
        return PhaseFailureKind::ThinkTruncated;
    }

    // Otherwise classify by the error text the pipeline's parser
    // produced. We emit these strings from the atlas pipeline (see
    // `literary_atlas::parse_phase1`); the classifier maps them
    // into the enum without changing the reason the operator sees.
    let msg = format!("{err}");
    if msg.contains("did not extract anything") {
        PhaseFailureKind::EmptyExtraction
    } else {
        PhaseFailureKind::ParseDrift
    }
}

/// Capture the most diagnostically useful slice of a failing model
/// response, capped at `FAILURE_HEAD_CHAR_CAP` characters.
///
/// Thinking-capable models (Qwen3, DeepSeek R1, o1-family) emit a
/// 2–4 KB `<think>…</think>` reasoning trace followed by their actual
/// answer. When the parser fails on such a response, the reasoning
/// trace is almost always noise: what failed to parse lives *after*
/// `</think>`. Truncating from the front captures only the reasoning
/// and throws away the evidence.
///
/// This function prefers the post-thinking content — the substring
/// the parser actually tried to deserialise — and falls back to the
/// reasoning preamble only when thinking was truncated mid-trace
/// (an unclosed `<think>` with no answer emitted), flagging the case
/// so an operator reading the run file sees immediately which failure
/// mode hit.
///
/// Returns `None` when the input is entirely whitespace.
fn truncate_response_head(text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }

    // Normal path: capture what the parser saw, i.e. the reasoning-
    // stripped body. `strip_reasoning_tags` removes every complete
    // `<think>…</think>` span and drops an unclosed tail.
    let stripped = strip_reasoning_tags(text);
    let stripped_trim = stripped.trim();
    if !stripped_trim.is_empty() {
        return Some(cap_chars(stripped_trim, FAILURE_HEAD_CHAR_CAP));
    }

    // Fallback: nothing remained after stripping. Either the response
    // was pure reasoning with no answer, or `<think>` opened and never
    // closed (truncation mid-trace). In either case we want *some*
    // signal — show the reasoning head so the operator can see the
    // model was thinking when the budget ran out.
    let marker = if is_truncated_thinking_response(text) {
        "<think block truncated — answer never emitted>"
    } else {
        "<reasoning-only response — no answer after </think>>"
    };
    let preview = cap_chars(text.trim(), FAILURE_HEAD_CHAR_CAP.saturating_sub(marker.len() + 2));
    Some(format!("{marker}\n{preview}"))
}

/// Truncate `s` to at most `cap` characters (NOT bytes — must not
/// split a UTF-8 codepoint), appending a marker that records how many
/// characters were dropped.
fn cap_chars(s: &str, cap: usize) -> String {
    let total = s.chars().count();
    if total <= cap {
        return s.to_string();
    }
    let head: String = s.chars().take(cap).collect();
    let dropped = total - cap;
    format!("{head}… [+{dropped} chars]")
}

/// Flatten whitespace and cap at ~200 chars so a parse-failure line in
/// the CLI's progress stream stays on one terminal row. The full head
/// is still persisted to the run file.
fn one_line_excerpt(text: &str) -> String {
    const EXCERPT_CHAR_CAP: usize = 200;
    let flat: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() <= EXCERPT_CHAR_CAP {
        flat
    } else {
        let head: String = flat.chars().take(EXCERPT_CHAR_CAP).collect();
        format!("{head}…")
    }
}

/// Executor for one admin-harness pipeline run.
///
/// The CLI constructs a `PhaseRunner` once per subcommand (cheap)
/// and invokes the phase method matching its command. Heavy state
/// (loaded exemplars, LanceDB handles) lives behind the injected
/// closures and the `Arc<dyn Pipeline>` so this struct stays cheap
/// to clone.
pub struct PhaseRunner {
    pipeline: Arc<dyn Pipeline>,
    embed: EmbedFn,
    chat: ChatCompletionFn,
    /// Optional per-call token override closure. When a retry mode
    /// requests a specific `max_output_tokens` (e.g. a terse retry
    /// bumping from 4096 to 16384), the runner calls this closure
    /// instead of `chat` so the cap applies to the retry only —
    /// without mutating the shared client.
    ///
    /// Configured via `with_chat_with_tokens`. When unset, retries
    /// fall back to `chat` with its existing cap; the runner still
    /// swaps the prompt variant.
    chat_with_tokens: Option<ChatCompletionWithTokensFn>,
    cache: PhaseCache,
    runs: RunOutputWriter,
    exemplars_dir: PathBuf,
    /// When set, the Phase 1 loop appends one JSONL line per chapter
    /// (success or failure) to this path immediately after the chapter
    /// completes, BEFORE moving on to the next. Lets a long run
    /// survive a crash mid-flight: on restart, the caller reads the
    /// checkpoint, builds the set of already-processed chapter ids,
    /// and runs only the remainder. See
    /// `Phase1CheckpointEntry` for the on-disk shape.
    checkpoint_path: Option<PathBuf>,
}

impl PhaseRunner {
    pub fn new(
        pipeline: Arc<dyn Pipeline>,
        embed: EmbedFn,
        chat: ChatCompletionFn,
        cache: PhaseCache,
        runs: RunOutputWriter,
        exemplars_dir: impl AsRef<Path>,
    ) -> Self {
        Self {
            pipeline,
            embed,
            chat,
            chat_with_tokens: None,
            cache,
            runs,
            exemplars_dir: exemplars_dir.as_ref().to_path_buf(),
            checkpoint_path: None,
        }
    }

    /// Append per-chapter results to `path` as JSONL while Phase 1
    /// runs. The append happens after each chapter is processed, so a
    /// crash mid-flight loses at most one chapter (the one in flight
    /// at the moment of the crash). Resume reads the file via
    /// [`read_phase1_checkpoint`] and skips the recorded chapter ids.
    ///
    /// Set this on long runs (Wikipedia-scale Tier-2 enrichment).
    /// Short runs can leave it unset; the legacy "single run-file
    /// at the end" behaviour is preserved.
    pub fn with_checkpoint_path(mut self, path: impl AsRef<Path>) -> Self {
        self.checkpoint_path = Some(path.as_ref().to_path_buf());
        self
    }

    /// Best-effort: append a successful chapter to the checkpoint
    /// file. A write failure is logged but not fatal — the in-memory
    /// `extracted` vec still holds the result for the final run-file.
    /// Resume after a crash would lose this chapter, but the run can
    /// continue.
    fn persist_success_checkpoint(&self, ch: &ExtractedQuestion) {
        let Some(path) = self.checkpoint_path.as_deref() else {
            return;
        };
        let entry = Phase1CheckpointEntry::Success {
            chapter_id: ch.chapter_id.clone(),
            extracted: ch.clone(),
        };
        if let Err(e) = append_phase1_checkpoint(path, &entry) {
            tracing::warn!(
                error = %e,
                chapter_id = %ch.chapter_id,
                "phase1: checkpoint append (success) failed; run continues but resume may re-process this chapter"
            );
        }
    }

    /// Companion to `persist_success_checkpoint` for failures.
    fn persist_failure_checkpoint(&self, f: &Phase1Failure) {
        let Some(path) = self.checkpoint_path.as_deref() else {
            return;
        };
        let entry = Phase1CheckpointEntry::Failure {
            chapter_id: f.chapter_id.clone(),
            failure: f.clone(),
        };
        if let Err(e) = append_phase1_checkpoint(path, &entry) {
            tracing::warn!(
                error = %e,
                chapter_id = %f.chapter_id,
                "phase1: checkpoint append (failure) failed; run continues but resume may re-process this chapter"
            );
        }
    }

    /// Configure an optional per-call token-aware chat closure so
    /// retry modes (e.g. `RetryMode::Terse`) can raise the output
    /// budget for a single retry without rebuilding the chat client.
    pub fn with_chat_with_tokens(mut self, chat: ChatCompletionWithTokensFn) -> Self {
        self.chat_with_tokens = Some(chat);
        self
    }

    pub fn pipeline(&self) -> &Arc<dyn Pipeline> {
        &self.pipeline
    }

    pub fn cache(&self) -> &PhaseCache {
        &self.cache
    }

    pub fn runs(&self) -> &RunOutputWriter {
        &self.runs
    }

    pub fn exemplar_path(&self, phase: PipelinePhase) -> PathBuf {
        self.exemplars_dir.join(format!("{}.json", phase.id()))
    }

    /// Run Stage 1a — seed entity extraction. Dispatches on the
    /// pipeline's `seed_strategy()`:
    ///
    /// - `Llm`: calls `compose_seed_prompt` on the first section,
    ///   hands the prompt to the chat closure, parses the response
    ///   via `parse_seed_response`, and returns the typed seed
    ///   list wrapped in a `SeedEntities` record.
    /// - `Structural`: calls `extract_seed_structural` with the
    ///   supplied `CorpusContext`. No chat call.
    /// - `None`: returns `Ok(None)` — the pipeline does not need a
    ///   seed list.
    ///
    /// On success the result is written to `cache/seed.json` so
    /// subsequent `phase_1_extract_questions_with_retry` calls can
    /// read it without re-running Stage 1a. `force_refresh = true`
    /// skips the cache-read optimisation and always recomputes.
    pub async fn phase_1a_extract_seed(
        &self,
        corpus_id: &str,
        ctx: &CorpusContext,
        force_refresh: bool,
    ) -> Result<Option<SeedEntities>> {
        // Fast path: cache hit + not forced.
        if !force_refresh {
            if let Some(cached) = self
                .cache
                .read::<SeedEntities>(PipelinePhase::SeedExtraction)?
            {
                return Ok(Some(cached));
            }
        }

        match self.pipeline.seed_strategy() {
            SeedStrategy::None => Ok(None),
            SeedStrategy::Llm => {
                let first_chapter = ctx
                    .chapters
                    .first()
                    .ok_or_else(|| {
                        Error::InvalidInput(
                            "cannot run Stage 1a seed extraction: corpus has no chapters"
                                .into(),
                        )
                    })?;
                let prompt = self
                    .pipeline
                    .compose_seed_prompt(first_chapter)
                    .ok_or_else(|| {
                        Error::InvalidInput(format!(
                            "pipeline `{}` declares SeedStrategy::Llm but compose_seed_prompt \
                             returned None — override the method or declare a different strategy",
                            self.pipeline.id()
                        ))
                    })?;
                let response = (self.chat)(&prompt).await?;
                let entries = self.pipeline.parse_seed_response(&response)?;
                let seed = SeedEntities {
                    schema_version: SeedEntities::SCHEMA_VERSION,
                    corpus_id: corpus_id.to_string(),
                    origin: SeedOrigin::Llm,
                    entries,
                    written_at: now_rfc3339(),
                };
                self.cache
                    .write(PipelinePhase::SeedExtraction, &seed)?;
                Ok(Some(seed))
            }
            SeedStrategy::Structural => {
                let entries = self.pipeline.extract_seed_structural(ctx)?;
                let seed = SeedEntities {
                    schema_version: SeedEntities::SCHEMA_VERSION,
                    corpus_id: corpus_id.to_string(),
                    origin: SeedOrigin::Structural,
                    entries,
                    written_at: now_rfc3339(),
                };
                self.cache
                    .write(PipelinePhase::SeedExtraction, &seed)?;
                Ok(Some(seed))
            }
        }
    }

    /// Run phase 1 against the supplied chapters with no retry mode —
    /// the default path for `sovereign enrich extract`.
    ///
    /// Thin shim over
    /// [`phase_1_extract_questions_with_retry`] that passes `None`.
    pub async fn phase_1_extract_questions<F>(
        &self,
        chapters: &[ChapterInput],
        selection: &ChapterSelection,
        progress: F,
    ) -> Result<Phase1RunResult>
    where
        F: Fn(Phase1Progress<'_>),
    {
        self.phase_1_extract_questions_with_retry(chapters, selection, None, progress)
            .await
    }

    /// Run phase 1 against the supplied chapters, optionally with a
    /// retry mode that selects an alternate prompt variant and/or
    /// token budget.
    ///
    /// `retry_mode = None` is the default path (system prompt from
    /// `pipeline.compose_phase1`, shared chat closure).
    /// `retry_mode = Some(Terse { max_output_tokens })` dispatches to
    /// `pipeline.compose_phase1_terse` and, when the runner has a
    /// `chat_with_tokens` closure configured, uses it with the
    /// requested cap. A pipeline that doesn't implement
    /// `compose_phase1_terse` (returns `None`) causes an immediate
    /// error — the runner will not silently fall back to the default
    /// prompt for a terse retry.
    pub async fn phase_1_extract_questions_with_retry<F>(
        &self,
        chapters: &[ChapterInput],
        selection: &ChapterSelection,
        retry_mode: Option<RetryMode>,
        progress: F,
    ) -> Result<Phase1RunResult>
    where
        F: Fn(Phase1Progress<'_>),
    {
        // Resolve which chapters to run.
        let targets: Vec<&ChapterInput> = match selection {
            ChapterSelection::Full => chapters.iter().collect(),
            ChapterSelection::Subset(ids) | ChapterSelection::RetryFailed(ids) => {
                let mut picked = Vec::with_capacity(ids.len());
                for id in ids {
                    let found = chapters.iter().find(|c| c.chapter_id == *id).ok_or_else(
                        || Error::InvalidInput(format!("chapter not found in manifest: {id}")),
                    )?;
                    picked.push(found);
                }
                picked
            }
        };
        if targets.is_empty() {
            return Err(Error::InvalidInput(
                "phase 1 was asked to run with zero target chapters".into(),
            ));
        }

        // Load the exemplar bank. Bank presence is optional — phase 1
        // runs with an empty bank (no few-shot context) the first time
        // through.
        let exemplar_path = self.exemplar_path(PipelinePhase::Questions);
        let bank = ExemplarBank::load_embedded(
            &exemplar_path,
            PipelinePhase::Questions,
            &self.embed,
        )
        .await?;
        let k = self.pipeline.top_k_exemplars(PipelinePhase::Questions);

        progress(Phase1Progress::Start {
            total: targets.len(),
            exemplars_loaded: bank.len(),
        });

        // Read the Stage 1a seed list once per map loop. A cache
        // miss is non-fatal — the pipeline's compose_phase1_with_seed
        // default falls through to the seedless prompt. Pipelines
        // with `SeedStrategy::None` never write this cache entry.
        let seed_opt: Option<SeedEntities> =
            self.cache.read(PipelinePhase::SeedExtraction)?;

        let mut extracted: Vec<ExtractedQuestion> = Vec::with_capacity(targets.len());
        let mut failures: Vec<Phase1Failure> = Vec::new();

        for (i, chapter) in targets.iter().enumerate() {
            progress(Phase1Progress::ChapterStart {
                i: i + 1,
                total: targets.len(),
                chapter_id: &chapter.chapter_id,
            });

            // Skip sections with essentially no body. The chapter-regex
            // detector treats `Part I` headings as their own section
            // even though the body between them and the first real
            // chapter is just front-matter (e.g. "Book I. The History
            // Of A Family\n\n"). Sending that to chat produces a
            // schema-template echo — not a real analysis. Register the
            // skip as a failure so the run file surfaces it rather
            // than silently caching `"..."`.
            let words = approx_word_count(&chapter.text);
            if words < MIN_PHASE1_CHAPTER_WORDS {
                let reason = format!(
                    "skipped: chapter body is too short to analyze ({words} words < \
                     {MIN_PHASE1_CHAPTER_WORDS} word minimum) — likely a Part-level \
                     heading or front-matter section"
                );
                progress(Phase1Progress::ChapterFailed {
                    chapter_id: &chapter.chapter_id,
                    reason: &reason,
                });
                let failure = Phase1Failure {
                    chapter_id: chapter.chapter_id.clone(),
                    reason,
                    raw_response_head: None,
                    failure_kind: PhaseFailureKind::Skipped,
                };
                self.persist_failure_checkpoint(&failure);
                failures.push(failure);
                continue;
            }

            // Build the query-side embedding used to score exemplars
            // against this chapter.
            let query_text = phase1_query_text(chapter);
            let picked: Vec<&Exemplar> = if bank.is_empty() {
                Vec::new()
            } else {
                let query_emb = (self.embed)(&query_text).await?;
                bank.select_top_k(&query_emb, k)
            };

            // Prompt composition + chat dispatch branch on retry
            // mode. Default: `compose_phase1_with_seed` (threads the
            // Stage 1a seed list if available) + shared `chat`.
            // Terse: `compose_phase1_terse` (error if pipeline doesn't
            // support it) + token-aware chat if configured, else
            // fall back to shared `chat` with its existing cap.
            //
            // Seed lookup: the cache holds the Stage 1a output. Read
            // once per map loop, not once per chapter — all chapters
            // share the same seed.
            let prompt = match retry_mode {
                Some(RetryMode::Terse { .. }) => match self.pipeline.compose_phase1_terse(chapter) {
                    Some(p) => p,
                    None => {
                        return Err(Error::InvalidInput(format!(
                            "retry mode `terse` requested, but pipeline `{}` does not \
                             implement `compose_phase1_terse`. Either use a pipeline that \
                             supports a terse variant (e.g. `literary_atlas`) or drop the \
                             --terse flag.",
                            self.pipeline.id()
                        )));
                    }
                },
                None => {
                    let seed = seed_opt.as_ref();
                    self.pipeline
                        .compose_phase1_with_seed(chapter, &picked, seed)
                }
            };

            let chat_result = match retry_mode {
                Some(RetryMode::Terse { max_output_tokens }) => match &self.chat_with_tokens {
                    Some(chat_t) => (chat_t)(&prompt, max_output_tokens).await,
                    None => (self.chat)(&prompt).await,
                },
                None => {
                    // Seed-threaded default path gets a per-call
                    // output-budget bump. The seed block adds ~500
                    // tokens of input, the reasoning trace typically
                    // grows to match the extra context, and the
                    // six-facet JSON output then starves on the
                    // standard 16384 cap — observed as `parse_drift`
                    // on 3/5 chapters in the Landing 1 smoke test.
                    // Cap at `PHASE1_SEED_OUTPUT_BUDGET` so a chapter
                    // that was going to truncate mid-relations now
                    // has headroom to finish its JSON on first pass.
                    // Non-seed runs keep the baseline — they never
                    // starved.
                    match (seed_opt.as_ref(), &self.chat_with_tokens) {
                        (Some(_), Some(chat_t)) => {
                            (chat_t)(&prompt, PHASE1_SEED_OUTPUT_BUDGET).await
                        }
                        _ => (self.chat)(&prompt).await,
                    }
                }
            };

            let response = match chat_result {
                Ok(r) => r,
                Err(e) => {
                    let reason = format!("chat error: {e}");
                    progress(Phase1Progress::ChapterFailed {
                        chapter_id: &chapter.chapter_id,
                        reason: &reason,
                    });
                    let failure = Phase1Failure {
                        chapter_id: chapter.chapter_id.clone(),
                        reason,
                        // No response body ever arrived — nothing to capture.
                        raw_response_head: None,
                        failure_kind: PhaseFailureKind::ChatError,
                    };
                    self.persist_failure_checkpoint(&failure);
                    failures.push(failure);
                    continue;
                }
            };

            let parsed = match self.pipeline.parse_phase1(&response) {
                Ok(p) => p,
                Err(e) => {
                    let head = truncate_response_head(&response);
                    // Keep a one-line excerpt in `reason` so the CLI's
                    // streaming progress printer shows something useful
                    // without having to open the run file. The full head
                    // still goes to `raw_response_head` for the file.
                    let excerpt = head
                        .as_deref()
                        .map(one_line_excerpt)
                        .unwrap_or_else(|| "<empty response>".into());
                    let reason =
                        format!("parse error: {e} | response[head]: {excerpt}");
                    progress(Phase1Progress::ChapterFailed {
                        chapter_id: &chapter.chapter_id,
                        reason: &reason,
                    });
                    let failure_kind = classify_phase1_parse_failure(&response, &e);
                    let failure = Phase1Failure {
                        chapter_id: chapter.chapter_id.clone(),
                        reason,
                        raw_response_head: head,
                        failure_kind,
                    };
                    self.persist_failure_checkpoint(&failure);
                    failures.push(failure);
                    continue;
                }
            };

            progress(Phase1Progress::ChapterDone {
                chapter_id: &chapter.chapter_id,
                question_count: parsed.questions.len(),
            });

            // Stamp the runner-known chapter_id over whatever the model
            // emitted as `section_id`. Models routinely truncate or
            // mangle the section id (`sec_00`, `sec_002`, etc. instead
            // of `sec_0001`); preserving those mangled ids cascades
            // into Phase 2 cluster refs that don't resolve in Phase 3,
            // producing empty clusters that fail naming. We always
            // know which chapter we sent — trust that, not the echo.
            let mut section_extraction = parsed.section_extraction;
            if let Some(ref mut sx) = section_extraction {
                sx.section_id = chapter.chapter_id.clone();
                // Phase 1b coverage check — an audit pass that asks
                // the model "what did you miss?" against its own
                // extraction. Disabled by default because the SEP
                // 2026-05-07 ablation found it a net-negative for
                // bench: it recovers ~55% additional Entity atoms
                // that never seed into the retriever's cosine top-12,
                // and slightly suppresses ArgumentReconstruction
                // richness (because each Phase 1 call has less budget
                // when shadowed by a downstream coverage pass). Costs
                // ~7-8 min per atlas of LLM time for ≤+1 essay point.
                //
                // Re-enable with `SOVEREIGN_RUN_PHASE1B=1` for
                // domains where coverage recall matters more than
                // arg richness (e.g., entity-dense reference works
                // where the bench credits broad-name attribution).
                // Skipped on retry runs because the original failure
                // is the signal we care about.
                let run_1b = std::env::var("SOVEREIGN_RUN_PHASE1B")
                    .ok()
                    .filter(|v| !v.trim().is_empty() && v != "0")
                    .is_some();
                if retry_mode.is_none() && run_1b {
                    run_phase1b_coverage(
                        self.pipeline.as_ref(),
                        chapter,
                        sx,
                        &self.chat,
                    )
                    .await;
                }
            }
            let entry = ExtractedQuestion {
                chapter_id: chapter.chapter_id.clone(),
                questions: parsed.questions,
                reveals: parsed.reveals,
                thematic_carriers: parsed.thematic_carriers,
                setting: parsed.setting,
                plot: parsed.plot,
                section_extraction,
            };
            self.persist_success_checkpoint(&entry);
            extracted.push(entry);
        }

        // Assemble the Phase1Output. Failures land in the output so
        // the run file is self-contained for post-mortem debugging.
        let output = Phase1Output {
            schema_version: Phase1Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            questions_by_chapter: extracted.clone(),
            failures: failures.clone(),
            written_at: now_rfc3339(),
        };

        // Write the run file + apply cache semantics. Mode label
        // reflects the retry mode when one is active so run files
        // are distinguishable at a glance
        // (`questions-terse-retry-NNN.json`,
        // `questions-retry-NNN.json`). Cache semantics split four
        // ways:
        //
        //   - Full run, no retry mode → overwrite the cache in one
        //     shot. Existing behaviour.
        //   - Terse retry → merge successes into the existing
        //     cache (replace matching chapter entries; drop those
        //     chapters from the cached failures list). No existing
        //     cache means nothing to merge into — the flag is a
        //     no-op rather than an error, which lets an operator
        //     re-run without worrying about order.
        //   - RetryFailed selection (any retry mode) → same
        //     merge-into-cache behaviour as terse retry. The point
        //     of `--retry-failed` is that a successful recovery
        //     promotes those chapters into the cache without
        //     requiring a hand-merge.
        //   - Subset run, no retry mode → diagnostic; cache
        //     untouched.
        let mode_label: &'static str = match retry_mode {
            Some(RetryMode::Terse { .. }) => "terse-retry",
            None => selection.mode_label(),
        };
        let run_path = self.runs.write(
            PipelinePhase::Questions,
            mode_label,
            &output,
        )?;
        let should_merge =
            matches!(retry_mode, Some(RetryMode::Terse { .. })) || selection.should_merge_into_cache();
        let cache_updated = if should_merge {
            merge_phase1_into_cache(&self.cache, &extracted)?
        } else if retry_mode.is_none() && selection.should_update_cache() {
            self.cache.write(PipelinePhase::Questions, &output)?;
            true
        } else {
            false
        };

        progress(Phase1Progress::Done {
            produced: extracted.len(),
            failed: failures.len(),
            run_path: &run_path,
        });

        Ok(Phase1RunResult {
            output,
            run_path,
            cache_updated,
            failures,
        })
    }
}

// ── Phases 2-7 result types ───────────────────────────────

#[derive(Debug, Clone)]
pub struct PhaseRunResult<T> {
    pub output: T,
    pub run_path: std::path::PathBuf,
    pub cache_updated: bool,
    pub failures: Vec<PhaseFailure>,
}

// ── PhaseFailure (unified) ──────────────────────────────────
//
// The canonical `PhaseFailure` type lives in
// `pipeline::types::PhaseFailure` so every phase's output shares one
// shape and the `sovereign enrich errors` aggregator groups across
// phases without adapters. The runner's older context-string-only
// shape was replaced in Landing 3.A; all construction sites now
// populate `phase`, `subject`, and `kind` explicitly.
pub use super::types::PhaseFailure;

pub type Phase2RunResult = PhaseRunResult<Phase2Output>;
pub type Phase3RunResult = PhaseRunResult<Phase3Output>;
pub type Phase4RunResult = PhaseRunResult<Phase4Output>;
pub type Phase5RunResult = PhaseRunResult<Phase5Output>;
pub type Phase6RunResult = PhaseRunResult<Phase6Output>;
pub type Phase7RunResult = PhaseRunResult<Phase7Output>;

/// Result of an atlas-pipeline Phase 2 run. Separate from
/// `Phase2RunResult` because the atlas output shape differs — one
/// list of facet-tagged clusters rather than the v1 question-only
/// clusters. Phase 2 atlas has no per-item failure concept (HDBSCAN
/// can't fail per-point), so there's no `failures` field.
#[derive(Debug, Clone)]
pub struct Phase2AtlasRunResult {
    pub output: Phase2AtlasOutput,
    pub run_path: std::path::PathBuf,
    pub cache_updated: bool,
}

/// A single cascade step's outcome, one variant per phase that can run.
#[derive(Debug, Clone)]
pub enum CascadeStep {
    Phase1(Phase1RunResult),
    Phase2(Phase2RunResult),
    Phase3(Phase3RunResult),
    Phase4(Phase4RunResult),
    Phase5(Phase5RunResult),
    Phase6(Phase6RunResult),
    Phase7(Phase7RunResult),
}

#[derive(Debug, Clone)]
pub struct CascadeResult {
    pub steps: Vec<CascadeStep>,
}

// ── PhaseRunner phase 2-7 + cascade ───────────────────────────

impl PhaseRunner {
    /// Phase 2 — cluster every question from phase 1 by embedding
    /// similarity. Reads `Questions` cache, embeds each question,
    /// runs HDBSCAN, writes `QuestionClusters` cache.
    pub async fn phase_2_cluster_questions(&self) -> Result<Phase2RunResult> {
        let phase1: Phase1Output = self
            .cache
            .read(PipelinePhase::Questions)?
            .ok_or_else(|| missing_upstream(PipelinePhase::Questions))?;

        // Flatten into (ref, text) pairs.
        let mut refs: Vec<(QuestionRef, String)> = Vec::new();
        for entry in &phase1.questions_by_chapter {
            for (idx, q) in entry.questions.iter().enumerate() {
                refs.push((
                    QuestionRef {
                        chapter_id: entry.chapter_id.clone(),
                        question_index: idx,
                    },
                    q.clone(),
                ));
            }
        }
        if refs.is_empty() {
            return Err(Error::InvalidInput(
                "phase 1 cache has no questions to cluster".into(),
            ));
        }

        // Embed.
        let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(refs.len());
        for (_, text) in &refs {
            embeddings.push((self.embed)(text).await?);
        }

        // Cluster.
        let config = self.pipeline.question_clustering_config();
        let result = cluster_vectors(&embeddings, &config)?;

        // Group.
        let mut clusters: std::collections::HashMap<i32, Vec<QuestionRef>> =
            std::collections::HashMap::new();
        let mut unclustered: Vec<QuestionRef> = Vec::new();
        for (i, label) in result.labels.iter().enumerate() {
            let r = refs[i].0.clone();
            if *label < 0 {
                unclustered.push(r);
            } else {
                clusters.entry(*label).or_default().push(r);
            }
        }
        let mut cluster_vec: Vec<QuestionCluster> = clusters
            .into_iter()
            .map(|(id, members)| QuestionCluster {
                id: format!("qc_{:04}", id + 1),
                question_refs: members,
            })
            .collect();
        cluster_vec.sort_by(|a, b| a.id.cmp(&b.id));

        let output = Phase2Output {
            schema_version: Phase2Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            clusters: cluster_vec,
            unclustered,
            failures: Vec::new(),
            written_at: now_rfc3339(),
        };
        let run_path = self.runs.write(
            PipelinePhase::QuestionClusters,
            "full",
            &output,
        )?;
        self.cache.write(PipelinePhase::QuestionClusters, &output)?;
        Ok(PhaseRunResult {
            output,
            run_path,
            cache_updated: true,
            failures: Vec::new(),
        })
    }

    /// Phase 2 (atlas) — cluster every facet of Phase 1's atlas
    /// sketches. Reads `Questions` cache, pulls `section_extraction`
    /// payloads from each chapter, runs HDBSCAN per facet with the
    /// facet-specific secondary-signal post-pass, and writes
    /// `AtlasClusters` cache.
    ///
    /// Returns a descriptive error when the cache is empty or when
    /// no chapter carries a `section_extraction` — both mean the
    /// current pipeline isn't producing atlas output and the
    /// operator should re-init with an atlas-shaped pipeline.
    pub async fn phase_2_cluster_atlas(&self) -> Result<Phase2AtlasRunResult> {
        use crate::enrichment::pipeline::atlas_clustering::cluster_all_facets;

        let phase1: Phase1Output = self
            .cache
            .read(PipelinePhase::Questions)?
            .ok_or_else(|| missing_upstream(PipelinePhase::Questions))?;

        let sections: Vec<SectionExtraction> = phase1
            .questions_by_chapter
            .iter()
            .filter_map(|c| c.section_extraction.clone())
            .collect();
        if sections.is_empty() {
            return Err(Error::InvalidInput(
                "phase 1 cache has no section_extraction payloads — re-init with an \
                 atlas pipeline (e.g. literary_atlas) and re-run extract before \
                 clustering"
                    .into(),
            ));
        }

        let config = self.pipeline.question_clustering_config();
        let facet_results = cluster_all_facets(&sections, &self.embed, &config).await?;

        // Flatten per-facet results into the shared output shape.
        // Every cluster keeps its facet tag; the `clusters_by_facet`
        // accessor on `Phase2AtlasOutput` gives O(N) per-facet views
        // without sacrificing the single source of truth.
        let mut clusters: Vec<AtlasCluster> = Vec::new();
        let mut unclustered: Vec<SketchRef> = Vec::new();
        for r in facet_results {
            clusters.extend(r.clusters);
            unclustered.extend(r.unclustered);
        }

        let output = Phase2AtlasOutput {
            schema_version: Phase2AtlasOutput::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            clusters,
            unclustered,
            failures: Vec::new(),
            written_at: now_rfc3339(),
        };
        let run_path = self.runs.write(
            PipelinePhase::AtlasClusters,
            "full",
            &output,
        )?;
        self.cache.write(PipelinePhase::AtlasClusters, &output)?;

        Ok(Phase2AtlasRunResult {
            output,
            run_path,
            cache_updated: true,
        })
    }

    /// Phase 3 — name the canonical concern for each question cluster.
    pub async fn phase_3_name_concerns(
        &self,
        ctx: &CorpusContext,
    ) -> Result<Phase3RunResult> {
        let phase1: Phase1Output = self
            .cache
            .read(PipelinePhase::Questions)?
            .ok_or_else(|| missing_upstream(PipelinePhase::Questions))?;
        let phase2: Phase2Output = self
            .cache
            .read(PipelinePhase::QuestionClusters)?
            .ok_or_else(|| missing_upstream(PipelinePhase::QuestionClusters))?;

        let bank = ExemplarBank::load_embedded(
            &self.exemplar_path(PipelinePhase::Concerns),
            PipelinePhase::Concerns,
            &self.embed,
        )
        .await?;
        let k = self.pipeline.top_k_exemplars(PipelinePhase::Concerns);

        let mut concerns: Vec<CanonicalConcern> = Vec::with_capacity(phase2.clusters.len());
        let mut failures: Vec<PhaseFailure> = Vec::new();

        for (ci, cluster) in phase2.clusters.iter().enumerate() {
            // Pull chapter excerpts for the first few refs.
            let excerpts: Vec<&ChapterInput> = cluster
                .question_refs
                .iter()
                .take(3)
                .filter_map(|r| ctx.chapters.iter().find(|c| c.chapter_id == r.chapter_id))
                .collect();

            // Query text for exemplar selection = the first question's text.
            let query_text = first_question_text(&phase1, &cluster.question_refs)
                .unwrap_or_else(|| "canonical concern".to_string());
            let picked: Vec<&Exemplar> = if bank.is_empty() {
                Vec::new()
            } else {
                let query_emb = (self.embed)(&query_text).await?;
                bank.select_top_k(&query_emb, k)
            };

            let prompt = self.pipeline.compose_phase3(cluster, &excerpts, &picked);
            let response = match (self.chat)(&prompt).await {
                Ok(r) => r,
                Err(e) => {
                    failures.push(PhaseFailure {
                        phase: PipelinePhase::Concerns,
                        subject: format!("cluster:{}", cluster.id),
                        kind: PhaseFailureKind::ChatError,
                        reason: format!("chat: {e}"),
                        raw_response_head: None,
                    });
                    continue;
                }
            };
            let parsed = match self.pipeline.parse_phase3(&response) {
                Ok(p) => p,
                Err(e) => {
                    let head = truncate_response_head(&response);
                    let excerpt = head
                        .as_deref()
                        .map(one_line_excerpt)
                        .unwrap_or_else(|| "<empty response>".into());
                    let kind = classify_phase1_parse_failure(&response, &e);
                    failures.push(PhaseFailure {
                        phase: PipelinePhase::Concerns,
                        subject: format!("cluster:{}", cluster.id),
                        kind,
                        reason: format!("parse: {e} | response[head]: {excerpt}"),
                        raw_response_head: head,
                    });
                    continue;
                }
            };
            concerns.push(CanonicalConcern {
                id: format!("cc_{:04}", ci + 1),
                cluster_id: cluster.id.clone(),
                concern_text: parsed.concern_text,
                scope: parsed.scope,
                primary_arcs: parsed.primary_arcs,
            });
        }

        let output = Phase3Output {
            schema_version: Phase3Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            concerns,
            failures: failures.clone(),
            written_at: now_rfc3339(),
        };
        let run_path = self.runs.write(PipelinePhase::Concerns, "full", &output)?;
        self.cache.write(PipelinePhase::Concerns, &output)?;
        Ok(PhaseRunResult {
            output,
            run_path,
            cache_updated: true,
            failures,
        })
    }

    /// Phase 4 — cluster paragraph-level chunk embeddings. Embeds every
    /// chunk on-the-fly (can be slow; admin corpora are in the low
    /// thousands of chunks).
    pub async fn phase_4_cluster_chunks(
        &self,
        ctx: &CorpusContext,
    ) -> Result<Phase4RunResult> {
        if ctx.chunks.is_empty() {
            return Err(Error::InvalidInput(
                "phase 4 requires paragraph chunks in the corpus context".into(),
            ));
        }

        let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(ctx.chunks.len());
        for chunk in &ctx.chunks {
            embeddings.push((self.embed)(&chunk.text).await?);
        }

        let config = self.pipeline.chunk_clustering_config();
        let result = cluster_vectors(&embeddings, &config)?;

        // Group into ChunkClusters with centroids.
        let members = result.members_by_cluster();
        let mut clusters: Vec<ChunkCluster> = Vec::with_capacity(members.len());
        for (label, indices) in &members {
            let chunk_ids: Vec<u64> = indices.iter().map(|&i| ctx.chunks[i].id).collect();
            let centroid = mean_vector(
                &indices.iter().map(|&i| embeddings[i].clone()).collect::<Vec<_>>(),
            );
            clusters.push(ChunkCluster {
                id: format!("kc_{:04}", label + 1),
                chunk_ids,
                noise: false,
                centroid,
            });
        }
        clusters.sort_by(|a, b| a.id.cmp(&b.id));

        // Collect noise as a synthetic "kc_noise" cluster (optional, for audit).
        let noise_ids: Vec<u64> = result
            .labels
            .iter()
            .enumerate()
            .filter_map(|(i, l)| if *l < 0 { Some(ctx.chunks[i].id) } else { None })
            .collect();
        if !noise_ids.is_empty() {
            clusters.push(ChunkCluster {
                id: "kc_noise".into(),
                chunk_ids: noise_ids,
                noise: true,
                centroid: Vec::new(),
            });
        }

        let output = Phase4Output {
            schema_version: Phase4Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            clusters,
            failures: Vec::new(),
            written_at: now_rfc3339(),
        };
        let run_path = self.runs.write(PipelinePhase::ChunkClusters, "full", &output)?;
        self.cache.write(PipelinePhase::ChunkClusters, &output)?;
        Ok(PhaseRunResult {
            output,
            run_path,
            cache_updated: true,
            failures: Vec::new(),
        })
    }

    /// Phase 5 — extract grounded positions. For each canonical concern,
    /// align to the top-K chunk clusters by centroid cosine similarity
    /// (embedding of the concern text vs cluster centroid), then for
    /// each aligned cluster compose+call+parse.
    pub async fn phase_5_extract_positions(
        &self,
        ctx: &CorpusContext,
    ) -> Result<Phase5RunResult> {
        let concerns_out: Phase3Output = self
            .cache
            .read(PipelinePhase::Concerns)?
            .ok_or_else(|| missing_upstream(PipelinePhase::Concerns))?;
        let chunks_out: Phase4Output = self
            .cache
            .read(PipelinePhase::ChunkClusters)?
            .ok_or_else(|| missing_upstream(PipelinePhase::ChunkClusters))?;

        if concerns_out.concerns.is_empty() {
            return Err(Error::InvalidInput(
                "phase 3 cache has no canonical concerns — re-run `sovereign enrich name-concerns`"
                    .into(),
            ));
        }
        let usable_clusters: Vec<&ChunkCluster> =
            chunks_out.clusters.iter().filter(|c| !c.noise).collect();
        if usable_clusters.is_empty() {
            return Err(Error::InvalidInput(
                "phase 4 cache has no non-noise chunk clusters".into(),
            ));
        }

        let bank = ExemplarBank::load_embedded(
            &self.exemplar_path(PipelinePhase::Positions),
            PipelinePhase::Positions,
            &self.embed,
        )
        .await?;
        let k_exemplars = self.pipeline.top_k_exemplars(PipelinePhase::Positions);
        const ALIGN_TOP_K: usize = 3;

        // Map chunk id → text for grounding lookups.
        let chunk_lookup: std::collections::HashMap<u64, &ChunkRecord> =
            ctx.chunks.iter().map(|c| (c.id, c)).collect();

        let mut positions: Vec<Position> = Vec::new();
        let mut failures: Vec<PhaseFailure> = Vec::new();
        let mut pos_ordinal = 0usize;

        for concern in &concerns_out.concerns {
            let concern_emb = (self.embed)(&concern.concern_text).await?;

            // Score each cluster by centroid cosine; take top-K.
            let mut scored: Vec<(f32, &ChunkCluster)> = usable_clusters
                .iter()
                .map(|cl| (cosine_similarity(&concern_emb, &cl.centroid), *cl))
                .collect();
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(ALIGN_TOP_K);

            for (_, cluster) in scored {
                // Build (chunk_id, text) pairs for the cluster.
                let texts: Vec<(u64, String)> = cluster
                    .chunk_ids
                    .iter()
                    .take(8)
                    .filter_map(|id| {
                        chunk_lookup.get(id).map(|c| (*id, c.text.clone()))
                    })
                    .collect();
                if texts.is_empty() {
                    continue;
                }

                let picked: Vec<&Exemplar> = if bank.is_empty() {
                    Vec::new()
                } else {
                    bank.select_top_k(&concern_emb, k_exemplars)
                };

                let prompt =
                    self.pipeline
                        .compose_phase5(concern, cluster, &texts, &picked);
                let response = match (self.chat)(&prompt).await {
                    Ok(r) => r,
                    Err(e) => {
                        failures.push(PhaseFailure {
                            phase: PipelinePhase::Positions,
                            subject: format!("pair:{}:{}", concern.id, cluster.id),
                            kind: PhaseFailureKind::ChatError,
                            reason: format!("chat: {e}"),
                            raw_response_head: None,
                        });
                        continue;
                    }
                };
                let parsed = match self.pipeline.parse_phase5(&response) {
                    Ok(p) => p,
                    Err(e) => {
                        let head = truncate_response_head(&response);
                        let excerpt = head
                            .as_deref()
                            .map(one_line_excerpt)
                            .unwrap_or_else(|| "<empty response>".into());
                        let kind = classify_phase1_parse_failure(&response, &e);
                        failures.push(PhaseFailure {
                            phase: PipelinePhase::Positions,
                            subject: format!("pair:{}:{}", concern.id, cluster.id),
                            kind,
                            reason: format!("parse: {e} | response[head]: {excerpt}"),
                            raw_response_head: head,
                        });
                        continue;
                    }
                };
                // Backfill section_id on grounding entries when the
                // model omitted it.
                let grounding: Vec<Grounding> = parsed
                    .grounding
                    .into_iter()
                    .map(|mut g| {
                        if g.section_id.is_empty() {
                            if let Some(rec) = chunk_lookup.get(&g.chunk_id) {
                                g.section_id = rec.section_id.clone();
                            }
                        }
                        g
                    })
                    .collect();

                pos_ordinal += 1;
                positions.push(Position {
                    id: format!("pos_{:04}", pos_ordinal),
                    concern_id: concern.id.clone(),
                    chunk_cluster_id: cluster.id.clone(),
                    position_text: parsed.position_text,
                    grounding,
                    extensions: parsed.extensions,
                });
            }
        }

        let output = Phase5Output {
            schema_version: Phase5Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            positions,
            failures: failures.clone(),
            written_at: now_rfc3339(),
        };
        let run_path = self.runs.write(PipelinePhase::Positions, "full", &output)?;
        self.cache.write(PipelinePhase::Positions, &output)?;
        Ok(PhaseRunResult {
            output,
            run_path,
            cache_updated: true,
            failures,
        })
    }

    /// Phase 6 — pairwise tension detection between positions aligned
    /// to the SAME canonical concern. Positions from different concerns
    /// are not paired (no structural signal there).
    pub async fn phase_6_detect_tensions(&self) -> Result<Phase6RunResult> {
        let pos_out: Phase5Output = self
            .cache
            .read(PipelinePhase::Positions)?
            .ok_or_else(|| missing_upstream(PipelinePhase::Positions))?;

        let bank = ExemplarBank::load_embedded(
            &self.exemplar_path(PipelinePhase::Tensions),
            PipelinePhase::Tensions,
            &self.embed,
        )
        .await?;
        let k = self.pipeline.top_k_exemplars(PipelinePhase::Tensions);

        // Group positions by concern_id.
        let mut by_concern: std::collections::BTreeMap<String, Vec<&Position>> =
            std::collections::BTreeMap::new();
        for p in &pos_out.positions {
            by_concern
                .entry(p.concern_id.clone())
                .or_default()
                .push(p);
        }

        let mut tensions: Vec<Tension> = Vec::new();
        let mut failures: Vec<PhaseFailure> = Vec::new();
        let mut t_ordinal = 0usize;

        for (_concern_id, positions) in &by_concern {
            if positions.len() < 2 {
                continue;
            }
            for i in 0..positions.len() {
                for j in (i + 1)..positions.len() {
                    let a = positions[i];
                    let b = positions[j];

                    let picked: Vec<&Exemplar> = if bank.is_empty() {
                        Vec::new()
                    } else {
                        // Query = concatenation of the two position texts.
                        let query = format!("{}\n\n{}", a.position_text, b.position_text);
                        let q_emb = (self.embed)(&query).await?;
                        bank.select_top_k(&q_emb, k)
                    };

                    let prompt = self.pipeline.compose_phase6(a, b, &picked);
                    let response = match (self.chat)(&prompt).await {
                        Ok(r) => r,
                        Err(e) => {
                            failures.push(PhaseFailure {
                                phase: PipelinePhase::Tensions,
                                subject: format!("pair:{}:{}", a.id, b.id),
                                kind: PhaseFailureKind::ChatError,
                                reason: format!("chat: {e}"),
                                raw_response_head: None,
                            });
                            continue;
                        }
                    };
                    let parsed = match self.pipeline.parse_phase6(&response) {
                        Ok(p) => p,
                        Err(e) => {
                            let head = truncate_response_head(&response);
                            let excerpt = head
                                .as_deref()
                                .map(one_line_excerpt)
                                .unwrap_or_else(|| "<empty response>".into());
                            let kind = classify_phase1_parse_failure(&response, &e);
                            failures.push(PhaseFailure {
                                phase: PipelinePhase::Tensions,
                                subject: format!("pair:{}:{}", a.id, b.id),
                                kind,
                                reason: format!("parse: {e} | response[head]: {excerpt}"),
                                raw_response_head: head,
                            });
                            continue;
                        }
                    };
                    if let Some(t) = parsed {
                        t_ordinal += 1;
                        tensions.push(Tension {
                            id: format!("t_{:04}", t_ordinal),
                            position_a_id: a.id.clone(),
                            position_b_id: b.id.clone(),
                            description: t.description,
                            specific_disagreement: t.specific_disagreement,
                            structural_type: t.structural_type,
                        });
                    }
                }
            }
        }

        let output = Phase6Output {
            schema_version: Phase6Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            tensions,
            failures: failures.clone(),
            written_at: now_rfc3339(),
        };
        let run_path = self.runs.write(PipelinePhase::Tensions, "full", &output)?;
        self.cache.write(PipelinePhase::Tensions, &output)?;
        Ok(PhaseRunResult {
            output,
            run_path,
            cache_updated: true,
            failures,
        })
    }

    /// Phase 7 — gap detection. Single call; model sees concerns,
    /// positions, and chapter titles.
    pub async fn phase_7_detect_gaps(
        &self,
        ctx: &CorpusContext,
    ) -> Result<Phase7RunResult> {
        let concerns_out: Phase3Output = self
            .cache
            .read(PipelinePhase::Concerns)?
            .ok_or_else(|| missing_upstream(PipelinePhase::Concerns))?;
        let pos_out: Phase5Output = self
            .cache
            .read(PipelinePhase::Positions)?
            .ok_or_else(|| missing_upstream(PipelinePhase::Positions))?;
        // Tensions are part of the atlas too but the prompt doesn't strictly
        // require them; check cache exists for staleness but don't block on
        // empty tensions.
        let _tensions_exists = self.cache.read::<Phase6Output>(PipelinePhase::Tensions)?;

        let bank = ExemplarBank::load_embedded(
            &self.exemplar_path(PipelinePhase::Gaps),
            PipelinePhase::Gaps,
            &self.embed,
        )
        .await?;
        let k = self.pipeline.top_k_exemplars(PipelinePhase::Gaps);

        let picked: Vec<&Exemplar> = if bank.is_empty() {
            Vec::new()
        } else {
            // Query: "gap detection" summary — cheap & stable.
            let q = "gap detection across canonical concerns".to_string();
            let q_emb = (self.embed)(&q).await?;
            bank.select_top_k(&q_emb, k)
        };

        let prompt = self.pipeline.compose_phase7(
            &concerns_out.concerns,
            &pos_out.positions,
            &ctx.chapter_titles,
            &picked,
        );
        let response = (self.chat)(&prompt).await?;
        let parsed = self.pipeline.parse_phase7(&response)?;

        let gaps: Vec<Gap> = parsed
            .into_iter()
            .enumerate()
            .map(|(i, p)| Gap {
                id: format!("gap_{:04}", i + 1),
                gap_text: p.gap_text,
                evidence: p.evidence,
                significance: p.significance,
            })
            .collect();

        let output = Phase7Output {
            schema_version: Phase7Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            gaps,
            failures: Vec::new(),
            written_at: now_rfc3339(),
        };
        let run_path = self.runs.write(PipelinePhase::Gaps, "full", &output)?;
        self.cache.write(PipelinePhase::Gaps, &output)?;
        Ok(PhaseRunResult {
            output,
            run_path,
            cache_updated: true,
            failures: Vec::new(),
        })
    }

    /// Run every phase downstream of (and including) `from` in
    /// ordinal order. Phase 1 needs a chapter selection — when
    /// `from == Questions`, the caller must pass a non-empty
    /// `selection`. Other phases derive their inputs from `ctx` +
    /// upstream caches.
    pub async fn cascade(
        &self,
        from: PipelinePhase,
        ctx: &CorpusContext,
        phase1_selection: Option<ChapterSelection>,
    ) -> Result<CascadeResult> {
        let mut steps = Vec::new();

        for phase in PipelinePhase::ALL {
            if phase.ordinal() < from.ordinal() {
                continue;
            }
            match phase {
                PipelinePhase::Ingest => {
                    // Ingest is not an LLM phase; skip silently.
                }
                PipelinePhase::Questions => {
                    let sel = phase1_selection.clone().unwrap_or(ChapterSelection::Full);
                    let r = self
                        .phase_1_extract_questions(&ctx.chapters, &sel, |_| {})
                        .await?;
                    steps.push(CascadeStep::Phase1(r));
                }
                PipelinePhase::QuestionClusters => {
                    let r = self.phase_2_cluster_questions().await?;
                    steps.push(CascadeStep::Phase2(r));
                }
                PipelinePhase::Concerns => {
                    let r = self.phase_3_name_concerns(ctx).await?;
                    steps.push(CascadeStep::Phase3(r));
                }
                PipelinePhase::ChunkClusters => {
                    let r = self.phase_4_cluster_chunks(ctx).await?;
                    steps.push(CascadeStep::Phase4(r));
                }
                PipelinePhase::Positions => {
                    let r = self.phase_5_extract_positions(ctx).await?;
                    steps.push(CascadeStep::Phase5(r));
                }
                PipelinePhase::Tensions => {
                    let r = self.phase_6_detect_tensions().await?;
                    steps.push(CascadeStep::Phase6(r));
                }
                PipelinePhase::Gaps => {
                    let r = self.phase_7_detect_gaps(ctx).await?;
                    steps.push(CascadeStep::Phase7(r));
                }
                PipelinePhase::AtlasClusters | PipelinePhase::AtlasNamedClusters => {
                    // Atlas phases are driven by their dedicated
                    // subcommands (`atlas-cluster`, `name-atlas-clusters`)
                    // rather than the v1 cascade. Skipping here keeps
                    // `cascade` focused on the original
                    // questions→positions→tensions flow and avoids
                    // implicitly re-running atlas work that was
                    // intentionally diagnostic.
                }
                PipelinePhase::SeedExtraction => {
                    // Seed extraction is a Stage 1a pre-map step.
                    // Cascade is a v1 flow; seed is the atlas pipeline's
                    // business and gets run by the CLI's extract path
                    // before calling phase_1_extract_questions.
                    // Skipping here keeps v1 cascade behaviour stable.
                }
            }
        }

        Ok(CascadeResult { steps })
    }
}

// ── Helpers ───────────────────────────────────────────────────

fn missing_upstream(phase: PipelinePhase) -> Error {
    Error::InvalidInput(format!(
        "phase '{}' cache is missing — run its upstream command first \
         (see `sovereign enrich status <corpus>` for phase states)",
        phase.id()
    ))
}

fn first_question_text(phase1: &Phase1Output, refs: &[QuestionRef]) -> Option<String> {
    let first = refs.first()?;
    let entry = phase1
        .questions_by_chapter
        .iter()
        .find(|e| e.chapter_id == first.chapter_id)?;
    entry.questions.get(first.question_index).cloned()
}

fn mean_vector(vecs: &[Vec<f32>]) -> Vec<f32> {
    if vecs.is_empty() {
        return Vec::new();
    }
    let dims = vecs[0].len();
    let mut sum = vec![0.0_f64; dims];
    for v in vecs {
        for (i, &x) in v.iter().enumerate() {
            sum[i] += x as f64;
        }
    }
    let n = vecs.len() as f64;
    sum.into_iter().map(|s| (s / n) as f32).collect()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Build the query-side text a chapter is scored against when picking
/// exemplars. We want a short, shape-agnostic handle — the chapter
/// title plus its opening prose. Longer bodies don't improve
/// selection and cost extra embed time.
fn phase1_query_text(chapter: &ChapterInput) -> String {
    let mut out = String::new();
    out.push_str(&chapter.title);
    out.push_str("\n\n");
    let mut budget = 800usize;
    for ch in chapter.text.chars() {
        if budget == 0 {
            break;
        }
        out.push(ch);
        budget -= 1;
    }
    out
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Merge a batch of Phase-1 successes into the existing
/// `cache/questions.json`.
///
/// Each extracted chapter replaces any cached entry with the same id
/// (or is appended if new), and the corresponding entry is dropped
/// from the cached failures list — a retry that succeeds resolves
/// that failure by construction.
///
/// Returns `true` when at least one chapter was merged and the cache
/// was rewritten; `false` when no cache exists yet (nothing to merge
/// into) or `extracted` is empty. The empty-cache branch is
/// deliberately a no-op rather than an error: an operator running
/// `--retry-failed` before a first full run gets a clean skip
/// instead of a cryptic failure.
fn merge_phase1_into_cache(
    cache: &PhaseCache,
    extracted: &[ExtractedQuestion],
) -> Result<bool> {
    if extracted.is_empty() {
        return Ok(false);
    }
    let Some(mut existing) = cache.read::<Phase1Output>(PipelinePhase::Questions)? else {
        return Ok(false);
    };
    let mut merged = 0usize;
    for q in extracted {
        if let Some(slot) = existing
            .questions_by_chapter
            .iter_mut()
            .find(|e| e.chapter_id == q.chapter_id)
        {
            *slot = q.clone();
        } else {
            existing.questions_by_chapter.push(q.clone());
        }
        existing.failures.retain(|f| f.chapter_id != q.chapter_id);
        merged += 1;
    }
    existing.written_at = now_rfc3339();
    cache.write(PipelinePhase::Questions, &existing)?;
    Ok(merged > 0)
}

/// Run the optional Phase 1b coverage check for one chapter and
/// merge any newly-surfaced entities into `sx.entities_introduced`.
///
/// Best-effort: if the pipeline doesn't opt in (returns `None` from
/// either compose method), or if any chat / parse step fails, we
/// log and proceed — the chapter keeps its original Phase 1 atoms.
/// Dedup is a case-insensitive canonical-name match against the
/// existing list, so a coverage-pass repeat of an atom the first
/// pass already lifted is silently dropped.
async fn run_phase1b_coverage(
    pipeline: &dyn Pipeline,
    chapter: &ChapterInput,
    sx: &mut SectionExtraction,
    chat: &ChatCompletionFn,
) {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = sx
        .entities_introduced
        .iter()
        .map(|e| e.canonical_name.trim().to_lowercase())
        .collect();
    let mut new_total: usize = 0;
    let prompts = [
        ("entity", pipeline.compose_phase1b_entity_coverage(chapter, sx)),
        ("concept", pipeline.compose_phase1b_concept_coverage(chapter, sx)),
    ];
    for (label, maybe_prompt) in prompts {
        let Some(prompt) = maybe_prompt else { continue };
        let response = match (chat)(&prompt).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    chapter = %chapter.chapter_id,
                    pass = %label,
                    "phase 1b coverage chat failed: {e}"
                );
                continue;
            }
        };
        let atoms = match pipeline.parse_phase1b_coverage(&response) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(
                    chapter = %chapter.chapter_id,
                    pass = %label,
                    "phase 1b coverage parse failed: {e}"
                );
                continue;
            }
        };
        for atom in atoms {
            let key = atom.canonical_name.trim().to_lowercase();
            if key.is_empty() || seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            sx.entities_introduced.push(atom);
            new_total += 1;
        }
    }
    if new_total > 0 {
        tracing::debug!(
            chapter = %chapter.chapter_id,
            "phase 1b coverage added {new_total} entity sketch(es)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::pipeline::pipelines::literary::LiteraryPipeline;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    #[test]
    fn classify_phase1_parse_failure_detects_think_truncation() {
        // Unclosed <think> — no answer past the reasoning trace.
        // Even if the serde error looks like generic parse drift,
        // the structured classifier calls this ThinkTruncated.
        let response = "<think>the model ran out of budget";
        let err = Error::Serialization("no JSON".into());
        assert_eq!(
            classify_phase1_parse_failure(response, &err),
            PhaseFailureKind::ThinkTruncated
        );
    }

    #[test]
    fn classify_phase1_parse_failure_detects_empty_extraction() {
        // Well-formed envelope but no atoms — the pipeline parser
        // signals this with a distinctive "did not extract" error
        // string. The classifier maps it to EmptyExtraction so the
        // CLI can route to prompt-fix work, not a plain retry.
        let response = "{\"section_id\":\"sec_0001\"}";
        let err = Error::Serialization(
            "phase 1 (atlas) did not extract anything usable".into(),
        );
        assert_eq!(
            classify_phase1_parse_failure(response, &err),
            PhaseFailureKind::EmptyExtraction
        );
    }

    #[test]
    fn classify_phase1_parse_failure_defaults_to_parse_drift() {
        // Anything else — malformed JSON, schema validation fail —
        // is generic parse drift. A plain retry is the right first
        // move for these.
        let response = "<think>done</think>\n{malformed json";
        let err = Error::Serialization("missing field `foo` at line 3".into());
        assert_eq!(
            classify_phase1_parse_failure(response, &err),
            PhaseFailureKind::ParseDrift
        );
    }

    #[test]
    fn truncate_response_head_returns_none_on_whitespace() {
        assert_eq!(truncate_response_head("   "), None);
        assert_eq!(truncate_response_head(""), None);
    }

    #[test]
    fn truncate_response_head_prefers_post_think_content() {
        // Simulates the common failure: thinking preamble + malformed
        // JSON. We want to see the JSON, not the reasoning.
        let raw = format!(
            "<think>{}</think>\n{{\"questions\": [\"?\"], \"oops: missing_brace\"",
            "reasoning text ".repeat(500) // ~7.5 KB of think
        );
        let head = truncate_response_head(&raw).expect("has content");
        assert!(
            head.contains("oops: missing_brace"),
            "expected post-think JSON candidate, got: {head}"
        );
        assert!(
            !head.contains("reasoning text"),
            "reasoning preamble leaked into head: {head}"
        );
    }

    #[test]
    fn truncate_response_head_flags_truncated_thinking() {
        // <think> opened but never closed — no answer was emitted.
        let raw = format!("<think>{}", "half a thought ".repeat(200));
        let head = truncate_response_head(&raw).expect("has content");
        assert!(
            head.starts_with("<think block truncated"),
            "expected truncation marker, got: {head}"
        );
    }

    #[test]
    fn truncate_response_head_caps_post_think_content() {
        let raw = format!("<think>short</think>{}", "J".repeat(FAILURE_HEAD_CHAR_CAP * 2));
        let head = truncate_response_head(&raw).expect("has content");
        // Head is capped and carries the "+N chars" marker.
        assert!(head.contains("+"), "expected drop marker, got: {head}");
        assert!(head.chars().count() <= FAILURE_HEAD_CHAR_CAP + 64);
    }

    #[test]
    fn truncate_response_head_passes_short_response_through_unchanged() {
        let raw = "not valid json";
        assert_eq!(truncate_response_head(raw).unwrap(), "not valid json");
    }

    fn chapter(id: &str, title: &str, body: &str) -> ChapterInput {
        // Pad every test body past MIN_PHASE1_CHAPTER_WORDS so the
        // short-chapter skip doesn't fire on fixtures that are meant
        // to exercise the chat-and-parse path. The original `body`
        // stays at the start so `canned_chat`'s substring matches
        // ("FAIL", "Two", etc.) still dispatch correctly.
        let padding_word = " filler";
        // 60 copies of "filler" = 60 words, comfortably above the
        // 40-word MIN_PHASE1_CHAPTER_WORDS threshold regardless of the
        // caller's seed body length.
        let text = format!("{body}{}", padding_word.repeat(60));
        let approx_tokens = text.len() / 4;
        ChapterInput {
            chapter_id: id.into(),
            title: title.into(),
            text,
            metadata: HashMap::new(),
            approx_tokens,
        }
    }

    /// Deterministic embed: returns a 3-dim vector keyed by the first
    /// ASCII letter. Lets tests verify top-K selection without real
    /// embeddings.
    fn alphabet_embed() -> EmbedFn {
        Arc::new(move |s: &str| {
            let c = s.chars().next().unwrap_or('z');
            let v = match c {
                'a'..='i' => vec![1.0_f32, 0.0, 0.0],
                'j'..='r' => vec![0.0, 1.0, 0.0],
                _ => vec![0.0, 0.0, 1.0],
            };
            Box::pin(async move { Ok(v) })
        })
    }

    /// Deterministic chat: returns a fixed Phase1-shaped JSON keyed
    /// by the chapter title embedded in the user prompt. Fails for
    /// a chapter whose title includes "FAIL".
    fn canned_chat() -> ChatCompletionFn {
        Arc::new(move |prompt: &ChatPrompt| {
            let user = prompt.user.clone();
            let body: String = if user.contains("FAIL") {
                // Respond with something that doesn't parse.
                "not-json at all".into()
            } else if user.contains("NOJSON") {
                "```\ngarbage\n```".into()
            } else {
                let q = if user.contains("Two") {
                    r#"{"questions":["q-a","q-b"]}"#
                } else {
                    r#"{"questions":["only-q"]}"#
                };
                q.into()
            };
            Box::pin(async move { Ok(body) })
        })
    }

    fn runner_under_test(root: &Path) -> PhaseRunner {
        let cache = PhaseCache::new(root.join("cache"));
        let runs = RunOutputWriter::new(root.join("runs"));
        PhaseRunner::new(
            Arc::new(LiteraryPipeline::new()),
            alphabet_embed(),
            canned_chat(),
            cache,
            runs,
            root.join("exemplars"),
        )
    }

    #[tokio::test]
    async fn phase_1_terse_retry_errors_when_pipeline_has_no_terse_variant() {
        // The v1 `LiteraryPipeline` does NOT override
        // `compose_phase1_terse` — the trait default returns None.
        // A --terse retry against that pipeline must fail fast with
        // a clear error rather than silently reusing the default
        // prompt.
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let chapters = vec![chapter("ch_01", "Chapter 1", "A body")];
        let err = runner
            .phase_1_extract_questions_with_retry(
                &chapters,
                &ChapterSelection::Subset(vec!["ch_01".into()]),
                Some(RetryMode::Terse { max_output_tokens: 16384 }),
                |_| {},
            )
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("does not implement `compose_phase1_terse`"),
            "expected terse-unsupported error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn phase_1_terse_retry_uses_token_aware_chat_when_available() {
        // When the runner has a `chat_with_tokens` closure configured
        // and the pipeline supports a terse variant, a Terse retry
        // routes through the token-aware closure with the requested
        // cap.
        use crate::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;

        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path().join("cache"));
        let runs = RunOutputWriter::new(dir.path().join("runs"));

        // The token-aware chat records the requested max_output_tokens
        // for inspection. Returns a canned atlas JSON so the chapter
        // succeeds.
        let observed = Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
        let observed_c = observed.clone();
        let chat_with_tokens: ChatCompletionWithTokensFn =
            Arc::new(move |_prompt: &ChatPrompt, tokens: u32| {
                observed_c.lock().unwrap().push(tokens);
                let body = r#"{
                  "section_id": "ch_01",
                  "entities_introduced": [{"canonical_name": "A", "entity_type": "person"}],
                  "questions_raised": [{"content": "Why?"}]
                }"#
                .to_string();
                Box::pin(async move { Ok(body) })
            });

        // The default chat is used only when retry_mode is None.
        // Our test sets retry_mode = Some(Terse), so this closure
        // should NOT be invoked — we make it panic to prove that.
        let default_chat: ChatCompletionFn = Arc::new(move |_prompt: &ChatPrompt| {
            Box::pin(async move {
                panic!("default chat should not be invoked when terse retry is active");
            })
        });

        let runner = PhaseRunner::new(
            Arc::new(LiteraryAtlasPipeline::new()),
            alphabet_embed(),
            default_chat,
            cache,
            runs,
            dir.path().join("exemplars"),
        )
        .with_chat_with_tokens(chat_with_tokens);

        let chapters = vec![chapter("ch_01", "Chapter 1", "body")];
        let res = runner
            .phase_1_extract_questions_with_retry(
                &chapters,
                &ChapterSelection::Subset(vec!["ch_01".into()]),
                Some(RetryMode::Terse { max_output_tokens: 12345 }),
                |_| {},
            )
            .await
            .expect("terse retry succeeds on literary_atlas");

        assert_eq!(res.output.questions_by_chapter.len(), 1);
        let recorded = observed.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![12345],
            "expected exactly one token-aware call at the requested cap"
        );
    }

    #[tokio::test]
    async fn phase_1_default_variant_ignores_token_aware_chat() {
        // Sanity check the other direction: when retry_mode is None,
        // the runner goes through the default `chat` closure and
        // does NOT touch `chat_with_tokens`, even if configured.
        use crate::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;

        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path().join("cache"));
        let runs = RunOutputWriter::new(dir.path().join("runs"));

        let chat_with_tokens: ChatCompletionWithTokensFn =
            Arc::new(move |_prompt: &ChatPrompt, _tokens: u32| {
                Box::pin(async move {
                    panic!(
                        "chat_with_tokens should not be invoked when retry_mode is None"
                    );
                })
            });

        let runner = PhaseRunner::new(
            Arc::new(LiteraryAtlasPipeline::new()),
            alphabet_embed(),
            // Canned atlas response so the chapter parses cleanly.
            Arc::new(move |_prompt: &ChatPrompt| {
                let body = r#"{
                  "section_id": "ch_01",
                  "entities_introduced": [{"canonical_name": "A", "entity_type": "person"}],
                  "questions_raised": [{"content": "Why?"}]
                }"#
                .to_string();
                Box::pin(async move { Ok(body) })
            }),
            cache,
            runs,
            dir.path().join("exemplars"),
        )
        .with_chat_with_tokens(chat_with_tokens);

        let chapters = vec![chapter("ch_01", "Chapter 1", "body")];
        let res = runner
            .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
            .await
            .unwrap();
        assert_eq!(res.output.questions_by_chapter.len(), 1);
    }

    #[tokio::test]
    async fn phase_1_retry_failed_merges_into_cache() {
        // `--retry-failed` → ChapterSelection::RetryFailed. A success
        // for one of the previously-failed chapter ids should (a) flip
        // `cache_updated = true`, (b) replace the cached entry for
        // that chapter, (c) drop the now-resolved failure from the
        // cached failures list. Unrelated cached chapters are
        // untouched. This is the architectural fix for the
        // hand-merge workaround we used on the Dopesick Jesus
        // recovery.
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());

        // Seed an existing cache with one success (ch_01) and one
        // failure (ch_02) — the shape left by a `--full` run that
        // stumbled on one chapter.
        let seeded = Phase1Output {
            schema_version: Phase1Output::SCHEMA_VERSION,
            pipeline_id: "literary".into(),
            questions_by_chapter: vec![ExtractedQuestion {
                chapter_id: "ch_01".into(),
                questions: vec!["pre-existing".into()],
                reveals: None,
                thematic_carriers: Vec::new(),
                setting: None,
                plot: None,
                section_extraction: None,
            }],
            failures: vec![Phase1Failure {
                chapter_id: "ch_02".into(),
                reason: "parse error".into(),
                raw_response_head: None,
                failure_kind: PhaseFailureKind::ParseDrift,
            }],
            written_at: "prior".into(),
        };
        runner
            .cache
            .write(PipelinePhase::Questions, &seeded)
            .expect("seed cache");

        let chapters = vec![
            chapter("ch_01", "One", "body"),
            chapter("ch_02", "One", "body"),
        ];
        let res = runner
            .phase_1_extract_questions_with_retry(
                &chapters,
                &ChapterSelection::RetryFailed(vec!["ch_02".into()]),
                None,
                |_| {},
            )
            .await
            .expect("retry-failed run");

        assert!(res.cache_updated, "RetryFailed with a success must merge into cache");
        assert_eq!(res.output.questions_by_chapter.len(), 1);
        assert_eq!(res.output.questions_by_chapter[0].chapter_id, "ch_02");

        // Read back the cache. Both chapters should now be present as
        // successes; the failures list is empty.
        let cached: Phase1Output = runner
            .cache
            .read(PipelinePhase::Questions)
            .expect("read cache")
            .expect("cache present");
        assert_eq!(cached.questions_by_chapter.len(), 2);
        let ch1 = cached
            .questions_by_chapter
            .iter()
            .find(|e| e.chapter_id == "ch_01")
            .expect("ch_01 still cached");
        // ch_01 was NOT targeted by the retry — its questions must
        // remain the pre-existing value.
        assert_eq!(ch1.questions, vec!["pre-existing".to_string()]);
        let ch2 = cached
            .questions_by_chapter
            .iter()
            .find(|e| e.chapter_id == "ch_02")
            .expect("ch_02 merged");
        assert!(!ch2.questions.is_empty());
        assert!(
            cached.failures.is_empty(),
            "resolved failure must drop out of cached failures list, got: {:?}",
            cached.failures
        );
    }

    #[tokio::test]
    async fn phase_1_retry_failed_with_no_prior_cache_is_noop() {
        // An operator who runs `--retry-failed` before any `--full`
        // has been successful (no cache file yet) should get a clean
        // `cache_updated = false` rather than an error. The run file
        // still captures the recovery attempt; only the promote step
        // is skipped.
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let chapters = vec![chapter("ch_01", "One", "body")];
        let res = runner
            .phase_1_extract_questions_with_retry(
                &chapters,
                &ChapterSelection::RetryFailed(vec!["ch_01".into()]),
                None,
                |_| {},
            )
            .await
            .expect("run completes even without a prior cache");
        assert!(!res.cache_updated);
        // The run file exists for debugging.
        assert!(res.run_path.exists(), "run file should be written");
        // No cache was seeded → none should have been written.
        let cached: Option<Phase1Output> = runner
            .cache
            .read(PipelinePhase::Questions)
            .expect("read cache");
        assert!(
            cached.is_none(),
            "retry with empty cache must not create a cache file"
        );
    }

    #[tokio::test]
    async fn phase_1_retry_failed_labels_run_file_with_retry_mode() {
        // Mode label "retry" distinguishes RetryFailed runs from
        // diagnostic subsets ("subset") in the runs/ directory. The
        // run filename shape is `questions-<mode>-NNN.json`.
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let chapters = vec![chapter("ch_01", "One", "body")];
        let res = runner
            .phase_1_extract_questions_with_retry(
                &chapters,
                &ChapterSelection::RetryFailed(vec!["ch_01".into()]),
                None,
                |_| {},
            )
            .await
            .expect("run completes");
        let name = res
            .run_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(
            name.starts_with("questions-retry-"),
            "expected retry-labelled run file, got: {name}"
        );
    }

    #[tokio::test]
    async fn phase_1_with_seed_cache_routes_to_chat_with_tokens_at_seed_budget() {
        // Landing 2.B invariant: when the cache has a seed file AND
        // the runner has `chat_with_tokens` configured, the default
        // Phase 1 branch routes through that closure with the
        // runner's seed-scoped output budget (`PHASE1_SEED_OUTPUT_BUDGET`).
        // Without a seed cached, the runner falls back to the
        // un-capped default chat (covered by
        // `phase_1_default_variant_ignores_token_aware_chat`).
        use crate::enrichment::pipeline::atlas::{
            EntityType as AtlasEntityType, SeedEntities, SeedEntity, SeedOrigin,
        };
        use crate::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;

        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path().join("cache"));
        let runs = RunOutputWriter::new(dir.path().join("runs"));

        // Pre-populate the seed cache so the default Phase 1 branch
        // sees `seed_opt = Some(...)` and routes to chat_with_tokens.
        let seed = SeedEntities {
            schema_version: SeedEntities::SCHEMA_VERSION,
            corpus_id: "bk".into(),
            origin: SeedOrigin::Llm,
            entries: vec![SeedEntity {
                canonical_name: "A".into(),
                aliases: Vec::new(),
                entity_type: AtlasEntityType::Person,
                description: "seed".into(),
            }],
            written_at: "2026-04-23T00:00:00Z".into(),
        };
        cache.write(PipelinePhase::SeedExtraction, &seed).unwrap();

        // Record the token cap each chat_with_tokens call is invoked
        // with. Return a minimal atlas JSON so parse succeeds.
        let observed = Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
        let observed_c = observed.clone();
        let chat_with_tokens: ChatCompletionWithTokensFn =
            Arc::new(move |_prompt: &ChatPrompt, tokens: u32| {
                observed_c.lock().unwrap().push(tokens);
                let body = r#"{
                  "section_id": "ch_01",
                  "entities_introduced": [{"canonical_name": "A", "entity_type": "person"}],
                  "questions_raised": [{"content": "Why?"}]
                }"#
                .to_string();
                Box::pin(async move { Ok(body) })
            });

        // The main Phase 1 branch must route through chat_with_tokens
        // (verified below). Phase 1B coverage refinement, which the
        // LiteraryAtlasPipeline opts into, runs after the main extraction
        // and uses the default chat (no seed-budget bump needed for the
        // small entity/concept coverage prompts). Track default_chat
        // invocations so we can assert it was used for Phase 1B only,
        // not for the main Phase 1 dispatch.
        let default_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let default_calls_c = default_calls.clone();
        let default_chat: ChatCompletionFn = Arc::new(move |_prompt: &ChatPrompt| {
            default_calls_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Empty Phase 1B coverage response — the parser accepts this
            // as "no missed entities/concepts" and the runner proceeds
            // with the original Phase 1 atoms unchanged.
            let body = r#"{"missed_entities": [], "missed_concepts": []}"#.to_string();
            Box::pin(async move { Ok(body) })
        });

        let runner = PhaseRunner::new(
            Arc::new(LiteraryAtlasPipeline::new()),
            alphabet_embed(),
            default_chat,
            cache,
            runs,
            dir.path().join("exemplars"),
        )
        .with_chat_with_tokens(chat_with_tokens);

        let chapters = vec![chapter("ch_01", "Chapter 1", "body")];
        let res = runner
            .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
            .await
            .unwrap();
        assert_eq!(res.output.questions_by_chapter.len(), 1);
        let recorded = observed.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![PHASE1_SEED_OUTPUT_BUDGET],
            "expected one token-aware call at the seed output budget"
        );
        // Phase 1B coverage runs after the main extraction (entity +
        // concept passes) using the default chat. Two calls confirm
        // both passes ran without affecting the seed-routing assertion.
        let default_n = default_calls.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            default_n, 2,
            "expected 2 default_chat calls (Phase 1B entity + concept coverage), got {default_n}"
        );
    }

    #[tokio::test]
    async fn phase_1_full_writes_run_and_cache() {
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let chapters = vec![
            chapter("ch_01", "Chapter 1", "A body with One question."),
            chapter("ch_02", "Chapter 2", "A body with Two questions."),
        ];
        let progress_count = AtomicUsize::new(0);
        let res = runner
            .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_ev| {
                progress_count.fetch_add(1, Ordering::Relaxed);
            })
            .await
            .unwrap();
        assert_eq!(res.output.questions_by_chapter.len(), 2);
        assert!(res.cache_updated);
        assert!(res.run_path.exists());
        // Cache file should exist and round-trip through PhaseCache.
        let back: Option<Phase1Output> =
            runner.cache().read(PipelinePhase::Questions).unwrap();
        assert!(back.is_some());
        assert!(progress_count.load(Ordering::Relaxed) >= 4); // Start + 2 chapters + Done at minimum
    }

    #[tokio::test]
    async fn phase_1_subset_writes_run_but_not_cache() {
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let chapters = vec![
            chapter("ch_01", "Chapter 1", "Body one"),
            chapter("ch_02", "Chapter 2", "Body two"),
            chapter("ch_03", "Chapter 3", "Body three"),
        ];
        let res = runner
            .phase_1_extract_questions(
                &chapters,
                &ChapterSelection::Subset(vec!["ch_01".into(), "ch_03".into()]),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(res.output.questions_by_chapter.len(), 2);
        assert_eq!(res.output.questions_by_chapter[0].chapter_id, "ch_01");
        assert_eq!(res.output.questions_by_chapter[1].chapter_id, "ch_03");
        assert!(!res.cache_updated);
        assert!(res.run_path.exists());
        // Cache should NOT have been written by a subset run.
        assert!(runner
            .cache()
            .read::<Phase1Output>(PipelinePhase::Questions)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn phase_1_subset_rejects_unknown_chapter_id() {
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let chapters = vec![chapter("ch_01", "Chapter 1", "body")];
        let err = runner
            .phase_1_extract_questions(
                &chapters,
                &ChapterSelection::Subset(vec!["nope".into()]),
                |_| {},
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("chapter not found"));
    }

    #[tokio::test]
    async fn phase_1_parse_failure_captured_as_failure_not_run_failure() {
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let chapters = vec![
            chapter("ch_01", "Chapter 1", "A body with one question."),
            // The chat mock replies with non-JSON when title contains FAIL.
            chapter("ch_02", "FAIL Chapter", "body"),
        ];
        let res = runner
            .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
            .await
            .unwrap();
        assert_eq!(res.output.questions_by_chapter.len(), 1);
        assert_eq!(res.failures.len(), 1);
        assert_eq!(res.failures[0].chapter_id, "ch_02");
        assert!(res.failures[0].reason.contains("parse error"));
    }

    #[tokio::test]
    async fn phase_1_skips_chapters_with_empty_bodies() {
        // A short "Part I"-style section has no substantive body.
        // The runner should register a skip without burning a chat
        // call, and the failure reason should name the root cause.
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let short = ChapterInput {
            chapter_id: "sec_0001".into(),
            title: "Part I".into(),
            text: "Book I. The History Of A Family".into(),
            metadata: std::collections::HashMap::new(),
            approx_tokens: 10,
        };
        let real = chapter("sec_0002", "Chapter 1", &"body word ".repeat(60));
        let res = runner
            .phase_1_extract_questions(&[short, real], &ChapterSelection::Full, |_| {})
            .await
            .unwrap();
        assert_eq!(res.output.questions_by_chapter.len(), 1);
        assert_eq!(res.failures.len(), 1);
        assert_eq!(res.failures[0].chapter_id, "sec_0001");
        assert!(
            res.failures[0].reason.contains("too short"),
            "expected short-body reason, got: {}",
            res.failures[0].reason
        );
        // The skip must not fabricate a raw response.
        assert!(res.failures[0].raw_response_head.is_none());
    }

    #[tokio::test]
    async fn phase_1_zero_chapters_errors_cleanly() {
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let chapters: Vec<ChapterInput> = Vec::new();
        let err = runner
            .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("zero target chapters"));
    }

    /// A chat mock that returns well-formed JSON for every phase 1-7
    /// call. Branches on which system preamble is present in the prompt
    /// to return the right shape.
    fn multiphase_chat() -> ChatCompletionFn {
        Arc::new(move |prompt: &ChatPrompt| {
            let sys = prompt.system.to_string();
            let body = if sys.contains("Phase 1") {
                // Echo the first word of the chapter body so different
                // chapters produce questions that embed into different
                // groups under `four_group_embed`.
                let seed = prompt
                    .user
                    .split("**Body:**")
                    .nth(1)
                    .and_then(|b| b.split_whitespace().next())
                    .unwrap_or("question")
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_string();
                format!(r#"{{"questions":["{seed} question for this chapter"]}}"#)
            } else if sys.contains("Phase 3") {
                r#"{"concern_text":"Can meaning survive defiance?","scope":"novel-wide"}"#
                    .to_string()
            } else if sys.contains("Phase 5") {
                // Echo any chunk_id the prompt mentions; grab the first
                // `chunk_id=N` token.
                let cid = prompt
                    .user
                    .split("chunk_id=")
                    .nth(1)
                    .and_then(|s| s.split('`').next())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                format!(
                    r#"{{"position_text":"a position","grounding":[{{"chunk_id":{cid},"section_id":"sec_0001","summary":"s"}}]}}"#
                )
            } else if sys.contains("Phase 6") {
                r#"{"tension":true,"description":"structural parallel","structural_type":"parallel_contrast"}"#.to_string()
            } else if sys.contains("Phase 7") {
                r#"{"gaps":[{"gap_text":"Vronsky social world fades","evidence":"few refs","significance":"medium"}]}"#.to_string()
            } else {
                r#"{"ok":true}"#.to_string()
            };
            Box::pin(async move { Ok(body) })
        })
    }

    /// Deterministic embed that maps text to a 4-dim vector keyed by the
    /// FIRST non-whitespace letter — enough variety to let HDBSCAN
    /// produce two+ clusters when inputs span two letter groups.
    fn four_group_embed() -> EmbedFn {
        Arc::new(move |text: &str| {
            let c = text
                .chars()
                .find(|c| !c.is_whitespace())
                .unwrap_or('z')
                .to_ascii_lowercase();
            let v: Vec<f32> = match c {
                'a'..='g' => vec![1.0, 0.0, 0.0, 0.0],
                'h'..='m' => vec![0.0, 1.0, 0.0, 0.0],
                'n'..='t' => vec![0.0, 0.0, 1.0, 0.0],
                _ => vec![0.0, 0.0, 0.0, 1.0],
            };
            // Add a tiny per-character jitter so HDBSCAN doesn't reject
            // identical vectors as degenerate.
            let len = text.len() as f32;
            let jitter: Vec<f32> = v
                .iter()
                .enumerate()
                .map(|(i, x)| x + 0.001 * (len + i as f32))
                .collect();
            Box::pin(async move { Ok(jitter) })
        })
    }

    fn multiphase_runner(root: &Path) -> PhaseRunner {
        let cache = PhaseCache::new(root.join("cache"));
        let runs = RunOutputWriter::new(root.join("runs"));
        PhaseRunner::new(
            Arc::new(LiteraryPipeline::new()),
            four_group_embed(),
            multiphase_chat(),
            cache,
            runs,
            root.join("exemplars"),
        )
    }

    fn synth_context() -> CorpusContext {
        // Three dense embed-groups × 3 chapters each so HDBSCAN (default
        // `min_cluster_size=3` on LiteraryPipeline) finds at least one
        // cluster per group.
        let groups = [
            ("apples", "Apples and acorns abound here."),
            ("hills", "Hills hide hopeful hares here."),
            ("nectar", "Nectar never numbs nerves here."),
        ];
        let mut chapters = Vec::new();
        let mut chapter_titles = Vec::new();
        for gi in 0..3 {
            for ci in 0..3 {
                let id = format!("ch_{:02}", gi * 3 + ci + 1);
                let title = format!("Chapter {}", gi * 3 + ci + 1);
                chapter_titles.push(title.clone());
                chapters.push(chapter(
                    &id,
                    &title,
                    &format!("{}, variation {}.", groups[gi].1, ci),
                ));
            }
        }
        let mut chunks: Vec<ChunkRecord> = Vec::new();
        let mut cid = 0u64;
        for gi in 0..3 {
            for ci in 0..6 {
                chunks.push(ChunkRecord {
                    id: cid,
                    section_id: format!("sec_{:04}", gi + 1),
                    text: format!("{} variation {}", groups[gi].1, ci),
                });
                cid += 1;
            }
        }
        CorpusContext { chapters, chunks, chapter_titles }
    }

    #[tokio::test]
    async fn phase_2_clusters_questions_from_cache() {
        let dir = tempdir().unwrap();
        let runner = multiphase_runner(dir.path());
        let ctx = synth_context();

        // Seed phase 1 with --full.
        runner
            .phase_1_extract_questions(&ctx.chapters, &ChapterSelection::Full, |_| {})
            .await
            .unwrap();

        let res = runner.phase_2_cluster_questions().await.unwrap();
        assert!(res.cache_updated);
        assert!(res.run_path.exists());
        // Each chapter produced one question; groups are keyed on text's
        // first letter via four_group_embed. Clusters may coalesce
        // depending on HDBSCAN density — we only assert the output
        // shape is coherent, not a specific count.
        let total: usize =
            res.output.clusters.iter().map(|c| c.question_refs.len()).sum::<usize>()
                + res.output.unclustered.len();
        assert_eq!(total, 9);
    }

    #[tokio::test]
    async fn phase_2_atlas_errors_when_cache_has_no_section_extraction() {
        // The v1 LiteraryPipeline doesn't populate section_extraction.
        // Phase 2 atlas against a v1 cache should fail with a clear
        // message pointing the operator at an atlas pipeline.
        let dir = tempdir().unwrap();
        let runner = multiphase_runner(dir.path());
        let ctx = synth_context();
        runner
            .phase_1_extract_questions(&ctx.chapters, &ChapterSelection::Full, |_| {})
            .await
            .unwrap();

        let err = runner.phase_2_cluster_atlas().await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("section_extraction"),
            "expected error about missing atlas sketches: {msg}"
        );
    }

    #[tokio::test]
    async fn phase_2_atlas_clusters_synthesized_sketches() {
        use crate::enrichment::pipeline::atlas::{
            ClaimSketch, DiscourseAct, EnrichmentDepth, EpistemicStatus, QuestionSketch,
            SectionExtraction,
        };
        use crate::enrichment::pipeline::{ExtractedQuestion, Phase1Output};

        let dir = tempdir().unwrap();
        let runner = multiphase_runner(dir.path());

        // Seed a synthetic Phase 1 cache whose chapters carry
        // atlas sketches — bypasses the v1 pipeline, which doesn't
        // produce them.
        let section = |id: &str| SectionExtraction {
            section_id: id.into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            claims: vec![
                ClaimSketch {
                    content: "love costs".into(),
                    discourse_act: DiscourseAct::Enact,
                    epistemic_status: EpistemicStatus::Confident,
                    attributed_to: None,
                    quotable_excerpt: None,
                    anchor: String::new(),
                },
                ClaimSketch {
                    content: "love rewards".into(),
                    discourse_act: DiscourseAct::Enact,
                    epistemic_status: EpistemicStatus::Confident,
                    attributed_to: None,
                    quotable_excerpt: None,
                    anchor: String::new(),
                },
            ],
            questions_raised: vec![QuestionSketch {
                content: "what remains after loss?".into(),
                anchor: String::new(),
            }],
            ..Default::default()
        };
        let phase1 = Phase1Output {
            schema_version: Phase1Output::SCHEMA_VERSION,
            pipeline_id: "literary_atlas".into(),
            questions_by_chapter: vec![
                ExtractedQuestion {
                    chapter_id: "sec_0001".into(),
                    questions: vec!["what remains after loss?".into()],
                    reveals: None,
                    thematic_carriers: Vec::new(),
                    setting: None,
                    plot: None,
                    section_extraction: Some(section("sec_0001")),
                },
                ExtractedQuestion {
                    chapter_id: "sec_0002".into(),
                    questions: vec!["what remains after loss?".into()],
                    reveals: None,
                    thematic_carriers: Vec::new(),
                    setting: None,
                    plot: None,
                    section_extraction: Some(section("sec_0002")),
                },
            ],
            failures: Vec::new(),
            written_at: "t".into(),
        };
        runner
            .cache()
            .write(PipelinePhase::Questions, &phase1)
            .unwrap();

        let res = runner.phase_2_cluster_atlas().await.unwrap();
        assert!(res.cache_updated);
        assert!(res.run_path.exists());
        // 4 claim sketches (2 per chapter × 2 chapters) + 2
        // questions. Every ref lands either in a cluster or in
        // the unclustered noise pile.
        let total: usize = res
            .output
            .clusters
            .iter()
            .map(|c| c.refs.len())
            .sum::<usize>()
            + res.output.unclustered.len();
        assert_eq!(total, 6);
        // Every produced cluster carries its facet tag.
        for cluster in &res.output.clusters {
            assert!(matches!(
                cluster.facet,
                Facet::Claim | Facet::Question
            ));
        }
    }

    #[tokio::test]
    async fn phase_1a_returns_none_for_pipelines_with_no_seed_strategy() {
        // LiteraryPipeline (v1) inherits SeedStrategy::None via the
        // trait default; phase_1a_extract_seed returns None without
        // running chat or writing cache.
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path()); // v1 LiteraryPipeline
        let ctx = CorpusContext {
            chapters: vec![chapter("ch_01", "Chapter 1", "text body")],
            chunks: vec![],
            chapter_titles: vec!["Chapter 1".into()],
        };
        let seed = runner.phase_1a_extract_seed("test_corpus", &ctx, false).await.unwrap();
        assert!(seed.is_none());
        let cached: Option<crate::enrichment::pipeline::atlas::SeedEntities> = runner
            .cache()
            .read(PipelinePhase::SeedExtraction)
            .unwrap();
        assert!(cached.is_none());
    }

    #[tokio::test]
    async fn phase_1a_llm_path_writes_cache_and_threads_seed_into_phase_1() {
        use crate::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path().join("cache"));
        let runs = RunOutputWriter::new(dir.path().join("runs"));

        let saw_seed_block = Arc::new(std::sync::Mutex::new(false));
        let saw_seed_block_c = saw_seed_block.clone();
        let chat: ChatCompletionFn = Arc::new(move |prompt: &ChatPrompt| {
            let is_seed = prompt.system.contains("seed entity list");
            let saw = saw_seed_block_c.clone();
            let body = if is_seed {
                r#"{"entries":[{"canonical_name":"Alyosha","aliases":["Alyoshka"],"entity_type":"person","description":"Youngest Karamazov."}]}"#
                    .to_string()
            } else {
                if prompt.user.contains("Known canonical names") {
                    *saw.lock().unwrap() = true;
                }
                r#"{"section_id":"ch_01","entities_introduced":[{"canonical_name":"Alyosha","entity_type":"person"}],"questions_raised":[{"content":"?"}]}"#
                    .to_string()
            };
            Box::pin(async move { Ok(body) })
        });

        let runner = PhaseRunner::new(
            Arc::new(LiteraryAtlasPipeline::new()),
            alphabet_embed(),
            chat,
            cache,
            runs,
            dir.path().join("exemplars"),
        );

        let ctx = CorpusContext {
            chapters: vec![chapter("ch_01", "Chapter 1", "body with Alyosha")],
            chunks: vec![],
            chapter_titles: vec!["Chapter 1".into()],
        };

        let seed = runner
            .phase_1a_extract_seed("test_corpus", &ctx, false)
            .await
            .unwrap()
            .expect("Llm strategy returns Some");
        assert_eq!(seed.entries.len(), 1);
        assert_eq!(seed.entries[0].canonical_name, "Alyosha");

        // Cache-hit short-circuit: second call no chat.
        let seed2 = runner
            .phase_1a_extract_seed("test_corpus", &ctx, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(seed2.entries[0].canonical_name, "Alyosha");

        // Phase 1 must carry the seed block.
        let _ = runner
            .phase_1_extract_questions(&ctx.chapters, &ChapterSelection::Full, |_| {})
            .await
            .unwrap();
        assert!(
            *saw_seed_block.lock().unwrap(),
            "phase 1 prompt must include the Known canonical names block \
             when Stage 1a seed cache is populated"
        );
    }

    #[tokio::test]
    async fn phase_1a_force_refresh_recomputes_even_when_cache_present() {
        use crate::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path().join("cache"));
        let runs = RunOutputWriter::new(dir.path().join("runs"));

        let chat_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let chat_calls_c = chat_calls.clone();
        let chat: ChatCompletionFn = Arc::new(move |_prompt: &ChatPrompt| {
            chat_calls_c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let body = r#"{"entries":[{"canonical_name":"X","entity_type":"person","description":"x"}]}"#.to_string();
            Box::pin(async move { Ok(body) })
        });

        let runner = PhaseRunner::new(
            Arc::new(LiteraryAtlasPipeline::new()),
            alphabet_embed(),
            chat,
            cache,
            runs,
            dir.path().join("exemplars"),
        );

        let ctx = CorpusContext {
            chapters: vec![chapter("ch_01", "Chapter 1", "body")],
            chunks: vec![],
            chapter_titles: vec!["Chapter 1".into()],
        };

        let _ = runner.phase_1a_extract_seed("c", &ctx, false).await.unwrap();
        let _ = runner.phase_1a_extract_seed("c", &ctx, false).await.unwrap();
        assert_eq!(chat_calls.load(std::sync::atomic::Ordering::Relaxed), 1);

        let _ = runner.phase_1a_extract_seed("c", &ctx, true).await.unwrap();
        assert_eq!(chat_calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn phase_3_requires_phase_2_cache() {
        let dir = tempdir().unwrap();
        let runner = multiphase_runner(dir.path());
        let ctx = synth_context();
        let err = runner.phase_3_name_concerns(&ctx).await.unwrap_err();
        assert!(format!("{err}").contains("cache is missing"));
    }

    #[tokio::test]
    async fn phase_4_clusters_chunks() {
        let dir = tempdir().unwrap();
        let runner = multiphase_runner(dir.path());
        let ctx = synth_context();
        let res = runner.phase_4_cluster_chunks(&ctx).await.unwrap();
        assert!(res.cache_updated);
        // Every non-noise cluster should carry a centroid.
        for c in &res.output.clusters {
            if !c.noise {
                assert!(!c.centroid.is_empty(), "non-noise cluster {} missing centroid", c.id);
            }
        }
    }

    #[tokio::test]
    async fn cascade_from_questions_runs_all_phases() {
        let dir = tempdir().unwrap();
        let runner = multiphase_runner(dir.path());
        let ctx = synth_context();
        let res = runner
            .cascade(
                PipelinePhase::Questions,
                &ctx,
                Some(ChapterSelection::Full),
            )
            .await
            .unwrap();
        // We expect 7 non-Ingest steps.
        assert_eq!(res.steps.len(), 7, "cascade should produce 7 steps");
        // Every phase cache should be populated.
        for phase in [
            PipelinePhase::Questions,
            PipelinePhase::QuestionClusters,
            PipelinePhase::Concerns,
            PipelinePhase::ChunkClusters,
            PipelinePhase::Positions,
            PipelinePhase::Tensions,
            PipelinePhase::Gaps,
        ] {
            let path = runner.cache().path(phase);
            assert!(path.exists(), "cache for {:?} not written", phase);
        }
    }

    #[tokio::test]
    async fn cascade_from_positions_only_reruns_downstream() {
        let dir = tempdir().unwrap();
        let runner = multiphase_runner(dir.path());
        let ctx = synth_context();
        // Seed phases 1-4 first.
        runner
            .phase_1_extract_questions(&ctx.chapters, &ChapterSelection::Full, |_| {})
            .await
            .unwrap();
        runner.phase_2_cluster_questions().await.unwrap();
        runner.phase_3_name_concerns(&ctx).await.unwrap();
        runner.phase_4_cluster_chunks(&ctx).await.unwrap();

        let res = runner
            .cascade(PipelinePhase::Positions, &ctx, None)
            .await
            .unwrap();
        // Positions, Tensions, Gaps — three steps.
        assert_eq!(res.steps.len(), 3);
        for step in &res.steps {
            match step {
                CascadeStep::Phase5(_) | CascadeStep::Phase6(_) | CascadeStep::Phase7(_) => {}
                other => panic!("unexpected cascade step: {other:?}"),
            }
        }
    }

    #[test]
    fn phase1_query_text_clamps_to_budget() {
        let body = "x".repeat(5000);
        let ch = chapter("ch", "Title", &body);
        let q = phase1_query_text(&ch);
        // Title (5) + "\n\n" (2) + 800 chars of body = 807. Allow some
        // slack for char vs byte counting.
        assert!(q.chars().count() <= 810);
        assert!(q.starts_with("Title"));
    }
}
