//! `sovereign enrich extract-typed <corpus> [--chapters …|--full] [--force]`
//!
//! Workstream B routed-Phase-1 v1: second pass after `enrich extract`.
//! Reads the Phase 0 classification cache, runs the per-type
//! typed-extension prompt for sections whose classification names a
//! supported type, and merges the typed extension back into
//! `cache/questions.json` as `section_extraction.type_extension`.
//!
//! v1 supports ArgumentativeEssay only. Sections classified as
//! anything else are skipped silently (they keep the literary
//! extraction the base pass produced). Subsequent workstreams add
//! Journal / ProjectNote / Reference / Criticism / Poetry / Meeting
//! parsers under `corpus_engine::enrichment::pipeline::typed_schemas`.
//!
//! Why it's a separate subcommand and not folded into `extract`:
//! v1 minimises runner surgery. The runner is already plumbing
//! exemplars, retries, checkpoints, token ledger — folding routed
//! Phase 1 into its loop is a bigger refactor than wins are worth
//! at this stage. v2 will move this into the build orchestration
//! once the per-type parsers (B5–B9) are all working.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::enrichment::pipeline::atlas::TypeExtension;
use corpus_engine::enrichment::pipeline::section_classifier::content_hash;
use corpus_engine::enrichment::pipeline::typed_schemas::argumentative::{
    parse_phase1_argumentative_extension, phase1_argumentative_schema, PHASE1_ARGUMENTATIVE_SYSTEM,
};
use corpus_engine::enrichment::pipeline::typed_schemas::descriptive::{
    parse_phase1_descriptive_extension, phase1_descriptive_schema, PHASE1_DESCRIPTIVE_SYSTEM,
};
use corpus_engine::enrichment::pipeline::typed_schemas::lyric::{
    parse_phase1_lyric_extension, phase1_lyric_schema, PHASE1_LYRIC_SYSTEM,
};
use corpus_engine::enrichment::pipeline::typed_schemas::modulators::{
    apply_modulators, ModulatorContext,
};
use corpus_engine::enrichment::pipeline::typed_schemas::narrative::{
    parse_phase1_narrative_extension, phase1_narrative_schema, PHASE1_NARRATIVE_SYSTEM,
};
use corpus_engine::enrichment::pipeline::typed_schemas::procedural::{
    parse_phase1_procedural_extension, phase1_procedural_schema, PHASE1_PROCEDURAL_SYSTEM,
};
use corpus_engine::enrichment::pipeline::typed_schemas::reflective::{
    parse_phase1_reflective_extension, phase1_reflective_schema, PHASE1_REFLECTIVE_SYSTEM,
};
use corpus_engine::enrichment::pipeline::types::{
    ChapterInput, ChatPrompt, DiscourseMode, Phase1Output, SectionClassificationVector,
    SectionClassificationsFile, DISCOURSE_ROUTING_THRESHOLD,
};
use corpus_engine::enrichment::pipeline::PipelinePhase;
use corpus_engine::enrichment::pipeline::{PhaseCache, RunOutputWriter};
use sovereign_tools::typed_call::{
    TYPED_BUDGET_INITIAL as TYPED_CALL_BUDGET_INITIAL,
    TYPED_BUDGET_RETRY as TYPED_CALL_BUDGET_RETRY,
};

use super::config::EnrichConfig;
use super::corpus_io::rebuild_corpus_state;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich extract-typed",
    summary: "Routed-Phase-1 v1 — run the per-section-type typed extension over an already-extracted corpus.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich extract-typed <corpus-id> [--chapters <id1,id2,...> | --full] [--force]",
        ),
        HelpSection::Flags(&[
            (
                "--chapters <ids>",
                "Comma-separated chapter ids. Default: every section whose classification names a supported type.",
            ),
            (
                "--full",
                "Run on every classified section. Same as the default; matches `enrich extract` flag shape.",
            ),
            (
                "--force",
                "Re-run even when the section's existing type_extension was produced from the same content hash. Use after a typed-prompt revision.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign enrich extract-typed obsidian-vault",
                "Run typed extension over every ArgumentativeEssay-classified section.",
            ),
            (
                "sovereign enrich extract-typed obsidian-vault --chapters sec_00002 --force",
                "Re-run a single section after revising the argumentative prompt.",
            ),
        ]),
        HelpSection::Notes(
            "Prerequisites: `enrich init`, `enrich classify`, and `enrich extract` must all have been \
             run for the corpus. v1 supports ArgumentativeEssay only — other types are skipped \
             until their parser lands. The merged output goes to cache/questions.json — the \
             downstream phases see the typed extension via the `type_extension` field on each \
             section's `SectionExtraction`.",
        ),
    ],
};

