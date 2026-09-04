// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ONE atlas-context loader: `atoms.json` (+ `edges.json`) → filtered,
//! embedded [`AtlasContext`] bag — the input `build_persistent_ann_seed_table`
//! turns into the per-corpus ANN seed table (`atlas/atoms_ann.lance`).
//!
//! It reached `sovereign-cli-llm::eval_cmd::runner` first, then
//! `sovereign-tools` (ontology-v1 P0.2) so the daemon could seed a freshly
//! written atlas in-process. It lands HERE (order ei-5a-build-cut) because
//! every type it touches was already corpus-engine's: the atoms, the edges,
//! the ontology, and — since noun-convergence rung 6 —
//! [`build_persistent_ann_seed_table`] and the [`AtlasContext`] bag itself.
//! The one thing keeping it in the inference stack was its embedder
//! parameter, and the file's own test said what that parameter really is:
//! "the writer's only inference need is `embed_query`". So it takes an
//! [`EmbedFn`] — corpus-engine's own embed closure — and the atlas write
//! stops dragging llama.cpp behind it. `sovereign_tools::
//! atlas_context_manager` re-exports every name here, so
//! `atlas_context_manager::{load_atlas_context, backfill_ann,
//! AtlasContextFilter, BackfillOutcome, LoadAtlasError}` still resolves for
//! every existing caller (ARCH §10.6, one decider — a re-export, never a
//! twin).
//!
//! Diagnostics are `tracing` events, not stderr: the same function runs
//! inside the daemon.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::enrichment::atlas::ann_store::ANN_TABLE_DIRNAME;
use crate::enrichment::atlas::context::{
    build_persistent_ann_seed_table, render_atom_entry, AnnBuildStats, AtlasContext, AtlasEntry,
};
use crate::enrichment::atlas::seed_population::{
    seed_population, write_population_marker,
};
use crate::enrichment::atlas::{
    read_atlas_atoms, read_atlas_edges, read_atlas_ontology, AtomEnvelope, AtomType, EdgeType,
};
use crate::types::EmbedFn;

pub use super::context_filter::AtlasContextFilter;

/// Why [`load_atlas_context`] produced no bag. Typed so a caller can tell
/// "this corpus has nothing seedable" (a legitimate outcome for an atlas that
/// carries only non-Entity surfaces) from "reading it failed" without
/// matching on message text (ARCH §18.3). `Display` renders the operator
/// messages the CLI printed before this type existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadAtlasError {
    /// No `atlas/` directory at all.
    NoAtlas {
        corpus_id: String,
        atlas_dir: PathBuf,
    },
    /// `atoms.json` read, but the filter admitted nothing.
    FilterExcludedAll {
        corpus_id: String,
        min_description_chars: usize,
    },
    /// `atoms.json` unreadable or unparseable.
    Read(String),
    /// The filter admitted atoms and the embedder refused every one of them.
    ///
    /// Distinct from [`Self::FilterExcludedAll`] because the two send an
    /// operator to opposite places: that one means the knobs are too tight,
    /// this one means the embed slot is down or the model is not loaded. They
    /// were the same outcome until ei-3-index made the seed a hard failure,
    /// and the message an operator then saw was "no atom-bearing entries
    /// (0/0) -- nothing to index", which reads as "this atlas has nothing to
    /// index" and is a substitution ARCH 18.3 forbids.
    EmbedRefusedAll {
        corpus_id: String,
        attempted: usize,
        last_error: String,
    },
}

impl std::fmt::Display for LoadAtlasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAtlas {
                corpus_id,
                atlas_dir,
            } => write!(
                f,
                "no atlas at {} — `svrn enrich ingest {corpus_id} \
                 --strategy structure_first --source-corpus <id>` first",
                atlas_dir.display()
            ),
            Self::FilterExcludedAll {
                corpus_id,
                min_description_chars,
            } => write!(
                f,
                "atlas-context: filter excluded every atom in `{corpus_id}`. \
                 Lower --atlas-min-description-chars (currently {min_description_chars}) \
                 or check --atlas-depth, or pass --atlas-include claim,tension if the \
                 atlas only carries non-Entity surfaces."
            ),
            Self::EmbedRefusedAll {
                corpus_id,
                attempted,
                last_error,
            } => write!(
                f,
                "atlas-context: the embedder refused every one of the {attempted} atom(s) \
                 admitted for `{corpus_id}` (last: {last_error}). The embed slot is down or \
                 its model is not loaded -- load it and re-run \
                 `svrn atlas backfill-ann {corpus_id}`. This is NOT a filter problem."
            ),
            Self::Read(e) => write!(f, "read atlas atoms.json: {e}"),
        }
    }
}

