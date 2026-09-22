// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atlas-grounded retrieval primitives: the traversal surface over the atom
//! graph, plus the query-time fusion that turns atom hits into `ScoredChunk`s.
//!
//! Lived in `sovereign-core` until 2026-08-20 and named 44 corpus-engine types
//! across 176 references to do it, while importing NOTHING from sovereign —
//! the whole file was corpus-engine logic on the far side of a domain
//! boundary. `sovereign_core::atlas_context` re-exports this module at its
//! historical path, so no call site moved (noun-convergence rung 6).
//!
//! This is the VERB the rung's order asked for. `AtomView` / `EdgeView` /
//! `EvidenceRef` are how a consumer reads the graph without holding
//! `AtomEnvelope`, `Edge` or `EdgeType` — which is why those types can stay
//! private to this crate rather than crossing 569 times.
//!
//! The atlas is a typed knowledge graph computed offline (see
//! `corpus-engine/ATLAS.md`). At query time, retrieval can fuse atlas
//! Entity matches into the chunk hit set as virtual `ScoredChunk`s:
//! cosine the question embedding against pre-embedded Entity
//! descriptions, take top-K, surface them as additional candidates.
//! This module owns the data types + math; the eval CLI provides one
//! loader (against `ChatSession::inference`) and the daemon provides
//! another (`sovereign-tools::atlas_context_manager`) that loads at
//! daemon boot and reuses across queries.

use std::collections::HashMap;
use std::sync::Arc;

use crate::atoms::AtomEnvelope;
use corpus_index::types::ScoredChunk;

// Split (file ceiling): `AtlasGraph` + loaders + CallChain traversal live in
// `graph`, the borrowing views + walk helpers + `atlas_navigate_ann` in
// `views`. Both re-export their public items below so every historical
// `context::X` path — including corpus-engine's glob re-export and
// `sovereign_core::atlas_context` — keeps resolving unchanged.
#[path = "context/graph.rs"]
mod graph;
#[path = "context/views.rs"]
mod views;
pub use graph::{
    open_and_attach_ann_seed_table, open_ann_seed_table, AtlasGraph, NavigationAttachment,
};
pub use views::{
    atlas_navigate_ann, atom_verbatim_excerpt, contains_whole_word, edge_weight,
    render_call_chain_brief, AtomView, CallChainNode, CallChainResult, CallDirection, ChunkRequest,
    EdgeView, EvidenceRef,
};

/// One pre-embedded atlas atom available to retrieval as a virtual
/// chunk. Built by a loader, immutable after that.
#[derive(Debug, Clone)]
pub struct AtlasEntry {
    /// The backing atom's id (`entity-<hash>`). First-class since
    /// ATLAS_STORAGE_V2 Phase B: seeding reads it directly instead of
    /// reverse-resolving from `embed_text`, so `resolve_atom_id_from_entry` is
    /// gone. Empty only for entries with no backing atom (the non-default,
    /// eval-only `include_tensions` edge virtual-chunks).
    pub atom_id: String,
    pub canonical_name: String,
    pub embed_text: String,
    pub embedding: Vec<f32>,
}

/// Pre-embedded atlas entity bag for one corpus. Carries the
/// `top_k` the loader was constructed with so the per-query call
/// site doesn't need to re-pick a value.
#[derive(Debug, Clone)]
pub struct AtlasContext {
    pub atlas_corpus_id: String,
    pub entries: Vec<AtlasEntry>,
    pub top_k: usize,
}

/// DARK (ontology-v1 P5, default **OFF**) — `SOVEREIGN_ATLAS_EMBED_ATTRIBUTES`.
///
/// When on, a declared atom's `attributes` are appended to its embed text, so
/// "which coins are silver" has something to match on: today the metal lives
/// in a JSON map the embedder never sees. Read once (the renderer runs per
/// atom over million-atom atlases).
///
/// Off by default because it changes what every atom embeds to, and the cache
/// signature does not key on it — see the `DEFAULTS_LEDGER.md` row for the
/// flip conditions.
static EMBED_ATTRIBUTES: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
    let on = std::env::var("SOVEREIGN_ATLAS_EMBED_ATTRIBUTES")
        .ok()
        .map(|v| {
            let v = v.trim().to_string();
            v == "1" || v.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false);
    tracing::debug!(
        enabled = on,
        "atlas render: SOVEREIGN_ATLAS_EMBED_ATTRIBUTES"
    );
    on
});