struct Args {
    corpus_id: String,
    selection: Selection,
    force: bool,
}

enum Selection {
    Full,
    Subset(Vec<String>),
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    if args.is_empty() {
        return Err("missing required <corpus-id>".into());
    }
    let mut out = Args {
        corpus_id: args[0].clone(),
        selection: Selection::Full,
        force: false,
    };
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--full" => out.selection = Selection::Full,
            "--force" => out.force = true,
            "--chapters" => {
                i += 1;
                let ids = args
                    .get(i)
                    .ok_or_else(|| "--chapters needs a value".to_string())?;
                let parsed: Vec<String> = ids
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if parsed.is_empty() {
                    return Err("--chapters needs at least one id".into());
                }
                out.selection = Selection::Subset(parsed);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(out)
}

fn classifications_path(corpus_id: &str) -> PathBuf {
    paths::cache_dir(corpus_id).join("section_classifications.json")
}

fn load_classifications(path: &PathBuf) -> Result<SectionClassificationsFile, String> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    SectionClassificationsFile::from_json_with_migration(&raw)
        .map_err(|e| format!("parsing {}: {e}", path.display()))
}

/// Tight default `max_output_tokens` budget for the typed-extension
/// chat call. Re-exports the canonical constant from
/// [`sovereign_tools::typed_call`] so this CLI and the
/// `typed_extension` orchestrator stay in lockstep — a bench-driven
/// tuning move on the helper propagates here automatically.
pub const TYPED_BUDGET_INITIAL: u32 = TYPED_CALL_BUDGET_INITIAL as u32;

/// Retry budget when the tight initial call returned a parse-drift
/// response. Re-export from [`sovereign_tools::typed_call`]. A
/// second parse failure at this budget is a real content miss, not
/// a budget issue, and gets surfaced as an error.
pub const TYPED_BUDGET_RETRY: u32 = TYPED_CALL_BUDGET_RETRY as u32;

/// Compose the typed-extension prompt for one section under one
/// discourse mode. The user body is the same shape across every
/// mode (title + frontmatter tags + body); only the system preamble
/// and the response schema differ.
///
/// Carries only the section body (not the exemplar block or the
/// literary system preamble) — the base Phase 1 already extracted
/// the common atoms; this call targets the mode's typed-extension
/// collections only.
///
/// `max_output_tokens` is supplied per-call so the dispatcher can
/// retry with a doubled budget when the initial tight-budget call
/// returns parse drift (see [`TYPED_BUDGET_INITIAL`] / [`TYPED_BUDGET_RETRY`]).
fn compose_typed_prompt(
    chapter: &ChapterInput,
    mode: DiscourseMode,
    max_output_tokens: u32,
) -> ChatPrompt {
    let mut user = String::new();
    user.push_str(&format!(
        "# Section to extract ({} typed extension)\n\n",
        mode.tag()
    ));
    user.push_str(&format!("**Title:** {}\n", chapter.title));
    if let Some(tags) = chapter.metadata.get("tags") {
        if !tags.is_empty() {
            user.push_str(&format!("**Frontmatter tags:** {tags}\n"));
        }
    }
    user.push_str("\n**Body:**\n\n");
    user.push_str(&chapter.text);
    user.push_str("\n\n---\n\n");
    user.push_str(
        "Return a single JSON object with the typed-extension collections \
         per the schema in the system message. Omit any collection you cannot \
         populate with real content. No prose, no <think> block, no code-fence \
         markers.",
    );
    let (system, schema, schema_name, phase_id) = match mode {
        DiscourseMode::Argumentative => (
            PHASE1_ARGUMENTATIVE_SYSTEM,
            phase1_argumentative_schema(),
            "phase1_argumentative_extension",
            "phase1_argumentative",
        ),
        DiscourseMode::Narrative => (
            PHASE1_NARRATIVE_SYSTEM,
            phase1_narrative_schema(),
            "phase1_narrative_extension",
            "phase1_narrative",
        ),
        DiscourseMode::Descriptive => (
            PHASE1_DESCRIPTIVE_SYSTEM,
            phase1_descriptive_schema(),
            "phase1_descriptive_extension",
            "phase1_descriptive",
        ),
        DiscourseMode::Reflective => (
            PHASE1_REFLECTIVE_SYSTEM,
            phase1_reflective_schema(),
            "phase1_reflective_extension",
            "phase1_reflective",
        ),
        DiscourseMode::Procedural => (
            PHASE1_PROCEDURAL_SYSTEM,
            phase1_procedural_schema(),
            "phase1_procedural_extension",
            "phase1_procedural",
        ),
        DiscourseMode::Lyric => (
            PHASE1_LYRIC_SYSTEM,
            phase1_lyric_schema(),
            "phase1_lyric_extension",
            "phase1_lyric",
        ),
    };
    ChatPrompt::new(system, user)
        .with_response_schema(schema_name, schema)
        .with_phase_id(phase_id)
        .with_max_output_tokens(max_output_tokens)
}

