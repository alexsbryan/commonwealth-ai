// SPDX-License-Identifier: AGPL-3.0-or-later
//! Entity-typed atom enumeration + overview/summary Claim
//! grounding — atlas-directed retrieval for enumeration-class
//! and overview-class questions.

use std::collections::HashMap;

use super::super::*;

impl Runtime {
    /// Entity-typed atom enumeration for enumeration-class questions.
    ///
    /// The failure mode this targets generalizes well beyond any one
    /// corpus: a question that asks for a *set* of same-typed entities
    /// the user never names —
    ///   "which energy companies were counterparties"   → institution
    ///   "who were the executives involved"             → person
    ///   "what themes recur across these essays"        → concept
    /// — embeds into a single query vector that collapses onto the ONE
    /// dominant member of the set. (Measured on the Enron mail: a
    /// counterparty question retrieved 15/28 Dynegy chunks and zero of
    /// the five other companies the answer needs — though Williams
    /// alone carries 128 atoms in-corpus. The facts are present; the
    /// query has no handle on them.) LLM query-expansion
    /// (`expand_question_to_titles`) cannot rescue this: it can only
    /// name entities the question already implies, and an enumeration
    /// question names none. The set the user wants *is* the corpus's
    /// own typed atom graph — so enumerate it directly.
    ///
    /// Two corpus-agnostic stages:
    ///   1. One Fast-slot classify call: enumeration or lookup, and if
    ///      enumeration, over which `EntityType`. Biased to LOOKUP —
    ///      enumeration is the marked, higher-bar case — so an
    ///      already-focused lookup is never polluted with atom noise.
    ///      That pollution (firing on non-enumeration questions) is the
    ///      exact regression that sank the first atom-grounding attempt;
    ///      the gate + the lookup bias are the structural fix.
    ///   2. Rank the `Entity` atoms of that type by GRAPH CENTRALITY
    ///      (edge degree) and take the top-K, one focused sub-query per
    ///      atom name. Degree is the prominence signal that actually
    ///      discriminates: this corpus's `salience` is a flat 0.70
    ///      default (no signal) and post-reconciliation every name is
    ///      frequency-1, but edge degree separates the real cast (Enron
    ///      1096, Lay 923, Dynegy 59) from address-book noise (~0), and
    ///      centrality generalizes across atlas corpora. Atoms are read
    ///      from the in-memory atlas GRAPH — *not* the role-filtered
    ///      context bag. The graph (`AtlasGraph::load_from_disk`) holds
    ///      every atom unconditionally; the embed bag drops no-role
    ///      institutions, which is why the earlier attempt could not see
    ///      El Paso / Calpine. Ranking by degree (never *filtering* on
    ///      role) keeps those no-role-but-real entities in play.
    ///
    /// The sub-queries are fanned out + decayed through the shared
    /// `fan_out_decomposed_queries` helper, identical to title-expand,
    /// so they augment rather than displace strong base hits.
    ///
    /// Opt-in via `SOVEREIGN_ATOM_ENUM=1` (off by default; un-gating
    /// needs the cross-corpus validation TITLE_EXPAND got). Top-K via
    /// `SOVEREIGN_ATOM_ENUM_TOPK` (default 16).
    ///
    /// Returns `None` when: the gate is off, no atlas provider is
    /// attached, the classify call fails or parses empty, the model
    /// says lookup, or the enabled corpora hold no atoms of the chosen
    /// type. Caller proceeds without enumeration in every case.
    /// `pool_corpora`: the corpora already present in this turn's retrieved
    /// chunks. Used as the scope when `enabled_corpora` is absent — see
    /// [`resolve_atom_enum_scope`] for why an unscoped turn must NOT fall
    /// back to every installed corpus.
    pub(crate) async fn enumerate_typed_atom_chunks(
        &self,
        message: &str,
        enabled_corpora: Option<&[String]>,
        corpus_ceiling: Option<&[String]>,
        pool_corpora: &[String],
    ) -> Option<Vec<corpus_engine::ScoredChunk>> {
        let atom_enum_on = std::env::var("SOVEREIGN_ATOM_ENUM").ok().as_deref() == Some("1");
        // Default ON (parity push — surface atlas Claims for overview questions
        // in desktop + bench). Set SOVEREIGN_ATOM_ENUM_OVERVIEW=0 to disable.
        let overview_on = std::env::var("SOVEREIGN_ATOM_ENUM_OVERVIEW")
            .ok()
            .as_deref()
            != Some("0");
        if !atom_enum_on && !overview_on {
            return None;
        }
        // Overview/summary path (default ON; SOVEREIGN_ATOM_ENUM_OVERVIEW=0 disables).
        // An overview question ("what is the most important thing in X", "give
        // me an overview / summary of …") names no entity to enumerate, so the
        // entity classifier below would (correctly) decline — and the answer
        // then abstains or confabulates over a diffuse, anchorless chunk pool.
        // But the corpus's atlas Claim atoms ARE its key points; inject them as
        // grounding so the answer is built from the corpus's own assertions.
        // Detected by question shape (no LLM call); returns before the
        // enumerate classify.
        if overview_on && Self::looks_like_overview(message) {
            return self
                .enumerate_overview_claim_chunks(
                    message,
                    enabled_corpora,
                    corpus_ceiling,
                    pool_corpora,
                )
                .await;
        }
        if !atom_enum_on {
            return None;
        }
        // Need the atlas graph to enumerate against; bail before the
        // classify call if no provider is attached — otherwise we would
        // pay an LLM round-trip only to find nothing to enumerate.
        let provider = self.atlas_context_provider.as_ref()?;

        // ---- Stage 1: classify enumeration vs lookup (+ target type).
        // Question-shape only, no conversation context: whether a
        // question enumerates a set is a property of its phrasing, not
        // the dialogue around it, and a tight prompt keeps the Fast
        // call fast. Examples are deliberately domain-neutral so the
        // classifier learns the enumerate/lookup distinction, not this
        // corpus's vocabulary.
        let prompt = format!(
            "Classify the question on ONE axis: ENUMERATE or LOOKUP.\n\n\
             ENUMERATE — its core ask is for MULTIPLE same-typed entities (a \
             LIST of several) that the question does NOT name — it asks \
             WHICH or WHO without naming the members, expecting the set as \
             the answer. The requested category must be PLURAL: people, \
             companies / organizations, places, concepts, works. A trailing \
             descriptive clause (\"… and what each did\", \"… and how they \
             relate\") does NOT change this; the core ask is still the set, \
             so it is still ENUMERATE.\n\
             - \"which organizations were involved\" -> enumerate / institution\n\
             - \"who were the members, and what did each contribute\" -> enumerate / person\n\
             - \"what concepts do these texts discuss\" -> enumerate / concept\n\
             - \"what places are mentioned\" -> enumerate / place\n\n\
             LOOKUP — it asks for ONE entity, NAMES the specific entit(ies) \
             it is about, or asks to explain / describe / justify a specific \
             thing or event. The decisive test: if the question already \
             names the entities it concerns, it is LOOKUP — it investigates \
             those named things, it does NOT enumerate an unknown set. This \
             holds even when SEVERAL entities are named and even when the \
             phrasing is plural (\"the X and Y partnerships\"). Asking \
             \"which/who\" about a SINGLE entity is LOOKUP, not enumerate.\n\
             - \"who led the negotiation\" (one entity) -> lookup\n\
             - \"what does this say about a specific named deal\" (names its subject) -> lookup\n\
             - \"what do these reveal about the Alpha and Beta partnerships\" (names its subjects, even though several) -> lookup\n\
             - \"describe the agreement\" -> lookup\n\
             - \"why did the project fail\" -> lookup\n\n\
             If enumerate, name the entity_type from: person, institution, \
             initiative, concept, work, place.\n\n\
             Question: {message}\n\n\
             Output only this JSON, nothing after it:\n\
             {{\"mode\": \"enumerate\", \"entity_type\": \"institution\"}}"
        );

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "mode": {"type": "string", "enum": ["enumerate", "lookup"]},
                "entity_type": {
                    "type": "string",
                    "enum": ["person", "institution", "initiative", "concept", "work", "place"]
                }
            },
            "required": ["mode"]
        });

        let request = CompletionRequest {
            prompt,
            system_message: None,
            preferred_speed: Speed::Fast,
            max_tokens: Some(40),
            temperature: Some(0.0),
            think_budget: Some(0),
            structured_output: Some(schema),
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
            prompt_shape: None,
            stable_prefix_len: None,
        };

        let response = match self.inference.complete(&request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "atom_enum: Fast-slot classify call failed; skipping enumeration"
                );
                return None;
            }
        };
        let raw = response.text.trim();
        // Glassbox: log the model's actual classify output for EVERY
        // question, before parsing. The enumerate/lookup decision is the
        // load-bearing gate; this line makes "why did (didn't) atom-enum
        // fire here" inspectable in one grep, and surfaces ramble-past-
        // JSON (the Fast slot's known failure: `{...}\n\nWait, let…`).
        tracing::info!(
            target: "retrieval_audit",
            event = "atom_enum_classify",
            query = %truncate_with_ellipsis(message, 80),
            raw = %truncate_with_ellipsis(raw, 240),
            "retrieval_audit: atom_enum classify raw"
        );
        // Tolerate ramble-past-JSON: take the first balanced {...} object
        // rather than requiring the whole reply to be valid JSON.
        let json_str = extract_first_json_object(raw).unwrap_or_else(|| {
            raw.strip_prefix("```json")
                .and_then(|s| s.strip_suffix("```"))
                .unwrap_or(raw)
                .trim()
                .to_string()
        });
        let parsed: serde_json::Value = match serde_json::from_str(&json_str) {
            Ok(v) => v,
            Err(e) => {
                tracing::info!(
                    error = %e,
                    raw = %raw,
                    "atom_enum: classify parse failed; skipping enumeration"
                );
                return None;
            }
        };
        // Bias to lookup: anything that is not an explicit `enumerate`
        // verdict (including a missing/garbled mode) is treated as a
        // lookup and short-circuits — no atom noise on focused queries.
        if parsed.get("mode").and_then(|v| v.as_str()) != Some("enumerate") {
            return None;
        }
        let target_type = parsed
            .get("entity_type")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())?
            .to_string();

        // ---- Stage 2: enumerate top-salience atoms of that type from
        // the atlas GRAPH. The graph is failure-immune by construction:
        // `AtlasGraph::load_from_disk` inserts every atom, so no-role
        // institutions the embed bag would drop are present here.
        let top_k: usize = std::env::var("SOVEREIGN_ATOM_ENUM_TOPK")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&k| k > 0 && k <= 100)
            .unwrap_or(16);

        // Use the enabled-corpora list directly when scoped. The atlas
        // GRAPH can be loaded even when its embedding-bag CONTEXT isn't:
        // a freshly re-enriched atlas has a new atoms.json but a stale
        // embeddings cache, so `load_one` skips the context — yet
        // `AtlasGraph::load_from_disk` still loaded the graph. Keying off
        // `loaded_corpus_ids()` (contexts only) would drop exactly that
        // corpus (observed: enron-sample-multi-wide right after re-enrich
        // → corpora=[] → no enumeration). `provider.graph(id)` below
        // returns None for any id that genuinely has no graph, so an
        // unscoped fallback to loaded contexts is still safe.
        // ONE decider for this scope — see `resolve_atom_enum_scope`. An
        // absent scope resolves to the corpora this turn actually reached,
        // never to every corpus installed on the box.
        let corpus_ids: Vec<String> = resolve_atom_enum_scope(enabled_corpora, pool_corpora);

        // Prominence per atom: graph degree (in + out edges), tie-broken
        // by alias count then salience. Degree is the real signal — this
        // corpus's salience is a flat 0.70 default and post-reconciliation
        // frequency is uniformly 1, but degree separates the real cast
        // (Lay 923, Dynegy 59 edges) from address-book noise (~0).
        // Graceful: alias/salience only break ties, and cover corpora
        // whose atlas has no edges.json (every degree 0).
        #[derive(Clone)]
        struct Candidate {
            prominence: (usize, usize), // (degree, alias_count)
            salience: f32,
            corpus: String,
            chunk_id: String, // first_appearance.chunk_id (numeric OR "sec_NNNN")
            preview: Option<String>, // passage_preview — FTS key for section-shaped ids
            embed_text: String, // "name. description" — relevance-rank key
        }
        let outranks = |a: &Candidate, b: &Candidate| -> bool {
            a.prominence.cmp(&b.prominence) == std::cmp::Ordering::Greater
                || (a.prominence == b.prominence && a.salience > b.salience)
        };
        // Dedup by canonical name (cross-corpus + intra-corpus variants),
        // keeping the most-prominent record. The cap then bounds the
        // injection regardless of how many atoms of a type the corpus
        // holds (4,525 institutions here, most address-book noise).
        let filter_disabled = std::env::var("SOVEREIGN_ATOM_ENUM_NOFILTER")
            .ok()
            .as_deref()
            == Some("1");
        // Relation-evidence candidates (default on; SOVEREIGN_ATOM_ENUM_RELATIONS=0
        // to ablate). See the relation loop below for the rationale.
        let include_relations = std::env::var("SOVEREIGN_ATOM_ENUM_RELATIONS")
            .ok()
            .as_deref()
            != Some("0");
        let mut best: HashMap<String, Candidate> = HashMap::new();
        for id in &corpus_ids {
            let Some(graph) = provider.graph(id) else {
                continue;
            };
            for view in graph.atoms_of_kind(corpus_engine::enrichment::atlas::AtomType::Entity) {
                if view.subtype() != target_type {
                    continue;
                }
                let name = view.name().trim();
                if name.is_empty() {
                    continue;
                }
                // Collective-noun filter (person enumeration). The
                // extractor sometimes types group phrases as person
                // entities ("Enron executives", "Enron management",
                // "Enron analysts"). These paraphrase a "who were the
                // executives" question, so cosine ranks them highly, yet
                // they name no individual and pollute the enumerated set
                // (and crowd out real people like Fastow). A real
                // individual's name never contains a generic group noun;
                // drop person atoms whose name does. Person-only:
                // institutions legitimately contain "Committee"/"Board"
                // (e.g. the Special Committee on Related Party
                // Transactions that actually investigated LJM). Env hatch
                // SOVEREIGN_ATOM_ENUM_NOFILTER=1 disables for ablation.
                if target_type == "person" && !filter_disabled {
                    const GROUP_NOUNS: &[&str] = &[
                        "executives",
                        "executive",
                        "management",
                        "mgmt",
                        "employees",
                        "employee",
                        "team",
                        "staff",
                        "analysts",
                        "analyst",
                        "representatives",
                        "representative",
                        "board",
                        "directors",
                        "director",
                        "members",
                        "member",
                        "officials",
                        "official",
                        "personnel",
                        "leadership",
                        "committee",
                        "everyone",
                        "people",
                        "folks",
                        "others",
                    ];
                    let lname = name.to_lowercase();
                    if lname
                        .split(|c: char| !c.is_alphanumeric())
                        .any(|tok| GROUP_NOUNS.contains(&tok))
                    {
                        continue;
                    }
                }
                let degree = graph.edge_degree(view.id());
                let desc = view.description().trim();
                let embed_text = if desc.is_empty() {
                    name.to_string()
                } else {
                    format!("{name}. {desc}")
                };
                // An Entity's evidence is its single `first_appearance` ref.
                let first = view.evidence().next();
                let cand = Candidate {
                    prominence: (degree, view.alias_count()),
                    salience: view.salience(),
                    corpus: id.clone(),
                    chunk_id: first
                        .as_ref()
                        .map(|ev| ev.chunk_id().to_string())
                        .unwrap_or_default(),
                    preview: first
                        .as_ref()
                        .map(|ev| ev.passage_preview().to_string())
                        .filter(|s| !s.is_empty()),
                    embed_text,
                };
                best.entry(name.to_string())
                    .and_modify(|cur| {
                        if outranks(&cand, cur) {
                            *cur = cand.clone();
                        }
                    })
                    .or_insert(cand);
            }

            // Relation-evidence candidates. For PREDICATE enumerations
            // ("which energy companies are COUNTERPARTIES / competitors")
            // the answer set is defined by a RELATIONSHIP, not an entity
            // type — and an entity's first_appearance chunk proves only
            // that it exists, not that it holds the relationship (so the
            // counterparty turn could name Calpine but never ground it as
            // a counterparty). Relation atoms carry the relationship-
            // bearing evidence chunk directly ("beat out Reliant and TXU",
            // "competing for partnership", "potential acquisition target
            // of"). We add them to the same candidate pool and let the
            // relevance/RRF re-rank surface them when the query is
            // relational — the relation's `label + participants` embeds
            // near the predicate, and the fetched evidence chunk STATES
            // the relationship. On non-relational ("who were the X")
            // queries relations cosine-rank low and the entity atoms win,
            // so this is additive, not a regression to entity enumeration.
            // Keyed by display string so identical relations dedup without
            // colliding with entity names.
            if include_relations {
                for view in
                    graph.atoms_of_kind(corpus_engine::enrichment::atlas::AtomType::Relation)
                {
                    let label = view.label().trim();
                    // First evidence ref grounds the relationship; skip
                    // relations with no label or no evidence (same guard as
                    // the former `label.is_empty() || r.evidence.is_empty()`).
                    let Some(ev) = view.evidence().next() else {
                        continue;
                    };
                    if label.is_empty() {
                        continue;
                    }
                    let parts: Vec<String> = view
                        .participants()
                        .filter_map(|pid| graph.atom(pid))
                        .filter(|a| a.kind() == corpus_engine::enrichment::atlas::AtomType::Entity)
                        .filter_map(|a| {
                            let n = a.name().trim();
                            (!n.is_empty()).then(|| n.to_string())
                        })
                        .collect();
                    let display = if parts.is_empty() {
                        label.to_string()
                    } else {
                        format!("{label} ({})", parts.join(", "))
                    };
                    let embed_text = if parts.is_empty() {
                        label.to_string()
                    } else {
                        format!("{label}. {}", parts.join(", "))
                    };
                    let cand = Candidate {
                        // Relations carry no graph degree; cosine rank is
                        // their only RRF signal, which is exactly what a
                        // predicate query rewards.
                        prominence: (0, 0),
                        salience: 0.5,
                        corpus: id.clone(),
                        chunk_id: ev.chunk_id().to_string(),
                        preview: {
                            let p = ev.passage_preview();
                            (!p.is_empty()).then(|| p.to_string())
                        },
                        embed_text,
                    };
                    best.entry(display).or_insert(cand);
                }
            }
        }
        if best.is_empty() {
            tracing::info!(
                target: "retrieval_audit",
                event = "atom_enum_empty",
                target_type = %target_type,
                corpora = ?corpus_ids,
                "atom_enum: no atoms of chosen type in enabled corpora; skipping"
            );
            return None;
        }

        let mut ranked: Vec<(String, Candidate)> = best.into_iter().collect();
        // Base order: prominence (degree) desc, salience, name asc. This
        // is the deterministic fallback when the embedder is unavailable,
        // and the prefilter base when the type-pool exceeds the cost cap.
        ranked.sort_by(|a, b| {
            b.1.prominence
                .cmp(&a.1.prominence)
                .then_with(|| {
                    b.1.salience
                        .partial_cmp(&a.1.salience)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.0.cmp(&b.0))
        });

        // Cost bound for wiki-scale atlases: cap the pool we embed. Enron
        // (284 institutions / 622 persons) is far under the default 800,
        // so this is a no-op here; it stops a 50k-atom type from issuing
        // a 50k-text embed batch. Prefilter is by degree (keeps the real
        // cast); logged so the truncation is never silent.
        let pool_cap: usize = std::env::var("SOVEREIGN_ATOM_ENUM_POOL")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(800);
        let pool_truncated = ranked.len() > pool_cap;
        if pool_truncated {
            ranked.truncate(pool_cap);
        }

        // HYBRID RE-RANK (default = RRF). Neither raw signal generalizes
        // across entity types:
        //   - DEGREE alone ranks the custodian's ego-network (high-degree
        //     address-book hubs — United Way, Moody's) above the sparse
        //     orgs an institution question enumerates (Calpine / El Paso /
        //     Williams, LJM / Marlin).
        //   - RELEVANCE (cosine) alone ranks query-PARAPHRASE atoms
        //     ("Enron executives", "Enron upper mgmt") above the real
        //     people a person question enumerates (Lay / Skilling /
        //     Fastow), because the question text embeds nearest to atoms
        //     that restate it rather than answer it.
        // Reciprocal Rank Fusion (k=60, the codebase's hybrid-search
        // idiom) demands BOTH: a real answer entity ranks well on at
        // least one signal and decently on the other, beating junk that
        // spikes on only one. RRF is also robust to degree's extreme skew
        // (Lay ~923 edges dwarfs every other person), where a linear blend
        // would let one hub crush the normalisation. Embedding is
        // on-the-fly (not the precomputed bag) because a re-enriched atlas
        // has a stale embeddings cache (atoms.json newer than
        // atoms.embeddings.bin). Env hatch SOVEREIGN_ATOM_ENUM_RANK ∈
        // {rrf (default), relevance, degree}; any embedder failure falls
        // back to degree order.
        let rank_mode = std::env::var("SOVEREIGN_ATOM_ENUM_RANK").unwrap_or_else(|_| "rrf".into());
        let mut ranked_by = "degree";
        if rank_mode != "degree" && !ranked.is_empty() {
            // `ranked` is already degree-sorted, so position == degree rank.
            let texts: Vec<String> = ranked.iter().map(|(_, c)| c.embed_text.clone()).collect();
            match (
                self.inference.embed_query(message).await,
                self.inference.embed_batch(&texts).await,
            ) {
                (Ok(q), Ok(embs)) if embs.len() == ranked.len() && !q.is_empty() => {
                    let n = ranked.len();
                    let cosines: Vec<f32> = (0..n)
                        .map(|i| crate::atlas_context::cosine(&q, &embs[i]))
                        .collect();
                    if rank_mode == "relevance" {
                        // Pure cosine (ablation).
                        let mut order: Vec<usize> = (0..n).collect();
                        order.sort_by(|&a, &b| {
                            cosines[b]
                                .partial_cmp(&cosines[a])
                                .unwrap_or(std::cmp::Ordering::Equal)
                                .then_with(|| ranked[a].0.cmp(&ranked[b].0))
                        });
                        ranked = order.into_iter().map(|i| ranked[i].clone()).collect();
                        ranked_by = "relevance";
                    } else {
                        // RRF of degree rank (position) + cosine rank.
                        let mut by_cos: Vec<usize> = (0..n).collect();
                        by_cos.sort_by(|&a, &b| {
                            cosines[b]
                                .partial_cmp(&cosines[a])
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });
                        let mut cos_rank = vec![0usize; n];
                        for (r, &i) in by_cos.iter().enumerate() {
                            cos_rank[i] = r;
                        }
                        const RRF_K: f32 = 60.0;
                        let rrf = |i: usize| -> f32 {
                            1.0 / (RRF_K + i as f32) + 1.0 / (RRF_K + cos_rank[i] as f32)
                        };
                        let mut order: Vec<usize> = (0..n).collect();
                        order.sort_by(|&a, &b| {
                            rrf(b)
                                .partial_cmp(&rrf(a))
                                .unwrap_or(std::cmp::Ordering::Equal)
                                .then_with(|| ranked[a].0.cmp(&ranked[b].0))
                        });
                        ranked = order.into_iter().map(|i| ranked[i].clone()).collect();
                        ranked_by = "rrf";
                    }
                }
                _ => { /* embedder unavailable / dim mismatch → keep degree order */ }
            }
        }
        tracing::info!(
            target: "retrieval_audit",
            event = "atom_enum_rank",
            target_type = %target_type,
            ranked_by,
            pool = ranked.len(),
            pool_truncated,
            "atom_enum: candidate ranking ({ranked_by})"
        );
        ranked.truncate(top_k);

        // Inject the enumerated entities DIRECTLY as compact virtual
        // chunks (name + role + description) rather than fanning out a
        // re-search per atom. The atom metadata already carries the
        // answer — "Kenneth Lay — Chairman and Chief Executive" — so one
        // dense item per atom surfaces the fact without the N×limit
        // chunk flood that displaces base hits (measured −0.33 on a
        // person enumeration when re-searching). Scored descending by
        // rank so the most-central members sit highest; SOVEREIGN_ATOM_
        // ENUM_SCORE tunes the band relative to base cosine hits.
        let enum_score: f32 = std::env::var("SOVEREIGN_ATOM_ENUM_SCORE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|&s| s > 0.0)
            .unwrap_or(0.04);

        // Atlas DIRECTS retrieval. For each enumerated entity, fetch its
        // REAL evidence chunk (`first_appearance.chunk_id`) from the
        // corpus index rather than synthesising a name+role virtual
        // chunk. This is the load-bearing fix and the architectural
        // contract: the enrichment graph says WHICH chunks the question
        // needs; the normal pipeline then ranks them. Real chunks carry
        // real content + a real `chunk_id`, so — unlike virtual chunks —
        // they survive corpus-isolation, dedup, and the synthesis
        // snapshot, and they earn their slot via `reweight_by_query_
        // relevance` on actual text instead of a hand-set score (which
        // reweight would clobber anyway). Resolution is shape-aware:
        // numeric chunk_id → direct LanceDB fetch; section-shaped id
        // ("sec_0001", the modern pipelines) → FTS the passage_preview
        // for the evidence chunk (per atom). An unresolvable atom is a
        // no-op.
        let mut chunks: Vec<corpus_engine::ScoredChunk> = Vec::new();
        let mut fetched_names: Vec<&str> = Vec::new();
        for (i, (name, c)) in ranked.iter().enumerate() {
            // Shape-aware resolution to a REAL chunk. Numeric chunk_id
            // (legacy corpus-mode atoms) → direct LanceDB fetch.
            // Section-shaped id ("sec_0001", the modern pipelines) has no
            // direct row → FTS the corpus for its passage_preview and
            // take the top hit (the atom's evidence chunk). Either way
            // the result is a real chunk (real id + content) that earns
            // its rank via reweight and survives the pipeline.
            let mut fetched = match c.chunk_id.trim().parse::<u64>() {
                Ok(cid) => self.fetch_chunk_by_id(&c.corpus, cid).await,
                Err(_) => None,
            };
            if fetched.is_none() {
                if let Some(pv) = c
                    .preview
                    .as_deref()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                {
                    // Preview-FTS rescue scoped to the atom's OWN corpus —
                    // its evidence lives there by construction. Searching
                    // the whole enabled set was the dominant retrieval cost
                    // of the 2026-07-21 soak (2,647 cross-corpus searches /
                    // 5,949 cumulative s in 90 min; 31 corpora touched per
                    // turn) and could attach the WRONG corpus's chunk as
                    // evidence. An atom whose corpus is outside the enabled
                    // set gets no rescue — that corpus isn't in play.
                    let own_scope = [c.corpus.clone()];
                    if enabled_corpora.is_none_or(|en| en.contains(&c.corpus)) {
                        fetched = self
                            .search_corpus_indexes_with_overrides(
                                &[],
                                pv,
                                1,
                                "AtomEnum",
                                None,
                                Some(&own_scope[..]),
                                corpus_ceiling,
                            )
                            .await
                            .into_iter()
                            .next();
                    }
                }
            }
            let Some(mut chunk) = fetched else {
                continue;
            };
            // Seed score; reweight overwrites it from real content
            // overlap. The taper only orders ties before reweight runs.
            chunk.score = enum_score * 0.96_f32.powi(i as i32);
            chunk
                .metadata
                .insert("source".to_string(), "atom-enum".to_string());
            chunk
                .metadata
                .insert("atom_entity".to_string(), name.clone());
            chunk
                .metadata
                .insert("entity_type".to_string(), target_type.clone());
            chunks.push(chunk);
            fetched_names.push(name.as_str());
        }
        if chunks.is_empty() {
            tracing::info!(
                target: "retrieval_audit",
                event = "atom_enum_nofetch",
                target_type = %target_type,
                candidates = ranked.len(),
                sample = ?ranked.iter().take(3).map(|(n, c)| format!("{n}|cid={}|pv={}", c.chunk_id, c.preview.is_some())).collect::<Vec<_>>(),
                "atom_enum: candidates found but no evidence chunks fetched"
            );
            return None;
        }

        tracing::info!(
            target: "retrieval_audit",
            event = "atom_enum",
            query = %truncate_with_ellipsis(message, 120),
            entity_type = %target_type,
            count = chunks.len(),
            names = ?fetched_names,
            "retrieval_audit: atom_enum directed-fetch"
        );
        Some(chunks)
    }

    /// Overview/summary grounding: inject the scoped corpus's atlas Claim
    /// atoms as compact virtual chunks. An overview question has no entity
    /// anchor, so normal retrieval returns a diffuse pool the grounding gate
    /// can't tie to "the most important thing" — and the answer abstains or
    /// confabulates a theme. The atlas Claims ARE the corpus's key points
    /// (e.g. maple-house's 67 charter rules), pre-extracted and grounded by
    /// construction. Tagged `source=atom-enum` so the `cap_and_reserve`
    /// atom-enum reserve carries them through truncation (their reweight score
    /// is irrelevant — an overview query has no lexical anchor to reweight on).
    /// Claims that carry a verbatim `quotable_excerpt` rank first (the answer
    /// can quote the corpus's own words); `confidence` breaks ties. Returns
    /// `None` when the scoped corpus holds no Claim atoms (entity-only atlas,
    /// or none) so the caller falls through to the normal pool. Gated by
    /// `SOVEREIGN_ATOM_ENUM_OVERVIEW`.
    async fn enumerate_overview_claim_chunks(
        &self,
        message: &str,
        enabled_corpora: Option<&[String]>,
        corpus_ceiling: Option<&[String]>,
        pool_corpora: &[String],
    ) -> Option<Vec<corpus_engine::ScoredChunk>> {
        let provider = self.atlas_context_provider.as_ref()?;
        let top_k: usize = std::env::var("SOVEREIGN_ATOM_ENUM_TOPK")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&k| k > 0 && k <= 100)
            .unwrap_or(16);
        let enum_score: f32 = std::env::var("SOVEREIGN_ATOM_ENUM_SCORE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|&s| s > 0.0)
            .unwrap_or(0.04);
        // ONE decider for this scope — see `resolve_atom_enum_scope`. An
        // absent scope resolves to the corpora this turn actually reached,
        // never to every corpus installed on the box.
        let corpus_ids: Vec<String> = resolve_atom_enum_scope(enabled_corpora, pool_corpora);
        struct ClaimCand {
            content: String,
            excerpt: Option<String>,
            corpus: String,
            has_excerpt: bool,
            confidence: f32,
            // Evidence pointer (the claim's first ChunkRef) — lets the injector
            // resolve the claim to its REAL source chunk (MAP) instead of
            // injecting the atom's paraphrased `content` (DATA). `None` for
            // derived claims that carry no evidence.
            evidence_chunk_id: Option<String>,
            evidence_preview: Option<String>,
            has_evidence: bool,
            /// Fraction of the question's content tokens this claim's own text
            /// mentions, in [0, 1]. The ONLY query-conditioned term in the
            /// ranking below — see the floor comment for why it has to exist.
            relevance: f32,
        }
        // Query-relevance floor for the injection (2026-08-05).
        //
        // `step_atom_enum` runs POST noise-floor on purpose (retrieval_pipeline.rs
        // :1140) so these metadata chunks survive a filter built for prose, and
        // `merge_demand_select` then PINS every `source=atom-enum` chunk
        // unconditionally (merge_select.rs:121). Nothing downstream can reject an
        // irrelevant claim once it is injected — so the selection here is the last
        // and only place topicality can be checked.
        //
        // It was not being checked. The ranking below was (has_evidence,
        // has_excerpt, confidence) with no query term at all — `message` reached
        // this function and was used only in a tracing event — so for a fixed pool
        // the SAME claims were injected for EVERY overview question, and the shared
        // top-k was swept by whichever corpus had the richest ATLAS rather than the
        // one the question was about. Measured on `bench sep/summarize`
        // (14 questions, 420 chunks): `commonwealth-ai-arch-principles` contributed
        // 5-8 pinned chunks to 14 of 14 questions — 89 total, 21% of the pool —
        // with three identical titles in all fourteen, alongside the user's private
        // Obsidian vault and chat archive. Off-topic total: 116/420 = 27.6%.
        //
        // The scope guard alone cannot carry this. `pool_corpora` grants injection
        // rights on the presence of ONE surviving chunk, and the post-floor pool is
        // 533 RRF-fused chunks in which everything scores ~0.03 — measured, it puts
        // `chaos-saltgrass` and `arch-principles` at the TOP for a question about
        // idealism. Pool standing is not a topicality signal; query overlap is.
        // Same `extract_tokens` decider `reweight_by_query_relevance` already
        // applies to every other chunk (ARCH §10.6 — one decider for relevance),
        // which scores these claims ~0.035 against ~0.70 for on-topic evidence.
        // The system already knew; only the pin ignored it.
        //
        // Deliberately a floor at >0, not a tuned threshold: a claim that shares no
        // content word with the question is what we are removing, and any stronger
        // cut risks the obscure-entry case where the atlas IS the useful signal
        // (killing the feature outright costs sep-summarize-obscure 0.83 -> 0.78
        // fact recall, which is why the fix is an admission rule and not a flag).
        let query_tokens = overview_topic_tokens(message);
        let mut cands: Vec<ClaimCand> = Vec::new();
        for id in &corpus_ids {
            let Some(graph) = provider.graph(id) else {
                continue;
            };
            for view in graph.atoms_of_kind(corpus_engine::enrichment::atlas::AtomType::Claim) {
                let content = view.content().trim();
                if content.is_empty() {
                    continue;
                }
                let excerpt = {
                    let e = view.excerpt().trim();
                    (!e.is_empty()).then(|| e.to_string())
                };
                let ev = view.evidence().next();
                cands.push(ClaimCand {
                    relevance: claim_query_relevance(content, excerpt.as_deref(), &query_tokens),
                    content: content.to_string(),
                    has_excerpt: excerpt.is_some(),
                    excerpt,
                    corpus: id.clone(),
                    confidence: view.confidence(),
                    evidence_chunk_id: ev.as_ref().map(|e| e.chunk_id().to_string()),
                    evidence_preview: ev.as_ref().and_then(|e| {
                        let p = e.passage_preview();
                        (!p.is_empty()).then(|| p.to_string())
                    }),
                    has_evidence: ev.is_some(),
                });
            }
        }
        if cands.is_empty() {
            return None;
        }
        // THE ADMISSION TEST IS PER CORPUS, NOT PER CLAIM.
        //
        // The question this path has to answer is "is this corpus what the user
        // is asking about?", and that is a property of the corpus, not of any
        // one claim: a corpus's key points legitimately include claims that
        // restate none of the question's words. Testing each claim instead
        // (the first cut of this fix, measured 2026-08-05) admits a corpus on a
        // single generic word — "argument" from *the cosmological argument*
        // matched `commonwealth-ai-system-overview` claims — and then RANKS
        // those matches first. Measured: off-topic went UP, 27.6% -> 33.6%,
        // with the freed slots refilled by whichever corpus happened to share a
        // common noun. Per-claim relevance survives as the ORDERING key inside
        // an admitted corpus; it is not the gate.
        //
        // A corpus is admitted when its BEST claim covers at least
        // `TOPIC_GRIP` of the question's topic tokens — i.e. the corpus can
        // show one key point that is recognisably about what was asked. A
        // majority (>0.5) is the default: a claim sharing one noun out of three
        // is coincidence, one sharing two of three is aboutness.
        //
        // The case this exists to produce is INJECTING NOTHING. On
        // `bench sep/summarize` the resolved scope is nine Commonwealth/chaos
        // corpora and does NOT contain `sep` — the corpus the questions are
        // actually about — so no corpus can clear the bar and the correct
        // output is an empty injection. That the scope lacks `sep` at all is a
        // SEPARATE and deeper defect (the scope is taken from a 533-chunk
        // unranked pool, not from the evidence the answer will use); this gate
        // stops that defect from putting private notes in a philosophy answer,
        // it does not fix it. See note `8758759a`.
        let topic_grip: f32 = std::env::var("SOVEREIGN_ATOM_ENUM_TOPIC_GRIP")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|g| (0.0..=1.0).contains(g))
            .unwrap_or(0.5);
        let mut best_by_corpus: HashMap<String, f32> = HashMap::new();
        for c in cands.iter() {
            let e = best_by_corpus.entry(c.corpus.clone()).or_insert(0.0);
            if c.relevance > *e {
                *e = c.relevance;
            }
        }
        let admitted: std::collections::HashSet<String> = best_by_corpus
            .iter()
            .filter(|(_, best)| **best > topic_grip)
            .map(|(id, _)| id.clone())
            .collect();
        // SAY WHAT WAS DROPPED, per corpus. A silent filter here would be
        // indistinguishable from "that corpus had no claims" — and the whole
        // reason this defect survived two audits is that bulk admission looked
        // like retrieval (ARCH §0.1 glassbox, §18.3 never silently substitute).
        let before = cands.len();
        let mut dropped_by_corpus: HashMap<String, usize> = HashMap::new();
        for c in cands.iter().filter(|c| !admitted.contains(&c.corpus)) {
            *dropped_by_corpus.entry(c.corpus.clone()).or_insert(0) += 1;
        }
        cands.retain(|c| admitted.contains(&c.corpus));
        if cands.len() < before {
            let mut dropped: Vec<(String, usize)> = dropped_by_corpus.into_iter().collect();
            dropped.sort_by(|a, b| b.1.cmp(&a.1));
            let mut grip: Vec<(String, f32)> = best_by_corpus.into_iter().collect();
            grip.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            tracing::info!(
                target: "retrieval_audit",
                event = "atom_enum_topic_gate",
                query = %truncate_with_ellipsis(message, 120),
                topic_grip = topic_grip,
                topic_tokens = ?query_tokens,
                candidates_before = before,
                kept = cands.len(),
                admitted_corpora = ?admitted,
                best_relevance_by_corpus = ?grip,
                dropped_by_corpus = ?dropped,
                "retrieval_audit: atom_enum overview claims gated — corpus must show one key point about the question"
            );
        }
        if cands.is_empty() {
            // No corpus in scope is about this question. Injecting nothing is
            // the correct outcome — the normal pool already holds this turn's
            // evidence, and this is the expected result whenever the on-topic
            // corpus has no atlas or was never in scope.
            return None;
        }
        // Rank query relevance FIRST, then the grounding-quality tiebreakers:
        // claims that carry a resolvable evidence chunk (→ MAP to a real source
        // chunk), then claims with a verbatim quote, then higher extraction
        // confidence. Relevance leads because the tiebreakers measure how good a
        // claim is *as a claim*, which is a property of the corpus's enrichment
        // and not of the question — ranking on them first is what let one
        // richly-enriched corpus sweep the shared top-k on every question.
        cands.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.has_evidence.cmp(&a.has_evidence))
                .then_with(|| b.has_excerpt.cmp(&a.has_excerpt))
                .then_with(|| {
                    b.confidence
                        .partial_cmp(&a.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        cands.truncate(top_k);
        let mut chunks: Vec<corpus_engine::ScoredChunk> = Vec::with_capacity(cands.len());
        let mut seen_chunk_ids: std::collections::HashSet<(String, u64)> =
            std::collections::HashSet::new();
        let mut mapped = 0usize;
        for (i, c) in cands.iter().enumerate() {
            // Gentle taper preserves the evidence/quote/confidence order before
            // reweight; the cap reserve (not the score) carries these through
            // truncation, and reweight overwrites it from real content on the
            // MAP chunks.
            let seed_score = enum_score * 0.99_f32.powi(i as i32);

            // MAP-first: resolve the claim's evidence to its REAL source chunk —
            // the SAME shape-aware resolution the entity-enumeration path uses
            // (numeric chunk_id → direct LanceDB fetch; section-shaped id → FTS
            // the passage_preview for the evidence chunk). The answer then
            // grounds on the article's actual text with a real chunk_id, not the
            // atom's propositional paraphrase, and the real chunk survives
            // dedup + the synthesis snapshot. DATA injection is the fallback ONLY
            // when the claim has no resolvable evidence (derived claims, stale
            // ids) — so an overview corpus still gets its key points either way.
            let resolved = match c
                .evidence_chunk_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(cid) => {
                    let mut got = match cid.parse::<u64>() {
                        Ok(n) => self.fetch_chunk_by_id(&c.corpus, n).await,
                        Err(_) => None,
                    };
                    if got.is_none() {
                        if let Some(pv) = c
                            .evidence_preview
                            .as_deref()
                            .map(str::trim)
                            .filter(|p| !p.is_empty())
                        {
                            // Same own-corpus scoping as the AtomEnum
                            // rescue above — see that comment for the
                            // 2026-07-21 soak measurements. The claim's
                            // evidence chunk can only live in c.corpus.
                            let own_scope = [c.corpus.clone()];
                            if enabled_corpora.is_none_or(|en| en.contains(&c.corpus)) {
                                got = self
                                    .search_corpus_indexes_with_overrides(
                                        &[],
                                        pv,
                                        1,
                                        "AtomEnumOverview",
                                        None,
                                        Some(&own_scope[..]),
                                        corpus_ceiling,
                                    )
                                    .await
                                    .into_iter()
                                    .next();
                            }
                        }
                    }
                    got
                }
                None => None,
            };

            if let Some(mut chunk) = resolved {
                // Claims often cluster on one section — skip a duplicate evidence
                // chunk (keep the higher-ranked claim's). dedupe_merged would
                // catch content dupes downstream too, but this avoids a redundant
                // fetch occupying an atom-enum reserve slot.
                if let Some(cid) = chunk.chunk_id {
                    if !seen_chunk_ids.insert((chunk.corpus_id.clone(), cid)) {
                        continue;
                    }
                }
                chunk.score = seed_score;
                chunk
                    .metadata
                    .insert("source".to_string(), "atom-enum".to_string());
                chunk
                    .metadata
                    .insert("atom_type".to_string(), "claim".to_string());
                chunks.push(chunk);
                mapped += 1;
            } else {
                // DATA fallback: inject the claim text (+ verbatim quote when
                // present). Tagged `atom_claim_unmapped` so the glassbox shows
                // which overview chunks are synthetic vs resolved-to-source.
                let content = match &c.excerpt {
                    Some(q) => format!("{}\n\nSource quote: \"{}\"", c.content, q),
                    None => c.content.clone(),
                };
                let mut metadata = std::collections::HashMap::new();
                metadata.insert("source".to_string(), "atom-enum".to_string());
                metadata.insert("atom_type".to_string(), "claim".to_string());
                metadata.insert("atom_claim_unmapped".to_string(), "1".to_string());
                chunks.push(corpus_engine::ScoredChunk {
                    content,
                    title: Some(format!("{} — key point", c.corpus)),
                    url: None,
                    corpus_id: c.corpus.clone(),
                    score: seed_score,
                    metadata,
                    chunk_id: None,
                    source_doc_id: None,
                    vector_distance: None,
                });
            }
        }
        if chunks.is_empty() {
            return None;
        }
        tracing::info!(
            target: "retrieval_audit",
            event = "atom_enum_overview",
            query = %truncate_with_ellipsis(message, 120),
            count = chunks.len(),
            mapped_to_real_chunks = mapped,
            data_fallback = chunks.len() - mapped,
            corpora = ?corpus_ids,
            "retrieval_audit: atom_enum overview-claim injection (MAP-first; DATA fallback for unresolvable claims)"
        );
        Some(chunks)
    }

    /// Question-shape heuristic for the overview/summary claim path. No LLM
    /// call — a corpus-level "what matters here" ask is recognisable from
    /// phrasing alone, and keeping it cheap means it can run on every turn the
    /// flag is set. Deliberately broad: a false positive just augments the
    /// pool with the corpus's key points (bounded, atom-enum-tagged), which is
    /// harmless; a false negative falls through to normal retrieval.
    fn looks_like_overview(message: &str) -> bool {
        let m = message.to_lowercase();
        OVERVIEW_MARKERS.iter().any(|k| m.contains(k))
    }
}