/// The `\nattr: k=v; k2=v2` suffix an atom's declared attributes contribute to
/// its embed text, or `""`.
///
/// Empty when the knob is off AND when the atom has no attributes — which is
/// every atom of every undeclared corpus, so SEP / Wikipedia / Enron render
/// identically whichever way the knob is set. Keys are already sorted
/// (`serde_json::Map` is a BTreeMap under the default feature), so the suffix
/// is deterministic.
///
/// ONE decider: both the ANN-backfill renderer ([`render_atom_entry`]) and the
/// daemon's bag loader call this, so an entry's `embed_text` stays stable
/// across the build and read paths — the invariant [`ATLAS_ENTRY_CHAR_LIMIT`]
/// documents.
pub fn atom_attributes_suffix(attrs: &serde_json::Map<String, serde_json::Value>) -> String {
    if !*EMBED_ATTRIBUTES {
        return String::new();
    }
    render_attributes(attrs)
}

/// The rendering itself, independent of the knob — the half a unit test can
/// exercise (a `LazyLock` env read is decided once per process, so the gate
/// above is proven by its default, not by flipping it mid-run).
pub fn render_attributes(attrs: &serde_json::Map<String, serde_json::Value>) -> String {
    if attrs.is_empty() {
        return String::new();
    }
    // Sorted HERE, not inherited from the map. `serde_json::Map` is a
    // `BTreeMap` or an insertion-ordered `IndexMap` depending on whether
    // anything in the build enables `serde_json/preserve_order` — and
    // something in this workspace does, so cargo's feature unification
    // decides the order for every crate at once. This string is EMBEDDED
    // (`SOVEREIGN_ATLAS_EMBED_ATTRIBUTES`), so inheriting that order would
    // make an atom's vector depend on the order the extractor happened to
    // emit its keys, and on which crates were in the build. Caught 2026-09-02
    // by `attribute_suffix_is_dark_and_key_sorted` failing in the full
    // workspace and passing under `-p corpus-engine`.
    let mut pairs: Vec<(&String, &serde_json::Value)> = attrs.iter().collect();
    pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
    let rendered = pairs
        .into_iter()
        .map(|(k, v)| match v {
            serde_json::Value::String(s) => format!("{k}={s}"),
            other => format!("{k}={other}"),
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("\nattr: {rendered}")
}

/// Max chars of rendered atom text fed to the embedder — the cap
/// [`render_atom_entry`] truncates to. The loaders share the renderer, so an
/// entry's `embed_text` is stable across the build (embed) and read (bag) paths.
pub const ATLAS_ENTRY_CHAR_LIMIT: usize = 3000;

/// Render one atom into its `(canonical_name, embed_text)` bag pair — the SINGLE
/// source of the atlas embed-text shape, shared by the build-time embedder
/// (eval / backfill, over `atoms.json`) and the read-time bag builder
/// ([`build_atlas_context_from_ann`], over the resident store). `canonical_name`
/// is the Entity's name (so rigid source-matching credits it) or the
/// `article_slug` for the article-scoped kinds (Claim / Configuration /
/// ArgumentReconstruction / Position / State). `None` for atom kinds that never
/// enter the bag — and a `None` here is what makes a kind UNSEEDABLE however a
/// corpus's navigation map names it, so a kind added to a seed row belongs in
/// this fan-out too (see `seed_population`). Both
/// paths sharing this guarantees the embedding written to the ANN table
/// corresponds to the bag's re-rendered `embed_text`. `pub` so the eval / backfill
/// loaders reuse it rather than forking the rendering.
pub fn render_atom_entry(atom: &AtomEnvelope, article_slug: &str) -> Option<(String, String)> {
    match atom {
        AtomEnvelope::Entity(e) => {
            let mut text = String::new();
            text.push_str(&e.canonical_name);
            text.push('\n');
            if !e.aliases.is_empty() {
                text.push_str(&e.aliases.join(", "));
                text.push('\n');
            }
            text.push_str(&e.description);
            text.push_str(&atom_attributes_suffix(&e.attributes));
            if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
            }
            Some((e.canonical_name.clone(), text))
        }
        AtomEnvelope::Claim(c) => {
            let act = serde_json::to_string(&c.discourse_act)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let status = serde_json::to_string(&c.epistemic_status)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let mut text = format!("[Claim: {act}, {status}] {content}", content = c.content);
            text.push_str(&atom_attributes_suffix(&c.attributes));
            if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
            }
            Some((article_slug.to_string(), text))
        }
        AtomEnvelope::Configuration(cfg) => {
            let mut text = format!("[Configuration: {}] {}", cfg.label, cfg.description);
            if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
            }
            Some((article_slug.to_string(), text))
        }
        // Position and State render because the pre-registered navigation
        // table SEEDS on them — `tension` on Claim + Position, `trajectory` on
        // Entity + State (`ontology::navigation`). A kind a map can name as a
        // seed and this function cannot render is a population the writer
        // derives and then silently drops, which is the substitution ARCH
        // §18.3 forbids; the two arms are what make
        // `seed_population::seed_population` honourable. Article-scoped, like
        // Claim and Configuration: neither carries a name retrieval matches
        // sources on.
        AtomEnvelope::Position(p) => {
            let mut text = format!(
                "[Position: {stance}] {name} — {content}",
                stance = p.stance,
                name = p.canonical_name,
                content = p.content
            );
            if !p.anchors.is_empty() {
                text.push(' ');
                text.push_str(&p.anchors.join("; "));
            }
            if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
            }
            Some((article_slug.to_string(), text))
        }
        AtomEnvelope::State(st) => {
            let state_type = serde_json::to_string(&st.state_type)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let mut text = format!("[State: {state_type}] {}", st.label);
            if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
            }
            Some((article_slug.to_string(), text))
        }
        // ei-7a. The seed population derives `Summary` from the `thematic`
        // row's seed kinds, and a kind a map names as a seed that this
        // function cannot render is a population the writer derives and then
        // silently drops — the substitution §18.3 forbids, and exactly what
        // the Position/State arms above were added to prevent. Article-scoped
        // like every other non-Entity kind: a summary carries no name
        // retrieval matches sources on.
        //
        // The level is in the text because it is the one navigational fact
        // that distinguishes two summaries of the same article (level 0
        // summarises chunks; higher levels summarise summaries), and the
        // retiring injector's `min_level` knob was built on exactly that
        // distinction.
        AtomEnvelope::Summary(sum) => {
            let mut text = format!("[Summary L{}] {}", sum.level, sum.text);
            if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
            }
            Some((article_slug.to_string(), text))
        }
        AtomEnvelope::ArgumentReconstruction(a) => {
            let mut text = String::with_capacity(256);
            text.push_str("[Argument: ");
            text.push_str(&a.name);
            text.push_str("] ");
            for p in &a.premises {
                text.push_str(p);
                text.push(' ');
            }
            text.push_str(&a.conclusion);
            for o in &a.objections {
                if !o.content.trim().is_empty() {
                    text.push(' ');
                    text.push_str(o.content.trim());
                } else if !o.name.trim().is_empty() {
                    text.push(' ');
                    text.push_str(o.name.trim());
                }
            }
            if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
            }
            Some((article_slug.to_string(), text))
        }
        // Event and Relation render because the `lookup` row seeds on them,
        // and the rule is the one the Position/State/Summary arms above were
        // added to enforce: a kind a map can name as a seed and this function
        // cannot render is a population the writer derives and then silently
        // drops (ARCH §18.3).
        //
        // They were unrenderable — and therefore unseedable, and therefore
        // unreachable by any walk — from the day the section-extraction schema
        // began emitting them. Measured 2026-09-09 on `chaos-secret-agent`:
        // `atlas status --json` reports `ann.embedded_atoms 151` against 226
        // atoms, and the missing 75 are exactly Event 33 + Relation 20 +
        // Question 22. The atom that answers the bench's `present-killer-weapon`
        // ("With what kind of weapon does Winnie kill Adolf Verloc?") is
        // event-0033, "Mrs Verloc stabs Mr Verloc in the breast with a carving
        // knife" — it scores 0.5627 on that question against 0.3659 for the
        // `Mr Verloc` Entity the seeder DID admit, so the corpus held the
        // answer and no retrieval surface could carry it. The Relation twin is
        // the bank's `present-stevie-relation`: relation-0001 scores 0.7521
        // where the same entity scores 0.4302.
        //
        // Article-scoped like every other non-Entity kind: neither carries a
        // name `score_sources` matches a source on.
        AtomEnvelope::Event(e) => {
            let mut text = format!("[Event: {}] {}", e.event_type.as_str_repr(), e.description);
            text.push_str(&atom_attributes_suffix(&e.attributes));
            if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
            }
            Some((article_slug.to_string(), text))
        }
        AtomEnvelope::Relation(r) => {
            let mut text = format!("[Relation: {}] {}", r.relation_type.as_str_repr(), r.label);
            text.push_str(&atom_attributes_suffix(&r.attributes));
            if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
            }
            Some((article_slug.to_string(), text))
        }
        _ => None,
    }
}

