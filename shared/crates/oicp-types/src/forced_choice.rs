// SPDX-License-Identifier: AGPL-3.0-or-later
//! The forced-choice wire shape: the one body constructor, its parser, and
//! the sentinel the detector ([`crate::completion::CompletionRequest::forced_choice_candidates`])
//! reads. A caller asks a closed question; the engine answers with its
//! next-token distribution over the labels in one forward pass, no
//! generation (`embedded::model_slot::forced_choice_probs`).
//!
//! Every forced-choice body in the tree is built by [`schema`], and every
//! caller issues it through a census (`cargo xtask judge-funnel-gate`).

use std::collections::BTreeMap;

/// The key a body declares and the detector reads. Underscore, deliberately
/// distinct from the advertised feature string `x:forced_choice`.
pub const SENTINEL: &str = "x_forced_choice";

/// The forced-choice body over `labels`. Each label must encode to a single
/// token; the engine refuses a set where none does.
pub fn schema(labels: &[&str]) -> serde_json::Value {
    serde_json::json!({ "type": "string", "enum": labels, "x_forced_choice": true })
}

/// The distribution a forced-choice call answers with, label to probability
/// (`{"A":0.7,"B":0.3}`). `None` when the text is not that map.
pub fn parse(text: &str) -> Option<BTreeMap<String, f64>> {
    serde_json::from_str(text.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::CompletionRequest;

    #[test]
    fn the_body_is_what_the_detector_reads_and_the_answer_parses() {
        let req = CompletionRequest {
            structured_output: Some(schema(&["A", "B", "0"])),
            ..Default::default()
        };
        assert_eq!(
            req.forced_choice_candidates(),
            Some(vec!["A".to_string(), "B".to_string(), "0".to_string()])
        );
        let d = parse(" {\"0\":0.1,\"A\":0.7,\"B\":0.2} ").unwrap();
        assert_eq!(d["A"], 0.7);
        assert!(parse("A").is_none());
    }
}
