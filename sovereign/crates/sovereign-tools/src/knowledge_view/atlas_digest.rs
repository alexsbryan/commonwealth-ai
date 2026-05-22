//! Atlas-derived landscape digest for the `conversation-history`
//! view.
//!
//! The other two KnowledgeView views (`personal-knowledge`,
//! `institutional-notes`) still run v1 `field_model` enrichment and
//! consume `field_skeleton.json` via [`super::digest::format_landscape`].
//! Conversation-history migrated to the v2 `conversation_atlas`
//! pipeline (see `corpus-engine::enrichment::pipeline::pipelines::conversation_atlas`)
//! which writes `atlas/atoms.json` rather than `field_skeleton.json`.
//!
//! This module is the splice path's reader for that atlas. It loads
//! `atoms.json`, ranks Entity / Claim / Question atoms by salience,
//! and emits a markdown block in the same heading shape v1 produced
//! so the rest of the splice pipeline (`cross_view::format_digest`,
//! token-budget enforcement, `LandscapeDigestProvider::splice_into`)
//! is unchanged.
//!
//! Pure function — no I/O outside `std::fs::read`, no tokio, no
//! clock. Easy to unit-test against a fixture atoms.json.

use std::path::Path;

use corpus_engine::enrichment::atlas::{AtomEnvelope, AtomsFile};

use super::tokens::estimate_tokens;

/// Bullets per section. Mirrors the v1 cap at
/// [`super::digest::format_landscape`] so the digest looks the same
/// regardless of which pipeline produced it.
const SECTION_BULLET_CAP: usize = 5;

/// Render an atlas-derived landscape digest for the
/// `conversation-history` view, formatted to match the v1 markdown
/// shape so downstream splice code is unchanged.
///
/// Output shape (max-budget):
///
/// ```text
/// Conversation history:
///
///   People & topics:
///     — Alice Chen
///     — Q3 runway strategy
///
///   Recurring threads:
///     — Series B fundraising plan settled in late September
///
///   Open questions:
///     — Should we extend the runway before Series B closes?
/// ```
///
/// Returns an empty string when the atlas exists but has no atoms in
/// any of the three buckets (still-enriching corpus). Returns a
/// "not yet enriched" sentinel when the atoms.json file is missing —
/// the caller's existing `Ok(format!("{title}: not yet enriched."))`
/// fallback is preserved by the manager's branch rather than here, so
/// this function only handles the populated path.
pub(crate) fn render_atlas_digest(atlas_dir: &Path, budget_tokens: usize) -> String {
    let atoms_path = atlas_dir.join("atoms.json");
    let Ok(raw) = std::fs::read(&atoms_path) else {
        tracing::debug!(
            atlas_dir = %atlas_dir.display(),
            "atlas_digest: atoms.json absent — caller should fall back"
        );
        return String::new();
    };
    let file: AtomsFile = match serde_json::from_slice(&raw) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                atlas_dir = %atlas_dir.display(),
                error = %e,
                "atlas_digest: atoms.json failed to parse — returning empty digest"
            );
            return String::new();
        }
    };

    let mut entities: Vec<(&str, f32)> = Vec::new();
    let mut claims: Vec<(&str, f32)> = Vec::new();
    let mut questions: Vec<(&str, f32)> = Vec::new();
    for env in &file.atoms {
        match env {
            AtomEnvelope::Entity(e) => entities.push((&e.canonical_name, e.salience)),
            AtomEnvelope::Claim(c) => {
                // Claim atoms don't carry their own salience field, so
                // surface the most epistemically-confident ones first
                // and fall back to extraction confidence when present.
                let s = c.confidence.unwrap_or(0.5);
                claims.push((&c.content, s));
            }
            AtomEnvelope::Question(q) => {
                // Surface every Question; rank by stable ordering of
                // the source text so the digest is deterministic.
                questions.push((&q.content, 1.0));
            }
            _ => {}
        }
    }
    sort_by_salience_desc(&mut entities);
    sort_by_salience_desc(&mut claims);
    sort_by_salience_desc(&mut questions);

    let mut out = String::new();
    out.push_str("Conversation history:\n\n");

    push_section(&mut out, "People & topics", &entities, budget_tokens);
    push_section(&mut out, "Recurring threads", &claims, budget_tokens);
    push_section(&mut out, "Open questions", &questions, budget_tokens);

    // Hard budget guard: if a single long bullet squeaked past the
    // per-line check, drop trailing lines until we fit. Same
    // conservative posture as `format_landscape`.
    while estimate_tokens(&out) > budget_tokens {
        match out.rfind('\n') {
            Some(idx) if idx > 0 => out.truncate(idx),
            _ => {
                out.clear();
                break;
            }
        }
    }

    tracing::debug!(
        atlas_dir = %atlas_dir.display(),
        budget_tokens,
        output_tokens = estimate_tokens(&out),
        input_atoms = file.atoms.len(),
        input_entities = entities.len(),
        input_claims = claims.len(),
        input_questions = questions.len(),
        "atlas_digest: rendered"
    );

    if out.trim() == "Conversation history:" {
        // Atlas exists but every section is empty. Treat the same as
        // missing — caller falls back to the "not yet enriched" path.
        return String::new();
    }
    out
}