/// Resolve a CONCEPTUAL seed atom by MEANING — the seed source for a
/// natural-language CallChain query ("how does it check whether a version
/// satisfies a requirement"). Prefers the persistent ANN seed table (atom-ids
/// directly, re-scored with the canonical [`cosine`] so the ranking matches the
/// cosine path); falls back to exact cosine over the embedding bag and reads the
/// matched [`AtlasEntry`]'s first-class `atom_id` when a corpus isn't backfilled
/// — the same ANN-or-cosine adaptivity [`atlas_navigate_ann`] uses for its seeds,
/// factored here so the CallChain (the `atlas-query` CLI and, later, chat) seeds
/// identically rather than forking it. Returns `(atom_id, cosine_score)`; `None`
/// when the query doesn't embed or nothing resolves.
pub async fn seed_atom_by_meaning(
    query_embedding: &[f32],
    graph: &AtlasGraph,
    fallback_ctx: Option<&AtlasContext>,
) -> Option<(String, f32)> {
    if query_embedding.is_empty() {
        return None;
    }
    // Prefer the ANN seed table when the corpus has been backfilled.
    if let Some(ann) = graph.ann_seed_table() {
        match ann.nearest_with_vectors(query_embedding, 8).await {
            Ok(hits) => {
                let mut best: Option<(String, f32)> = None;
                for (atom_id, vector) in hits {
                    let s = cosine(query_embedding, &vector);
                    if best.as_ref().map(|(_, b)| s > *b).unwrap_or(true) {
                        best = Some((atom_id, s));
                    }
                }
                if best.is_some() {
                    return best;
                }
            }
            Err(e) => tracing::warn!(
                corpus = %graph.atlas_corpus_id,
                "seed_atom_by_meaning: ANN nearest failed ({e}); falling back to cosine bag"
            ),
        }
    }
    // Fallback: exact cosine over the embedding bag, then read the matched
    // entry's first-class atom-id (ATLAS_STORAGE_V2 Phase B — the `atom_id` is
    // resident on the entry, so the old reverse-resolve join is gone).
    let ctx = fallback_ctx?;
    let mut best: Option<(&AtlasEntry, f32)> = None;
    for entry in &ctx.entries {
        if entry.embedding.is_empty() {
            continue;
        }
        let s = cosine(query_embedding, &entry.embedding);
        if best.as_ref().map(|(_, b)| s > *b).unwrap_or(true) {
            best = Some((entry, s));
        }
    }
    let (entry, score) = best?;
    // Entries with no backing atom (the eval-only edge virtual-chunks) carry an
    // empty `atom_id`; treat that as "nothing resolved", preserving the prior
    // `resolve_atom_id_from_entry(...)?` bail-to-`None` semantics.
    if entry.atom_id.is_empty() {
        return None;
    }
    Some((entry.atom_id.clone(), score))
}

