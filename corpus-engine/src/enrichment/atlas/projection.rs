// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared atom projection — the hot-field record the v2 store derives from.
//!
//! `atoms.json` is the canonical source; the query-time consumers don't read it
//! directly. They read a **flat projection** of each atom: the scalar/text
//! fields the navigate + typed-enumeration paths touch, structured (so a typed
//! enumeration filters on `kind` without parsing the payload), plus the full
//! `AtomEnvelope` kept as a JSON payload blob for the rare deep read.
//!
//! This module is the single source of that projection. The v2 store writer
//! (`super::store`) projects atoms through [`project`] into the columnar
//! `atoms.lance`, and the reader ([`super::store::LancePreload`]) re-projects the
//! lossless `payload` back into [`AtomRecord`]s — so the columns and the resident
//! records derive from the *same* projection by construction. (Formerly this also
//! backed the v1 `atoms.rkyv` archive; that backend was retired in
//! ATLAS_STORAGE_V2, leaving the projection types here, rkyv-free.)
//!
//! This module used to declare its own `AtomKindTag`, `ArchEdgeType` and
//! `ArchChunkRef` — hand-synced mirrors of [`super::atoms::AtomType`],
//! [`super::edges::EdgeType`] and [`super::atoms::ChunkRef`], two of which
//! crossed into sovereign as a SECOND published name for a concept
//! corpus-engine already published. All three were deleted 2026-08-20: the
//! projection carries the canonical types and `super::store` maps them to
//! their on-disk bytes directly. The byte values are unchanged, so
//! `atoms.lance` and `edges.csr` written before the change read back
//! identically (ARCH §10.6 — one decider, one name).

use super::atoms::{AtomEnvelope, AtomType, ChunkRef};

/// One atom: structured hot fields + the full-fidelity JSON payload.
#[derive(Clone, Debug, PartialEq)]
pub struct AtomRecord {
    pub id: String,
    pub kind: AtomType,
    /// `Entity.canonical_name` (else `""`).
    pub name: String,
    /// `Relation.label` (else `""`).
    pub label: String,
    /// `Claim.content` (else `""`).
    pub content: String,
    /// `Entity.entity_type` string repr (else `""`).
    pub subtype: String,
    /// `Entity.description` (else `""`). Feeds the atom-enumeration
    /// `embed_text` (`"{name}. {description}"`) and the Entity branch of the
    /// seed embed-text rendering — both are hot paths, so the field is projected
    /// rather than payload-parsed.
    pub description: String,
    /// `Claim.quotable_excerpt` (else `""`).
    pub excerpt: String,
    /// `Claim.confidence` (0.5 default; 0.0 for non-claims).
    pub confidence: f32,
    /// `Entity.salience` (0.0 for non-entities). Prominence tie-break in
    /// the atom-enumeration path.
    pub salience: f32,
    /// `Entity.aliases` (else empty). The enumeration uses the count as
    /// a prominence tie-break; the seed embed-text rendering joins them
    /// back into the Entity text.
    pub aliases: Vec<String>,
    /// `Relation.participants` atom ids (else empty).
    pub participants: Vec<String>,
    /// Normalised evidence refs, from the canonical `AtomEnvelope::evidence`
    /// accessor. Carries `ChunkRef` itself: this field held a third mirror type
    /// (`ArchChunkRef`, the same three fields with the `Option`s collapsed to
    /// `""`) until 2026-08-20. The collapse is now the reader's business, so a
    /// caller that needs to tell "no preview" from "empty preview" still can.
    pub evidence: Vec<ChunkRef>,
    /// The full `AtomEnvelope` as canonical JSON — re-parsed only on the rare
    /// deep/point read, never in the bulk enumeration path.
    pub payload: Vec<u8>,
}

