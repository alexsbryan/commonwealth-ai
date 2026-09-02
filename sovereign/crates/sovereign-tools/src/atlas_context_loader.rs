// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ONE atlas-context loader: `atoms.json` (+ `edges.json`) → filtered,
//! embedded [`AtlasContext`] bag — the input `build_persistent_ann_seed_table`
//! turns into the per-corpus ANN seed table (`atlas/atoms_ann.lance`).
//!
//! Moved here from `sovereign-cli-llm::eval_cmd::runner` (ontology-v1 P0.2)
//! so the daemon can seed a freshly written atlas in-process. The CLI
//! (`svrn atlas backfill-ann`, `migrate-all`, the eval harness) keeps a thin
//! wrapper that supplies `session.inference` and the atlas dir; the body is
//! byte-for-byte the runner's. It sits beside — not inside —
//! `atlas_context_manager.rs` because that file is at ~900 lines and the
//! loader is ~350 (ARCH §3.1); the manager re-exports it so
//! `atlas_context_manager::load_atlas_context` is the one name.
//!
//! Filter semantics live on [`AtlasContextFilter`] (the manager owns it —
//! ARCH §10.6, one decider). Diagnostics are `tracing` events, not stderr:
//! the same function now runs inside the daemon.

use std::path::{Path, PathBuf};
use std::time::Instant;

use corpus_engine::enrichment::atlas::ann_store::ANN_TABLE_DIRNAME;
use corpus_engine::enrichment::atlas::{
    read_atlas_atoms, read_atlas_edges, read_atlas_ontology, AtomEnvelope, EdgeType,
};
use sovereign_core::atlas_context::{
    atom_attributes_suffix, build_persistent_ann_seed_table, AnnBuildStats, AtlasContext,
    AtlasEntry,
};
use sovereign_core::traits::InferenceProvider;

