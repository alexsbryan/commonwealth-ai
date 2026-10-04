// SPDX-License-Identifier: AGPL-3.0-or-later
//! OICP v0.4 feature negotiation for the chat host's structured output.
//!
//! The rule is normative: a client MUST NOT send a constraint the host does
//! not advertise. So rather than assume `json_schema`, read the host's
//! advertised `features` and pick the strongest constraint it supports.

use oicp_client::StructuredOutputMode;

/// The mode to use against `chat_base`: what its OICP manifest advertises,
/// else `current`. Best-effort and non-fatal: a host that does not answer
/// `/oicp/v1/capabilities` (a v0.3 host, a bare llama-server, an offline
/// daemon) keeps `current`.
pub(super) async fn discover_structured_output(
    chat_base: &str,
    current: StructuredOutputMode,
) -> StructuredOutputMode {
    let Some(manifest) = oicp_client::fetch_manifest(chat_base, None).await else {
        tracing::debug!(
            base = %chat_base,
            mode = ?current,
            "OICP manifest unavailable — keeping default structured-output mode"
        );
        return current;
    };
    match derive_structured_mode_from_features(&manifest.features) {
        Some(mode) => {
            tracing::info!(
                base = %chat_base,
                from = ?current,
                to = ?mode,
                "structured-output mode set from advertised OICP features"
            );
            mode
        }
        None => {
            tracing::debug!(
                base = %chat_base,
                mode = ?current,
                "OICP features advertise no constraint:* — keeping default structured-output mode"
            );
            current
        }
    }
}

/// `constraint:json_schema` → [`StructuredOutputMode::JsonSchema`] (schema
/// enforced, the strongest), else `constraint:json_object` →
/// [`StructuredOutputMode::JsonObject`] (valid JSON, no schema). `None` when
/// the features name neither: the caller keeps its default rather than
/// downgrade blindly. `lark` is a distinct capability, not on this ladder.
fn derive_structured_mode_from_features(features: &[String]) -> Option<StructuredOutputMode> {
    use oicp_types::features::{CONSTRAINT_JSON_OBJECT, CONSTRAINT_JSON_SCHEMA};
    if features.iter().any(|f| f == CONSTRAINT_JSON_SCHEMA) {
        Some(StructuredOutputMode::JsonSchema)
    } else if features.iter().any(|f| f == CONSTRAINT_JSON_OBJECT) {
        Some(StructuredOutputMode::JsonObject)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_json_schema_wins_over_json_object() {
        let f = vec![
            "constraint:json_object".to_string(),
            "constraint:json_schema".to_string(),
        ];
        assert_eq!(
            derive_structured_mode_from_features(&f),
            Some(StructuredOutputMode::JsonSchema)
        );
    }

    #[test]
    fn features_json_object_only() {
        let f = vec!["constraint:json_object".to_string()];
        assert_eq!(
            derive_structured_mode_from_features(&f),
            Some(StructuredOutputMode::JsonObject)
        );
    }

    #[test]
    fn features_without_constraint_keep_default() {
        // A host advertising features but no constraint:* gives no decisive
        // signal; neither does an empty list (a v0.3 host).
        let f = vec!["think_budget".to_string(), "ingest:v1".to_string()];
        assert_eq!(derive_structured_mode_from_features(&f), None);
        assert_eq!(derive_structured_mode_from_features(&[]), None);
    }
}
