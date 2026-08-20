// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure formatters used by the glass-box reading surface.
//!
//! Why this lives outside `reading_http`: every function here is a
//! per-variant match over `AtomEnvelope` (eight arms) or `EdgeType`
//! (ten arms) with no I/O, no async, and no daemon state. Pulling
//! the formatters out drops `reading_http.rs` under ARCH §3.1's
//! "justify yourself" threshold and gives the type-label / surface
//! / evidence helpers a stable seam to grow against.

use corpus_engine::enrichment::atlas::{AtomEnvelope, EdgeType};

/// Pull the human-readable fields for any atom type. Not every
/// type has every field — for atoms without a clean canonical name
/// we synthesize from the most descriptive available text so the
/// panel still shows something sensible.
pub(crate) fn atom_surface_fields(
    atom: &AtomEnvelope,
) -> (String, Vec<String>, String, Option<f32>) {
    match atom {
        AtomEnvelope::Entity(e) => (
            e.canonical_name.clone(),
            e.aliases.clone(),
            e.description.clone(),
            Some(e.salience),
        ),
        AtomEnvelope::Event(e) => (
            truncate(&e.description, 80),
            Vec::new(),
            e.description.clone(),
            None,
        ),
        AtomEnvelope::State(s) => (
            s.label.clone(),
            Vec::new(),
            format!("State of {}: {}", s.entity_id.as_str(), s.label),
            s.confidence,
        ),
        AtomEnvelope::Relation(r) => (r.label.clone(), Vec::new(), r.label.clone(), None),
        AtomEnvelope::Claim(c) => (
            truncate(&c.content, 80),
            Vec::new(),
            c.content.clone(),
            c.confidence,
        ),
        AtomEnvelope::Question(q) => (
            truncate(&q.content, 80),
            Vec::new(),
            q.content.clone(),
            None,
        ),
        AtomEnvelope::Configuration(c) => (
            c.label.clone(),
            Vec::new(),
            c.description.clone(),
            Some(c.confidence),
        ),
        AtomEnvelope::ArgumentReconstruction(a) => (
            a.name.clone(),
            Vec::new(),
            format!(
                "{}{}{}",
                a.premises
                    .iter()
                    .enumerate()
                    .map(|(i, p)| format!("P{}. {}", i + 1, p))
                    .collect::<Vec<_>>()
                    .join(" "),
                if !a.premises.is_empty() { " " } else { "" },
                if !a.conclusion.is_empty() {
                    format!("C. {}", a.conclusion)
                } else {
                    String::new()
                }
            ),
            None,
        ),
        AtomEnvelope::Position(p) => (
            p.canonical_name.clone(),
            Vec::new(),
            p.content.clone(),
            Some(p.salience),
        ),
        AtomEnvelope::Opposition(o) => (
            o.canonical_label.clone(),
            Vec::new(),
            if o.framing.is_empty() {
                format!("{} vs {}", o.left_label, o.right_label)
            } else {
                o.framing.clone()
            },
            Some(o.salience),
        ),
        AtomEnvelope::Asset(a) => {
            let name = if a.original_filename.is_empty() {
                format!("{} ({})", a.asset_kind, &a.sha256[..12.min(a.sha256.len())])
            } else {
                a.original_filename.clone()
            };
            let detail = format!(
                "{} asset, {} bytes, sha256:{}",
                a.asset_kind,
                a.size,
                &a.sha256[..16.min(a.sha256.len())]
            );
            (name, Vec::new(), detail, None)
        }
    }
}

pub(crate) fn truncate(s: &str, max_chars: usize) -> String {
    let trimmed: String = s.chars().take(max_chars).collect();
    if trimmed.chars().count() < s.chars().count() {
        format!("{trimmed}…")
    } else {
        trimmed
    }
}

/// Extract every `(section_id, optional_preview)` pair from an
/// atom's evidence (or `first_appearance` for entities, or
/// `section_position` for events). Order preserves the order
/// evidence was written.
pub(crate) fn atom_evidence_section_refs(atom: &AtomEnvelope) -> Vec<(String, Option<String>)> {
    match atom {
        AtomEnvelope::Entity(e) => vec![(
            e.first_appearance.chunk_id.clone(),
            e.first_appearance.passage_preview.clone(),
        )],
        AtomEnvelope::Event(e) => {
            let mut out = vec![(e.section_position.section_id.clone(), None)];
            for c in &e.evidence {
                out.push((c.chunk_id.clone(), c.passage_preview.clone()));
            }
            out
        }
        AtomEnvelope::State(s) => s
            .evidence
            .iter()
            .map(|c| (c.chunk_id.clone(), c.passage_preview.clone()))
            .collect(),
        AtomEnvelope::Relation(r) => r
            .evidence
            .iter()
            .map(|c| (c.chunk_id.clone(), c.passage_preview.clone()))
            .collect(),
        AtomEnvelope::Claim(c) => c
            .evidence
            .iter()
            .map(|cr| (cr.chunk_id.clone(), cr.passage_preview.clone()))
            .collect(),
        AtomEnvelope::Question(q) => q
            .raised_at
            .iter()
            .map(|c| (c.chunk_id.clone(), c.passage_preview.clone()))
            .collect(),
        AtomEnvelope::Configuration(c) => c
            .evidence
            .iter()
            .map(|cr| (cr.chunk_id.clone(), cr.passage_preview.clone()))
            .collect(),
        AtomEnvelope::ArgumentReconstruction(a) => {
            let mut out = vec![(a.section_position.section_id.clone(), None)];
            for c in &a.evidence {
                out.push((c.chunk_id.clone(), c.passage_preview.clone()));
            }
            out
        }
        AtomEnvelope::Position(p) => vec![(
            p.first_appearance.chunk_id.clone(),
            p.first_appearance.passage_preview.clone(),
        )],
        AtomEnvelope::Opposition(o) => vec![(
            o.first_appearance.chunk_id.clone(),
            o.first_appearance.passage_preview.clone(),
        )],
        // Asset atoms don't have section-level evidence; reachable
        // via the carrier doc's Attaches edge.
        AtomEnvelope::Asset(_) => Vec::new(),
    }
}
