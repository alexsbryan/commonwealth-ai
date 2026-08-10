// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich classify <corpus> [--chapters …|--full] [--force]` —
//! Phase 0: per-section type classification.
//!
//! Reads each section in the corpus's chapters.json, dispatches one
//! chat call per section to the Phase 0 classifier, and writes the
//! resulting `SectionClassification` records to
//! `~/.svrnmesh/enrichment/<corpus>/cache/section_classifications.json`.
//!
//! Idempotent. A section whose `content_hash` matches the cached
//! entry is skipped without a chat call. `--force` re-classifies
//! everything regardless of cache state — useful when the
//! classifier prompt changes.
//!
//! Cache shape lives on `SectionClassificationsFile` (see
//! `corpus_engine::enrichment::pipeline::types`). The CLI is the
//! only producer; downstream Phase 1 routed extraction consumes it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::enrichment::pipeline::section_classifier::{
    classify_section_axes, content_hash,
};
use corpus_engine::enrichment::pipeline::types::{
    ChapterInput, SectionClassificationVector, SectionClassificationsFile,
};

use super::config::EnrichConfig;
use super::corpus_io::rebuild_corpus_state;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn enrich classify",
    summary: "Phase 0 — per-section type classification. Writes cache/section_classifications.json.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich classify <corpus-id> [--chapters <id1,id2,...> | --full] [--force]",
        ),
        HelpSection::Flags(&[
            (
                "--chapters <ids>",
                "Comma-separated chapter ids. Other sections keep their existing cache entries. Default: all sections (effectively --full).",
            ),
            (
                "--full",
                "Explicit full-corpus run. Same as the default; included so the CLI shape matches `enrich extract`.",
            ),
            (
                "--force",
                "Re-classify even when the cached entry's content_hash matches. Use after a classifier-prompt revision.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "svrn enrich classify obsidian-vault",
                "Classify every section that isn't already cached at its current content_hash. Fast iteration loop once the classifier prompt is settled.",
            ),
            (
                "svrn enrich classify obsidian-vault --force",
                "Re-classify everything from scratch after a classifier-prompt update.",
            ),
            (
                "svrn enrich classify obsidian-vault --chapters sec_00002,sec_00007",
                "Spot-classify two sections — useful while tuning the prompt against specific failure cases.",
            ),
        ]),
        HelpSection::Notes(
            "Phase 0 is the prelude to routed Phase 1. The classification carries a content_hash so a re-classify on an unchanged section is a no-op chat-call-wise. Re-run after any source edit; the routed Phase 1 will fall back to a single literary schema when no classification exists.",
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

/// Where the per-corpus classifications cache lives. Sibling of
/// `cache/questions.json`.
fn classifications_path(corpus_id: &str) -> PathBuf {
    paths::cache_dir(corpus_id).join("section_classifications.json")
}

fn load_cache(path: &PathBuf, pipeline_id: &str) -> SectionClassificationsFile {
    match std::fs::read_to_string(path) {
        Ok(raw) => match SectionClassificationsFile::from_json_with_migration(&raw) {
            Ok(c) if c.pipeline_id == pipeline_id => c,
            Ok(other) => {
                // Pipeline mismatch — discard. A classification cache
                // produced by `literary_atlas` should not feed an
                // `obsidian_atlas` run; routing rules differ.
                eprintln!(
                    "  · cache built by '{}' pipeline; current corpus is '{}'. Starting fresh.",
                    other.pipeline_id, pipeline_id
                );
                fresh_cache(pipeline_id)
            }
            Err(e) => {
                eprintln!(
                    "  · cache at {} is unreadable ({e}); starting fresh.",
                    path.display()
                );
                fresh_cache(pipeline_id)
            }
        },
        Err(_) => fresh_cache(pipeline_id),
    }
}

fn fresh_cache(pipeline_id: &str) -> SectionClassificationsFile {
    SectionClassificationsFile {
        schema_version: SectionClassificationsFile::SCHEMA_VERSION,
        pipeline_id: pipeline_id.to_string(),
        classifications: Vec::new(),
        written_at: chrono::Utc::now(),
    }
}

fn save_cache(path: &PathBuf, cache: &SectionClassificationsFile) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(cache).map_err(std::io::Error::other)?;
    std::fs::write(path, body)
}

fn render_summary(out: &SectionClassificationsFile) {
    // v2-shaped summary: primary discourse mode tag distribution +
    // primary-weight bands (proxy for the legacy `confidence` until
    // the classifier prompt rewrite lands and emits the vector
    // directly).
    let mut by_mode: HashMap<&'static str, usize> = HashMap::new();
    let mut by_weight_band = [0usize; 4]; // <0.5, 0.5-0.7, 0.7-0.9, >=0.9
    for c in &out.classifications {
        *by_mode.entry(c.discourse_mode.primary.tag()).or_insert(0) += 1;
        let w = c.discourse_mode.primary_weight;
        let b = if w < 0.5 {
            0
        } else if w < 0.7 {
            1
        } else if w < 0.9 {
            2
        } else {
            3
        };
        by_weight_band[b] += 1;
    }
    println!();
    println!("  Classified {} section(s):", out.classifications.len());
    let mut tier: Vec<_> = by_mode.into_iter().collect();
    tier.sort_by(|a, b| b.1.cmp(&a.1));
    for (tag, n) in &tier {
        println!("    {:<24} {}", tag, n);
    }
    println!();
    println!("  Primary-weight bands:");
    let bands = ["< 0.5", "0.5–0.7", "0.7–0.9", "≥ 0.9"];
    for (label, n) in bands.iter().zip(by_weight_band.iter()) {
        println!("    {:<8} {}", label, n);
    }
}

