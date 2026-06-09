// SPDX-License-Identifier: AGPL-3.0-or-later
//! AtomSpan detector — finds where atoms surface in a chunk's text.
//!
//! Powers the desktop reading surface's atom layer: for every chunk
//! the user reads, we want to know which character ranges correspond
//! to atoms in the atlas, so the UI can render dotted underlines
//! (and on click, open the atom panel).
//!
//! ### Section-id ↔ chunk join
//!
//! Atom evidence today carries `chunk_id: String` that's actually a
//! **section id** (per `enrichment/atlas/atoms.rs:63-66`: "Step 3a
//! fills `chunk_id` with the section id; Phase 5 refines it to the
//! paragraph chunk id once the full chunk index is traversed").
//! The sectioned chunker stamps `section_id` into chunk metadata, so
//! we can join atoms to chunks at the section grain.
//!
//! - Caller passes `section_id` extracted from the chunk's metadata
//!   JSON. `None` means the chunk wasn't produced by a sectioned
//!   chunker — atom layer no-ops gracefully (returns empty Vec).
//! - We filter atoms whose `evidence[].chunk_id == section_id`.
//! - For each surviving atom, look for surface forms in the chunk
//!   text via case-insensitive whole-word search.
//!
//! ### Why not fold()?
//!
//! `enrichment::atlas::resolution::fold` strips diacritics for
//! drift-tolerant matching ("Karamázov" ↔ "Karamazov"). It's
//! powerful but non-length-preserving, which complicates byte-offset
//! round-trip back to the original chunk text. For v1, atoms were
//! extracted *from* the chunk text in the same ingest, so their
//! canonical_name + aliases should appear verbatim. Case-insensitive
//! match handles common drift; full diacritic-tolerant matching can
//! land later when Phase 5 refines evidence to chunk-level precision
//! and we need it.

use crate::enrichment::atlas::AtomEnvelope;

/// A located atom mention in the chunk text. Byte offsets are valid
/// indices into the original chunk text — `&text[span_start..span_end]`
/// must equal `surface_form`.
#[derive(Debug, Clone, PartialEq)]
pub struct AtomSpan {
    pub atom_id: String,
    /// Stable string discriminator. The on-disk envelope tag uses
    /// PascalCase (`"Entity"`, `"Event"`); we lowercase it for the
    /// HTTP wire so frontend CSS classes can use it directly
    /// (`atom--entity`, `atom--event`).
    pub atom_type: &'static str,
    pub span_start: usize,
    pub span_end: usize,
    pub surface_form: String,
}

