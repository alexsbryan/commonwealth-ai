// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich init <corpus> --source <path>` — first-run setup.
//!
//! Responsibilities:
//!   1. Probe the daemon + resolve default chat/embed models.
//!   2. Read the source file; run the `SectionedChunker` (respecting
//!      `--chapter-regex`).
//!   3. On `--dry-run`, print the `SectionReport` and exit 0.
//!   4. Otherwise: write `chapters.json` + `config.json` + scaffold
//!      the `exemplars/`, `cache/`, `runs/` directories.
//!
//! Tuning shortcut: `--from-template <name>` (or `--template-path
//! <path>`) materialises a built-in philosophy fixture into a
//! synthesised source file under the corpus dir and proceeds with the
//! normal init flow. Used by the `enrich eval` harness to scaffold a
//! reproducible corpus against which prompt iterations are scored.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::chunkers::sectioned::{ChapterRegexDetector, SectionedChunker};
use corpus_engine::enrichment::pipeline::ChapterManifest;
use corpus_engine::{CorpusEngine, EmbedFn};

use super::config::{EnrichConfig, TocMarkers, CONFIG_SCHEMA_VERSION};
use super::inference_client::{probe_daemon, resolve_default_models};
use super::paths;
use super::templates;
use sovereign_cli_shared::help::{self, Help, HelpSection};
use sovereign_cli_shared::prompts::{confirm, stdin_is_tty};
use sovereign_cli_shared::urls::DEFAULT_CLIENT_PORT;

const HELP: Help = Help {
    command: "svrn enrich init",
    summary: "Scaffold an enrichment-admin tree for a corpus: chapters.json + config.json + dirs.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich init <corpus-id> --source <path> \\\n  [--chapter-regex <pat> | --toc [--toc-start <m>] [--toc-end <m>]] \\\n  [--min-section-body-words <n>] [--pipeline <id>] [--chat-model <id>] [--embed-model <id>] \\\n  [--dry-run] [--force]",
        ),
        HelpSection::Flags(&[
            ("--source <path>", "Absolute path to the plaintext source file. Required unless --from-template / --template-path / --from-corpus is used."),
            (
                "--from-corpus <id>",
                "Drive enrichment off an already-indexed multi-document corpus (e.g. \
                 `wikipedia`). Reads the LanceDB chunks, groups by (source_doc_id, \
                 section_path), and writes one chapter per (article, section) into \
                 chapters.json — no source file or chunker needed. Mutually exclusive \
                 with --source / --from-template / --template-path.",
            ),
            (
                "--limit-articles <N>",
                "Truncate --from-corpus to the first N articles by source_doc_id sort \
                 order. Useful for nailing down enrichment shape on a small sample \
                 before scaling to the full corpus.",
            ),
            (
                "--include-articles <path>",
                "Restrict --from-corpus to article titles listed in <path>. Accepts \
                 plain titles (one per line; lines beginning with # and blank lines \
                 are ignored) OR the JSON produced by `svrn enrich \
                 triage-candidates --json` (reads top_in_corpus_by_centrality[*].name). \
                 Title match is case + underscore folded. Mutually exclusive with \
                 --limit-articles.",
            ),
            (
                "--from-template <name>",
                "Materialise a built-in fixture into a synthesised source file under the corpus dir, then proceed normally. Pins the template's pipeline_id + min-section-body-words=20 unless overridden. Available names: free-will-debate, virtue-ethics-fragments, stoicism-mini (philosophy); bk-book-1, dubliners-3 (literary).",
            ),
            (
                "--template-path <path>",
                "Same as --from-template but reads the TOML template from a user-supplied path. Useful for prompt-tuning iterations on hand-authored fixtures before promoting them into the built-in registry.",
            ),
            ("--chapter-regex <pat>", "Override the default section-detector pattern."),
            ("--pipeline <id>", "Pipeline id from the registry. Default: literary (or template's pipeline_id when --from-template is used)."),
            ("--chat-model <id>", "Pin a chat model id. Default: auto-resolve from /v1/models."),
            ("--embed-model <id>", "Pin an embed model id. Default: auto-resolve from /v1/models."),
            (
                "--min-section-body-words <n>",
                "Drop sections whose body has fewer than <n> words. Guards against a regex \
                 that matches both a list-of-headings index and the real bodies. Default 40; \
                 set to 0 to disable.",
            ),
            (
                "--toc",
                "Drive section detection from an author-declared Table of Contents between \
                 [[CONTENTS]] and [[/CONTENTS]] markers instead of the regex. The titles \
                 inside become section anchors when they reappear at line starts below.",
            ),
            (
                "--toc-start <marker>",
                "Override the default ToC start marker ([[CONTENTS]]). Implies --toc.",
            ),
            (
                "--toc-end <marker>",
                "Override the default ToC end marker ([[/CONTENTS]]). Implies --toc.",
            ),
            ("--dry-run", "Print detected sections and exit without writing anything."),
            ("--force", "Overwrite an existing config.json."),
        ]),
        HelpSection::Examples(&[
            (
                "svrn enrich init anna-karenina --source ~/books/ak.txt",
                "First-run setup with auto-resolved models and default chapter regex.",
            ),
            (
                "svrn enrich init ak --source ak.txt --chapter-regex '^BOOK [A-Z]+' --dry-run",
                "Preview section detection with a custom regex; do not write state.",
            ),
            (
                "svrn enrich init bk --source bk.txt --pipeline literary_atlas",
                "Use the atlas-schema Phase 1 extractor (full atom graph) instead of the legacy questions-only pipeline.",
            ),
            (
                "svrn enrich init compatibilism --source compatibilism.md --pipeline philosophy_atlas",
                "Philosophy-tuned atlas pipeline (same schema, argumentative-prose prompts).",
            ),
            (
                "svrn enrich init fwd --from-template free-will-debate",
                "Scaffold a corpus from the bundled `free-will-debate` philosophy fixture. The eval harness scores Gemma-4B output against bench/philosophy/free-will-debate.toml.",
            ),
        ]),
        HelpSection::Notes(
            "Writes to ~/.sovereign/enrichment/<corpus>/ and ~/.sovereign/indexes/<corpus>/. \
             config.json pins the chapter regex + model ids so every later subcommand operates \
             against a reproducible shape. Re-run with --force to overwrite.",
        ),
    ],
};

