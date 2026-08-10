// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich raptor <corpus>` — retrofit an already-installed
//! corpus with a RAPTOR tier-3 summary tree: one tree per source
//! document, persisted into `conv_raptor_nodes` keyed
//! `(corpus_id, source_doc_id)`.
//!
//! ## Why this exists
//!
//! Chunk-RAG answers "find the passages near this query" — it samples
//! for cosine similarity, not coverage. A "summarize <work>" query
//! needs the *whole document*, which top-k can't give: the answer
//! (a definition, an argument's arc) is a global property no single
//! chunk encodes. A per-document RAPTOR tree's root node *is* that
//! document's own summary. This verb builds those trees over a corpus
//! that already shipped (e.g. SEP), so summarization has something
//! whole to reach for.
//!
//! ## Additive by construction
//!
//! The RAPTOR tree lives in the SQLite state store (`conv_raptor_nodes`);
//! the corpus's atom-graph atlas lives on disk (`atlas/atoms.json`).
//! Different storage, no overlap — this never touches the atom graph.
//! It reuses the existing leaf embeddings from `chunks.lance` (no
//! re-chunk, no re-embed of leaves); only the generated summary nodes
//! are embedded.
//!
//! It drives the same per-document builder the watched-folder path uses
//! ([`FolderTieredProvider`] + `enrich_conversation`), so it inherits
//! that path's per-document checkpointing (resumable — a re-run skips
//! the LLM work for trees already built), `_enrichment_state.json`
//! stamping, and motif extraction. The only difference is the cue:
//! `--doc-type argument` asks the summarizer for claim-level summaries,
//! the right shape for SEP's philosophy essays.
//!
//! ## Furniture filtering
//!
//! SEP entries publish through a fixed template whose copyright / contact
//! / navigation blocks get chunked alongside the prose. Left in, RAPTOR
//! clusters them and wastes a summary producing "this is just metadata"
//! — which would also pollute a whole-document summary. `--strip-furniture`
//! drops those chunks before clustering (at the CHUNK level, because
//! k-means sometimes mixes a content sentence into a furniture cluster —
//! dropping nodes would lose content). `--inspect-furniture` prints
//! exactly what would be dropped, no writes, so the filter can be
//! eyeballed before committing.
//!
//! ## Glassbox
//!
//! Every document prints a one-line record (chunks · bucket · nodes ·
//! wall-time); the run ends with a totals summary. `--dry-run` prints
//! the per-bucket dispatch plan with no inference and no writes, so the
//! cost is legible before committing to the full pass. `--limit N`
//! builds only the N smallest documents — a fast spike to validate the
//! pipeline before the long tail.

use std::sync::Arc;
use std::time::Instant;

use corpus_engine::enrichment::tiered::ConvBucket;
use corpus_engine::index::CorpusIndex;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::DocumentTypeTag;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::conv_tiered_provider::{
    FolderTieredProvider, IndexDirResolver, StaticIndexDirResolver,
};

use crate::chat_cmd::bootstrap::SplitInferenceProvider;
use sovereign_cli_shared::help;

/// Parsed `enrich raptor` invocation.
struct RaptorArgs {
    corpus_id: String,
    doc_type: DocumentTypeTag,
    limit: Option<usize>,
    dry_run: bool,
    strip_furniture: bool,
    inspect_furniture: bool,
    force: bool,
    /// Rebuild ONLY documents whose stored trees are stale — any
    /// summary-bearing node with a `prompt_version` other than the
    /// current `RAPTOR_PROMPT_VERSION`, or a `summarizer_model`
    /// other than the stem the configured chat model resolves to
    /// (pre-stamping rows have both empty → stale by definition).
    /// Fresh documents are skipped; missing ones are built (T1 P1.3).
    refresh_stale: bool,
    /// How summaries are produced (T1 P1.1): `abstractive` (default,
    /// LLM prose with per-cluster extractive fallback on failure) or
    /// `extractive` (LLM-free verbatim sentence selection).
    summary_mode: sovereign_tools::raptor_atlas::SummaryMode,
    /// T1 P1.2 verification policy override for abstractive builds
    /// (`on` | `off` | `sample:<p>`). `None` = SP3-adaptive default.
    verify_summaries: Option<sovereign_tools::summary_verify::VerifyPolicy>,
    /// Restrict the build to a curated set of articles (one slug/title per
    /// line). Overrides the default smallest-first selection — used to pilot
    /// RAPTOR on representative multi-section articles instead of stubs.
    titles_file: Option<String>,
    /// Wikipedia keys chunks per-section (`…/Article#Section`); this merges an
    /// article's sections (strip the `#anchor`) into one document so RAPTOR
    /// trees span the whole article instead of single sections.
    group_by_article: bool,
    daemon_base: String,
    chat_model: String,
    embed_model: String,
}