/// Detect atom mentions in a chunk's text.
///
/// `section_id` is the `section_id` value extracted from chunk
/// metadata (when produced by the sectioned chunker). When `None`,
/// returns empty — non-sectioned corpora don't get an atom layer in
/// v1 because atom evidence is anchored at section grain.
pub fn detect_atom_spans(
    text: &str,
    section_id: Option<&str>,
    atoms: &[AtomEnvelope],
) -> Vec<AtomSpan> {
    let Some(section) = section_id else {
        return Vec::new();
    };
    if text.is_empty() {
        return Vec::new();
    }

    // Stage 1 — gather (atom_id, atom_type_label, surface_forms) for
    // every atom anchored at this section. We only emit spans for
    // atoms whose evidence (or first_appearance, for entities) ties
    // them to this section. This avoids spurious matches from atoms
    // that happen to share a token with a chunk word but aren't
    // actually anchored here.
    let mut candidates: Vec<(String, &'static str, Vec<String>)> = Vec::new();
    for atom in atoms {
        if !atom_anchored_at(atom, section) {
            continue;
        }
        let surface_forms = atom_surface_forms(atom);
        if surface_forms.is_empty() {
            continue;
        }
        candidates.push((
            atom.id().as_str().to_string(),
            atom_type_label(atom),
            surface_forms,
        ));
    }

    // Stage 2 — find every (start, end, atom_id, atom_type, form)
    // triple by walking each (atom, surface_form) pair across the
    // text. Whole-word, case-insensitive.
    let lower_text = text.to_lowercase();
    let mut hits: Vec<AtomSpan> = Vec::new();
    for (atom_id, atom_type, forms) in &candidates {
        for form in forms {
            if form.is_empty() {
                continue;
            }
            let lower_form = form.to_lowercase();
            for (start, end) in find_whole_word_byte_ranges(&lower_text, &lower_form) {
                // Map back to the *original* text. Lowercasing in
                // ASCII is length-preserving; for non-ASCII chars
                // the byte length of the lowercase form can differ
                // from the original (e.g. uppercase German 'ẞ' is
                // 3 bytes but its lowercase 'ß' is 2). Guard with a
                // text-slice equality check on the lowercase
                // representation rather than blindly slicing.
                if start > text.len() || end > text.len() {
                    continue;
                }
                let raw = &text[start..end];
                if raw.to_lowercase() != lower_form {
                    continue;
                }
                hits.push(AtomSpan {
                    atom_id: atom_id.clone(),
                    atom_type,
                    span_start: start,
                    span_end: end,
                    surface_form: raw.to_string(),
                });
            }
        }
    }

    // Stage 3 — resolve overlaps. Sort by start asc, then by length
    // desc (longest match wins on ties). Walk and skip any span that
    // overlaps a previously-accepted span.
    hits.sort_by(|a, b| {
        a.span_start
            .cmp(&b.span_start)
            .then_with(|| (b.span_end - b.span_start).cmp(&(a.span_end - a.span_start)))
    });
    let mut accepted: Vec<AtomSpan> = Vec::with_capacity(hits.len());
    let mut cursor: usize = 0;
    for span in hits {
        if span.span_start >= cursor {
            cursor = span.span_end;
            accepted.push(span);
        }
    }
    accepted
}

fn atom_type_label(atom: &AtomEnvelope) -> &'static str {
    match atom {
        AtomEnvelope::Entity(_) => "entity",
        AtomEnvelope::Event(_) => "event",
        AtomEnvelope::State(_) => "state",
        AtomEnvelope::Relation(_) => "relation",
        AtomEnvelope::Claim(_) => "claim",
        AtomEnvelope::Question(_) => "question",
        AtomEnvelope::Configuration(_) => "configuration",
        AtomEnvelope::ArgumentReconstruction(_) => "argument",
        AtomEnvelope::Position(_) => "position",
        AtomEnvelope::Opposition(_) => "opposition",
        AtomEnvelope::Asset(_) => "asset",
    }
}

/// True when this atom has any anchor (evidence chunk_id, first
/// appearance, section position, or section range) pointing at the
/// given section.
fn atom_anchored_at(atom: &AtomEnvelope, section: &str) -> bool {
    match atom {
        AtomEnvelope::Entity(e) => e.first_appearance.chunk_id == section,
        AtomEnvelope::Event(e) => {
            e.section_position.section_id == section
                || e.evidence.iter().any(|c| c.chunk_id == section)
        }
        AtomEnvelope::State(s) => {
            section_in_range(section, &s.section_range.start, &s.section_range.end)
                || s.evidence.iter().any(|c| c.chunk_id == section)
        }
        AtomEnvelope::Relation(r) => r.evidence.iter().any(|c| c.chunk_id == section),
        AtomEnvelope::Claim(c) => c.evidence.iter().any(|cr| cr.chunk_id == section),
        AtomEnvelope::Question(q) => q.raised_at.iter().any(|c| c.chunk_id == section),
        AtomEnvelope::Configuration(c) => c.evidence.iter().any(|cr| cr.chunk_id == section),
        AtomEnvelope::ArgumentReconstruction(a) => {
            a.section_position.section_id == section
                || a.evidence.iter().any(|c| c.chunk_id == section)
        }
        AtomEnvelope::Position(p) => p.first_appearance.chunk_id == section,
        AtomEnvelope::Opposition(o) => o.first_appearance.chunk_id == section,
        // Asset atoms attach to documents, not sections — anchored
        // at the corpus level rather than within a single chunk.
        AtomEnvelope::Asset(_) => false,
    }
}

