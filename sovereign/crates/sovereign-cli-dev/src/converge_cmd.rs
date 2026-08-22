// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code converge` — the concept-convergence workhorse.
//!
//! Thin CLI wrapper over `corpus_engine_scip::converge`. Resolves the corpus
//! the same way `arch-report` / `suggest-seams` / `dry-report` do (explicit
//! `--corpus-id`, or the sole indexed code corpus), reads the SCIP graph
//! read-only, and prints. Never writes, never calls a model, never builds.
//!
//! Sibling verbs answer adjacent questions and this one defers to them
//! rather than reimplementing (ARCH §19): duplicated BEHAVIOUR is
//! `dry-report`, oversized FILES are `suggest-seams`, crate coupling is
//! `arch-report`. This verb owns duplicated IDENTITY only.

use std::collections::BTreeMap;
use std::path::PathBuf;

use corpus_engine_scip::converge::{
    census, crate_dag, dossier, duplicate_count, render_census, render_dossier, type_defs,
    SourceScope,
};
use corpus_engine_scip::ScipGraph;

const HELP: &str = "\
svrn code converge <census|noun|status> [options]

Duplicated concept IDENTITY over the SCIP graph — names defined as a type in
more than one crate. Read-only; no daemon, no model, no build.

  census                  what is duplicated, ranked
    --kin                 also count morphological family (over-collects)
    --limit N             rows to print (0 = all; default 40)

  noun <Name>             one noun's dossier: definitions, users, the crate
                          that could own it, and the users that cannot reach it

  status                  the ratchet number, against a frozen baseline
    --baseline <file>     default quality/baselines/concepts.txt
    --mint                write the current number as the baseline

Common:
  --corpus-id <id>        default: the sole indexed code corpus
  --include <prefix>      restrict to a path prefix (repeatable)
  --json                  machine output, carrying the scope that produced it

Adjacent verbs, deliberately not duplicated here:
  svrn code dry-report      duplicated BEHAVIOUR (clone + near-clone bodies)
  svrn code suggest-seams   split proposals for an oversized FILE
  svrn code arch-report     crate coupling and the carrier symbols
";

pub(crate) async fn run(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        println!("{HELP}");
        return i32::from(args.is_empty());
    }

    let sub = args[0].clone();
    let mut corpus_id: Option<String> = None;
    let mut baseline = PathBuf::from("quality/baselines/concepts.txt");
    let mut includes: Vec<String> = Vec::new();
    let mut noun: Option<String> = None;
    let mut limit: usize = 40;
    let mut kin = false;
    let mut json = false;
    let mut mint = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus-id" => {
                i += 1;
                corpus_id = args.get(i).cloned();
                if corpus_id.is_none() {
                    eprintln!("error: --corpus-id requires a value");
                    return 1;
                }
            }
            "--baseline" => {
                i += 1;
                match args.get(i) {
                    Some(v) => baseline = PathBuf::from(v),
                    None => {
                        eprintln!("error: --baseline requires a path");
                        return 1;
                    }
                }
            }
            "--include" => {
                i += 1;
                match args.get(i) {
                    Some(v) => includes.push(v.clone()),
                    None => {
                        eprintln!("error: --include requires a prefix");
                        return 1;
                    }
                }
            }
            "--limit" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(0) => limit = usize::MAX,
                    Some(n) => limit = n,
                    None => {
                        eprintln!("error: --limit requires an integer");
                        return 1;
                    }
                }
            }
            "--kin" => kin = true,
            "--json" => json = true,
            "--mint" => mint = true,
            "-h" | "--help" => {
                println!("{HELP}");
                return 0;
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag {flag}");
                return 1;
            }
            positional => {
                if noun.is_none() {
                    noun = Some(positional.to_string());
                }
            }
        }
        i += 1;
    }

    let indexes_dir = sovereign_cli_shared::dirs::sovereign_root().join("indexes");
    let corpus_id = match resolve_corpus(corpus_id, &indexes_dir) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let db_path = indexes_dir.join(&corpus_id).join("scip_graph.db");
    if !db_path.exists() {
        eprintln!(
            "error: no SCIP graph at {} — run `svrn project init` first",
            db_path.display()
        );
        return 1;
    }

    let graph = match ScipGraph::open(&db_path, &corpus_id) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: opening {}: {e}", db_path.display());
            return 1;
        }
    };
    let symbols = match graph.iter_all_symbols().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: reading symbols: {e}");
            return 1;
        }
    };

    let scope = SourceScope {
        include_prefixes: includes,
        ..SourceScope::default()
    };
    let defs = type_defs(&symbols, &scope);
    if defs.is_empty() {
        eprintln!(
            "error: no type definitions in scope — the graph has {} symbols, so \
             the scope filter is probably wrong (check --include)",
            symbols.len()
        );
        return 1;
    }

    match sub.as_str() {
        "census" => {
            let c = census(&defs, &scope, kin);
            if json {
                println!("{}", serde_json::to_string_pretty(&c).unwrap_or_default());
            } else {
                print!("{}", render_census(&c, limit, kin));
            }
            0
        }
        "noun" => {
            let Some(name) = noun else {
                eprintln!("error: `converge noun` requires a name, e.g. `converge noun Verdict`");
                return 1;
            };
            let refs = match graph.iter_all_refs().await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: reading refs: {e}");
                    return 1;
                }
            };
            let dag = crate_dag(&refs, &scope);
            let d = dossier(&name, &defs, &refs, &dag, &scope);
            if json {
                println!("{}", serde_json::to_string_pretty(&d).unwrap_or_default());
            } else {
                print!("{}", render_dossier(&d));
            }
            0
        }
        "status" => cmd_status(&defs, &baseline, mint, json),
        other => {
            eprintln!("error: unknown converge subcommand `{other}`");
            println!("{HELP}");
            1
        }
    }
}