pub async fn cmd_raptor(args: &[String]) -> i32 {
    if help::wants_help(args) {
        print_usage();
        return 0;
    }
    let mut parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}\n");
            print_usage();
            return 2;
        }
    };

    // Resolve paths exactly as the daemon does: `data_dir` owns BOTH the
    // state DB (`sovereign.db`) and the corpus indexes dir. Matching the
    // daemon's derivation (daemon_cmd.rs) is what guarantees we augment
    // the same store the daemon serves retrieval from.
    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root());
    let indexes_dir = data_dir.join("indexes");
    let db_path = data_dir.join("sovereign.db");
    // Accept a display name or unique fragment, not just the raw id —
    // ids carry a hash suffix nobody should have to type.
    match crate::corpus_resolve::resolve_corpus_id(&indexes_dir, &parsed.corpus_id) {
        Ok(id) => {
            if id != parsed.corpus_id {
                println!("Corpus '{}' resolved to '{id}'", parsed.corpus_id);
            }
            parsed.corpus_id = id;
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            return 1;
        }
    }
    let index_path = indexes_dir.join(&parsed.corpus_id);

    if !index_path.exists() {
        eprintln!(
            "error: corpus '{}' is not installed at {}",
            parsed.corpus_id,
            index_path.display()
        );
        return 1;
    }

    // Open the index and compute the per-document dispatch plan BEFORE
    // touching inference, so `--dry-run` is free and a bad corpus fails
    // fast with a clear message.
    let index = match CorpusIndex::open(&index_path).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: open index {}: {e}", index_path.display());
            return 1;
        }
    };
    let groups = match index.group_chunks_by_source_doc().await {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: group chunks by source doc: {e}");
            return 1;
        }
    };
    // Vault / watched-folder corpora are per-FILE units: a 3-chunk
    // note is a complete essay, so it takes `classify_note` (Tiny only
    // at 0-1 chunks) — matching `run_folder_tiered_enrichment`.
    // Document corpora (wiki/SEP retrofits) keep `classify`'s 8-chunk
    // Tiny floor: lowering it there would silently multiply LLM spend
    // across thousands of small articles on a retrofit re-run.
    let per_file_units = index
        .display()
        .and_then(|d| d.category)
        .map(|c| c == "vault" || c == "watched_folder")
        .unwrap_or(false);
    let classify_bucket: fn(usize) -> ConvBucket = if per_file_units {
        ConvBucket::classify_note
    } else {
        ConvBucket::classify
    };
    if groups.is_empty() {
        eprintln!(
            "error: corpus '{}' has no source documents to summarize",
            parsed.corpus_id
        );
        return 1;
    }

    // Per-article grouping (--group-by-article): Wikipedia keys chunks by
    // per-section source_doc_id (`…/Albert_Einstein#General_relativity`), so
    // grouping by source_doc_id alone yields tiny per-section trees. Merge all
    // of an article's sections (strip the `#…` anchor) into one document so a
    // RAPTOR tree spans the whole article. `article_sections` keeps the section
    // source_doc_ids so the build loop fetches + concatenates them.
    let mut article_sections: Option<std::collections::HashMap<String, Vec<String>>> = None;
    let mut docs: Vec<(String, usize)> = if parsed.group_by_article {
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut sections: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for (sdi, chunk_ids) in &groups {
            let article = sdi.split('#').next().unwrap_or(sdi).to_string();
            *counts.entry(article.clone()).or_default() += chunk_ids.len();
            sections.entry(article).or_default().push(sdi.clone());
        }
        let docs = counts.into_iter().collect();
        article_sections = Some(sections);
        docs
    } else {
        groups
            .iter()
            .map(|(id, chunk_ids)| (id.clone(), chunk_ids.len()))
            .collect()
    };
    // Optional curated subset: --titles-file restricts the build to articles
    // whose slug (last path segment of source_doc_id) matches a line in the
    // file. Lets a pilot target representative multi-section articles instead
    // of the default smallest-first stubs. Spaces in a line are normalized to
    // '_' so the file may hold either "Albert Einstein" or "Albert_Einstein".
    if let Some(path) = &parsed.titles_file {
        let raw = match std::fs::read_to_string(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: --titles-file {path}: {e}");
                return 1;
            }
        };
        let wanted: std::collections::HashSet<String> = raw
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.replace(' ', "_"))
            .collect();
        let before = docs.len();
        docs.retain(|(id, _)| {
            let slug = id.trim_end_matches('/').rsplit('/').next().unwrap_or(id);
            wanted.contains(slug)
        });
        println!(
            "  titles-file: {path} → matched {} of {before} source docs ({} requested)",
            docs.len(),
            wanted.len()
        );
        if docs.is_empty() {
            eprintln!(
                "error: --titles-file matched no source docs — check slugs against source_doc_id"
            );
            return 1;
        }
    }
    // Smallest documents first (tie-break by id for determinism): the
    // cheapest trees land early so the operator sees progress — and any
    // failure surfaces — before the long tail of big essays.
    docs.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    if let Some(limit) = parsed.limit {
        docs.truncate(limit);
    }
    let total_docs = docs.len();
    let total_chunks: usize = docs.iter().map(|(_, n)| *n).sum();

    let mut bucket_hist: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
    for (_, n) in &docs {
        *bucket_hist.entry(classify_bucket(*n).label()).or_default() += 1;
    }

    println!("RAPTOR retrofit plan for corpus '{}':", parsed.corpus_id);
    println!("  documents:  {total_docs}");
    println!("  chunks:     {total_chunks}");
    println!("  doc-type:   {}", parsed.doc_type.label());
    println!("  summaries:  {:?}", parsed.summary_mode);
    println!(
        "  furniture:  {}",
        if parsed.strip_furniture {
            "stripping SEP page-template chunks"
        } else {
            "kept (pass --strip-furniture to drop)"
        }
    );
    println!(
        "  by bucket:  {}",
        bucket_hist
            .iter()
            .map(|(b, n)| format!("{b}={n}"))
            .collect::<Vec<_>>()
            .join("  ")
    );
    if let Some(limit) = parsed.limit {
        println!("  (limited to the {limit} smallest documents)");
    }

    if parsed.dry_run {
        println!("\n--dry-run: no inference, no writes. Re-run without --dry-run to build.");
        return 0;
    }

    // Furniture inspection: load the (limited) docs, run the filter, and
    // print exactly what it would drop — so the filter can be verified
    // clean (furniture only, no philosophy) before any build. No
    // inference, no writes.
    if parsed.inspect_furniture {
        println!(
            "\nFurniture inspection (no inference, no writes) — what --strip-furniture would drop:"
        );
        let mut total = 0usize;
        let mut dropped = 0usize;
        let mut samples: Vec<String> = Vec::new();
        for (doc_id, _) in &docs {
            let rows = match index.chunks_for_source_doc_with_embeddings(doc_id).await {
                Ok(r) => r,
                Err(_) => continue,
            };
            for (chunk, _) in &rows {
                total += 1;
                if is_sep_furniture(&chunk.content) {
                    dropped += 1;
                    if samples.len() < 14 {
                        let preview: String = chunk.content.chars().take(95).collect();
                        samples.push(preview.replace('\n', " "));
                    }
                }
            }
        }
        let pct = if total > 0 {
            100.0 * dropped as f64 / total as f64
        } else {
            0.0
        };
        println!("  chunks scanned: {total} across {} docs", docs.len());
        println!("  would drop:     {dropped} ({pct:.0}%)");
        println!("  sample dropped chunks (verify these are ALL furniture, no philosophy):");
        for s in &samples {
            println!("    · {s}");
        }
        return 0;
    }

    // Wire inference (daemon over HTTP) + the doc-keyed provider. WAL mode
    // (migrations.rs) makes the concurrent store handle with the running
    // daemon safe; we keep a clone for post-build node counting.
    let v1 = format!("{}/v1", parsed.daemon_base.trim_end_matches('/'));
    let inference: Arc<dyn InferenceProvider> = Arc::new(SplitInferenceProvider::new(
        &v1,
        parsed.chat_model.clone(),
        parsed.embed_model.clone(),
        8192,
        sovereign_core::models_manifest::DEFAULT_MANIFEST
            .embed_query_instruction(&parsed.embed_model),
    ));
    let probe_inference = Arc::clone(&inference);

    let store = match SqliteStateStore::open(&db_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("error: open state db {}: {e}", db_path.display());
            return 1;
        }
    };
    let verify_store = Arc::clone(&store);

    let resolver: Arc<dyn IndexDirResolver> = Arc::new(StaticIndexDirResolver {
        indexes_root: indexes_dir.clone(),
    });
    // T1 P1.2 verification policy for abstractive builds. Explicit
    // flag wins; the default is SP3-adaptive on the corpus scale this
    // run will touch: full verification up to ~1.5k estimated nodes,
    // 12% deterministic sampling above (measured SEP density ≈ 17
    // chunks per node). Extractive builds have nothing to verify.
    let verify_policy = match (parsed.summary_mode, parsed.verify_summaries) {
        (sovereign_tools::raptor_atlas::SummaryMode::Extractive, _) => None,
        (_, Some(policy)) => Some(policy),
        (sovereign_tools::raptor_atlas::SummaryMode::Abstractive, None) => {
            let est_nodes = total_chunks / 17;
            if est_nodes <= 1500 {
                Some(sovereign_tools::summary_verify::VerifyPolicy::On)
            } else {
                Some(sovereign_tools::summary_verify::VerifyPolicy::Sample(0.12))
            }
        }
    };
    let mut provider = FolderTieredProvider::new(store, inference)
        .with_index_dir_resolver(resolver)
        .with_doc_type(parsed.doc_type.clone())
        .with_summary_mode(parsed.summary_mode);
    if let Some(policy) = verify_policy {
        provider = provider.with_verify_policy(policy);
    }
    let provider = provider.into_handle();

    println!(
        "\nBuilding RAPTOR trees via daemon {} (chat={}, embed={})…",
        parsed.daemon_base, parsed.chat_model, parsed.embed_model
    );
    match verify_policy {
        Some(sovereign_tools::summary_verify::VerifyPolicy::On) => {
            println!("Summary verification: ON — every abstractive summary judged against its member texts (T1 P1.2)\n")
        }
        Some(sovereign_tools::summary_verify::VerifyPolicy::Sample(p)) => {
            println!("Summary verification: SAMPLE {:.0}% (SP3 economics — corpus above ~1.5k estimated nodes)\n", p * 100.0)
        }
        Some(sovereign_tools::summary_verify::VerifyPolicy::Off) => {
            println!("Summary verification: OFF (explicit --verify-summaries off)\n")
        }
        None => println!(),
    }

    // --refresh-stale compares stored per-node stamps against the
    // CURRENT build config: the prompt version const and the model
    // that would serve a summary call RIGHT NOW. The build stamps each
    // node with `resp.model_id` — the routing decision actually made
    // per call (SLOT_POLICY routes the Workload::EnrichBulk summary
    // fan-out to the fast lane, not the pinned chat slot). So the
    // expected value must come from the same probe: one tiny EnrichBulk
    // completion through the same provider. Resolving the chat-model
    // alias via /v1/models instead compares attribution against
    // aspiration — observed live 2026-07-31: the alias table said the
    // 35B, EnrichBulk served the resident 4B, and every run reported
    // stale and rebuilt forever. A failed probe is a hard stop, not a
    // guess: with no truthful expected value the comparison is
    // meaningless.
    // Expected stamps are `(prompt_version, summarizer_model)` and
    // depend on the build mode: extractive trees are stamped with the
    // algo version + "extractive" (no probe needed — no LLM serves
    // them); abstractive trees need the probe above. Note a partially
    // fallen-back abstractive tree (some clusters stamped extractive
    // because their LLM call failed at build time) reads as stale
    // here — deliberately: refreshing it retries the abstractive
    // summaries the tree was supposed to have.
    let expected: Option<(String, String)> = if parsed.refresh_stale {
        match parsed.summary_mode {
            sovereign_tools::raptor_atlas::SummaryMode::Extractive => Some((
                sovereign_tools::raptor_atlas::EXTRACTIVE_ALGO_VERSION.to_string(),
                sovereign_tools::raptor_atlas::EXTRACTIVE_SUMMARIZER.to_string(),
            )),
            sovereign_tools::raptor_atlas::SummaryMode::Abstractive => {
                let mut probe = sovereign_core::types::CompletionRequest::for_workload(
                    sovereign_core::slot_policy::Workload::EnrichBulk,
                    "Reply with the single word: ok".to_string(),
                )
                .with_output_budget(8);
                probe.think_budget = Some(0);
                match probe_inference.complete(&probe).await {
                    Ok(r) if !r.model_id.is_empty() => Some((
                        sovereign_tools::raptor_atlas::RAPTOR_PROMPT_VERSION.to_string(),
                        r.model_id,
                    )),
                    Ok(_) => {
                        eprintln!(
                            "error: --refresh-stale probe completion against {} returned an empty model_id — staleness comparison would be meaningless",
                            parsed.daemon_base
                        );
                        return 1;
                    }
                    Err(e) => {
                        eprintln!(
                            "error: --refresh-stale probe completion against {} failed: {e} — staleness comparison would be meaningless",
                            parsed.daemon_base
                        );
                        return 1;
                    }
                }
            }
        }
    } else {
        None
    };
    if let Some((pv, stem)) = &expected {
        println!("  refresh-stale: current prompt {pv} · summarizer {stem}");
    }

    let run_start = Instant::now();
    let mut built = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut resumed = 0usize;
    let mut fresh = 0usize;
    let mut stale_rebuilt = 0usize;
    let mut empty = 0usize;
    let mut nodes_total = 0usize;

    for (idx, (doc_id, _)) in docs.into_iter().enumerate() {
        // Doc-level resume. The per-doc RAPTOR checkpoint shares ONE dir
        // per corpus (each document clobbers the previous one's), so it
        // gives no cross-document resume on a batch run. Skipping docs
        // that already have persisted nodes makes a crashed multi-day
        // build restart-cheap: a re-launch flies past completed docs and
        // picks up where it stopped. `--force` rebuilds regardless.
        if !parsed.force {
            let existing = verify_store
                .list_conv_raptor_nodes(&parsed.corpus_id, &doc_id)
                .await
                .unwrap_or_default();
            if !existing.is_empty() {
                if let Some((current_pv, stem)) = &expected {
                    // --refresh-stale: rebuild exactly the documents
                    // whose stored stamps disagree with the current
                    // build config. Synthetic rows (empty summary
                    // stamps AND no LLM provenance possible — the
                    // note-title rows) are level-0 singletons the
                    // builder never wrote; every builder-written node
                    // carries stamps from now on, and pre-stamping
                    // rows (both fields empty) are stale by
                    // definition — that is the point.
                    let stale_reason = existing.iter().find_map(|r| {
                        if &r.prompt_version != current_pv {
                            Some(format!(
                                "prompt {} != {current_pv}",
                                if r.prompt_version.is_empty() {
                                    "<unstamped>"
                                } else {
                                    &r.prompt_version
                                }
                            ))
                        } else if &r.summarizer_model != stem {
                            Some(format!(
                                "summarizer {} != {stem}",
                                if r.summarizer_model.is_empty() {
                                    "<unstamped>"
                                } else {
                                    &r.summarizer_model
                                }
                            ))
                        } else {
                            None
                        }
                    });
                    match stale_reason {
                        Some(reason) => {
                            println!("  stale: {doc_id} — {reason}; rebuilding");
                            stale_rebuilt += 1;
                            // fall through to the build below
                        }
                        None => {
                            fresh += 1;
                            continue;
                        }
                    }
                } else {
                    if resumed == 0 {
                        println!(
                            "  (resuming — skipping documents already built; --force to rebuild)"
                        );
                    }
                    resumed += 1;
                    continue;
                }
            }
        }
        let rows = if let Some(sections) = &article_sections {
            // --group-by-article: concatenate every section's chunks for this
            // article (each section is a separate source_doc_id under the
            // article URL). The whole-article chunk set is what RAPTOR clusters.
            let mut all = Vec::new();
            let mut fetch_err: Option<String> = None;
            for sdi in sections.get(&doc_id).map(|v| v.as_slice()).unwrap_or(&[]) {
                match index.chunks_for_source_doc_with_embeddings(sdi).await {
                    Ok(r) => all.extend(r),
                    Err(e) => {
                        fetch_err = Some(format!("section {sdi}: {e}"));
                        break;
                    }
                }
            }
            if let Some(e) = fetch_err {
                eprintln!(
                    "  [{}/{total_docs}] {doc_id}: chunk fetch failed: {e}",
                    idx + 1
                );
                failed += 1;
                continue;
            }
            all
        } else {
            match index.chunks_for_source_doc_with_embeddings(&doc_id).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "  [{}/{total_docs}] {doc_id}: chunk fetch failed: {e}",
                        idx + 1
                    );
                    failed += 1;
                    continue;
                }
            }
        };
        if rows.is_empty() {
            eprintln!(
                "  [{}/{total_docs}] {doc_id}: no embedded chunks; skipping",
                idx + 1
            );
            failed += 1;
            continue;
        }
        // Strip SEP page-template furniture before clustering so RAPTOR
        // doesn't spend a summary on copyright/nav/contact blocks (and so
        // those never pollute a whole-document summary). Filtering the
        // CHUNKS, not the resulting nodes — k-means can mix a content
        // sentence into a furniture cluster, so a node-level drop would
        // lose real content.
        let raw_count = rows.len();
        let rows: Vec<_> = if parsed.strip_furniture {
            rows.into_iter()
                .filter(|(c, _)| !is_sep_furniture(&c.content))
                .collect()
        } else {
            rows
        };
        let dropped = raw_count - rows.len();
        if rows.is_empty() {
            eprintln!(
                "  [{}/{total_docs}] {doc_id}: all {raw_count} chunks were furniture; skipping",
                idx + 1
            );
            skipped += 1;
            continue;
        }
        let bucket = classify_bucket(rows.len());
        let kept = rows.len();
        let (chunks, embeddings): (Vec<_>, Vec<_>) = rows.into_iter().unzip();

        let t = Instant::now();
        match provider
            .enrich_conversation(&parsed.corpus_id, &doc_id, chunks, embeddings, bucket)
            .await
        {
            Ok(()) => {
                let node_count = verify_store
                    .list_conv_raptor_nodes(&parsed.corpus_id, &doc_id)
                    .await
                    .map(|n| n.len())
                    .unwrap_or(0);
                let furniture_note = if dropped > 0 {
                    format!(" (-{dropped} furniture)")
                } else {
                    String::new()
                };
                if node_count == 0 {
                    // A non-skipped document that persists ZERO nodes is an
                    // anomaly — almost always the summarizer failing every
                    // cluster (e.g. the daemon's inference slot crashing
                    // mid-run, as on 2026-06-07 when the 92 largest SEP docs
                    // silently produced nothing). Flag it loudly and count it
                    // as a failure so a crash can never masquerade as "built";
                    // resume retries it (it has no nodes) on the next run.
                    empty += 1;
                    eprintln!(
                        "  [{}/{total_docs}] {doc_id}  {kept} chunks{furniture_note} · {} · 0 NODES — summarizer FAILED (will retry on resume) · {:.1}s",
                        idx + 1,
                        bucket.label(),
                        t.elapsed().as_secs_f64(),
                    );
                } else {
                    nodes_total += node_count;
                    built += 1;
                    println!(
                        "  [{}/{total_docs}] {doc_id}  {kept} chunks{furniture_note} · {} · {node_count} nodes · {:.1}s",
                        idx + 1,
                        bucket.label(),
                        t.elapsed().as_secs_f64(),
                    );
                }
            }
            Err(e) => {
                failed += 1;
                eprintln!("  [{}/{total_docs}] {doc_id}: build failed: {e}", idx + 1);
            }
        }
    }

    let elapsed = run_start.elapsed();
    println!("\nRAPTOR retrofit complete for '{}':", parsed.corpus_id);
    println!("  documents built:  {built}");
    if resumed > 0 {
        println!("  documents resumed (already built): {resumed}");
    }
    if parsed.refresh_stale {
        println!("  documents fresh (stamps match, skipped): {fresh}");
        println!("  documents stale (rebuilt): {stale_rebuilt}");
    }
    if skipped > 0 {
        println!("  documents skipped (all furniture): {skipped}");
    }
    if failed > 0 {
        println!("  documents failed: {failed}");
    }
    if empty > 0 {
        println!("  documents with 0 nodes (summarizer FAILED — re-run to retry): {empty}");
    }
    println!(
        "  nodes persisted:  {nodes_total}  (conv_raptor_nodes, corpus_id='{}')",
        parsed.corpus_id
    );
    println!("  elapsed:          {:.1}s", elapsed.as_secs_f64());
    if built > 0 {
        println!(
            "  avg per document: {:.1}s",
            elapsed.as_secs_f64() / built as f64
        );
    }
    if !per_file_units {
        println!(
            "\nThe atom-graph atlas (atlas/atoms.json) is untouched — RAPTOR nodes are additive."
        );
    }

    // Post-build: (re)build the RAPTOR summary-node ANN index so query-time
    // grounding takes the fast LanceDB path (`raptor_summaries.lance`) instead
    // of the brute-force scan. Once per RUN (not per document) — the index is a
    // whole-corpus derivative of `conv_raptor_nodes`. Mirrors the
    // `build_structural_atlas` post-install hook. Skipped on a total failure
    // (nothing to index); `enrich raptor-index` rebuilds it standalone.
    if built > 0 || resumed > 0 {
        let outcome = sovereign_tools::raptor_index::build_corpus_raptor_index(
            &verify_store,
            &index_path,
            &parsed.corpus_id,
        )
        .await;
        println!("RAPTOR summary-node ANN index: {outcome}");
    }

    // Folder corpora: finish with the same finalize the daemon-side
    // build runs — vault synthesis (vault_themes over the NEW node set)
    // + the typed-extension pass into atlas/atoms.json. Without this
    // the retrofit leaves vault_themes referencing the pre-retrofit
    // nodes, and the typed pass's cross-leaf Pass B (opposition /
    // concession) extracts from stale themes — measured 2026-06-11:
    // both cross-leaf axes dropped to 0 against the obsidian golden
    // until synthesis caught up. Document corpora (wiki/SEP) skip this:
    // vault synthesis + typed atoms are folder-shaped concerns.
    if per_file_units && (built > 0 || resumed > 0) {
        println!("\nFolder corpus — running finalize (vault synthesis + typed extension)…");
        match provider.finalize_corpus(&parsed.corpus_id).await {
            Ok(()) => println!("  finalize complete (vault_themes + atlas/atoms.json refreshed)"),
            Err(e) => eprintln!(
                "  finalize failed (non-fatal — re-run `svrn atlas typed-extension {}` \
                 after fixing): {e}",
                parsed.corpus_id
            ),
        }
    }

    // Total failure (nothing built) is a non-zero exit; partial
    // failures are tolerated like the folder runner — one bad document
    // shouldn't sink a multi-day pass.
    if empty > 0 || (built == 0 && failed > 0) {
        return 1;
    }
    0
}

