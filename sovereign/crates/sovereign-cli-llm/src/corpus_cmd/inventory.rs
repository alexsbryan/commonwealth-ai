// SPDX-License-Identifier: AGPL-3.0-or-later
//! Corpus inventory commands — extracted from `corpus_cmd` (§3.2).
//! list / install / remove / status: the day-to-day corpus management
//! surface.

use std::path::PathBuf;

use corpus_engine::Corpus;

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
    println!("Install with: svrn corpus install <id>");
    0
}

/// `svrn corpus install <id> [--params name=value,...] [--param key=value]...`
///
/// Submits an install request to the running daemon's
/// `/internal/corpus/install` endpoint. The daemon owns the actual
/// ingest task — this CLI command is a thin client so the install
/// runs in the background and the user can disconnect / re-attach
/// via `svrn corpus status`.
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
    let mut wait_secs: Option<u64> = None;

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
            "--wait" => {
                wait_secs = Some(DEFAULT_WAIT_SECS);
            }
            _ if a.starts_with("--wait=") => {
                let raw = &a["--wait=".len()..];
                match raw.parse::<u64>() {
                    Ok(n) => wait_secs = Some(n),
                    Err(_) => {
                        eprintln!("--wait= expects whole seconds, got `{raw}`");
                        return 1;
                    }
                }
            }
            "--help" | "-h" => {
                println!(
                    "Usage: svrn corpus install <id|recipe.toml> [--wait[=SECS]] \
                     [--params k=v[,k=v...]] [--params-file <path>]\n\n\
                     Given a path to a recipe file, registers it under \
                     ~/.svrnmesh/recipes/<id>/ (printing what it registered) and \
                     installs the id the recipe declares. Given an id, installs \
                     that id.\n\n\
                     Submits an install request to the running daemon. Recipe \
                     parameters declared in the recipe's `[recipe.parameters]` block \
                     are validated by the daemon before ingest spawns; missing \
                     required parameters fail the request synchronously.\n\n\
                     Without --wait the command returns as soon as the daemon \
                     ACCEPTS the request — the ingest runs in the background and \
                     the index does not exist yet. Exit 0 means \"requested\", not \
                     \"installed\".\n\n\
                     With --wait it polls until the index is actually usable and \
                     exits non-zero if it never becomes so, so exit 0 means \
                     \"installed\". Default budget {DEFAULT_WAIT_SECS}s; \
                     --wait=SECS to change it. Use this in scripts and gates."
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

    let Some(arg) = positional.first() else {
        eprintln!("Missing corpus ID. Usage: svrn corpus install <id|recipe.toml> [--params …]");
        return 1;
    };

    // `svrn recipe validate my-coins.toml` takes a path, so `svrn corpus
    // install my-coins.toml` has to as well — the two are consecutive lines
    // in every template header. Installing by path registers the file where
    // the registry looks for user recipes, then installs the id it declares.
    let id = if super::recipe_source::looks_like_recipe_path(arg) {
        match super::recipe_source::register(std::path::Path::new(arg)) {
            Ok(reg) => {
                println!(
                    "Registered {} as corpus '{}' → {}",
                    arg,
                    reg.id,
                    reg.registered_at.display()
                );
                if let Some((before, after)) = &reg.acquire_rewrite {
                    println!(
                        "  acquire path `{before}` resolved against the recipe's directory → {after}"
                    );
                }
                reg.id
            }
            Err(msg) => {
                eprintln!("error: {msg}");
                return 1;
            }
        }
    } else {
        arg.clone()
    };
    let id = &id;

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

    let code = submit_install_request(id, params).await;
    if code != 0 {
        return code;
    }
    match wait_secs {
        None => 0,
        Some(budget) => wait_until_installed(id, budget).await,
    }
}

/// Default `--wait` budget. Generous enough for any LOCAL recipe (the
/// journey fixture commits in ~20s on this host) and far too short for a
/// 22 GB catalog download — which is the point: `--wait` is for scripts
/// and gates that need "installed" to mean installed, and a caller waiting
/// on a multi-hour download should say `--wait=<their own number>`.
const DEFAULT_WAIT_SECS: u64 = 300;

/// Poll [`corpus_readiness`] until `corpus_id` is [`CorpusReadiness::Ready`]
/// or the budget runs out. Exit code, not a bool, because this IS the
/// command's exit code.
///
/// WHY THIS EXISTS. `corpus install` POSTs and returns; the ingest lands
/// seconds-to-hours later, and until the finalise step renames the
/// partition there is no canonical index at all. So exit 0 from a bare
/// `corpus install` truthfully means "the daemon accepted the request" and
/// was being read everywhere as "the corpus is installed". The CLI-contract
/// `enrich-atlas` journey read it that way and then failed two steps later
/// with `Index not found`, having reported two green ticks first.
///
/// The absence is REPORTED here, never defaulted (§18.3): a budget that
/// expires exits 1 and names the state it actually observed, so a caller
/// cannot mistake "still building" for "installed".
async fn wait_until_installed(corpus_id: &str, budget_secs: u64) -> i32 {
    let indexes_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root())
        .join("indexes");
    let start = std::time::Instant::now();
    let budget = std::time::Duration::from_secs(budget_secs);
    let mut last = CorpusReadiness::Absent;
    loop {
        last = corpus_readiness(&indexes_dir, corpus_id);
        tracing::debug!(
            corpus = corpus_id,
            state = last.label(),
            waited_secs = start.elapsed().as_secs(),
            "corpus install --wait: polled readiness"
        );
        if last == CorpusReadiness::Ready {
            println!(
                "✓ installed: {corpus_id} (ready after {}s)",
                start.elapsed().as_secs()
            );
            return 0;
        }
        if start.elapsed() >= budget {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    eprintln!(
        "error: {corpus_id} is still `{}` after {budget_secs}s — the index at {} is not usable.",
        last.label(),
        indexes_dir.join(corpus_id).display()
    );
    eprintln!(
        "       Check `svrn corpus status {corpus_id}` and the daemon log; \
         raise the budget with --wait=SECS for a large corpus."
    );
    1
}

/// Base URL of the daemon's INTERNAL listener, honouring
/// `[daemon] internal_port` in `~/.svrnmesh/config.toml`.
///
/// The resolution now lives in `sovereign_contracts::setup_config`, next to the
/// field it reads, because this same literal had been hardcoded at FOUR sites
/// (here, `alignment_cmd.rs` progress, `pipeline_cmd.rs` pause, and a `doctor`
/// probe). The focused sweep this note used to defer is done; all four call the
/// shared helper. Kept as a thin alias so the call sites below read locally.
use sovereign_contracts::setup_config::internal_daemon_base;

/// POST an install request to the running daemon's `/internal/corpus/install`
/// endpoint and report the outcome. The daemon owns the actual ingest task; this
/// is the thin, fire-and-forget client. Shared by `corpus install` and a
/// `workflow run <recipe-id>` dispatch so both delegate to the *same* install path
/// (surface-unify, don't deep-collapse — one client, two callers).
pub(crate) async fn submit_install_request(
    id: &str,
    params: std::collections::BTreeMap<String, serde_json::Value>,
) -> i32 {
    let url = format!("{}/internal/corpus/install", internal_daemon_base());
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
    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            // The endpoint is fire-and-forget, but its 200 body carries a
            // `spawned` flag: true = a new ingest task started, false = an
            // ingest for this corpus was already in flight (idempotent
            // no-op). Distinguish them so an already-running corpus doesn't
            // read as a fresh "Install requested" — the daemon returns 4xx
            // (handled below) for genuine failures, so a 200 here is never
            // a silent error, only "started" vs "already going".
            let body_text = resp.text().await.unwrap_or_default();
            let spawned = serde_json::from_str::<serde_json::Value>(&body_text)
                .ok()
                .and_then(|v| v.get("spawned").and_then(|s| s.as_bool()));
            match spawned {
                Some(false) => {
                    println!("Already in progress: {id} (ingest already running — not re-spawned)");
                }
                _ => {
                    // spawned:true, or a body we couldn't parse — treat as
                    // a fresh request and still show the raw body for
                    // observability.
                    println!("Install requested: {id}");
                    if !body_text.is_empty() {
                        println!("{body_text}");
                    }
                }
            }
            println!("Watch progress: svrn corpus status");
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
                 Is `svrn daemon` running? Try: svrn daemon status"
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
                    "Usage: svrn corpus remove <corpus_id> [--canonical-only|--partitions-only] [--yes]\n\n\
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
                     Stop the daemon first (`svrn daemon stop`) if it's actively writing \
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
        eprintln!("Missing corpus ID. Usage: svrn corpus remove <corpus_id> [--canonical-only|--partitions-only] [--yes]");
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
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root());
    let index_dir = data_dir.join("indexes");

    // Discover what's actually on disk for this corpus. `Corpus` owns the
    // layout (canonical root, partition prefix, meta sidecar); this command
    // used to spell all three itself.
    let Some(corpus) = Corpus::named(&index_dir, &corpus_id) else {
        // Refused, not defaulted: `index_dir.join("")` is the index ROOT, so an
        // empty id used to mean "every corpus on this node" (ARCH §18.3).
        eprintln!("corpus id must not be empty");
        return 1;
    };
    let canonical_path = corpus.root();
    let canonical_exists = corpus.is_installed();

    let prefix = corpus.partition_prefix();
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
                    "   Consider running `svrn mesh push {corpus_id}` first if you have peers."
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
             Stop it (`svrn daemon stop`) and re-run."
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
/// `svrn corpus status [<corpus>]`
///
/// With no argument, every corpus the indexes dir knows about. With a
/// corpus id, just that one — which is what makes the `state` column
/// assertable by a caller that cares about ONE corpus (the CLI-contract
/// `enrich-atlas` journey greps this output for `ready`; unfiltered, some
/// OTHER corpus being ready would satisfy it).
pub(super) async fn cmd_corpus_status(args: &[String]) -> i32 {
    let indexes_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root())
        .join("indexes");
    let filter: Option<&str> = args
        .iter()
        .map(|s| s.as_str())
        .find(|a| !a.starts_with('-'));
    let mut rows = match scan_corpus_rows(&indexes_dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: read {}: {e}", indexes_dir.display());
            return 1;
        }
    };
    if let Some(want) = filter {
        rows.retain(|r| r.corpus_id == want);
        if rows.is_empty() {
            // ABSENCE IS REPORTED, NOT DEFAULTED (§18.3). A filtered
            // status that matched nothing must say so in the state
            // vocabulary the caller is grepping for, and must not print
            // an empty table that a `stdout_non_empty` check would pass.
            println!("{:<32} {:>12}", want, CorpusReadiness::Absent.label());
            println!(
                "(no index for '{want}' under {} — `svrn corpus install {want} --wait`)",
                indexes_dir.display()
            );
            return 0;
        }
    }
    if rows.is_empty() {
        println!("(no corpora installed at {})", indexes_dir.display());
        return 0;
    }
    println!(
        "{:<32} {:>12} {:>14} {:>10} {:>10} {:>10} {:>12}",
        "corpus", "state", "chunks", "atlas", "tier-2", "embed-cache", "tier-2 toks"
    );
    println!("{}", "─".repeat(105));
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
            "{:<32} {:>12} {:>14} {:>10} {:>10} {:>10} {:>12}",
            r.corpus_id,
            r.state.label(),
            chunks,
            atlas,
            tier2,
            cache,
            tokens
        );
    }
    0
}

