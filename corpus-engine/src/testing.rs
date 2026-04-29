//! Recipe test harness for `corpus-engine`.
//!
//! Provides [`run_test`] which downloads a small sample of a recipe's data,
//! runs the full extract → chunk → (optionally embed → search) pipeline,
//! and returns a [`TestReport`] that can be rendered as Markdown for
//! inclusion in a community recipe PR.
//!
//! The harness calls the same acquirers, extractors, and chunkers as the
//! production ingest pipeline, but:
//! - Uses `.take(sample_size)` on the extractor iterator to stop early.
//! - For HuggingFace datasets, downloads only the first parquet shard.
//! - Builds a temporary index that is discarded after the run.
//! - Never modifies any production index.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::acquirers::huggingface::{HuggingFaceDatasetAcquirer, HF_USER_AGENT};
use crate::engine::{blake3_hex, normalize_content, CorpusEngine};
use crate::error::{Error, Result};
use crate::index::{CorpusIndex, InsertChunk};
use crate::recipe::{AcquirerConfig, ChunkerConfig, Recipe};

// ── Public types ────────────────────────────────────────────────────────────

/// Options controlling the test harness run.
pub struct TestOptions {
    /// Number of source records to sample. Set to 0 for validation-only mode
    /// (no download, no extraction).
    pub sample_size: usize,
    /// Embed chunks and run a search test. Requires a working `EmbedFn`.
    /// Default: `false`.
    pub embed: bool,
    /// Override the auto-derived test queries (from chunk titles).
    pub queries: Option<Vec<String>>,
    /// Where to write the Markdown report.
    /// Default: `<recipe_dir>/TEST_REPORT.md`.
    pub output: Option<PathBuf>,
    /// Skip the HTTP HEAD-check on the source URL.
    pub offline: bool,
    /// Print per-record extraction outcome to stderr.
    pub verbose: bool,
    /// User-supplied parameter values, validated against the
    /// recipe's `[recipe.parameters]` schema before acquisition.
    /// Empty for recipes without parameters; the test harness
    /// reports "missing required parameter" the same way the
    /// production install path does.
    pub parameters: std::collections::BTreeMap<String, toml::Value>,
}

impl Default for TestOptions {
    fn default() -> Self {
        Self {
            sample_size: 100,
            embed: false,
            queries: None,
            output: None,
            offline: false,
            verbose: false,
            parameters: std::collections::BTreeMap::new(),
        }
    }
}

/// The structured result of a full harness run.
pub struct TestReport {
    pub recipe_id: String,
    pub recipe_name: String,
    pub recipe_path: PathBuf,
    /// RFC3339 timestamp.
    pub tested_at: String,
    /// Value of `CARGO_PKG_VERSION` at compile time.
    pub engine_version: String,
    pub validation: ValidationResult,
    /// `None` when `sample_size == 0` or acquisition failed.
    pub acquisition: Option<AcquisitionResult>,
    /// `None` when acquisition failed.
    pub extraction: Option<ExtractionResult>,
    /// `None` when extraction produced zero documents.
    pub chunking: Option<ChunkingResult>,
    /// Up to 5 sample chunks from the first extracted documents.
    pub sample_chunks: Vec<SampleChunk>,
    /// Results of the test queries. Empty when `embed == false`.
    pub test_queries: Vec<TestQueryResult>,
    /// Rough projection from sample metrics. Always set when chunking ran.
    pub corpus_estimate: Option<CorpusEstimate>,
    /// Whether the embed + search phase was requested.
    pub embed_enabled: bool,
    /// Per-section misses surfaced by the `html_sections` extractor.
    /// Populated after extraction by reading
    /// `<source-dir>/_section_misses.json` if it exists. Empty for
    /// recipes that don't use the section-aware extractor or whose
    /// section regexes all matched. The markdown report renders
    /// these under "Section misses" with a "Suggestion" hint so the
    /// recipe author can iterate on the regex.
    pub section_misses: Vec<crate::extractors::html_sections::MissReport>,
}

impl TestReport {
    /// `true` when the recipe is ready to merge:
    /// - No validation errors
    /// - Extraction rate > 80 % (if extraction ran)
    /// - No chunks exceed `max_chars` (if chunking ran)
    /// - All test queries returned ≥ 1 hit (if embedding ran)
    pub fn passed(&self) -> bool {
        if !self.validation.errors.is_empty() {
            return false;
        }
        if let Some(ref ext) = self.extraction {
            if ext.extraction_rate < 0.80 {
                return false;
            }
        }
        if let Some(ref ch) = self.chunking {
            if ch.chunks_over_limit > 0 {
                return false;
            }
        }
        for q in &self.test_queries {
            if q.hit_count == 0 {
                return false;
            }
        }
        true
    }

    /// Non-fatal observations worth noting in the PR review.
    pub fn warnings(&self) -> Vec<String> {
        let mut w = self.validation.warnings.clone();
        if let Some(ref ext) = self.extraction {
            if ext.extraction_rate >= 0.80 && ext.extraction_rate < 0.90 {
                w.push(format!(
                    "Low extraction rate: {:.1}% (above 80% threshold but below 90%)",
                    ext.extraction_rate * 100.0
                ));
            }
        }
        w
    }

