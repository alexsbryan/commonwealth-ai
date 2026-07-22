// SPDX-License-Identifier: AGPL-3.0-or-later
//! Corpus partition tooling — extracted from `corpus_cmd` (§3.2).
//! pull / merge-partitions / reconstruct-manifest / migrate-to-partition
//! + the self-partition discovery helpers (shared with diagnostics).

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::{CorpusEngine, ReconstructionMethod};

use super::fmt::human_bytes;

/// `svrn corpus pull <id> [--from <peer-url>] [--expected-fingerprint <hex>]`
///
/// Stream a peer's canonical index over HTTP, validate the
/// content fingerprint, and atomically rename it into place at
/// `<index_dir>/<id>/`. Refuses if a canonical already exists at
/// the destination — the user must explicitly remove it first
/// (`svrn corpus remove <id> --canonical-only --yes`).
///
/// `--from <peer-url>` supplies the peer's mesh API base URL
/// (e.g. `http://100.64.0.2:9742`). Required for v1 — peer
/// auto-discovery from gossip lands in the auto_recover follow-
/// up commit. `--expected-fingerprint <hex>` adds a pre-flight
/// validation: the puller refuses if the peer's advertised
/// fingerprint doesn't match the expected value (used by the
/// auto-recover path to pin the source it chose from gossip).
///
/// On success, reports throughput + the fingerprint that's now
/// stamped on the local canonical. The on-disk meta carries the
/// original peer's fingerprint verbatim; the next daemon round
/// will pick the canonical up via `installed_indexes()` and
/// publish it onto our own gossip slot.
pub(super) async fn cmd_corpus_pull(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut peer_url: Option<String> = None;
    let mut expected_fingerprint: Option<String> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--from" => {
                let Some(val) = iter.next() else {
                    eprintln!("--from requires a peer URL (e.g. http://100.64.0.2:9742)");
                    return 1;
                };
                peer_url = Some(val.clone());
            }
            "--expected-fingerprint" => {
                let Some(val) = iter.next() else {
                    eprintln!("--expected-fingerprint requires a hex value");
                    return 1;
                };
                expected_fingerprint = Some(val.clone());
            }
            "--help" | "-h" => {
                println!(
                    "Usage: svrn corpus pull <corpus_id> --from <peer-url> \
                     [--expected-fingerprint <hex>]\n\n\
                     Stream a peer's canonical index over the mesh and atomically \
                     install it locally.\n\n\
                     Refuses when a canonical already exists at \
                     <data_dir>/indexes/<corpus_id>/. Run \
                     `svrn corpus remove <id> --canonical-only --yes` first.\n\n\
                     The peer URL is the mesh API base (port 9742). The \
                     X-Canonical-Fingerprint header on the response is \
                     validated against --expected-fingerprint (if given) AND \
                     against the recomputed fingerprint of the unpacked \
                     canonical. A mismatch wipes the temp dir and errors out \
                     — no partial canonical is left behind."
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
        eprintln!("Missing corpus ID. Usage: svrn corpus pull <corpus_id> --from <peer-url>");
        return 1;
    };
    let Some(peer_url) = peer_url else {
        eprintln!(
            "Missing --from <peer-url>. Auto-discovery from gossip is a \
             follow-up commit; for now pass the peer's mesh API URL \
             explicitly (e.g. http://100.64.0.2:9742)."
        );
        return 1;
    };

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        });
    let index_dir = data_dir.join("indexes");

    println!("Pulling canonical for '{corpus_id}' from {peer_url}…");
    println!("(streaming tar.zst → unpack → fingerprint validate → atomic rename)");
    println!();

    let started = std::time::Instant::now();
    // CLI path is single-target — the operator gave us one URL.
    // Wrap in a single-element slice so the function signature
    // (which loops over candidates for the auto-pull path) sees
    // exactly the one address the user wants to try.
    let candidates = vec![peer_url.clone()];
    match sovereign_mesh::canonical_pull::pull_canonical_from_peer(
        &candidates,
        &corpus_id,
        &index_dir,
        expected_fingerprint.as_deref(),
    )
    .await
    {
        Ok(report) => {
            let elapsed = started.elapsed();
            let mb_per_sec = if elapsed.as_secs_f64() > 0.0 {
                (report.bytes_uncompressed as f64 / elapsed.as_secs_f64()) / 1_048_576.0
            } else {
                0.0
            };
            println!("✓ pulled {corpus_id}");
            println!("  fingerprint:        {}", report.fingerprint);
            println!(
                "  uncompressed bytes: {}",
                human_bytes(report.bytes_uncompressed)
            );
            println!(
                "  elapsed:            {}m{}s ({:.1} MB/s uncompressed)",
                elapsed.as_secs() / 60,
                elapsed.as_secs() % 60,
                mb_per_sec,
            );
            println!("  canonical at:       {}", report.canonical_path.display());
            0
        }
        Err(e) => {
            eprintln!("✗ pull failed: {e}");
            1
        }
    }
}

