// SPDX-License-Identifier: AGPL-3.0-or-later
//! The corpus-search fan-out: filter chain (kind / dim /
//! sensitivity / allow-list / principal ceiling) + concurrent
//! per-corpus hybrid search, and the allow-list / seal / rerank
//! helpers.

use std::collections::HashMap;

use super::super::*;

// ─── EXPERIMENTAL corpus relevance pre-filter ────────────────────────────
//
// Behind `SOVEREIGN_CORPUS_PREFILTER_TOPK`. An unscoped DeepQuery fans out to
// every installed corpus; this ranks corpora by relevance to the query and
// keeps only the top-K (+ guardrails) before the fan-out.
//
// The relevance signal is `CorpusIndex::nearest_vector_distance` — the true
// nearest-chunk cosine over the WHOLE index (threshold-free), which answers
// "does this corpus have ANY region near the query". `vector_distance` is the
// cross-corpus-comparable axis, so scores sort across corpora. Two cheaper
// signals were tried and rejected against the real 30-corpus set (2026-07-13):
// a mean-of-sample centroid and a max-of-sample — both biased, because
// `sample_embeddings(n)` is first-N in scan order, not a random sample, so a
// diffuse mega-corpus (wikipedia) mis-scored on its own questions. See
// [[project_corpus_prefilter_signal_2026_07_13]].

