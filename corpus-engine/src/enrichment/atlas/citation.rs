//! Primary-source citation handles for atlas atoms.
//!
//! Empirical driver: 2026-05-24 typed-extension pass over RAPTOR
//! cluster summaries was the first atlas-producing pipeline to wire
//! verbatim source-sentence previews onto every atom's
//! [`ChunkRef`]. The need is general — any pipeline whose atoms
//! derive from a paraphrased intermediate (RAPTOR summary, GLiNER-
//! aggregated entity sketch, theme synthesis) wants the same
//! contract: every atom dereferences to a real source chunk plus a
//! verbatim sentence the operator can grep against the source.
//!
//! This module hoists the primitives out of the typed_extension
//! orchestration so future atlas pipelines inherit the contract by
//! default: build a `HashMap<section_id, SourceCitation>` while
//! emitting your synthetic [`SectionExtraction`]s, then call
//! [`apply_citation`] on every [`ChunkRef`] the resolver emitted.
//!
//! Naming: `SourcePassage` would be a clearer field-level term than
//! the transport-layer name `QuoteSpan` (used in
//! `sovereign_core::types`), but `corpus-engine` deliberately does
//! NOT depend on `sovereign-core` (ARCH §8.3 — one-way dep
//! direction). Consumers carrying structured QuoteSpan inputs
//! convert to a plain `(chunk_id: u32, text: &str)` pair at the
//! boundary before constructing a `SourceCitation`.
//!
//! See [`crate::enrichment::pipeline::typed_schemas::source_recovery`]
//! for the matching prompt fragment that instructs the model to PREFER
//! verbatim phrasings from these citations when naming atoms.

use std::collections::HashMap;

use super::atoms::ChunkRef;

/// Primary-source handle the orchestrator attaches to every section
/// it emits. Threads through [`super::resolution::resolve_type_extensions`]
/// (which currently encodes only the section id as the `chunk_id` on
/// every atom's `ChunkRef`) and gets consumed by [`apply_citation`]
/// to populate `ChunkRef.passage_preview` with the verbatim source
/// sentence.
///
/// Load-bearing for glassbox source recovery: an atom's
/// `first_appearance.chunk_id` resolves to a real chunk in the
/// corpus's `chunks.lance`, and its `passage_preview` carries the
/// verbatim sentence the model used to ground the atom.
#[derive(Debug, Clone)]
pub struct SourceCitation {
    /// The `section_id` the orchestrator put on the synthetic
    /// [`crate::enrichment::pipeline::atlas::SectionExtraction`].
    /// `resolve_type_extensions` copies this verbatim into every
    /// atom's `first_appearance.chunk_id` + every edge-emission
    /// `ChunkRef`. Shape: `chunk:<u32>` when a verbatim excerpt is
    /// available; falls back to a coarser per-pipeline tag (e.g.
    /// `raptor:<node>` or `theme:<id>`) when not — still useful for
    /// tracing, just at coarser grain.
    pub section_id: String,
    /// The verbatim sentence the resolver-emitted `ChunkRef`s should
    /// carry as their `passage_preview`. `None` when no source
    /// excerpt was available — `ChunkRef.passage_preview` stays
    /// `None` and the atom degrades to chunk-level grounding only.
    pub passage_preview: Option<String>,
}

impl SourceCitation {
    /// Build a citation from a primary source pair `(chunk_id, text)`.
    /// Callers carrying ordered excerpts (e.g. RAPTOR's
    /// cosine-to-centroid quote_spans) typically pick the first
    /// (most-representative) excerpt as the primary source handle for
    /// every atom the section produces.
    ///
    /// `fallback_section_id` is used when `primary` is `None` — keeps
    /// the citation usable at a coarser grain (atoms still ground at
    /// the RAPTOR-node or theme level rather than orphaning).
    pub fn from_primary(fallback_section_id: &str, primary: Option<(u32, &str)>) -> Self {
        match primary {
            Some((chunk_id, text)) => Self {
                section_id: format!("chunk:{chunk_id}"),
                passage_preview: Some(text.to_string()),
            },
            None => Self {
                section_id: fallback_section_id.to_string(),
                passage_preview: None,
            },
        }
    }
}