/// The phrasings that mark a question as overview-class. Hoisted to module
/// scope because TWO deciders need them and they must never disagree
/// (ARCH §10.6): `looks_like_overview` uses them to ENTER the claim-injection
/// path, and `overview_framing_tokens` subtracts them from the query before
/// measuring topical relevance — a word that decided the question is an
/// overview cannot also count as evidence of what it is an overview OF.
const OVERVIEW_MARKERS: &[&str] = &[
    "most important",
    "summar", // summary / summarize / summarise
    "overview",
    "main point",
    "main idea",
    "main theme",
    "main takeaway",
    "key point",
    "key idea",
    "key theme",
    "key takeaway",
    "the gist",
    "tell me about",
    "what is this about",
    "what's this about",
    "what is it about",
    "what are these about",
    "high level",
    "high-level",
];

/// Generic request scaffolding — the verbs and nouns by which someone ASKS for
/// an overview, carrying no subject matter. Separate from [`OVERVIEW_MARKERS`]
/// because these do not classify the question; they merely pad it.
///
/// This list is short and deliberately conservative. It is not a stopword list:
/// every entry has to be a word that cannot distinguish one overview question
/// from another, because anything broader starts discarding real topic signal.
const OVERVIEW_FRAMING_EXTRAS: &[&str] = &[
    "provide",
    "comprehensive",
    "cover",
    "give",
    "explain",
    "describe",
    "discuss",
    "outline",
    "detailed",
    "brief",
];

