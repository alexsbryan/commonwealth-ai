// SPDX-License-Identifier: AGPL-3.0-or-later
//! Corpus inventory commands — extracted from `corpus_cmd` (§3.2).
//! list / install / remove / status: the day-to-day corpus management
//! surface.

use std::path::PathBuf;

use corpus_engine::Corpus;

use super::fmt::{dir_size_bytes, format_count, human_bytes};
// The readiness decider moved to `status.rs` with `corpus status`; `--wait`
// imports it rather than keeping a second reading of "ready" (§10.6).
use super::status::{corpus_readiness, CorpusReadiness};

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

    // Read readiness BEFORE the request goes out. `--wait` polls until the
    // index is Ready, and an index that was ALREADY ready satisfies the first
    // poll — so the wait reports "ready after 0s" for an ingest that has not
    // run and may never run. Observed 2026-09-02: the probe corpus's source
    // markdown was edited, `corpus install` printed `✓ installed (ready after
    // 0s)`, and every later step read the previous build's atoms.
    let was_ready = corpus_readiness(&indexes_dir(), id) == CorpusReadiness::Ready;

    let code = submit_install_request(id, params).await;
    if code != 0 {
        return code;
    }
    match wait_secs {
        None => 0,
        Some(budget) => wait_until_installed(id, budget, was_ready).await,
    }
}

/// Where `corpus_readiness` looks. One resolution, so the pre-check and the
/// wait cannot disagree about which directory they are talking about (§10.6).
fn indexes_dir() -> PathBuf {
    sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root())
        .join("indexes")
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
///
/// `was_ready` is the same reading taken BEFORE the request was submitted.
/// When it is true, Ready on the first poll is not evidence of anything —
/// the index was already there — so this says so rather than claiming an
/// ingest it did not witness. Still exit 0: the corpus IS installed, which
/// is what the command promises. What it must not do is let an author who
/// edited their source read "✓ installed" and go on to query stale atoms.
async fn wait_until_installed(corpus_id: &str, budget_secs: u64, was_ready: bool) -> i32 {
    let indexes_dir = indexes_dir();
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
            if was_ready && start.elapsed().as_secs() == 0 {
                println!(
                    "· {corpus_id} was ALREADY installed and ready — this run \
                     did not witness an ingest."
                );
                println!(
                    "  `corpus install` is idempotent. If the source changed, \
                     `svrn corpus remove {corpus_id}` first, or you will query \
                     the previous build."
                );
            } else {
                println!(
                    "✓ installed: {corpus_id} (ready after {}s)",
                    start.elapsed().as_secs()
                );
            }
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
