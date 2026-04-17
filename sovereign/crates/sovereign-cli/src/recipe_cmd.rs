//! `sovereign recipe` subcommand handlers.
//!
//! Provides two commands that don't require a loaded inference model:
//!
//!   sovereign recipe test <path>      [--sample-size N] [--output path]
//!                                     [--no-embed] [--verbose] [--offline]
//!   sovereign recipe validate <path>  [--offline]
//!
//! Both commands use a stub `EmbedFn` that returns zero-vectors. Embedding
//! is always disabled (`--no-embed`) in this code path because loading an
//! inference model requires `--model`, which is handled by the main REPL
//! entry point. Run `sovereign recipe test --embed` with a model to enable
//! the embed + search phase — that workflow is not yet supported here.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::{CorpusEngine, EmbedFn, RecipeRegistry, TestOptions};

// ── Public entry points ─────────────────────────────────────────────────────

/// Run a `recipe` subcommand. Returns the exit code.
pub async fn run_recipe(args: &[String]) -> i32 {
    if args.is_empty() {
        crate::util::help::print(&HELP);
        return 1;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        crate::util::help::print(&HELP);
        return 0;
    }

    match args[0].as_str() {
        "test" => cmd_test(&args[1..]).await,
        "validate" => cmd_validate(&args[1..]).await,
        "list" => cmd_list(&args[1..]).await,
        other => {
            eprintln!("Unknown recipe subcommand: {other}");
            crate::util::help::print(&HELP);
            1
        }
    }
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign recipe",
    summary: "Run corpus ingestion recipes: test, validate, list.",
    sections: &[
        crate::util::help::HelpSection::Usage("sovereign recipe <subcommand> [args]"),
        crate::util::help::HelpSection::Subcommands(&[
            ("list",             "List all corpora available in the registry"),
            ("test <path>",      "Run the full test harness against a recipe file"),
            ("validate <path>",  "Validate recipe fields without downloading data"),
        ]),
        crate::util::help::HelpSection::Notes(
            "`list` takes --offline (skip live registry refresh).\n\
             `test` takes --sample-size N, --output <path>, --offline, --verbose.\n\
             `validate` takes --offline.",
        ),
    ],
};

// ── `recipe test` ───────────────────────────────────────────────────────────