    /// Render this report as the `TEST_REPORT.md` file.
    pub fn to_markdown(&self) -> String {
        let status = if self.passed() { "✅ PASS" } else { "❌ FAIL" };
        let warnings = self.warnings();
        let has_warnings = !warnings.is_empty();
        let display_status = if self.passed() && has_warnings { "⚠️ PASS (with warnings)" } else { status };

        let mut md = String::new();

        // ── Header ──────────────────────────────────────────────────────────
        md.push_str(&format!("# Recipe Test Report: {}\n\n", self.recipe_name));
        md.push_str("| Field | Value |\n|---|---|\n");
        md.push_str(&format!("| Recipe ID | `{}` |\n", self.recipe_id));
        md.push_str(&format!("| Status | {} |\n", display_status));
        md.push_str(&format!("| Tested at | {} |\n", self.tested_at));
        md.push_str(&format!("| Engine version | {} |\n", self.engine_version));
        if let Some(ref acq) = self.acquisition {
            md.push_str(&format!("| Records sampled | {} |\n", acq.records_fetched));
        }
        md.push_str(&format!(
            "| Embed phase | {} |\n",
            if self.embed_enabled { "enabled" } else { "skipped (--no-embed)" }
        ));
        md.push('\n');

        // ── Warnings ────────────────────────────────────────────────────────
        if !warnings.is_empty() {
            md.push_str("## ⚠️ Warnings\n\n");
            for w in &warnings {
                md.push_str(&format!("- {w}\n"));
            }
            md.push('\n');
        }

        // ── Validation ──────────────────────────────────────────────────────
        md.push_str("## Validation\n\n");
        md.push_str("| Check | Status |\n|---|---|\n");
        md.push_str(&format!("| `corpus.id` present | {} |\n", check(self.validation.corpus_id_present)));
        md.push_str(&format!("| `corpus.name` present | {} |\n", check(self.validation.corpus_name_present)));
        md.push_str(&format!("| `corpus.license` present | {} |\n", check(self.validation.license_present)));
        md.push_str(&format!("| Source configured | {} |\n", check(self.validation.source_present)));
        md.push_str(&format!("| Format known | {} |\n", check(self.validation.format_known)));
        match self.validation.source_reachable {
            Some(true) => md.push_str("| Source reachable | ✅ |\n"),
            Some(false) => md.push_str("| Source reachable | ❌ (HEAD request failed) |\n"),
            None => md.push_str("| Source reachable | *(offline — not checked)* |\n"),
        }
        md.push('\n');

        if !self.validation.errors.is_empty() {
            md.push_str("**Validation errors:**\n\n");
            for e in &self.validation.errors {
                md.push_str(&format!("- {e}\n"));
            }
            md.push('\n');
        }

        // ── Acquisition ─────────────────────────────────────────────────────
        md.push_str("## Acquisition\n\n");
        if let Some(ref acq) = self.acquisition {
            md.push_str(&format!("- **Source:** {}\n", acq.source_url));
            md.push_str(&format!("- **Records fetched:** {}\n", acq.records_fetched));
            md.push_str(&format!("- **Bytes downloaded:** {}\n", format_bytes(acq.bytes_downloaded)));
            md.push_str(&format!("- **Duration:** {}ms\n", acq.duration_ms));
        } else {
            md.push_str("*Skipped (validation-only mode or acquisition failed)*\n");
        }
        md.push('\n');

        // ── Extraction ──────────────────────────────────────────────────────
        md.push_str("## Extraction\n\n");
        if let Some(ref ext) = self.extraction {
            md.push_str(&format!(
                "- **Attempted:** {}\n- **Succeeded:** {} ({:.1}%)\n- **Failed:** {}\n",
                ext.records_attempted,
                ext.records_succeeded,
                ext.extraction_rate * 100.0,
                ext.records_attempted - ext.records_succeeded,
            ));
            if !ext.failed_examples.is_empty() {
                md.push_str("\n**Failed examples (up to 5):**\n\n");
                for ex in &ext.failed_examples {
                    md.push_str(&format!("- Record {}: {}\n", ex.index, ex.reason));
                }
            }
        } else {
            md.push_str("*Skipped*\n");
        }
        md.push('\n');

        // ── Section misses (html_sections only) ────────────────────────────
        if !self.section_misses.is_empty() {
            md.push_str("### Section misses\n\n");
            md.push_str(
                "These section regexes did not match the sample. The recipe \
                 author's likely next step is to anchor on the actual heading \
                 the document uses (shown in **nearby text** below).\n\n",
            );
            md.push_str("| File | Section | Nearby text (200 chars) |\n|---|---|---|\n");
            for miss in &self.section_misses {
                let nearby = miss
                    .nearby_text
                    .as_deref()
                    .unwrap_or("(empty)")
                    .replace('\n', " ")
                    .replace('|', "\\|");
                md.push_str(&format!(
                    "| `{}` | `{}` | {} |\n",
                    miss.file, miss.section, nearby,
                ));
            }
            md.push('\n');
        }

        // ── Chunking ────────────────────────────────────────────────────────
        md.push_str("## Chunking\n\n");
        if let Some(ref ch) = self.chunking {
            md.push_str(&format!(
                "- **Total chunks:** {}\n\
                 - **Avg per record:** {:.1}\n\
                 - **Avg chars:** {:.0}\n\
                 - **Min chars:** {}\n\
                 - **Max chars:** {}\n\
                 - **Over limit ({} chars):** {}\n",
                ch.total_chunks,
                ch.avg_per_record,
                ch.avg_chars,
                ch.min_chars,
                ch.max_chars,
                ch.recipe_max_chars,
                ch.chunks_over_limit,
            ));
        } else {
            md.push_str("*Skipped*\n");
        }
        md.push('\n');

        if !self.sample_chunks.is_empty() {
            md.push_str("### Sample chunks\n\n");
            for (i, sc) in self.sample_chunks.iter().enumerate() {
                let title_part = sc.title.as_deref().unwrap_or("(untitled)");
                md.push_str(&format!("**{}. {}** · {} chars\n\n", i + 1, title_part, sc.char_count));
                md.push_str("> ");
                md.push_str(&sc.preview.replace('\n', "\n> "));
                if sc.preview.chars().count() == 400 {
                    md.push_str("…");
                }
                md.push_str("\n\n");
            }
        }

        // ── Embedding & Search ───────────────────────────────────────────────
        md.push_str("## Embedding & Search\n\n");
        if !self.embed_enabled {
            md.push_str("*Skipped — run with `--embed` to test embedding and search.*\n");
        } else if self.test_queries.is_empty() {
            md.push_str("*No queries were generated (no chunk titles available).*\n");
        } else {
            md.push_str("| Query | Hits | Top score | Top result |\n|---|---|---|---|\n");
            for q in &self.test_queries {
                let score = q.top_score.map(|s| format!("{s:.3}")).unwrap_or_else(|| "—".into());
                let top = q.top_title.as_deref().unwrap_or("—");
                md.push_str(&format!("| `{}` | {} | {} | {} |\n", q.query, q.hit_count, score, top));
            }
        }
        md.push('\n');

        // ── Full-corpus estimate ─────────────────────────────────────────────
        md.push_str("## Full-Corpus Estimate\n\n");
        if let Some(ref est) = self.corpus_estimate {
            md.push_str("*Estimated from sample metrics only — treat as a rough order of magnitude.*\n\n");
            md.push_str(&format!(
                "Calculation: `total_records × {:.2} (extraction) × {:.1} (chunks/record)`\n\n",
                est.extraction_rate, est.avg_chunks_per_record,
            ));
            md.push_str("| Metric | Value |\n|---|---|\n");
            match est.total_source_records {
                Some(n) => md.push_str(&format!("| Total source records | ~{n} |\n")),
                None => md.push_str("| Total source records | unknown (single-shard sample) |\n"),
            }
            md.push_str(&format!("| Extraction rate | {:.1}% |\n", est.extraction_rate * 100.0));
            md.push_str(&format!("| Avg chunks/record | {:.1} |\n", est.avg_chunks_per_record));
            match est.estimated_total_chunks {
                Some(n) => md.push_str(&format!("| Estimated total chunks | ~{n} |\n")),
                None => md.push_str("| Estimated total chunks | — |\n"),
            }
            match est.estimated_index_size_gb {
                Some(gb) => md.push_str(&format!("| Estimated index size | ~{gb:.1} GB |\n")),
                None => md.push_str("| Estimated index size | — |\n"),
            }
            if est.test_index_bytes > 0 {
                md.push_str(&format!("| Test index size | {} |\n", format_bytes(est.test_index_bytes)));
            }
        } else {
            md.push_str("*Not available — extraction did not run.*\n");
        }
        md.push('\n');

        // ── Footer ───────────────────────────────────────────────────────────
        md.push_str("---\n\n");
        md.push_str(&format!(
            "*Generated by `corpus-engine` v{} — [How to run this yourself](https://github.com/alexsbryan/corpus-engine/blob/main/README.md#recipe-test-harness)*\n",
            self.engine_version,
        ));

        md
    }
}