/// The query tokens that carry no topic on the overview path — the union of
/// [`OVERVIEW_MARKERS`] and [`OVERVIEW_FRAMING_EXTRAS`], tokenised on the same
/// decider as everything else.
///
/// Exists because of a measured false positive: the bench question "Provide a
/// comprehensive **overview** of idealism…" contributed the token `overview`,
/// which then matched the `commonwealth-ai-arch-principles` claim
/// "**SYSTEM_OVERVIEW**.md is a contract" — an off-topic claim clearing a
/// topical floor on the very word that routed the question here. Subtracting
/// the framing is what makes the floor measure subject matter.
fn overview_framing_tokens() -> std::collections::HashSet<String> {
    OVERVIEW_MARKERS
        .iter()
        .chain(OVERVIEW_FRAMING_EXTRAS.iter())
        .flat_map(|m| {
            crate::runtime::evidence::extract_tokens(
                m,
                crate::runtime::evidence::EVIDENCE_TITLE_MIN_TOKEN_LEN,
            )
        })
        // `summar` is a PREFIX marker ("summary"/"summarise"), so it never
        // survives tokenisation as a whole word — name its family explicitly.
        .chain(
            ["summary", "summarize", "summarise", "summarizing"]
                .iter()
                .map(|s| s.to_string()),
        )
        .collect()
}

