//! Brief assembler — render a [`TraversalResult`] into prose.
//!
//! The assembler is the only place in the atlas stack that makes
//! presentation decisions. It takes a [`TraversalResult`] and
//! emits a string a user (or an LLM consumer) can read cold. Two
//! invariants:
//!
//! 1. **Depth calibration.** Every atom carries an
//!    `enrichment_depth` tag. Today's pipelines produce
//!    `Extracted` atoms exclusively, so the assembler frames
//!    everything as "the atlas records…", "attributed to…",
//!    "on the evidence of sec_NNNN…". When a future structure-
//!    first pipeline ships `Structural` atoms, the same
//!    assembler uses plain assertive phrasing ("Alyosha is…").
//!    A mixed brief (cross-corpus) keeps each atom's framing
//!    independent.
//! 2. **Confidence markers.** Atoms with `confidence < 0.7`
//!    get a hedge — "tentatively" or "with confidence N.N".
//!    Above the threshold, no extra marker: high-confidence
//!    findings present cleanly.
//!
//! The brief has a stable structure: one headline + one short
//! body. The body mixes bullets and prose based on what's in the
//! traversal. Keep it under ~60 lines for readability.

use crate::enrichment::atlas::edges::{Edge, EdgeType};
use crate::enrichment::pipeline::atlas::EnrichmentDepth;

use super::engine::TraversalResult;

/// The confidence floor above which the assembler drops the
/// hedging marker. Matches spec §7.3 — confidence markers surface
/// *only when below* the threshold so clean findings stay terse.
const CONFIDENCE_HEDGE_THRESHOLD: f32 = 0.7;

/// A rendered brief. Stringly-typed today; a future richer shape
/// (per-atom citations, inline evidence links) lives behind the
/// same API so callers don't have to change.
#[derive(Debug, Clone)]
pub struct Brief {
    pub headline: String,
    pub body: String,
}

impl Brief {
    /// Full brief as one string — headline + blank line + body.
    /// Most callers want this form; holding `headline` + `body`
    /// separately lets a caller inject its own frontmatter.
    pub fn to_text(&self) -> String {
        if self.body.trim().is_empty() {
            self.headline.clone()
        } else {
            format!("{}\n\n{}", self.headline, self.body)
        }
    }
}

/// Render a traversal result into a Brief. Misses get a short
/// one-line brief that quotes the engine's miss headline
/// verbatim — the engine already shaped it for readability.
pub fn assemble_brief(result: &TraversalResult) -> Brief {
    if !result.hit {
        return Brief {
            headline: result.headline.clone(),
            body: String::new(),
        };
    }

    match result.kind.as_str() {
        "entity_lookup" => assemble_entity_lookup(result),
        "trajectory" => assemble_trajectory(result),
        "relation_lookup" => assemble_relation_lookup(result),
        "tension_list" => assemble_tension_list(result),
        "configuration_list" => assemble_configuration_list(result),
        "corpus_overview" => assemble_corpus_overview(result),
        other => Brief {
            headline: format!("Traversal kind '{other}' has no brief assembler yet."),
            body: String::new(),
        },
    }
}

