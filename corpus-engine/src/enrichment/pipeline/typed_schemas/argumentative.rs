//! Argumentative-essay Phase 1 — system prompt, JSON schema, parser.
//!
//! Empirical driver from the obsidian-vault bench loop: long-form
//! mechanism-design essays (`Pharmacy Benefit`, `FIFA Financialized`)
//! produced 0 atoms under the literary schema. The literary schema's
//! 10-cap on `claims` and absence of first-class slots for
//! mechanisms/evidence/positions made it impossible for the model to
//! represent what the section was actually carrying. This module
//! gives those slots first-class status so the prompt can be
//! generous without competing with a single 10-cap.
//!
//! The output is parsed into an `ArgumentativeExtension`, which the
//! routed Phase 1 dispatcher attaches to `SectionExtraction.type_extension`.
//! Common atoms (Person/Place/Institution/Work/Concept, Events, base
//! Claims) keep flowing through the existing `SectionExtraction`
//! fields — argumentative essays still have characters, places, and
//! the occasional event.

use crate::enrichment::pipeline::atlas::{
    ArgumentativeExtension, ConcessionSketch, EvidenceInvocationSketch, MechanismSketch,
    OppositionSketch, PositionSketch, TypeExtension,
};
use crate::enrichment::pipeline::types::strip_reasoning_tags;
use crate::error::{Error, Result};
use serde::Deserialize;

/// System preamble for the argumentative-essay extension. The
/// common-atoms portion (Person/Place/Concept/etc.) is still produced
/// by the literary preamble in `obsidian_atlas`; this preamble
/// targets the FIVE typed-extension atom collections only.
pub const PHASE1_ARGUMENTATIVE_SYSTEM: &str =
    include_str!("argumentative_phase1_system.md");

/// JSON Schema for the argumentative typed-extension output. The
/// runner pairs this with the prompt above via `with_response_schema`
/// so grammar-constrained decode produces parseable JSON.
pub fn phase1_argumentative_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "positions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["name", "content"],
                    "properties": {
                        "name": { "type": "string", "minLength": 1 },
                        "content": { "type": "string", "minLength": 1 },
                        "proponent": { "type": "string" },
                        "stance": {
                            "type": "string",
                            "enum": ["endorse", "rebut", "survey", "mixed"]
                        },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "mechanisms": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["name", "description"],
                    "properties": {
                        "name": { "type": "string", "minLength": 1 },
                        "description": { "type": "string", "minLength": 1 },
                        "domain": { "type": "string" },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "evidence_invocations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["label", "content"],
                    "properties": {
                        "label": { "type": "string", "minLength": 1 },
                        "content": { "type": "string", "minLength": 1 },
                        "kind": {
                            "type": "string",
                            "enum": [
                                "study",
                                "figure",
                                "historical_example",
                                "case_study",
                                "personal_anecdote",
                                "quotation",
                                "other"
                            ]
                        },
                        "supports": { "type": "string" },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "oppositions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["left", "right"],
                    "properties": {
                        "left": { "type": "string", "minLength": 1 },
                        "right": { "type": "string", "minLength": 1 },
                        "axis": { "type": "string" },
                        "framing": { "type": "string" },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "concessions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["content"],
                    "properties": {
                        "content": { "type": "string", "minLength": 1 },
                        "addresses": { "type": "string" },
                        "outcome": {
                            "type": "string",
                            "enum": ["intact", "narrowed", "retracted"]
                        },
                        "anchor": { "type": "string" }
                    }
                }
            }
        }
    })
}