/// Which corpora an atom-enumeration pass may draw claims from.
///
/// THE INVARIANT, and the reason this function exists rather than an
/// inline `match` at each call site: **an absent scope means "the corpora
/// this turn actually reached", never "every corpus installed on the
/// box."**
///
/// Both enumeration paths previously resolved `None` to
/// `provider.discoverable_corpus_ids()` — deliberately the widest set
/// available ("every registered atlas dir, not just the bags warmed so
/// far"). That is unsafe *here specifically* because atom enumeration is
/// **query-independent**: it collects every Claim atom of every corpus it
/// is given and ranks them by extraction metadata (`has_evidence`,
/// `has_excerpt`, `confidence`) with no reference to the message or its
/// embedding. So every extra corpus in this list contributes claims that
/// no downstream relevance test can remove — and the injected chunks are
/// pinned ahead of the real pool, post-noise-floor by design.
///
/// Measured consequence before this fix (audit `RETRIEVAL_AUDIT_2026-08-04.md`
/// D1): a "summarize proof theory" question drew 43–80% of its evidence
/// pool from unrelated corpora — `sep-chinese-logic-language`,
/// `sep-atheism-agnosticism`, `sep-compatibilism`, `sep-kant-metaphysics`
/// on different runs. Which one won varied per process, because the
/// ranking key is query-independent and `HashMap` iteration order broke
/// the ties (audit D2).
///
/// `pool_corpora` is the corpus set already present in the turn's
/// retrieved chunks — the same signal `apply_atlas_grounding` derives at
/// `atlas_grounding.rs:104-113`. An empty result is correct and means "no
/// corpus was reached": the caller then enumerates nothing.
/// The question's TOPIC tokens: its content tokens minus the overview framing.
///
/// Empty is a meaningful answer — "give me an overview" is pure framing and
/// names no subject — and [`claim_query_relevance`] treats it as "cannot
/// measure, so admit" rather than dropping every candidate.
pub(crate) fn overview_topic_tokens(message: &str) -> Vec<String> {
    let framing = overview_framing_tokens();
    crate::runtime::evidence::extract_tokens(
        message,
        crate::runtime::evidence::EVIDENCE_TITLE_MIN_TOKEN_LEN,
    )
    .into_iter()
    .filter(|t| !framing.contains(t))
    .collect()
}

