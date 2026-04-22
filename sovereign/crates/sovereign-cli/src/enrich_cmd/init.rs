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

use super::config::{EnrichConfig, CONFIG_SCHEMA_VERSION};
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
            "sovereign enrich init <corpus-id> --source <path> \\\n  [--chapter-regex <pat>] [--pipeline <id>] [--chat-model <id>] [--embed-model <id>] \\\n  [--dry-run] [--force]",
        ),
        HelpSection::Flags(&[
            ("--source <path>", "Absolute path to the plaintext source file. Required."),
            ("--chapter-regex <pat>", "Override the default section-detector pattern."),
            ("--pipeline <id>", "Pipeline id from the registry. Default: literary."),
            ("--chat-model <id>", "Pin a chat model id. Default: auto-resolve from /v1/models."),
            ("--embed-model <id>", "Pin an embed model id. Default: auto-resolve from /v1/models."),
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

    // Read source file.
    if !parsed.source_path.exists() {
        eprintln!(
            "error: source file does not exist: {}",
            parsed.source_path.display()
        );
        return 1;
    }
    let source = match fs::read_to_string(&parsed.source_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "error: reading source file {}: {e}",
                parsed.source_path.display()
            );
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
    let detector = match ChapterRegexDetector::with_pattern(&regex_pattern) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: invalid --chapter-regex: {e}");
            return 1;
        }
    };
    let chunker = SectionedChunker::with_detector(detector);
    let report = chunker.dry_run(&source);
    println!("{}", report.format_summary(&source));
    if report.total == 0 {
        eprintln!(
            "error: regex matched zero sections. Re-run with --chapter-regex to widen the pattern."
        );
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
        embed_model: embed.clone(),
        base_url,
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
        "  Next: sovereign enrich {} extract --chapters {}",
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
    Ok(ParsedInit {
        corpus_id,
        source_path: absolutise(source_path),
        chapter_regex,
        pipeline_id,
        chat_model,
        embed_model,
        dry_run,
        force,
    })
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
        assert!(!p.dry_run);
        assert!(!p.force);
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