fn parse_args(args: &[String]) -> Result<RaptorArgs, String> {
    let mut corpus_id: Option<String> = None;
    let mut doc_type = DocumentTypeTag::Unknown;
    let mut limit: Option<usize> = None;
    let mut dry_run = false;
    let mut strip_furniture = false;
    let mut inspect_furniture = false;
    let mut force = false;
    let mut refresh_stale = false;
    let mut titles_file: Option<String> = None;
    let mut group_by_article = false;
    let mut verify_summaries: Option<sovereign_tools::summary_verify::VerifyPolicy> = None;
    let mut daemon_base = "http://localhost:9741".to_string();
    let mut chat_model = "primary".to_string();
    let mut embed_model = "embed".to_string();
    let mut summary_mode = sovereign_tools::raptor_atlas::SummaryMode::Abstractive;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--doc-type" => {
                i += 1;
                let v = args.get(i).ok_or("--doc-type needs a value")?;
                doc_type = parse_doc_type(v)?;
            }
            "--limit" => {
                i += 1;
                let v = args.get(i).ok_or("--limit needs a value")?;
                limit = Some(
                    v.parse::<usize>()
                        .map_err(|_| format!("--limit: not a number: {v}"))?,
                );
            }
            "--dry-run" => dry_run = true,
            "--strip-furniture" => strip_furniture = true,
            "--inspect-furniture" => {
                inspect_furniture = true;
                strip_furniture = true;
            }
            "--force" => force = true,
            "--refresh-stale" => refresh_stale = true,
            "--daemon" => {
                i += 1;
                daemon_base = args.get(i).ok_or("--daemon needs a value")?.clone();
            }
            "--chat-model" => {
                i += 1;
                chat_model = args.get(i).ok_or("--chat-model needs a value")?.clone();
            }
            "--embed-model" => {
                i += 1;
                embed_model = args.get(i).ok_or("--embed-model needs a value")?.clone();
            }
            "--titles-file" => {
                i += 1;
                titles_file = Some(args.get(i).ok_or("--titles-file needs a path")?.clone());
            }
            "--group-by-article" => group_by_article = true,
            "--verify-summaries" => {
                i += 1;
                let v = args.get(i).ok_or("--verify-summaries needs a value")?;
                verify_summaries = Some(
                    sovereign_tools::summary_verify::VerifyPolicy::parse(v)
                        .map_err(|e| format!("--verify-summaries: {e}"))?,
                );
            }
            "--summary-mode" => {
                i += 1;
                let v = args.get(i).ok_or("--summary-mode needs a value")?;
                summary_mode = match v.to_ascii_lowercase().as_str() {
                    "abstractive" => sovereign_tools::raptor_atlas::SummaryMode::Abstractive,
                    "extractive" => sovereign_tools::raptor_atlas::SummaryMode::Extractive,
                    other => {
                        return Err(format!(
                            "unknown --summary-mode '{other}' (abstractive|extractive)"
                        ))
                    }
                };
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_some() {
                    return Err(format!("unexpected argument: {other}"));
                }
                corpus_id = Some(other.to_string());
            }
        }
        i += 1;
    }

    let corpus_id = corpus_id.ok_or("missing <corpus-id>")?;
    Ok(RaptorArgs {
        corpus_id,
        doc_type,
        limit,
        dry_run,
        strip_furniture,
        inspect_furniture,
        force,
        refresh_stale,
        titles_file,
        group_by_article,
        verify_summaries,
        daemon_base,
        chat_model,
        embed_model,
        summary_mode,
    })
}