impl std::error::Error for LoadAtlasError {}

/// What [`backfill_ann`] did for one corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackfillOutcome {
    /// The table at `atlas/atoms_ann.lance` was (re)written.
    Built(AnnBuildStats),
    /// The production grounding filter admitted no atom, so there is nothing
    /// to seed from — the table is not written and grounding for this corpus
    /// stays where it was. Not a failure: an atlas of Claims-only or
    /// structural atoms is a real shape (mirrors `migrate_all`'s `"none"`
    /// state, WITHOUT its relaxed-floor retry — one filter, the one the daemon
    /// seeds with).
    NoSeedableAtoms { min_description_chars: usize },
}

/// Build (or rebuild) one corpus's persistent ANN seed table from its
/// `atoms.json`: [`load_atlas_context`] under `filter`, then
/// `build_persistent_ann_seed_table`. The one writer behind `svrn atlas
/// backfill-ann`, the `enrich build` Backfill step, and the daemon's
/// post-write hook (ontology-v1 P0) — lifted from `backfill_ann.rs`'s
/// per-corpus loop rather than written again (ARCH §19). `filter.top_k`
/// rides into the bag unchanged; the table does not use it.
///
/// `Err` is a real failure (unreadable atlas, embed provider down, Lance
/// write failed) and carries the underlying message; callers name the
/// recovery command (`svrn atlas backfill-ann <id>`) at their own surface.
pub async fn backfill_ann(
    embed: &EmbedFn,
    atlas_dir: &Path,
    corpus_id: &str,
    filter: &AtlasContextFilter,
) -> Result<BackfillOutcome, String> {
    // The POPULATION is the map's decision, taken here — at the one writer —
    // rather than at each of the four call sites that reach it (the atlas
    // writer, `svrn atlas backfill-ann`, the `enrich build` Backfill step,
    // `atlas migrate-all`). None of them passes it, none of them can get it
    // wrong, and none of their signatures moved: the atlas dir is all the
    // derivation needs (ARCH §10.6, §19).
    let population = seed_population(atlas_dir);
    let filter = &AtlasContextFilter {
        seed_kinds: Some(population.kinds.clone()),
        ..filter.clone()
    };
    tracing::info!(
        corpus = corpus_id,
        population = %population
            .kinds
            .iter()
            .map(AtomType::label)
            .collect::<Vec<_>>()
            .join(","),
        source = %population.source.label(),
        "backfill-ann: seed population derived from the navigation map"
    );
    let ctx = match load_atlas_context(embed, atlas_dir, corpus_id, filter.top_k, filter).await {
        Ok(ctx) => ctx,
        Err(LoadAtlasError::FilterExcludedAll {
            min_description_chars,
            ..
        }) => {
            tracing::info!(
                corpus = corpus_id,
                min_description_chars,
                depth_allowlist = ?filter.depth_allowlist,
                "backfill-ann: no seedable atoms under the grounding filter; table not written"
            );
            return Ok(BackfillOutcome::NoSeedableAtoms {
                min_description_chars,
            });
        }
        Err(e) => return Err(e.to_string()),
    };
    let stats = build_persistent_ann_seed_table(atlas_dir, &ctx).await?;
    // The marker rides with the table, written second so it is never newer
    // than what it describes. Without it `ann_table_is_fresh` would keep an
    // Entity-only table that merely post-dates `atoms.json` — which is every
    // table on this box, and the reason a population change has to read as
    // STALENESS rather than as an operator's problem to remember.
    if let Err(e) = write_population_marker(atlas_dir, &population) {
        // Not fatal: the table is on disk and correct. A missing marker reads
        // as STALE, so the cost is a re-embed, never a wrong seed.
        tracing::warn!(
            corpus = corpus_id,
            error = %e,
            "backfill-ann: seed table written but its population marker was not; \
             the table will read as stale and be rebuilt"
        );
    }
    tracing::info!(
        corpus = corpus_id,
        resolved = stats.resolved,
        total = stats.total,
        population = %population
            .kinds
            .iter()
            .map(AtomType::label)
            .collect::<Vec<_>>()
            .join(","),
        table = %atlas_dir.join(ANN_TABLE_DIRNAME).display(),
        "backfill-ann: wrote ANN seed table"
    );
    Ok(BackfillOutcome::Built(stats))
}

