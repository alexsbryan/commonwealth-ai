// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas context bag — READ half.
//!
//! `load_atlas_context` reads `atoms.json` through the context filter and
//! returns the resident bag the walk seeds on, with its typed
//! [`LoadAtlasError`]. Carved into this leaf 2026-09-21 (FIVE_PROGRAMS §12
//! decision 1); the WRITE half — the ANN backfill — stays in corpus-engine's
//! `atlas::context_loader`, which re-imports this module at the historical
//! path (ARCH §10.6 — a re-export, never a twin).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub use crate::context::ATLAS_ENTRY_CHAR_LIMIT;
use crate::context::{render_atom_entry, AtlasContext, AtlasEntry};
use crate::context_filter::AtlasContextFilter;
use crate::raw::read_atlas_ontology;
use corpus_index::types::EmbedFn;
use understanding_vocab::atoms::{AtomEnvelope, AtomType};
use understanding_vocab::edges::EdgeType;
use understanding_vocab::read::{read_atlas_atoms, read_atlas_edges};

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
        // Labelled so a reader of the rendered line can tell derived
        // text from source text without consulting the atom.
        Some(Summary(s)) => format!("Summary (level {}): {}", s.level, s.text),
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
    for atom in atoms.atoms() {
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
            atoms.atoms().iter().map(|a| (a.id().as_str(), a)).collect();
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