/// Project a resolver-emitted `ChunkRef` through the `citations`
/// lookup. The resolver writes `ChunkRef::new(section_id, None)` for
/// every atom + edge endpoint it builds; this walk replaces the
/// `(None)` preview with the verbatim source sentence the
/// orchestrator attached to that section_id.
///
/// No-op when:
/// - The `target.chunk_id` isn't in the citations map (defensive
///   pass-through for resolver shapes that emit `ChunkRef`s pointing
///   at existing atoms rather than synthetic-section ids).
/// - The citation has no verbatim preview available (the source had
///   no extractable excerpts — atom still grounds at chunk
///   granularity via the section_id, just without the sentence-level
///   handle).
/// - `target.passage_preview` is already populated upstream — never
///   clobber an existing preview.
///
/// The `chunk_id` itself is left untouched — orchestrators that
/// encoded the source chunk handle into the section_id
/// (`chunk:<u32>` form) thereby propagate the right grain through
/// the resolver's `ChunkRef::new(section_id, …)` calls.
pub fn apply_citation(target: &mut ChunkRef, citations: &HashMap<String, SourceCitation>) {
    if target.passage_preview.is_some() {
        return;
    }
    if let Some(citation) = citations.get(&target.chunk_id) {
        if let Some(preview) = citation.passage_preview.as_ref() {
            target.passage_preview = Some(preview.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citation_with_primary_points_at_source_chunk() {
        let citation = SourceCitation::from_primary(
            "raptor:fallback",
            Some((
                42,
                "Spread pricing lets PBMs charge payers more than they reimburse.",
            )),
        );
        assert_eq!(citation.section_id, "chunk:42");
        assert_eq!(
            citation.passage_preview.as_deref(),
            Some("Spread pricing lets PBMs charge payers more than they reimburse.")
        );
    }

    #[test]
    fn citation_without_primary_falls_back() {
        let citation = SourceCitation::from_primary("raptor:n-leaf-1", None);
        assert_eq!(citation.section_id, "raptor:n-leaf-1");
        assert!(citation.passage_preview.is_none());
    }

    #[test]
    fn apply_citation_populates_preview_from_map() {
        let mut map = HashMap::new();
        map.insert(
            "chunk:7".to_string(),
            SourceCitation {
                section_id: "chunk:7".to_string(),
                passage_preview: Some("verbatim source sentence".to_string()),
            },
        );
        let mut target = ChunkRef::new("chunk:7", None);
        apply_citation(&mut target, &map);
        assert_eq!(
            target.passage_preview.as_deref(),
            Some("verbatim source sentence")
        );
    }

    #[test]
    fn apply_citation_is_noop_when_target_already_has_preview() {
        let mut map = HashMap::new();
        map.insert(
            "chunk:7".to_string(),
            SourceCitation {
                section_id: "chunk:7".to_string(),
                passage_preview: Some("from map".to_string()),
            },
        );
        let mut target = ChunkRef::new("chunk:7", Some("already populated".into()));
        apply_citation(&mut target, &map);
        assert_eq!(target.passage_preview.as_deref(), Some("already populated"));
    }

    #[test]
    fn apply_citation_is_noop_when_section_id_missing() {
        let map: HashMap<String, SourceCitation> = HashMap::new();
        let mut target = ChunkRef::new("chunk:99", None);
        apply_citation(&mut target, &map);
        assert!(target.passage_preview.is_none());
    }

    #[test]
    fn apply_citation_is_noop_when_citation_has_no_preview() {
        let mut map = HashMap::new();
        map.insert(
            "raptor:n-1".to_string(),
            SourceCitation {
                section_id: "raptor:n-1".to_string(),
                passage_preview: None,
            },
        );
        let mut target = ChunkRef::new("raptor:n-1", None);
        apply_citation(&mut target, &map);
        assert!(target.passage_preview.is_none());
    }
}