/// Scan `indexes_dir` into one row per CORPUS (not per directory).
///
/// Split out of [`cmd_corpus_status`] so the rule it encodes is testable
/// without a daemon: the bug this function exists to prevent could only be
/// reproduced through a live install before, because the printing and the
/// scanning were the same function.
///
/// Two rules, both of which the by-directory-name version got wrong:
///
/// 1. **A row is a corpus, keyed by the `corpus_id` in its
///    `_corpus_meta.json`** — never by the directory name. An in-flight
///    ingest writes `<corpus>-partition-<node>/`, and naming the row after
///    the directory invented a corpus called
///    `journey-fixture-partition-node-3148a89c1ae48238` that no one can
///    install, remove, or query.
/// 2. **Readiness comes from [`corpus_readiness`]**, the one decider — so
///    a corpus whose bytes are still landing reads `building`, not a row
///    indistinguishable from a finished install.
fn scan_corpus_rows(indexes_dir: &std::path::Path) -> std::io::Result<Vec<CorpusStatusRow>> {
    let mut by_id: std::collections::BTreeMap<String, CorpusStatusRow> =
        std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(indexes_dir)?.flatten() {
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
        // The corpus this directory belongs to — its meta's `corpus_id`,
        // which a partition dir carries verbatim (observed: the partition
        // `journey-fixture-partition-node-…` declares
        // `"corpus_id": "journey-fixture"`). Fall back to the directory
        // name only for a dir with no readable meta, which is also the
        // only case where the two can legitimately disagree.
        let corpus_id = read_meta_corpus_id(&path).unwrap_or_else(|| name.to_string());
        let state = corpus_readiness(indexes_dir, &corpus_id);
        // Prefer the CANONICAL directory's numbers when it exists: it is
        // the one `enrich init`, `chat` and search actually open. Same
        // preference `installed_indexes()`' `dedupe_by_corpus_id` applies.
        let is_canonical = name == corpus_id;
        if let Some(existing) = by_id.get(&corpus_id) {
            if !is_canonical && existing.from_canonical {
                continue;
            }
        }
        let mut row = read_corpus_status_row(&corpus_id, &path);
        row.state = state;
        row.from_canonical = is_canonical;
        by_id.insert(corpus_id, row);
    }
    Ok(by_id.into_values().collect())
}