/// Inclusive section-id range check. Section ids are typically
/// `sec_NNNN` strings; lexicographic comparison gives the right
/// answer when ids are zero-padded fixed-width (which the chunker
/// guarantees).
fn section_in_range(section: &str, start: &str, end: &str) -> bool {
    section >= start && section <= end
}

/// Surface forms worth searching for in chunk text. Different atom
/// types have very different shapes — entities have clean
/// canonical_name + aliases; claims/questions/configurations are
/// long sentences that don't span-match cleanly. v1 emits spans
/// for the atom types where the match is likely to be precise:
/// Entity (canonical_name + aliases) and State (label).
///
/// Other atom types are reachable from clicked-entity context (the
/// atom panel's "where else this appears" section), so they don't
/// need to be span-matched in chunk text to be discoverable.
fn atom_surface_forms(atom: &AtomEnvelope) -> Vec<String> {
    match atom {
        AtomEnvelope::Entity(e) => {
            let mut forms: Vec<String> = Vec::with_capacity(1 + e.aliases.len());
            forms.push(e.canonical_name.clone());
            forms.extend(e.aliases.iter().cloned());
            forms
        }
        AtomEnvelope::State(s) => {
            // State labels are short condition phrases ("in
            // crisis", "as novice"). Span-matching them lets the
            // user hover a state and reach the entity it
            // describes.
            if s.label.len() >= 3 {
                vec![s.label.clone()]
            } else {
                Vec::new()
            }
        }
        AtomEnvelope::ArgumentReconstruction(a) => {
            // The argument's name ("Knowledge Argument") is the
            // span-matchable handle.
            if a.name.len() >= 3 {
                vec![a.name.clone()]
            } else {
                Vec::new()
            }
        }
        AtomEnvelope::Position(p) => {
            // Position name is a 3-7-word stance label; usable as a
            // surface form when long enough to disambiguate.
            if p.canonical_name.len() >= 3 {
                vec![p.canonical_name.clone()]
            } else {
                Vec::new()
            }
        }
        AtomEnvelope::Opposition(o) => {
            // Opposition canonical label combines both sides; the
            // raw left/right labels are also worth surfacing.
            let mut forms = Vec::with_capacity(3);
            if o.canonical_label.len() >= 3 {
                forms.push(o.canonical_label.clone());
            }
            if o.left_label.len() >= 3 {
                forms.push(o.left_label.clone());
            }
            if o.right_label.len() >= 3 {
                forms.push(o.right_label.clone());
            }
            forms
        }
        // Other atom types: no clean surface form for span
        // matching. Reachable via entity → atom panel.
        AtomEnvelope::Event(_)
        | AtomEnvelope::Relation(_)
        | AtomEnvelope::Claim(_)
        | AtomEnvelope::Question(_)
        | AtomEnvelope::Configuration(_)
        // Asset atoms surface via the Attaches edge from their
        // carrier doc, not via span-matching in chunk text.
        | AtomEnvelope::Asset(_) => Vec::new(),
    }
}

