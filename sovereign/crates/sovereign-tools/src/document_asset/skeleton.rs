// SPDX-License-Identifier: AGPL-3.0-or-later
//! Skeleton extraction — document type detection, segmentation, and the
//! structural skeleton the router reads.

// One cooperating unit split for size (ARCH §3.2), not independent modules:
// the manager, its three phases and the skeleton free functions all name each
// other's types. The import surface stays in `mod.rs`.
use super::*;

// ─── Skeleton extraction (free functions) ────────────────────
//
// These are free functions rather than methods on DocumentAssetManager
// because they're called from spawned futures that can't borrow &self.

/// Detect the document type from the first few chunks.
pub(super) async fn detect_document_type(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[TextChunk],
) -> DocumentTypeTag {
    let sample: String = chunks
        .iter()
        .take(3)
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let prompt = format!(
        "Classify this document into one category based on these opening passages:\n\n\
         {sample}\n\n\
         Categories:\n\
         - Narrative (novels, memoirs, literary non-fiction)\n\
         - Argument (dissertations, essays, philosophy)\n\
         - Evidence (legal briefs, scientific papers)\n\
         - Chronicle (history, biography, journalism)\n\
         - Technical (manuals, specifications, documentation)\n\n\
         Respond with exactly one word: Narrative, Argument, Evidence, Chronicle, or Technical."
    );

    // SLOT_POLICY §3 Route: document-type classification consumed by
    // control flow (DocumentTypeTag), never shown to the user. Route's
    // Some(0) think budget matches this site verbatim.
    let mut request = Workload::Route.request(prompt).with_output_budget(16);
    request.temperature = Some(0.0);
    let response = inference.complete(&request).await;

    let detected = match response {
        Ok(r) => {
            // Strip `<think>...</think>` blocks first — Qwen thinking
            // models emit them even when `think_budget: Some(0)` is set,
            // and without stripping the raw text looks like
            // `"<think>\n</think>\n\nArgument"` which never matches any
            // category and always falls through to Unknown.
            let cleaned = sovereign_core::title::strip_think_blocks(&r.text);
            match cleaned.trim().to_lowercase().as_str() {
                "narrative" => DocumentTypeTag::Narrative,
                "argument" => DocumentTypeTag::Argument,
                "evidence" => DocumentTypeTag::Evidence,
                "chronicle" => DocumentTypeTag::Chronicle,
                "technical" => DocumentTypeTag::Technical,
                _ => DocumentTypeTag::Unknown,
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "detect_document_type — inference failed, defaulting to Unknown");
            DocumentTypeTag::Unknown
        }
    };
    tracing::info!(detected = ?detected, "detect_document_type — classified");
    detected
}