/// Static validation results.
pub struct ValidationResult {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub corpus_id_present: bool,
    pub corpus_name_present: bool,
    pub license_present: bool,
    pub source_present: bool,
    pub format_known: bool,
    /// `None` when `--offline` was passed; `Some(false)` on HEAD failure.
    pub source_reachable: Option<bool>,
}

pub struct AcquisitionResult {
    pub source_url: String,
    pub records_fetched: usize,
    pub bytes_downloaded: u64,
    pub duration_ms: u64,
}

pub struct ExtractionResult {
    pub records_attempted: usize,
    pub records_succeeded: usize,
    pub extraction_rate: f32,
    pub failed_examples: Vec<FailedRecord>,
}

pub struct FailedRecord {
    pub index: usize,
    pub reason: String,
}

pub struct ChunkingResult {
    pub total_chunks: usize,
    pub avg_per_record: f32,
    pub avg_chars: f32,
    pub min_chars: usize,
    pub max_chars: usize,
    /// Number of chunks that exceed the recipe's configured `max_chars`.
    pub chunks_over_limit: usize,
    /// The `max_chars` value from the recipe's chunker config.
    pub recipe_max_chars: usize,
}

pub struct SampleChunk {
    pub title: Option<String>,
    pub url: Option<String>,
    pub char_count: usize,
    /// First 400 chars of chunk text.
    pub preview: String,
}

pub struct TestQueryResult {
    pub query: String,
    pub hit_count: usize,
    pub top_score: Option<f32>,
    pub top_title: Option<String>,
}

pub struct CorpusEstimate {
    /// Known only when the source reports a total count.
    pub total_source_records: Option<u64>,
    pub extraction_rate: f32,
    pub avg_chunks_per_record: f32,
    pub estimated_total_chunks: Option<u64>,
    pub estimated_index_size_gb: Option<f32>,
    /// Bytes used by the temporary test index (0 if embed was disabled).
    pub test_index_bytes: u64,
}

// ── Main entry point ────────────────────────────────────────────────────────