/// Sync bridge for [`backfill_ann`] — the atlas writer
/// (`writer::write_atlas_full`) is sync at every lifecycle point that writes
/// the v2 store, and the seed table is written in that same write. Runs the
/// async backfill on a dedicated-thread runtime through the atlas module's ONE
/// such bridge (`store::run_blocking`, the same one `write_store_blocking`
/// uses), so it is safe whether or not an ambient tokio runtime exists.
///
/// One writer, one bridge: this is a thin wrapper over [`backfill_ann`], never
/// a second implementation of it (ARCH §10.6).
pub fn backfill_ann_blocking(
    embed: &EmbedFn,
    atlas_dir: &Path,
    corpus_id: &str,
    filter: &AtlasContextFilter,
) -> Result<BackfillOutcome, String> {
    let fut = backfill_ann(embed, atlas_dir, corpus_id, filter);
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            // Drive the backfill on the AMBIENT runtime, not a fresh one. The
            // embedder is a closure the caller built on that runtime -- an HTTP
            // client whose connection pool is bound to its reactor, or a
            // channel to a resident slot task -- and driving it from a foreign
            // reactor hangs or reports a dead IO driver. `block_in_place` hands
            // the worker back to the scheduler for the duration, so the caller
            // (the daemon, mid-ingest) keeps serving.
            tokio::task::block_in_place(|| h.block_on(fut))
        }
        // No ambient runtime (a plain sync caller), or a current-thread one
        // where `block_in_place` panics: the atlas module's own bridge, a
        // dedicated thread with its own reactor.
        _ => super::store::run_blocking(fut),
    }
}

/// Truncate atlas-entity text for embedding. Embed models cap context
/// somewhere around 8K tokens; entities with augmented descriptions
/// (questions + anchors aggregated across many sections) routinely run
/// 18KB chars. 3000 chars (~750 tokens) keeps headroom while still
/// covering the description and the strongest section signals.
const ATLAS_ENTRY_CHAR_LIMIT: usize = 3000;

/// Render a tension-edge endpoint as a single line for the virtual
/// chunk's embed text. Endpoint atoms are commonly Entities or
/// Claims, but the spec permits any atom type, so we cover the
/// natural-language fields each variant carries. Returns an
/// "<id> (missing)" placeholder when the edge points at an id that
/// doesn't resolve — better to keep the tension visible with a
/// half-known endpoint than to drop it silently.
fn endpoint_text(atom: Option<&AtomEnvelope>, atom_id: &str) -> String {
    use AtomEnvelope::*;
    match atom {
        Some(Entity(e)) => format!("{}: {}", e.canonical_name, e.description),
        Some(Claim(c)) => {
            let act = serde_json::to_string(&c.discourse_act)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let status = serde_json::to_string(&c.epistemic_status)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            format!("[Claim: {act}, {status}] {}", c.content)
        }
        Some(Question(q)) => format!("Question: {}", q.content),
        Some(State(s)) => format!("State: {}", s.label),
        Some(Relation(r)) => format!("Relation: {}", r.label),
        Some(Event(ev)) => format!("Event: {}", ev.description),
        Some(Configuration(cfg)) => format!("{}: {}", cfg.label, cfg.description),
        Some(ArgumentReconstruction(a)) => format!("Argument: {}", a.name),
        Some(Position(p)) => format!("Position ({}): {}", p.stance, p.canonical_name),
        Some(Opposition(o)) => format!("Opposition: {}", o.canonical_label),
        Some(Asset(a)) => {
            let name = if a.original_filename.is_empty() {
                format!("asset:{}", &a.sha256[..12.min(a.sha256.len())])
            } else {
                a.original_filename.clone()
            };
            format!("Asset ({}): {}", a.asset_kind, name)
        }
        None => format!("{atom_id} (missing)"),
    }
}