fn assemble_entity_lookup(result: &TraversalResult) -> Brief {
    let Some(entity) = result.entities.first() else {
        return Brief {
            headline: result.headline.clone(),
            body: String::new(),
        };
    };
    let frame = depth_frame(entity.enrichment_depth);
    let mut body = String::new();
    let kind_clause = match entity.concept_kind.as_deref() {
        Some("mechanism") => " (named mechanism in the section's argument)",
        Some("definition") => " (definition card)",
        Some("image") => " (lyric image)",
        Some("motif") => " (recurring motif)",
        Some("formal_device") => " (formal device)",
        _ => "",
    };
    body.push_str(&format!(
        "{} {}{} first appears in {}. {}",
        frame.records,
        bold(&entity.canonical_name),
        kind_clause,
        entity.first_appearance.chunk_id,
        hedge_confidence(&entity.description, Some(1.0)),
    ));
    if !entity.aliases.is_empty() {
        body.push_str(&format!(
            "\n\nAlso referenced as: {}.",
            entity.aliases.join(", ")
        ));
    }

    if !result.relations.is_empty() {
        body.push_str(&format!(
            "\n\n**Relations ({}):**\n",
            result.relations.len()
        ));
        let id_to_name = entity_name_map(&result.entities);
        for r in &result.relations {
            let participants = r
                .participants
                .iter()
                .filter(|p| **p != entity.id)
                .map(|p| {
                    id_to_name
                        .get(p.as_str())
                        .cloned()
                        .unwrap_or_else(|| p.as_str().to_string())
                })
                .collect::<Vec<_>>()
                .join(" × ");
            body.push_str(&format!(
                "- {} — {} (with {})\n",
                depth_tag(r.enrichment_depth),
                r.label,
                participants
            ));
        }
    }

    if !result.claims.is_empty() {
        body.push_str(&format!(
            "\n**Claims attributed to {} ({}):**\n",
            entity.canonical_name,
            result.claims.len()
        ));
        for c in &result.claims {
            let hedge = hedge_confidence("", c.confidence);
            // Gap-B claim_kind qualifier: render evidence claims as
            // "the section invokes X as <kind>", concessions as
            // "the section concedes X (outcome=Y)". Base claims
            // keep the existing clean rendering.
            let kind_prefix = match c.claim_kind.as_deref() {
                Some("evidence") => {
                    let kind = c.evidence_kind.as_deref().unwrap_or("evidence");
                    format!("[evidence:{}] ", kind)
                }
                Some("concession") => {
                    let outcome = c.concession_outcome.as_deref().unwrap_or("intact");
                    format!("[concession:{}] ", outcome)
                }
                _ => String::new(),
            };
            body.push_str(&format!(
                "- {} {}{}{}\n",
                depth_tag(c.enrichment_depth),
                kind_prefix,
                c.content,
                hedge,
            ));
        }
    }

    if !result.states.is_empty() {
        body.push_str(&format!(
            "\n**Trajectory ({} state(s)):**\n",
            result.states.len()
        ));
        for s in &result.states {
            body.push_str(&format!(
                "- `{}` {} — {}\n",
                s.section_range.start,
                depth_tag(s.enrichment_depth),
                s.label
            ));
        }
    }

    // Gap-B typed atoms: named positions this entity is the
    // proponent of, plus oppositions where this entity is one
    // side. Renders below the trajectory block so the reader sees
    // factual atoms first, argumentative scaffolding second.
    if !result.positions.is_empty() {
        body.push_str(&format!(
            "\n**Positions defended by {} ({}):**\n",
            entity.canonical_name,
            result.positions.len()
        ));
        for p in &result.positions {
            let stance_phrase = match p.stance.as_str() {
                "endorse" => "endorses",
                "rebut" => "rebuts",
                "mixed" => "takes a mixed stance on",
                _ => "surveys",
            };
            body.push_str(&format!(
                "- {} *{}* {}: {}\n",
                depth_tag(p.enrichment_depth),
                p.canonical_name,
                stance_phrase,
                p.content,
            ));
        }
    }

    if !result.oppositions.is_empty() {
        body.push_str(&format!(
            "\n**Oppositions touching {} ({}):**\n",
            entity.canonical_name,
            result.oppositions.len()
        ));
        for o in &result.oppositions {
            let axis_clause = if o.axis.is_empty() {
                String::new()
            } else {
                format!(" (axis: {})", o.axis)
            };
            body.push_str(&format!(
                "- {} **{}** vs **{}**{}\n",
                depth_tag(o.enrichment_depth),
                o.left_label,
                o.right_label,
                axis_clause,
            ));
        }
    }

    Brief {
        headline: result.headline.clone(),
        body,
    }
}