/// Load a corpus's custom atlas ontology from its recipe, if it declares one.
///
/// The `custom_atlas` pipeline is built from DATA, not the registry: this reads
/// `<data_dir>/recipes/<corpus_id>/recipe.toml` and materializes
/// `[enrichment.ontology]` into a [`corpus_engine::enrichment::pipeline::CustomAtlasSpec`]
/// (the single recipe→pipeline mapping is `Recipe::custom_atlas_spec`). `None`
/// when the recipe is missing or has no non-empty ontology guidance — so the
/// caller can fall back to (or reject) a registry pipeline.
fn custom_ontology_spec(
    corpus_id: &str,
) -> Option<corpus_engine::enrichment::pipeline::CustomAtlasSpec> {
    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .ok()?;
    let recipe_path = data_dir.join("recipes").join(corpus_id).join("recipe.toml");
    corpus_engine::Recipe::from_file(&recipe_path)
        .ok()?
        .custom_atlas_spec()
}

pub async fn cmd_init(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }

    let mut parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };

    // If the operator passed --from-template / --template-path,
    // materialise the template's prose into a file under the corpus
    // dir and continue as if --source had pointed at that file.
    // Template metadata supplies sensible defaults for pipeline,
    // chapter regex, and section body floor — the operator can still
    // override each one explicitly.
    if parsed.from_template.is_some() || parsed.template_path.is_some() {
        if let Err(e) = apply_template_to_parsed(&mut parsed) {
            eprintln!("error: {e}");
            return 1;
        }
    }

    // A recipe-declared [enrichment.ontology] takes precedence over any
    // --pipeline pin / domain heuristic. Normalize the pipeline id to
    // `custom_atlas` up front so validation, config.json, and logs all reflect
    // what actually runs — the caller need not pass --pipeline custom_atlas.
    if custom_ontology_spec(&parsed.corpus_id).is_some() {
        parsed.pipeline_id =
            corpus_engine::enrichment::pipeline::pipelines::configurable_atlas::PIPELINE_ID
                .to_string();
    }

    // Validate the pipeline id against the registry before doing
    // anything expensive. A typo here would otherwise only surface at
    // `extract` time, after section detection and model resolution.
    {
        let is_custom = parsed.pipeline_id
            == corpus_engine::enrichment::pipeline::pipelines::configurable_atlas::PIPELINE_ID;
        if is_custom {
            // `custom_atlas` is built from the recipe's [enrichment.ontology],
            // not the registry — require that ontology to be present + non-empty
            // here so the failure is legible at init, not deep in the build.
            if custom_ontology_spec(&parsed.corpus_id).is_none() {
                eprintln!(
                    "error: --pipeline custom_atlas needs a recipe with a non-empty \
                     [enrichment.ontology].guidance for corpus `{}` \
                     (looked in <data_dir>/recipes/{}/recipe.toml)",
                    parsed.corpus_id, parsed.corpus_id
                );
                return 2;
            }
        } else {
            let registry = corpus_engine::enrichment::pipeline::PipelineRegistry::builtin();
            if registry.get(&parsed.pipeline_id).is_none() {
                let mut known = registry.pipeline_ids();
                known.sort();
                eprintln!(
                    "error: unknown pipeline: {:?}. Known ids: {:?}",
                    parsed.pipeline_id, known
                );
                return 2;
            }
        }
    }

    // From-corpus branch: skip the source-file / chunker path
    // entirely. Build a chapter manifest directly from the LanceDB
    // index of an already-installed corpus.
    if let Some(ref source_corpus) = parsed.from_corpus {
        return cmd_init_from_corpus(&parsed, source_corpus).await;
    }

    // Read source file.
    if !parsed.source_path.exists() {
        eprintln!(
            "error: source file does not exist: {}",
            parsed.source_path.display()
        );
        return 1;
    }
    let source = match super::source_loader::load_plaintext(&parsed.source_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if source.trim().is_empty() {
        eprintln!(
            "error: source file is empty: {}",
            parsed.source_path.display()
        );
        return 1;
    }

    // Detect sections (dry-run-friendly).
    let regex_pattern = parsed
        .chapter_regex
        .clone()
        .unwrap_or_else(|| ChapterRegexDetector::DEFAULT_PATTERN.to_string());
    let min_body_words = parsed.min_section_body_words;
    let toc_markers = parsed.toc_markers.clone();
    let report = if let Some(ref tm) = toc_markers {
        let detector = corpus_engine::chunkers::sectioned::TocAnchoredDetector::with_markers(
            &tm.start, &tm.end,
        );
        let chunker = SectionedChunker::with_detector(detector);
        chunker.dry_run(&source)
    } else {
        let detector = match ChapterRegexDetector::with_pattern(&regex_pattern) {
            Ok(d) => d.with_min_body_words(min_body_words),
            Err(e) => {
                eprintln!("error: invalid --chapter-regex: {e}");
                return 1;
            }
        };
        let chunker = SectionedChunker::with_detector(detector);
        chunker.dry_run(&source)
    };
    println!("{}", report.format_summary(&source));
    if report.total == 0 {
        match &toc_markers {
            Some(tm) => eprintln!(
                "error: no sections detected — either the start/end markers {start:?}/{end:?} \
                 were not found, or the block between them was empty, or none of its titles \
                 appeared at a line start in the body of {path}.",
                start = tm.start,
                end = tm.end,
                path = parsed.source_path.display(),
            ),
            None => eprintln!(
                "error: regex {regex_pattern:?} matched zero sections in {}.",
                parsed.source_path.display()
            ),
        }
        eprintln!();
        eprintln!("The first non-empty lines of the loaded text are:");
        eprintln!();
        for (i, line) in preview_nonempty_lines(&source, 25).iter().enumerate() {
            eprintln!("  {:>3}. {}", i + 1, line);
        }
        eprintln!();
        if toc_markers.is_some() {
            eprintln!(
                "Verify the Table-of-Contents block is bounded by the configured markers \
                 and that every title inside it appears on its own line in the manuscript body."
            );
        } else {
            eprintln!(
                "Re-run with --chapter-regex '<pattern>' tailored to this corpus \
                 (pattern must use `(?m)` + `^` so `^` anchors per-line), \
                 or with --toc to drive detection from an author-declared Table of Contents."
            );
        }
        return 1;
    }
    if parsed.dry_run {
        return 0;
    }

    // Check whether config.json already exists.
    let config_path = paths::config_path(&parsed.corpus_id);
    if config_path.exists() && !parsed.force {
        eprintln!(
            "error: config already exists at {} — re-run with --force to overwrite",
            config_path.display()
        );
        return 1;
    }

    // Probe daemon + resolve defaults for any un-pinned model ids.
    let base_url = format!("http://localhost:{}", DEFAULT_CLIENT_PORT);
    let daemon_up = probe_daemon(&base_url).await;
    if !daemon_up {
        eprintln!("note: daemon is not responding at {base_url}.");
        eprintln!(
            "      You can still finish init if --chat-model / --embed-model are both pinned,"
        );
        eprintln!("      but `svrn enrich extract` will fail until the daemon is up.");
        if parsed.chat_model.is_none() || parsed.embed_model.is_none() {
            // Non-interactive context (pipeline driver, CI, redirected
            // stdin): never prompt — there's no human to answer, and
            // two concurrent pipeline children sharing a tty would
            // each block forever in `n_tty_read`. Fail cleanly with a
            // non-zero exit so the orchestrator's retry loop can
            // re-claim the unit when the daemon is up; the probe has
            // a 500ms timeout and momentary CPU contention (two
            // parallel parquet loads) is enough to trip it
            // intermittently.
            if !stdin_is_tty() {
                eprintln!(
                    "      stdin is not a terminal — refusing to prompt. Either start \
                     the daemon and retry, or re-invoke with --chat-model + --embed-model."
                );
                return 2;
            }
            if !confirm(
                "  Continue without a running daemon and pick sensible defaults?",
                false,
            ) {
                return 2;
            }
        }
    }

    let (auto_chat, auto_embed) = if daemon_up {
        resolve_default_models(&base_url).await
    } else {
        (None, None)
    };

    if parsed.chat_model.is_none() {
        parsed.chat_model = auto_chat.clone().or(Some("chat".into()));
    }
    if parsed.embed_model.is_none() {
        parsed.embed_model = auto_embed.clone().or(Some("qwen3-embedding-0.6b".into()));
    }

    let chat = parsed.chat_model.clone().expect("chat model resolved");
    let embed = parsed.embed_model.clone().expect("embed model resolved");

    // Build + save the chapter manifest.
    let manifest =
        ChapterManifest::from_detected_sections(&parsed.corpus_id, &source, &report.sections);
    let manifest_path = paths::chapters_manifest_path(&parsed.corpus_id);
    if let Err(e) = manifest.save(&manifest_path) {
        eprintln!(
            "error: saving chapter manifest {}: {e}",
            manifest_path.display()
        );
        return 1;
    }
    println!(
        "  ✓ wrote {} ({} chapters)",
        manifest_path.display(),
        manifest.len()
    );

    // Scaffold the enrichment tree.
    if let Err(e) = scaffold_dirs(&parsed.corpus_id) {
        eprintln!("error: creating enrichment directories: {e}");
        return 1;
    }

    // Save config.
    let cfg = EnrichConfig {
        schema_version: CONFIG_SCHEMA_VERSION,
        corpus_id: parsed.corpus_id.clone(),
        pipeline_id: parsed.pipeline_id.clone(),
        source_path: parsed.source_path.clone(),
        chapter_regex: regex_pattern,
        chat_model: chat.clone(),
        // Per-phase model overrides are an opt-in operator concern;
        // `enrich init` writes None and the operator hand-edits
        // `chat_models` into config.json when they want bulk phases
        // routed to a smaller/faster model than the default chat_model.
        chat_models: None,
        embed_model: embed.clone(),
        base_url,
        min_section_body_words: min_body_words,
        toc_markers,
        max_output_tokens: parsed.max_output_tokens,
        // Operator-driven; default off. Set in config.json post-init
        // when running thinking-off models against a referential
        // pipeline (Phase 1b is schema-free and will bloat without
        // a per-phase cap).
        phase1b_max_output_tokens: None,
        phase_overrides: None,
        ontology: custom_ontology_spec(&parsed.corpus_id),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(e) = cfg.save() {
        eprintln!("error: saving config.json: {e}");
        return 1;
    }
    println!("  ✓ wrote {}", cfg.path().display());
    println!("  ✓ pipeline      = {}", parsed.pipeline_id);
    println!("  ✓ chat_model    = {chat}");
    println!("  ✓ embed_model   = {embed}");
    println!();
    println!(
        "  Next: sovereign enrich extract {} --chapters {}",
        parsed.corpus_id,
        manifest
            .chapter_ids()
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>()
            .join(",")
    );

    0
}

/// Drive enrichment init from an already-indexed multi-document
/// corpus. Reads every chunk from the LanceDB index, groups by
/// `(source_doc_id, section_path)` so each (article, section) pair
/// becomes one chapter, and writes the resulting manifest +
/// config without ever touching a source file.
///
/// Each chapter's `text` is the concatenation of its chunks'
/// content, in stable id order. `chunk_ids` are pre-populated
/// (the corpus is already indexed; no need for a post-ingest
/// stitch pass).
async fn cmd_init_from_corpus(parsed: &ParsedInit, source_corpus: &str) -> i32 {
    // Resolve data dir + indexes dir from setup config (matches
    // every other command's path resolution).
    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        });
    let recipes_dir = data_dir.join("recipes");
    let indexes_dir = data_dir.join("indexes");

    // We never embed during manifest synthesis — wire a no-op
    // `EmbedFn` so `CorpusEngine::new` succeeds without a model.
    let noop_embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(Vec::<f32>::new()) }));
    let engine = CorpusEngine::new(recipes_dir, indexes_dir.clone(), noop_embed);

    let index = match engine.open_index_for_corpus(source_corpus).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!(
                "error: could not open index for source corpus `{source_corpus}`: {e}\n\
                 hint: install it first via `svrn corpus install {source_corpus}`."
            );
            return 1;
        }
    };

    let config_path = paths::config_path(&parsed.corpus_id);
    if config_path.exists() && !parsed.force {
        eprintln!(
            "error: config already exists at {} — re-run with --force to overwrite",
            config_path.display()
        );
        return 1;
    }

    eprintln!("streaming chunk rows from `{source_corpus}` index ...");
    let t_stream = std::time::Instant::now();
    let rows = match index.all_chunks_full().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: streaming chunk rows: {e}");
            return 1;
        }
    };
    let stream_ms = t_stream.elapsed().as_millis() as u64;
    eprintln!("streamed {} chunk rows in {} ms", rows.len(), stream_ms);

    let manifest = match build_manifest_from_corpus_rows(
        &parsed.corpus_id,
        rows,
        parsed.limit_articles,
        parsed.include_articles.clone(),
        // First-run init numbers chapters from `sec_00001`. The
        // incremental `enrich delta-manifest` path passes a higher
        // start ordinal so newly-appended chapters continue past the
        // existing manifest length.
        1,
    ) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: building chapter manifest: {e}");
            return 1;
        }
    };

    if parsed.dry_run {
        println!(
            "dry-run: would write {} chapter(s) to {}",
            manifest.len(),
            paths::chapters_manifest_path(&parsed.corpus_id).display()
        );
        return 0;
    }

    if manifest.is_empty() {
        eprintln!(
            "error: built an empty chapter manifest from `{source_corpus}` — \
             the corpus has no chunks with section metadata. Verify the source \
             corpus has been ingested with a Wikipedia / referential extractor."
        );
        return 1;
    }

    // Probe daemon + resolve defaults for un-pinned model ids,
    // mirroring the source-file path so downstream extract works
    // identically.
    let base_url = format!("http://localhost:{}", DEFAULT_CLIENT_PORT);
    let daemon_up = probe_daemon(&base_url).await;
    let (auto_chat, auto_embed) = if daemon_up {
        resolve_default_models(&base_url).await
    } else {
        (None, None)
    };
    let chat = parsed
        .chat_model
        .clone()
        .or(auto_chat)
        .unwrap_or_else(|| "chat".to_string());
    let embed = parsed
        .embed_model
        .clone()
        .or(auto_embed)
        .unwrap_or_else(|| "qwen3-embedding-0.6b".to_string());

    let manifest_path = paths::chapters_manifest_path(&parsed.corpus_id);
    if let Err(e) = manifest.save(&manifest_path) {
        eprintln!(
            "error: saving chapter manifest {}: {e}",
            manifest_path.display()
        );
        return 1;
    }
    println!(
        "  ✓ wrote {} ({} chapters)",
        manifest_path.display(),
        manifest.len()
    );

    if let Err(e) = scaffold_dirs(&parsed.corpus_id) {
        eprintln!("error: creating enrichment directories: {e}");
        return 1;
    }

    let cfg = EnrichConfig {
        schema_version: CONFIG_SCHEMA_VERSION,
        corpus_id: parsed.corpus_id.clone(),
        pipeline_id: parsed.pipeline_id.clone(),
        // No physical source file in corpus mode; record the
        // sentinel `corpus:<id>` so config.json round-trips
        // truthfully. `enrich extract` never reads this path —
        // chapter inputs come from the manifest's chunk_ids.
        source_path: PathBuf::from(format!("corpus:{}", source_corpus)),
        chapter_regex: String::new(),
        chat_model: chat.clone(),
        chat_models: None,
        embed_model: embed.clone(),
        base_url,
        min_section_body_words: parsed.min_section_body_words,
        toc_markers: None,
        max_output_tokens: parsed.max_output_tokens,
        // Operator-driven; default off. Set in config.json post-init
        // when running thinking-off models against a referential
        // pipeline (Phase 1b is schema-free and will bloat without
        // a per-phase cap).
        phase1b_max_output_tokens: None,
        phase_overrides: None,
        ontology: custom_ontology_spec(&parsed.corpus_id),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(e) = cfg.save() {
        eprintln!("error: saving config.json: {e}");
        return 1;
    }
    println!("  ✓ wrote {}", cfg.path().display());
    println!("  ✓ pipeline      = {}", parsed.pipeline_id);
    println!("  ✓ chat_model    = {chat}");
    println!("  ✓ embed_model   = {embed}");
    println!("  ✓ from_corpus   = {source_corpus}");
    if let Some(n) = parsed.limit_articles {
        println!("  ✓ limit_articles = {n}");
    }
    if let Some(titles) = parsed.include_articles.as_ref() {
        println!("  ✓ include_articles = {} title(s)", titles.len());
    }
    println!();
    println!(
        "  Next: sovereign enrich extract {} --chapters {}",
        parsed.corpus_id,
        manifest
            .chapter_ids()
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>()
            .join(",")
    );

    0
}

