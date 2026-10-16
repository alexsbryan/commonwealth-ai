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

use serde_json::Value;

/// The key a body declares and the detector reads. Underscore, deliberately
/// distinct from the advertised feature string `x:forced_choice`.
pub const SENTINEL: &str = "x_forced_choice";

/// The key a wave body declares: several forced choices answered off ONE
/// decoded prompt. Each branch appends its own text to the request's prompt
/// and is a forced-choice body of its own ([`schema`]); the engine answers
/// every branch as the single request `prompt + suffix` would be answered,
/// and no branch sees another (`embedded::model_slot`, `decide_wave`).
/// Advertised as `x:forced_choice_wave`.
pub const WAVE: &str = "x_forced_choice_wave";

/// The key a refused branch's answer carries in place of a distribution.
pub const REFUSED: &str = "x_refused";

/// One question of a wave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// The text this question appends to the request's prompt.
    pub suffix: String,
    /// Its labels, as its own forced-choice body names them.
    pub labels: Vec<String>,
}

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

/// The labels a forced-choice body names; `None` when `body` is not one. The
/// one reader of the sentinel: the request's detector and a wave's branches
/// both read through it.
pub fn candidates(body: &Value) -> Option<Vec<String>> {
    if body.get(SENTINEL).and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let labels: Vec<String> = body
        .get("enum")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    (!labels.is_empty()).then_some(labels)
}

/// The forced-choice body `branch` stands for: the one its single request
/// carries. The engine renders a branch as that request, and a caller splits a
/// wave back into its single questions with it.
pub fn branch_body(branch: &Branch) -> Value {
    let labels: Vec<&str> = branch.labels.iter().map(String::as_str).collect();
    schema(&labels)
}

/// The wave body over `branches`: each its suffix beside its own
/// forced-choice body ([`branch_body`]).
pub fn wave(branches: &[Branch]) -> Value {
    let items: Vec<Value> = branches
        .iter()
        .map(|b| serde_json::json!({ "suffix": b.suffix, "choice": branch_body(b) }))
        .collect();
    let mut body = serde_json::Map::new();
    body.insert(WAVE.to_string(), Value::Array(items));
    Value::Object(body)
}

/// The branches `body` asks when it is a wave. `None` when it declares no
/// wave; `Err` when it declares one it does not spell, so a malformed wave is
/// refused rather than read as an ordinary schema and generated against.
pub fn wave_branches(body: &Value) -> Option<Result<Vec<Branch>, String>> {
    let items = body.get(WAVE)?;
    let Some(items) = items.as_array() else {
        return Some(Err("the wave is not a list".to_string()));
    };
    if items.is_empty() {
        return Some(Err("the wave asks nothing".to_string()));
    }
    Some(
        items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let suffix = item
                    .get("suffix")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("branch {i} has no suffix"))?;
                let labels = item
                    .get("choice")
                    .and_then(candidates)
                    .ok_or_else(|| format!("branch {i} is not a forced choice"))?;
                Ok(Branch {
                    suffix: suffix.to_string(),
                    labels,
                })
            })
            .collect(),
    )
}

/// The engine's answer to a single forced choice: the text a caller parses
/// with [`parse`] and an answer store records.
pub fn answer(dist: &BTreeMap<String, f32>) -> String {
    serde_json::to_string(dist).expect("a label-to-probability map always serializes")
}

/// The engine's answer to a wave, in branch order: each branch's
/// distribution as the very text [`answer`] gives its single request (f32
/// digits, which `json!` would widen to f64's), or `{"x_refused": why}`.
pub fn wave_answer(answers: &[Result<BTreeMap<String, f32>, String>]) -> String {
    let items: Vec<String> = answers
        .iter()
        .map(|a| match a {
            Ok(dist) => answer(dist),
            Err(why) => {
                let mut refused = serde_json::Map::new();
                refused.insert(REFUSED.to_string(), Value::String(why.clone()));
                Value::Object(refused).to_string()
            }
        })
        .collect();
    format!("[{}]", items.join(","))
}