fn assemble_trajectory(result: &TraversalResult) -> Brief {
    let Some(entity) = result.entities.first() else {
        return Brief {
            headline: result.headline.clone(),
            body: String::new(),
        };
    };
    let frame = depth_frame(entity.enrichment_depth);
    let mut body = format!(
        "{} a trajectory of {} state(s) for {}.\n\n",
        frame.records,
        result.states.len(),
        bold(&entity.canonical_name),
    );

    for (i, s) in result.states.iter().enumerate() {
        let hedge = hedge_confidence("", s.confidence);
        body.push_str(&format!(
            "{}. `{}` — {}{}\n",
            i + 1,
            s.section_range.start,
            s.label,
            hedge
        ));
    }

    if !result.edges.is_empty() {
        body.push_str(&format!(
            "\n**Transitions ({}):**\n",
            result
                .edges
                .iter()
                .filter(|e| e.edge_type == EdgeType::Transition)
                .count()
        ));
        for e in result
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Transition)
        {
            let trigger = e
                .trigger_event
                .as_ref()
                .and_then(|tid| result.events.iter().find(|ev| &ev.id == tid))
                .map(|ev| format!(" (triggered by: {})", ev.description))
                .unwrap_or_else(|| " (no explicit trigger)".to_string());
            body.push_str(&format!("- {} → {}{}\n", e.source.as_str(), e.target.as_str(), trigger));
        }
    }

    Brief {
        headline: result.headline.clone(),
        body,
    }
}

fn assemble_relation_lookup(result: &TraversalResult) -> Brief {
    let mut body = String::new();
    for r in &result.relations {
        let frame = depth_frame(r.enrichment_depth);
        body.push_str(&format!(
            "{} {} (id `{}`).\n",
            frame.records,
            r.label,
            r.id.as_str()
        ));
    }
    if !result.states.is_empty() {
        body.push_str("\n**Relation states:**\n");
        for s in &result.states {
            body.push_str(&format!(
                "- `{}` {}\n",
                s.section_range.start, s.label
            ));
        }
    }
    Brief {
        headline: result.headline.clone(),
        body,
    }
}

fn assemble_tension_list(result: &TraversalResult) -> Brief {
    let mut body = String::new();
    if !result.questions.is_empty() {
        body.push_str("**Open questions the corpus raises:**\n");
        for q in &result.questions {
            body.push_str(&format!(
                "- {} {}\n",
                depth_tag(q.enrichment_depth),
                q.content
            ));
        }
    }
    let tensions: Vec<&Edge> = result
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Tension)
        .collect();
    if !tensions.is_empty() {
        body.push_str(&format!("\n**Tension edges ({}):**\n", tensions.len()));
        for t in tensions {
            let sub = t
                .sub_question
                .as_deref()
                .unwrap_or("(no sub-question recorded)");
            body.push_str(&format!(
                "- {} ↔ {} — on: {}\n",
                t.source.as_str(),
                t.target.as_str(),
                sub
            ));
        }
    }
    if body.is_empty() {
        body.push_str("(No tensions or open questions on this atlas.)");
    }
    Brief {
        headline: result.headline.clone(),
        body,
    }
}

fn assemble_configuration_list(result: &TraversalResult) -> Brief {
    let mut body = String::new();
    for c in &result.configurations {
        body.push_str(&format!(
            "**{}** {}\n",
            c.label,
            depth_tag(c.enrichment_depth)
        ));
        body.push_str(&format!("{}\n", c.description));
        // Configuration.confidence is still `f32` (LLM-reported by
        // design) so wrap it to match `hedge_confidence`'s
        // Option-aware signature.
        let hedge = hedge_confidence("", Some(c.confidence));
        body.push_str(&format!(
            "_Alternative reading:_ {}{}\n\n",
            c.interpretive_note, hedge
        ));
    }
    Brief {
        headline: result.headline.clone(),
        body,
    }
}

