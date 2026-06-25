// SPDX-License-Identifier: AGPL-3.0-or-later
//! Corpus inventory commands — extracted from `corpus_cmd` (§3.2).
//! list / install / remove / status: the day-to-day corpus management
//! surface.

use std::path::PathBuf;


use super::fmt::{dir_size_bytes, format_count, human_bytes};

// ── Mesh subcommand implementations ──────────────────────

pub(super) async fn cmd_corpus_list() -> i32 {
    println!("Available built-in corpora:");
    println!();
    println!("  wikipedia       Wikipedia (6.8M articles, ~22 GB download)");
    println!("  stackexchange   Stack Exchange (12.4M answers, ~40 GB)");
    println!("  openalex        OpenAlex scholarly abstracts (~45 GB)");
    println!("  gutenberg       Project Gutenberg (~25 GB)");
    println!("  sep             Stanford Encyclopedia of Philosophy (~0.5 GB)");
    println!("  crs_reports     Congressional Research Service reports (~4 GB)");
    println!();
    println!("Install with: sovereign corpus install <id>");
    0
}

/// `sovereign corpus install <id> [--params name=value,...] [--param key=value]...`
///
/// Submits an install request to the running daemon's
/// `/internal/corpus/install` endpoint. The daemon owns the actual
/// ingest task — this CLI command is a thin client so the install
/// runs in the background and the user can disconnect / re-attach
/// via `sovereign corpus status`.
///
/// Recipe parameters: when the recipe declares a
/// `[recipe.parameters]` block (e.g. `sec-filings` asking for an
/// entity list), supply values via either:
///
/// - `--params entities=NVDA,MSFT,GOOGL --params start_date=2022-01-01`
///   (each `--params` flag carries one comma-joined `key=value`)
/// - `--param entities=NVDA,MSFT --param start_date=2022-01-01`
///   (singular form, easier to remember; semantically identical)
/// - `--params-file <path>` for a JSON file containing the full
///   parameter map — handy for SEC investigations with dozens of
///   CIK numbers.
pub(super) async fn cmd_corpus_install(args: &[String]) -> i32 {
    let mut positional: Vec<String> = Vec::new();
    let mut params: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    let mut params_file: Option<PathBuf> = None;

    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--params" | "--param" => {
                let Some(spec) = iter.next() else {
                    eprintln!("{a} requires a `key=value` argument");
                    return 1;
                };
                if let Err(e) = parse_param_spec(spec, &mut params) {
                    eprintln!("Invalid {a}: {e}");
                    return 1;
                }
            }
            "--params-file" => {
                let Some(p) = iter.next() else {
                    eprintln!("--params-file requires a path argument");
                    return 1;
                };
                params_file = Some(PathBuf::from(p));
            }
            "--help" | "-h" => {
                println!(
                    "Usage: sovereign corpus install <id> [--params k=v[,k=v...]] \
                     [--params-file <path>]\n\n\
                     Submits an install request to the running daemon. Recipe \
                     parameters declared in the recipe's `[recipe.parameters]` block \
                     are validated by the daemon before ingest spawns; missing \
                     required parameters fail the request synchronously."
                );
                return 0;
            }
            other if !other.starts_with('-') => positional.push(other.to_string()),
            other => {
                eprintln!("Unknown flag: {other}");
                return 1;
            }
        }
    }

    let Some(id) = positional.first() else {
        eprintln!("Missing corpus ID. Usage: sovereign corpus install <id> [--params …]");
        return 1;
    };

    if let Some(path) = params_file {
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to read --params-file {}: {e}", path.display());
                return 1;
            }
        };
        let from_file: std::collections::BTreeMap<String, serde_json::Value> =
            match serde_json::from_str(&raw) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "--params-file {} is not a JSON object of parameters: {e}",
                        path.display()
                    );
                    return 1;
                }
            };
        for (k, v) in from_file {
            params.entry(k).or_insert(v);
        }
    }

    submit_install_request(id, params).await
}