/// The `corpus_id` a directory's `_corpus_meta.json` declares, if any.
fn read_meta_corpus_id(dir: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(Corpus::meta_in(dir)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("corpus_id")
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

/// Whether a corpus is usable on disk — THE one decider for that question
/// on the CLI side, and the reason both `corpus status` and
/// `corpus install --wait` cannot drift apart (§10.6).
///
/// It delegates the actual judgement to
/// [`corpus_engine::index::CorpusIndex::is_ingest_finished`] — NOT to
/// `is_ingestion_complete`, which answers the narrower "is a writer active
/// right now" and which this surface used until 2026-08-28. That predicate
/// is true for an ingest that stopped without ever building its indexes, so
/// `corpus status` printed `ready` for 7 of 355 local corpora that no
/// retrieval path would touch (`corpus_unavailability` refuses `NotBuilt`
/// before it looks at the query). One of them, `wikipedia-newsworthy`, cost
/// a chaos-soak triage two wrong conclusions: the app's honest "I cannot
/// search this corpus" was read as a fabricated system status, because this
/// surface contradicted it about the same corpus. Before this existed, `corpus status` answered the same
/// question by asking whether a DIRECTORY existed — a second, wrong
/// implementation of "is it installed", and the one that reported an
/// ingest 0 seconds old as an installed corpus.
///
/// Four states, not a boolean, deliberately: `Building` is the state the
/// old surface had no name for and therefore rendered as success, and
/// `Unsearchable` is the one it had no name for AFTER that. Same shape as
/// `sovereign-ci-bench.sh`'s `PASS(warn:setup)` — when the thing you would
/// judge is not there yet, say THAT rather than pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CorpusReadiness {
    /// The canonical index exists and its ingestion is fully committed.
    /// This is the only state in which `enrich init`, `chat --corpus` and
    /// search can open it.
    Ready,
    /// Bytes are landing: a partition directory exists, or the canonical
    /// directory is present but still flagged `ingestion_in_progress`.
    Building,
    /// On disk, no writer running — and the indexes were never built. The
    /// ingest STOPPED rather than finished, so every retrieval path refuses
    /// this corpus (`UnavailabilityReason::NotBuilt`) even though nothing is
    /// in progress and the directory looks complete. Distinct from
    /// `Building`, which will resolve on its own; this one will not, and
    /// wants a rebuild.
    Unsearchable,
    /// Nothing on disk for this corpus id.
    Absent,
}

impl CorpusReadiness {
    /// Lowercase, single-word, greppable — the CLI-contract journey
    /// asserts on these exact strings, so they are API, not decoration.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Building => "building",
            Self::Unsearchable => "unsearchable",
            Self::Absent => "absent",
        }
    }
}