fn assemble_corpus_overview(result: &TraversalResult) -> Brief {
    let mut body = String::new();
    if !result.entities.is_empty() {
        body.push_str("**Top entities (by salience):**\n");
        for e in &result.entities {
            body.push_str(&format!(
                "- {} {} (salience {:.2})\n",
                depth_tag(e.enrichment_depth),
                e.canonical_name,
                e.salience
            ));
        }
    }
    if !result.relations.is_empty() {
        body.push_str(&format!(
            "\n**Key relations ({}):**\n",
            result.relations.len()
        ));
        for r in &result.relations {
            body.push_str(&format!("- {}\n", r.label));
        }
    }
    if !result.configurations.is_empty() {
        body.push_str(&format!(
            "\n**Configurations ({}):**\n",
            result.configurations.len()
        ));
        for c in &result.configurations {
            body.push_str(&format!("- {}\n", c.label));
        }
    }
    Brief {
        headline: result.headline.clone(),
        body,
    }
}

// ── Depth + confidence framing helpers ─────────────────────

struct DepthFrame {
    /// The verb/phrase to use when asserting something. Today
    /// this is always "The atlas records" for `Extracted`; the
    /// stub for `Structural` / `StructuralClassified` is ready
    /// for when a structure-first pipeline lands.
    records: &'static str,
}

fn depth_frame(depth: EnrichmentDepth) -> DepthFrame {
    match depth {
        EnrichmentDepth::Extracted => DepthFrame {
            records: "The atlas records that",
        },
        EnrichmentDepth::Structural => DepthFrame {
            // Structural atoms come from deterministic parsing —
            // assert directly. Reserved for the future structure-
            // first strategy.
            records: "The work records that",
        },
        EnrichmentDepth::StructuralClassified => DepthFrame {
            records: "The work records (with LLM classification) that",
        },
    }
}

/// One-word tag for inline use — e.g. `[extracted]`. Keeps the
/// brief terse when every atom would otherwise carry a long
/// "The atlas records…" preamble.
fn depth_tag(depth: EnrichmentDepth) -> &'static str {
    match depth {
        EnrichmentDepth::Extracted => "[extracted]",
        EnrichmentDepth::Structural => "[structural]",
        EnrichmentDepth::StructuralClassified => "[classified]",
    }
}

fn hedge_confidence(_context: &str, confidence: Option<f32>) -> String {
    // `None` means "deterministic resolver output, no LLM score" —
    // there's nothing to hedge on, so omit the confidence qualifier.
    // A Some value below the threshold gets the hedge as before.
    match confidence {
        None => String::new(),
        Some(c) if c >= CONFIDENCE_HEDGE_THRESHOLD => String::new(),
        Some(c) => format!(" _(confidence {c:.2})_"),
    }
}

fn bold(s: &str) -> String {
    format!("**{s}**")
}