/// Build the query-time embedding bag from a corpus's persistent ANN seed table
/// joined to its resident atoms — the ATLAS_STORAGE_V2 Phase B read path. Atom
/// embeddings live ONLY in `atoms_ann.lance` (written once at enrich / backfill);
/// the bag is derived here at load with no re-embed and no `atoms.embeddings.bin`
/// sidecar. Each ANN row's `(atom_id, embedding)` joins to the resident atom for
/// its rendered `(canonical_name, embed_text)` via [`render_atom_entry`], so the
/// bag's text matches the text the embedding represents. Requires `graph` to
/// carry an ANN table (attached by [`open_and_attach_ann_seed_table`]); a corpus
/// with no table yields no bag (it then contributes only name-match seeds).
pub async fn build_atlas_context_from_ann(
    atlas_corpus_id: &str,
    graph: &AtlasGraph,
    top_k: usize,
) -> Result<AtlasContext, String> {
    let Some(ann) = graph.ann_seed_table() else {
        return Err(format!(
            "no ANN seed table for {atlas_corpus_id}; backfill with `sovereign atlas backfill-ann`"
        ));
    };
    let rows = ann.all_rows().await?;
    let mut entries: Vec<AtlasEntry> = Vec::with_capacity(rows.len());
    for (atom_id, embedding) in rows {
        // Join the ANN row back to its resident atom for the rendered text. An
        // atom referenced by the table but absent from the store (a torn build)
        // is skipped rather than fatal.
        let Some(envelope) = graph.atom(&atom_id).and_then(|v| v.atom_envelope()) else {
            continue;
        };
        let Some((canonical_name, embed_text)) = render_atom_entry(&envelope, graph.article_slug())
        else {
            continue;
        };
        entries.push(AtlasEntry {
            atom_id,
            canonical_name,
            embed_text,
            embedding,
        });
    }
    Ok(AtlasContext {
        atlas_corpus_id: atlas_corpus_id.to_string(),
        entries,
        top_k,
    })
}