/// See [`CorpusReadiness`]. Pure function of the filesystem — no daemon,
/// no async, cheap enough for a status command (it reads one small JSON
/// per candidate directory and never opens LanceDB).
pub(crate) fn corpus_readiness(indexes_dir: &std::path::Path, corpus_id: &str) -> CorpusReadiness {
    let canonical = indexes_dir.join(corpus_id);
    if canonical.is_dir() {
        if corpus_engine::index::CorpusIndex::is_ingest_finished(&canonical) {
            return CorpusReadiness::Ready;
        }
        if corpus_engine::index::CorpusIndex::is_ingestion_complete(&canonical) {
            // No writer running, yet the ingest never built its indexes.
            // Reported `ready` until 2026-08-28 — see the type's docs.
            return CorpusReadiness::Unsearchable;
        }
        // The canonical dir exists but its ingest never committed — a
        // process killed mid-embed. `installed_indexes()` skips it; so do
        // we, and we say why rather than listing it as installed.
        return CorpusReadiness::Building;
    }
    // No canonical dir. An ingest in flight writes
    // `<corpus_id>-partition-<node_id>` and the canonical directory is
    // materialised ONLY by the finalise/merge step (see
    // `CorpusEngine::partition_path`), so a partition is exactly the
    // "still building" signal.
    let Some(partition_prefix) =
        Corpus::named(indexes_dir, corpus_id).map(|c| c.partition_prefix())
    else {
        return CorpusReadiness::Absent;
    };
    if let Ok(read) = std::fs::read_dir(indexes_dir) {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&partition_prefix) && entry.path().is_dir() {
                return CorpusReadiness::Building;
            }
        }
    }
    CorpusReadiness::Absent
}