fn resolve_corpus(explicit: Option<String>, indexes_dir: &std::path::Path) -> Result<String, i32> {
    if let Some(c) = explicit {
        return Ok(c);
    }
    let mut corpora: Vec<String> = std::fs::read_dir(indexes_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().join("scip_graph.db").exists())
                .filter_map(|e| e.file_name().to_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    corpora.sort();
    match corpora.len() {
        1 => Ok(corpora.remove(0)),
        0 => {
            eprintln!(
                "error: no code corpus under {} — run `svrn project init` first",
                indexes_dir.display()
            );
            Err(1)
        }
        _ => {
            eprintln!(
                "error: multiple code corpora — pass --corpus-id <one of: {}>",
                corpora.join(", ")
            );
            Err(1)
        }
    }
}

/// The ratchet. Exits 1 when the count rises — a duplicate was ADDED, which is
/// the failure the line-count arch-gate cannot catch.
fn cmd_status(
    defs: &[corpus_engine_scip::converge::TypeDef],
    baseline: &std::path::Path,
    mint: bool,
    json: bool,
) -> i32 {
    let n = duplicate_count(defs);
    let prior: Option<usize> = std::fs::read_to_string(baseline)
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse().ok());

    if mint {
        if let Some(parent) = baseline.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(baseline, format!("{n}\n")) {
            eprintln!("error: writing {}: {e}", baseline.display());
            return 1;
        }
        println!("minted {n} -> {}", baseline.display());
        return 0;
    }

    if json {
        let body: BTreeMap<&str, serde_json::Value> = [
            ("duplicated_names", serde_json::json!(n)),
            ("baseline", serde_json::json!(prior)),
            (
                "delta",
                serde_json::json!(prior.map(|p| n as i64 - p as i64)),
            ),
        ]
        .into_iter()
        .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_default()
        );
    }

    match prior {
        None => {
            if !json {
                println!("duplicated names: {n}");
                println!(
                    "baseline: none — mint one with `svrn code converge status --mint`\n\
                     (four verdicts, not two: this is NEVER-RAN, not a pass)"
                );
            }
            // No baseline is not a pass. ARCH §18.2.
            4
        }
        Some(p) => {
            let delta = n as i64 - p as i64;
            if !json {
                println!("duplicated names: {n}");
                println!("baseline: {p}   delta: {delta:+}");
            }
            if delta > 0 {
                if !json {
                    println!(
                        "\nRATCHET BROKEN — {delta} concept(s) added. Either converge the new \
                         duplicate,\nor rename it apart and say which in the landing verdict."
                    );
                }
                1
            } else {
                0
            }
        }
    }
}
