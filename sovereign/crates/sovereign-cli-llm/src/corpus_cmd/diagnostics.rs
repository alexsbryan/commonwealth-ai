// SPDX-License-Identifier: AGPL-3.0-or-later
//! Corpus diagnostics + analysis commands — extracted from `corpus_cmd`
//! (§3.2). diag / dedupe / repair / stream-axes + parcel export: the
//! audit + rescue surface.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::CorpusEngine;

use super::partitions::{find_self_partition, processed_shards_summary};

/// Minimal RFC-4180 field escaping: quote when the cell contains a comma,
/// quote, or newline, doubling any embedded quote.
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// `svrn corpus export-parcels --corpus <id> [--out <path>]` — write a
/// corpus's deterministic parcel atoms to CSV so a reader can re-sum the
/// figures independently (open in Excel, sum `assessed_land_value`). This
/// is the reproducibility half of the SF-LVT "no confabulated numbers"
/// guarantee: the exact input set `parcel_analytics` folds over is the same
/// table exported here, one row per atom, carrying its source-chunk id.
pub(super) async fn cmd_corpus_export_parcels(args: &[String]) -> i32 {
    use corpus_engine::enrichment::atlas::atoms::AtomEnvelope;
    use corpus_engine::enrichment::atlas::writer::{read_atlas_atoms, ATLAS_DIRNAME};
    use corpus_engine::enrichment::pipeline::atlas::EntityType;

    let mut corpus_id = "sf-assessor-roll".to_string();
    let mut entity_type = "parcel".to_string();
    let mut out_path: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    corpus_id = v.clone();
                }
            }
            "--entity-type" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    entity_type = v.clone();
                }
            }
            "--out" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    out_path = Some(PathBuf::from(v));
                }
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: svrn corpus export-parcels --corpus <id> [--entity-type parcel] [--out <path>]"
                );
                return 0;
            }
            other => {
                eprintln!("error: unexpected argument `{other}`");
                return 2;
            }
        }
        i += 1;
    }

    let atlas_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        })
        .join("indexes")
        .join(&corpus_id)
        .join(ATLAS_DIRNAME);

    if !atlas_dir.join("atoms.json").exists() {
        eprintln!(
            "error: no atoms.json for corpus `{corpus_id}` at {} — is it ingested?",
            atlas_dir.display()
        );
        return 1;
    }
    let atoms_file = match read_atlas_atoms(&atlas_dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: read atoms.json for `{corpus_id}`: {e}");
            return 1;
        }
    };

    let parcels: Vec<_> = atoms_file
        .atoms
        .into_iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e) => match &e.entity_type {
                EntityType::Other(t) if *t == entity_type => Some(e),
                _ => None,
            },
            _ => None,
        })
        .collect();
    if parcels.is_empty() {
        eprintln!("error: corpus `{corpus_id}` has no `{entity_type}` atoms to export.");
        return 1;
    }

    // Stable column order: the sorted union of attribute keys.
    let attr_keys: Vec<String> = {
        let mut set = std::collections::BTreeSet::new();
        for p in &parcels {
            for k in p.attributes.keys() {
                set.insert(k.clone());
            }
        }
        set.into_iter().collect()
    };

    let mut header = vec![
        "atom_id".to_string(),
        "parcel_number".to_string(),
        "source_chunk".to_string(),
    ];
    header.extend(attr_keys.iter().cloned());
    let mut buf = String::new();
    buf.push_str(
        &header
            .iter()
            .map(|h| csv_escape(h))
            .collect::<Vec<_>>()
            .join(","),
    );
    buf.push('\n');

    let mut land_sum = 0.0_f64;
    for p in &parcels {
        let chunk = p
            .provenance
            .source_chunk_id
            .clone()
            .unwrap_or_else(|| p.provenance.source_doc_id.clone());
        let mut row = vec![
            csv_escape(p.id.as_str()),
            csv_escape(&p.canonical_name),
            csv_escape(&chunk),
        ];
        for k in &attr_keys {
            let cell = match p.attributes.get(k) {
                Some(serde_json::Value::Number(n)) => n.to_string(),
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            row.push(csv_escape(&cell));
        }
        buf.push_str(&row.join(","));
        buf.push('\n');
        if let Some(land) = p
            .attributes
            .get("assessed_land_value")
            .and_then(|v| v.as_f64())
        {
            if land > 0.0 {
                land_sum += land;
            }
        }
    }

    let out_path = out_path.unwrap_or_else(|| PathBuf::from(format!("{corpus_id}-parcels.csv")));
    if let Err(e) = std::fs::write(&out_path, buf) {
        eprintln!("error: write {}: {e}", out_path.display());
        return 1;
    }

    println!(
        "Exported {} `{entity_type}` rows from `{corpus_id}` → {}",
        parcels.len(),
        out_path.display()
    );
    println!("Columns: {}", header.join(", "));
    println!(
        "Cross-check: Σ assessed_land_value = ${land_sum:.2} — sum that column in your spreadsheet to match parcel_analytics."
    );
    0
}
/// — no daemon needed — and compares the article URL set against the
/// recipe's title filter. For Wikipedia (Vital Articles L5 Core scope)
/// this surfaces silent gaps caused by the resume-cursor bug where the
/// `committed_iter_pos` coordinate space shifted between runs as
/// `processed_shards` shrunk the assigned set.
///
/// `svrn corpus stream-axes` — backfill per-corpus stream-axis
/// blocks into installed `_corpus_meta.json` files.
///
/// Walks installed corpora; for each one that lacks a `stream` block
/// (or `--force`d), derives stability via
/// [`corpus_engine::stream_axes::derive_stability_from_info`] and
/// writes the block via [`corpus_engine::index::set_stream_axes`].
/// Idempotent. Move 5 Stage 2.
pub(super) async fn cmd_corpus_stream_axes(args: &[String]) -> i32 {
    let mut force = false;
    let mut filter: Option<String> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--force" => force = true,
            "--corpus" => match iter.next() {
                Some(v) => filter = Some(v.clone()),
                None => {
                    eprintln!("--corpus requires an argument");
                    return 1;
                }
            },
            "--all" => {} // default behaviour
            "--help" | "-h" => {
                println!(
                    "svrn corpus stream-axes [--corpus <id>] [--force]\n\
                    \n\
                    Backfill per-corpus stream-axis (stability) block into\n\
                    installed _corpus_meta.json files. Derives from corpus\n\
                    kind + acquire shape + parent_corpus_id.\n\
                    \n\
                    --corpus <id>   Limit to one corpus by id.\n\
                    --force         Re-derive even when a stream block already exists."
                );
                return 0;
            }
            other => {
                eprintln!("unknown flag: {other}");
                return 1;
            }
        }
    }

    let indexes_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        })
        .join("indexes");

    // Use a no-op embed fn — we only need installed_indexes(), which
    // doesn't touch embedding. The recipes_dir is irrelevant for
    // listing, so we pass a dummy path that won't be read.
    let engine = CorpusEngine::new(
        std::env::temp_dir(),
        indexes_dir.clone(),
        Arc::new(|_: &str| {
            Box::pin(async move { Ok::<Vec<f32>, corpus_engine::Error>(Vec::new()) })
        }),
    );

    let indexes = match engine.installed_indexes().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: list indexes: {e}");
            return 1;
        }
    };

    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    println!("{:<32} {:<10} {:<10} from", "corpus", "stability", "source");
    println!("{}", "─".repeat(96));

    for info in indexes {
        if let Some(f) = &filter {
            if info.corpus_id != *f {
                continue;
            }
        }
        let existing = info.stream.clone();
        if let Some(ex) = existing.as_ref() {
            if !force {
                println!(
                    "{:<32} {:<10} {:<10} {}",
                    info.corpus_id,
                    ex.stability.as_str(),
                    "(existing)",
                    ex.from_signal,
                );
                skipped += 1;
                continue;
            }
        }
        let (stability, from_signal) =
            corpus_engine::stream_axes::derive_stability_from_info(&info);
        let axes = corpus_engine::stream_axes::StreamAxes {
            stability,
            source: corpus_engine::stream_axes::StreamAxesSource::Backfill,
            derived_at: corpus_engine::stream_axes::timestamp_now(),
            from_signal: from_signal.clone(),
        };
        match corpus_engine::index::set_stream_axes(&info.path, axes.clone()) {
            Ok(()) => {
                println!(
                    "{:<32} {:<10} {:<10} {}",
                    info.corpus_id,
                    stability.as_str(),
                    "backfill",
                    from_signal
                );
                written += 1;
            }
            Err(e) => {
                eprintln!("  ✗ {}: {e}", info.corpus_id);
                errors += 1;
            }
        }
    }

    println!();
    println!(
        "Summary: {} written · {} skipped (existing) · {} errors",
        written, skipped, errors
    );
    if errors > 0 {
        1
    } else {
        0
    }
}