fn sort_by_salience_desc(items: &mut [(&str, f32)]) {
    items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
}

fn push_section(out: &mut String, heading: &str, items: &[(&str, f32)], budget_tokens: usize) {
    if items.is_empty() {
        return;
    }
    let header = format!("  {heading}:\n");
    if estimate_tokens(out) + estimate_tokens(&header) > budget_tokens {
        return;
    }
    out.push_str(&header);
    for (text, _) in items.iter().take(SECTION_BULLET_CAP) {
        let line = format!("    — {}\n", text.trim());
        if estimate_tokens(out) + estimate_tokens(&line) > budget_tokens {
            break;
        }
        out.push_str(&line);
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a fixture atoms.json from a JSON literal — keeps the
    /// test independent of the (largely module-private) atom-variant
    /// enums and pins the on-disk shape the reader actually consumes.
    fn write_atoms_json(dir: &Path, json: &str) {
        std::fs::write(dir.join("atoms.json"), json).unwrap();
    }

    fn entity_json(id: u32, name: &str, salience: f32) -> String {
        format!(
            r#"{{
              "id": "entity-{id:04}",
              "atom_type": "Entity",
              "enrichment_depth": "extracted",
              "data": {{
                "id": "entity-{id:04}",
                "canonical_name": "{name}",
                "entity_type": "person",
                "first_appearance": {{ "chunk_id": "chunk_0001" }},
                "description": "",
                "salience": {salience},
                "enrichment_depth": "extracted"
              }}
            }}"#
        )
    }

    fn claim_json(id: u32, content: &str, confidence: f32) -> String {
        format!(
            r#"{{
              "id": "claim-{id:04}",
              "atom_type": "Claim",
              "enrichment_depth": "extracted",
              "data": {{
                "id": "claim-{id:04}",
                "content": "{content}",
                "discourse_act": "assert",
                "epistemic_status": "held",
                "scope": "global",
                "confidence": {confidence},
                "enrichment_depth": "extracted"
              }}
            }}"#
        )
    }

    fn question_json(id: u32, content: &str) -> String {
        format!(
            r#"{{
              "id": "question-{id:04}",
              "atom_type": "Question",
              "enrichment_depth": "extracted",
              "data": {{
                "id": "question-{id:04}",
                "content": "{content}",
                "question_type": "open",
                "resolution_status": {{ "kind": "open" }},
                "enrichment_depth": "extracted"
              }}
            }}"#
        )
    }

    fn wrap(atoms: &[String]) -> String {
        format!(
            r#"{{
              "schema_version": "2.0",
              "atoms": [{}]
            }}"#,
            atoms.join(",\n")
        )
    }

    #[test]
    fn missing_atoms_json_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let body = render_atlas_digest(tmp.path(), 200);
        assert!(body.is_empty(), "missing atoms.json must return empty");
    }

    #[test]
    fn empty_atoms_returns_empty() {
        let tmp = TempDir::new().unwrap();
        write_atoms_json(tmp.path(), &wrap(&[]));
        let body = render_atlas_digest(tmp.path(), 200);
        assert!(body.is_empty(), "no atoms must return empty digest");
    }

    #[test]
    fn entities_render_ranked_by_salience() {
        let tmp = TempDir::new().unwrap();
        write_atoms_json(
            tmp.path(),
            &wrap(&[
                entity_json(1, "Low salience person", 0.1),
                entity_json(2, "Top salience person", 0.99),
                entity_json(3, "Mid salience person", 0.5),
            ]),
        );
        let body = render_atlas_digest(tmp.path(), 1000);
        assert!(body.contains("Conversation history:"), "shape wrong: {body}");
        assert!(body.contains("People & topics:"), "section missing: {body}");
        let top_idx = body.find("Top salience person").unwrap();
        let low_idx = body.find("Low salience person").unwrap();
        assert!(top_idx < low_idx, "salience ordering wrong: {body}");
    }

    #[test]
    fn budget_cap_truncates_output() {
        let tmp = TempDir::new().unwrap();
        write_atoms_json(
            tmp.path(),
            &wrap(&[
                entity_json(1, "A", 0.9),
                entity_json(2, "B", 0.8),
                claim_json(1, "Some recurring thread", 0.7),
                question_json(1, "Some open question"),
            ]),
        );
        // Very tight budget — heading + maybe one line.
        let body = render_atlas_digest(tmp.path(), 5);
        assert!(
            estimate_tokens(&body) <= 5,
            "digest must not overshoot budget; got {} tokens: {body}",
            estimate_tokens(&body)
        );
    }

    #[test]
    fn three_section_shape_renders_when_all_atom_types_present() {
        let tmp = TempDir::new().unwrap();
        write_atoms_json(
            tmp.path(),
            &wrap(&[
                entity_json(1, "Alice", 0.9),
                claim_json(1, "Migration shipped on time", 0.6),
                question_json(1, "Should we extend runway?"),
            ]),
        );
        let body = render_atlas_digest(tmp.path(), 1000);
        assert!(body.contains("People & topics:"));
        assert!(body.contains("Recurring threads:"));
        assert!(body.contains("Open questions:"));
    }
}