impl Runtime {
    /// Search every installed knowledge/catalog corpus with optional
    /// per-corpus K overrides (hot-corpora affinity pre-merge bias).
    /// When the conversation has already drawn many chunks from a
    /// corpus, we increase its candidate pool so the merge layer
    /// sees more of its top results. Per-corpus K defaults to
    /// `limit` for any corpus not in the override map.
    ///
    /// `enabled_corpora` is the user-controlled per-conversation
    /// allow-list (`Conversation::enabled_corpora`). `None` means
    /// "no filter — search every installed corpus" (the default
    /// behavior). `Some(allow)` drops every index whose `corpus_id`
    /// is absent from the allow-list, with one twist: an index whose
    /// `parent_corpus_id` is in the list is kept (layer/satellite
    /// corpora follow their parent). The filter applies AFTER the
    /// existing kind/dim/sensitivity filters so they can short-circuit
    /// without inspecting the allow-list.
    pub(crate) async fn search_corpus_indexes_with_overrides(
        &self,
        embedding: &[f32],
        query_text: &str,
        limit: usize,
        label: &str,
        per_corpus_limits: Option<&HashMap<String, usize>>,
        enabled_corpora: Option<&[String]>,
        corpus_ceiling: Option<&[String]>,
    ) -> Vec<corpus_engine::ScoredChunk> {
        let mut chunks = Vec::new();
        let engine = match &self.corpus_engine {
            Some(e) => e,
            None => {
                tracing::warn!("{label}: corpus_engine is None — no corpus search possible");
                return chunks;
            }
        };
        let indexes = match engine.installed_indexes().await {
            Ok(ix) => ix,
            Err(e) => {
                tracing::warn!(error = %e, "{label}: installed_indexes() failed");
                return chunks;
            }
        };
        if indexes.is_empty() {
            tracing::warn!("{label}: installed_indexes() returned 0 indexes — nothing to search");
        } else {
            tracing::info!(count = indexes.len(), "{label}: found corpus indexes");
        }

        // Filter 1 — drop Code corpora; keep Knowledge + Catalog.
        //
        // Code indexes (produced by `sovereign code index`) are served
        // by the dedicated symbol_lookup / code_search MCP tools;
        // pulling them into chat retrieval lets BM25 keyword overlap
        // on tokens like `main`, `argument`, or `democracy` drown out
        // the actual knowledge corpus for the turn.
        //
        // Catalog corpora are kept — they're the primary signal for
        // "system knows of this work but hasn't read it yet." The
        // synthesis prompt has a CATALOG-AWARE section that tells
        // the model how to handle them (no confabulation, end with
        // an ingest offer). `format_scored_chunks` buckets them
        // into a separate evidence tier downstream.
        let total_indexes = indexes.len();
        let indexes: Vec<_> = indexes
            .into_iter()
            .filter(|info| {
                if matches!(
                    info.kind,
                    corpus_engine::CorpusKind::Knowledge | corpus_engine::CorpusKind::Catalog
                ) {
                    true
                } else {
                    tracing::debug!(
                        corpus = %info.corpus_id,
                        kind = ?info.kind,
                        "{label}: skipping code corpus for chat retrieval"
                    );
                    false
                }
            })
            .collect();
        if indexes.len() < total_indexes {
            tracing::info!(
                knowledge = indexes.len(),
                code_skipped = total_indexes - indexes.len(),
                "{label}: filtered code corpora"
            );
        }

        // Filter 2 — drop dimension mismatches. A corpus built with
        // a different embedding model can't serve hybrid search for
        // the current query. When the query embedding is empty
        // (FTS-only path), skip this filter so every remaining
        // (knowledge) index serves its BM25 results.
        let query_dims = embedding.len();
        let total_indexes = indexes.len();
        let eligible: Vec<_> = indexes
            .into_iter()
            .filter(|info| {
                // Readiness: an index that never finished building (ingest
                // stalled / sync paused) has no searchable content — skip it on
                // EVERY path so the model can't fabricate over the void. The
                // readiness disclosure step surfaces a rebuild prompt when the
                // SCOPED corpus is the cause.
                if !info.indexes_built {
                    tracing::debug!(
                        corpus = %info.corpus_id,
                        "{label}: skipping corpus — index not built (rebuild/resume needed)"
                    );
                    return false;
                }
                // The vector + dimension checks apply only to the vector path
                // (query_dims != 0); the FTS-only path keeps every built index
                // so it can still serve its BM25 results.
                if query_dims != 0 {
                    if !info.vector_index_built {
                        tracing::debug!(
                            corpus = %info.corpus_id,
                            "{label}: skipping corpus — vector index missing (rebuild needed)"
                        );
                        return false;
                    }
                    if info.embedding_dimensions != query_dims {
                        tracing::debug!(
                            corpus = %info.corpus_id,
                            stored_dims = info.embedding_dimensions,
                            query_dims,
                            embedding_model = %info.embedding_model,
                            "{label}: skipping corpus — embedding-dimension mismatch"
                        );
                        return false;
                    }
                }
                true
            })
            .collect();
        if eligible.len() < total_indexes {
            tracing::info!(
                eligible = eligible.len(),
                skipped = total_indexes - eligible.len(),
                query_dims,
                "{label}: dim-filtered index set"
            );
        }

        // Filter 3 — drop sensitive corpora from ambient retrieval.
        //
        // Folder-ingest v1 §3.4: a watched-folder corpus marked
        // sensitive is structurally absent from the agent's pre-turn
        // ambient context. This is the runtime-side enforcement
        // layer; sovereign-tools' `WatchedFolderConfig.sensitive`
        // flag and its on-disk state-file mirror are the other
        // layers (ARCH §7.4 defence in depth). When no oracle is
        // wired (tests, pre-v1 builds), this filter is a no-op and
        // every corpus passes through.
        //
        // Sensitivity composes with skill-level local_only
        // suppression, but they're orthogonal: local_only is a
        // categorical skill gate; sensitivity is per-corpus and
        // applies in every register that does ambient retrieval.
        let eligible_pre_sensitivity = eligible.len();
        let eligible: Vec<_> = if let Some(oracle) = &self.sensitive_corpora {
            let sensitive_ids = oracle.sensitive_corpus_ids().await;
            if sensitive_ids.is_empty() {
                eligible
            } else {
                eligible
                    .into_iter()
                    .filter(|info| {
                        if sensitive_ids.contains(&info.corpus_id) {
                            tracing::debug!(
                                corpus = %info.corpus_id,
                                "{label}: skipping sensitive corpus — excluded from ambient retrieval"
                            );
                            false
                        } else {
                            true
                        }
                    })
                    .collect()
            }
        } else {
            eligible
        };
        if eligible.len() < eligible_pre_sensitivity {
            tracing::info!(
                eligible = eligible.len(),
                sensitive_skipped = eligible_pre_sensitivity - eligible.len(),
                "{label}: sensitivity-filtered index set"
            );
        }

        // Filter 4 — user-controlled per-conversation allow-list. Layer
        // corpora (info.parent_corpus_id matches an allowed parent) are
        // retained automatically so toggling Wikipedia ON enables
        // Wikipedia + its newsworthy/recent-events layers in one click.
        let eligible_pre_allow = eligible.len();
        let eligible = apply_corpus_allow_list(eligible, enabled_corpora);
        if eligible.len() < eligible_pre_allow {
            tracing::info!(
                eligible = eligible.len(),
                allow_skipped = eligible_pre_allow - eligible.len(),
                "{label}: corpus allow-list filtered index set"
            );
        }

        // Filter 5 — per-principal retrieval ceiling (multi-tenant hub).
        //
        // The AIRTIGHT upper bound, and the ONLY corpus filter that is a
        // security boundary. On a multi-tenant hub the server injects a
        // `PrincipalResolver`; `build_context` then stamps this conversation's
        // ceiling = `{Org corpora} ∪ {Private corpora the principal owns}`.
        // We re-apply the SAME parent-aware allow-list filter — but with the
        // ceiling, INDEPENDENT of the user-controlled `enabled_corpora`
        // (Filter 4). That independence is the whole point: Filter 4 is a
        // no-op on `None` (the default), so a client that sends no selection —
        // or forges one naming another tenant's Private corpus — is bounded
        // ONLY here. Filter 5 drops every index whose corpus (or parent
        // corpus) is outside the ceiling, so cross-tenant content can never
        // enter the merged pool regardless of what Filter 4 let through.
        //
        // `None` (single-user / desktop — no principal injected) ⇒ no-op, so
        // retrieval is bit-identical to pre-multi-tenant behaviour. A
        // `Some(empty)` ceiling (a principal with zero visible corpora)
        // correctly yields zero eligible indexes — fail-closed, not fail-open.
        // See `ConversationContext::corpus_ceiling`.
        let eligible_pre_ceiling = eligible.len();
        let eligible = apply_corpus_allow_list(eligible, corpus_ceiling);
        if eligible.len() < eligible_pre_ceiling {
            tracing::info!(
                target: "retrieval.isolation",
                label = %label,
                ceiling = ?corpus_ceiling,
                eligible_after = eligible.len(),
                ceiling_dropped = eligible_pre_ceiling - eligible.len(),
                "retrieval.isolation: principal-ceiling excluded cross-tenant indexes"
            );
        }

        // EXPERIMENTAL corpus relevance pre-filter (unscoped turns only): prune
        // to the top-K query-relevant corpora before the fan-out. No-op unless
        // `SOVEREIGN_CORPUS_PREFILTER_TOPK` is set. A scoped turn already
        // expressed intent via `enabled_corpora`, so we never second-guess it.
        let eligible = if enabled_corpora.is_none() {
            self.corpus_relevance_prefilter(engine, eligible, embedding, label)
                .await
        } else {
            eligible
        };

        // Per-corpus fan-out. Concurrency is env-gated
        // (`SOVEREIGN_KQ_FANOUT_CONCURRENCY`) and defaults to 4 (2026-06-26: was
        // the historical serial 1). This is BEHAVIOUR-IDENTICAL: every corpus
        // still pours into one merged pool that is sorted/capped downstream, so
        // concurrency changes only WALL-TIME, never results — which is what makes
        // raising it the SAFE way to bound an UNSCOPED turn's latency (no corpus
        // is dropped, so the answer's corpus is never lost). On a many-corpus
        // unscoped turn the serial fan-out was the dominant retrieval latency
        // (~2s/corpus × N ≈ 60s at N=29); 4-way concurrency collapses that ~4×
        // toward the slowest single corpus. Bounded at a moderate default (not
        // unbounded) so a wide fan-out can't thundering-herd the big indexes
        // (sep/wikipedia) on open + search. Per-corpus + total timing is emitted
        // on `retrieval_audit` so a run can prove where the latency went.
        use futures::StreamExt as _;
        let concurrency = std::env::var("SOVEREIGN_KQ_FANOUT_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(4);
        // Owned, shareable handles so each task captures only owned values (no
        // fn-scope borrow across the .await — that trips the higher-ranked-
        // lifetime / Send bound). We build a Vec of owned futures in a sync loop
        // (each one owns its clones) then drive them with bounded concurrency —
        // the closure-returning-async-block-over-`&info` form does NOT satisfy
        // the HRTB the stream wants, but a Vec of concrete futures does.
        let engine_arc = std::sync::Arc::clone(engine);
        let embedding_arc: std::sync::Arc<[f32]> = std::sync::Arc::from(embedding);
        let query_arc: std::sync::Arc<str> = std::sync::Arc::from(query_text);
        let label_arc: std::sync::Arc<str> = std::sync::Arc::from(label);
        let rerank_fn = self.rerank_fn.clone();
        let rerank_base = self.rerank_config.clone();
        let rerank_enabled = self.rerank_config.enabled && self.rerank_fn.is_some();
        let fanout_t0 = std::time::Instant::now();
        let mut tasks = Vec::with_capacity(eligible.len());
        for info in &eligible {
            let engine = std::sync::Arc::clone(&engine_arc);
            let embedding = std::sync::Arc::clone(&embedding_arc);
            let query_text = std::sync::Arc::clone(&query_arc);
            let label = std::sync::Arc::clone(&label_arc);
            let rerank_fn = rerank_fn.clone();
            let effective_limit = per_corpus_limits
                .and_then(|m| m.get(&info.corpus_id).copied())
                .unwrap_or(limit);
            // Per-corpus effective rerank config: opts this corpus into
            // source-dedup when its recipe declared it (recipe-driven SEP
            // promotion), otherwise the runtime base config unchanged.
            let corpus_rerank = rerank_config_for_corpus(&rerank_base, info);
            let corpus_id = info.corpus_id.clone();
            let path = info.path.clone();
            let chunk_count = info.chunk_count;
            let dims = info.embedding_dimensions;
            let embed_model = info.embedding_model.clone();
            tasks.push(async move {
                let corpus_t0 = std::time::Instant::now();
                tracing::info!(
                    corpus = %corpus_id,
                    path = %path.display(),
                    chunks = chunk_count,
                    dims = dims,
                    embedding_model = %embed_model,
                    "{label}: opening index"
                );
                let idx = match engine.open_index(&path).await {
                    Ok(i) => i,
                    Err(e) => {
                        tracing::warn!(corpus = %corpus_id, error = %e, "{label}: open_index failed");
                        return Vec::new();
                    }
                };
                if effective_limit != limit {
                    tracing::info!(
                        corpus = %corpus_id,
                        base_limit = limit,
                        effective_limit,
                        "{label}: per-corpus K override applied"
                    );
                }
                match idx
                    .search_with_rerank(
                        &embedding,
                        &query_text,
                        effective_limit,
                        rerank_fn.as_ref(),
                        &corpus_rerank,
                        None,
                    )
                    .await
                {
                    Ok(scored) => {
                        let elapsed_ms = corpus_t0.elapsed().as_millis() as u64;
                        tracing::info!(
                            corpus = %corpus_id,
                            results = scored.len(),
                            elapsed_ms,
                            rerank_enabled,
                            "{label}: search complete"
                        );
                        // Naturalistic audit: top-3 per corpus so post-mortem
                        // can answer "did the right article even reach the merge
                        // pool from this corpus?" before any cap or expansion.
                        let top3: Vec<(String, f32)> = scored
                            .iter()
                            .take(3)
                            .map(|c| (c.title.clone().unwrap_or_default(), c.score))
                            .collect();
                        tracing::info!(
                            target: "retrieval_audit",
                            event = "corpus_results",
                            label = %label,
                            corpus = %corpus_id,
                            count = scored.len(),
                            effective_limit,
                            elapsed_ms,
                            top3 = ?top3,
                            "retrieval_audit: corpus_results"
                        );
                        scored
                    }
                    Err(e) => {
                        tracing::warn!(corpus = %corpus_id, error = %e, "{label}: search failed");
                        Vec::new()
                    }
                }
            });
        }
        let per_corpus: Vec<Vec<corpus_engine::ScoredChunk>> = futures::stream::iter(tasks)
            .buffer_unordered(concurrency)
            .collect()
            .await;
        for scored in per_corpus {
            chunks.extend(scored);
        }
        tracing::info!(
            target: "retrieval_audit",
            event = "fanout_complete",
            label = label,
            corpora = eligible.len(),
            concurrency,
            fanout_ms = fanout_t0.elapsed().as_millis() as u64,
            merged = chunks.len(),
            "retrieval_audit: fanout_complete"
        );

        // Merged-pool diversity (glassbox, OPT-IN). After fan-out + merge
        // this is the candidate set synthesis will see. The regressed
        // bench title-coverage metric scores against the DISTINCT source
        // titles present here, so logging the merged distinct-title count
        // + the titles makes "did the expected articles survive the
        // merge?" answerable from logs alone. Pairs with the per-corpus
        // `rerank_diversity` event emitted inside search_with_rerank.
        //
        // Gated on the `retrieval_audit` target: when off (production
        // default) we pay one atomic level-check and skip the dedup pass
        // + title clones entirely. The work only runs under
        // `retrieval_audit=info`.
        if tracing::enabled!(target: "retrieval_audit", tracing::Level::INFO) {
            use std::collections::{HashMap, HashSet};
            let mut seen = HashSet::new();
            let mut distinct_titles: Vec<String> = Vec::new();
            let mut by_corpus: HashMap<String, usize> = HashMap::new();
            for c in &chunks {
                *by_corpus.entry(c.corpus_id.clone()).or_insert(0) += 1;
                let t = c.title.clone().unwrap_or_default();
                if seen.insert(t.clone()) {
                    distinct_titles.push(t);
                }
            }
            // Chunk counts per corpus, busiest first — the at-a-glance
            // cross-corpus-contamination signal (e.g. a wikipedia-target
            // turn whose pool is mostly `sep` chunks). This fan-out is
            // the shared retrieval entry point, so this single event
            // covers every handler — KnowledgeQuery, ComparisonQuery,
            // AND the DeepQuery/Simple path that has no turn_summary.
            let mut corpus_pairs: Vec<(String, usize)> = by_corpus.into_iter().collect();
            corpus_pairs.sort_by(|a, b| b.1.cmp(&a.1));
            // Truncated query so events correlate to the bench question
            // without threading an id through every call site.
            let query_preview: String = query_text.chars().take(80).collect();
            tracing::info!(
                target: "retrieval_audit",
                event = "merged_pool",
                label = label,
                query = %query_preview,
                merged_chunks = chunks.len(),
                distinct_titles = distinct_titles.len(),
                corpora_searched = eligible.len(),
                by_corpus = ?corpus_pairs,
                titles = ?distinct_titles,
                "retrieval_audit: merged_pool"
            );
        }
        chunks
    }

    /// EXPERIMENTAL (`SOVEREIGN_CORPUS_PREFILTER_TOPK`): prune an UNSCOPED
    /// eligible corpus set to the top-K most query-relevant corpora before the
    /// fan-out, ranked by query↔centroid cosine. Attacks the "unscoped
    /// DeepQuery searches every installed corpus" cost on two axes — fewer
    /// indexes opened/searched (wall-time) AND a smaller, more on-topic merge
    /// pool (which shrinks the synthesis prefill downstream).
    ///
    /// Guardrails, all fail-safe:
    /// - No-op unless the flag is set (`None`/`0` → return input unchanged).
    /// - Caller only invokes this for UNSCOPED turns; a scoped turn already
    ///   expressed intent via `enabled_corpora` and is never second-guessed.
    /// - No-op when the query vector is empty (FTS-only) or the set already
    ///   fits in K (nothing to prune).
    /// - ALWAYS keeps `personal_scope` corpora regardless of score.
    /// - Fails OPEN: a corpus whose sample can't be computed is KEPT, so a
    ///   cold/degraded corpus is never silently dropped from retrieval.
    /// Emits a `retrieval_audit` `corpus_prefilter` event with the kept/dropped
    /// corpora and their scores so a run can prove what was pruned and why.
    async fn corpus_relevance_prefilter(
        &self,
        engine: &std::sync::Arc<corpus_engine::CorpusEngine>,
        eligible: Vec<corpus_engine::IndexInfo>,
        query_embedding: &[f32],
        label: &str,
    ) -> Vec<corpus_engine::IndexInfo> {
        let top_k = match std::env::var("SOVEREIGN_CORPUS_PREFILTER_TOPK")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|k| *k >= 1)
        {
            Some(k) => k,
            None => return eligible, // flag off — production default
        };
        if query_embedding.is_empty() || eligible.len() <= top_k {
            return eligible;
        }
        let prefilter_t0 = std::time::Instant::now();

        let eligible_total = eligible.len();
        let mut n_personal = 0usize;
        // Corpora the probe couldn't score, tagged with why: `open`
        // (open_index failed), `probe_err` (search errored), `no_signal` (no
        // vector path). Each is fail-OPEN (kept); a large set here means weak
        // pruning from a probe gap, not genuine irrelevance, so we surface it.
        let mut failed_ann: Vec<(String, &'static str)> = Vec::new();
        let mut kept: Vec<corpus_engine::IndexInfo> = Vec::new();
        let mut ranked: Vec<(corpus_engine::IndexInfo, f32)> = Vec::new();
        for info in eligible {
            if info.personal_scope {
                n_personal += 1;
                kept.push(info); // user vaults are always kept
                continue;
            }
            // Nearest-chunk cosine over the WHOLE index — threshold-free, so a
            // weak-but-present match ranks low instead of vanishing. `open_index`
            // is handle-cached, sharing the handle the fan-out would open for a
            // kept corpus. `Ok(None)`/`Err` = unscoreable → fail-OPEN.
            let ann_sim = match engine.open_index(&info.path).await {
                Ok(idx) => match idx.nearest_vector_distance(query_embedding, 8).await {
                    Ok(Some(d)) => Some(1.0 - d),
                    Ok(None) => {
                        failed_ann.push((info.corpus_id.clone(), "no_signal"));
                        None
                    }
                    Err(_) => {
                        failed_ann.push((info.corpus_id.clone(), "probe_err"));
                        None
                    }
                },
                Err(_) => {
                    failed_ann.push((info.corpus_id.clone(), "open"));
                    None
                }
            };
            match ann_sim {
                Some(s) => ranked.push((info, s)),
                None => kept.push(info), // fail-open
            }
        }
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let r3 = |x: f32| (x * 1000.0).round() / 1000.0;
        let kept_ranked: Vec<(String, f32)> = ranked
            .iter()
            .take(top_k)
            .map(|(i, s)| (i.corpus_id.clone(), r3(*s)))
            .collect();
        let dropped: Vec<(String, f32)> = ranked
            .iter()
            .skip(top_k)
            .map(|(i, s)| (i.corpus_id.clone(), r3(*s)))
            .collect();
        for (info, _) in ranked.into_iter().take(top_k) {
            kept.push(info);
        }
        tracing::info!(
            target: "retrieval_audit",
            event = "corpus_prefilter",
            label = %label,
            top_k,
            eligible_total,
            n_ranked = kept_ranked.len() + dropped.len(),
            n_personal,
            n_failopen = failed_ann.len(),
            kept = kept.len(),
            dropped = dropped.len(),
            failed_ann = ?failed_ann,
            kept_ranked = ?kept_ranked,
            dropped_corpora = ?dropped,
            prefilter_ms = prefilter_t0.elapsed().as_millis() as u64,
            "retrieval_audit: corpus_prefilter"
        );
        kept
    }

    /// Search a *specific subset* of installed corpora — the
    /// metalingual companion to [`search_corpus_indexes`].
    ///
    /// Two filter axes:
    /// - `kind_filter`: if `Some`, restrict to that `CorpusKind`
    ///   (e.g. `Code` for SystemCode locators). If `None`, allow all
    ///   kinds (Knowledge + Code + Catalog).
    /// - `name_match`: if `Some`, restrict to corpora whose
    ///   `corpus_id` or `corpus_name` *contains* the substring (case-
    ///   insensitive). Used to resolve NamedSource locators like
    ///   "according to SEP" → only the `sep` corpus.
    ///
    /// Empty result is meaningful — caller treats it as "no source
    /// for this locator is indexed" and surfaces that to the user.
    pub(crate) async fn search_corpora_filtered(
        &self,
        embedding: &[f32],
        query_text: &str,
        limit: usize,
        kind_filter: Option<corpus_engine::CorpusKind>,
        name_match: Option<&str>,
        label: &str,
        enabled_corpora: Option<&[String]>,
        corpus_ceiling: Option<&[String]>,
    ) -> Vec<corpus_engine::ScoredChunk> {
        let mut chunks = Vec::new();
        let engine = match &self.corpus_engine {
            Some(e) => e,
            None => {
                tracing::warn!("{label}: corpus_engine is None");
                return chunks;
            }
        };
        let indexes = match engine.installed_indexes().await {
            Ok(ix) => ix,
            Err(e) => {
                tracing::warn!(error = %e, "{label}: installed_indexes() failed");
                return chunks;
            }
        };

        let name_lower = name_match.map(str::to_lowercase);
        let eligible: Vec<_> = indexes
            .into_iter()
            .filter(|info| {
                let kind_ok = match kind_filter {
                    Some(k) => info.kind == k,
                    None => true,
                };
                let name_ok = match &name_lower {
                    Some(needle) => {
                        info.corpus_id.to_lowercase().contains(needle)
                            || info.corpus_name.to_lowercase().contains(needle)
                    }
                    None => true,
                };
                kind_ok && name_ok
            })
            .filter(|info| {
                // Dim filter — skip embedding-mismatched corpora when
                // we have an embedding to compare against. Mirrors
                // search_corpus_indexes's filter 2.
                embedding.is_empty() || info.embedding_dimensions == embedding.len()
            })
            .collect();
        // Per-conversation allow-list — drop indexes the user has
        // toggled off. Layer corpora follow their parent's state.
        // See `apply_corpus_allow_list` for the parent-aware filter
        // contract.
        let eligible = apply_corpus_allow_list(eligible, enabled_corpora);
        // Filter 5 — per-principal retrieval ceiling (multi-tenant hub).
        // The independent, airtight bound — twin of the one in
        // `search_corpus_indexes_with_overrides`. `None` (single-user) ⇒
        // no-op. A forged/over-broad `name_match` or `enabled_corpora` (and
        // even a deliberate exemption like `bridge_boost`'s) cannot widen
        // retrieval past the principal's `{Org} ∪ {owned Private}` corpora.
        let eligible = apply_corpus_allow_list(eligible, corpus_ceiling);

        if eligible.is_empty() {
            tracing::info!(
                kind_filter = ?kind_filter,
                name_match = ?name_match,
                "{label}: no eligible corpora after filter"
            );
            return chunks;
        }

        for info in &eligible {
            tracing::info!(
                corpus = %info.corpus_id,
                kind = ?info.kind,
                "{label}: opening filtered index"
            );
            let idx = match engine.open_index(&info.path).await {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!(corpus = %info.corpus_id, error = %e, "{label}: open_index failed");
                    continue;
                }
            };
            let corpus_rerank = rerank_config_for_corpus(&self.rerank_config, info);
            match idx
                .search_with_rerank(
                    embedding,
                    query_text,
                    limit,
                    self.rerank_fn.as_ref(),
                    &corpus_rerank,
                    None,
                )
                .await
            {
                Ok(scored) => {
                    chunks.extend(scored);
                }
                Err(e) => {
                    tracing::warn!(corpus = %info.corpus_id, error = %e, "{label}: search failed");
                }
            }
        }
        chunks
    }
}

