// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn recipe new` and `svrn recipe migrate` — the authoring verbs.
//!
//! Split out of [`super`] (ARCH §3.1: `recipe_cmd.rs` was 1155 lines and
//! inside the arch-gate's 800-1200 approach band). Both verbs scaffold or
//! rewrite recipe TOML from `corpus_engine::recipe_templates` and
//! `Recipe::migrate_ontology_version`; neither loads an inference model.

use super::*;

// ── `recipe new` ────────────────────────────────────────────────────────────

/// `svrn recipe new --ontology <name> [--id <id>] [--out <path>]`. Scaffolds a
/// complete recipe from a built-in ontology-v1 template
/// (`corpus_engine::recipe_templates`). The template text is the payload, so
/// it goes to stdout unless `--out` names a file; `--out` never overwrites.
pub(super) fn cmd_new(args: &[String]) -> i32 {
    const USAGE: &str =
        "Usage: svrn recipe new --ontology <name> [--id <corpus-id>] [--out <path>]";
    if sovereign_cli_shared::help::wants_help(args) {
        println!("{USAGE}");
        println!(
            "  templates: {}",
            corpus_engine::recipe_templates::list_builtin_names().join(", ")
        );
        return 0;
    }
    let mut ontology: Option<String> = None;
    let mut id: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--ontology" => {
                i += 1;
                ontology = args.get(i).cloned();
            }
            "--id" => {
                i += 1;
                id = args.get(i).cloned();
            }
            "--out" => {
                i += 1;
                out = args.get(i).map(PathBuf::from);
            }
            other => {
                eprintln!("error: unknown argument '{other}'");
                eprintln!("{USAGE}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(name) = ontology else {
        eprintln!("error: --ontology <name> is required");
        eprintln!("{USAGE}");
        eprintln!(
            "  templates: {}",
            corpus_engine::recipe_templates::list_builtin_names().join(", ")
        );
        return 2;
    };
    if name == "list" {
        for n in corpus_engine::recipe_templates::list_builtin_names() {
            println!("{n}");
        }
        return 0;
    }
    let template = match corpus_engine::recipe_templates::load_builtin(&name) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let text = corpus_engine::recipe_templates::instantiate(template, id.as_deref());
    match out {
        None => {
            print!("{text}");
            0
        }
        Some(path) => {
            if path.exists() {
                eprintln!(
                    "error: {} exists; choose another --out (recipe new never overwrites)",
                    path.display()
                );
                return 1;
            }
            if let Err(e) = std::fs::write(&path, &text) {
                eprintln!("error: write {}: {e}", path.display());
                return 1;
            }
            println!("wrote {}", path.display());
            0
        }
    }
}

// ── `recipe migrate` ────────────────────────────────────────────────────────

/// `svrn recipe migrate <path> --ontology-version N [--dry-run]`. Adds (or
/// raises) the `version = N` line under `[enrichment.ontology]` and nothing
/// else — `Recipe::migrate_ontology_version` is the one implementation and it
/// verifies the result loads. The diff is printed either way; without
/// `--dry-run` the file is rewritten in place.
pub(super) fn cmd_migrate(args: &[String]) -> i32 {
    const USAGE: &str = "Usage: svrn recipe migrate <path> --ontology-version N [--dry-run]";
    if sovereign_cli_shared::help::wants_help(args) {
        println!("{USAGE}");
        return 0;
    }
    let mut path: Option<PathBuf> = None;
    let mut target: Option<u32> = None;
    let mut dry_run = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--ontology-version" => {
                i += 1;
                target = args.get(i).and_then(|s| s.parse().ok());
                if target.is_none() {
                    eprintln!("error: --ontology-version needs an integer");
                    return 2;
                }
            }
            "--dry-run" => dry_run = true,
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown argument '{flag}'");
                eprintln!("{USAGE}");
                return 2;
            }
            p => path = Some(PathBuf::from(p)),
        }
        i += 1;
    }
    let (Some(path), Some(target)) = (path, target) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let before = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: read {}: {e}", path.display());
            return 1;
        }
    };
    let after = match Recipe::migrate_ontology_version(&before, target) {
        Ok(Some(after)) => after,
        Ok(None) => {
            println!(
                "{}: [enrichment.ontology] already declares version >= {target}; nothing to do",
                path.display()
            );
            return 0;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    // The diff IS the payload: one inserted or replaced line, with context.
    println!("--- {}", path.display());
    println!("+++ {}", path.display());
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let first = a
        .iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    let ctx_start = first.saturating_sub(1);
    for line in &a[ctx_start..first] {
        println!(" {line}");
    }
    if b.len() > a.len() {
        println!("+{}", b[first]);
    } else {
        println!("-{}", a[first]);
        println!("+{}", b[first]);
    }
    if let Some(next) = a.get(first + usize::from(b.len() <= a.len())) {
        println!(" {next}");
    }
    if dry_run {
        println!("(dry run — {} not modified)", path.display());
        return 0;
    }
    if let Err(e) = std::fs::write(&path, &after) {
        eprintln!("error: write {}: {e}", path.display());
        return 1;
    }
    println!("rewrote {}", path.display());
    0
}