/// Parse the model response under the named discourse mode. Returns
/// the matching `TypeExtension` variant so the caller can push it
/// directly into `SectionExtraction.type_extensions`.
fn parse_typed_response(
    response: &str,
    mode: DiscourseMode,
) -> Result<TypeExtension, TypedDispatchError> {
    let parsed = match mode {
        DiscourseMode::Argumentative => parse_phase1_argumentative_extension(response),
        DiscourseMode::Narrative => parse_phase1_narrative_extension(response),
        DiscourseMode::Descriptive => parse_phase1_descriptive_extension(response),
        DiscourseMode::Reflective => parse_phase1_reflective_extension(response),
        DiscourseMode::Procedural => parse_phase1_procedural_extension(response),
        DiscourseMode::Lyric => parse_phase1_lyric_extension(response),
    };
    parsed.map_err(|e| TypedDispatchError {
        mode,
        message: format!("{e}"),
    })
}

/// Per-dispatch failure record. Combines a discourse-mode tag with the
/// upstream parser's error so the CLI can render `argumentative: ParseError(…)`
/// in a fan-out summary without losing the mode that failed.
pub struct TypedDispatchError {
    pub mode: DiscourseMode,
    pub message: String,
}

impl std::fmt::Display for TypedDispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.mode.tag(), self.message)
    }
}