/// Build the effective [`corpus_engine::RerankConfig`] for a
/// single-corpus search. Starts from the runtime's base config (which may
/// carry an operator env-var override or a wired cross-encoder) and, when
/// the corpus's recipe declared `[retrieval] dedup_by_source` (surfaced on
/// `IndexInfo::dedup_by_source`), enables per-article source dedup for it.
///
/// This is the recipe-driven promotion of the SEP dedup lever (+6 sources,
/// 76%→85% on the eval bank, validated 2026-06-04): it now fires in every
/// runtime — desktop, server, CLI — with no env var. Corpora that don't
/// opt in are returned unchanged, so topical corpora (e.g. Wikipedia),
/// which regress under blind dedup, keep baseline behaviour.
fn rerank_config_for_corpus(
    base: &corpus_engine::RerankConfig,
    info: &corpus_engine::IndexInfo,
) -> corpus_engine::RerankConfig {
    if !info.dedup_by_source {
        return base.clone();
    }
    let mut cfg = base.clone();
    cfg.enabled = true;
    cfg.per_article = true;
    // Single-corpus search: a `None` filter means "every candidate
    // eligible", which here is exactly this (opted-in) corpus. Clearing any
    // operator-set filter avoids it excluding the very corpus that asked
    // for dedup; the cross-encoder (`rerank_fn`) is passed separately and
    // is unaffected.
    cfg.dedup_corpus_filter = None;
    cfg
}