fn parse_doc_type(s: &str) -> Result<DocumentTypeTag, String> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "argument" => DocumentTypeTag::Argument,
        "narrative" => DocumentTypeTag::Narrative,
        "evidence" => DocumentTypeTag::Evidence,
        "chronicle" => DocumentTypeTag::Chronicle,
        "technical" => DocumentTypeTag::Technical,
        "journal" => DocumentTypeTag::Journal,
        "unknown" | "document" => DocumentTypeTag::Unknown,
        other => {
            return Err(format!(
                "unknown --doc-type '{other}' (argument|narrative|evidence|chronicle|technical|journal|unknown)"
            ))
        }
    })
}

/// SEP page-template furniture detector. SEP entries publish through a
/// fixed template whose copyright / contact / navigation blocks get
/// chunked alongside the entry prose. This matches those blocks so the
/// RAPTOR pass doesn't waste a summary on "this is metadata" (and so
/// furniture never pollutes a whole-document summary). Deliberately
/// CONSERVATIVE: it matches unambiguous template strings at the chunk
/// start, plus the SEP-wide copyright footer anywhere — so bibliography,
/// block quotes, and entry prose are never dropped.
fn is_sep_furniture(content: &str) -> bool {
    // The SEP-wide copyright footer can sit mid-chunk; match it anywhere.
    if content.contains("The Stanford Encyclopedia of Philosophy is copyright") {
        return true;
    }
    // Chunks are "<slug>\n\n<body>"; the per-entry furniture blocks lead
    // the body with a fixed template string.
    let body = content
        .split_once("\n\n")
        .map(|(_, rest)| rest)
        .unwrap_or(content)
        .trim_start();
    const START_MARKERS: &[&str] = &[
        "[Please contact the author",
        "Please contact the author",
        "Copyright ©",
        "Academic Tools",
        "Other Internet Resources",
        "Related Entries",
        "How to cite this entry",
        "Friends PDF Preview",
        "Author and Citation Info",
        "Preview the PDF version",
    ];
    START_MARKERS.iter().any(|m| body.starts_with(m))
}