#[derive(Debug)]
struct CorpusStatusRow {
    corpus_id: String,
    /// Whether this corpus can actually be opened — see [`corpus_readiness`].
    state: CorpusReadiness,
    /// True when the numbers came from the canonical directory rather
    /// than a partition, so a later partition row cannot overwrite it.
    from_canonical: bool,
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
    let chunk_count = std::fs::read_to_string(Corpus::meta_in(dir))
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
        // Overwritten by `scan_corpus_rows` from the one decider. The
        // pessimistic default matters: a future caller that forgets to set
        // it under-claims rather than inventing a readiness it never checked.
        state: CorpusReadiness::Building,
        from_canonical: false,
        chunk_count,
        atlas_entities,
        atlas_extracted_entities,
        atlas_embeddings_cached,
        tier2_total_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Write a `_corpus_meta.json` these tests can turn on.
    ///
    /// EVERY FIELD BELOW IS LOAD-BEARING. `IndexMeta` (corpus-engine
    /// `index/mod.rs:286`) makes eight of them mandatory — `corpus_id`,
    /// `corpus_name`, `embedding_model`, `embedding_dimensions`,
    /// `mesh_sharing`, `license`, `created_at`, `last_updated` — and
    /// `read_meta` returns `Err` for the whole file if one is missing.
    /// `is_ingestion_complete` then maps that `Err` to `false`, so an
    /// under-specified fixture reads as "not complete" and every
    /// readiness assertion in this module fails for a reason that has
    /// nothing to do with readiness. (It did, on the first run.)
    ///
    /// That failure direction is the correct one — an unparseable meta is
    /// not an installed corpus — but it makes the fixture's completeness
    /// part of what these tests assert, so do not trim this down.
    /// The ordinary two states. `indexes_built` is derived as
    /// `!ingestion_in_progress` here because that is what a HEALTHY ingest
    /// looks like — but the coupling is exactly the assumption that broke
    /// `corpus status` (a stopped-but-unfinished ingest has neither flag
    /// set), so the stalled case needs [`write_meta_with`].
    fn write_meta(dir: &Path, corpus_id: &str, ingestion_in_progress: bool) {
        write_meta_with(
            dir,
            corpus_id,
            ingestion_in_progress,
            !ingestion_in_progress,
        );
    }