async fn cmd_test(args: &[String]) -> i32 {
    let mut recipe_path: Option<PathBuf> = None;
    let mut sample_size: usize = 100;
    let mut output: Option<PathBuf> = None;
    let mut offline = false;
    let mut verbose = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--sample-size" => {
                i += 1;
                if let Some(n) = args.get(i).and_then(|s| s.parse().ok()) {
                    sample_size = n;
                } else {
                    eprintln!("error: --sample-size requires a number");
                    return 1;
                }
            }
            "--output" => {
                i += 1;
                output = args.get(i).map(PathBuf::from);
            }
            "--no-embed" => { /* default — embed is always false here */ }
            "--offline" => offline = true,
            "--verbose" | "-v" => verbose = true,
            flag if flag.starts_with('-') => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            path => {
                recipe_path = Some(PathBuf::from(path));
            }
        }
        i += 1;
    }

    let recipe_path = match recipe_path {
        Some(p) => p,
        None => {
            eprintln!("error: missing recipe path");
            eprintln!("Usage: sovereign recipe test <path> [--sample-size N] [--output path] [--offline] [--verbose]");
            return 1;
        }
    };

    // Derive the default output path from the recipe's directory.
    let output = output.unwrap_or_else(|| {
        recipe_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("TEST_REPORT.md")
    });

    let engine = build_stub_engine();
    let options = TestOptions {
        sample_size,
        embed: false,
        queries: None,
        output: Some(output.clone()),
        offline,
        verbose,
    };

    eprintln!("Testing recipe: {}", recipe_path.display());
    eprintln!("Sample size:    {sample_size}");
    eprintln!("Output:         {}", output.display());

    match engine.test_recipe(&recipe_path, &options).await {
        Ok(report) => {
            let markdown = report.to_markdown();

            if let Err(e) = std::fs::write(&output, &markdown) {
                eprintln!("error: failed to write report to {}: {e}", output.display());
                return 1;
            }

            eprintln!();
            eprintln!("Report written to: {}", output.display());

            let warnings = report.warnings();
            if !warnings.is_empty() {
                eprintln!();
                eprintln!("Warnings:");
                for w in &warnings {
                    eprintln!("  ⚠  {w}");
                }
            }

            if !report.validation.errors.is_empty() {
                eprintln!();
                eprintln!("Errors:");
                for e in &report.validation.errors {
                    eprintln!("  ✗  {e}");
                }
            }

            eprintln!();
            if report.passed() {
                eprintln!("Result: PASS");
                0
            } else {
                eprintln!("Result: FAIL");
                1
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

// ── `recipe validate` ───────────────────────────────────────────────────────

async fn cmd_validate(args: &[String]) -> i32 {
    let mut recipe_path: Option<PathBuf> = None;
    let mut offline = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--offline" => offline = true,
            flag if flag.starts_with('-') => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            path => {
                recipe_path = Some(PathBuf::from(path));
            }
        }
        i += 1;
    }

    let recipe_path = match recipe_path {
        Some(p) => p,
        None => {
            eprintln!("error: missing recipe path");
            eprintln!("Usage: sovereign recipe validate <path> [--offline]");
            return 1;
        }
    };

    let engine = build_stub_engine();
    let options = TestOptions {
        sample_size: 0, // validation-only
        embed: false,
        offline,
        ..Default::default()
    };

    eprintln!("Validating recipe: {}", recipe_path.display());

    match engine.test_recipe(&recipe_path, &options).await {
        Ok(report) => {
            if report.validation.errors.is_empty() {
                eprintln!("✓ Validation passed");
                if !report.validation.warnings.is_empty() {
                    for w in &report.validation.warnings {
                        eprintln!("  ⚠  {w}");
                    }
                }
                0
            } else {
                eprintln!("✗ Validation failed:");
                for e in &report.validation.errors {
                    eprintln!("  - {e}");
                }
                1
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

// ── `recipe list` ───────────────────────────────────────────────────────────

async fn cmd_list(args: &[String]) -> i32 {
    let mut offline = false;

    for arg in args {
        match arg.as_str() {
            "--offline" => offline = true,
            flag if flag.starts_with('-') => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            _ => {}
        }
    }

    let mut registry = RecipeRegistry::from_bundled(None);

    if !offline {
        registry.refresh().await;
    }

    let entries = registry.list_entries();
    if entries.is_empty() {
        eprintln!("No corpora found in registry.");
        return 0;
    }

    // Header
    println!("{:<16} {:<40} {:<14} {:>8} {:>8}",
        "ID", "Name", "License", "Compressed", "Indexed");
    println!("{}", "-".repeat(90));

    for entry in &entries {
        println!("{:<16} {:<40} {:<14} {:>7.0}GB {:>7.0}GB",
            entry.id,
            if entry.name.len() > 39 { &entry.name[..39] } else { &entry.name },
            if entry.license.len() > 13 { &entry.license[..13] } else { &entry.license },
            entry.size_compressed_gb,
            entry.size_indexed_gb,
        );
    }

    println!();
    println!("{} corpus/corpora in registry", entries.len());
    if offline {
        println!("(offline mode — showing bundled snapshot)");
    }

    0
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Build a `CorpusEngine` with a stub embed function.
///
/// The stub returns zero-vectors; it is never called when `embed = false`.
fn build_stub_engine() -> CorpusEngine {
    let stub_embed: EmbedFn = Arc::new(|_text| {
        Box::pin(async { Ok(vec![0f32; 768]) })
    });

    // Use a temporary location for downloads; the engine's index_dir is
    // unused since we never write a production index.
    let tmp = std::env::temp_dir().join("sovereign-recipe-test");
    CorpusEngine::new(tmp.clone(), tmp, stub_embed)
}

// print_usage replaced by crate::util::help::print(&HELP); see HELP const above.