/// Find every whole-word byte-range of `needle` in `haystack`.
/// Both arguments must already be lowercased.
fn find_whole_word_byte_ranges(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return Vec::new();
    }
    let is_boundary = |c: char| !c.is_alphanumeric();
    let mut hits = Vec::new();
    let mut start = 0usize;
    while let Some(rel) = haystack[start..].find(needle) {
        let pos = start + rel;
        let end = pos + needle.len();

        let before_ok = pos == 0
            || haystack[..pos]
                .chars()
                .last()
                .map(is_boundary)
                .unwrap_or(true);
        let after_ok = end == haystack.len()
            || haystack[end..]
                .chars()
                .next()
                .map(is_boundary)
                .unwrap_or(true);
        if before_ok && after_ok {
            hits.push((pos, end));
        }
        // Advance past the start of this candidate (not its end —
        // overlapping needles are rare but possible; we let the
        // overlap resolver handle them).
        start = pos + needle.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::{AtomId, ChunkRef, Entity, SectionRange, State};
    use crate::enrichment::pipeline::{EnrichmentDepth, EntityType, StateType};

    fn entity_atom(idx: usize, canonical: &str, aliases: &[&str], section: &str) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity(idx),
            canonical_name: canonical.into(),
            aliases: aliases.iter().map(|s| (*s).to_string()).collect(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new(section, None),
            description: "test".into(),
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

    #[test]
    fn returns_empty_when_no_section_id() {
        let atoms = vec![entity_atom(1, "Alyosha", &[], "sec_0001")];
        assert!(detect_atom_spans("Alyosha walked.", None, &atoms).is_empty());
    }

    #[test]
    fn no_spans_when_atom_anchored_at_different_section() {
        let atoms = vec![entity_atom(1, "Alyosha", &[], "sec_0042")];
        // Same text, but the atom anchors at a different section —
        // so we don't light it up here.
        assert!(detect_atom_spans("Alyosha walked.", Some("sec_0001"), &atoms).is_empty());
    }

    #[test]
    fn finds_canonical_name_with_byte_offsets_that_round_trip() {
        let atoms = vec![entity_atom(1, "Alyosha", &[], "sec_0001")];
        let text = "When Alyosha entered the room, he was silent.";
        let spans = detect_atom_spans(text, Some("sec_0001"), &atoms);
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!(s.atom_type, "entity");
        assert_eq!(&text[s.span_start..s.span_end], "Alyosha");
        assert_eq!(s.surface_form, "Alyosha");
    }

    #[test]
    fn case_insensitive_match_preserves_original_casing_in_surface_form() {
        let atoms = vec![entity_atom(1, "Alyosha", &[], "sec_0001")];
        let text = "alyosha and ALYOSHA are the same person.";
        let spans = detect_atom_spans(text, Some("sec_0001"), &atoms);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].surface_form, "alyosha");
        assert_eq!(spans[1].surface_form, "ALYOSHA");
    }

    #[test]
    fn alias_matches_when_canonical_does_not() {
        let atoms = vec![entity_atom(
            1,
            "Alexei Fyodorovich Karamazov",
            &["Alyosha"],
            "sec_0001",
        )];
        let text = "Alyosha is a novice.";
        let spans = detect_atom_spans(text, Some("sec_0001"), &atoms);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].surface_form, "Alyosha");
    }

    #[test]
    fn whole_word_boundary_prevents_substring_false_positive() {
        // "Fyodor" should NOT match inside the patronymic
        // "Fyodorovich" — the textbook drift-confusion bug.
        let atoms = vec![entity_atom(1, "Fyodor", &[], "sec_0001")];
        let text = "Alexei Fyodorovich was the youngest.";
        let spans = detect_atom_spans(text, Some("sec_0001"), &atoms);
        assert!(spans.is_empty(), "spans = {:?}", spans);
    }

    #[test]
    fn longest_match_wins_on_overlap() {
        // Two atoms whose surface forms overlap on the same span:
        // the longer one should win, the shorter is suppressed.
        let atoms = vec![
            entity_atom(1, "Fyodor Karamazov", &[], "sec_0001"),
            entity_atom(2, "Fyodor", &[], "sec_0001"),
        ];
        let text = "Fyodor Karamazov died first.";
        let spans = detect_atom_spans(text, Some("sec_0001"), &atoms);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].surface_form, "Fyodor Karamazov");
        assert_eq!(spans[0].atom_id, "entity-0001");
    }

    #[test]
    fn state_label_lights_up() {
        let state = AtomEnvelope::State(State {
            id: AtomId::state(1),
            entity_id: AtomId::entity(1),
            label: "in crisis".into(),
            state_type: StateType::Psychological,
            evidence: vec![ChunkRef::new("sec_0001", None)],
            section_range: SectionRange::point("sec_0001"),
            confidence: None,
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        let text = "By chapter three he was in crisis again.";
        let spans = detect_atom_spans(text, Some("sec_0001"), &[state]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].atom_type, "state");
        assert_eq!(spans[0].surface_form, "in crisis");
    }

    #[test]
    fn empty_text_yields_empty_spans() {
        let atoms = vec![entity_atom(1, "Alyosha", &[], "sec_0001")];
        assert!(detect_atom_spans("", Some("sec_0001"), &atoms).is_empty());
    }
}