/// Cosine similarity. Returns 0 on zero-length vectors or
/// dimension mismatch — both are signs of a misconfigured loader,
/// and silently degrading to zero score keeps retrieval going
/// rather than poisoning a query.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = (na.sqrt() * nb.sqrt()).max(1e-9);
    dot / denom
}

/// Score every entry by cosine sim to `query_embedding`, take the
/// top-K from `ctx`, return as virtual `ScoredChunk`s. Each chunk's
/// `corpus_id` is `atlas:<corpus_id>` so downstream provenance keeps
/// the origin obvious — the per-question report distinguishes
/// "wikipedia chunk" from "atlas-derived virtual chunk."
///
/// Phase C4 — every chunk also carries provenance metadata so eval
/// `--inspect` and the desktop's hit attribution can surface where
/// each result actually came from:
///
///   - `metadata["source"] = "atlas"` — discriminator for atlas vs
///     chunk vs mesh-peer hits.
///   - `metadata["atlas_corpus"] = <corpus_id>` — the underlying
///     corpus the atlas was built over.
///   - `metadata["atlas_tier"] = "tier-2"` — for now we only carry
///     extracted entries (see `AtlasContextFilter::default`); a
///     future per-entry tier would land here when the loader
///     surfaces mixed depths.
pub fn atlas_top_k_as_chunks(query_embedding: &[f32], ctx: &AtlasContext) -> Vec<ScoredChunk> {
    atlas_top_k_across(query_embedding, std::slice::from_ref(&ctx), ctx.top_k)
}