/// POST an install request to the running daemon's `/internal/corpus/install`
/// endpoint and report the outcome. The daemon owns the actual ingest task; this
/// is the thin, fire-and-forget client. Shared by `corpus install` and a
/// `workflow run <recipe-id>` dispatch so both delegate to the *same* install path
/// (surface-unify, don't deep-collapse — one client, two callers).
pub(crate) async fn submit_install_request(
    id: &str,
    params: std::collections::BTreeMap<String, serde_json::Value>,
) -> i32 {
    let url = "http://127.0.0.1:9742/internal/corpus/install";
    let body = serde_json::json!({
        "corpus_id": id,
        "parameters": params,
    });
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build HTTP client: {e}");
            return 1;
        }
    };
    match client.post(url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            // The endpoint is fire-and-forget; surface the success
            // shape so users know the daemon picked it up.
            let body_text = resp.text().await.unwrap_or_default();
            println!("Install requested: {id}");
            if !body_text.is_empty() {
                println!("{body_text}");
            }
            println!("Watch progress: sovereign corpus status");
            0
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            eprintln!("Daemon rejected install ({status}): {body}");
            1
        }
        Err(e) => {
            eprintln!(
                "Failed to contact daemon at {url}: {e}\n\n\
                 Is `sovereign daemon` running? Try: sovereign daemon status"
            );
            1
        }
    }
}

/// Parse a single `--params` / `--param` value into the running
/// parameter map. Accepts:
///
/// - `key=value` — single string value
/// - `key=v1,v2,v3` — list of strings (comma-separated)
/// - `key=` — empty value (rare but useful for clearing a default)
///
/// The daemon does the type coercion (strings → ints / dates per
/// the recipe's declared `ParameterKind`), so the CLI just shapes
/// the JSON.
fn parse_param_spec(
    spec: &str,
    out: &mut std::collections::BTreeMap<String, serde_json::Value>,
) -> std::result::Result<(), String> {
    let (key, value) = spec
        .split_once('=')
        .ok_or_else(|| format!("expected `key=value`, got `{spec}`"))?;
    let key = key.trim();
    if key.is_empty() {
        return Err("empty parameter name".into());
    }
    out.insert(key.to_string(), param_json_value(value));
    Ok(())
}

/// Shape a single `--param`/`--params` *value* into JSON: a comma-bearing value
/// becomes an array of trimmed, non-empty strings; otherwise a single trimmed
/// string. The daemon coerces strings → ints/dates per the recipe's declared
/// `ParameterKind`, so the CLI only shapes the JSON. Shared by [`parse_param_spec`]
/// and the `workflow run <recipe-id>` param conversion so the one `--param`
/// convention behaves identically across both surfaces.
pub(crate) fn param_json_value(value: &str) -> serde_json::Value {
    if value.contains(',') {
        let items: Vec<serde_json::Value> = value
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(serde_json::Value::String)
            .collect();
        serde_json::Value::Array(items)
    } else {
        serde_json::Value::String(value.trim().to_string())
    }
}

#[cfg(test)]
mod install_tests {
    use super::*;

    #[test]
    fn parse_param_spec_string() {
        let mut params = std::collections::BTreeMap::new();
        parse_param_spec("start_date=2022-01-01", &mut params).unwrap();
        assert_eq!(
            params.get("start_date"),
            Some(&serde_json::Value::String("2022-01-01".into()))
        );
    }

