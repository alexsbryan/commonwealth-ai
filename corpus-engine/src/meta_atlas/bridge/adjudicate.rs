// SPDX-License-Identifier: AGPL-3.0-or-later
//! LLM adjudication seam — types the *uncertain band* of candidate pairs.
//!
//! corpus-engine stays inference-agnostic: the model call is an injected
//! [`AdjudicateFn`] (the caller issues a `structured_output`
//! CompletionRequest on its own inference stack). This module owns the
//! PURE parts — the prompt builder, the JSON schema, and the response
//! parser — so they're unit-testable here and the caller's closure is a
//! thin "prompt → model → parse" wrapper. Nothing here names a corpus.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Deserialize;

use super::edges::BridgeRelation;
use crate::error::{Error, Result};

/// What the adjudicator is shown about a candidate `left → right` pair.
#[derive(Debug, Clone)]
pub struct AdjudicationRequest {
    pub left_title: String,
    pub left_gloss: String,
    /// Names of the left article's argument structure
    /// (`ArgumentReconstruction` / `Position` labels), for the model to
    /// judge the relation against. Empty for inventory-only sides.
    pub left_arguments: Vec<String>,
    pub right_title: String,
    pub right_gloss: String,
}

/// The adjudicator's verdict for a pair it judged a real correspondence.
#[derive(Debug, Clone)]
pub struct AdjudicationVerdict {
    pub relation: BridgeRelation,
    pub confidence: f32,
    pub rationale: Option<String>,
}

/// Injected model call. The caller implements this by building a
/// `structured_output` request (schema from [`adjudication_schema`],
/// prompt from [`build_adjudication_prompt`]) and parsing the reply with
/// [`parse_adjudication_response`]. `Ok(None)` = the model judged the
/// pair `different` (no edge).
pub type AdjudicateFn = Arc<
    dyn Fn(
            AdjudicationRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Option<AdjudicationVerdict>>> + Send>>
        + Send
        + Sync,
>;

/// Build the forced-choice adjudication prompt. Pure + testable.
pub fn build_adjudication_prompt(req: &AdjudicationRequest) -> String {
    let args = if req.left_arguments.is_empty() {
        String::new()
    } else {
        let names: Vec<&str> = req
            .left_arguments
            .iter()
            .take(8)
            .map(|s| s.as_str())
            .collect();
        format!("\nIts argument structure includes: {}.", names.join("; "))
    };
    format!(
        "You are aligning concepts across two knowledge corpora.\n\n\
         LEFT — \"{lt}\":\n{lg}{args}\n\n\
         RIGHT — \"{rt}\":\n{rg}\n\n\
         Decide how the LEFT concept relates to the RIGHT concept:\n\
         - same: the same concept (possibly treated in a different register)\n\
         - broader: LEFT subsumes RIGHT (RIGHT is a special case of LEFT)\n\
         - narrower: LEFT is a special case of RIGHT\n\
         - related: connected, but not the same and neither subsumes the other\n\
         - different: not actually about the same thing\n\n\
         Answer with the relation, a confidence in 0..1, and a one-sentence rationale.",
        lt = req.left_title,
        lg = truncate(&req.left_gloss, 800),
        args = args,
        rt = req.right_title,
        rg = truncate(&req.right_gloss, 800),
    )
}

/// JSON schema for the forced-choice structured output. Hand this to the
/// inference layer's `structured_output` field.
pub fn adjudication_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "relation": {
                "type": "string",
                "enum": ["same", "broader", "narrower", "related", "different"]
            },
            "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
            "rationale": { "type": "string" }
        },
        "required": ["relation", "confidence"]
    })
}

#[derive(Deserialize)]
struct RawVerdict {
    relation: String,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    rationale: Option<String>,
}

/// Parse the model's JSON reply. `different` → `Ok(None)` (no edge);
/// unknown relations and malformed JSON are errors. Pure + testable.
pub fn parse_adjudication_response(json: &str) -> Result<Option<AdjudicationVerdict>> {
    let raw: RawVerdict = serde_json::from_str(json.trim())
        .map_err(|e| Error::Extraction(format!("adjudication parse: {e}; body={json}")))?;
    let relation = match raw.relation.trim().to_lowercase().as_str() {
        "same" => BridgeRelation::Same,
        "broader" => BridgeRelation::Broader,
        "narrower" => BridgeRelation::Narrower,
        "related" => BridgeRelation::Related,
        "different" => return Ok(None),
        other => return Err(Error::Extraction(format!("unknown relation: {other}"))),
    };
    Ok(Some(AdjudicationVerdict {
        relation,
        confidence: raw.confidence.clamp(0.0, 1.0),
        rationale: raw.rationale.filter(|s| !s.is_empty()),
    }))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> AdjudicationRequest {
        AdjudicationRequest {
            left_title: "Externalism About the Mind".into(),
            left_gloss: "Content externalism via Twin Earth.".into(),
            left_arguments: vec!["Twin Earth Argument".into()],
            right_title: "Semantic externalism".into(),
            right_gloss: "'''Semantic externalism''' is the view that…".into(),
        }
    }

    #[test]
    fn prompt_includes_both_sides_and_arguments() {
        let p = build_adjudication_prompt(&req());
        assert!(p.contains("Externalism About the Mind"));
        assert!(p.contains("Semantic externalism"));
        assert!(p.contains("Twin Earth Argument"));
        assert!(p.contains("broader"));
    }

    #[test]
    fn parse_same() {
        let v = parse_adjudication_response(
            r#"{"relation":"same","confidence":0.9,"rationale":"both content externalism"}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(v.relation, BridgeRelation::Same);
        assert!((v.confidence - 0.9).abs() < 1e-6);
        assert_eq!(v.rationale.as_deref(), Some("both content externalism"));
    }

    #[test]
    fn parse_broader_and_clamps_confidence() {
        let v = parse_adjudication_response(r#"{"relation":"broader","confidence":1.7}"#)
            .unwrap()
            .unwrap();
        assert_eq!(v.relation, BridgeRelation::Broader);
        assert_eq!(v.confidence, 1.0);
        assert!(v.rationale.is_none());
    }

    #[test]
    fn parse_different_is_no_edge() {
        assert!(
            parse_adjudication_response(r#"{"relation":"different","confidence":0.8}"#)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn parse_unknown_relation_errors() {
        assert!(
            parse_adjudication_response(r#"{"relation":"identical","confidence":0.8}"#).is_err()
        );
    }

    #[test]
    fn parse_malformed_errors() {
        assert!(parse_adjudication_response("not json").is_err());
    }
}