/// Merge every `<corpus>-partition-*/` directory on this node into a
/// canonical `<corpus>/` index.
///
/// One-shot rescue for the stranded-partition case the daemon's
/// `corpus_collaborate` recovery path can't reach: the in-memory
/// MeshStore wipes handoff blobs on every daemon restart, so a
/// queue-mode ingest that finished its dispatch phase but never
/// finalised the merge ends up in a deadlock — every partition is on
/// disk, every shard is "claimed" across the union, but no canonical
/// exists and there's nothing to re-fire from.
///
/// What this does:
///  1. Discover all `<corpus>-partition-*/` directories under
///     `<data_dir>/indexes/`.
///  2. Preflight: every partition must agree on embedding model + dim.
///     (`merge_shards` errors otherwise; we check up front for a
///     nicer message.)
///  3. Refuse if `<corpus>/` already exists with data — never clobber.
///  4. y/N gate (or `--yes`).
///  5. Run `corpus_engine::sharding::merge_shards()` — content_hash +
///     (unit_id, source_doc_id) dedup during merge.
///  6. Stamp scope + total_shards + union'd processed_shards on the
///     canonical meta (merge_shards writes a fresh default meta, so
///     these need restoring from input partitions).
///  7. `build_indexes(true, true)` — IVF-PQ vector index + Tantivy FTS
///     on the merged chunks.
///  8. `mark_indexes_built()` + `mark_ingestion_complete()`.
///  9. Optional `--remove-partitions` deletes the partition dirs after
///     successful merge.
pub(super) async fn cmd_corpus_merge_partitions(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut yes = false;
    let mut remove_partitions = false;

    let iter = args.iter();
    for arg in iter {
        match arg.as_str() {
            "--yes" | "-y" => yes = true,
            "--remove-partitions" => remove_partitions = true,
            "--help" | "-h" => {
                println!(
                    "Usage: svrn corpus merge-partitions <corpus_id> [--yes] [--remove-partitions]\n\n\
                     Merge every <corpus>-partition-*/ dir on this node into \
                     a canonical <corpus>/ index, deduping by content_hash + \
                     (unit_id, source_doc_id) during merge. Builds vector + \
                     FTS indexes on the canonical and marks ingestion complete.\n\n\
                     Use this when:\n\
                     - The daemon logs `corpus_collaborate: queue drained but \
                     no canonical index and no local handoff found`\n\
                     - Multiple <corpus>-partition-*/ dirs exist on disk but \
                     no canonical <corpus>/ does\n\
                     - Auto-resume fires but the dispatcher returns \
                     `corpus already complete — cooling down` while the data \
                     is actually split across partitions\n\n\
                     --remove-partitions  Delete each <corpus>-partition-*/ \
                     dir AFTER the merge succeeds. Off by default — verify \
                     the canonical index serves queries first.\n\n\
                     Stop the daemon (svrn daemon stop) before running \
                     this if it's currently writing to any of the partitions \
                     (LanceDB locks are per-directory, but a peer-pulled \
                     partition can still be receiving writes from gossip)."
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
        eprintln!("Missing corpus ID. Usage: svrn corpus merge-partitions <corpus_id>");
        return 1;
    };

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        });
    let index_dir = data_dir.join("indexes");
    let canonical_path = index_dir.join(&corpus_id);

    // Refuse to clobber an existing canonical. If the user genuinely
    // wants to rebuild from partitions, they can `corpus remove` the
    // canonical first.
    if canonical_path.join("_corpus_meta.json").exists() {
        eprintln!(
            "Canonical index already exists at {}.\n\
             merge-partitions never clobbers existing canonical data. If you \
             want to rebuild from the partition dirs, remove the canonical \
             first:\n  svrn corpus remove {corpus_id}",
            canonical_path.display()
        );
        return 1;
    }

    // Discover every <corpus>-partition-*/ directory. Self-partition,
    // peer-partition, doesn't matter — we own the chunks once they're
    // on local disk, and merge_shards dedupes by content_hash so
    // overlap between partitions is collapsed automatically.
    let prefix = format!("{corpus_id}-partition-");
    let mut partitions: Vec<(PathBuf, String)> = Vec::new();
    match std::fs::read_dir(&index_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = entry.file_name();
                let Some(name_str) = name.to_str() else {
                    continue;
                };
                let Some(suffix) = name_str.strip_prefix(&prefix) else {
                    continue;
                };
                if !path.join("_corpus_meta.json").exists() {
                    continue;
                }
                partitions.push((path, suffix.to_string()));
            }
        }
        Err(e) => {
            eprintln!("Failed to scan {}: {e}", index_dir.display());
            return 1;
        }
    }
    partitions.sort_by(|a, b| a.1.cmp(&b.1));

    if partitions.is_empty() {
        eprintln!(
            "No partitions found at {}/{}-partition-* — nothing to merge.",
            index_dir.display(),
            corpus_id
        );
        return 1;
    }

    // Discovery summary: chunk counts, processed_shards, embedding
    // model. Open each partition once and reuse the handle through
    // the preflight checks.
    println!(
        "Found {} partition(s) for '{}':",
        partitions.len(),
        corpus_id
    );
    println!();

    struct PartitionSummary {
        path: PathBuf,
        embedding_model: String,
        embedding_dimensions: usize,
        total_shards: Option<usize>,
    }

    let mut summaries: Vec<PartitionSummary> = Vec::new();
    let mut union_processed: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    let mut total_chunks_input: u64 = 0;

    for (path, label) in &partitions {
        let idx = match corpus_engine::CorpusIndex::open(path).await {
            Ok(i) => i,
            Err(e) => {
                eprintln!("Failed to open partition {}: {e}", path.display());
                return 1;
            }
        };
        let info = match idx.info().await {
            Ok(i) => i,
            Err(e) => {
                eprintln!("Failed to read partition info {}: {e}", path.display());
                return 1;
            }
        };
        let processed: Vec<u64> = idx
            .processed_shards()
            .unwrap_or_default()
            .into_iter()
            .map(|n| n as u64)
            .collect();
        for s in &processed {
            union_processed.insert(*s);
        }
        // Read total_shards + scope directly from the meta JSON since
        // they're not exposed via IndexInfo. Falls back to None on any
        // parse error — fine, we'll just not stamp them on canonical.
        let raw = std::fs::read_to_string(path.join("_corpus_meta.json")).unwrap_or_default();
        let meta_v: serde_json::Value =
            serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
        let total_shards = meta_v["total_shards"].as_u64().map(|n| n as usize);

        total_chunks_input += info.chunk_count;
        println!(
            "  partition-{}: {} chunks, {}/{} shards processed{}",
            label,
            info.chunk_count,
            processed.len(),
            total_shards
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".to_string()),
            if let Some(missing_shards) = total_shards.map(|n| {
                (0..n as u64)
                    .filter(|s| !processed.iter().any(|p| p == s))
                    .collect::<Vec<_>>()
            }) {
                if missing_shards.is_empty() {
                    String::new()
                } else {
                    format!(" (missing: {missing_shards:?})")
                }
            } else {
                String::new()
            }
        );

        summaries.push(PartitionSummary {
            path: path.clone(),
            embedding_model: info.embedding_model,
            embedding_dimensions: info.embedding_dimensions,
            total_shards,
        });
    }

    // Preflight: validate embedding model + dim across every
    // partition. Mirrors merge_shards's logic exactly:
    //   - Empty embedding_model is a wildcard (peer-pull copy bug
    //     left it blank in the meta; chunks themselves are valid).
    //   - Two distinct non-empty values error out.
    //   - Dims compared strictly.
    //   - At least one non-empty model required (the canonical
    //     gets stamped with the resolved model so future query
    //     paths can pick the right embed function).
    // Doing the check here gives a clearer message before the
    // merge starts spending I/O.
    let first = &summaries[0];
    let mut resolved_model: String = first.embedding_model.clone();
    for s in summaries.iter().skip(1) {
        match (resolved_model.is_empty(), s.embedding_model.is_empty()) {
            (true, false) => {
                resolved_model = s.embedding_model.clone();
            }
            (false, false) if s.embedding_model != resolved_model => {
                eprintln!(
                    "\nEmbedding model mismatch — refusing to merge:\n  \
                     {} uses '{}'\n  resolved model so far is '{}'",
                    s.path.display(),
                    s.embedding_model,
                    resolved_model,
                );
                return 1;
            }
            _ => {}
        }
        if s.embedding_dimensions != first.embedding_dimensions {
            eprintln!(
                "\nEmbedding dimension mismatch — refusing to merge:\n  \
                 {} = {}\n  {} = {}",
                first.path.display(),
                first.embedding_dimensions,
                s.path.display(),
                s.embedding_dimensions,
            );
            return 1;
        }
    }
    if resolved_model.is_empty() {
        eprintln!(
            "\nEvery partition has an empty embedding_model — cannot stamp \
             the canonical meta with a usable model. Aborting."
        );
        return 1;
    }
    let blank_inputs: Vec<&PathBuf> = summaries
        .iter()
        .filter(|s| s.embedding_model.is_empty())
        .map(|s| &s.path)
        .collect();
    if !blank_inputs.is_empty() {
        println!();
        println!(
            "WARN: {} partition(s) have an empty embedding_model in their \
             meta. This is the peer-pull stamp bug — chunks themselves are \
             valid (the peer's actual embedder produced them). The merged \
             canonical will be stamped with '{}' (resolved from the other \
             partitions).",
            blank_inputs.len(),
            resolved_model,
        );
        for p in blank_inputs {
            println!("  - {}", p.display());
        }
    }

    // Resolve the canonical total_shards for the output meta. Priority:
    //   1. Highest total_shards stamped on any input partition (if any
    //      partition was extracted post-stamping, it's authoritative).
    //   2. max(union(processed_shards)) + 1 fallback.
    let total_shards_canonical: Option<usize> = summaries
        .iter()
        .filter_map(|s| s.total_shards)
        .max()
        .or_else(|| union_processed.iter().max().map(|m| (*m + 1) as usize));

    println!();
    println!("Merge plan:");
    println!(
        "  embedding model:  {} ({}d)",
        resolved_model, first.embedding_dimensions
    );
    println!(
        "  total chunks in:  {total_chunks_input} (across {} partitions; will dedup during merge)",
        summaries.len()
    );
    println!(
        "  processed shards: {} of {}{}",
        union_processed.len(),
        total_shards_canonical
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".to_string()),
        match total_shards_canonical {
            Some(n) => {
                let missing: Vec<u64> = (0..n as u64)
                    .filter(|s| !union_processed.contains(s))
                    .collect();
                if missing.is_empty() {
                    " (FULL COVERAGE — safe to merge)".to_string()
                } else {
                    format!(" (still missing: {missing:?} — merge will produce a partial index)")
                }
            }
            None => String::new(),
        }
    );
    println!("  output:           {}", canonical_path.display());
    if remove_partitions {
        println!(
            "  cleanup:          DELETE all {} partition dir(s) after merge succeeds",
            summaries.len()
        );
    } else {
        println!("  cleanup:          partitions left in place (re-run with --remove-partitions to delete)");
    }

    if !yes {
        eprint!("\nProceed? [y/N] ");
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

    // Hand off to the shared recovery primitive. CLI prints
    // human-readable progress at each phase boundary; the daemon's
    // auto-recover loop calls the same function with a tracing-only
    // progress callback. Keeping the merge logic in one place stops
    // the two paths from drifting.
    let merge_start = std::time::Instant::now();
    let progress_cb: std::sync::Arc<dyn Fn(corpus_engine::MergePhaseProgress) + Send + Sync> =
        std::sync::Arc::new(|phase| match phase {
            corpus_engine::MergePhaseProgress::DiscoveryComplete { partition_count } => {
                eprintln!(
                    "\n[1/3] Merging {partition_count} partition(s) (chunk copy + dedup pass)…"
                );
            }
            corpus_engine::MergePhaseProgress::MergeComplete {
                chunks_merged,
                chunks_deduped,
            } => {
                eprintln!(
                "  merged {chunks_merged} chunks ({chunks_deduped} duplicates collapsed during merge)"
            );
                eprintln!("\n[2/3] Stamping canonical metadata (scope, processed_shards, total_shards, provenance)…");
            }
            corpus_engine::MergePhaseProgress::MetaStamped => {
                eprintln!("  ✓");
                eprintln!("\n[3/3] Building search indexes (IVF-PQ + FTS)…");
                eprintln!(
                    "  this is the slow phase; on Wikipedia-scale data it can take 30+ minutes"
                );
            }
            corpus_engine::MergePhaseProgress::BuildSubPhase { done, total } => {
                if total > 0 {
                    eprintln!("  build progress: {done}/{total}");
                }
            }
            corpus_engine::MergePhaseProgress::Complete => {
                eprintln!("  ✓ canonical marked complete");
            }
        });

    let report = match corpus_engine::merge_partitions_into_canonical(
        &index_dir,
        &corpus_id,
        Some(progress_cb),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\nmerge_partitions_into_canonical failed: {e}");
            eprintln!("Canonical (if partial) is at {}.", canonical_path.display());
            eprintln!(
                "You can retry with: svrn corpus install {corpus_id}  \
                 (resume picks up the partial state)"
            );
            return 1;
        }
    };

    // ── Optional cleanup ──────────────────────────────────────────
    if remove_partitions {
        println!("\nRemoving partition directories…");
        for path in &report.partition_paths {
            match std::fs::remove_dir_all(path) {
                Ok(_) => println!("  removed {}", path.display()),
                Err(e) => eprintln!("  WARN: failed to remove {}: {e}", path.display()),
            }
        }
    }

    println!();
    println!(
        "✓ merge-partitions complete in {:.1}s.",
        merge_start.elapsed().as_secs_f64(),
    );
    println!("  canonical:        {}", report.canonical_path.display());
    println!(
        "  chunks:           {} (input {}, deduped during merge {})",
        report.chunks_merged,
        report.chunks_input,
        report.chunks_input.saturating_sub(report.chunks_merged)
    );
    println!(
        "  shards covered:   {} of {}",
        report.shard_union.len(),
        report
            .total_shards
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".to_string()),
    );
    println!(
        "  embedding model:  {} ({}d)",
        report.embedding_model, report.embedding_dimensions
    );
    println!();
    println!("Next: the daemon's installed_indexes() picks up the canonical on its next tick.");
    println!("Verify with: svrn corpus diag {corpus_id}");
    0
}