/// Read `atoms.json` for the named atlas corpus and embed each Entity's
/// `name + aliases + description` once per call. ATLAS_STORAGE_V2 Phase B
/// removed the `atoms.embeddings.bin` cache, so every call re-embeds from
/// `atoms.json` (multi-minute cold load for wiki-scale atlases); the
/// persistent `atoms_ann.lance` seed table is now the durable cross-run
/// artifact.
pub async fn load_atlas_context(
    embed: &EmbedFn,
    atlas_dir: &Path,
    atlas_corpus_id: &str,
    top_k: usize,
    filter: &AtlasContextFilter,
) -> Result<AtlasContext, LoadAtlasError> {
    if !atlas_dir.exists() {
        return Err(LoadAtlasError::NoAtlas {
            corpus_id: atlas_corpus_id.to_string(),
            atlas_dir: atlas_dir.to_path_buf(),
        });
    }

    let atoms = read_atlas_atoms(atlas_dir).map_err(|e| LoadAtlasError::Read(e.to_string()))?;

    // The corpus's DECLARED claim types, for the
    // `SOVEREIGN_ATLAS_INCLUDE_DECLARED_CLAIMS` knob below. Empty for every
    // corpus that declares nothing, which makes the admission guard inert
    // there whatever the knob says (I5).
    let declared_claim_types: Vec<String> = read_atlas_ontology(atlas_dir)
        .map(|f| f.policies)
        .filter(|p| p.has_declarations())
        .map(|p| p.claim_types().map(|t| t.name.clone()).collect())
        .unwrap_or_default();
    if filter.include_declared_claim_types && !declared_claim_types.is_empty() {
        tracing::debug!(
            corpus = atlas_corpus_id,
            declared_claim_types = ?declared_claim_types,
            "atlas loader: admitting declared claim types as virtual chunks"
        );
    }

    // Build embed-text per Entity, applying filters. Counters track
    // why each entity was kept or dropped so the pre-embed log is
    // diagnostic — operators tuning a Tier-2 atlas need to see "we
    // dropped 51000 structural one-liners and kept the 52 extracted
    // entries" rather than just a final total.
    // Path 2 Phase A — Claim atoms ride alongside Entities in the
    // virtual-chunk pool when `--atlas-include claim` is set. They
    // surface with `canonical_name = article_slug` so rigid-source
    // matching credits the article. For per-article SEP atlases the
    // corpus_id is `sep-<slug>`; strip it. Other atlases pass
    // through unchanged.
    let article_slug: String = atlas_corpus_id
        .strip_prefix("sep-")
        .unwrap_or(atlas_corpus_id)
        .to_string();

    // (atom_id, canonical_name, embed_text) per virtual chunk. atom_id is the
    // backing atom's id; it seeds the v2 persistent ANN table. Empty only for
    // edge-derived Tension chunks, which have no single backing atom.
    let mut payloads: Vec<(String, String, String)> = Vec::new();
    // Per-kind census, not six named counters. The population is a SET of atom
    // kinds now (`seed_population`), so a fixed fan-out over entities / claims
    // / configurations would go dark on exactly the kinds this order added.
    let mut seen_by_kind: BTreeMap<AtomType, usize> = BTreeMap::new();
    let mut kept_by_kind: BTreeMap<AtomType, usize> = BTreeMap::new();
    let mut drop_kind = 0usize;
    let mut drop_placeholder = 0usize;
    let mut drop_short_desc = 0usize;
    let mut drop_depth = 0usize;
    let mut drop_cap = 0usize;
    let mut drop_unrenderable: BTreeMap<AtomType, usize> = BTreeMap::new();
    for atom in &atoms.atoms {
        let kind = atom.atom_type();
        *seen_by_kind.entry(kind).or_default() += 1;
        // ONE admission predicate (ARCH §10.6): the seed population the map
        // derived, unioned with what the retrieval filter itself admits. It
        // can only widen — a corpus whose map names no Claim still keeps the
        // claims an operator switched on with `SOVEREIGN_ATLAS_INCLUDE_CLAIMS`.
        if !filter.admits_atom(atom, &declared_claim_types) {
            drop_kind += 1;
            continue;
        }
        // Entity-only quality gates. A NAMED atom is never a placeholder —
        // names are first-class grounding signal — so drop only atoms with no
        // name AND no description, and measure the FULL embed signal (name +
        // aliases + description) against the floor rather than the description
        // alone. (Was `description.is_empty() && salience == 0.0`, which
        // discarded named-but-unscored entities.) The other kinds carry no
        // name/description pair to measure, and their content IS the signal.
        if let AtomEnvelope::Entity(e) = atom {
            if e.canonical_name.trim().is_empty() && e.description.is_empty() {
                drop_placeholder += 1;
                continue;
            }
            let signal_len = e.canonical_name.len()
                + e.aliases.iter().map(|a| a.len()).sum::<usize>()
                + e.description.len();
            if signal_len < filter.min_description_chars {
                drop_short_desc += 1;
                continue;
            }
        }
        if !filter.depth_allowlist.is_empty() {
            // Match against the serialised form of EnrichmentDepth. `serde_json`
            // keeps it lowercase (snake_case) — the same form operators see in
            // atoms.json.
            let depth_label = serde_json::to_string(&atom.enrichment_depth())
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            if !filter
                .depth_allowlist
                .iter()
                .any(|d| d.eq_ignore_ascii_case(&depth_label))
            {
                drop_depth += 1;
                continue;
            }
        }
        if let Some(cap) = filter.max_entries {
            if payloads.len() >= cap {
                drop_cap += 1;
                continue;
            }
        }
        // The ONE renderer (`context::render_atom_entry`), not a second copy
        // of it. This loop carried a byte-identical fork of that function's
        // four arms until ei-3c — the same rendering the read-time bag builder
        // uses, written twice, which is the fork §10.6 names and the reason a
        // kind added to one side went missing on the other. `None` means the
        // renderer has no shape for this kind: the atom was ADMITTED and could
        // not be rendered, which is reported per kind below rather than folded
        // into a filter drop (§18.3).
        let Some((name, text)) = render_atom_entry(atom, &article_slug) else {
            *drop_unrenderable.entry(kind).or_default() += 1;
            continue;
        };
        payloads.push((atom.id().as_str().to_string(), name, text));
        *kept_by_kind.entry(kind).or_default() += 1;
    }

    // Path 2 Phase B — fold Tension edges into the virtual-chunk pool.
    // Tensions live in `edges.json`, not `atoms.json`. Each edge points
    // at two endpoint atoms (commonly Entities or Claims) and carries
    // a `sub_question` summarising the dialectical question the pair
    // turns on. Surfacing all three pieces in one embed text gives the
    // retriever a hit for questions phrased around that very tension.
    let mut kept_tensions = 0usize;
    let mut total_tensions = 0usize;
    if filter.include_tensions {
        // Build a lookup over atoms keyed by id once, since each edge
        // resolves two endpoints. Cheap — atlases are at most a few
        // thousand atoms.
        use std::collections::HashMap;
        let atoms_by_id: HashMap<&str, &AtomEnvelope> =
            atoms.atoms.iter().map(|a| (a.id().as_str(), a)).collect();
        match read_atlas_edges(atlas_dir) {
            Ok(edges_file) => {
                for edge in &edges_file.edges {
                    if edge.edge_type != EdgeType::Tension {
                        continue;
                    }
                    total_tensions += 1;
                    if let Some(cap) = filter.max_entries {
                        if payloads.len() >= cap {
                            drop_cap += 1;
                            continue;
                        }
                    }
                    let src = atoms_by_id.get(edge.source.as_str()).copied();
                    let tgt = atoms_by_id.get(edge.target.as_str()).copied();
                    let sub = edge
                        .sub_question
                        .as_deref()
                        .unwrap_or("(no sub_question recorded)");
                    let mut text = format!("[Tension] {sub}");
                    text.push('\n');
                    text.push_str(&endpoint_text(src, edge.source.as_str()));
                    text.push_str("\n↔\n");
                    text.push_str(&endpoint_text(tgt, edge.target.as_str()));
                    if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                        text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
                    }
                    payloads.push((String::new(), article_slug.clone(), text));
                    kept_tensions += 1;
                }
            }
            Err(e) => {
                // Missing edges.json is fine — older atlases may not
                // have run Phase 6. Log and continue with whatever we
                // already collected.
                tracing::warn!(
                    corpus = atlas_corpus_id,
                    error = %e,
                    "atlas-context: include_tensions requested but edges.json unreadable; skipping Tension surface"
                );
            }
        }
    }

    tracing::info!(
        corpus = atlas_corpus_id,
        population = %filter
            .seed_kinds
            .as_ref()
            .map(|p| p.iter().map(AtomType::label).collect::<Vec<_>>().join(","))
            .unwrap_or_else(|| "<retrieval filter>".to_string()),
        seen = ?seen_by_kind,
        kept = ?kept_by_kind,
        unrenderable = ?drop_unrenderable,
        kept_tensions,
        total_tensions,
        drop_kind,
        drop_placeholder,
        min_description_chars = filter.min_description_chars,
        drop_short_desc,
        drop_depth,
        drop_cap,
        top_k,
        "atlas-context: filtered atoms.json (pre-embed)"
    );
    // An ADMITTED kind the one renderer has no shape for is a hole in the
    // population, not a filter decision — say so at warn (§18.3), because the
    // corpus's own map asked for those seeds and did not get them.
    for (kind, n) in &drop_unrenderable {
        tracing::warn!(
            corpus = atlas_corpus_id,
            kind = kind.label(),
            atoms = n,
            "atlas-context: the seed population admits this kind and \
             `render_atom_entry` has no shape for it; those atoms are NOT seeded"
        );
    }
    if payloads.is_empty() {
        return Err(LoadAtlasError::FilterExcludedAll {
            corpus_id: atlas_corpus_id.to_string(),
            min_description_chars: filter.min_description_chars,
        });
    }

    let attempted = payloads.len();
    let mut entries: Vec<AtlasEntry> = Vec::with_capacity(attempted);
    let mut last_embed_error: Option<String> = None;
    let t0 = Instant::now();
    for (atom_id, name, text) in payloads {
        match embed(&text).await {
            Ok(v) => entries.push(AtlasEntry {
                atom_id,
                canonical_name: name,
                embed_text: text,
                embedding: v,
            }),
            Err(e) => {
                tracing::warn!(
                    corpus = atlas_corpus_id,
                    entry = %name,
                    error = %e,
                    "atlas-context: embed failed; entry skipped"
                );
                last_embed_error = Some(e.to_string());
            }
        }
    }
    // Every admitted atom refused. A per-entry warn is the right shape for a
    // few drops in a large atlas; a TOTAL refusal is a different fact and the
    // caller must be able to tell it from "the filter admitted nothing"
    // (ARCH 18.3 -- absence is reported, never defaulted into a neighbouring
    // outcome).
    if entries.is_empty() {
        return Err(LoadAtlasError::EmbedRefusedAll {
            corpus_id: atlas_corpus_id.to_string(),
            attempted,
            last_error: last_embed_error.unwrap_or_else(|| "no error recorded".into()),
        });
    }
    tracing::info!(
        corpus = atlas_corpus_id,
        entries = entries.len(),
        elapsed_ms = t0.elapsed().as_millis() as u64,
        "atlas-context: embedded"
    );

    Ok(AtlasContext {
        atlas_corpus_id: atlas_corpus_id.to_string(),
        entries,
        top_k,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::ann_store::{ann_table_is_fresh, ann_table_present};
    use std::sync::Arc;

    /// Embeds deterministically. The `InferenceProvider` impl this replaced
    /// carried three methods the loader never called — `complete` and
    /// `complete_stream` were `unreachable!()` and `capabilities` was filler —
    /// which is precisely the evidence that the parameter was only ever an
    /// embedder (ARCH §5.1: the trait was eight times wider than the use).
    fn unit_embed() -> EmbedFn {
        Arc::new(|text: &str| {
            let n = text.len() as f32;
            Box::pin(async move { Ok(vec![n, 1.0, 0.0, 0.0]) })
        })
    }

    /// The production grounding filter, spelled out so the test does not
    /// depend on the `SOVEREIGN_ATLAS_*` env knobs `Default` reads.
    fn grounding_filter() -> AtlasContextFilter {
        AtlasContextFilter {
            min_description_chars: 10,
            depth_allowlist: vec!["extracted".into()],
            max_entries: None,
            top_k: 3,
            include_claims: false,
            include_tensions: false,
            include_configurations: false,
            include_declared_claim_types: false,
            seed_kinds: None,
        }
    }

    /// One Entity envelope in the on-disk `atoms.json` shape (copied from a
    /// real maple-house atlas), at the given enrichment depth.
    fn atoms_json(depth: &str) -> String {
        format!(
            r#"{{"schema_version":"2","atoms":[{{"atom_type":"Entity","data":{{"id":"entity-0001","canonical_name":"guest logbook","entity_type":"work","first_appearance":{{"chunk_id":"sec_00001","passage_preview":"signed into the guest logbook"}},"description":"A physical record kept by the front door to track overnight guests.","salience":0.33,"enrichment_depth":"{depth}","provenance":{{"signal_kind":"llm_batch"}}}}}}]}}"#
        )
    }

    #[tokio::test]
    async fn backfill_ann_writes_a_fresh_table_for_an_extracted_entity() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas = tmp.path().join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        std::fs::write(atlas.join("atoms.json"), atoms_json("extracted")).unwrap();

        let out = backfill_ann(&unit_embed(), &atlas, "t", &grounding_filter())
            .await
            .expect("backfill succeeds");
        assert_eq!(
            out,
            BackfillOutcome::Built(AnnBuildStats {
                resolved: 1,
                total: 1
            })
        );
        assert!(ann_table_present(&atlas));
        assert!(
            ann_table_is_fresh(&atlas),
            "a table written after atoms.json must read as fresh"
        );
    }

    /// The typed skip: an atlas whose atoms all sit outside the grounding
    /// filter's depth allowlist (structural-only) writes no table and says
    /// so, distinguishable from a failure without matching message text.
    #[tokio::test]
    async fn backfill_ann_reports_no_seedable_atoms_when_the_filter_admits_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas = tmp.path().join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        std::fs::write(atlas.join("atoms.json"), atoms_json("structural")).unwrap();

        let out = backfill_ann(&unit_embed(), &atlas, "t", &grounding_filter())
            .await
            .expect("an admitted-nothing filter is an outcome, not an error");
        assert_eq!(
            out,
            BackfillOutcome::NoSeedableAtoms {
                min_description_chars: 10
            }
        );
        assert!(!ann_table_present(&atlas), "no table may be written");
    }

    /// One Entity, one Claim and one Configuration in the on-disk shape, plus
    /// an `ontology.json` whose navigation map seeds on Claim and
    /// Configuration. Copied from real atlases (wessex-hoard's claim,
    /// brothers-karamazov-book-1's configuration) so the fixture is not a
    /// hopeful guess at the wire format.
    fn atlas_with_a_claim_and_configuration_map(atlas: &std::path::Path) {
        std::fs::create_dir_all(atlas).unwrap();
        std::fs::write(
            atlas.join("atoms.json"),
            r#"{"schema_version":"2","atoms":[
              {"atom_type":"Entity","data":{"id":"entity-0001","canonical_name":"guest logbook",
               "entity_type":"work","first_appearance":{"chunk_id":"sec_00001","passage_preview":"p"},
               "description":"A physical record kept by the front door.","salience":0.33,
               "enrichment_depth":"extracted"}},
              {"atom_type":"Claim","data":{"id":"claim-0001",
               "content":"Prior to Aldfrith, English coins named mints or moneyers, never the ruler.",
               "discourse_act":"assert","epistemic_status":"confident","scope":"universal",
               "evidence":[{"chunk_id":"sec_00001","passage_preview":"p"}],
               "anchor":"before him","claim_kind":"attribution","enrichment_depth":"extracted"}},
              {"atom_type":"Configuration","data":{"id":"config-0001",
               "label":"The Father as the Source of Structural Chaos",
               "description":"An entropic centre that generates the novel's conflicts.",
               "constituent_atoms":["entity-0001","claim-0001"],
               "evidence":[{"chunk_id":"sec_0003"}],"confidence":0.92,
               "interpretive_note":"An alternative reading makes him a passive victim.",
               "enrichment_depth":"extracted"}}]}"#,
        )
        .unwrap();
        std::fs::write(
            atlas.join("ontology.json"),
            r#"{"schema_version":"1","ontology_version":1,"pipeline_id":"custom_atlas",
              "policies":{"shape":{"types":[{"name":"attribution","kind":"claim"}]},
              "navigation":{
                "thematic":{"seed":{"kinds":["Configuration","Entity"]},"walk":[],"hops":2,"budget":12},
                "trajectory":{"seed":{"kinds":[]},"walk":[],"hops":2,"budget":12},
                "tension":{"seed":{"kinds":["Claim","Position"]},"walk":[],"hops":1,"budget":12},
                "enumeration":{"seed":{"kinds":[]},"walk":[],"hops":0,"budget":12},
                "lookup":{"seed":{"kinds":[]},"walk":[],"hops":1,"budget":12}}}}"#,
        )
        .unwrap();
    }

    /// ei-3c's whole point: the seed table's population is the corpus's
    /// navigation map, not the retrieval filter. The filter passed in is the
    /// PRODUCTION one — claims off, configurations off — and the map's
    /// `tension` and `thematic` rows put both kinds in the table anyway.
    ///
    /// Failing input: `seed_population` narrowed to the filter's admission, or
    /// `backfill_ann` not attaching the population — either drops the table to
    /// the one Entity, which is the Entity-only state ei-4 measured on every
    /// atlas on this box.
    #[tokio::test]
    async fn the_seed_table_population_is_the_maps_not_the_retrieval_filters() {
        use crate::enrichment::atlas::ann_store::AnnSeedTable;
        let tmp = tempfile::tempdir().unwrap();
        let atlas = tmp.path().join("atlas");
        atlas_with_a_claim_and_configuration_map(&atlas);

        let out = backfill_ann(&unit_embed(), &atlas, "t", &grounding_filter())
            .await
            .expect("backfill succeeds");
        assert_eq!(
            out,
            BackfillOutcome::Built(AnnBuildStats {
                resolved: 3,
                total: 3
            }),
            "entity + claim + configuration, all three seeded"
        );

        // Read the ids back OUT of the table, not off the stats: the done-when
        // is that those KINDS land in it.
        let table = AnnSeedTable::open_for_atlas(&atlas)
            .await
            .expect("table opens");
        let mut ids = table
            .nearest(&[1.0_f32, 1.0, 0.0, 0.0], 16)
            .await
            .expect("nearest");
        ids.sort();
        assert_eq!(ids, vec!["claim-0001", "config-0001", "entity-0001"]);

        // …and the table records the population it was built under, so a later
        // build that derives a different one rebuilds rather than trusting it.
        assert!(crate::enrichment::atlas::seed_population::population_marker_is_current(&atlas));
        assert!(ann_table_is_fresh(&atlas));
    }

    #[tokio::test]
    async fn load_atlas_context_missing_atlas_is_a_typed_error() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas = tmp.path().join("nope").join("atlas");
        let err = load_atlas_context(&unit_embed(), &atlas, "t", 3, &grounding_filter())
            .await
            .err()
            .expect("missing atlas dir must be an error");
        assert!(matches!(err, LoadAtlasError::NoAtlas { .. }), "got {err:?}");
        assert!(err.to_string().contains("no atlas at"));
    }
}