pub async fn cmd_extract_typed(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };

    let cfg = match EnrichConfig::require(&parsed.corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    if !probe_daemon(&cfg.base_url).await {
        eprintln!(
            "error: daemon is not responding at {} — start it with `sovereign daemon start`",
            cfg.base_url
        );
        return 2;
    }

    // Load classifications (required input).
    let cls_path = classifications_path(&cfg.corpus_id);
    let classifications = match load_classifications(&cls_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!(
                "hint: run `sovereign enrich classify {}` first to produce the classification cache.",
                cfg.corpus_id
            );
            return 1;
        }
    };
    // v2: route on the full classification vector. Each section's
    // active modes = primary + every secondary whose weight is at
    // least `DISCOURSE_ROUTING_THRESHOLD`. The dispatcher fans out
    // one chat call per active mode and merges the resulting
    // `TypeExtension`s into `SectionExtraction.type_extensions`.
    // Epistemic + Temporal axes apply via `apply_modulators` post-
    // extraction.
    let by_section_vector: HashMap<String, SectionClassificationVector> = classifications
        .classifications
        .iter()
        .map(|c| (c.section_id.clone(), c.clone()))
        .collect();

    // Load chapters → ChapterInputs.
    let (inputs, _manifest) = match rebuild_corpus_state(&cfg) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("error: rebuilding corpus state: {e}");
            return 1;
        }
    };
    let by_id: HashMap<String, ChapterInput> = inputs
        .into_iter()
        .map(|c| (c.chapter_id.clone(), c))
        .collect();

    // Load the existing Phase 1 cache. extract-typed mutates this in
    // place — every section we touch gets a `type_extension` attached
    // to its `section_extraction`.
    let cache = PhaseCache::new(paths::cache_dir(&cfg.corpus_id));
    let mut phase1: Phase1Output = match cache.read::<Phase1Output>(PipelinePhase::Questions) {
        Ok(Some(o)) => o,
        Ok(None) => {
            eprintln!(
                "error: cache/questions.json missing — run `sovereign enrich extract {}` first.",
                cfg.corpus_id
            );
            return 1;
        }
        Err(e) => {
            eprintln!("error: reading cache/questions.json: {e}");
            return 1;
        }
    };

    // Pick sections to dispatch. v2 routes every classified section
    // (each one has at least its primary mode); per-mode fan-out
    // happens inside the loop. Selection::Subset still restricts to
    // the operator's chapter ids.
    let dispatch_targets: Vec<&ChapterInput> = match &parsed.selection {
        Selection::Full => phase1
            .questions_by_chapter
            .iter()
            .filter_map(|q| {
                if by_section_vector.contains_key(&q.chapter_id) {
                    by_id.get(&q.chapter_id)
                } else {
                    None
                }
            })
            .collect(),
        Selection::Subset(ids) => {
            let mut targets = Vec::new();
            for id in ids {
                if !by_section_vector.contains_key(id) {
                    eprintln!(
                        "  · {id}: not in classifications cache — re-run `enrich classify {id}` to add it."
                    );
                    continue;
                }
                if let Some(ch) = by_id.get(id) {
                    targets.push(ch);
                }
            }
            targets
        }
    };

    if dispatch_targets.is_empty() {
        println!("  · no classified sections to extract — nothing to do.");
        return 0;
    }

    // Daemon chat closure.
    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (_embed, chat, _chat_with_tokens) = client.into_closures_with_tokens();
    let chat: Arc<_> = chat;

    let mut classified = 0usize;
    let mut cache_hits = 0usize;
    let mut errors = 0usize;
    let total = dispatch_targets.len();

    // Build a quick lookup by chapter_id so we can mutate the matching
    // entry in `phase1.questions_by_chapter`.
    let mut by_chapter_idx: HashMap<String, usize> = HashMap::new();
    for (i, q) in phase1.questions_by_chapter.iter().enumerate() {
        by_chapter_idx.insert(q.chapter_id.clone(), i);
    }

    for (idx, chapter) in dispatch_targets.iter().enumerate() {
        let h = content_hash(&chapter.text);
        let i = by_chapter_idx
            .get(&chapter.chapter_id)
            .copied()
            .unwrap_or(usize::MAX);

        // Cache hit check: if the chapter already has any typed
        // extensions and `--force` is unset, skip. The plural
        // `type_extensions` slot is canonical for v2; we still
        // honour the legacy singular slot so a v1-extracted cache
        // doesn't get re-dispatched on every run.
        let already_has = if i != usize::MAX {
            phase1.questions_by_chapter[i]
                .section_extraction
                .as_ref()
                .map(|se| !se.type_extensions.is_empty() || se.type_extension.is_some())
                .unwrap_or(false)
        } else {
            false
        };
        if already_has && !parsed.force {
            cache_hits += 1;
            println!(
                "    [{}/{}] {} (type_extensions already populated, --force to re-run)",
                idx + 1,
                total,
                chapter.chapter_id
            );
            continue;
        }

        // Resolve active discourse modes for this section.
        let vector = match by_section_vector.get(&chapter.chapter_id) {
            Some(v) => v,
            None => {
                eprintln!(
                    "  · {} missing from classification cache — skipping",
                    chapter.chapter_id
                );
                continue;
            }
        };
        let active_modes = vector
            .discourse_mode
            .active_modes(DISCOURSE_ROUTING_THRESHOLD);

        print!(
            "    [{}/{}] {} → {} mode(s) … ",
            idx + 1,
            total,
            chapter.chapter_id,
            active_modes.len()
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());

        // Fan out one chat call per active mode. Collect the
        // resulting `TypeExtension`s; a failure in any one mode
        // logs but does not abort the others.
        let mut extensions_for_section: Vec<TypeExtension> = Vec::new();
        let mut per_mode_summaries: Vec<String> = Vec::new();
        for (mode, _weight) in &active_modes {
            // Tight-budget first attempt. Most sections fit cleanly
            // under `TYPED_BUDGET_INITIAL`; the small minority that
            // overrun and parse-drift get one retry at
            // `TYPED_BUDGET_RETRY`. Two attempts at most — a second
            // parse failure is a real content miss, not a budget
            // issue, and gets surfaced as an error.
            let budgets = [TYPED_BUDGET_INITIAL, TYPED_BUDGET_RETRY];
            let mut last_parse_err: Option<TypedDispatchError> = None;
            let mut succeeded: Option<TypeExtension> = None;
            let mut chat_error: Option<String> = None;
            let mut attempts_used = 0usize;
            for (attempt, budget) in budgets.iter().enumerate() {
                attempts_used = attempt + 1;
                let prompt = compose_typed_prompt(chapter, *mode, *budget);
                let response = match chat(&prompt).await {
                    Ok(s) => s,
                    Err(e) => {
                        chat_error = Some(format!("{e}"));
                        break;
                    }
                };
                match parse_typed_response(&response, *mode) {
                    Ok(ext) => {
                        succeeded = Some(ext);
                        break;
                    }
                    Err(e) => {
                        last_parse_err = Some(e);
                        // Loop continues to retry budget on next iter.
                    }
                }
            }

            if let Some(msg) = chat_error {
                per_mode_summaries.push(format!("{}=CHAT_ERR", mode.tag()));
                eprintln!("\n      {}: CHAT ERROR: {msg}", mode.tag());
                errors += 1;
                continue;
            }
            match succeeded {
                Some(ext) => {
                    let n = ext.atom_count();
                    let label = if attempts_used == 1 {
                        format!("{}={}", mode.tag(), n)
                    } else {
                        format!("{}={}↑", mode.tag(), n)
                    };
                    per_mode_summaries.push(label);
                    extensions_for_section.push(ext);
                }
                None => {
                    let e = last_parse_err.expect(
                        "fan-out invariant: when both retries fail without a chat \
                         error, the second attempt's parse error must be populated",
                    );
                    per_mode_summaries.push(format!("{}=PARSE_ERR", mode.tag()));
                    eprintln!("\n      {}", e);
                    errors += 1;
                }
            }
        }

        // Apply Epistemic + Temporal modulators. v1 is a no-op on
        // the atom shapes but the call is wired so future modulator
        // logic lands without dispatcher surgery.
        let ctx = ModulatorContext::new(vector.epistemic_posture, vector.temporal_frame);
        let extensions_for_section = apply_modulators(extensions_for_section, ctx);

        if extensions_for_section.is_empty() {
            println!("no extensions");
            continue;
        }
        let total_atoms: usize = extensions_for_section.iter().map(|e| e.atom_count()).sum();
        println!(
            "{total_atoms} atom(s) — [{}]",
            per_mode_summaries.join(", ")
        );

        // Attach to the chapter's section_extraction.
        if i == usize::MAX {
            eprintln!(
                "  · WARNING: chapter {} extracted but is not in phase1.questions_by_chapter — \
                 typed extensions dropped. Re-run `enrich extract` to backfill.",
                chapter.chapter_id
            );
            continue;
        }
        let entry = &mut phase1.questions_by_chapter[i];
        if entry.section_extraction.is_none() {
            entry.section_extraction = Some(
                corpus_engine::enrichment::pipeline::atlas::SectionExtraction {
                    section_id: chapter.chapter_id.clone(),
                    ..Default::default()
                },
            );
        }
        let se = entry.section_extraction.as_mut().unwrap();
        // v2 dispatcher writes plural; clear the legacy singular
        // slot so the cache shape is unambiguous post-rewrite.
        se.type_extension = None;
        se.type_extensions = extensions_for_section;
        classified += 1;
        let _ = h; // content_hash unused this iteration — reserved for v2 cache check.
    }

    // Persist updated cache.
    phase1.written_at = chrono::Utc::now().to_rfc3339();
    if let Err(e) = cache.write(PipelinePhase::Questions, &phase1) {
        eprintln!("error: writing cache/questions.json: {e}");
        return 1;
    }

    // Also write a run-file for traceability — matches the convention
    // `enrich extract` uses.
    let runs = RunOutputWriter::new(paths::runs_dir(&cfg.corpus_id));
    if let Err(e) = runs.write(PipelinePhase::Questions, "extract-typed", &phase1) {
        eprintln!("  · warning: writing run-file failed: {e}");
    }

    println!();
    println!(
        "  ✓ updated cache/questions.json — classified={classified} cache_hits={cache_hits} errors={errors} total={total}"
    );
    if errors > 0 {
        return 1;
    }
    0
}