    fn write_meta_with(
        dir: &Path,
        corpus_id: &str,
        ingestion_in_progress: bool,
        indexes_built: bool,
    ) {
        std::fs::create_dir_all(dir).unwrap();
        let meta = serde_json::json!({
            "corpus_id": corpus_id,
            "corpus_name": format!("{corpus_id} (test)"),
            "embedding_model": "qwen-embedding-0.6b",
            "embedding_dimensions": 1024,
            "mesh_sharing": false,
            "license": "private",
            "created_at": 1_786_548_248_u64,
            "last_updated": 1_786_548_248_u64,
            "schema_version": 3,
            "is_shard": false,
            "ingestion_in_progress": ingestion_in_progress,
            "indexes_built": indexes_built,
        });
        std::fs::write(
            Corpus::meta_in(dir),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
    }

    /// THE REGRESSION. Reproduced live on 2026-08-12 against a fresh sandbox
    /// HOME: `corpus install journey-fixture` exits 0 immediately, the daemon
    /// writes `journey-fixture-partition-node-3148a89c1ae48238/`, and the
    /// canonical `journey-fixture/` does not exist for another ~20 seconds.
    ///
    /// `corpus status` listed that partition directory BY NAME, so its output
    /// contained the string `journey-fixture` at t+0 with zero chunks
    /// committed — which is how the CLI-contract `enrich-atlas` journey's
    /// `stdout_contains = "{corpus}"` barrier passed, twice, before
    /// `enrich init` failed with `Index not found`.
    #[test]
    fn in_flight_partition_is_not_reported_as_an_installed_corpus() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path();
        write_meta(
            &indexes.join("journey-fixture-partition-node-3148a89c1ae48238"),
            "journey-fixture",
            true,
        );

        let rows = scan_corpus_rows(indexes).unwrap();

        assert_eq!(rows.len(), 1, "one corpus is being built, so one row");
        // The row names the CORPUS, never the partition directory. There is
        // no corpus called `journey-fixture-partition-node-…` — you cannot
        // install it, remove it, or query it.
        assert_eq!(rows[0].corpus_id, "journey-fixture");
        assert_eq!(
            rows[0].state,
            CorpusReadiness::Building,
            "a partition mid-ingest is `building`; reporting it as installed \
             is the bug this test exists for"
        );
        assert_ne!(rows[0].state, CorpusReadiness::Ready);
    }

    /// The order's install → remove → install sequence, pinned at the level
    /// the decider actually sees. Recorded live in the same order:
    /// ready (t+25s) → absent (after `corpus remove --yes`) → building
    /// (t+0 of the second install). The third state is the one that used to
    /// read as success.
    #[test]
    fn install_after_remove_is_building_not_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path();
        std::fs::create_dir_all(indexes).unwrap();

        // 1. First install completed: canonical dir, ingestion committed.
        write_meta(&indexes.join("journey-fixture"), "journey-fixture", false);
        assert_eq!(
            corpus_readiness(indexes, "journey-fixture"),
            CorpusReadiness::Ready
        );

        // 2. `corpus remove --yes` — observed to remove the canonical dir and
        //    leave nothing behind. No registry row, no cache marker: this is
        //    the evidence that eliminated "remove is the liar".
        std::fs::remove_dir_all(indexes.join("journey-fixture")).unwrap();
        assert_eq!(
            corpus_readiness(indexes, "journey-fixture"),
            CorpusReadiness::Absent,
            "remove leaves nothing — absence must be reported as absence"
        );