/// Build the T2 (entity-tier) skeleton by processing chunks in
/// parallel through the LLM. Extracts sections, entities (with
/// kind), and structural moments. Returns a *partial* skeleton —
/// `overview` is empty and `segments` is empty; those are T3
/// outputs, filled in by `build_and_persist_raptor_atlas`.
///
/// Parallelism: per-batch tasks fan out across the mesh via
/// `futures::stream::iter(...).buffered(T2_BATCH_CONCURRENCY)`. On a
/// 2-peer mesh, this gives near-linear speedup over the previous
/// sequential `for batch in chunks.chunks(4)` loop; on a single-machine
/// deployment, the Slow slot serialises them but the async overhead
/// is no worse than the sequential version. The May-21 lean-grammar
/// probe measured 1.4s/batch; with concurrency=6 on a 250-batch
/// document that projects to ~60s for the entity-extraction phase.
pub(super) async fn build_skeleton(
    inference: &Arc<dyn InferenceProvider>,
    store: &Arc<dyn StateStore>,
    asset_id: &str,
    chunks: &[TextChunk],
    doc_type: &DocumentTypeTag,
    on_progress: &Arc<dyn Fn(IngestProgress) + Send + Sync>,
    entity_extractor: Option<&Arc<dyn EntityExtractor>>,
) -> Result<DocumentSkeleton> {
    let chunk_count = chunks.len();

    // Glassbox: which T2 entity path is this ingest taking? A local NER
    // model (GLiNER) when one is wired, else the per-window LLM pass.
    // Per-window fallback (empty NER result) is logged at the call site;
    // this line records the intended path once, up front, so an operator
    // reading logs can see the −70%-token swap is engaged without
    // inferring it from the absence of "List the named entities" calls.
    tracing::info!(
        chunks = chunk_count,
        entity_path = if entity_extractor.is_some() {
            "ner"
        } else {
            "llm"
        },
        "build_skeleton — T2 entity extraction path"
    );

    // Process chunks in 12-chunk windows (was batches of 4 until
    // 2026-07-24). Profiling on the turbocharge arc showed DECODE
    // volume is the enrichment wall on this hardware (batched decode
    // doesn't amortize on Vulkan/LPDDR5), and per-chunk entity lines
    // re-emit the same recurring names ~N times per window. The window
    // schema emits ONE deduped name list per window (~3.5× less
    // decode, 3× fewer calls) and chunk-level attribution is
    // recovered DETERMINISTICALLY by scanning each window chunk for
    // each name (`parse_window_skeleton_batch`) — exact where the
    // model's per-line alignment was merely grammar-constrained.
    let batch_size = 12;
    let batches: Vec<(usize, Vec<TextChunk>)> = chunks
        .chunks(batch_size)
        .enumerate()
        .map(|(idx, b)| (idx, b.to_vec()))
        .collect();
    let total_batches = batches.len();
    let completed = Arc::new(AtomicUsize::new(0));

    let inference_arc = Arc::clone(inference);
    let store_arc = Arc::clone(store);
    let on_progress_arc = Arc::clone(on_progress);
    let asset_id_owned = asset_id.to_string();
    let doc_type_owned = doc_type.clone();
    let chunk_count_for_progress = chunk_count;

    let extractor_arc = entity_extractor.cloned();

    let batch_results: Vec<(usize, Option<Vec<SkeletonBatchEntry>>)> = stream::iter(batches)
        .map(|(batch_idx, batch)| {
            let inference = Arc::clone(&inference_arc);
            let store = Arc::clone(&store_arc);
            let on_progress = Arc::clone(&on_progress_arc);
            let asset_id = asset_id_owned.clone();
            let doc_type = doc_type_owned.clone();
            let completed = Arc::clone(&completed);
            let extractor = extractor_arc.clone();
            async move {
                let batch_start = batch_idx * batch_size;
                let passage: String = batch
                    .iter()
                    .map(|c| c.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n");

                // ── NER fast path.
                //
                // The LLM prompt below asks for exactly one thing: the
                // named entities present in this window. A local NER
                // model answers that directly, for zero LLM tokens.
                //
                // `extract_entities` is synchronous and CPU-bound (ONNX),
                // so it goes on the blocking pool — running it inline
                // would stall the executor driving the other windows.
                //
                // An empty result is NOT trusted: `LazyGlinerExtractor`
                // returns empty until its background load finishes, and a
                // model that whiffs on a window would silently erase that
                // window's entities. Empty ⇒ fall through to the LLM, so
                // the worst case is the previous behaviour.
                let ner_names: Option<Vec<String>> = match extractor {
                    Some(g) => {
                        let text = passage.clone();
                        match tokio::task::spawn_blocking(move || g.extract_entities(&text)).await {
                            Ok(names) if !names.is_empty() => Some(names),
                            Ok(_) => None,
                            Err(e) => {
                                tracing::debug!(
                                    batch_idx,
                                    error = %e,
                                    "build_skeleton — NER task failed; falling back to LLM for this window"
                                );
                                None
                            }
                        }
                    }
                    None => None,
                };
                if let Some(names) = ner_names {
                    let entries = attribute_entity_names(names, batch_start, &batch);
                    let done_now = completed.fetch_add(1, Ordering::SeqCst) + 1;
                    let chunks_done = (done_now * batch_size).min(chunk_count_for_progress);
                    on_progress(IngestProgress::BuildingSkeleton {
                        done: chunks_done,
                        total: chunk_count_for_progress,
                    });
                    let _ = store
                        .update_asset_state(
                            &asset_id,
                            &AssetState::BuildingSkeleton {
                                chunks_done,
                                chunks_total: chunk_count_for_progress,
                            },
                        )
                        .await;
                    return (batch_idx, Some(entries));
                }

                // Window entity schema with llguidance grammar
                // enforcement: ONE deduped, comma-separated list of
                // canonical names for the whole window. Chunk-level
                // attribution happens in the parser by scanning each
                // window chunk for each name — the model only has to
                // NAME the entities, never to align them.
                let prompt = format!(
                    "List the named entities mentioned in the passage below from this \
                     {doc_type} document — characters, organizations, places, key \
                     concepts — using their canonical names EXACTLY as they appear in \
                     the text. Output ONE comma-separated list with each name once. \
                     No prose, no JSON, no headers.\n\n\
                     Passage (sections from {batch_start}):\n\n{passage}\n\nAnswer (one line):",
                    doc_type = doc_type.label(),
                );
                let lark_grammar = "start: line\n\
                     line: (entity (\",\" \" \"? entity)*)?\n\
                     entity: /[A-Z][A-Za-z'.]*( [A-Z][A-Za-z'.]*)*/\n"
                    .to_string();

                // SLOT_POLICY §3 EnrichBulk: high-volume, small-output,
                // grammar-constrained extraction — the Fast-class bundle
                // whose 512-token cap this fits with 4× headroom.
                // Changed from ExtractDurable 2026-07-24 (enrichment
                // turbocharge arc): Normal-class routing serialized all
                // ~250 batches through the single primary slot, making
                // `buffered(N)` fan-out a no-op locally; Fast-class
                // routing engages the FastShort continuous-batching
                // companion under fan-out, which is REAL concurrency.
                // Durability is protected by the llguidance grammar
                // (shape cannot desync) + the 4B-parity result from the
                // 2026-07-23 enrichment-model ladder (skeleton quality
                // is not model-bound above 4B).
                let mut request =
                    Workload::EnrichBulk.request(prompt)
                        .with_output_budget(120);
                request.temperature = Some(0.1);
                // Grammar constraint preserved verbatim (see lark_grammar above).
                request.lark_grammar = Some(lark_grammar);
                // POLICY-DEBT(SLOT_POLICY §3 ExtractDurable): Some(0) preserved
                // for P1 neutrality (bundle is None); P5 confirms.
                request.think_budget = Some(0);
                let response = inference.complete(&request).await;
                let parsed = response
                    .ok()
                    .map(|resp| parse_window_skeleton_batch(&resp.text, batch_start, &batch));

                // Per-batch progress tick. Atomic counter is the only
                // way to give the UI monotonic progress when batches
                // complete out of order under buffered().
                let done_now = completed.fetch_add(1, Ordering::SeqCst) + 1;
                let chunks_done =
                    (done_now * batch_size).min(chunk_count_for_progress);
                on_progress(IngestProgress::BuildingSkeleton {
                    done: chunks_done,
                    total: chunk_count_for_progress,
                });
                let _ = store
                    .update_asset_state(
                        &asset_id,
                        &AssetState::BuildingSkeleton {
                            chunks_done,
                            chunks_total: chunk_count_for_progress,
                        },
                    )
                    .await;

                (batch_idx, parsed)
            }
        })
        .buffered(T2_BATCH_CONCURRENCY)
        .collect()
        .await;

    let _ = total_batches; // referenced for future progress assertions; kept silent

    // Merge results sequentially after the parallel stream completes.
    // Order by batch_idx so the resulting sections list is in document
    // order — some downstream code reads sections in order.
    let mut sorted_results = batch_results;
    sorted_results.sort_by_key(|(idx, _)| *idx);
    let mut sections = Vec::new();
    let mut entity_mentions: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    let mut entity_kinds: std::collections::HashMap<String, EntityKind> =
        std::collections::HashMap::new();
    let mut structural_moments = Vec::new();
    for (_, parsed_opt) in sorted_results {
        let Some(parsed) = parsed_opt else { continue };
        for entry in parsed {
            for (name, kind) in &entry.entity_names_and_kinds {
                entity_mentions
                    .entry(name.clone())
                    .or_default()
                    .push(entry.chunk_index);
                entity_kinds
                    .entry(name.clone())
                    .or_insert_with(|| kind.clone());
            }
            if let Some(ref desc) = entry.moment_description {
                structural_moments.push(StructuralMoment {
                    chunk_index: entry.chunk_index,
                    description: desc.clone(),
                    salience: 0.8,
                });
            }
            sections.push(SectionAnnotation {
                chunk_index: entry.chunk_index,
                function: entry.function,
                key_entities: entry
                    .entity_names_and_kinds
                    .iter()
                    .map(|(n, _)| n.clone())
                    .collect(),
                establishes: String::new(),
            });
        }
    }

    // ── Build entity ranking ────────────────────────────────
    let total_sections = sections.len().max(1);
    let mut main_entities: Vec<RankedEntity> = entity_mentions
        .iter()
        .map(|(name, indices)| {
            let first = indices.iter().copied().min().unwrap_or(0);
            let last = indices.iter().copied().max().unwrap_or(0);
            let presence_rate = indices.len() as f32 / total_sections as f32;
            let kind = entity_kinds
                .get(name)
                .cloned()
                .unwrap_or(EntityKind::Concept);
            RankedEntity {
                name: name.clone(),
                kind,
                presence_rate,
                first_appearance: first,
                last_appearance: last,
            }
        })
        .collect();
    main_entities.sort_by(|a, b| b.presence_rate.partial_cmp(&a.presence_rate).unwrap());
    main_entities.truncate(30);

    // ── Build entity index ──────────────────────────────────
    let entity_index: std::collections::HashMap<String, EntityAppearances> = entity_mentions
        .into_iter()
        .filter(|(name, _)| main_entities.iter().any(|e| &e.name == name))
        .map(|(name, indices)| {
            // Char-bounded truncation. The prior `c.content[..len.min(200)]`
            // byte-sliced and panicked when byte 200 landed inside a
            // multi-byte char (curly quotes, em-dashes, ellipses — common
            // in literary text). Same fix shape as `short_snippet`: take
            // chars not bytes.
            let quote_samples: Vec<String> = indices
                .iter()
                .take(3)
                .filter_map(|&i| chunks.get(i))
                .map(|c| c.content.chars().take(200).collect::<String>())
                .collect();
            (
                name,
                EntityAppearances {
                    chunk_indices: indices,
                    quote_samples,
                },
            )
        })
        .collect();

    structural_moments.truncate(40);

    // ── Action atoms (atlas-light) ──────────────────────────
    // For each top-N entity, run a Fast-slot pass over the entity's
    // appearance chunks and extract verb-object pairs anchored to
    // chunk_index. Cap N at 6 so the per-document cost stays bounded.
    // Action atoms route around the embedding-similarity gap — the
    // model queries by entity name, the tool consults the atom index,
    // and the original chunk surfaces by structural lookup rather
    // than embedding similarity.
    let actions = extract_action_atoms(inference, chunks, &main_entities, &entity_index).await;

    // T2-phase skeleton is partial — `overview` and `segments` are
    // empty placeholders. T3 (`build_and_persist_raptor_atlas` called
    // from `ingest`) fills them in: `overview` from the RAPTOR root
    // summary, `segments` from `extract_segments` (TextTiling).
    // This split is what powers the tiered state machine — the asset
    // transitions to `MultiHopReady` after this function returns,
    // before T3 enrichment starts.
    Ok(DocumentSkeleton {
        sections,
        main_entities,
        entity_index,
        structural_moments,
        overview: String::new(),
        actions,
        segments: Vec::new(),
        built_at: chrono::Utc::now(),
    })
}

/// Two-pass segment extraction.
///
/// **Pass A — boundary detection.** For each pair of adjacent
/// chunks, ask the model whether there is a segment break between
/// them. Output is a single word (`BREAK` or `CONTINUE`) — minimal
/// decode cost, accuracy is the model's job. ~N-1 calls for N
/// chunks.
///
/// **Pass B — segment naming.** Derive segment ranges from the
/// boundary decisions, then fire one call per segment to produce
/// title + summary + function. Output is bounded JSON.
///
/// Both passes use Speed::Slow (Primary 35B). Fast slot would
/// likely do well at Pass A (binary decision on adjacent chunks)
/// but is currently unloaded; revisit if ingest latency becomes a
/// production-perf blocker.
///
/// The function is fault-tolerant: any call failure falls back to
/// `CONTINUE` (no break) or a default title, so a partial network
/// blip produces fewer, larger segments rather than failing the
/// whole ingest. Segments are an additive retrieval surface, not
/// a load-bearing index — degraded extraction degrades retrieval
/// gracefully.
pub(super) async fn extract_segments(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[TextChunk],
    main_entities: &[RankedEntity],
    doc_type: DocumentTypeTag,
    stored_embeddings: Option<Vec<Vec<f32>>>,
) -> Vec<DocumentSegment> {
    if chunks.len() < 2 {
        return Vec::new();
    }

    // ── Pass A — boundary detection via TextTiling ─────────
    //
    // Replaces the original per-pair LLM Pass A (one Speed::Slow
    // call per adjacent chunk pair, N-1 sequential calls for N
    // chunks — ~17 min on the Conrad 1006-chunk doc). TextTiling
    // (Hearst 1997, embedding variant) computes adjacent-chunk
    // cosine similarity, smooths it, scores each gap by its
    // "depth" (how far it dips below the surrounding peaks), and
    // thresholds at mean + k·std. Zero LLM calls; ~30s for
    // embedding + sub-second for the boundary detection.
    //
    // The earlier batched-LLM Pass A failed validation 2026-05-21
    // (template-shaped output, 5% precision). TextTiling has none
    // of that failure mode — boundaries fall out of arithmetic on
    // numbers the embedding model already produced for the chunk
    // store. The per-document-type cue is gone because the
    // similarity signal is doc-type-agnostic; doc-type-aware
    // naming still happens in Pass B.
    // Reuse T1's stored embeddings when the caller has them (they are
    // the SAME model + same chunk texts — re-embedding was pure waste,
    // ~30s per 300-chunk document, caught by the 2026-07-24 turbocharge
    // profile). Fall back to a fresh embed_batch when absent.
    let embeddings = match stored_embeddings {
        Some(e) if e.len() == chunks.len() => e,
        _ => {
            let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
            match inference.embed_batch(&texts).await {
                Ok(e) if e.len() == chunks.len() => e,
                _ => {
                    // Embedding failure or count mismatch — fall back to one
                    // segment per chunk so the rest of the pipeline still
                    // makes progress.
                    tracing::warn!(
                        "extract_segments — embed_batch failed or returned wrong count; treating doc as one segment per chunk"
                    );
                    vec![]
                }
            }
        }
    };
    let breaks: Vec<bool> = if embeddings.is_empty() {
        vec![false; chunks.len().saturating_sub(1)]
    } else {
        detect_segment_boundaries(&embeddings, /* window = */ 3, /* depth_k = */ 1.0)
    };
    tracing::info!(
        chunks = chunks.len(),
        breaks = breaks.iter().filter(|b| **b).count(),
        "extract_segments — TextTiling complete"
    );

    // Derive segment ranges from break decisions.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut seg_start = 0usize;
    for (i, &is_break) in breaks.iter().enumerate() {
        if is_break {
            // Segment [seg_start..=i] ends. Next segment starts at i+1.
            ranges.push((seg_start, i));
            seg_start = i + 1;
        }
    }
    // Close the final segment, which runs to the last chunk.
    ranges.push((seg_start, chunks.len() - 1));

    // Cap segment count so a very-low depth_k or a pathological
    // embedding signal that fires breaks on every gap can't blow
    // up Pass B with hundreds of single-chunk segments.
    if ranges.len() > 200 {
        return Vec::new();
    }

    // ── Pass B — name ALL segments in one batched call ─────
    //
    // (2026-07-24 turbocharge arc.) The prior per-segment loop made
    // ~25-30 sequential ExtractDurable calls whose decode (~3k
    // tokens of title+summary+key_entities JSON) was the measured
    // wall of the T3 "silent block" (135s of a 285s subset build).
    // The briefing's scene map consumes ONLY `title` (+ chunk
    // range), so the batched schema emits `index|title|function`
    // lines — one call, ~15 tokens per segment, grammar-enforced
    // line count so alignment can't desync. `summary`/`key_entities`
    // are left empty (never read on the retrieval path; segments
    // carry structure, not content).
    let entity_list = main_entities
        .iter()
        .take(8)
        .map(|e| e.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    // Chunk the naming into calls of ≤14 segments, dispatched
    // concurrently.
    //
    // Two separate lessons are baked into this number.
    //
    // The 64 ceiling came from a correctness bug: the first cut of this
    // pass named all segments in ONE call clamped at 2048 output tokens
    // and silently placeholder-titled everything past segment ~85 on a
    // full book (caught by the 2026-07-24 quality gate).
    //
    // The drop from 64 to 14 came from the same arc's per-call ledger.
    // Naming is the only *decode*-bound call in the pipeline: 52
    // segments in one call meant 1288 completion tokens and 32.6s.
    // Split into four, the calls dispatch together and retire in
    // lockstep (three of them to the same millisecond) — measured
    // evidence that this path gets a batched decode — and the block
    // fell to 21.5s. Note the split does NOT bring the call inside the
    // FastShort claim: `ExtractDurable` is `LatencyClass::Normal`, so
    // gate 2 of `pick_slot` (`preferred_speed == Fast`) excludes it
    // regardless of size. Whatever coalesces these, it isn't that.
    //
    // What this costs on a host that serves these serially: each call
    // repeats the ~500-char instruction preamble, so total prompt grows
    // ~12% (3660 → 4088 tokens on the bench subset) for the same total
    // decode. That is the honest downside on a CPU-only box where
    // nothing batches. It is worth paying anyway because smaller calls
    // are also the fix for the truncation bug above — a short
    // grammar-forced output can't run out of budget mid-document.
    //
    // Order is preserved: `buffered` yields in input order.
    const PASS_B_CALL_SEGMENTS: usize = 14;
    // Matches the FastShort lane's `n_seq_max=8`; more in flight than
    // that cannot join a batch anywhere in the stack.
    const PASS_B_CONCURRENCY: usize = 8;
    let windows: Vec<(usize, Vec<(usize, usize)>)> = ranges
        .chunks(PASS_B_CALL_SEGMENTS)
        .enumerate()
        .map(|(w, window)| (w * PASS_B_CALL_SEGMENTS, window.to_vec()))
        .collect();
    let titles: Vec<(String, SectionFunction)> = stream::iter(windows)
        .map(|(base, window)| {
            let entity_list = entity_list.clone();
            let doc_type = doc_type.clone();
            async move {
                let n = window.len();
                let mut catalog = String::new();
                for (i, (start, end)) in window.iter().enumerate() {
                    let opening: String = chunks
                        .get(*start)
                        .map(|c| {
                            c.content
                                .chars()
                                .take(220)
                                .collect::<String>()
                                .replace('\n', " ")
                        })
                        .unwrap_or_default();
                    catalog
                        .push_str(&format!("#{} [chunks {start}..={end}] {opening}\n", base + i));
                }
                let prompt = format!(
                    "You are naming {n} segments of a {doc_type} document. Main document \
                     entities: {entity_list}. For EACH segment below write one line: \
                     <index>|<short title in the document's own register>|<function>, where \
                     function is one of Introduces, Develops, Complicates, Resolves, \
                     Transitions, Evidences. Output EXACTLY {n} lines in order, nothing else.\n\n\
                     Segments (index, chunk range, opening snippet):\n{catalog}\nAnswer ({n} lines):",
                    doc_type = doc_type.label(),
                );
                let mut start_rhs = String::from("line");
                for _ in 1..n {
                    start_rhs.push_str(" \"\\n\" line");
                }
                let lark_grammar = format!(
                    "start: {start_rhs}\n\
                     line: /[0-9]+/ \"|\" /[^|\\n]{{1,80}}/ \"|\" func\n\
                     func: \"Introduces\"|\"Develops\"|\"Complicates\"|\"Resolves\"|\"Transitions\"|\"Evidences\"\n",
                );
                // SLOT_POLICY §3 ExtractDurable: segment naming written to the
                // durable skeleton.
                let mut request = Workload::ExtractDurable.request(prompt)
                    .with_output_budget((((n * 24) + 40) as u32).min(2048));
                request.temperature = Some(0.1);
                request.lark_grammar = Some(lark_grammar);
                // POLICY-DEBT(SLOT_POLICY §3 ExtractDurable): Some(0) preserved for
                // P1 neutrality (bundle is None); P5 confirms.
                request.think_budget = Some(0);
                let mut call_titles: Vec<(String, SectionFunction)> =
                    match inference.complete(&request).await {
                        Ok(r) => parse_segment_title_lines(&r.text),
                        Err(_) => Vec::new(),
                    };
                call_titles.truncate(n);
                // Pad with placeholders so downstream position-matching stays
                // aligned even if a call under-delivered.
                while call_titles.len() < n {
                    call_titles.push((String::new(), SectionFunction::Develops));
                }
                call_titles
            }
        })
        .buffered(PASS_B_CONCURRENCY)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect();
    let mut segments = Vec::new();
    for (i, (start, end)) in ranges.into_iter().enumerate() {
        let (title, function) = match titles.get(i) {
            Some((t, f)) if !t.is_empty() => (t.clone(), f.clone()),
            _ => (
                format!("Segment chunks {start}..={end}"),
                SectionFunction::Develops,
            ),
        };
        segments.push(DocumentSegment {
            id: format!("seg-{start}"),
            chunk_start: start,
            chunk_end: end,
            title,
            summary: String::new(),
            key_entities: Vec::new(),
            function,
        });
    }

    segments
}

/// Parse the batched Pass-B `index|title|function` lines. Position in
/// the output is authoritative (the grammar forces one line per
/// segment, in order); the leading index is advisory and ignored.
pub(super) fn parse_segment_title_lines(text: &str) -> Vec<(String, SectionFunction)> {
    text.trim()
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '|');
            let _idx = parts.next()?;
            let title = parts.next()?.trim();
            if title.is_empty() {
                return None;
            }
            let function = match parts.next().map(str::trim).unwrap_or("Develops") {
                "Introduces" => SectionFunction::Introduces,
                "Complicates" => SectionFunction::Complicates,
                "Resolves" => SectionFunction::Resolves,
                "Transitions" => SectionFunction::Transitions,
                "Evidences" => SectionFunction::Evidences,
                _ => SectionFunction::Develops,
            };
            Some((title.to_string(), function))
        })
        .collect()
}