fn print_usage() {
    eprintln!(
        "svrn enrich raptor — retrofit an installed corpus with a per-document RAPTOR tier-3 summary tree."
    );
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("  svrn enrich raptor <corpus-id> [flags]");
    eprintln!();
    eprintln!("FLAGS:");
    eprintln!("  --doc-type <tag>    Summary cue: argument|narrative|evidence|chronicle|technical|unknown");
    eprintln!(
        "                      (default: unknown). SEP philosophy essays → argument (claim-level)."
    );
    eprintln!("  --limit N           Build only the N smallest documents (by chunk count). Use for a spike.");
    eprintln!("  --strip-furniture   Drop SEP page-template chunks (copyright/contact/nav) before clustering.");
    eprintln!("  --inspect-furniture Show which chunks --strip-furniture would drop, then exit. Implies --strip-furniture.");
    eprintln!("  --dry-run           Print the dispatch plan and exit (no inference, no writes).");
    eprintln!("  --force             Rebuild every document, even ones already built (default: resume/skip them).");
    eprintln!("  --refresh-stale     Rebuild only documents whose stored trees carry an outdated prompt_version or summarizer_model stamp (pre-stamping trees count as stale).");
    eprintln!("  --summary-mode <m>  abstractive (default: LLM prose, extractive fallback on failure) | extractive (LLM-free verbatim sentence selection, T1 P1.1)");
    eprintln!("  --verify-summaries <p>  Abstractive verification gate (T1 P1.2): on | off | sample:<p>. Default adapts to corpus scale: on up to ~1.5k estimated nodes, sample:0.12 above (SP3).");
    eprintln!("  --titles-file <path>  Restrict the build to a curated article set (one slug/title per line).");
    eprintln!("  --group-by-article  Group chunks into per-article documents (SEP-style corpora) instead of per-source-doc.");
    eprintln!("  --daemon <url>      Daemon base URL (default: http://localhost:9741).");
    eprintln!("  --chat-model <id>   Summarizer model id/alias (default: primary).");
    eprintln!("  --embed-model <id>  Embedding model id/alias for summary nodes (default: embed).");
    eprintln!();
    eprintln!("Additive: does NOT modify the corpus's atom-graph atlas (atlas/atoms.json).");
    eprintln!(
        "Resumable: each document checkpoints under its index dir; re-runs skip completed trees."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_corpus_and_defaults() {
        let p = parse_args(&sv(&["sep"])).unwrap();
        assert_eq!(p.corpus_id, "sep");
        assert_eq!(p.doc_type, DocumentTypeTag::Unknown);
        assert!(p.limit.is_none());
        assert!(!p.dry_run);
        assert!(!p.strip_furniture);
        assert!(!p.force);
        assert_eq!(p.chat_model, "primary");
        assert_eq!(p.embed_model, "embed");
        assert_eq!(p.daemon_base, "http://localhost:9741");
    }

    #[test]
    fn parses_all_flags() {
        let p = parse_args(&sv(&[
            "sep",
            "--doc-type",
            "argument",
            "--limit",
            "5",
            "--dry-run",
            "--strip-furniture",
            "--chat-model",
            "Qwen3.6-35B-A3B-MTP-UD-Q6_K",
            "--embed-model",
            "Qwen3-Embedding-0.6B-Q8_0",
        ]))
        .unwrap();
        assert_eq!(p.doc_type, DocumentTypeTag::Argument);
        assert_eq!(p.limit, Some(5));
        assert!(p.dry_run);
        assert!(p.strip_furniture);
        assert_eq!(p.chat_model, "Qwen3.6-35B-A3B-MTP-UD-Q6_K");
        assert_eq!(p.embed_model, "Qwen3-Embedding-0.6B-Q8_0");
    }

    #[test]
    fn doc_type_is_case_insensitive() {
        assert_eq!(
            parse_doc_type("Argument").unwrap(),
            DocumentTypeTag::Argument
        );
        assert_eq!(
            parse_doc_type("NARRATIVE").unwrap(),
            DocumentTypeTag::Narrative
        );
        assert_eq!(
            parse_doc_type("document").unwrap(),
            DocumentTypeTag::Unknown
        );
    }

    #[test]
    fn rejects_unknown_doc_type() {
        assert!(parse_doc_type("philosophy").is_err());
    }

    #[test]
    fn rejects_missing_corpus() {
        assert!(parse_args(&sv(&["--limit", "5"])).is_err());
    }

    #[test]
    fn rejects_unknown_flag() {
        assert!(parse_args(&sv(&["sep", "--bogus"])).is_err());
    }

    #[test]
    fn rejects_second_positional() {
        assert!(parse_args(&sv(&["sep", "wikipedia"])).is_err());
    }

    #[test]
    fn limit_requires_number() {
        assert!(parse_args(&sv(&["sep", "--limit", "lots"])).is_err());
    }

    #[test]
    fn parses_furniture_flags() {
        let p = parse_args(&sv(&["sep", "--strip-furniture"])).unwrap();
        assert!(p.strip_furniture);
        assert!(!p.inspect_furniture);
        let p2 = parse_args(&sv(&["sep", "--inspect-furniture"])).unwrap();
        assert!(p2.inspect_furniture);
        assert!(p2.strip_furniture, "inspect implies strip");
    }

    #[test]
    fn parses_force_flag() {
        assert!(parse_args(&sv(&["sep", "--force"])).unwrap().force);
        assert!(!parse_args(&sv(&["sep"])).unwrap().force);
    }

    #[test]
    fn furniture_detects_template_blocks() {
        assert!(is_sep_furniture(
            "david\n\n[Please contact the author with suggestions"
        ));
        assert!(is_sep_furniture(
            "holes\n\nCopyright © 2019 by   Roberto Casati <casati@ehess>"
        ));
        assert!(is_sep_furniture(
            "holes\n\nThe Stanford Encyclopedia of Philosophy is copyright © 2021 by The Metaphysics Research Lab"
        ));
        assert!(is_sep_furniture("x\n\nRelated Entries\n\natomism | Plato"));
    }

    #[test]
    fn furniture_keeps_real_content() {
        assert!(!is_sep_furniture(
            "leucippus\n\nLeucippus is recognized as the founder of ancient Greek atomism."
        ));
        // bibliography is content, not furniture
        assert!(!is_sep_furniture(
            "leucippus\n\nThe standard scholarly edition of the ancient reports concerning the Presocratics is Diels-Kranz' work (DK)."
        ));
        // a passing mention of copyright mid-sentence is not furniture
        assert!(!is_sep_furniture(
            "ip\n\nThe modern concept of copyright © emerged in the 18th century as a legal response to printing."
        ));
    }
}