/// Group LanceDB chunk rows by `(source_doc_id_or_title, section_path)`
/// and emit one [`ChapterEntry`] per group.
///
/// Falls back to `title` as the article-grouping key when
/// `source_doc_id` is absent — older ingestions may have one but
/// not the other.
///
/// `start_ordinal` is the first chapter ordinal to assign. First-run
/// `enrich init --from-corpus` passes `1` (chapters are
/// `sec_00001 …`). The incremental `enrich delta-manifest` path passes
/// `existing_manifest_len + 1` so newly-detected chapters get
/// `sec_NNNNN` ids that continue past the live manifest without
/// colliding — the `chapter` field + `ordinal` metadata follow the
/// same numbering.
pub(crate) fn build_manifest_from_corpus_rows(
    corpus_id: &str,
    rows: Vec<corpus_engine::EnrichmentChunkRow>,
    limit_articles: Option<usize>,
    include_articles: Option<Vec<String>>,
    start_ordinal: u32,
) -> Result<ChapterManifest, String> {
    use corpus_engine::WikipediaChunkMetadata;

    // Per-(article, section) bucket. BTreeMap so chapter ids come
    // out in deterministic order across runs.
    type ArticleKey = String; // source_doc_id (or title) of the article
    type SectionKey = String; // joined section_path; "" for lead

    #[derive(Default)]
    struct Bucket {
        article_title: String,
        section_name: String,
        section_path_joined: String,
        section_type: Option<String>,
        pov_count: i64,
        citation_needed_count: i64,
        url: Option<String>,
        chunk_ids: Vec<u64>,
        chunks: Vec<(u64, String)>, // (id, content) — sorted by id at finalisation
    }

    let mut buckets: BTreeMap<(ArticleKey, SectionKey), Bucket> = BTreeMap::new();
    let mut article_first_seen: BTreeMap<ArticleKey, usize> = BTreeMap::new();
    let mut counter: usize = 0;

    for row in rows {
        // Article-grouping key: prefer title (per-article in Wikipedia /
        // wiki-shaped corpora — every chunk in an article shares the
        // same title) over source_doc_id (which the Wikipedia extractor
        // sets to the per-section URL, so it varies *within* an article
        // and groups too finely). Fall back to source_doc_id stripped
        // of any URL fragment, then to "<untitled>" as a last resort.
        let article_key = row
            .title
            .clone()
            .or_else(|| {
                row.source_doc_id
                    .as_deref()
                    .map(|s| s.split('#').next().unwrap_or(s).to_string())
            })
            .unwrap_or_else(|| "<untitled>".to_string());
        let article_title = row.title.clone().unwrap_or_else(|| article_key.clone());

        // Section identification — driven by Wikipedia-shaped
        // metadata. Other referential extractors should serialise
        // `WikipediaChunkMetadata`-compatible JSON for now (the
        // section_path / section_name / section_type fields are
        // the load-bearing ones); a generalisation lives behind
        // the `Pipeline` trait if more shapes appear.
        let (section_path_vec, section_name, section_type, pov_count, citation_needed_count) =
            match row
                .metadata_raw
                .as_deref()
                .and_then(|s| serde_json::from_str::<WikipediaChunkMetadata>(s).ok())
            {
                Some(m) => (
                    m.section_path.clone(),
                    m.section_name.clone(),
                    Some(m.section_type.clone()),
                    m.pov_count.unwrap_or(0),
                    m.citation_needed_count.unwrap_or(0),
                ),
                None => (Vec::<String>::new(), String::new(), None, 0, 0),
            };
        let section_path_joined = section_path_vec.join(" › ");

        article_first_seen
            .entry(article_key.clone())
            .or_insert_with(|| {
                let n = counter;
                counter += 1;
                n
            });

        let bucket = buckets
            .entry((article_key.clone(), section_path_joined.clone()))
            .or_insert_with(|| Bucket {
                article_title: article_title.clone(),
                section_name: section_name.clone(),
                section_path_joined: section_path_joined.clone(),
                section_type: section_type.clone(),
                pov_count,
                citation_needed_count,
                url: row.url.clone(),
                chunk_ids: Vec::new(),
                chunks: Vec::new(),
            });
        bucket.chunk_ids.push(row.id);
        bucket.chunks.push((row.id, row.content));
    }

    // Apply the per-article cap and/or include-list. `include_articles`
    // takes precedence — if the operator handed us an explicit title
    // list (typically the top-K from `enrich triage-candidates`), keep
    // exactly those articles regardless of order. Otherwise fall back
    // to the existing first-seen-order limit.
    //
    // `--include-articles` is normalised through
    // `corpus_engine::filters::normalize_title` to be tolerant of the
    // operator's underscore vs space habits in their title file.
    let total_articles = article_first_seen.len();
    let kept_articles: std::collections::HashSet<ArticleKey> = if let Some(want) =
        include_articles.as_ref()
    {
        let want_norm: std::collections::HashSet<String> = want
            .iter()
            .map(|t| corpus_engine::filters::normalize_title(t))
            .collect();
        let mut hits: std::collections::HashSet<ArticleKey> = std::collections::HashSet::new();
        let mut missing_count = 0usize;
        for (key, _) in article_first_seen {
            if want_norm.contains(&corpus_engine::filters::normalize_title(&key)) {
                hits.insert(key);
            }
        }
        // Diagnostic so the operator knows how many of their listed
        // titles actually exist in the source corpus.
        let want_total = want_norm.len();
        if hits.len() < want_total {
            missing_count = want_total - hits.len();
            eprintln!(
                "manifest: --include-articles matched {}/{} titles ({} not present in source corpus)",
                hits.len(),
                want_total,
                missing_count,
            );
        }
        hits
    } else if let Some(n) = limit_articles {
        let mut articles: Vec<(ArticleKey, usize)> = article_first_seen.into_iter().collect();
        articles.sort_by_key(|(_, ord)| *ord);
        articles.into_iter().take(n).map(|(k, _)| k).collect()
    } else {
        article_first_seen.into_keys().collect()
    };
    eprintln!(
        "manifest: {} articles, {} sections — keeping {} articles",
        total_articles,
        buckets.len(),
        kept_articles.len(),
    );

    // Emit one ChapterEntry per surviving (article, section). The
    // loop pre-increments `chapter_ord`, so seed it one below
    // `start_ordinal` (saturating so a stray `0` still yields a valid
    // `sec_00001` rather than underflowing).
    let mut manifest = ChapterManifest::new(corpus_id);
    let mut chapter_ord: u32 = start_ordinal.saturating_sub(1);
    for ((article_key, _section_key), mut bucket) in buckets {
        if !kept_articles.contains(&article_key) {
            continue;
        }
        bucket.chunks.sort_by_key(|(id, _)| *id);
        bucket.chunk_ids.sort_unstable();
        let body: String = bucket
            .chunks
            .iter()
            .map(|(_, c)| c.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let word_count = body.split_whitespace().count() as u64;
        let first_line = body
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .chars()
            .take(160)
            .collect::<String>();
        let title = if bucket.section_name.is_empty() {
            bucket.article_title.clone()
        } else {
            format!("{} — {}", bucket.article_title, bucket.section_name)
        };
        chapter_ord += 1;
        let id = format!("sec_{:05}", chapter_ord);
        let mut metadata: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        metadata.insert("article_title".into(), bucket.article_title);
        metadata.insert("section_path".into(), bucket.section_path_joined);
        if let Some(st) = bucket.section_type {
            metadata.insert("section_type".into(), st);
        }
        if bucket.pov_count > 0 {
            metadata.insert("pov_count".into(), bucket.pov_count.to_string());
        }
        if bucket.citation_needed_count > 0 {
            metadata.insert(
                "citation_needed_count".into(),
                bucket.citation_needed_count.to_string(),
            );
        }
        if let Some(u) = bucket.url {
            metadata.insert("url".into(), u);
        }
        metadata.insert("ordinal".into(), chapter_ord.to_string());

        manifest
            .chapters
            .push(corpus_engine::enrichment::pipeline::ChapterEntry {
                id,
                title,
                part: None,
                chapter: Some(chapter_ord),
                first_line,
                word_count,
                chunk_ids: bucket.chunk_ids,
                characters_present: Vec::new(),
                metadata,
            });
    }

    Ok(manifest)
}

#[derive(Debug)]
struct ParsedInit {
    corpus_id: String,
    source_path: PathBuf,
    chapter_regex: Option<String>,
    pipeline_id: String,
    /// Tracks whether the operator explicitly chose a pipeline. When
    /// `--from-template` is also passed, the template's pipeline_id
    /// only wins if the operator did not override it. Avoids the
    /// "default literary clobbers template's philosophy_atlas" bug.
    pipeline_id_explicit: bool,
    chat_model: Option<String>,
    embed_model: Option<String>,
    min_section_body_words: usize,
    min_section_body_words_explicit: bool,
    toc_markers: Option<TocMarkers>,
    max_output_tokens: u32,
    dry_run: bool,
    force: bool,
    /// Built-in template name (e.g. `"free-will-debate"`).
    /// Mutually exclusive with `template_path` and `source_path`.
    from_template: Option<String>,
    /// Path to a user-supplied template TOML file. Mutually exclusive
    /// with `from_template` and `source_path`.
    template_path: Option<PathBuf>,
    /// Corpus id of an already-indexed multi-document corpus to
    /// drive enrichment from. Mutually exclusive with `source_path`,
    /// `from_template`, `template_path`. When set, the init flow
    /// reads the corpus's LanceDB chunks and synthesises a chapter
    /// manifest where each (article, section) pair is one chapter
    /// — no chunker, no source file needed.
    from_corpus: Option<String>,
    /// When `from_corpus` is set, optional cap on the number of
    /// articles included (sort by source_doc_id, take first N).
    /// `None` means no cap.
    limit_articles: Option<usize>,
    /// When `from_corpus` is set, optional explicit list of article
    /// titles to keep (one per line, with comments and blank lines
    /// ignored). Mutually exclusive with `--limit-articles`.
    /// Match is case + underscore folded by
    /// `corpus_engine::filters::normalize_title`. Designed to consume
    /// the top-K title list from
    /// `svrn enrich triage-candidates --json`.
    include_articles: Option<Vec<String>>,
}

fn parse_args(args: &[String]) -> Result<ParsedInit, String> {
    let mut corpus_id: Option<String> = None;
    let mut source: Option<PathBuf> = None;
    let mut chapter_regex: Option<String> = None;
    let mut pipeline_id = "literary".to_string();
    let mut pipeline_id_explicit = false;
    let mut chat_model: Option<String> = None;
    let mut embed_model: Option<String> = None;
    let mut min_section_body_words: usize = 40;
    let mut min_section_body_words_explicit = false;
    let mut toc: bool = false;
    let mut toc_start: Option<String> = None;
    let mut toc_end: Option<String> = None;
    // Mirror config::default_max_output_tokens — long sections (SEP
    // article introductions, brothers_karamazov chapter heads) regularly
    // exceed 4096 tokens of thinking trace + JSON answer under Q5_K_S
    // quantization, producing parse_drift failures the auto-retry can't
    // recover. 16384 covers the long tail; operators on tight contexts
    // override with --max-output-tokens.
    let mut max_output_tokens: u32 = 16384;
    let mut dry_run = false;
    let mut force = false;
    let mut from_template: Option<String> = None;
    let mut template_path: Option<PathBuf> = None;
    let mut from_corpus: Option<String> = None;
    let mut limit_articles: Option<usize> = None;
    let mut include_articles_path: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--source" => {
                source = Some(PathBuf::from(
                    args.get(i + 1)
                        .ok_or("--source requires a path argument".to_string())?,
                ));
                i += 2;
            }
            "--from-template" => {
                from_template = Some(
                    args.get(i + 1)
                        .ok_or("--from-template requires a name".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--template-path" => {
                template_path = Some(PathBuf::from(
                    args.get(i + 1)
                        .ok_or("--template-path requires a path".to_string())?,
                ));
                i += 2;
            }
            "--from-corpus" => {
                from_corpus = Some(
                    args.get(i + 1)
                        .ok_or("--from-corpus requires a corpus id".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--limit-articles" => {
                let raw = args
                    .get(i + 1)
                    .ok_or("--limit-articles requires a value".to_string())?;
                let n = raw
                    .parse::<usize>()
                    .map_err(|e| format!("--limit-articles must be a positive integer: {e}"))?;
                if n == 0 {
                    return Err("--limit-articles must be > 0".into());
                }
                limit_articles = Some(n);
                i += 2;
            }
            "--include-articles" => {
                include_articles_path = Some(PathBuf::from(
                    args.get(i + 1)
                        .ok_or("--include-articles requires a path argument".to_string())?,
                ));
                i += 2;
            }
            "--chapter-regex" => {
                chapter_regex = Some(
                    args.get(i + 1)
                        .ok_or("--chapter-regex requires a value".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--pipeline" => {
                pipeline_id = args
                    .get(i + 1)
                    .ok_or("--pipeline requires a value".to_string())?
                    .clone();
                pipeline_id_explicit = true;
                i += 2;
            }
            "--chat-model" => {
                chat_model = Some(
                    args.get(i + 1)
                        .ok_or("--chat-model requires a value".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--embed-model" => {
                embed_model = Some(
                    args.get(i + 1)
                        .ok_or("--embed-model requires a value".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--min-section-body-words" => {
                let raw = args
                    .get(i + 1)
                    .ok_or("--min-section-body-words requires a value".to_string())?;
                min_section_body_words = raw.parse::<usize>().map_err(|e| {
                    format!("--min-section-body-words must be a non-negative integer: {e}")
                })?;
                min_section_body_words_explicit = true;
                i += 2;
            }
            "--toc" => {
                toc = true;
                i += 1;
            }
            "--toc-start" => {
                toc_start = Some(
                    args.get(i + 1)
                        .ok_or("--toc-start requires a value".to_string())?
                        .clone(),
                );
                toc = true;
                i += 2;
            }
            "--toc-end" => {
                toc_end = Some(
                    args.get(i + 1)
                        .ok_or("--toc-end requires a value".to_string())?
                        .clone(),
                );
                toc = true;
                i += 2;
            }
            "--max-output-tokens" => {
                let raw = args
                    .get(i + 1)
                    .ok_or("--max-output-tokens requires a value".to_string())?;
                max_output_tokens = raw
                    .parse::<u32>()
                    .map_err(|e| format!("--max-output-tokens must be a positive integer: {e}"))?;
                if max_output_tokens == 0 {
                    return Err("--max-output-tokens must be > 0".into());
                }
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--force" => {
                force = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
            }
        }
    }

    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    if from_template.is_some() && template_path.is_some() {
        return Err("--from-template and --template-path are mutually exclusive".to_string());
    }
    let template_mode = from_template.is_some() || template_path.is_some();
    let corpus_mode = from_corpus.is_some();
    if (template_mode as u8 + corpus_mode as u8 + source.is_some() as u8) > 1 {
        return Err(
            "--source / --from-template / --template-path / --from-corpus are mutually exclusive"
                .to_string(),
        );
    }
    if !template_mode && !corpus_mode && source.is_none() {
        return Err(
            "missing input — supply one of --source <path>, --from-corpus <id>, \
             or --from-template <name>"
                .to_string(),
        );
    }
    if limit_articles.is_some() && !corpus_mode {
        return Err("--limit-articles requires --from-corpus".to_string());
    }
    if include_articles_path.is_some() && !corpus_mode {
        return Err("--include-articles requires --from-corpus".to_string());
    }
    if include_articles_path.is_some() && limit_articles.is_some() {
        return Err("--include-articles and --limit-articles are mutually exclusive".to_string());
    }
    let include_articles: Option<Vec<String>> = if let Some(path) = include_articles_path {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("--include-articles {}: {e}", path.display()))?;
        // The file can be plain titles (one per line) OR the JSON
        // produced by `enrich triage-candidates --json`. Detect by
        // first non-whitespace char.
        let trimmed = raw.trim_start();
        let titles: Vec<String> = if trimmed.starts_with('{') || trimmed.starts_with('[') {
            // JSON: pull `top_in_corpus_by_centrality[*].name`. If
            // that key is missing, fall back to scanning every
            // string field that looks like a title (defensive — a
            // future schema add shouldn't silently break this
            // path).
            let v: serde_json::Value = serde_json::from_str(&raw)
                .map_err(|e| format!("--include-articles JSON parse: {e}"))?;
            let mut out: Vec<String> = Vec::new();
            if let Some(arr) = v
                .get("top_in_corpus_by_centrality")
                .and_then(|x| x.as_array())
            {
                // Two producers, two shapes — accept both:
                //   - `enrich triage-candidates --json` writes objects
                //     `{name, inbound, outbound, centrality}` (richer
                //     shape with degree info)
                //   - `atlas_postinstall::build_triage_candidates`
                //     (the daemon's post-install hook) writes plain
                //     strings. Without tolerating both, the post-
                //     install tier-2 launch fails at `enrich init`
                //     with "JSON had no top_in_corpus_by_centrality
                //     entries" even though the JSON has 33 entries.
                //     Surfaced by conversations-personal install
                //     2026-05-17.
                for entry in arr {
                    if let Some(name) = entry.as_str() {
                        out.push(name.to_string());
                    } else if let Some(name) = entry.get("name").and_then(|n| n.as_str()) {
                        out.push(name.to_string());
                    }
                }
            }
            if out.is_empty() {
                return Err(
                    "--include-articles JSON had no top_in_corpus_by_centrality entries"
                        .to_string(),
                );
            }
            out
        } else {
            raw.lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(|l| l.to_string())
                .collect()
        };
        if titles.is_empty() {
            return Err(
                "--include-articles file contained no titles (after stripping comments + blank lines)"
                    .to_string(),
            );
        }
        Some(titles)
    } else {
        None
    };
    // Source path is `None` in template / corpus modes; cmd_init resolves
    // it to the materialised file (template mode) or to a placeholder
    // (corpus mode — there's no source file at all).
    let source_path = source.unwrap_or_default();
    let toc_markers = if toc {
        Some(TocMarkers {
            start: toc_start.unwrap_or_else(|| {
                corpus_engine::chunkers::sectioned::TocAnchoredDetector::DEFAULT_START.to_string()
            }),
            end: toc_end.unwrap_or_else(|| {
                corpus_engine::chunkers::sectioned::TocAnchoredDetector::DEFAULT_END.to_string()
            }),
        })
    } else if toc_start.is_some() || toc_end.is_some() {
        // Reached only if future parsing admits one without --toc.
        // The current parser forces `toc=true` on either override,
        // so this branch is unreachable; belt-and-braces.
        return Err("--toc-start/--toc-end require --toc".to_string());
    } else {
        None
    };
    Ok(ParsedInit {
        corpus_id,
        source_path: if template_mode || corpus_mode {
            // Template mode: cmd_init fills this in after materialising
            // the fixture. Corpus mode: there's no source path — the
            // chapter manifest is synthesised from LanceDB rows.
            PathBuf::new()
        } else {
            absolutise(source_path)
        },
        chapter_regex,
        pipeline_id,
        pipeline_id_explicit,
        chat_model,
        embed_model,
        min_section_body_words,
        min_section_body_words_explicit,
        toc_markers,
        max_output_tokens,
        dry_run,
        force,
        from_template,
        template_path,
        from_corpus,
        limit_articles,
        include_articles,
    })
}

/// First `n` non-empty lines of `text`, each trimmed and truncated
/// to 100 chars. Used by the 0-sections diagnostic so operators can
/// see what shape their source has without opening the file.
fn preview_nonempty_lines(text: &str, n: usize) -> Vec<String> {
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(n)
        .map(|l| {
            if l.chars().count() > 100 {
                let head: String = l.chars().take(97).collect();
                format!("{head}…")
            } else {
                l.to_string()
            }
        })
        .collect()
}

fn absolutise(p: PathBuf) -> PathBuf {
    if p.is_absolute() {
        return p;
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(&p),
        Err(_) => p,
    }
}

fn scaffold_dirs(corpus_id: &str) -> std::io::Result<()> {
    let root = paths::enrichment_root(corpus_id);
    fs::create_dir_all(&root)?;
    fs::create_dir_all(paths::exemplars_dir(corpus_id))?;
    fs::create_dir_all(paths::cache_dir(corpus_id))?;
    fs::create_dir_all(paths::runs_dir(corpus_id))?;
    Ok(())
}

/// Resolve the template (built-in or path-supplied), materialise its
/// chapters into a plaintext file under the corpus root, and patch
/// the `ParsedInit` so the rest of `cmd_init` proceeds against that
/// file. Template defaults — `pipeline_id`, `chapter_regex`,
/// `min_section_body_words` — apply only when the operator did not
/// override them explicitly on the command line.
fn apply_template_to_parsed(parsed: &mut ParsedInit) -> Result<(), String> {
    let template = if let Some(name) = &parsed.from_template {
        templates::load_builtin(name)?
    } else if let Some(path) = &parsed.template_path {
        templates::load_from_path(path)?
    } else {
        unreachable!("apply_template_to_parsed called without a template source");
    };

    if template.chapters.is_empty() {
        return Err(format!(
            "template '{}' contains zero chapters — nothing to enrich",
            template.meta.name
        ));
    }

    let body = templates::materialise_to_plaintext(&template);

    let corpus_root = paths::enrichment_root(&parsed.corpus_id);
    fs::create_dir_all(&corpus_root)
        .map_err(|e| format!("create corpus dir {}: {e}", corpus_root.display()))?;
    let source_path = corpus_root.join("source.txt");
    fs::write(&source_path, &body)
        .map_err(|e| format!("write template source {}: {e}", source_path.display()))?;
    parsed.source_path = source_path;

    if !parsed.pipeline_id_explicit {
        parsed.pipeline_id = template.meta.pipeline_id.clone();
    }
    if parsed.chapter_regex.is_none() {
        parsed.chapter_regex = Some(templates::CHAPTER_REGEX.to_string());
    }
    if !parsed.min_section_body_words_explicit {
        // Templates ship dense, compact chapters (~200-400 words).
        // The default 40-word floor would still pass them, but a
        // looser floor avoids surprising the operator if they
        // hand-edit a chapter down later. 20 keeps the index/body
        // guard meaningful without rejecting fragments.
        parsed.min_section_body_words = 20;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_minimal_form() {
        let args = vec!["ak".to_string(), "--source".into(), "/tmp/ak.txt".into()];
        let p = parse_args(&args).unwrap();
        assert_eq!(p.corpus_id, "ak");
        assert_eq!(p.source_path, PathBuf::from("/tmp/ak.txt"));
        assert_eq!(p.pipeline_id, "literary");
        assert_eq!(
            p.min_section_body_words, 40,
            "default should match config default"
        );
        assert!(!p.dry_run);
        assert!(!p.force);
    }

    #[test]
    fn parse_args_accepts_min_section_body_words_override() {
        let args: Vec<String> = [
            "ak",
            "--source",
            "/tmp/ak.txt",
            "--min-section-body-words",
            "0",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let p = parse_args(&args).unwrap();
        assert_eq!(p.min_section_body_words, 0);
    }

    #[test]
    fn parse_args_rejects_non_numeric_min_section_body_words() {
        let args: Vec<String> = [
            "ak",
            "--source",
            "/tmp/ak.txt",
            "--min-section-body-words",
            "lots",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let err = parse_args(&args).unwrap_err();
        assert!(
            err.contains("non-negative integer"),
            "unexpected err: {err}"
        );
    }

    #[test]
    fn parse_args_all_flags() {
        let args: Vec<String> = [
            "ak",
            "--source",
            "/abs/ak.txt",
            "--chapter-regex",
            "^BOOK",
            "--pipeline",
            "literary",
            "--chat-model",
            "chat-x",
            "--embed-model",
            "embed-y",
            "--dry-run",
            "--force",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let p = parse_args(&args).unwrap();
        assert_eq!(p.chapter_regex.as_deref(), Some("^BOOK"));
        assert_eq!(p.chat_model.as_deref(), Some("chat-x"));
        assert_eq!(p.embed_model.as_deref(), Some("embed-y"));
        assert!(p.dry_run);
        assert!(p.force);
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(&["ak".into(), "--gibberish".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }

    #[test]
    fn parse_args_requires_corpus_id_and_source() {
        let err = parse_args(&[]).unwrap_err();
        assert!(err.contains("corpus-id"));
        let err = parse_args(&["ak".into()]).unwrap_err();
        assert!(err.contains("source"));
    }

    #[test]
    fn parse_args_rejects_extra_positional() {
        let err =
            parse_args(&["a".into(), "--source".into(), "/x".into(), "b".into()]).unwrap_err();
        assert!(err.contains("unexpected positional"));
    }

    #[test]
    fn parse_args_accepts_from_template_without_source() {
        let args: Vec<String> = ["fwd", "--from-template", "free-will-debate"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let p = parse_args(&args).unwrap();
        assert_eq!(p.corpus_id, "fwd");
        assert_eq!(p.from_template.as_deref(), Some("free-will-debate"));
        assert!(p.template_path.is_none());
        // source_path is filled in by cmd_init after materialisation.
        assert_eq!(p.source_path, std::path::PathBuf::new());
    }

    #[test]
    fn parse_args_rejects_template_with_source() {
        let args: Vec<String> = [
            "x",
            "--from-template",
            "free-will-debate",
            "--source",
            "/tmp/x.txt",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let err = parse_args(&args).unwrap_err();
        assert!(err.contains("mutually exclusive"), "err: {err}");
    }

    #[test]
    fn parse_args_rejects_both_template_flags() {
        let args: Vec<String> = [
            "x",
            "--from-template",
            "free-will-debate",
            "--template-path",
            "/tmp/x.toml",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let err = parse_args(&args).unwrap_err();
        assert!(err.contains("mutually exclusive"), "err: {err}");
    }

    #[test]
    fn parse_args_tracks_explicit_pipeline_with_template() {
        // Default — pipeline_id is "literary" but unmodified by the
        // operator, so apply_template_to_parsed will overwrite with
        // the template's pipeline_id.
        let args: Vec<String> = ["x", "--from-template", "free-will-debate"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let p = parse_args(&args).unwrap();
        assert!(!p.pipeline_id_explicit);

        // With explicit override, the operator's choice wins.
        let args: Vec<String> = [
            "x",
            "--from-template",
            "free-will-debate",
            "--pipeline",
            "literary",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let p = parse_args(&args).unwrap();
        assert!(p.pipeline_id_explicit);
        assert_eq!(p.pipeline_id, "literary");
    }
}