fn entity_name_map(
    entities: &[crate::enrichment::atlas::atoms::Entity],
) -> std::collections::HashMap<String, String> {
    entities
        .iter()
        .map(|e| (e.id.as_str().to_string(), e.canonical_name.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{
        AtomId, ChunkRef, Configuration, Entity, SectionRange, State,
    };
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType, StateType};

    fn extracted_entity(name: &str) -> Entity {
        Entity {
            id: AtomId::entity(1),
            canonical_name: name.into(),
            aliases: vec!["alias".into()],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "An important character.".into(),
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            defining_quote: None,
                    concept_kind: None,
}
}

    #[test]
    fn brief_calibrates_extracted_atoms_with_interpretive_framing() {
        let result = TraversalResult {
            hit: true,
            kind: "entity_lookup".into(),
            headline: "Entity: Alyosha".into(),
            entities: vec![extracted_entity("Alyosha")],
            ..Default::default()
        };
        let brief = assemble_brief(&result);
        assert_eq!(brief.headline, "Entity: Alyosha");
        // The Extracted frame uses interpretive phrasing.
        assert!(
            brief.body.contains("The atlas records"),
            "Extracted depth should produce interpretive framing; got: {}",
            brief.body
        );
        assert!(brief.body.contains("**Alyosha**"));
    }

    #[test]
    fn brief_hedges_low_confidence_claim() {
        let mut e = extracted_entity("Alyosha");
        e.salience = 1.0;
        let low_conf_claim = crate::enrichment::atlas::atoms::Claim {
            id: AtomId::claim(1),
            content: "Faith is a habit".into(),
            discourse_act: crate::enrichment::pipeline::atlas::DiscourseAct::Assert,
            epistemic_status: crate::enrichment::pipeline::atlas::EpistemicStatus::Tentative,
            scope: crate::enrichment::pipeline::atlas::ClaimScope::Universal,
            evidence: vec![],
            quotable_excerpt: None,
            attributed_to: Some(e.id.clone()),
            confidence: Some(0.4), // below the hedge threshold
            anchor: None,
            claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        let result = TraversalResult {
            hit: true,
            kind: "entity_lookup".into(),
            headline: "Entity: Alyosha".into(),
            entities: vec![e],
            claims: vec![low_conf_claim],
            ..Default::default()
        };
        let brief = assemble_brief(&result);
        assert!(
            brief.body.contains("confidence 0.40"),
            "low-confidence claims should carry a hedge marker; got: {}",
            brief.body
        );
    }

    #[test]
    fn brief_on_miss_shows_engine_headline_and_empty_body() {
        let result = TraversalResult {
            hit: false,
            kind: "entity_lookup".into(),
            headline: "No entity atom matches 'Grushenka' in this atlas.".into(),
            ..Default::default()
        };
        let brief = assemble_brief(&result);
        assert!(brief.body.is_empty());
        assert!(brief.headline.contains("Grushenka"));
    }

    #[test]
    fn trajectory_brief_orders_states_and_reports_transitions_without_trigger() {
        let states = vec![
            State {
                id: AtomId::from_raw("state-0001"),
                entity_id: AtomId::entity(1),
                label: "first state".into(),
                state_type: StateType::Other("x".into()),
                evidence: vec![],
                section_range: SectionRange::point("sec_0001"),
                confidence: Some(1.0),
                enrichment_depth: EnrichmentDepth::Extracted,
            },
            State {
                id: AtomId::from_raw("state-0002"),
                entity_id: AtomId::entity(1),
                label: "second state".into(),
                state_type: StateType::Other("x".into()),
                evidence: vec![],
                section_range: SectionRange::point("sec_0003"),
                confidence: Some(1.0),
                enrichment_depth: EnrichmentDepth::Extracted,
            },
        ];
        let result = TraversalResult {
            hit: true,
            kind: "trajectory".into(),
            headline: "Trajectory: Alyosha".into(),
            entities: vec![extracted_entity("Alyosha")],
            states,
            ..Default::default()
        };
        let brief = assemble_brief(&result);
        let text = brief.to_text();
        // Section markers appear in reading order.
        let i1 = text.find("sec_0001").unwrap();
        let i2 = text.find("sec_0003").unwrap();
        assert!(i1 < i2);
    }

    #[test]
    fn configuration_brief_emits_alternative_reading_line() {
        let cfg = Configuration {
            id: AtomId::from_raw("config-0001"),
            label: "Three Sons Archetype".into(),
            description: "Alyosha, Ivan, Dmitri as faith, reason, passion.".into(),
            constituent_atoms: vec![],
            evidence: vec![],
            confidence: 0.85,
            interpretive_note: "Alternative reading: facets of one divided soul.".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        let result = TraversalResult {
            hit: true,
            kind: "configuration_list".into(),
            headline: "1 configuration(s)".into(),
            configurations: vec![cfg],
            ..Default::default()
        };
        let brief = assemble_brief(&result);
        assert!(brief.body.contains("Alternative reading"));
        assert!(brief.body.contains("divided soul"));
    }
}

