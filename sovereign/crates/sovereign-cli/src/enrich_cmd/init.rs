//! `sovereign enrich init <corpus> --source <path>` — first-run setup.
//!
//! Responsibilities:
//!   1. Probe the daemon + resolve default chat/embed models.
//!   2. Read the source file; run the `SectionedChunker` (respecting
//!      `--chapter-regex`).
//!   3. On `--dry-run`, print the `SectionReport` and exit 0.
//!   4. Otherwise: write `chapters.json` + `config.json` + scaffold
//!      the `exemplars/`, `cache/`, `runs/` directories.

use std::fs;
use std::path::PathBuf;

use corpus_engine::chunkers::sectioned::{ChapterRegexDetector, SectionedChunker};
use corpus_engine::enrichment::pipeline::ChapterManifest;

use super::config::{EnrichConfig, TocMarkers, CONFIG_SCHEMA_VERSION};
use super::inference_client::{probe_daemon, resolve_default_models};
use super::paths;
use crate::util::help::{self, Help, HelpSection};
use crate::util::prompts::confirm;
use crate::util::urls::DEFAULT_CLIENT_PORT;

const HELP: Help = Help {
    command: "sovereign enrich init",
    summary: "Scaffold an enrichment-admin tree for a corpus: chapters.json + config.json + dirs.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich init <corpus-id> --source <path> \\\n  [--chapter-regex <pat> | --toc [--toc-start <m>] [--toc-end <m>]] \\\n  [--min-section-body-words <n>] [--pipeline <id>] [--chat-model <id>] [--embed-model <id>] \\\n  [--dry-run] [--force]",
        ),
        HelpSection::Flags(&[
            ("--source <path>", "Absolute path to the plaintext source file. Required."),
            ("--chapter-regex <pat>", "Override the default section-detector pattern."),
            ("--pipeline <id>", "Pipeline id from the registry. Default: literary."),
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
                "sovereign enrich init anna-karenina --source ~/books/ak.txt",
                "First-run setup with auto-resolved models and default chapter regex.",
            ),
            (
                "sovereign enrich init ak --source ak.txt --chapter-regex '^BOOK [A-Z]+' --dry-run",
                "Preview section detection with a custom regex; do not write state.",
            ),
            (
                "sovereign enrich init bk --source bk.txt --pipeline literary_atlas",
                "Use the atlas-schema Phase 1 extractor (full atom graph) instead of the legacy questions-only pipeline.",
            ),
            (
                "sovereign enrich init compatibilism --source compatibilism.md --pipeline philosophy_atlas",
                "Philosophy-tuned atlas pipeline (same schema, argumentative-prose prompts).",
            ),
        ]),
        HelpSection::Notes(
            "Writes to ~/.sovereign/enrichment/<corpus>/ and ~/.sovereign/indexes/<corpus>/. \
             config.json pins the chapter regex + model ids so every later subcommand operates \
             against a reproducible shape. Re-run with --force to overwrite.",
        ),
    ],
};

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

    // Validate the pipeline id against the registry before doing
    // anything expensive. A typo here would otherwise only surface at
    // `extract` time, after section detection and model resolution.
    {
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
        let detector =
            corpus_engine::chunkers::sectioned::TocAnchoredDetector::with_markers(&tm.start, &tm.end);
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
        eprintln!(
            "The first non-empty lines of the loaded text are:"
        );
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
        eprintln!(
            "      but `sovereign enrich extract` will fail until the daemon is up."
        );
        if parsed.chat_model.is_none() || parsed.embed_model.is_none() {
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
        eprintln!("error: saving chapter manifest {}: {e}", manifest_path.display());
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

#[derive(Debug)]
struct ParsedInit {
    corpus_id: String,
    source_path: PathBuf,
    chapter_regex: Option<String>,
    pipeline_id: String,
    chat_model: Option<String>,
    embed_model: Option<String>,
    min_section_body_words: usize,
    toc_markers: Option<TocMarkers>,
    max_output_tokens: u32,
    dry_run: bool,
    force: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedInit, String> {
    let mut corpus_id: Option<String> = None;
    let mut source: Option<PathBuf> = None;
    let mut chapter_regex: Option<String> = None;
    let mut pipeline_id = "literary".to_string();
    let mut chat_model: Option<String> = None;
    let mut embed_model: Option<String> = None;
    let mut min_section_body_words: usize = 40;
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
                max_output_tokens = raw.parse::<u32>().map_err(|e| {
                    format!("--max-output-tokens must be a positive integer: {e}")
                })?;
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
    let source_path = source.ok_or_else(|| "missing --source <path>".to_string())?;
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
        source_path: absolutise(source_path),
        chapter_regex,
        pipeline_id,
        chat_model,
        embed_model,
        min_section_body_words,
        toc_markers,
        max_output_tokens,
        dry_run,
        force,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_minimal_form() {
        let args = vec![
            "ak".to_string(),
            "--source".into(),
            "/tmp/ak.txt".into(),
        ];
        let p = parse_args(&args).unwrap();
        assert_eq!(p.corpus_id, "ak");
        assert_eq!(p.source_path, PathBuf::from("/tmp/ak.txt"));
        assert_eq!(p.pipeline_id, "literary");
        assert_eq!(p.min_section_body_words, 40, "default should match config default");
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
        assert!(err.contains("non-negative integer"), "unexpected err: {err}");
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
}
