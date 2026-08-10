// SPDX-License-Identifier: AGPL-3.0-or-later
//! Enrich rung mechanism — atom link-integrity: a deterministic post-pass over
//! the atoms enrichment produced. Reuses the canonical `AtomsFile` loader, the
//! `AtomEnvelope::evidence` accessor, `ChapterManifest` resolution, and the
//! index chunk read-back — nothing is reimplemented. No verdict thresholds
//! here; that judgment is the eval layer's.

use std::collections::HashMap;
use std::path::Path;

use crate::enrichment::atlas::atoms::{AtomEnvelope, AtomsFile};
use crate::enrichment::pipeline::chapter_manifest::ChapterManifest;
use crate::error::{Error, Result};
use crate::index::CorpusIndex;

/// One atom-evidence reference that failed an integrity check.
#[derive(Debug, Clone)]
pub struct EnrichMiss {
    pub atom_id: String,
    pub chunk_id: String,
    /// The unresolved id, or the quote that wasn't a verbatim substring.
    pub detail: String,
}

/// Atom link-integrity observations over the produced atoms — the mechanism.
pub struct EnrichOutput {
    pub atoms: usize,
    pub refs: usize,
    /// Evidence ids that did not resolve to a real chunk.
    pub unresolved: Vec<EnrichMiss>,
    /// Cited quotes that were not a verbatim substring of their chunk.
    pub non_verbatim: Vec<EnrichMiss>,
}