/// Apply the per-conversation corpus allow-list to a pool of
/// `IndexInfo`. Each index passes when its `corpus_id` is in the
/// allow-list OR its `parent_corpus_id` is. The parent-aware branch
/// is what lets layer/satellite corpora (e.g. wikipedia-newsworthy
/// under wikipedia) follow their parent's enabled state without the
/// caller knowing the layer hierarchy. `None` is the no-filter
/// signal — every index passes, bit-identical to pre-feature
/// behavior.
/// Corpora present in `chunks` that fall OUTSIDE the conversation seal `allow`
/// (deduped). The read side of the isolate contract: `apply_corpus_allow_list`
/// keeps retrieval *in* the seal at fetch time; this detects any chunk that
/// nonetheless escaped it, across every injection path, for the DeepQuery
/// seal-audit trace. `conversation-history` is exempt (prior turns, not a
/// corpus source); `atlas:<corpus>` virtual chunks are checked against their
/// underlying `<corpus>`. Returns empty when `allow` is `None` (no seal) or the
/// seal holds.
pub(super) fn corpora_outside_seal<'a>(
    chunks: &'a [corpus_engine::ScoredChunk],
    allow: Option<&[String]>,
) -> Vec<&'a str> {
    let Some(allow) = allow else {
        return Vec::new();
    };
    corpora_outside_scope(chunks, allow)
}