/// Multi-atlas variant: pool every entry across `ctxs`, score them
/// together, and return the global top-`k_total`. Each chunk carries
/// the metadata of the atlas it actually came from — so a virtual
/// chunk surfaced from `sep-consciousness` keeps `atlas:sep-consciousness`
/// as its corpus_id even when several atlases were considered.
///
/// Why a global top-K rather than per-atlas K then truncate: when
/// retrieval pools several per-article SEP atlases, the right 3
/// answers may all live in the topically-aligned atlas — a per-atlas
/// fairness budget would dilute that with noisy off-topic surfaces
/// from the other articles. The cosine score is the right
/// arbitrator.
pub fn atlas_top_k_across(
    query_embedding: &[f32],
    ctxs: &[&AtlasContext],
    k_total: usize,
) -> Vec<ScoredChunk> {
    if k_total == 0 {
        return Vec::new();
    }
    let mut scored: Vec<(f32, &AtlasContext, &AtlasEntry)> = Vec::new();
    for ctx in ctxs {
        for entry in &ctx.entries {
            let s = cosine(query_embedding, &entry.embedding);
            scored.push((s, ctx, entry));
        }
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k_total);
    scored
        .into_iter()
        .map(|(score, ctx, e)| {
            let mut metadata = HashMap::new();
            metadata.insert("source".to_string(), "atlas".to_string());
            metadata.insert("atlas_corpus".to_string(), ctx.atlas_corpus_id.clone());
            metadata.insert("atlas_tier".to_string(), "tier-2".to_string());
            ScoredChunk {
                content: e.embed_text.clone(),
                title: Some(e.canonical_name.clone()),
                url: None,
                corpus_id: format!("atlas:{}", ctx.atlas_corpus_id),
                score,
                metadata,
                chunk_id: None,
                source_doc_id: None,
                vector_distance: None,
                // A virtual chunk built from an atlas entity description, not
                // a row an index vouched for. It may orient retrieval and may
                // not ground a claim (TOPOLOGY §10 rung 9.1, hazard 1).
                provenance: corpus_index::index::ChunkProvenance::manufactured(
                    "atlas_context_entity",
                ),
            }
        })
        .collect()
}

/// Source of `AtlasContext`s, looked up at query time. The runtime
/// holds an `Option<Arc<dyn AtlasContextProvider>>` and consults it
/// inside the chunk-retrieval path; the daemon's
/// `AtlasContextManager` is the production implementation, while
/// the eval CLI builds one inline from `ChatSession`.
#[async_trait::async_trait]
pub trait AtlasContextProvider: Send + Sync {
    /// Look up a pre-loaded context by its atlas corpus id. Returns
    /// `None` when no atlas has been loaded for that id (e.g. the
    /// corpus has no `atlas/` dir, or daemon boot is still warming).
    fn get(&self, atlas_corpus_id: &str) -> Option<Arc<AtlasContext>>;

    /// All atlas corpus ids currently loaded. Used by the runtime
    /// to fuse atlas grounding for every installed corpus that has
    /// one — the caller doesn't need to know which corpora have
    /// atlases ahead of time.
    fn loaded_corpus_ids(&self) -> Vec<String>;

    /// Record that `canonical_name` from `atlas_corpus_id` matched a
    /// query (i.e. it landed in the top-K returned by
    /// [`atlas_top_k_as_chunks`]). Persisted as a per-corpus bump
    /// map and consumed by the next triage rebuild as a centrality
    /// addition — articles users actually ask about move up the
    /// Tier-2 enrichment queue. Default: no-op (eval CLI doesn't
    /// need adaptive triage).
    fn record_match(&self, _atlas_corpus_id: &str, _canonical_name: &str) {}

    /// Look up the structural graph layer for an atlas — atom-by-id,
    /// edge adjacency. Used by [`atlas_navigate`] to walk the typed
    /// knowledge graph beyond bag-of-atoms cosine matching. Default
    /// `None` for providers that haven't loaded the graph layer yet
    /// (back-compat with the entity-only embedding cache); they fall
    /// back to [`atlas_top_k_as_chunks`].
    fn graph(&self, _atlas_corpus_id: &str) -> Option<Arc<AtlasGraph>> {
        None
    }