/// Pure integrity check: given the atoms and a map of already-resolved chunk
/// texts (keyed by the `ChunkRef.chunk_id` string), record evidence ids that
/// don't resolve and quotes that aren't verbatim substrings. No I/O — the
/// resolution happens in [`verify_atoms`] so this stays unit-testable.
pub fn check_evidence(atoms: &[AtomEnvelope], resolved: &HashMap<String, String>) -> EnrichOutput {
    let mut out = EnrichOutput {
        atoms: atoms.len(),
        refs: 0,
        unresolved: Vec::new(),
        non_verbatim: Vec::new(),
    };
    for atom in atoms {
        let atom_id = atom.id().as_str().to_string();
        for r in atom.evidence() {
            out.refs += 1;
            match resolved.get(&r.chunk_id) {
                None => out.unresolved.push(EnrichMiss {
                    atom_id: atom_id.clone(),
                    chunk_id: r.chunk_id.clone(),
                    detail: r.chunk_id.clone(),
                }),
                Some(chunk_text) => {
                    if let Some(quote) = &r.passage_preview {
                        let q = quote.trim();
                        if !q.is_empty() && !chunk_text.contains(q) {
                            out.non_verbatim.push(EnrichMiss {
                                atom_id: atom_id.clone(),
                                chunk_id: r.chunk_id.clone(),
                                detail: q.chars().take(120).collect(),
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

/// Verify the atoms written to `atlas_dir/atoms.json`: every evidence
/// `chunk_id` must resolve to a real chunk (a numeric id reads the index row;
/// a `sec_NNNNN` id goes via the chapter manifest), and every `passage_preview`
/// must be a verbatim substring of its cited chunk. Reuses the canonical loader
/// + resolution; the pure logic is [`check_evidence`].
pub async fn verify_atoms(
    atlas_dir: &Path,
    index: &CorpusIndex,
    chapters_path: &Path,
) -> Result<EnrichOutput> {
    let raw = std::fs::read(atlas_dir.join("atoms.json"))?;
    let file: AtomsFile = serde_json::from_slice(&raw)
        .map_err(|e| Error::Serialization(format!("parse atoms.json: {e}")))?;
    let manifest = ChapterManifest::load(chapters_path)?;

    // Resolve every distinct chunk_id once (async I/O), then run the pure check.
    let mut resolved: HashMap<String, String> = HashMap::new();
    for atom in &file.atoms {
        for r in atom.evidence() {
            if resolved.contains_key(&r.chunk_id) {
                continue;
            }
            if let Some(text) = resolve_chunk_text(&r.chunk_id, index, manifest.as_ref()).await {
                resolved.insert(r.chunk_id.clone(), text);
            }
        }
    }
    Ok(check_evidence(&file.atoms, &resolved))
}

/// Convenience over [`verify_atoms`]: given a corpus index directory, locate
/// `atlas/atoms.json` + `chapters.json`, open the index, and verify. Returns
/// `Ok(None)` when the corpus has no atoms yet (not enriched) — the caller
/// reports that rather than treating it as a failure.
///
/// The harness verifies the atoms the real ingest/enrichment pipeline produced
/// (its single source of truth) rather than re-running enrichment itself —
/// atom production is the pipeline's job; checking integrity is the harness's.
pub async fn verify_atoms_at(index_dir: &Path) -> Result<Option<EnrichOutput>> {
    let atlas_dir = index_dir.join("atlas");
    if !atlas_dir.join("atoms.json").exists() {
        return Ok(None);
    }
    let index = CorpusIndex::open(index_dir).await?;
    let chapters = ChapterManifest::default_path(index_dir);
    Ok(Some(verify_atoms(&atlas_dir, &index, &chapters).await?))
}

/// Decode a *direct* chunk-id evidence ref to its numeric index id. Pure (no
/// I/O) so the parse contract is pinned by unit tests, not a live corpus.
///
/// Handles the two shapes that name an index chunk id directly:
///   * `chunk:<u32>` — the canonical citation encoding
///     ([`citation::SourceCitation::from_primary`] writes `format!("chunk:{id}")`);
///     the `<u32>` IS the index chunk id. The common case on every flat corpus
///     — without decoding it the rung reported 100 % false-unresolved.
///   * bare numeric `<u64>` — legacy / direct chunk id.
///
/// Returns `None` for a chapter id (`sec_NNNNN`) or any non-numeric shape —
/// those resolve via the chapter manifest instead (see [`resolve_chunk_text`]).
fn parse_direct_chunk_id(chunk_id: &str) -> Option<u64> {
    chunk_id
        .strip_prefix("chunk:")
        .unwrap_or(chunk_id)
        .parse::<u64>()
        .ok()
}

/// Resolve a `ChunkRef.chunk_id` to chunk text. A direct id (`chunk:<u32>` or
/// bare numeric, via [`parse_direct_chunk_id`]) reads the index row; a
/// `sec_NNNNN` chapter id resolves via the chapter manifest's `chunk_ids`.
/// Returns `None` when the id resolves to no chunk.
async fn resolve_chunk_text(
    chunk_id: &str,
    index: &CorpusIndex,
    manifest: Option<&ChapterManifest>,
) -> Option<String> {
    if let Some(id) = parse_direct_chunk_id(chunk_id) {
        let rows = index.get_chunks(&[id]).await.ok()?;
        return rows.into_iter().next().map(|c| c.content);
    }
    let entry = manifest?.get(chunk_id)?;
    if entry.chunk_ids.is_empty() {
        return None;
    }
    let rows = index.get_chunks(&entry.chunk_ids).await.ok()?;
    if rows.is_empty() {
        return None;
    }
    Some(
        rows.into_iter()
            .map(|c| c.content)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{AtomId, ChunkRef, Entity, Event, SectionPosition};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType, EventType};

    fn entity_citing(chunk: &str, quote: &str) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity(1),
            canonical_name: "Widget".into(),
            aliases: Vec::new(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new(chunk, Some(quote.into())),
            description: "d".into(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        })
    }

    fn event_citing(refs: Vec<ChunkRef>) -> AtomEnvelope {
        AtomEnvelope::Event(Event {
            id: AtomId::event(1),
            description: "d".into(),
            event_type: EventType::Decision,
            participants: Vec::new(),
            evidence: refs,
            section_position: SectionPosition::section("sec_0001"),
            causal_antecedents: Vec::new(),
            enrichment_depth: EnrichmentDepth::Extracted,
        })
    }

    #[test]
    fn flags_unresolved_ids_and_non_verbatim_quotes() {
        let atoms = vec![
            // Verbatim quote on a resolvable chunk — clean.
            entity_citing("c1", "about widgets"),
            // One non-verbatim quote on c1, one dangling chunk c99.
            event_citing(vec![
                ChunkRef::new("c1", Some("a quote that is NOT present".into())),
                ChunkRef::new("c99", Some("dangling".into())),
            ]),
        ];
        let mut resolved = HashMap::new();
        resolved.insert(
            "c1".to_string(),
            "a paragraph about widgets and gadgets".to_string(),
        );

        let out = check_evidence(&atoms, &resolved);
        assert_eq!(out.atoms, 2);
        assert_eq!(out.refs, 3, "1 entity ref + 2 event refs");
        assert_eq!(out.unresolved.len(), 1, "c99 dangles");
        assert_eq!(out.unresolved[0].chunk_id, "c99");
        assert_eq!(out.non_verbatim.len(), 1, "the NOT-present quote on c1");
        assert!(out.non_verbatim[0].detail.contains("NOT present"));
    }

    #[test]
    fn clean_atoms_have_no_misses() {
        let atoms = vec![entity_citing("c1", "widgets")];
        let mut resolved = HashMap::new();
        resolved.insert("c1".to_string(), "all about widgets".to_string());
        let out = check_evidence(&atoms, &resolved);
        assert!(out.unresolved.is_empty());
        assert!(out.non_verbatim.is_empty());
        assert_eq!(out.refs, 1);
    }

    #[test]
    fn parse_direct_chunk_id_decodes_canonical_and_legacy() {
        // The regression this pins: the canonical citation encoding is
        // `chunk:<u32>` (citation.rs `format!("chunk:{id}")`). Rung-6 once
        // parsed the id as a bare u64, so `chunk:409` failed → 100 %
        // false-unresolved on every flat corpus.
        assert_eq!(parse_direct_chunk_id("chunk:409"), Some(409));
        assert_eq!(parse_direct_chunk_id("chunk:0"), Some(0));
        // Bare numeric (legacy / direct chunk id) still works.
        assert_eq!(parse_direct_chunk_id("42"), Some(42));
        // Chapter ids and any non-numeric shape are NOT direct ids — they
        // resolve via the chapter manifest, so the decoder must decline them.
        assert_eq!(parse_direct_chunk_id("sec_0001"), None);
        assert_eq!(parse_direct_chunk_id("chunk:abc"), None);
        assert_eq!(parse_direct_chunk_id("ch015"), None);
        assert_eq!(parse_direct_chunk_id(""), None);
    }

    /// Live rung-6 proof over a REAL on-disk enriched corpus — the atoms the
    /// production pipeline actually wrote (`atlas/atoms.json`), resolved against
    /// the real index + chapter manifest. This is the check the toy 4-doc smoke
    /// could not exercise (it produced 0 atoms). Set `ATOMS_CORPUS_DIR` and run:
    ///   ATOMS_CORPUS_DIR=~/.svrnmesh/indexes/<corpus> cargo test -p corpus-engine \
    ///     --lib harness::enrich::tests::verify_real_corpus -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs a real enriched corpus on disk; set ATOMS_CORPUS_DIR"]
    async fn verify_real_corpus() {
        let dir = std::path::PathBuf::from(
            std::env::var("ATOMS_CORPUS_DIR")
                .expect("set ATOMS_CORPUS_DIR=<index_dir with atlas/atoms.json>"),
        );
        eprintln!("[rung6] verify_atoms_at {}", dir.display());
        let out = match verify_atoms_at(&dir).await {
            Err(e) => {
                eprintln!("[rung6] verify errored (index unopenable / parse fail): {e}");
                return;
            }
            Ok(None) => {
                eprintln!("[rung6] no atoms.json — corpus not enriched");
                return;
            }
            Ok(Some(o)) => o,
        };

        let resolved = out.refs - out.unresolved.len();
        let verbatim = out.refs - out.non_verbatim.len();
        eprintln!(
            "[rung6] atoms={} evidence_refs={} | resolved={}/{} ({:.0}%) | verbatim_clean={}/{} non_verbatim={}",
            out.atoms,
            out.refs,
            resolved,
            out.refs,
            if out.refs > 0 { 100.0 * resolved as f64 / out.refs as f64 } else { 0.0 },
            verbatim,
            out.refs,
            out.non_verbatim.len(),
        );
        for m in out.unresolved.iter().take(8) {
            eprintln!("  UNRESOLVED   atom={} chunk_id={}", m.atom_id, m.chunk_id);
        }
        for m in out.non_verbatim.iter().take(8) {
            eprintln!(
                "  NON-VERBATIM atom={} chunk={} quote={:?}",
                m.atom_id, m.chunk_id, m.detail
            );
        }

        // Pure diagnostic: the value is the printed report above (resolution
        // rate + surfaced misses), not a pass/fail — the eval layer owns the
        // verdict, and a dangling ref is a *finding*, not a test error.
    }
}