// ─── Raw deserialization (lenient) ─────────────────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawArgumentative {
    positions: Vec<RawPosition>,
    mechanisms: Vec<RawMechanism>,
    evidence_invocations: Vec<RawEvidence>,
    oppositions: Vec<RawOpposition>,
    concessions: Vec<RawConcession>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawPosition {
    name: String,
    content: String,
    proponent: String,
    stance: String,
    anchor: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawMechanism {
    name: String,
    description: String,
    domain: String,
    anchor: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawEvidence {
    label: String,
    content: String,
    kind: String,
    supports: String,
    anchor: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawOpposition {
    left: String,
    right: String,
    axis: String,
    framing: String,
    anchor: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawConcession {
    content: String,
    addresses: String,
    outcome: String,
    anchor: String,
}

fn trim_non_empty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Parse the model's response into an `ArgumentativeExtension`. The
/// caller (the routed Phase 1 dispatcher) is responsible for
/// attaching this onto `SectionExtraction.type_extension` as
/// `TypeExtension::Argumentative(...)`.
///
/// Strict on shape (required name/content/label must be non-empty)
/// but lenient on optional fields: missing `domain`, `stance`,
/// `outcome`, `kind` default to empty strings (the struct's own
/// `#[serde(default = …)]` then applies the type's canonical fallback
/// — see `default_position_stance`, `default_concession_outcome`,
/// `default_evidence_kind`).
pub fn parse_phase1_argumentative(response: &str) -> Result<ArgumentativeExtension> {
    let cleaned = strip_reasoning_tags(response);
    let cleaned = super::super::types::extract_json_block(&cleaned)
        .map(|s| s.to_string())
        .unwrap_or_else(|| cleaned.clone());
    let raw: RawArgumentative = serde_json::from_str(&cleaned).map_err(|e| {
        Error::Serialization(format!(
            "phase 1 (argumentative) response is not valid JSON: {e}; \
             body head: {}",
            cleaned.chars().take(200).collect::<String>()
        ))
    })?;

    let positions: Vec<PositionSketch> = raw
        .positions
        .into_iter()
        .filter_map(|p| {
            let name = trim_non_empty(p.name)?;
            let content = trim_non_empty(p.content)?;
            let stance = match p.stance.trim().to_ascii_lowercase().as_str() {
                "endorse" | "endorsed" | "support" => "endorse".to_string(),
                "rebut" | "rebutted" | "oppose" | "reject" => "rebut".to_string(),
                "mixed" | "ambivalent" => "mixed".to_string(),
                "" | "survey" | "surveyed" | "neutral" => "survey".to_string(),
                other => other.to_string(),
            };
            Some(PositionSketch {
                name,
                content,
                proponent: p.proponent.trim().to_string(),
                stance,
                anchor: p.anchor.trim().to_string(),
            })
        })
        .collect();

    let mechanisms: Vec<MechanismSketch> = raw
        .mechanisms
        .into_iter()
        .filter_map(|m| {
            let name = trim_non_empty(m.name)?;
            let description = trim_non_empty(m.description)?;
            Some(MechanismSketch {
                name,
                description,
                domain: m.domain.trim().to_string(),
                anchor: m.anchor.trim().to_string(),
            })
        })
        .collect();

    let evidence_invocations: Vec<EvidenceInvocationSketch> = raw
        .evidence_invocations
        .into_iter()
        .filter_map(|e| {
            let label = trim_non_empty(e.label)?;
            let content = trim_non_empty(e.content)?;
            let kind = match e.kind.trim().to_ascii_lowercase().as_str() {
                "study" | "studies" => "study".to_string(),
                "figure" | "statistic" | "stat" | "number" => "figure".to_string(),
                "historical_example" | "history" | "example" => {
                    "historical_example".to_string()
                }
                "case_study" | "case" => "case_study".to_string(),
                "personal_anecdote" | "anecdote" | "personal" => {
                    "personal_anecdote".to_string()
                }
                "quotation" | "quote" => "quotation".to_string(),
                "" | "other" => "other".to_string(),
                other => other.to_string(),
            };
            Some(EvidenceInvocationSketch {
                label,
                content,
                kind,
                supports: e.supports.trim().to_string(),
                anchor: e.anchor.trim().to_string(),
            })
        })
        .collect();

    let oppositions: Vec<OppositionSketch> = raw
        .oppositions
        .into_iter()
        .filter_map(|o| {
            let left = trim_non_empty(o.left)?;
            let right = trim_non_empty(o.right)?;
            Some(OppositionSketch {
                left,
                right,
                axis: o.axis.trim().to_string(),
                framing: o.framing.trim().to_string(),
                anchor: o.anchor.trim().to_string(),
            })
        })
        .collect();

    let concessions: Vec<ConcessionSketch> = raw
        .concessions
        .into_iter()
        .filter_map(|c| {
            let content = trim_non_empty(c.content)?;
            let outcome = match c.outcome.trim().to_ascii_lowercase().as_str() {
                "narrowed" | "narrow" => "narrowed".to_string(),
                "retracted" | "retract" | "withdrawn" => "retracted".to_string(),
                "" | "intact" | "preserved" => "intact".to_string(),
                other => other.to_string(),
            };
            Some(ConcessionSketch {
                content,
                addresses: c.addresses.trim().to_string(),
                outcome,
                anchor: c.anchor.trim().to_string(),
            })
        })
        .collect();

    Ok(ArgumentativeExtension {
        positions,
        mechanisms,
        evidence_invocations,
        oppositions,
        concessions,
    })
}

/// Convenience for the dispatcher: parse and wrap.
pub fn parse_phase1_argumentative_extension(response: &str) -> Result<TypeExtension> {
    Ok(TypeExtension::Argumentative(parse_phase1_argumentative(
        response,
    )?))
}

// ─── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_response() {
        let r = r#"{
            "positions": [{
                "name": "rent concentration thesis",
                "content": "The deepest AI rents pool at uncopyable monopoly chokepoints.",
                "proponent": "",
                "stance": "endorse"
            }],
            "mechanisms": [{
                "name": "EUV monopoly",
                "description": "ASML's sole control over leading-edge lithography machines.",
                "domain": "economics"
            }],
            "evidence_invocations": [],
            "oppositions": [],
            "concessions": []
        }"#;
        let ext = parse_phase1_argumentative(r).expect("parses");
        assert_eq!(ext.positions.len(), 1);
        assert_eq!(ext.positions[0].name, "rent concentration thesis");
        assert_eq!(ext.positions[0].stance, "endorse");
        assert_eq!(ext.mechanisms.len(), 1);
        assert_eq!(ext.mechanisms[0].name, "EUV monopoly");
        assert_eq!(ext.evidence_invocations.len(), 0);
    }

    #[test]
    fn drops_atoms_missing_required_fields() {
        // Empty name on a position → drop.
        let r = r#"{
            "positions": [
                {"name": "", "content": "valid content"},
                {"name": "valid name", "content": "valid content"}
            ]
        }"#;
        let ext = parse_phase1_argumentative(r).expect("parses");
        assert_eq!(ext.positions.len(), 1, "empty-name position should be dropped");
    }

    #[test]
    fn unknown_stance_passes_through() {
        // The schema constrains stance via enum, but a non-grammar
        // fallback path (e.g. a model that returns extra arbitrary
        // text) should not crash — store as-is.
        let r = r#"{"positions":[{"name":"x","content":"y","stance":"ambidextrous"}]}"#;
        let ext = parse_phase1_argumentative(r).expect("parses");
        assert_eq!(ext.positions[0].stance, "ambidextrous");
    }

    #[test]
    fn evidence_kind_aliases_normalise() {
        let r = r#"{
            "evidence_invocations": [
                {"label": "A", "content": "...", "kind": "STAT"},
                {"label": "B", "content": "...", "kind": "Quote"},
                {"label": "C", "content": "...", "kind": "case"}
            ]
        }"#;
        let ext = parse_phase1_argumentative(r).expect("parses");
        let kinds: Vec<_> = ext.evidence_invocations.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["figure", "quotation", "case_study"]);
    }

    #[test]
    fn concession_outcome_normalises() {
        let r = r#"{
            "concessions": [
                {"content": "A grain of truth", "outcome": "preserved"},
                {"content": "Only in narrow cases", "outcome": "narrow"},
                {"content": "On reflection wrong", "outcome": "retract"}
            ]
        }"#;
        let ext = parse_phase1_argumentative(r).expect("parses");
        let out: Vec<_> = ext.concessions.iter().map(|c| c.outcome.as_str()).collect();
        assert_eq!(out, vec!["intact", "narrowed", "retracted"]);
    }

    #[test]
    fn rejects_non_json() {
        let r = "Sorry, I couldn't extract.";
        let err = parse_phase1_argumentative(r).expect_err("non-JSON must error");
        let msg = format!("{err}");
        assert!(msg.contains("not valid JSON"), "msg: {msg}");
    }

    #[test]
    fn strips_reasoning_tags_before_json() {
        let r = "<think>let me think about positions</think>\n{\"positions\":[{\"name\":\"x\",\"content\":\"y\"}]}";
        let ext = parse_phase1_argumentative(r).expect("parses through think tag");
        assert_eq!(ext.positions.len(), 1);
    }

    #[test]
    fn empty_arrays_round_trip() {
        let r = r#"{}"#;
        let ext = parse_phase1_argumentative(r).expect("parses empty {}");
        assert_eq!(ext.atom_count(), 0);
    }

    #[test]
    fn schema_is_valid_json() {
        let _ = phase1_argumentative_schema();
    }
}