/// How much of the question this atlas Claim actually talks about: the
/// fraction of `query_tokens` that appear in the claim's own text (plus its
/// verbatim quote when it has one), in `[0, 1]`.
///
/// This is the ONLY query-conditioned signal in the overview-claim selector,
/// and it exists because that selector had none — it ranked on
/// `(has_evidence, has_excerpt, confidence)`, all properties of how well a
/// corpus was ENRICHED rather than of what was ASKED, so the same claims were
/// injected for every overview question and the richest atlas swept the shared
/// top-k. See `enumerate_overview_claim_chunks` for the measurement.
///
/// `query_tokens` must come from
/// [`crate::runtime::evidence::extract_tokens`] — the same decider
/// `reweight_by_query_relevance` applies to every non-injected chunk, so an
/// injected claim and a retrieved chunk are judged on one scale (ARCH §10.6).
///
/// Returns `1.0` for an empty token set (an all-stopword question): the floor
/// abstains rather than dropping every candidate on a signal it cannot compute.
pub(crate) fn claim_query_relevance(
    content: &str,
    excerpt: Option<&str>,
    query_tokens: &[String],
) -> f32 {
    if query_tokens.is_empty() {
        return 1.0;
    }
    let mut hay = content.to_lowercase();
    if let Some(e) = excerpt {
        hay.push(' ');
        hay.push_str(&e.to_lowercase());
    }
    let hits = query_tokens
        .iter()
        .filter(|q| hay.contains(q.as_str()))
        .count();
    hits as f32 / query_tokens.len() as f32
}