/// Run the recipe test harness.
///
/// Returns `Ok(report)` even when phases fail — check `report.passed()`.
/// Returns `Err` only for unrecoverable failures (recipe file not found,
/// I/O errors creating temp directories).
pub(crate) async fn run_test(
    engine: &CorpusEngine,
    recipe_path: &Path,
    options: &TestOptions,
) -> Result<TestReport> {
    // ── Phase 1: Parse ───────────────────────────────────────────────────────
    let mut recipe = Recipe::from_file(recipe_path)?;

    // ── Phase 1b: Resolve parameters (test harness path) ─────────────────────
    // Mirrors the production install flow: validate user-supplied
    // parameter values against the recipe's `[recipe.parameters]`
    // schema before any acquisition runs. Missing required values
    // surface as a validation error so the recipe author sees the
    // full picture in the markdown report.
    //
    // Skipped in validation-only mode (`sample_size == 0`) when no
    // params were supplied: the recipe author is asking "does the
    // schema look right?" — not "would these specific values
    // resolve?". The validate-only path should never require the
    // LLM to fabricate parameter values just to pass.
    let skip_param_resolution =
        options.sample_size == 0 && options.parameters.is_empty();
    if !skip_param_resolution
        && (!recipe.parameters.is_empty() || !options.parameters.is_empty())
    {
        match recipe.resolve_parameters(&options.parameters) {
            Ok(resolved) => {
                recipe = recipe.with_resolved_parameters(resolved);
            }
            Err(e) => {
                let mut validation = validate_recipe(&recipe, options.offline).await;
                validation
                    .errors
                    .push(format!("Parameter resolution failed: {e}"));
                return Ok(TestReport {
                    recipe_id: recipe.corpus.id.clone(),
                    recipe_name: recipe.corpus.name.clone(),
                    recipe_path: recipe_path.to_path_buf(),
                    tested_at: rfc3339_now(),
                    engine_version: env!("CARGO_PKG_VERSION").to_string(),
                    validation,
                    acquisition: None,
                    extraction: None,
                    chunking: None,
                    sample_chunks: Vec::new(),
                    test_queries: Vec::new(),
                    corpus_estimate: None,
                    embed_enabled: options.embed,
                    section_misses: Vec::new(),
                });
            }
        }
    }

    // ── Phase 2: Validate ────────────────────────────────────────────────────
    let validation = validate_recipe(&recipe, options.offline).await;

    let mut report = TestReport {
        recipe_id: recipe.corpus.id.clone(),
        recipe_name: recipe.corpus.name.clone(),
        recipe_path: recipe_path.to_path_buf(),
        tested_at: rfc3339_now(),
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
        validation,
        acquisition: None,
        extraction: None,
        chunking: None,
        sample_chunks: Vec::new(),
        test_queries: Vec::new(),
        corpus_estimate: None,
        embed_enabled: options.embed,
        section_misses: Vec::new(),
    };

    // Validation-only mode: stop here.
    if options.sample_size == 0 {
        return Ok(report);
    }

    // ── Phase 3: Acquire ─────────────────────────────────────────────────────
    let download_dir = std::env::temp_dir().join("corpus-engine-test-downloads");
    std::fs::create_dir_all(&download_dir)?;

    let acquire_start = Instant::now();
    let source_path = match acquire_for_test(engine, &recipe, &download_dir).await {
        Ok(p) => p,
        Err(e) => {
            report.validation.errors.push(format!("Acquisition failed: {e}"));
            return Ok(report);
        }
    };

    let source_url_display = acquirer_source_url(&recipe);
    let bytes_downloaded = {
        // Best-effort: report size of what we downloaded (may be pre-existing).
        if source_path.is_file() {
            std::fs::metadata(&source_path).map(|m| m.len()).unwrap_or(0)
        } else if source_path.is_dir() {
            dir_size_bytes(&source_path)
        } else {
            0
        }
    };

    report.acquisition = Some(AcquisitionResult {
        source_url: source_url_display,
        records_fetched: 0, // updated after extraction
        bytes_downloaded,
        duration_ms: acquire_start.elapsed().as_millis() as u64,
    });

    // ── Phase 4: Extract ─────────────────────────────────────────────────────
    let extractor = engine.make_extractor(&recipe.extract);
    let doc_iter = match extractor.extract(&source_path) {
        Ok(iter) => iter,
        Err(e) => {
            report.validation.errors.push(format!("Extractor failed to open source: {e}"));
            return Ok(report);
        }
    };

    let mut docs = Vec::new();
    let mut failed_examples = Vec::new();
    let mut attempted = 0usize;

    for doc_result in doc_iter.take(options.sample_size) {
        attempted += 1;
        match doc_result {
            Ok(doc) if !doc.content.is_empty() => {
                if options.verbose {
                    eprintln!(
                        "[{attempted}] OK: {}",
                        doc.title.as_deref().unwrap_or("<untitled>")
                    );
                }
                docs.push(doc);
            }
            Ok(_) => {
                if options.verbose {
                    eprintln!("[{attempted}] SKIP: content field empty");
                }
                if failed_examples.len() < 5 {
                    failed_examples.push(FailedRecord {
                        index: attempted,
                        reason: "content field is empty".into(),
                    });
                }
            }
            Err(e) => {
                if options.verbose {
                    eprintln!("[{attempted}] FAIL: {e}");
                }
                if failed_examples.len() < 5 {
                    failed_examples.push(FailedRecord {
                        index: attempted,
                        reason: e.to_string(),
                    });
                }
            }
        }
    }

    let extraction_rate = if attempted > 0 {
        docs.len() as f32 / attempted as f32
    } else {
        0.0
    };

    report.extraction = Some(ExtractionResult {
        records_attempted: attempted,
        records_succeeded: docs.len(),
        extraction_rate,
        failed_examples,
    });

    if let Some(ref mut acq) = report.acquisition {
        acq.records_fetched = attempted;
    }

    // Slurp the html_sections miss sidecar if the extractor wrote
    // one. Empty / missing file means no misses to report.
    let misses_candidates = [
        source_path.join("_section_misses.json"),
        source_path
            .parent()
            .map(|p| p.join("_section_misses.json"))
            .unwrap_or_default(),
    ];
    for candidate in &misses_candidates {
        if candidate.is_file() {
            if let Ok(raw) = std::fs::read_to_string(candidate) {
                if let Ok(parsed) = serde_json::from_str::<
                    Vec<crate::extractors::html_sections::MissReport>,
                >(&raw)
                {
                    report.section_misses = parsed;
                    break;
                }
            }
        }
    }

    if docs.is_empty() {
        return Ok(report);
    }

    // ── Phase 5: Chunk ───────────────────────────────────────────────────────
    let chunker = engine.make_chunker(&recipe.chunk);
    let recipe_max_chars = chunker_max_chars(&recipe.chunk);

    // Store (title, url, content) for each chunk.
    let mut all_chunks: Vec<(Option<String>, Option<String>, String)> = Vec::new();

    for doc in &docs {
        let cleaned = normalize_content(&doc.content);
        let text_chunks = chunker.chunk(&cleaned);

        for tc in text_chunks {
            // Prepend title, same as the production ingest pipeline.
            let content = match &doc.title {
                Some(t) if !tc.content.starts_with(t.as_str()) => {
                    format!("{t}\n\n{}", tc.content)
                }
                _ => tc.content,
            };
            all_chunks.push((doc.title.clone(), doc.url.clone(), content));
        }
    }

    // Sample chunks for the report (first 5).
    for (title, url, content) in all_chunks.iter().take(5) {
        let char_count = content.chars().count();
        let preview: String = content.chars().take(400).collect();
        report.sample_chunks.push(SampleChunk {
            title: title.clone(),
            url: url.clone(),
            char_count,
            preview,
        });
    }

    let total_chunks = all_chunks.len();
    let avg_per_record = total_chunks as f32 / docs.len() as f32;
    let char_counts: Vec<usize> = all_chunks.iter().map(|(_, _, c)| c.chars().count()).collect();
    let avg_chars = char_counts.iter().sum::<usize>() as f32 / char_counts.len() as f32;
    let min_chars = char_counts.iter().copied().min().unwrap_or(0);
    let max_chunk_chars = char_counts.iter().copied().max().unwrap_or(0);
    let chunks_over_limit = char_counts.iter().filter(|&&c| c > recipe_max_chars).count();

    report.chunking = Some(ChunkingResult {
        total_chunks,
        avg_per_record,
        avg_chars,
        min_chars,
        max_chars: max_chunk_chars,
        chunks_over_limit,
        recipe_max_chars,
    });

    // Corpus estimate (no source total available from a single-shard sample).
    report.corpus_estimate = Some(CorpusEstimate {
        total_source_records: None,
        extraction_rate,
        avg_chunks_per_record: avg_per_record,
        estimated_total_chunks: None,
        estimated_index_size_gb: None,
        test_index_bytes: 0,
    });

    // ── Phase 6: Embed + test index ──────────────────────────────────────────
    if options.embed && !all_chunks.is_empty() {
        let test_index_dir = std::env::temp_dir().join(format!(
            "corpus-engine-test-index-{}",
            recipe.corpus.id
        ));
        let _ = std::fs::remove_dir_all(&test_index_dir);
        std::fs::create_dir_all(&test_index_dir)?;

        // Probe embed to discover actual dimensions.
        let probe = match engine.embed("probe").await {
            Ok(v) => v,
            Err(e) => {
                report.validation.warnings.push(format!(
                    "Embed probe failed — skipping embed phase: {e}"
                ));
                return Ok(report);
            }
        };
        let dim = probe.len();

        let index = match CorpusIndex::create(
            &test_index_dir,
            &recipe.corpus.id,
            &recipe.corpus.name,
            &recipe.index.embedding_model,
            dim,
            recipe.corpus.mesh_sharing,
            &recipe.corpus.license,
        )
        .await
        {
            Ok(idx) => idx,
            Err(e) => {
                report.validation.warnings.push(format!(
                    "Failed to create test index: {e}"
                ));
                let _ = std::fs::remove_dir_all(&test_index_dir);
                return Ok(report);
            }
        };

        // Embed all chunks.
        let mut batch: Vec<(InsertChunk, Vec<f32>)> = Vec::new();
        for (title, url, content) in &all_chunks {
            match engine.embed(content).await {
                Ok(emb) => {
                    batch.push((
                        InsertChunk {
                            content: content.clone(),
                            title: title.clone(),
                            url: url.clone(),
                            metadata: None,
                            content_hash: Some(blake3_hex(content)),
                            source_doc_id: url.clone(),
                            source_file: None,
                            code: crate::index::InsertCodeMeta::default(),
                            unit_id: None,
                        },
                        emb,
                    ));
                }
                Err(e) => {
                    report.validation.warnings.push(format!(
                        "Embed failed for chunk: {e}"
                    ));
                }
            }
        }

        let test_index_bytes = if let Err(e) = index.insert_batch(&batch).await {
            report.validation.warnings.push(format!("Index insert failed: {e}"));
            0u64
        } else {
            let _ = index.build_indexes(true, true, None).await;
            dir_size_bytes(&test_index_dir)
        };

        if let Some(ref mut est) = report.corpus_estimate {
            est.test_index_bytes = test_index_bytes;
            // Estimate full index size from test index bytes per chunk.
            if total_chunks > 0 && test_index_bytes > 0 {
                // bytes_per_chunk × estimated_total_chunks → size in GB
                // We don't know total_chunks, so we leave estimation as None.
            }
        }

        // ── Phase 7: Test queries ─────────────────────────────────────────────
        let queries = options.queries.clone().unwrap_or_else(|| {
            report
                .sample_chunks
                .iter()
                .filter_map(|sc| sc.title.as_deref())
                .take(5)
                .map(|t| {
                    t.split_whitespace()
                        .take(4)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .filter(|q| !q.is_empty())
                .collect()
        });

        for query in &queries {
            let qemb = match engine.embed(query).await {
                Ok(v) => v,
                Err(e) => {
                    report.test_queries.push(TestQueryResult {
                        query: query.clone(),
                        hit_count: 0,
                        top_score: None,
                        top_title: Some(format!("embed error: {e}")),
                    });
                    continue;
                }
            };

            match index.search(&qemb, query, 10).await {
                Ok(hits) => {
                    report.test_queries.push(TestQueryResult {
                        query: query.clone(),
                        hit_count: hits.len(),
                        top_score: hits.first().map(|h| h.score),
                        top_title: hits.first().and_then(|h| h.title.clone()),
                    });
                }
                Err(e) => {
                    report.test_queries.push(TestQueryResult {
                        query: query.clone(),
                        hit_count: 0,
                        top_score: None,
                        top_title: Some(format!("search error: {e}")),
                    });
                }
            }
        }

        // Clean up the temporary test index.
        let _ = std::fs::remove_dir_all(&test_index_dir);
    }

    Ok(report)
}

// ── Private helpers ─────────────────────────────────────────────────────────

/// Validate the recipe fields and optionally check source reachability.
async fn validate_recipe(recipe: &Recipe, offline: bool) -> ValidationResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let corpus_id_present = !recipe.corpus.id.is_empty();
    let corpus_name_present = !recipe.corpus.name.is_empty();
    let license_present = !recipe.corpus.license.is_empty();

    if !corpus_id_present {
        errors.push("`corpus.id` is required but missing".into());
    }
    if !corpus_name_present {
        errors.push("`corpus.name` is required but missing".into());
    }
    if !license_present {
        warnings.push("`corpus.license` is empty — add a SPDX identifier (e.g. `CC-BY-SA-4.0`)".into());
    }

    // ── html_sections regex compilation + section breadth heuristic ───
    if let crate::recipe::ExtractorConfig::HtmlSections {
        sections,
        fallback: _,
        title_selector: _,
    } = &recipe.extract
    {
        validate_html_sections(sections, &mut errors, &mut warnings);
    }

    // ── http_api: URL-template lint vs declared parameters ────────────
    if let crate::recipe::AcquirerConfig::HttpApi {
        requests,
        headers,
        ..
    } = &recipe.acquire
    {
        validate_http_api_templates(
            requests,
            headers.as_ref(),
            &recipe.parameters,
            &mut errors,
            &mut warnings,
        );
    }

    // ── investigation patterns: surface reserved variants ─────────────
    // CustomSql is reserved but not yet implemented; recipes that
    // declare it parse fine and the pattern detector emits a
    // placeholder finding, but the recipe author should know up
    // front that the SQL won't actually run yet.
    if let Some(enr) = recipe.enrichment.as_ref() {
        for pattern in &enr.patterns {
            if let crate::recipe::PatternDecl::CustomSql { name, .. } = pattern {
                warnings.push(format!(
                    "pattern `{name}` uses `type = \"custom_sql\"` which is \
                     reserved for a future engine version. The shape parses \
                     today and the detector emits a placeholder finding, but \
                     the SQL will NOT execute until the sandboxed-rusqlite \
                     runtime ships. Track this in SYSTEM_OVERVIEW.md §3.10."
                ));
            }
        }
    }

    let source_present = !matches!(
        &recipe.acquire,
        AcquirerConfig::LocalFile { path } if path.is_empty()
    );

    let format_known = true; // recipe parsed successfully implies format is known

    let source_reachable = if offline {
        None
    } else {
        let url = acquirer_source_url(recipe);
        Some(head_check(&url).await)
    };

    ValidationResult {
        errors,
        warnings,
        corpus_id_present,
        corpus_name_present,
        license_present,
        source_present,
        format_known,
        source_reachable,
    }
}

/// Compile every regex in an `html_sections` extractor and surface
/// failures as recipe errors. Also flags broad / ungrounded patterns
/// (no anchors, very short, generic English words) as warnings —
/// the recipe author can ignore those, but they mostly indicate
/// something the test harness will catch as a false-positive match.
fn validate_html_sections(
    sections: &[crate::recipe::SectionRule],
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    if sections.is_empty() {
        errors.push(
            "html_sections extractor declared but `[[extract.sections]]` is empty"
                .into(),
        );
        return;
    }
    for s in sections {
        if let Err(e) = regex::Regex::new(&s.start_pattern) {
            errors.push(format!(
                "section `{}`: invalid start_pattern `{}`: {e}",
                s.name, s.start_pattern
            ));
        }
        if let Err(e) = regex::Regex::new(&s.end_pattern) {
            errors.push(format!(
                "section `{}`: invalid end_pattern `{}`: {e}",
                s.name, s.end_pattern
            ));
        }
        // Heuristic: a literal-only pattern shorter than 4 chars is
        // almost certainly going to false-positive. Flag once.
        if s.start_pattern.chars().count() < 4 {
            warnings.push(format!(
                "section `{}`: start_pattern `{}` is very short — \
                 likely matches false positives. Anchor with `^` or \
                 a heading marker.",
                s.name, s.start_pattern
            ));
        }
    }
}

/// Walk every `{name}` placeholder in the http_api templates and
/// confirm it references either `base_url` (the acquirer's special
/// reserved name) or a parameter declared in `[recipe.parameters]`.
/// Mis-spelled placeholders silently break ingest, so catching them
/// at validation time is one of the highest-leverage checks the
/// schema-aware validator can do.
fn validate_http_api_templates(
    requests: &[crate::recipe::RequestTemplate],
    headers: Option<&std::collections::BTreeMap<String, String>>,
    parameters: &std::collections::BTreeMap<String, crate::recipe::ParameterSpec>,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    use std::sync::OnceLock;
    static PLACEHOLDER: OnceLock<regex::Regex> = OnceLock::new();
    let re = PLACEHOLDER.get_or_init(|| {
        regex::Regex::new(r"\{([a-zA-Z_][a-zA-Z0-9_]*)\}").unwrap()
    });

    let known: std::collections::HashSet<String> = parameters
        .keys()
        .cloned()
        .chain(std::iter::once("base_url".to_string()))
        .collect();

    let mut undeclared: std::collections::BTreeSet<String> = Default::default();

    for (idx, req) in requests.iter().enumerate() {
        // for_each names must reference declared parameters
        for name in &req.for_each {
            if !parameters.contains_key(name) {
                errors.push(format!(
                    "request[{idx}].for_each references undeclared parameter `{name}` \
                     — declared: [{}]",
                    parameters
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        for cap in re.captures_iter(&req.url) {
            let name = cap[1].to_string();
            if !known.contains(&name) {
                undeclared.insert(name);
            }
        }
        if let Some(body) = &req.body {
            for cap in re.captures_iter(body) {
                let name = cap[1].to_string();
                if !known.contains(&name) {
                    undeclared.insert(name);
                }
            }
        }
    }
    if let Some(h) = headers {
        for (k, v) in h {
            for cap in re.captures_iter(v) {
                let name = cap[1].to_string();
                if !known.contains(&name) {
                    undeclared.insert(name.clone());
                }
            }
            // Header names with placeholders are not supported.
            if re.is_match(k) {
                warnings.push(format!(
                    "header name `{k}` contains `{{name}}` — header names are \
                     not interpolated; only header *values* are"
                ));
            }
        }
    }

    if !undeclared.is_empty() {
        let names: Vec<String> = undeclared
            .iter()
            .map(|n| format!("{{{n}}}"))
            .collect();
        errors.push(format!(
            "http_api templates reference undeclared placeholder(s): {} — \
             declared parameters: [{}]",
            names.join(", "),
            parameters
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
}

/// Acquire a sample source file for the test harness.
///
/// For HuggingFace datasets: downloads only the **first** parquet shard.
/// For BulkDownload / LocalFile: delegates to the standard acquirer.
async fn acquire_for_test(
    engine: &CorpusEngine,
    recipe: &Recipe,
    download_dir: &Path,
) -> Result<PathBuf> {
    match &recipe.acquire {
        AcquirerConfig::HuggingFaceDataset { repo, subset, .. } => {
            // Only download the first shard to keep test runs lightweight.
            let acq = HuggingFaceDatasetAcquirer::new(repo, subset.as_deref());
            let client = reqwest::Client::builder()
                .user_agent(HF_USER_AGENT)
                .build()
                .map_err(|e| Error::Http(e))?;

            let shards = acq.list_shards(&client).await?;
            if shards.is_empty() {
                return Err(Error::Recipe(format!(
                    "HuggingFace dataset '{repo}' returned no parquet shards"
                )));
            }

            let (shard_name, shard_url) = &shards[0];
            let dest_dir = download_dir.join(&recipe.corpus.id);
            std::fs::create_dir_all(&dest_dir)?;

            let final_path = dest_dir.join(shard_name);
            if !final_path.exists() {
                let part_path = dest_dir.join(format!("{shard_name}.part"));
                let response = client.get(shard_url).send().await.map_err(Error::Http)?;
                if !response.status().is_success() {
                    return Err(Error::Recipe(format!(
                        "HuggingFace shard download failed: HTTP {}",
                        response.status()
                    )));
                }
                let bytes = response.bytes().await.map_err(Error::Http)?;
                std::fs::write(&part_path, &bytes)?;
                std::fs::rename(&part_path, &final_path)?;
            }

            Ok(dest_dir)
        }

        // For other acquirers, use the engine's standard implementation.
        // BulkDownload: downloads the full file (potentially large).
        // LocalFile: validates the path and returns it.
        _ => {
            engine
                .acquire_source(recipe, download_dir, &None)
                .await
        }
    }
}

/// Returns the display URL for the recipe's source.
fn acquirer_source_url(recipe: &Recipe) -> String {
    match &recipe.acquire {
        AcquirerConfig::BulkDownload { url, urls, .. } => match (url, urls) {
            (Some(u), _) => u.clone(),
            (None, Some(us)) if !us.is_empty() => us.join(", "),
            _ => "(no source URL)".into(),
        },
        AcquirerConfig::HuggingFaceDataset { repo, subset, .. } => match subset {
            Some(s) => format!("https://huggingface.co/datasets/{repo} (subset: {s})"),
            None => format!("https://huggingface.co/datasets/{repo}"),
        },
        AcquirerConfig::WebCrawl { seed_urls, .. } => {
            seed_urls.first().cloned().unwrap_or_else(|| "(no seed URL)".into())
        }
        AcquirerConfig::HttpApi {
            base_url,
            requests,
            ..
        } => {
            // Prefer the base_url for surfacing in test reports; fall
            // back to the first request's URL template (which may
            // still contain unresolved `{name}` placeholders — that's
            // fine for diagnostics, the resolver runs later).
            if !base_url.is_empty() {
                base_url.clone()
            } else {
                requests
                    .first()
                    .map(|r| r.url.clone())
                    .unwrap_or_else(|| "(no http_api URL)".into())
            }
        }
        AcquirerConfig::LocalFile { path } => format!("file://{path}"),
        AcquirerConfig::Custom { kind, .. } => format!("custom:{kind}"),
    }
}

/// Extract `max_chars` from any `ChunkerConfig` variant.
fn chunker_max_chars(config: &ChunkerConfig) -> usize {
    match config {
        ChunkerConfig::Paragraph { max_chars, .. } => *max_chars,
        ChunkerConfig::Sentence { max_chars } => *max_chars,
        ChunkerConfig::Fixed { max_chars, .. } => *max_chars,
        ChunkerConfig::Semantic { max_chars } => *max_chars,
        // Passthrough has no bound — the upstream extractor (e.g. `code`)
        // is responsible for keeping pieces chunk-sized.
        ChunkerConfig::Passthrough => usize::MAX,
    }
}

/// Send an HTTP HEAD request and return whether the server responded 2xx.
async fn head_check(url: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.head(url).send().await {
        Ok(resp) => resp.status().is_success() || resp.status().as_u16() == 301 || resp.status().as_u16() == 302,
        Err(_) => false,
    }
}

/// Recursively sum the sizes of all files in a directory.
fn dir_size_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                total += std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            } else if p.is_dir() {
                total += dir_size_bytes(&p);
            }
        }
    }
    total
}

/// Format a byte count as a human-readable string (KB / MB / GB).
fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.2} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Return `✅` or `❌` for a boolean check.
fn check(v: bool) -> &'static str {
    if v { "✅" } else { "❌" }
}

/// Generate a rough RFC3339 timestamp using stdlib only (no chrono).
fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, mo, d, h, min, s) = unix_to_date_parts(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

/// Convert a Unix timestamp (seconds since epoch) to (year, month, day, hour, min, sec).
fn unix_to_date_parts(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let sec = secs % 60;
    let min = (secs / 60) % 60;
    let hour = (secs / 3600) % 24;
    let mut days = secs / 86400;

    let mut year = 1970u64;
    loop {
        let leap = is_leap(year);
        let days_in_year = if leap { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = is_leap(year);
    let month_days: [u64; 12] = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 0u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }

    (year, month + 1, days + 1, hour, min, sec)
}

fn is_leap(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal recipe TOML factory for validate-only tests. Uses
    /// `bulk_download` so we don't accidentally exercise other
    /// validators; the field-of-interest is plugged in by the test.
    fn parse_recipe(extra: &str) -> Recipe {
        let toml = format!(
            r#"
[corpus]
id = "demo"
name = "demo"
license = "Apache-2.0"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "plaintext"

[chunk]
type = "sentence"

{extra}
"#
        );
        Recipe::from_toml(&toml).expect("recipe parses")
    }

    #[tokio::test]
    async fn validate_flags_invalid_html_sections_regex() {
        let toml = r#"
[corpus]
id = "demo"
name = "demo"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "html_sections"

[[extract.sections]]
name = "md_and_a"
description = "Management Discussion & Analysis"
start_pattern = "(((unbalanced"
end_pattern = "(?i)item\\s+8"

[chunk]
type = "sentence"
"#;
        let recipe = Recipe::from_toml(toml).unwrap();
        let result = validate_recipe(&recipe, true).await;
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("md_and_a") && e.contains("invalid start_pattern")),
            "expected invalid-regex error, got {:?}",
            result.errors
        );
    }

    #[tokio::test]
    async fn validate_warns_on_short_pattern() {
        let toml = r#"
[corpus]
id = "demo"
name = "demo"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "html_sections"

[[extract.sections]]
name = "x"
description = "x"
start_pattern = "Hi"
end_pattern = "Bye"

[chunk]
type = "sentence"
"#;
        let recipe = Recipe::from_toml(toml).unwrap();
        let result = validate_recipe(&recipe, true).await;
        assert!(
            result.warnings.iter().any(|w| w.contains("very short")),
            "expected very-short warning, got {:?}",
            result.warnings
        );
    }

    #[tokio::test]
    async fn validate_flags_undeclared_placeholders_in_http_api() {
        let toml = r#"
[corpus]
id = "demo"
name = "demo"

[parameters.entity]
type = "list"
required = true

[acquire]
type = "http_api"
base_url = "https://api.example.com"

[[acquire.requests]]
url = "{base_url}?q={entity}&category={category}"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#;
        let recipe = Recipe::from_toml(toml).unwrap();
        let result = validate_recipe(&recipe, true).await;
        // {entity} is declared, {category} is not. Only {category} should error.
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("{category}") && !e.contains("{entity}")),
            "expected undeclared `{{category}}` error, got {:?}",
            result.errors
        );
    }

    #[tokio::test]
    async fn validate_flags_for_each_pointing_at_undeclared_param() {
        let toml = r#"
[corpus]
id = "demo"
name = "demo"

[parameters.entity]
type = "list"
required = true

[acquire]
type = "http_api"
base_url = "https://api.example.com"

[[acquire.requests]]
url = "{base_url}?q={entity}"
for_each = ["entity", "form_type"]

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#;
        let recipe = Recipe::from_toml(toml).unwrap();
        let result = validate_recipe(&recipe, true).await;
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("for_each") && e.contains("form_type")),
            "expected for_each-undeclared error, got {:?}",
            result.errors
        );
    }

    #[tokio::test]
    async fn validate_passes_clean_http_api_recipe() {
        let toml = r#"
[corpus]
id = "demo"
name = "demo"
license = "MIT"

[parameters.entity]
type = "list"
required = true

[parameters.start_date]
type = "date"
default = "2022-01-01"

[acquire]
type = "http_api"
base_url = "https://api.example.com"

[[acquire.requests]]
url = "{base_url}/search?q={entity}&from={start_date}"
for_each = ["entity"]

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#;
        let recipe = Recipe::from_toml(toml).unwrap();
        let result = validate_recipe(&recipe, true).await;
        assert!(
            result.errors.is_empty(),
            "clean recipe should validate, got errors: {:?}",
            result.errors
        );
    }

    /// Minimal smoke test to confirm the helper still produces a
    /// usable Recipe. Mostly exists so the `parse_recipe` helper
    /// stays warning-free if specific tests above are commented out
    /// during local iteration.
    #[tokio::test]
    async fn parse_recipe_helper_works() {
        let r = parse_recipe("");
        assert_eq!(r.corpus.id, "demo");
    }
}