    /// Whatever the GROUNDING WALK can read for this atlas — an
    /// [`AtlasProvider`](super::provider::AtlasProvider), which the v2 atom
    /// store is one of and the wiki-class columnar store is another.
    ///
    /// Distinct from [`Self::graph`] because they are distinct questions, and
    /// collapsing them would cost something either way. `graph` answers "give
    /// me the atom store", concretely: `atom_enum` calls `atoms_of_kind` and
    /// `edge_degree` on it, neither of which is on the trait — `atoms_of_kind`
    /// was deliberately removed from it, because the walk does not call it.
    /// This one answers "give me something the walk can read", and for a
    /// wiki-class corpus there is no `AtlasGraph` to hand back and never will
    /// be (`WIKIPEDIA_ATLAS_V2.md`: its edges carry per-edge strings a CSR
    /// cannot hold).
    ///
    /// REQUIRED, and it used to have a default that delegated to `graph()`.
    /// That default was the §18.3 shape: `graph()` can only ever hand back an
    /// atom store, so an implementor that did nothing got "no walkable store"
    /// for a corpus holding a perfectly good `articles.lance` — an absence
    /// DEFAULTED, silently, to the bag-of-atoms branch. It cost nothing while
    /// `AtlasContextManager` was the only implementor and overrode it
    /// correctly; it would have cost the next implementor a wrong answer that
    /// looked like a right one, and in an A/B two arms that agree perfectly
    /// because neither read its store.
    ///
    /// The default cannot be repaired in place: resolving BY CLASS needs the
    /// atlas directory (`open_walk_provider` takes one) and this trait does not
    /// have it. So the honest move is to remove the default and let the
    /// compiler ask each implementor the question, rather than answer it for
    /// them (principle 10 — make it structural, not remembered). Implementors
    /// that serve only atom-class stores write the old one-liner and mean it.
    fn walk_provider(
        &self,
        atlas_corpus_id: &str,
    ) -> Option<Arc<dyn super::provider::AtlasProvider>>;

    /// Ensure the given atlas corpora are loaded (bag + graph + ANN seed
    /// table), loading any not already resident. The lazy-load hook for
    /// scoped grounding: the runtime derives the query-relevant corpus set
    /// from the retrieved chunks and calls this before grounding, so boot
    /// no longer eager-loads every atlas. Ids without an atlas dir are
    /// skipped. Default: no-op (providers that pre-load need nothing).
    async fn ensure_loaded(&self, _ids: &[String]) {}

    /// Every atlas corpus the provider can serve — loaded OR lazily
    /// loadable. The atom-enumeration path uses this (it walks graphs,
    /// which lazy-load, not bags). Default: the loaded set, for back-compat
    /// with providers that pre-load.
    fn discoverable_corpus_ids(&self) -> Vec<String> {
        self.loaded_corpus_ids()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, embed: Vec<f32>) -> AtlasEntry {
        AtlasEntry {
            atom_id: format!("entity-{name}"),
            canonical_name: name.to_string(),
            embed_text: format!("{name} desc"),
            embedding: embed,
        }
    }

    #[test]
    fn cosine_matches_identical_vector_at_one() {
        let v = vec![1.0, 2.0, 3.0];
        let s = cosine(&v, &v);
        assert!((s - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_zero_on_dim_mismatch() {
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    #[test]
    fn top_k_returns_highest_cosine_first() {
        let ctx = AtlasContext {
            atlas_corpus_id: "test".into(),
            entries: vec![
                entry("Far", vec![-1.0, -1.0]),
                entry("Near", vec![1.0, 1.0]),
                entry("Mid", vec![1.0, 0.0]),
            ],
            top_k: 2,
        };
        let q = vec![1.0, 1.0];
        let chunks = atlas_top_k_as_chunks(&q, &ctx);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].title.as_deref(), Some("Near"));
        assert_eq!(chunks[0].corpus_id, "atlas:test");
    }

    /// Phase C4: every atlas chunk carries provenance metadata so
    /// downstream consumers can distinguish atlas vs chunk vs mesh
    /// hits without sniffing the corpus_id prefix.
    #[test]
    fn atlas_chunks_carry_provenance_metadata() {
        let ctx = AtlasContext {
            atlas_corpus_id: "wikipedia".into(),
            entries: vec![entry("Earth", vec![1.0, 0.0])],
            top_k: 1,
        };
        let chunks = atlas_top_k_as_chunks(&[1.0, 0.0], &ctx);
        let m = &chunks[0].metadata;
        assert_eq!(m.get("source").map(|s| s.as_str()), Some("atlas"));
        assert_eq!(m.get("atlas_corpus").map(|s| s.as_str()), Some("wikipedia"));
        assert_eq!(m.get("atlas_tier").map(|s| s.as_str()), Some("tier-2"));
    }
}