pub(crate) fn resolve_atom_enum_scope(
    enabled_corpora: Option<&[String]>,
    pool_corpora: &[String],
) -> Vec<String> {
    // An explicit scope is authoritative and is never widened HERE. Note
    // this is not the same as "the turn is airtight": `apply_corpus_allow_list`
    // (corpus_search.rs:785-792) separately admits any LAYER corpus whose
    // `parent_corpus_id` is allowed, so scoping to `wikipedia` still searches
    // `wikipedia-newsworthy`. That is a deliberate desktop-toggle behaviour and
    // a measurement hazard for anything calling itself isolated — see audit
    // D3a. What this function guarantees is narrower and still worth having:
    // enumeration never reaches a corpus the caller did not name.
    if let Some(enabled) = enabled_corpora {
        if !enabled.is_empty() {
            return enabled.to_vec();
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    pool_corpora
        .iter()
        .filter(|c| !c.is_empty())
        .filter(|c| seen.insert((*c).clone()))
        .cloned()
        .collect()
}

fn extract_first_json_object(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let start = s.find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    // start and i both index ASCII bytes ('{' / '}'),
                    // so this slice never splits a UTF-8 code point.
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod scope_tests {
    use super::{claim_query_relevance, overview_topic_tokens, resolve_atom_enum_scope};

    fn q_tokens(q: &str) -> Vec<String> {
        overview_topic_tokens(q)
    }

    /// The framing must be subtracted, or the floor measures the wrong thing.
    /// This is the exact false positive that broke the first cut of the fix.
    #[test]
    fn overview_framing_is_not_topic() {
        let t = overview_topic_tokens(
            "Provide a comprehensive overview of idealism as a philosophical position.",
        );
        assert!(
            !t.contains(&"overview".to_string()),
            "`overview` is how the question ASKS, not what it asks about: {t:?}"
        );
        assert!(!t.contains(&"provide".to_string()), "{t:?}");
        assert!(!t.contains(&"comprehensive".to_string()), "{t:?}");
        assert!(
            t.contains(&"idealism".to_string()),
            "topic must survive: {t:?}"
        );
        // The regression in one line: the arch-principles claim that beat the
        // first cut of this floor did so ONLY on the framing word.
        assert_eq!(
            claim_query_relevance("SYSTEM_OVERVIEW.md is a contract, not a diary", None, &t),
            0.0
        );
    }

    /// A pure-framing question names no subject. The token set is empty and the
    /// floor must abstain — this is the "tell me about this corpus" case the
    /// injection exists to serve, and failing closed would disable it.
    #[test]
    fn pure_framing_question_yields_no_topic_tokens() {
        assert!(overview_topic_tokens("Give me a high-level summary.").is_empty());
        assert_eq!(
            claim_query_relevance("anything", None, &overview_topic_tokens("What's the gist?")),
            1.0
        );
    }

    /// THE FAILING INPUT THIS FLOOR EXISTS TO REJECT (ARCH §18.1 — a check with
    /// no failing input you can name is not a check). Measured 2026-08-05:
    /// `commonwealth-ai-arch-principles` put 5-8 pinned chunks into 14 of 14
    /// questions on `bench sep/summarize`, 89 total, because pool presence
    /// granted injection rights and the selector had no query term. These are
    /// three of its REAL claim titles against a REAL bench question.
    #[test]
    fn arch_principles_claims_score_zero_against_a_philosophy_question() {
        let tokens =
            q_tokens("Provide a comprehensive overview of idealism as a philosophical position.");
        for claim in [
            "SYSTEM_OVERVIEW.md is a contract, not a diary; update it in the same PR",
            "cargo check is a correctness witness, cargo test is a behaviour witness",
            "Prefer helpers over trait gymnastics for small duplication",
        ] {
            assert_eq!(
                claim_query_relevance(claim, None, &tokens),
                0.0,
                "off-topic claim must not clear the floor: {claim:?}"
            );
        }
    }

    /// The gate is per CORPUS: a corpus earns its injection by showing ONE key
    /// point about the question, and its other key points then ride along —
    /// a corpus's key points legitimately include claims restating none of the
    /// question's words, so testing every claim would gut the feature.
    #[test]
    fn corpus_admitted_on_its_best_claim_not_on_every_claim() {
        let tokens =
            q_tokens("Provide a comprehensive overview of idealism as a philosophical position.");
        let best = claim_query_relevance(
            "Idealism is the philosophical view that reality is mental.",
            None,
            &tokens,
        );
        let sibling = claim_query_relevance("Berkeley denied matter outright.", None, &tokens);
        const TOPIC_GRIP: f32 = 0.5;
        assert!(best > TOPIC_GRIP, "corpus qualifies on this one: {best}");
        assert!(
            sibling <= TOPIC_GRIP,
            "and this one rides along without qualifying alone: {sibling}"
        );
    }

    /// Positive control — the floor must not eat the corpus the question IS
    /// about, which is the case that pays for the whole feature (killing the
    /// injection outright cost sep-summarize-obscure 0.83 -> 0.78 fact recall).
    #[test]
    fn on_topic_claims_clear_the_floor() {
        let tokens =
            q_tokens("Provide a comprehensive overview of idealism as a philosophical position.");
        assert!(
            claim_query_relevance(
                "Idealism holds that reality is fundamentally mental or experiential.",
                None,
                &tokens
            ) > 0.0
        );
        // The verbatim quote counts too — a claim paraphrased away from the
        // question's wording is still on topic when its excerpt names it.
        assert!(
            claim_query_relevance(
                "The doctrine denies mind-independent existence.",
                Some("This is the core of idealism."),
                &tokens
            ) > 0.0
        );
    }

    /// Relevance must ORDER, not merely admit: it is the sort key inside an
    /// admitted corpus, so a claim covering more of the question has to
    /// outscore one covering less.
    #[test]
    fn relevance_is_graded_not_boolean() {
        let tokens = q_tokens("Provide a comprehensive overview of idealism and phenomenalism.");
        let both = claim_query_relevance(
            "Idealism and phenomenalism both deny realism.",
            None,
            &tokens,
        );
        let one = claim_query_relevance("Idealism is a metaphysical thesis.", None, &tokens);
        assert!(both > one, "both={both} one={one}");
    }

    /// THE SECOND MEASURED FAILURE, and why the gate is a MAJORITY of the topic
    /// tokens rather than "any overlap". A `>0` bar let an off-topic corpus in
    /// on one shared common noun and then ranked it first — measured on
    /// `bench sep/summarize`, off-topic rose 27.6% -> 33.6% because the slots
    /// freed from `arch-principles` were refilled by
    /// `commonwealth-ai-system-overview` matching on "argument"/"existence".
    #[test]
    fn a_single_generic_noun_is_coincidence_not_aboutness() {
        let tokens = q_tokens(
            "Give a comprehensive overview of the cosmological argument for the \
                      existence of god.",
        );
        // A system-doc claim that happens to share ONE common noun.
        let generic = claim_query_relevance(
            "Every argument for a design decision belongs in the ADR, not the commit body.",
            None,
            &tokens,
        );
        // A claim that is genuinely about the subject.
        let real = claim_query_relevance(
            "The cosmological argument infers a first cause from the existence of contingent \
             beings.",
            None,
            &tokens,
        );
        const TOPIC_GRIP: f32 = 0.5; // the shipped default
        assert!(
            generic <= TOPIC_GRIP,
            "one shared noun must not clear the gate: {generic}"
        );
        assert!(
            real > TOPIC_GRIP,
            "a claim about the subject must clear it: {real}"
        );
    }

    /// An all-stopword question yields no tokens. The floor must ABSTAIN
    /// (admit) rather than drop every candidate on a signal it cannot compute —
    /// failing closed here would silently disable the feature (ARCH §18.3).
    #[test]
    fn empty_query_tokens_abstain_rather_than_drop_everything() {
        assert_eq!(claim_query_relevance("anything at all", None, &[]), 1.0);
    }

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// THE REGRESSION TEST for audit D1 (2026-08-04).
    ///
    /// The failing input, verbatim from production: a "summarize proof
    /// theory" turn reached only `sep`, carried NO explicit scope, and the
    /// enumerator resolved that to every installed corpus — so 43-80% of the
    /// evidence pool arrived from `sep-chinese-logic-language`,
    /// `sep-compatibilism`, `sep-kant-metaphysics` (a different one each run).
    /// Enumeration is query-independent, so nothing downstream could reject
    /// them.
    #[test]
    fn unscoped_turn_never_widens_beyond_the_corpora_it_reached() {
        let pool = v(&["sep"]);
        let scope = resolve_atom_enum_scope(None, &pool);
        assert_eq!(
            scope,
            v(&["sep"]),
            "an unscoped turn must enumerate only the corpora it reached"
        );
        for off_topic in [
            "sep-chinese-logic-language",
            "sep-compatibilism",
            "sep-kant-metaphysics",
            "sep-atheism-agnosticism",
        ] {
            assert!(
                !scope.iter().any(|c| c == off_topic),
                "{off_topic} was admitted into an unscoped proof-theory turn — D1 has regressed"
            );
        }
    }

    /// An explicit scope is authoritative and is never widened. This is what
    /// makes `--isolate` and a mesh principal's corpus selection airtight
    /// rather than advisory — the control that measured 0 off-target chunks
    /// in 904 depends on it.
    #[test]
    fn explicit_scope_is_authoritative_and_never_widened() {
        let pool = v(&["sep", "wikipedia", "enron-sample-tiny"]);
        assert_eq!(
            resolve_atom_enum_scope(Some(&v(&["sep"])), &pool),
            v(&["sep"]),
            "an explicit scope must not absorb corpora that merely appear in the pool"
        );
    }

    /// No corpus reached ⇒ nothing to enumerate. The old code treated this
    /// same state as "enumerate everything", which is the inversion at the
    /// heart of D1.
    #[test]
    fn empty_pool_and_no_scope_enumerates_nothing() {
        assert!(resolve_atom_enum_scope(None, &[]).is_empty());
        assert!(resolve_atom_enum_scope(Some(&[]), &[]).is_empty());
    }

    /// An empty explicit scope is treated as absent, not as "deny all" — it
    /// falls back to the pool, preserving the pre-existing caller contract
    /// (`Some(enabled) if !enabled.is_empty()`).
    #[test]
    fn empty_explicit_scope_falls_back_to_pool() {
        let pool = v(&["sep"]);
        assert_eq!(resolve_atom_enum_scope(Some(&[]), &pool), v(&["sep"]));
    }

    /// A pool naturally repeats a corpus once per chunk; the scope must be a
    /// set, and stable across runs. Unstable ordering here is what let
    /// `HashMap` iteration order pick a different off-topic corpus per
    /// process (audit D2).
    #[test]
    fn pool_derived_scope_is_deduplicated_and_order_stable() {
        let pool = v(&["sep", "wikipedia", "sep", "wikipedia", "sep", ""]);
        let first = resolve_atom_enum_scope(None, &pool);
        assert_eq!(first, v(&["sep", "wikipedia"]), "deduped, empties dropped");
        for _ in 0..8 {
            assert_eq!(
                resolve_atom_enum_scope(None, &pool),
                first,
                "scope must be identical across repeated resolutions"
            );
        }
    }
}

#[cfg(test)]
mod overview_tests {
    #[test]
    fn looks_like_overview_detects_summary_questions() {
        use super::Runtime;
        // Overview/summary phrasings → true: these are the anchorless
        // "what matters here" questions that should ground on the corpus's
        // atlas Claim atoms instead of abstaining over a diffuse pool.
        for q in [
            "What is the most important thing in the maple-house material, and why?",
            "Give me an overview of this corpus.",
            "Summarize what this material is mainly about.",
            "What are the main points here?",
            "Tell me about the sep corpus.",
            "What's the gist?",
            "Give me a high-level summary.",
        ] {
            assert!(Runtime::looks_like_overview(q), "expected overview: {q:?}");
        }
        // Specific, anchored questions → false: these retrieve normally and
        // must NOT trip the claim-injection path.
        for q in [
            "What does the charter say about smoking?",
            "In what year did the Great Depression begin?",
            "What is the value of FILES_COLUMN_WIDTH?",
            "Who led the negotiation?",
        ] {
            assert!(
                !Runtime::looks_like_overview(q),
                "expected NOT overview: {q:?}"
            );
        }
    }
}