/// Output: distinct articles in index, expected from filter, missing
/// titles count, plus a sample of up to 10 missing titles for spot-
/// checking. Non-zero exit when the gap exceeds 1% of the filter
/// expected size, so this is wireable into a CI / preflight check.
pub(super) async fn cmd_corpus_diag(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut titles_file: Option<PathBuf> = None;
    let mut sample_size: usize = 10;
    let mut check_duplicates = false;
    let mut total_shards_override: Option<usize> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--titles-file" => {
                if let Some(p) = iter.next() {
                    titles_file = Some(PathBuf::from(p));
                } else {
                    eprintln!("--titles-file requires a path argument");
                    return 1;
                }
            }
            "--sample" => {
                if let Some(n) = iter.next() {
                    match n.parse::<usize>() {
                        Ok(v) => sample_size = v,
                        Err(_) => {
                            eprintln!("--sample requires a non-negative integer");
                            return 1;
                        }
                    }
                } else {
                    eprintln!("--sample requires an integer argument");
                    return 1;
                }
            }
            "--check-duplicates" => check_duplicates = true,
            "--total-shards" => {
                if let Some(n) = iter.next() {
                    match n.parse::<usize>() {
                        Ok(v) => total_shards_override = Some(v),
                        Err(_) => {
                            eprintln!("--total-shards requires a non-negative integer");
                            return 1;
                        }
                    }
                } else {
                    eprintln!("--total-shards requires an integer argument");
                    return 1;
                }
            }
            "--help" | "-h" => {
                println!(
                    "Usage: svrn corpus diag <corpus_id> \
                     [--titles-file <path>] [--sample <n>] [--check-duplicates] \
                     [--total-shards <n>]\n\n\
                     Audit a corpus index against its filter title list. \
                     For wikipedia, --titles-file defaults to the bundled \
                     Vital Articles Level 5 list.\n\n\
                     --check-duplicates scans every chunk's content_hash to \
                     detect re-embedding (wasted work if a resume rewound \
                     past already-written rows). ~650MB transient RAM for a \
                     4M-chunk corpus.\n\n\
                     --total-shards overrides the meta-stored / inferred \
                     shard count when computing the missing-shards list. \
                     Useful for legacy indexes that pre-date the \
                     total_shards meta field."
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
        eprintln!("Missing corpus ID. Usage: svrn corpus diag <corpus_id>");
        return 1;
    };

    // Resolve the same indexes dir the daemon uses: read
    // `~/.sovereign/config.toml`'s `[data] dir` if present,
    // fall back to `~/.sovereign`. Diag is a read-only command so a
    // mis-resolution is recoverable by passing --titles-file later;
    // we still want it to "just work" against the live install
    // without operator config.
    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        });
    let index_dir = data_dir.join("indexes");

    // Resolve where this corpus actually lives. Three shapes are
    // valid in the wild:
    //
    // 1. **Canonical** — `<index_dir>/<corpus_id>/`. Produced by
    //    `coordinate_merge` after a queue-mode ingest finishes; this
    //    is the form peers fan-out queries to.
    // 2. **Self-partition** — `<index_dir>/<corpus_id>-partition-<self>/`.
    //    Active during ingest, and also the *terminal* state when a
    //    solo-node ingest never advances to merge (e.g. wikipedia
    //    here: 31/38 shards processed, indexes built, but no merge
    //    yet because the merge step waits on all-units-complete).
    // 3. **Peer-partition** — `<index_dir>/<corpus_id>-partition-<peer>/`.
    //    Foreign data the local node should not introspect.
    //
    // For diag we accept (1) and (2) via the file-system scan;
    // (3) is excluded by the `partition-<self>` suffix match. If
    // both exist we prefer canonical because it represents the
    // merged final state.
    let canonical_path = index_dir.join(&corpus_id);
    let (index_path, surface_label) = if canonical_path.exists() {
        (canonical_path, "canonical".to_string())
    } else if let Some((partition_path, node_id_label)) =
        find_self_partition(&index_dir, &corpus_id)
    {
        eprintln!(
            "  note: canonical `{corpus_id}/` is absent — diag is reading the self-partition\n  \
             at `{}/`. The partition contains everything ingested so far on this node;\n  \
             merging it into the canonical path is what peers (and `mesh_corpus.installed`)\n  \
             ultimately consume.\n",
            partition_path.display()
        );
        (partition_path, format!("partition-{node_id_label}"))
    } else {
        eprintln!(
            "Index not found at {} (and no self-partition either).\n  \
             Has this corpus been installed?",
            canonical_path.display()
        );
        return 1;
    };

    println!(
        "Opening index at {} ({}) …",
        index_path.display(),
        surface_label
    );

    // If we're reading a partition, surface the processed-shards gap
    // up front. The whole point of diag is to answer "is this corpus
    // complete?" — the partition's `_corpus_meta.json` already tracks
    // this so we don't have to wait for the title-list comparison
    // below to discover an obvious gap.
    if surface_label.starts_with("partition-") {
        if let Some(shard_summary) = processed_shards_summary(&index_path, total_shards_override) {
            println!("  shard coverage: {shard_summary}");
        }
    }

    let index = match corpus_engine::CorpusIndex::open(&index_path).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to open corpus index: {e}");
            return 1;
        }
    };

    let chunk_count = index.chunk_count().await.unwrap_or(0);
    println!("  chunks in table: {chunk_count}");

    println!("Scanning distinct source_doc_ids (this reads the full URL column)…");
    let indexed_ids = match index.list_indexed_source_doc_ids().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to list distinct source_doc_ids: {e}");
            return 1;
        }
    };

    // The Wikipedia extractor emits one ExtractedDoc per article
    // SECTION, not per article. So distinct source_doc_id URLs count
    // sections (and section URLs may include `#fragment` suffixes
    // from the streaming chunker). Strip the URL down to a normalized
    // article title so the comparison against `vital_articles_l5` is
    // honest — and report both numbers so an operator can spot the
    // distinction.
    let mut indexed_titles: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(indexed_ids.len());
    for id in &indexed_ids {
        let title = corpus_engine::extractors::wikipedia_types::wiki_title_from_url(id)
            .unwrap_or_else(|| id.clone());
        indexed_titles.insert(corpus_engine::filters::normalize_title(&title));
    }
    println!(
        "  distinct source_doc_id URLs (sections + fragments): {}",
        indexed_ids.len()
    );
    println!(
        "  distinct articles after url→title normalize:        {}",
        indexed_titles.len()
    );
    if !indexed_ids.is_empty() && !indexed_titles.is_empty() {
        let ratio = indexed_ids.len() as f64 / indexed_titles.len() as f64;
        println!(
            "  avg sections per article: {ratio:.1} \
             (Wikipedia-typical: 5–20, anomalously high suggests duplicate ingest)"
        );
    }
    if chunk_count > 0 && !indexed_ids.is_empty() {
        let cps = chunk_count as f64 / indexed_ids.len() as f64;
        println!(
            "  avg chunks per section:   {cps:.2} \
             (paragraph-chunked at 1024 chars; expect 1–10)"
        );
    }

    if check_duplicates {
        println!("Counting distinct content_hashes (this scans every chunk row)…");
        match index.count_distinct_content_hashes().await {
            Ok((distinct, with_hash, total)) => {
                println!("  total chunks:             {total}");
                println!("  with content_hash set:    {with_hash}");
                println!("  distinct content_hashes:  {distinct}");
                let hashless = total.saturating_sub(with_hash);
                if hashless > 0 {
                    println!(
                        "  hashless (legacy) rows:   {hashless} \
                         (predates content_hash population; cannot dedup-check these)"
                    );
                }
                if with_hash > 0 {
                    let dup = with_hash.saturating_sub(distinct);
                    if dup == 0 {
                        println!(
                            "  ✓ no duplicate chunks detected — embed-once invariant holds \
                             across the {with_hash} hashed rows."
                        );
                    } else {
                        let pct = dup as f64 / with_hash as f64 * 100.0;
                        println!(
                            "  ⚠ {dup} duplicate chunk rows ({pct:.2}% of hashed rows) — \
                             some chunks were embedded more than once. Likely cause: \
                             resume rewound the cursor past already-written rows."
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("  failed to count distinct content_hashes: {e}");
            }
        }
    }

    // Decide which title list to compare against. For wikipedia we
    // default to the bundled VITAL_ARTICLES_L5; --titles-file overrides.
    let (expected_titles, source_label) = match (titles_file.as_deref(), corpus_id.as_str()) {
        (Some(path), _) => {
            let bytes = match std::fs::read(path) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("Failed to read --titles-file {}: {e}", path.display());
                    return 1;
                }
            };
            (load_title_set(&bytes), format!("{}", path.display()))
        }
        (None, "wikipedia") => (
            load_title_set(corpus_engine::filters::assets::VITAL_ARTICLES_L5),
            "bundled vital_articles_l5".to_string(),
        ),
        (None, _) => {
            println!(
                "\nNo title list specified and no default for corpus '{corpus_id}'. \
                 Pass --titles-file to compare against an expected set."
            );
            return 0;
        }
    };

    let expected_count = expected_titles.len();
    let intersect = indexed_titles.intersection(&expected_titles).count();
    let missing: Vec<&String> = expected_titles.difference(&indexed_titles).collect();
    let unexpected: Vec<&String> = indexed_titles.difference(&expected_titles).collect();

    println!("\nFilter list: {source_label}");
    println!("  titles in list:           {expected_count}");
    println!("  in list ∩ in index:       {intersect}");
    println!(
        "  in list, missing in index: {} ({:.2}%)",
        missing.len(),
        if expected_count > 0 {
            100.0 * missing.len() as f64 / expected_count as f64
        } else {
            0.0
        }
    );
    println!(
        "  in index, not in list:    {} (likely redirect / normalisation drift)",
        unexpected.len()
    );

    if sample_size > 0 && !missing.is_empty() {
        println!("\nSample of missing titles (up to {sample_size}):");
        let mut sorted_missing: Vec<&String> = missing.to_vec();
        sorted_missing.sort();
        for t in sorted_missing.iter().take(sample_size) {
            println!("  • {t}");
        }
    }
    if sample_size > 0 && !unexpected.is_empty() {
        println!("\nSample of unexpected titles (up to {sample_size}):");
        let mut sorted_unexpected: Vec<&String> = unexpected.to_vec();
        sorted_unexpected.sort();
        for t in sorted_unexpected.iter().take(sample_size) {
            println!("  • {t}");
        }
    }

    // Exit non-zero if the gap is material. 1% threshold is arbitrary
    // but above the noise floor for L5 normalization quirks (a few
    // dozen titles shift between curator pulls).
    let gap_pct = if expected_count > 0 {
        100.0 * missing.len() as f64 / expected_count as f64
    } else {
        0.0
    };
    if gap_pct > 1.0 {
        eprintln!(
            "\n⚠ Material gap detected: {} titles missing ({:.2}%). \
             This may indicate the resume-cursor coordinate-space bug \
             — see plan to re-ingest with shard-set-drift fix.",
            missing.len(),
            gap_pct
        );
        return 2;
    }

    0
}