    #[test]
    fn parse_param_spec_list() {
        let mut params = std::collections::BTreeMap::new();
        parse_param_spec("entities=NVDA,MSFT,GOOGL", &mut params).unwrap();
        match params.get("entities") {
            Some(serde_json::Value::Array(arr)) => {
                assert_eq!(arr.len(), 3);
                assert_eq!(arr[0], serde_json::Value::String("NVDA".into()));
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn parse_param_spec_rejects_missing_equals() {
        let mut params = std::collections::BTreeMap::new();
        assert!(parse_param_spec("entities", &mut params).is_err());
    }

    #[test]
    fn parse_param_spec_rejects_empty_key() {
        let mut params = std::collections::BTreeMap::new();
        assert!(parse_param_spec("=NVDA", &mut params).is_err());
    }
}

/// Remove an installed corpus's on-disk index directories.
///
/// Two surfaces, gated by flags:
/// - Canonical `<corpus>/` (the merged, query-served index)
/// - Partition `<corpus>-partition-*/` (per-peer partial indexes,
///   produced during collaborative ingest; left in place by
///   merge-partitions for verification)
///
/// Default: removes BOTH (canonical + every partition). Operators
/// who want surgical cleanup (e.g. wipe a partial canonical but
/// keep partitions so the embed-side dedup gate still protects on
/// re-ingest) pass `--canonical-only` or `--partitions-only`.
///
/// No daemon coordination — POSIX rm-rf works even with open file
/// handles (LanceDB will see ENOENT on its next operation, and the
/// daemon's installed_indexes() rescans on its tick). If the daemon
/// is actively writing to the corpus, the WARN at the end of remove
/// suggests stopping it first; we don't gate on it because most
/// remove uses are post-hoc cleanups where the daemon is idle.
pub(super) async fn cmd_corpus_remove(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut yes = false;
    let mut canonical_only = false;
    let mut partitions_only = false;

    for arg in args {
        match arg.as_str() {
            "--yes" | "-y" => yes = true,
            "--canonical-only" => canonical_only = true,
            "--partitions-only" => partitions_only = true,
            "--help" | "-h" => {
                println!(
                    "Usage: sovereign corpus remove <corpus_id> [--canonical-only|--partitions-only] [--yes]\n\n\
                     Delete on-disk index directories for a corpus.\n\n\
                     Default: removes BOTH the canonical (<index_dir>/<corpus>/) and \
                     every partition (<index_dir>/<corpus>-partition-*/).\n\n\
                     --canonical-only   Remove only the canonical. Use after a partial-coverage \
                     merge produced an incomplete canonical that you want to discard while \
                     keeping the partition data for re-ingest.\n\
                     --partitions-only  Remove every partition. Use to reclaim disk after a \
                     successful merge has produced canonical and you no longer need the \
                     per-peer partial indexes for forensics.\n\
                     --yes / -y         Skip confirmation prompt.\n\n\
                     Stop the daemon first (`sovereign daemon stop`) if it's actively writing \
                     to the corpus — POSIX will let rm-rf succeed with open handles, but the \
                     daemon will surface ENOENT errors until it rescans."
                );
                return 0;
            }
            other if !other.starts_with('-') => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                }
            }
            other => {
                eprintln!("Unknown flag: {other}");
                return 1;
            }
        }
    }

    let Some(corpus_id) = corpus_id else {
        eprintln!("Missing corpus ID. Usage: sovereign corpus remove <corpus_id> [--canonical-only|--partitions-only] [--yes]");
        return 1;
    };

    if canonical_only && partitions_only {
        eprintln!(
            "--canonical-only and --partitions-only are mutually exclusive (default removes both)."
        );
        return 1;
    }

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        });
    let index_dir = data_dir.join("indexes");

    // Discover what's actually on disk for this corpus.
    let canonical_path = index_dir.join(&corpus_id);
    let canonical_exists = canonical_path.join("_corpus_meta.json").exists();

    let prefix = format!("{corpus_id}-partition-");
    let mut partition_paths: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&index_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if !name_str.starts_with(&prefix) {
                continue;
            }
            partition_paths.push(entry.path());
        }
    }
    partition_paths.sort();

    // Resolve which set of paths actually gets removed based on
    // flag combination. Partitions-only skips canonical even if it
    // exists; canonical-only skips partitions even if they exist.
    let remove_canonical = !partitions_only && canonical_exists;
    let remove_partitions = !canonical_only && !partition_paths.is_empty();

    if !remove_canonical && !remove_partitions {
        if canonical_only && !canonical_exists {
            eprintln!(
                "No canonical at {} (and --canonical-only specified — nothing to do).",
                canonical_path.display()
            );
        } else if partitions_only && partition_paths.is_empty() {
            eprintln!(
                "No partitions matching {}/{}-partition-*/ (and --partitions-only specified — nothing to do).",
                index_dir.display(),
                corpus_id
            );
        } else {
            eprintln!(
                "No on-disk artefacts found for corpus '{}' under {} — nothing to remove.",
                corpus_id,
                index_dir.display()
            );
        }
        return 0;
    }

    // Show what will be removed + sizes so the operator can sanity-
    // check before confirming.
    println!("Corpus '{corpus_id}' — remove plan:");
    println!();
    let mut total_bytes: u64 = 0;
    if remove_canonical {
        let bytes = dir_size_bytes(&canonical_path);
        total_bytes += bytes;
        println!(
            "  CANONICAL  {}  ({})",
            canonical_path.display(),
            human_bytes(bytes)
        );
    } else if canonical_exists {
        println!(
            "  CANONICAL  {}  (skipped — --partitions-only)",
            canonical_path.display()
        );
    }
    if remove_partitions {
        for path in &partition_paths {
            let bytes = dir_size_bytes(path);
            total_bytes += bytes;
            println!("  PARTITION  {}  ({})", path.display(), human_bytes(bytes));
        }
    } else if !partition_paths.is_empty() {
        for path in &partition_paths {
            println!(
                "  PARTITION  {}  (skipped — --canonical-only)",
                path.display()
            );
        }
    }
    println!();
    println!("  total reclaim:  {}", human_bytes(total_bytes));

    // Phase D3 — warn if removing destroys non-trivial Tier-2
    // enrichment work. Each `extracted` entity is ~14 wall-hours
    // of LLM time for the wiki-l5-tier2 reference run (52 entities
    // / 14h = ~16 min/entity at canonical pace), so the warning
    // helps the operator avoid an expensive accidental wipe.
    if remove_canonical {
        let atlas_dir = canonical_path.join("atlas");
        if let Some(summary) =
            corpus_engine::enrichment::atlas::read_or_compute_atlas_summary(&atlas_dir)
                .ok()
                .flatten()
        {
            if summary.tier2_count > 0 {
                println!();
                println!(
                    "⚠  This corpus has {} Tier-2 enriched entities (atlas).",
                    summary.tier2_count
                );
                println!("   That work is local-only unless a mesh peer has pulled this atlas.");
                println!(
                    "   Consider running `sovereign mesh push {corpus_id}` first if you have peers."
                );
            }
        }
    }

    if !yes {
        eprint!("\nProceed with removal? [y/N] ");
        use std::io::BufRead;
        let stdin = std::io::stdin();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).is_err() {
            eprintln!("aborted (could not read stdin)");
            return 1;
        }
        let answer = line.trim().to_lowercase();
        if answer != "y" && answer != "yes" {
            println!("aborted.");
            return 0;
        }
    }

    let mut failures: Vec<(PathBuf, std::io::Error)> = Vec::new();
    if remove_canonical {
        if let Err(e) = std::fs::remove_dir_all(&canonical_path) {
            failures.push((canonical_path.clone(), e));
        } else {
            println!("  ✓ removed {}", canonical_path.display());
        }
    }
    if remove_partitions {
        for path in &partition_paths {
            if let Err(e) = std::fs::remove_dir_all(path) {
                failures.push((path.clone(), e));
            } else {
                println!("  ✓ removed {}", path.display());
            }
        }
    }

    if !failures.is_empty() {
        eprintln!();
        eprintln!("Some removals failed:");
        for (path, err) in &failures {
            eprintln!("  ✗ {} — {}", path.display(), err);
        }
        eprintln!();
        eprintln!(
            "Most often this means the daemon is holding LanceDB file locks. \
             Stop it (`sovereign daemon stop`) and re-run."
        );
        return 1;
    }

    println!();
    println!(
        "✓ corpus remove complete ({} reclaimed).",
        human_bytes(total_bytes)
    );
    println!(
        "Note: the daemon's installed_indexes() rescans on its next tick — \
         hosted_corpora gossip will drop '{corpus_id}' shortly."
    );
    0
}
pub(super) async fn cmd_corpus_status() -> i32 {
    let indexes_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        })
        .join("indexes");
    let entries = match std::fs::read_dir(&indexes_dir) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!("error: read {}: {e}", indexes_dir.display());
            return 1;
        }
    };
    let mut rows: Vec<CorpusStatusRow> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') || name.starts_with('_') {
            continue;
        }
        rows.push(read_corpus_status_row(name, &path));
    }
    rows.sort_by(|a, b| a.corpus_id.cmp(&b.corpus_id));
    if rows.is_empty() {
        println!("(no corpora installed at {})", indexes_dir.display());
        return 0;
    }
    println!(
        "{:<32} {:>14} {:>10} {:>10} {:>10} {:>12}",
        "corpus", "chunks", "atlas", "tier-2", "embed-cache", "tier-2 toks"
    );
    println!("{}", "─".repeat(94));
    for r in rows {
        let chunks = r
            .chunk_count
            .map(|n| format_count(n as u64))
            .unwrap_or_else(|| "—".into());
        let atlas = r
            .atlas_entities
            .map(|n| format_count(n as u64))
            .unwrap_or_else(|| "—".into());
        let tier2 = r
            .atlas_extracted_entities
            .map(|n| format_count(n as u64))
            .unwrap_or_else(|| "—".into());
        let cache: String = if r.atlas_embeddings_cached {
            "✓".into()
        } else {
            "—".into()
        };
        let tokens = r
            .tier2_total_tokens
            .map(format_count)
            .unwrap_or_else(|| "—".into());
        println!(
            "{:<32} {:>14} {:>10} {:>10} {:>10} {:>12}",
            r.corpus_id, chunks, atlas, tier2, cache, tokens
        );
    }
    0
}