/// A wave's answer as each branch's own text: the answer its single request
/// gets, or the refusal. `None` when the text is not a list.
pub fn wave_texts(text: &str) -> Option<Vec<Result<String, String>>> {
    let items: Vec<Value> = serde_json::from_str(text.trim()).ok()?;
    Some(
        items
            .into_iter()
            .map(|item| match item.get(REFUSED) {
                Some(why) => Err(why.as_str().unwrap_or("refused").to_string()),
                None => Ok(item.to_string()),
            })
            .collect(),
    )
}

/// A wave's answer read back, in branch order: each branch's distribution or
/// its refusal. `None` when the text is not a list of either.
pub fn parse_wave(text: &str) -> Option<Vec<Result<BTreeMap<String, f64>, String>>> {
    let items: Vec<Value> = serde_json::from_str(text.trim()).ok()?;
    items
        .into_iter()
        .map(|item| match item.get(REFUSED) {
            Some(why) => Some(Err(why.as_str().unwrap_or("refused").to_string())),
            None => serde_json::from_value(item).ok().map(Ok),
        })
        .collect()
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

    fn branch(suffix: &str, labels: &[&str]) -> Branch {
        Branch {
            suffix: suffix.to_string(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    #[test]
    fn a_wave_round_trips_and_is_not_a_single_forced_choice() {
        let branches = vec![
            branch("Line 1?", &["A", "0"]),
            branch("Line 2?", &["A", "B", "0"]),
        ];
        let req = CompletionRequest {
            structured_output: Some(wave(&branches)),
            ..Default::default()
        };
        assert_eq!(req.forced_choice_wave(), Some(Ok(branches)));
        assert_eq!(req.forced_choice_candidates(), None);
        assert_eq!(
            req.forced_choice_feature(),
            Some(crate::features::X_FORCED_CHOICE_WAVE)
        );
        let single = CompletionRequest {
            structured_output: Some(schema(&["A", "0"])),
            ..Default::default()
        };
        assert_eq!(single.forced_choice_wave(), None);
        assert_eq!(
            single.forced_choice_feature(),
            Some(crate::features::X_FORCED_CHOICE)
        );
        assert_eq!(CompletionRequest::default().forced_choice_feature(), None);
    }

    #[test]
    fn a_malformed_wave_is_an_error_not_an_ordinary_schema() {
        for body in [
            serde_json::json!({ WAVE: "x" }),
            serde_json::json!({ WAVE: [] }),
            serde_json::json!({ WAVE: [{ "choice": schema(&["A"]) }] }),
            serde_json::json!({ WAVE: [{ "suffix": "q", "choice": { "enum": ["A"] } }] }),
        ] {
            assert!(matches!(wave_branches(&body), Some(Err(_))), "{body}");
        }
        assert_eq!(wave_branches(&schema(&["A"])), None);
    }

    #[test]
    fn a_wave_answer_keeps_each_branch_and_each_refusal_in_order() {
        let mut d = BTreeMap::new();
        d.insert("A".to_string(), 0.75f32);
        d.insert("0".to_string(), 0.25f32);
        let text = wave_answer(&[Ok(d), Err("no label encodes to one token".to_string())]);
        let back = parse_wave(&text).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].as_ref().unwrap()["A"], 0.75);
        assert_eq!(
            back[1].as_ref().unwrap_err(),
            "no label encodes to one token"
        );
        assert!(parse_wave("{\"A\":1.0}").is_none());
    }

    #[test]
    fn a_branch_answers_in_the_bytes_its_single_request_gets() {
        let mut d = BTreeMap::new();
        d.insert("0".to_string(), 0.9984144f32);
        d.insert("A".to_string(), 0.0014906893f32);
        d.insert("B".to_string(), 0.00009485963f32);
        let single = answer(&d);
        // Byte for byte the first answer converge-s3's Ward store recorded.
        assert_eq!(
            single,
            "{\"0\":0.9984144,\"A\":0.0014906893,\"B\":0.00009485963}"
        );
        let texts = wave_texts(&wave_answer(&[Ok(d.clone()), Ok(d)])).unwrap();
        assert_eq!(texts, vec![Ok(single.clone()), Ok(single)]);
    }
}