pub(super) async fn cmd_corpus_reconstruct_manifest(args: &[String]) -> i32 {
    // Parse: <corpus_id> [--source-dir <path>] [--yes]
    let mut corpus_id: Option<String> = None;
    let mut source_dir: Option<PathBuf> = None;
    let mut yes = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--source-dir" => {
                if let Some(p) = iter.next() {
                    source_dir = Some(PathBuf::from(p));
                } else {
                    eprintln!("--source-dir requires a path argument");
                    return 1;
                }
            }
            "--yes" | "-y" => yes = true,
            other if !other.starts_with('-') => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                }
            }
            other => {
                eprintln!("Unknown flag: {other}");
                eprintln!("Usage: svrn corpus reconstruct-manifest <corpus_id> [--source-dir <path>] [--yes]");
                return 1;
            }
        }
    }

    let Some(corpus_id) = corpus_id else {
        eprintln!("Missing corpus ID");
        eprintln!(
            "Usage: svrn corpus reconstruct-manifest <corpus_id> [--source-dir <path>] [--yes]"
        );
        return 1;
    };

    // Resolve the sovereign index dir: same logic as the daemon uses.
    let index_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sovereign")
        .join("indexes");

    // Build a no-op embed function — reconstruction reads metadata only.
    let noop_embed: corpus_engine::EmbedFn =
        Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; 0]) }));

    let recipes_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sovereign")
        .join("recipes");

    let engine = CorpusEngine::new(recipes_dir, index_dir, noop_embed);

    let report = match engine.reconstruct_source_manifest(&corpus_id, source_dir.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    };

    // Print report.
    let method_label = match &report.method {
        ReconstructionMethod::IterPosVerification => {
            "iter-pos verification (parquet row counts)".to_string()
        }
        ReconstructionMethod::ChunkCountHeuristic {
            median_rows_per_file,
        } => {
            format!("chunk-count heuristic (median {median_rows_per_file} rows/file)")
        }
        ReconstructionMethod::SingleFile => "single-file source (no shard splitting)".to_string(),
    };

    let total = report.manifest.files.len();
    let complete = report
        .manifest
        .files
        .iter()
        .filter(|f| matches!(f.status, corpus_engine::SourceFileStatus::Complete { .. }))
        .count();
    let in_progress = report
        .manifest
        .files
        .iter()
        .filter(|f| matches!(f.status, corpus_engine::SourceFileStatus::InProgress { .. }))
        .count();
    let pending = report
        .manifest
        .files
        .iter()
        .filter(|f| matches!(f.status, corpus_engine::SourceFileStatus::Pending))
        .count();

    println!();
    println!("Manifest reconstruction report for '{corpus_id}'");
    println!("  Method:           {method_label}");
    println!("  Files total:      {total}");
    println!("  Complete:         {complete}");
    println!("  In-progress:      {in_progress}  (reset to Pending — conservative)");
    println!("  Pending:          {pending}");
    if report.conservative_reprocessing_count > 0 {
        println!(
            "  Re-process count: {} (in-flight at crash time)",
            report.conservative_reprocessing_count
        );
    }
    if !report.warnings.is_empty() {
        println!();
        println!("Warnings:");
        for w in &report.warnings {
            println!("  - {w}");
        }
    }
    println!();

    if !yes {
        eprint!("Write manifest to index? [y/N] ");
        // Flush stderr before reading stdin.
        use std::io::Write;
        let _ = std::io::stderr().flush();
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err() {
            eprintln!("Could not read input — aborting. Use --yes to skip prompt.");
            return 1;
        }
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return 0;
        }
    }

    // The manifest has already been written by reconstruct_source_manifest().
    // Confirm path for the user.
    let index_path = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sovereign")
        .join("indexes")
        .join(&corpus_id)
        .join("_source_manifest.json");
    println!("Manifest written to: {}", index_path.display());
    println!();
    println!("Next step: svrn corpus collaborate {corpus_id}");
    println!();
    0
}