/// Parse a newline-delimited title list (the same format
/// `TitleListFilter::from_bytes` accepts) into a normalized
/// `HashSet<String>`. Comments (`#…`) and blank lines are skipped.
fn load_title_set(bytes: &[u8]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for line in bytes.split(|&b| b == b'\n') {
        let line = std::str::from_utf8(line).unwrap_or("").trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        out.insert(corpus_engine::filters::normalize_title(line));
    }
    out
}

/// `svrn corpus dedupe <corpus_id> [--yes]`
///
/// Run the one-shot rescue pass on an installed corpus: collapse
/// duplicate-content rows (same `content_hash`) so the index reflects
/// actual unique work. The cause this exists for: a resume-cursor-
/// rewind bug that re-embedded already-written content during a
/// long-running ingest, leaving up to ~65% of chunks as exact
/// duplicates of older rows. Reclaims disk and unblocks the
/// subsequent `build_indexes()` (which now runs a dedupe prelude
/// automatically — this command exists for partitions that already
/// completed their build over duplicated data, before the auto-dedup
/// landed).
///
/// Resolves both canonical and self-partition paths (mirrors
/// `corpus diag`'s resolution). Prints before/after counts and
/// duplication rate. `--yes` skips the y/N confirmation; default is
/// to confirm because the operation deletes rows.
pub(super) async fn cmd_corpus_dedupe(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut yes = false;

    let iter = args.iter();
    for arg in iter {
        match arg.as_str() {
            "--yes" | "-y" => yes = true,
            "--help" | "-h" => {
                println!(
                    "Usage: svrn corpus dedupe <corpus_id> [--yes]\n\n\
                     Collapse duplicate-content rows in an installed corpus. \
                     Detected via the chunk's content_hash. Hashless legacy \
                     rows are preserved (no signal to compare). Resolves \
                     both canonical (<index_dir>/<corpus>/) and self-\
                     partition (<index_dir>/<corpus>-partition-<self>/) \
                     paths."
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
        eprintln!("Missing corpus ID. Usage: svrn corpus dedupe <corpus_id>");
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

    // Same resolution as diag — canonical first, then self-partition.
    let canonical_path = index_dir.join(&corpus_id);
    let (index_path, surface_label) = if canonical_path.exists() {
        (canonical_path, "canonical".to_string())
    } else if let Some((partition_path, node_id_label)) =
        find_self_partition(&index_dir, &corpus_id)
    {
        (partition_path, format!("partition-{node_id_label}"))
    } else {
        eprintln!(
            "Index not found at {} (and no self-partition either).",
            canonical_path.display()
        );
        return 1;
    };

    println!(
        "Opening index at {} ({})…",
        index_path.display(),
        surface_label
    );
    let index = match corpus_engine::CorpusIndex::open(&index_path).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to open corpus index: {e}");
            return 1;
        }
    };

    // Show the user what we're about to do BEFORE the destructive
    // call. The count_distinct_content_hashes scan is the same one
    // dedupe runs internally, but cheap enough to repeat — the
    // delete pass is the load-bearing part.
    println!("Scanning content_hashes (full table read)…");
    let (distinct, with_hash, total) = match index.count_distinct_content_hashes().await {
        Ok(triple) => triple,
        Err(e) => {
            eprintln!("Failed to count content_hashes: {e}");
            return 1;
        }
    };
    let dup_rows = with_hash.saturating_sub(distinct);
    let dup_pct = if with_hash > 0 {
        dup_rows as f64 / with_hash as f64 * 100.0
    } else {
        0.0
    };
    println!("  total chunks:             {total}");
    println!("  with content_hash set:    {with_hash}");
    println!("  distinct content_hashes:  {distinct}");
    println!("  duplicates to delete:     {dup_rows} ({dup_pct:.2}% of hashed)");

    if dup_rows == 0 {
        println!("\n✓ Nothing to do — index already deduped.");
        return 0;
    }

    if !yes {
        eprint!(
            "\nAbout to delete {dup_rows} duplicate row(s) from {}.\n\
             Existing chunk_ids will be preserved for the surviving (lowest-id) \
             row in each group. Vector + FTS indexes remain valid.\n\
             Proceed? [y/N] ",
            index_path.display()
        );
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

    println!("\nRunning dedupe…");
    match index.dedupe_by_content_hash().await {
        Ok(report) => {
            println!("  rows before:              {}", report.rows_before);
            println!("  rows after:               {}", report.rows_after);
            println!("  duplicates deleted:       {}", report.duplicates_deleted);
            println!("  unique hashes preserved:  {}", report.unique_hashes_kept);
            println!(
                "  hashless rows preserved:  {}",
                report.hashless_rows_preserved
            );
            println!(
                "\n✓ Dedupe complete ({:.2}% duplication eliminated).",
                report.dup_fraction() * 100.0
            );
            0
        }
        Err(e) => {
            eprintln!("Dedupe failed: {e}");
            1
        }
    }
}

/// Reset a "completed" partition's meta back to in-progress so the
/// daemon's auto-resume / a fresh `corpus install` picks it up.
///
/// Why this exists: the resume-cursor-rewind bug we fought during the
/// wikipedia ingest could leave a partition with `indexes_built=true`,
/// `ingestion_in_progress=false`, and missing shards in
/// `processed_shards`. The system then considers the corpus DONE — even
/// though shards never made it through — and no automated path will
/// retry them.
///
/// This command makes the surgery explicit and reversible:
///   1. Resolve canonical or self-partition path.
///   2. Read meta. Show the user which shards are missing (vs.
///      `total_shards` if stamped, otherwise vs. trailing-shard
///      heuristic).
///   3. Show the flag transitions that will happen.
///   4. y/N confirm (or `--yes`).
///   5. Apply: `reset_for_resume()` flips the four `*_built` flags +
///      `ingestion_in_progress`. `set_provenance(SelfInitiated)`
///      flips PeerPulled → SelfInitiated so auto-resume actually
///      acts on it.
///
/// The embed-side dedup gate (loaded at ingest start from
/// `list_indexed_content_hashes`) makes resuming safe — already-
/// embedded content is skipped, so only the genuinely missing shards
/// do work.
pub(super) async fn cmd_corpus_repair(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut yes = false;
    let mut total_shards_override: Option<usize> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--yes" | "-y" => yes = true,
            "--total-shards" => {
                let Some(val) = iter.next() else {
                    eprintln!("--total-shards requires a value");
                    return 1;
                };
                match val.parse::<usize>() {
                    Ok(n) => total_shards_override = Some(n),
                    Err(_) => {
                        eprintln!("--total-shards value must be a non-negative integer");
                        return 1;
                    }
                }
            }
            "--help" | "-h" => {
                println!(
                    "Usage: svrn corpus repair <corpus_id> [--yes] [--total-shards N]\n\n\
                     Reset a partition that completed with missing shards \
                     back to in-progress, so the daemon's auto-resume or a \
                     subsequent `svrn corpus install` picks it up.\n\n\
                     Specifically:\n\
                     - Clears indexes_built / vector_index_built / \
                     content_fts_built / title_fts_built\n\
                     - Sets ingestion_in_progress = true\n\
                     - Stamps provenance = self_initiated (auto-resume \
                     skips peer_pulled)\n\n\
                     --total-shards N  Override the missing-shards display \
                     when meta.total_shards isn't stamped (older partitions). \
                     The surgery itself doesn't depend on this — the next \
                     ingest will discover and stamp the true count.\n\n\
                     Committed data (chunks, processed_shards, \
                     committed_iter_pos) is left untouched. The embed-\
                     side dedup gate prevents re-embedding any \
                     content_hash already on disk."
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
        eprintln!("Missing corpus ID. Usage: svrn corpus repair <corpus_id>");
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

    // Same resolution as diag/dedupe — canonical first, then
    // self-partition. Never touch peer partitions.
    let canonical_path = index_dir.join(&corpus_id);
    let (index_path, surface_label) = if canonical_path.exists() {
        (canonical_path, "canonical".to_string())
    } else if let Some((partition_path, node_id_label)) =
        find_self_partition(&index_dir, &corpus_id)
    {
        (partition_path, format!("partition-{node_id_label}"))
    } else {
        eprintln!(
            "Index not found at {} (and no self-partition either).",
            canonical_path.display()
        );
        return 1;
    };

    println!(
        "Resolved index: {} ({})",
        index_path.display(),
        surface_label
    );

    // Read the raw meta so we can show the user the exact diff.
    let meta_path = index_path.join("_corpus_meta.json");
    let raw = match std::fs::read_to_string(&meta_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read {}: {e}", meta_path.display());
            return 1;
        }
    };
    let meta_json: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Failed to parse meta JSON: {e}");
            return 1;
        }
    };

    let processed: Vec<u64> = meta_json["processed_shards"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
        .unwrap_or_default();
    let total_shards = meta_json["total_shards"].as_u64().map(|n| n as usize);
    let provenance = meta_json["provenance"].as_str().unwrap_or("self_initiated");

    let indexes_built = meta_json["indexes_built"].as_bool().unwrap_or(false);
    let vector_built = meta_json["vector_index_built"].as_bool().unwrap_or(false);
    let content_fts = meta_json["content_fts_built"].as_bool().unwrap_or(false);
    let title_fts = meta_json["title_fts_built"].as_bool().unwrap_or(false);
    let in_progress = meta_json["ingestion_in_progress"]
        .as_bool()
        .unwrap_or(false);

    // Compute missing shards. If total_shards isn't stamped, fall back
    // to "trailing shard from max(processed)+1" — same heuristic as
    // diag, with the same caveat (may undercount if the trailing shard
    // never started).
    let processed_set: std::collections::BTreeSet<u64> = processed.iter().copied().collect();
    // Priority chain matches diag: --total-shards override > meta-stamped >
    // legacy heuristic. Older partitions written before the total_shards
    // field landed need the override or they'll undercount trailing
    // missing shards (max(processed)+1 misses anything beyond max).
    let (total_for_display, missing): (String, Vec<u64>) = if let Some(n) = total_shards_override {
        let missing: Vec<u64> = (0..n as u64)
            .filter(|s| !processed_set.contains(s))
            .collect();
        (format!("{n} (--total-shards override)"), missing)
    } else if let Some(n) = total_shards {
        let missing: Vec<u64> = (0..n as u64)
            .filter(|s| !processed_set.contains(s))
            .collect();
        (format!("{n} (from meta.total_shards)"), missing)
    } else {
        let max_seen = processed.iter().max().copied().unwrap_or(0);
        let inferred_total = max_seen + 1;
        let missing: Vec<u64> = (0..inferred_total)
            .filter(|s| !processed_set.contains(s))
            .collect();
        (
            format!("{inferred_total} (heuristic: max(processed)+1)"),
            missing,
        )
    };

    println!();
    println!("Current state:");
    println!("  ingestion_in_progress:    {in_progress}");
    println!("  indexes_built:            {indexes_built}");
    println!("  vector_index_built:       {vector_built}");
    println!("  content_fts_built:        {content_fts}");
    println!("  title_fts_built:          {title_fts}");
    println!("  provenance:               {provenance}");
    println!(
        "  processed shards:         {} of {}",
        processed.len(),
        total_for_display
    );
    if !missing.is_empty() {
        println!("  missing shards:           {missing:?}");
    }

    // Decide whether there's anything to do.
    let needs_flag_reset =
        indexes_built || vector_built || content_fts || title_fts || !in_progress;
    let needs_provenance_flip = provenance == "peer_pulled";

    if !needs_flag_reset && !needs_provenance_flip && missing.is_empty() {
        println!("\n✓ Nothing to do — partition is already in a resumable state.");
        return 0;
    }
    if !needs_flag_reset && !needs_provenance_flip {
        println!(
            "\nMeta flags already say in-progress, but {} shards are missing.",
            missing.len()
        );
        println!("No reset needed — auto-resume / install should already pick this up.");
        return 0;
    }

    println!();
    println!("Will apply:");
    if needs_flag_reset {
        println!("  ingestion_in_progress: {in_progress} → true");
        if indexes_built {
            println!("  indexes_built:         true → false");
        }
        if vector_built {
            println!("  vector_index_built:    true → false");
        }
        if content_fts {
            println!("  content_fts_built:     true → false");
        }
        if title_fts {
            println!("  title_fts_built:       true → false");
        }
    }
    if needs_provenance_flip {
        println!("  provenance:            peer_pulled → self_initiated");
    }

    if missing.is_empty() {
        println!();
        println!(
            "Heads up: no shards appear missing. Repair will still flip the \
             flags above so a future ingest treats this corpus as work-needed, \
             but resume will short-circuit if there's truly nothing to do."
        );
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

    // Open the index to use the typed helpers. `reset_for_resume`
    // round-trips through serde so any unknown fields in the meta are
    // preserved (it reads → mutates → writes the typed struct).
    println!("\nOpening index…");
    let index = match corpus_engine::CorpusIndex::open(&index_path).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to open corpus index: {e}");
            return 1;
        }
    };

    if needs_flag_reset {
        if let Err(e) = index.reset_for_resume() {
            eprintln!("Failed to reset built/in-progress flags: {e}");
            return 1;
        }
        println!("  flags reset ✓");
    }

    if needs_provenance_flip {
        if let Err(e) = corpus_engine::set_provenance(
            &index_path,
            corpus_engine::CorpusProvenance::SelfInitiated,
        ) {
            eprintln!("Failed to flip provenance: {e}");
            return 1;
        }
        println!("  provenance: self_initiated ✓");
    }

    println!("\n✓ Repair complete.");
    println!();
    println!("Next steps:");
    println!("  - The daemon's auto-resume loop will pick this up on its next tick.");
    println!("  - Or run `svrn corpus install {corpus_id}` to kick off resume now.");
    println!(
        "  - Either path will skip already-embedded content_hashes via the embed-side dedup gate."
    );
    0
}