/// The atom's subtype — the author's noun where there is one.
///
/// The ONE answer to "what type is this atom, within its kind" (§10.6): the
/// store column, the resident record and the ontology-coverage rollup all read
/// it here.
///
/// Absence is `""`, and absence has two spellings on disk. `resolution.rs`
/// tags an unclassified relation `Other("unclassified")` and an unclassified
/// event `Other("unspecified")` — those are the ABSENCE of a subtype, not
/// subtypes with those names, and a reader that took them literally would
/// report a corpus as fully typed when nothing in it was typed at all.
pub fn subtype_of(atom: &AtomEnvelope) -> String {
    /// The two placeholders `resolution.rs` writes when nothing classified the
    /// atom. (The literals are spelled several times across the crate and were
    /// before this function; folding every site into these is its own change.)
    const ABSENT: [&str; 2] = ["unclassified", "unspecified"];
    let named = |s: &str| {
        if ABSENT.contains(&s) {
            String::new()
        } else {
            s.to_string()
        }
    };
    match atom {
        AtomEnvelope::Entity(e) => e.entity_type.as_str_repr().to_string(),
        AtomEnvelope::Relation(r) => named(r.relation_type.as_str_repr()),
        AtomEnvelope::Event(e) => named(e.event_type.as_str_repr()),
        AtomEnvelope::State(s) => named(s.state_type.as_str_repr()),
        // `claim_kind` is the Claim's subtype under both vocabularies — the
        // typed-extension qualifiers (`evidence`, `concession`, …) and a
        // declared claim type (ontology v1).
        AtomEnvelope::Claim(c) => c.claim_kind.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

/// Project an atom to its hot-field record. Shared by the v2 store writer
/// (`super::store::write_store`) and the reader (`LancePreload`), so the
/// columnar `atoms.lance` and the resident records derive from the *same*
/// projection rather than two functions kept in sync.
pub fn project(atom: &AtomEnvelope) -> AtomRecord {
    let id = atom.id().as_str().to_string();
    let kind = atom.atom_type();
    let mut name = String::new();
    let mut label = String::new();
    let mut content = String::new();
    let mut subtype = String::new();
    let mut description = String::new();
    let mut excerpt = String::new();
    let mut confidence = 0.0_f32;
    let mut salience = 0.0_f32;
    let mut aliases = Vec::new();
    let mut participants = Vec::new();
    match atom {
        AtomEnvelope::Entity(e) => {
            name = e.canonical_name.clone();
            subtype = subtype_of(atom);
            description = e.description.clone();
            salience = e.salience;
            aliases = e.aliases.clone();
        }
        AtomEnvelope::Relation(r) => {
            label = r.label.clone();
            participants = r
                .participants
                .iter()
                .map(|p| p.as_str().to_string())
                .collect();
            subtype = subtype_of(atom);
        }
        AtomEnvelope::Claim(c) => {
            content = c.content.clone();
            excerpt = c.quotable_excerpt.clone().unwrap_or_default();
            confidence = c.confidence.unwrap_or(0.5);
            subtype = subtype_of(atom);
        }
        // Event and State DO carry a subtype since P3 (a declared event type,
        // a `role_of` role) and [`subtype_of`] returns it — but putting it in
        // the store's column would change what an existing corpus reads back
        // on its next rebuild, and the readers of that column are P5/P6's to
        // move. The rule is one function; which kinds enter the column is this
        // caller's decision.
        _ => {}
    }
    // `AtomEnvelope::evidence` is the canonical per-variant evidence accessor
    // and says so in its own doc comment; this module used to carry a private
    // byte-identical copy (`evidence_refs`) that nothing kept in sync. Deleted
    // 2026-08-20 — one decider, one name (ARCH §10.6).
    let evidence = atom.evidence().into_iter().cloned().collect();
    let payload = serde_json::to_vec(atom).unwrap_or_default();
    AtomRecord {
        id,
        kind,
        name,
        label,
        content,
        subtype,
        description,
        excerpt,
        confidence,
        salience,
        aliases,
        participants,
        evidence,
        payload,
    }
}