/// Migrate a pre-unified canonical index into a partition-of-self
/// dir so the daemon's auto-collaborate loop will pick it up and
/// participate in collaborative ingest alongside peers.
///
/// Before Layer 1's unified-ingest primitive, `engine.ingest()`
/// wrote directly into `<index_dir>/<corpus_id>/`. New code writes
/// into `<index_dir>/<corpus_id>-partition-<self_node_id>/` and
/// promotes to canonical via `finalise_solo_ingest` or
/// `coordinate_merge`. A user mid-ingest when they upgraded has a
/// populated canonical and no partition-of-self — so auto_ingest
/// skips spawning local work for them (`partition_path.exists()`
/// is false), and `coordinate_merge` from a peer would collide on
/// the output path.
///
/// This subcommand is the one-shot fix: it renames the canonical
/// into the partition-of-self path and rewrites the meta so the
/// new code treats it as "this node's share of a collaborative
/// ingest in progress". No data is copied; the `chunks.lance`
/// table is preserved verbatim.
pub(super) async fn cmd_corpus_migrate_to_partition(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut dry_run = false;

    let iter = args.iter();
    for arg in iter {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--help" | "-h" => {
                eprintln!(
                    "Usage: svrn corpus migrate-to-partition <corpus_id> [--dry-run]\n\
                     \n\
                     Renames ~/.sovereign/indexes/<id>/ to\n\
                     ~/.sovereign/indexes/<id>-partition-<self_node_id>/ and\n\
                     flips the meta to partition shape so the daemon's\n\
                     auto-collaborate loop will resume the ingest and\n\
                     peers can participate.\n\
                     \n\
                     The canonical must have ingestion_in_progress=true\n\
                     (otherwise there's nothing to resume). Partition-of-self\n\
                     must not already exist."
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
                eprintln!("Usage: svrn corpus migrate-to-partition <corpus_id> [--dry-run]");
                return 1;
            }
        }
    }

    let Some(corpus_id) = corpus_id else {
        eprintln!("Missing corpus ID");
        eprintln!("Usage: svrn corpus migrate-to-partition <corpus_id> [--dry-run]");
        return 1;
    };

    // Resolve data_dir from the setup config so we read mesh.json
    // + indexes from exactly the same place the running daemon does.
    // Using `mesh_data_dir()` (platform data dir) would work for a
    // Desktop-only deployment but not for CLI-daemon setups where
    // `config.data.dir` commonly points at `~/.sovereign/`.
    let config = match sovereign_core::setup_config::SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "Failed to load setup config ({e}).\n\
                 Run `svrn setup` first so the migration knows which\n\
                 data_dir your daemon uses."
            );
            return 1;
        }
    };
    let data_dir = config.data.dir.clone();

    // Load the self_node_id the daemon uses so the partition path
    // matches. Prefer the explicit `<data_dir>/node_id` file; fall
    // back to mesh.json's `self_node_id` for deployments that never
    // materialised the separate file (the common path — the daemon
    // only writes node_id when it generates a fresh one, and existing
    // meshes carry the ID inside mesh.json).
    let self_node_id = match sovereign_mesh::persist::load_node_id(&data_dir) {
        Ok(Some(id)) => id,
        _ => match sovereign_mesh::persist::load(&data_dir) {
            Ok(Some(persisted)) => persisted.self_node_id,
            Ok(None) => {
                eprintln!(
                    "No mesh state at {} — run `svrn mesh create` or\n\
                     `svrn mesh join …` before migrating a corpus so the\n\
                     daemon has a stable node id.",
                    data_dir.display()
                );
                return 1;
            }
            Err(e) => {
                eprintln!("Failed to load mesh state from {}: {e}", data_dir.display());
                return 1;
            }
        },
    };
    let self_node_id_str = self_node_id.to_string();

    let index_dir = data_dir.join("indexes");
    let canonical = index_dir.join(&corpus_id);
    let partition = index_dir.join(format!("{corpus_id}-partition-{self_node_id_str}"));

    println!();
    println!("Migration plan for '{corpus_id}':");
    println!("  Canonical : {}", canonical.display());
    println!("  Partition : {}", partition.display());
    println!("  Node id   : {self_node_id_str}");

    if dry_run {
        println!();
        println!("Dry run — no changes made. Re-run without --dry-run to apply.");
        return 0;
    }

    // Engine just needs the directories + a no-op embed for this
    // file-moving operation; ingestion won't run during migration.
    let recipes_dir = data_dir.join("recipes");
    let noop_embed: corpus_engine::EmbedFn =
        Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; 0]) }));
    let engine = CorpusEngine::new(recipes_dir, index_dir, noop_embed)
        .with_self_node_id(self_node_id_str.clone());

    match engine.migrate_canonical_to_partition(&corpus_id) {
        Ok(new_path) => {
            println!();
            println!(
                "✓ Migration complete. New partition-of-self: {}",
                new_path.display()
            );
            println!();
            println!("Next steps:");
            println!(
                "  - If the daemon is running, its auto-collaborate loop will\n\
                   pick up the partition within 30 s and resume ingest."
            );
            println!(
                "  - If the daemon is not running, start it with `svrn daemon start`\n\
                   (or reopen Sovereign Desktop)."
            );
            0
        }
        Err(e) => {
            eprintln!();
            eprintln!("Migration failed: {e}");
            1
        }
    }
}