/// Corpora present in `chunks` that are not in `scope` (deduped, sorted).
///
/// The scope-agnostic core of the bleed check. It exists separately from
/// [`corpora_outside_seal`] because that wrapper answers `None` ⇒ "no seal,
/// nothing to check", and **that is precisely the case that bleeds.**
///
/// An unscoped turn has no conversation seal, so the seal audit returned
/// empty and its caller was additionally guarded by `if let Some(allow)`.
/// The detector was therefore armed only under `--isolate` — the one
/// configuration that cannot bleed — and disarmed on the production path,
/// which is how a query-independent injector put 43-80% of an evidence pool
/// into unrelated corpora while every isolated run logged "corpus seal
/// intact" (audit `RETRIEVAL_AUDIT_2026-08-04.md`, D1 and its detector
/// blindness).
///
/// On an unscoped turn the meaningful baseline is not a seal but **the
/// corpora retrieval actually searched**: any corpus appearing in the final
/// pool that search never reached was put there by an injector, and that is
/// the whole bug class — regardless of which injector did it.
///
/// `conversation-history` is exempt (prior turns, not a corpus source);
/// `atlas:<corpus>` virtual chunks are checked against their underlying
/// `<corpus>`.
pub(crate) fn corpora_outside_scope<'a>(
    chunks: &'a [corpus_engine::ScoredChunk],
    scope: &[String],
) -> Vec<&'a str> {
    let scope_set: std::collections::HashSet<&str> = scope.iter().map(String::as_str).collect();
    let mut out: Vec<&str> = chunks
        .iter()
        .map(|c| c.corpus_id.as_str())
        .filter(|cid| {
            let base = cid.strip_prefix("atlas:").unwrap_or(cid);
            *cid != "conversation-history" && !scope_set.contains(base)
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    // Deterministic order: this feeds a log line that operators diff between
    // runs, and HashSet order is randomized per process.
    out.sort_unstable();
    out
}

fn apply_corpus_allow_list(
    indexes: Vec<corpus_engine::IndexInfo>,
    allow: Option<&[String]>,
) -> Vec<corpus_engine::IndexInfo> {
    let Some(allow) = allow else {
        return indexes;
    };
    let allow_set: std::collections::HashSet<&str> = allow.iter().map(String::as_str).collect();
    indexes
        .into_iter()
        .filter(|info| {
            allow_set.contains(info.corpus_id.as_str())
                || info
                    .parent_corpus_id
                    .as_deref()
                    .map(|p| allow_set.contains(p))
                    .unwrap_or(false)
        })
        .collect()
}

#[cfg(test)]
mod allow_list_tests {
    use super::super::raptor_grounding::raptor_scored_chunk;
    use super::apply_corpus_allow_list;
    use super::corpora_outside_scope;
    use super::corpora_outside_seal;
    use super::rerank_config_for_corpus;

    fn chunk(corpus: &str) -> corpus_engine::ScoredChunk {
        raptor_scored_chunk(
            "conv/1".to_string(),
            corpus.to_string(),
            0,
            "body".to_string(),
            0.5,
        )
    }

    /// THE GENERALISATION TEST for audit D1.
    ///
    /// Scoping each injector individually cannot prove the bug class is gone —
    /// there are six injection paths and more will be added. This audits the
    /// OUTCOME: any corpus in the final pool that retrieval never searched was
    /// put there by an injector, whichever one it was.
    ///
    /// The chunks below are the real D1 shape: search reached `sep`, and
    /// `sep-chinese-logic-language` appeared anyway.
    #[test]
    fn unsealed_turn_still_detects_bleed_against_searched_corpora() {
        let searched = vec!["sep".to_string()];
        let pool = vec![
            chunk("sep"),
            chunk("sep-chinese-logic-language"),
            chunk("sep-compatibilism"),
        ];
        let bleed = corpora_outside_scope(&pool, &searched);
        assert_eq!(
            bleed,
            vec!["sep-chinese-logic-language", "sep-compatibilism"],
            "an unscoped turn must still report corpora search never reached"
        );
    }

    /// The blindness this replaces: the seal check answers `None` ⇒ "nothing
    /// to report", which is why D1 ran undetected in production while every
    /// `--isolate` run logged "corpus seal intact". Pinned so nobody
    /// "simplifies" the audit back onto the seal helper.
    #[test]
    fn seal_helper_is_blind_without_a_seal_which_is_why_scope_exists() {
        let pool = vec![chunk("sep"), chunk("sep-chinese-logic-language")];
        assert!(
            corpora_outside_seal(&pool, None).is_empty(),
            "documented behaviour of the SEAL helper: no seal ⇒ no finding"
        );
        assert!(
            !corpora_outside_scope(&pool, &["sep".to_string()]).is_empty(),
            "the SCOPE helper must find what the seal helper structurally cannot"
        );
    }

    /// `conversation-history` is prior turns, not a corpus source; `atlas:`
    /// virtuals are judged by their underlying corpus. Neither may be
    /// reported as bleed or the warning becomes noise operators learn to skip.
    #[test]
    fn scope_audit_exempts_history_and_resolves_atlas_virtuals() {
        let searched = vec!["sep".to_string()];
        let pool = vec![
            chunk("conversation-history"),
            chunk("atlas:sep"),
            chunk("atlas:wikipedia"),
        ];
        assert_eq!(
            corpora_outside_scope(&pool, &searched),
            vec!["atlas:wikipedia"]
        );
    }

    /// Deterministic output: this feeds a log line operators diff between
    /// runs, and the underlying set has randomized iteration order — the same
    /// randomness that made D1 pick a different off-topic corpus per process.
    #[test]
    fn scope_audit_output_is_order_stable() {
        let searched = vec!["sep".to_string()];
        let pool = vec![chunk("zeta"), chunk("alpha"), chunk("mid"), chunk("alpha")];
        let first = corpora_outside_scope(&pool, &searched);
        assert_eq!(first, vec!["alpha", "mid", "zeta"]);
        for _ in 0..8 {
            assert_eq!(corpora_outside_scope(&pool, &searched), first);
        }
    }

    #[test]
    fn dedup_by_source_corpus_opts_into_per_article() {
        // Baseline: a corpus that did NOT declare `[retrieval]
        // dedup_by_source` is returned the runtime's base config unchanged
        // (no dedup) — preserves Wikipedia-shape behaviour.
        let base = corpus_engine::RerankConfig::default();
        assert!(!base.enabled, "precondition: base config is disabled");

        let plain = idx("wikipedia", None); // idx() sets dedup_by_source = false
        let cfg_plain = rerank_config_for_corpus(&base, &plain);
        assert!(!cfg_plain.enabled);
        assert!(!cfg_plain.per_article);

        // Opted-in corpus (SEP): per-article source dedup is enabled even
        // though the runtime base config is off and no reranker is wired.
        let mut opted = idx("sep", None);
        opted.dedup_by_source = true;
        let cfg = rerank_config_for_corpus(&base, &opted);
        assert!(cfg.enabled, "opted-in corpus enables the dedup path");
        assert!(
            cfg.per_article,
            "opted-in corpus requests per-article dedup"
        );
        assert!(
            cfg.dedup_corpus_filter.is_none(),
            "single-corpus search clears any operator filter so this corpus is eligible"
        );
    }

    fn idx(id: &str, parent: Option<&str>) -> corpus_engine::IndexInfo {
        corpus_engine::IndexInfo {
            corpus_id: id.to_string(),
            corpus_name: id.to_string(),
            path: std::path::PathBuf::new(),
            chunk_count: 0,
            index_size_bytes: 0,
            created_at: 0,
            last_updated: 0,
            embedding_model: String::new(),
            embedding_dimensions: 0,
            mesh_sharing: false,
            query_sharing: false,
            dedup_by_source: false,
            personal_scope: false,
            grantable: false,
            is_shard: false,
            chunk_range: None,
            chunks_expected: None,
            resume_from: None,
            enrichment_requested: false,
            enriched_chunks: None,
            source_version: None,
            update_manifest_url: None,
            kind: corpus_engine::CorpusKind::Knowledge,
            parent_corpus_id: parent.map(String::from),
            indexes_built: true,
            vector_index_built: true,
            canonical_fingerprint: None,
            total_shards: None,
            processed_shards: Vec::new(),
            mutable_merge: None,
            stream: None,
            display: None,
        }
    }

    #[test]
    fn none_passes_everything() {
        let pool = vec![idx("wikipedia", None), idx("sep", None)];
        let out = apply_corpus_allow_list(pool.clone(), None);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn allow_list_filters_to_subset() {
        let pool = vec![
            idx("wikipedia", None),
            idx("sep", None),
            idx("gutenberg", None),
        ];
        let allow = vec!["sep".to_string()];
        let out = apply_corpus_allow_list(pool, Some(&allow));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].corpus_id, "sep");
    }

    #[test]
    fn parent_pulls_in_layers() {
        let pool = vec![
            idx("wikipedia", None),
            idx("wikipedia-newsworthy", Some("wikipedia")),
            idx("sep", None),
        ];
        let allow = vec!["wikipedia".to_string()];
        let out = apply_corpus_allow_list(pool, Some(&allow));
        let ids: Vec<_> = out.iter().map(|i| i.corpus_id.as_str()).collect();
        assert_eq!(ids, vec!["wikipedia", "wikipedia-newsworthy"]);
    }

    #[test]
    fn empty_allow_filters_everything() {
        let pool = vec![idx("wikipedia", None), idx("sep", None)];
        let allow: Vec<String> = vec![];
        let out = apply_corpus_allow_list(pool, Some(&allow));
        assert!(out.is_empty());
    }

    #[test]
    fn corpora_outside_seal_flags_only_disallowed() {
        let chunks = vec![
            raptor_scored_chunk("c1".into(), "wikipedia".into(), 0, "a".into(), 0.9),
            raptor_scored_chunk("c2".into(), "sep".into(), 0, "b".into(), 0.8),
            // `atlas:sep` is a virtual chunk over the `sep` corpus.
            raptor_scored_chunk("c3".into(), "atlas:sep".into(), 0, "c".into(), 0.7),
            raptor_scored_chunk(
                "c4".into(),
                "conversation-history".into(),
                0,
                "d".into(),
                0.6,
            ),
        ];
        // No seal → nothing flagged.
        assert!(corpora_outside_seal(&chunks, None).is_empty());

        // Sealed to `sep`: only `wikipedia` bleeds. `sep`, its `atlas:` virtual,
        // and conversation-history are all in-seal / exempt.
        let allow_sep = vec!["sep".to_string()];
        assert_eq!(
            corpora_outside_seal(&chunks, Some(&allow_sep)),
            vec!["wikipedia"]
        );

        // Sealed to `wikipedia`: `sep` bleeds AND `atlas:sep` bleeds (its
        // underlying corpus is outside the seal).
        let allow_wiki = vec!["wikipedia".to_string()];
        let mut bleed = corpora_outside_seal(&chunks, Some(&allow_wiki));
        bleed.sort_unstable();
        assert_eq!(bleed, vec!["atlas:sep", "sep"]);
    }
}