        // 3. Second install, t+0: the daemon spawned a REAL ingest
        //    (`spawned: true`) which writes a partition first. The canonical
        //    dir is materialised only by the finalise step.
        write_meta(
            &indexes.join("journey-fixture-partition-node-3148a89c1ae48238"),
            "journey-fixture",
            true,
        );
        assert_eq!(
            corpus_readiness(indexes, "journey-fixture"),
            CorpusReadiness::Building,
            "install exits 0 here; the index is NOT usable here"
        );
    }

    /// A canonical directory whose ingest never committed (process killed
    /// mid-embed) is not usable either — `installed_indexes()` skips it, and
    /// so must this. Same rule, other shape.
    #[test]
    fn interrupted_canonical_ingest_is_building_not_ready() {
        let tmp = tempfile::tempdir().unwrap();
        write_meta(&tmp.path().join("halfway"), "halfway", true);
        assert_eq!(
            corpus_readiness(tmp.path(), "halfway"),
            CorpusReadiness::Building
        );
    }

    /// The partition probe matches on `<corpus_id>-partition-`, so a
    /// DIFFERENT corpus that merely shares a name prefix cannot make this one
    /// look like it is building. `foo` and `foo-bar` are separate corpora.
    #[test]
    fn a_prefix_sharing_corpus_does_not_forge_readiness() {
        let tmp = tempfile::tempdir().unwrap();
        write_meta(&tmp.path().join("foo-bar"), "foo-bar", false);
        assert_eq!(
            corpus_readiness(tmp.path(), "foo"),
            CorpusReadiness::Absent,
            "`foo-bar` says nothing about `foo`"
        );
        assert_eq!(
            corpus_readiness(tmp.path(), "foo-bar"),
            CorpusReadiness::Ready
        );
    }

    /// When both a finished canonical dir and a leftover partition exist, the
    /// corpus is ready and appears ONCE — the canonical dir is what
    /// `enrich init` and search open.
    #[test]
    fn canonical_wins_over_a_leftover_partition_and_collapses_to_one_row() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path();
        write_meta(&indexes.join("journey-fixture"), "journey-fixture", false);
        write_meta(
            &indexes.join("journey-fixture-partition-node-old"),
            "journey-fixture",
            true,
        );

        let rows = scan_corpus_rows(indexes).unwrap();
        assert_eq!(rows.len(), 1, "one corpus, not one row per directory");
        assert_eq!(rows[0].corpus_id, "journey-fixture");
        assert_eq!(rows[0].state, CorpusReadiness::Ready);
        assert!(rows[0].from_canonical);
    }

    /// The labels are asserted on by `sovereign/docs/cli-contract.toml`
    /// (journey `enrich-atlas`), so they are API. Renaming one silently
    /// turns that journey's barrier back into a vacuous check.
    #[test]
    fn readiness_labels_are_stable_api() {
        assert_eq!(CorpusReadiness::Ready.label(), "ready");
        assert_eq!(CorpusReadiness::Building.label(), "building");
        assert_eq!(CorpusReadiness::Unsearchable.label(), "unsearchable");
        assert_eq!(CorpusReadiness::Absent.label(), "absent");
    }

    /// The 2026-08-28 failing input, recorded from disk rather than imagined:
    /// `wikipedia-newsworthy` had `ingestion_in_progress: false` beside
    /// `indexes_built: false` — 26 data fragments, no vector index — and this
    /// surface called it `ready`. 7 of 355 local corpora were in that state.
    ///
    /// It is not `Building`: nothing is writing, so it will never resolve on
    /// its own. It is not `Ready`: every retrieval path refuses it with
    /// `UnavailabilityReason::NotBuilt`. Reporting it as either is the
    /// substitution ARCH 18.3 forbids, and it made a truthful "I cannot
    /// search this corpus" from the app look like a fabrication.
    #[test]
    fn a_stopped_ingest_that_never_built_indexes_is_not_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path();
        write_meta_with(&indexes.join("stalled"), "stalled", false, false);
        assert_eq!(
            corpus_readiness(indexes, "stalled"),
            CorpusReadiness::Unsearchable,
            "a stopped ingest with no indexes must not report ready"
        );
    }

    /// The other direction: the change must not reclassify healthy corpora.
    #[test]
    fn a_finished_ingest_is_still_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path();
        write_meta_with(&indexes.join("healthy"), "healthy", false, true);
        assert_eq!(corpus_readiness(indexes, "healthy"), CorpusReadiness::Ready);
        // And a live writer is still Building, not Unsearchable — indexes are
        // legitimately absent mid-ingest and that state resolves itself.
        write_meta_with(&indexes.join("live"), "live", true, false);
        assert_eq!(corpus_readiness(indexes, "live"), CorpusReadiness::Building);
    }
}