// ── Helpers ──────────────────────────────────────────────

/// Locate `<index_dir>/<corpus_id>-partition-<self>/` when canonical
/// is absent. Returns the path and the truncated node-id label for
/// human-friendly logging.
///
/// We don't have direct access to the daemon's `self_node_id` from
/// the CLI (the cli is decoupled from any live mesh state — it can
/// run before the daemon does), so the "self" partition is
/// identified positively: scan the indexes dir for any directory
/// matching `<corpus_id>-partition-<NODE_HEX>` and prefer the one
/// where `_corpus_meta.json.indexes_built == true`. That's a
/// pragmatic stand-in for "the partition this machine actually
/// finished writing to" — peer-pulled partitions for OTHER nodes
/// have `indexes_built: false` until coordinate_merge promotes
/// them, so we don't accidentally read a peer's partial download.
pub(super) fn find_self_partition(
    index_dir: &std::path::Path,
    corpus_id: &str,
) -> Option<(PathBuf, String)> {
    let prefix = format!("{corpus_id}-partition-");
    let mut best: Option<(PathBuf, String, bool)> = None;
    let entries = std::fs::read_dir(index_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Some(suffix) = name_str.strip_prefix(&prefix) else {
            continue;
        };
        let meta_path = path.join("_corpus_meta.json");
        let Ok(content) = std::fs::read_to_string(&meta_path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        let built = meta["indexes_built"].as_bool().unwrap_or(false);
        // Prefer a built partition; if none built, fall back to any.
        match &best {
            Some((_, _, prior_built)) if *prior_built && !built => continue,
            _ => {
                best = Some((path, suffix.to_string(), built));
            }
        }
    }
    best.map(|(path, label, _)| (path, label))
}

/// Read the `processed_shards` array out of a partition's
/// `_corpus_meta.json` and produce a one-line summary.
///
/// Total-shard resolution priority:
/// 1. `--total-shards N` override (caller-supplied).
/// 2. `total_shards` field in `_corpus_meta.json` (stamped by the
///    extractor at ingest start; authoritative when present).
/// 3. `max(processed_shards) + 1` heuristic (legacy fallback;
///    silently undercounts trailing-missing shards — surface that
///    caveat in the output so operators don't trust it blindly).
///
/// Returns `None` only when there's no `processed_shards` array at
/// all (older schema or non-sharded corpus).
pub(super) fn processed_shards_summary(
    index_path: &std::path::Path,
    total_override: Option<usize>,
) -> Option<String> {
    let meta = std::fs::read_to_string(index_path.join("_corpus_meta.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&meta).ok()?;
    let processed: Vec<u64> = v["processed_shards"]
        .as_array()?
        .iter()
        .filter_map(|x| x.as_u64())
        .collect();
    if processed.is_empty() && total_override.is_none() {
        return Some("processed_shards present but empty (no shards finalized)".to_string());
    }
    let max_idx = processed.iter().copied().max().unwrap_or(0);
    let processed_set: std::collections::HashSet<u64> = processed.iter().copied().collect();

    // Resolve total shards via the priority chain.
    let total_meta = v["total_shards"].as_u64().map(|n| n as usize);
    let (total_inferred, total_source) = match (total_override, total_meta) {
        (Some(n), _) => (n, "--total-shards override"),
        (None, Some(n)) => (n, "stamped at extract start"),
        (None, None) => ((max_idx + 1) as usize, "inferred from max(processed)+1"),
    };

    let total = total_inferred as u64;
    let missing: Vec<u64> = (0..total).filter(|i| !processed_set.contains(i)).collect();

    let trailing_caveat = matches!(total_source, "inferred from max(processed)+1");

    if missing.is_empty() {
        Some(format!(
            "{} of {} shards processed (source: {total_source}; none missing)",
            processed.len(),
            total,
        ))
    } else {
        let preview: Vec<String> = missing.iter().take(8).map(|n| n.to_string()).collect();
        let suffix = if missing.len() > 8 {
            format!(" + {} more", missing.len() - 8)
        } else {
            String::new()
        };
        let caveat = if trailing_caveat {
            " (heuristic; trailing shards beyond max_idx may also be missing — \
             check daemon.log for `assigned … real shards` or pass --total-shards N)"
        } else {
            ""
        };
        Some(format!(
            "{} of {} shards processed (source: {total_source}); \
             missing: [{}]{}{caveat}",
            processed.len(),
            total,
            preview.join(", "),
            suffix,
        ))
    }
}