use crate::atlas_context_manager::AtlasContextFilter;

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
    inference: &dyn InferenceProvider,
    atlas_dir: &Path,
    corpus_id: &str,
    filter: &AtlasContextFilter,
) -> Result<BackfillOutcome, String> {
    let ctx = match load_atlas_context(inference, atlas_dir, corpus_id, filter.top_k, filter).await
    {
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
    tracing::info!(
        corpus = corpus_id,
        resolved = stats.resolved,
        total = stats.total,
        table = %atlas_dir.join(ANN_TABLE_DIRNAME).display(),
        "backfill-ann: wrote ANN seed table"
    );
    Ok(BackfillOutcome::Built(stats))
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
    inference: &dyn InferenceProvider,
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
    let mut total_entities = 0usize;
    let mut total_claims = 0usize;
    let mut kept_claims = 0usize;
    let mut total_configurations = 0usize;
    let mut kept_configurations = 0usize;
    let mut drop_placeholder = 0usize;
    let mut drop_short_desc = 0usize;
    let mut drop_depth = 0usize;
    let mut drop_cap = 0usize;
    for atom in &atoms.atoms {
        match atom {
            AtomEnvelope::Entity(e) => {
                total_entities += 1;
                // A NAMED atom is never a placeholder — names are first-class
                // grounding signal. Drop only atoms with no name AND no
                // description (truly empty); the signal_len floor below governs
                // the rest. (Was `description.is_empty() && salience == 0.0`,
                // which discarded named-but-unscored entities — exactly the
                // baked-in signal the v2 migration must not lose.)
                let is_placeholder = e.canonical_name.trim().is_empty() && e.description.is_empty();
                if is_placeholder {
                    drop_placeholder += 1;
                    continue;
                }
                // Measure the atom's FULL embed signal — name + aliases +
                // description — not the description alone. The embed text
                // (render_atom_entry) is name+aliases+description, so a
                // richly-named entity with a terse description ("Pierre
                // Abelard", "abductive reasoning") is strong grounding signal
                // and must NOT be dropped. Names are first-class.
                let signal_len = e.canonical_name.len()
                    + e.aliases.iter().map(|a| a.len()).sum::<usize>()
                    + e.description.len();
                if signal_len < filter.min_description_chars {
                    drop_short_desc += 1;
                    continue;
                }
                if !filter.depth_allowlist.is_empty() {
                    // Match against the serialised form of EnrichmentDepth.
                    // `serde_json` keeps it lowercase (snake_case) — same form
                    // operators see in atoms.json.
                    let depth_label = serde_json::to_string(&e.enrichment_depth)
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
                payloads.push((e.id.as_str().to_string(), e.canonical_name.clone(), text));
            }
            // `include_claims` is the corpus-wide switch. The declared-type
            // arm is narrower and DARK: it admits only claims whose
            // `claim_kind` names a type the author declared, so an undeclared
            // corpus admits nothing new no matter how the knob is set.
            AtomEnvelope::Claim(c)
                if filter.include_claims
                    || (filter.include_declared_claim_types
                        && declared_claim_types
                            .iter()
                            .any(|t| Some(t.as_str()) == c.claim_kind.as_deref())) =>
            {
                total_claims += 1;
                if !filter.depth_allowlist.is_empty() {
                    let depth_label = serde_json::to_string(&c.enrichment_depth)
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
                payloads.push((c.id.as_str().to_string(), article_slug.clone(), text));
                kept_claims += 1;
            }
            AtomEnvelope::Configuration(cfg) if filter.include_configurations => {
                total_configurations += 1;
                if !filter.depth_allowlist.is_empty() {
                    let depth_label = serde_json::to_string(&cfg.enrichment_depth)
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
                let mut text = format!("[Configuration: {}] {}", cfg.label, cfg.description);
                if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                    text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
                }
                payloads.push((cfg.id.as_str().to_string(), article_slug.clone(), text));
                kept_configurations += 1;
            }
            AtomEnvelope::ArgumentReconstruction(a) => {
                // Always include — these are the named-argument
                // reconstructions Phase 1 extracted. Embed text is
                // name + premises + conclusion so a question
                // mentioning the argument name OR matching its
                // content can seed navigation onto this atom. The
                // article-slug `canonical_name` lets `score_sources`
                // credit the article when the atom is in top-K.
                if !filter.depth_allowlist.is_empty() {
                    let depth_label = serde_json::to_string(&a.enrichment_depth)
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
                let mut text = String::with_capacity(256);
                text.push_str("[Argument: ");
                text.push_str(&a.name);
                text.push_str("] ");
                for p in &a.premises {
                    text.push_str(p);
                    text.push(' ');
                }
                text.push_str(&a.conclusion);
                // Append objection content so cosine seeding picks
                // this argument when the question vocabulary
                // overlaps with an objection (e.g. "Frankfurt"
                // mentioned ⇒ Consequence Argument seeds).
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
                payloads.push((a.id.as_str().to_string(), article_slug.clone(), text));
            }
            _ => continue,
        }
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
        kept_entities = payloads.len() - kept_claims - kept_tensions - kept_configurations,
        total_entities,
        kept_claims,
        total_claims,
        kept_tensions,
        total_tensions,
        kept_configurations,
        total_configurations,
        drop_placeholder,
        min_description_chars = filter.min_description_chars,
        drop_short_desc,
        drop_depth,
        drop_cap,
        top_k,
        "atlas-context: filtered atoms.json (pre-embed)"
    );
    if payloads.is_empty() {
        return Err(LoadAtlasError::FilterExcludedAll {
            corpus_id: atlas_corpus_id.to_string(),
            min_description_chars: filter.min_description_chars,
        });
    }

    let mut entries: Vec<AtlasEntry> = Vec::with_capacity(payloads.len());
    let t0 = Instant::now();
    for (atom_id, name, text) in payloads {
        match inference.embed_query(&text).await {
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
            }
        }
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
    use corpus_engine::enrichment::atlas::ann_store::{ann_table_is_fresh, ann_table_present};
    use sovereign_core::types::{
        CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
    };
    use std::pin::Pin;

    /// Embeds deterministically and never completes: the writer's only
    /// inference need is `embed_query`, so a `complete()` call is a wiring
    /// bug and panics.
    struct UnitEmbed;

    #[async_trait::async_trait]
    impl InferenceProvider for UnitEmbed {
        async fn complete(
            &self,
            _: &CompletionRequest,
        ) -> sovereign_core::Result<CompletionResponse> {
            unreachable!("backfill must not call complete()")
        }
        async fn complete_stream(
            &self,
            _: &CompletionRequest,
        ) -> sovereign_core::Result<
            Pin<Box<dyn futures::Stream<Item = sovereign_core::Result<String>> + Send>>,
        > {
            unreachable!("backfill must not stream")
        }
        async fn embed(&self, text: &str) -> sovereign_core::Result<Vec<f32>> {
            Ok(vec![text.len() as f32, 1.0, 0.0, 0.0])
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 8192,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            }
        }
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

        let out = backfill_ann(&UnitEmbed, &atlas, "t", &grounding_filter())
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

        let out = backfill_ann(&UnitEmbed, &atlas, "t", &grounding_filter())
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

    #[tokio::test]
    async fn load_atlas_context_missing_atlas_is_a_typed_error() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas = tmp.path().join("nope").join("atlas");
        let err = load_atlas_context(&UnitEmbed, &atlas, "t", 3, &grounding_filter())
            .await
            .err()
            .expect("missing atlas dir must be an error");
        assert!(matches!(err, LoadAtlasError::NoAtlas { .. }), "got {err:?}");
        assert!(err.to_string().contains("no atlas at"));
    }
}