#[derive(Debug)]
struct CorpusStatusRow {
    corpus_id: String,
    chunk_count: Option<usize>,
    atlas_entities: Option<usize>,
    atlas_extracted_entities: Option<usize>,
    atlas_embeddings_cached: bool,
    /// Cumulative tokens spent in the corpus's `<corpus>-tier2`
    /// workspace's most recent extract run (Phase D2). `None` when
    /// no `_tokens.json` sidecar exists yet — i.e. Tier-2 hasn't
    /// run for this corpus.
    tier2_total_tokens: Option<u64>,
}

fn read_corpus_status_row(corpus_id: &str, dir: &std::path::Path) -> CorpusStatusRow {
    // Chunks: read `_corpus_meta.json` for an `enriched_chunks` /
    // computed count. We don't open lance here — too heavy for a
    // status command. Instead we report whether the meta file
    // claims indexed status.
    let chunk_count = std::fs::read_to_string(dir.join("_corpus_meta.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("enriched_chunks")
                .and_then(|n| n.as_u64())
                .map(|n| n as usize)
        });

    // Atlas: use the cached summary helper so a) the count agrees
    // with what mesh gossip advertises (Phase C1) and b) repeat
    // status calls don't reparse atoms.json on every invocation.
    let atlas_dir = dir.join("atlas");
    let summary = corpus_engine::enrichment::atlas::read_or_compute_atlas_summary(&atlas_dir)
        .ok()
        .flatten();
    let (atlas_entities, atlas_extracted_entities) = match summary {
        Some(s) => (Some(s.atom_count as usize), Some(s.tier2_count as usize)),
        None => (None, None),
    };
    let atlas_embeddings_cached = atlas_dir.join("atoms.embeddings.bin").exists();

    // Phase D2: read `<enrichment>/<corpus>-tier2/_tokens.json` if
    // the Tier-2 workspace has run at least one extract pass.
    // <enrichment> is sibling of <indexes> — derive from the
    // corpus dir's grandparent.
    let tier2_total_tokens = dir
        .parent()
        .and_then(|p| p.parent())
        .map(|data_dir| {
            data_dir
                .join("enrichment")
                .join(format!("{corpus_id}-tier2"))
                .join("_tokens.json")
        })
        .and_then(|p| crate::enrich_cmd::extract::read_token_snapshot(&p))
        .map(|r| r.total_tokens);

    CorpusStatusRow {
        corpus_id: corpus_id.to_string(),
        chunk_count,
        atlas_entities,
        atlas_extracted_entities,
        atlas_embeddings_cached,
        tier2_total_tokens,
    }
}