pub async fn cmd_classify(args: &[String]) -> i32 {
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
            "error: daemon is not responding at {} — start it with `svrn daemon start`",
            cfg.base_url
        );
        return 2;
    }

    // Rebuild ChapterInputs from the corpus index. Mirrors `extract`'s
    // initialisation path so the classifier sees the same section
    // bodies Phase 1 will.
    let (inputs, _manifest) = match rebuild_corpus_state(&cfg) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("error: rebuilding corpus state: {e}");
            return 1;
        }
    };

    // Filter to the requested selection. Validate that every
    // explicitly-requested id exists in the manifest before
    // dispatching a single chat call — saves the user a 10-min run
    // with a typo in the last id.
    let by_id: HashMap<String, ChapterInput> = inputs
        .into_iter()
        .map(|c| (c.chapter_id.clone(), c))
        .collect();
    let targeted: Vec<ChapterInput> = match &parsed.selection {
        Selection::Full => {
            let mut v: Vec<ChapterInput> = by_id.values().cloned().collect();
            v.sort_by(|a, b| a.chapter_id.cmp(&b.chapter_id));
            v
        }
        Selection::Subset(ids) => {
            let mut missing = Vec::new();
            let mut found = Vec::new();
            for id in ids {
                match by_id.get(id) {
                    Some(c) => found.push(c.clone()),
                    None => missing.push(id.clone()),
                }
            }
            if !missing.is_empty() {
                eprintln!(
                    "error: chapter id(s) not in manifest: {}",
                    missing.join(", ")
                );
                return 1;
            }
            found
        }
    };

    let cache_path = classifications_path(&cfg.corpus_id);
    let mut cache = load_cache(&cache_path, &cfg.pipeline_id);
    // Build a lookup of current entries by section_id so we can skip
    // unchanged ones and overwrite changed ones in place. Records read
    // from disk are already v2 `SectionClassificationVector`s (v1 files
    // were projected via the migration helper).
    let mut by_section: HashMap<String, SectionClassificationVector> = cache
        .classifications
        .drain(..)
        .map(|c| (c.section_id.clone(), c))
        .collect();

    // Daemon client → chat closure.
    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (_embed, chat, _chat_with_tokens) = client.into_closures_with_tokens();
    let chat: Arc<_> = chat;

    let total = targeted.len();
    let mut classified = 0usize;
    let mut cache_hits = 0usize;
    let mut errors = 0usize;

    for (idx, chapter) in targeted.iter().enumerate() {
        let want_chat = parsed.force
            || by_section
                .get(&chapter.chapter_id)
                .map(|c| c.content_hash != content_hash(&chapter.text))
                .unwrap_or(true);
        if !want_chat {
            cache_hits += 1;
            println!(
                "    [{}/{}] {} (cache hit, skipping)",
                idx + 1,
                total,
                chapter.chapter_id
            );
            continue;
        }
        print!("    [{}/{}] {} … ", idx + 1, total, chapter.chapter_id);
        let _ = std::io::Write::flush(&mut std::io::stdout());
        match classify_section_axes(chapter, Arc::clone(&chat)).await {
            Ok(v) => {
                // Render the primary discourse mode + weight band plus
                // any active secondaries.
                let sec_str = if v.discourse_mode.secondaries.is_empty() {
                    String::new()
                } else {
                    let parts: Vec<String> = v
                        .discourse_mode
                        .secondaries
                        .iter()
                        .map(|(m, w)| format!("{}@{:.2}", m.tag(), w))
                        .collect();
                    format!(" / secondaries=[{}]", parts.join(", "))
                };
                println!(
                    "{}@{:.2}{} / {} / {}",
                    v.discourse_mode.primary.tag(),
                    v.discourse_mode.primary_weight,
                    sec_str,
                    v.epistemic_posture.tag(),
                    v.temporal_frame.tag()
                );
                by_section.insert(v.section_id.clone(), v);
                classified += 1;
            }
            Err(e) => {
                println!("ERROR: {e}");
                errors += 1;
            }
        }
    }

    // Reassemble + persist.
    let mut classifications: Vec<SectionClassificationVector> = by_section.into_values().collect();
    classifications.sort_by(|a, b| a.section_id.cmp(&b.section_id));
    let out = SectionClassificationsFile {
        schema_version: SectionClassificationsFile::SCHEMA_VERSION,
        pipeline_id: cfg.pipeline_id.clone(),
        classifications,
        written_at: chrono::Utc::now(),
    };
    if let Err(e) = save_cache(&cache_path, &out) {
        eprintln!("error: writing cache {}: {e}", cache_path.display());
        return 1;
    }
    render_summary(&out);
    println!();
    println!("  ✓ wrote {}", cache_path.display());
    println!(
        "    classified={classified}  cache_hits={cache_hits}  errors={errors}  total={total}"
    );
    if errors > 0 {
        return 1;
    }
    0
}
