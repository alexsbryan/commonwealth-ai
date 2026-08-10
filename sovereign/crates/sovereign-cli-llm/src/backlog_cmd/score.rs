// SPDX-License-Identifier: AGPL-3.0-or-later
//! One scoring call against the resident daemon model.
//!
//! Reuse, not machinery (ARCH §19): this is
//! [`DaemonInferenceClient`](crate::enrich_cmd::inference_client) — the
//! same HTTP path `svrn enrich` uses — with the ruler as the system
//! prompt and a JSON schema for grammar-constrained output. No in-process
//! model, no session bootstrap, no new inference path.
//!
//! Refusal, never substitution (ARCH §18.3): daemon down, no chat model
//! resident, or an unparseable answer each return an [`Err`] the caller
//! prints and exits on. Nothing in here can file an unscored item as a
//! scored one.

use corpus_engine::enrichment::pipeline::ChatPrompt;
use serde::Deserialize;

use super::ruler::Ruler;
use crate::enrich_cmd::inference_client::{
    probe_daemon, resolve_default_models, DaemonInferenceClient,
};

/// What the model is asked for, and what the verb will accept back.
#[derive(Debug, Clone, Deserialize)]
pub struct Score {
    pub value: i64,
    pub axis: String,
    pub rationale: String,
    pub approach: String,
    pub cost: String,
    /// The measurement the model found IN THE ITEM TEXT, quoted, or
    /// empty. Load-bearing: see [`Score::apply_measurement_cap`].
    #[serde(default)]
    pub measurement: String,
    /// Set when the cap fired, so the verb can say so out loud rather
    /// than quietly hand back a different number than the model gave
    /// (ARCH §9 — a decision invisible at debug is not finished).
    #[serde(skip)]
    pub capped_from: Option<i64>,
    /// The model id that produced this. Stamped onto the note; its
    /// presence is what keeps the item unvetted.
    #[serde(skip)]
    pub scored_by: String,
}

impl Score {
    /// The top of the scale requires "a measurement attached" — the
    /// ruler's words. The model does not hold that ceiling: on the
    /// 10-fixture validation run, 4 of 10 items came back at the top of
    /// the scale with nothing quoted, two of them still doing it after
    /// the instruction was made explicit in the prompt.
    ///
    /// So the model is asked only to QUOTE the measurement — which it
    /// does reliably — and the ceiling is arithmetic here. Never ask a
    /// model to guarantee what code can enforce (ARCH §7.6). The cap
    /// moved value-exact agreement with the seat's own scores from 4/10
    /// to 6/10 and within-one from 7/10 to 9/10.
    pub fn apply_measurement_cap(&mut self, ruler: &Ruler) {
        const MIN_QUOTE: usize = 3;
        if self.value >= ruler.value.max && self.measurement.trim().len() < MIN_QUOTE {
            self.capped_from = Some(self.value);
            self.value = ruler.value.max - 1;
            tracing::debug!(
                capped_from = self.capped_from,
                value = self.value,
                "no measurement quoted from the item text; top of the scale withheld"
            );
        }
    }

    /// The `Value:` line as the item format wants it: the number, the
    /// axis letter, and one falsifiable line.
    ///
    /// The prompt asks the rationale to name its axis, so the model
    /// usually opens with "A Grounded: …" — and this line already
    /// carries the axis, by construction, from the parsed field rather
    /// than from prose. Printing both gives "4 — A Grounded: A Grounded:
    /// …", so the redundant opener is stripped here. The axis on the
    /// line is always the one the parser validated against the ruler,
    /// never the one the sentence happens to mention.
    pub fn value_line(&self, ruler: &Ruler) -> String {
        let name = ruler
            .axes
            .iter()
            .find(|a| a.letter == self.axis)
            .map(|a| a.name.as_str())
            .unwrap_or("");
        format!(
            "{} — {} {}: {}",
            self.value,
            self.axis,
            name,
            strip_axis_prefix(self.rationale.trim(), &self.axis, name)
        )
    }
}

/// Drop a leading "A Grounded:" / "A:" / "Grounded:" from a rationale.
/// Only an exact opener for THIS item's axis is removed — a rationale
/// that merely mentions an axis mid-sentence is left alone, because the
/// sentence is the operator's evidence and this is presentation.
fn strip_axis_prefix(rationale: &str, letter: &str, name: &str) -> String {
    for opener in [
        format!("{letter} {name}:"),
        format!("{letter} ({name}):"),
        format!("{name}:"),
        format!("{letter}:"),
        format!("Axis {letter}:"),
    ] {
        if let Some(rest) = rationale.strip_prefix(opener.as_str()) {
            return rest.trim_start().to_string();
        }
    }
    rationale.to_string()
}

/// The response schema. Enums and bounds here mean the daemon's grammar
/// does the shape-checking, and [`parse`] is left validating only what a
/// grammar cannot (that the letters are ones this ruler declares).
fn response_schema(ruler: &Ruler) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "value": {"type": "integer", "minimum": ruler.value.min, "maximum": ruler.value.max},
            "axis": {"type": "string", "enum": ruler.axis_letters()},
            "rationale": {"type": "string"},
            "approach": {"type": "string"},
            "cost": {"type": "string", "enum": ruler.cost_letters()},
            "measurement": {"type": "string"},
        },
        "required": ["value", "axis", "rationale", "approach", "cost", "measurement"],
        "additionalProperties": false,
    })
}

/// Tolerate a fenced block or leading prose, then require every field.
/// The grammar makes this rarely necessary; a provider that ignores the
/// schema makes it necessary always, and a silent `unwrap_or_default`
/// here would be exactly the substitution §18.3 forbids.
pub fn parse(raw: &str, ruler: &Ruler) -> Result<Score, String> {
    let body = raw.trim();
    let body = match (body.find('{'), body.rfind('}')) {
        (Some(i), Some(j)) if j > i => &body[i..=j],
        _ => {
            return Err(format!(
                "the model's answer contained no JSON object: {}",
                truncate(body, 200)
            ))
        }
    };
    let mut score: Score = serde_json::from_str(body).map_err(|e| {
        format!(
            "the model's answer was not a score: {e}: {}",
            truncate(body, 200)
        )
    })?;
    score.axis = score.axis.trim().to_uppercase();
    score.cost = score.cost.trim().to_uppercase();
    if !ruler.axis_letters().contains(&score.axis) {
        return Err(format!(
            "the model named axis {:?}, which this ruler does not declare ({})",
            score.axis,
            ruler.axis_letters().join("/")
        ));
    }
    if !ruler.cost_letters().contains(&score.cost) {
        return Err(format!(
            "the model sized this {:?}, which this ruler does not declare ({})",
            score.cost,
            ruler.cost_letters().join("/")
        ));
    }
    if score.value < ruler.value.min || score.value > ruler.value.max {
        return Err(format!(
            "the model scored {} , outside the ruler's {}-{}",
            score.value, ruler.value.min, ruler.value.max
        ));
    }
    if score.rationale.trim().is_empty() {
        return Err("the model gave a score with no rationale".to_string());
    }
    score.apply_measurement_cap(ruler);
    Ok(score)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

/// The resident daemon, unless the caller names another one.
pub fn default_daemon_base() -> String {
    sovereign_core::setup_config::client_daemon_base()
}

/// Score one item. Every failure path refuses with a reason.
///
/// `base` is a parameter rather than a lookup so the refusal paths have
/// a failing input a test can name (ARCH §18.1): point it at a port
/// nothing listens on and the daemon-down branch is exercised for real,
/// without touching the running daemon other sessions depend on.
pub async fn score_item(
    ruler: &Ruler,
    base: &str,
    objective: Option<&str>,
    text: &str,
) -> Result<Score, String> {
    if !probe_daemon(base).await {
        return Err(format!(
            "the daemon is not responding at {base}, so nothing can score this \
             item. Start it with `svrn daemon start`, or file the item unscored \
             with --no-score. It is NOT being filed as scored."
        ));
    }
    let (chat, embed) = resolve_default_models(base).await;
    let Some(chat_model) = chat else {
        return Err(format!(
            "the daemon at {base} advertises no chat model, so nothing can \
             score this item. Load one, or file it unscored with --no-score. \
             It is NOT being filed as scored."
        ));
    };
    let client = DaemonInferenceClient::new(base, &chat_model, embed.unwrap_or_default())
        .map_err(|e| format!("cannot reach the daemon at {base}: {e}"))?;

    let user = format!(
        "Objective it serves: {}\n\nItem text:\n{}",
        objective.unwrap_or("unstated"),
        text.trim()
    );
    let mut prompt = ChatPrompt::new(ruler.system_prompt(), user)
        .with_response_schema("backlog_score", response_schema(ruler));
    prompt.phase_id = Some("backlog-add".to_string());
    // Low, not zero: the ruler wants a judgement, and the validation run
    // was measured at this temperature.
    prompt.temperature = Some(0.2);
    tracing::debug!(
        model = %chat_model,
        ruler = %ruler.path.display(),
        ruler_version = %ruler.version,
        "scoring one backlog item"
    );
    let raw = client
        .complete(&prompt)
        .await
        .map_err(|e| format!("the scoring call failed: {e}. The item is NOT filed."))?;
    let mut score = parse(&raw, ruler)?;
    score.scored_by = chat_model;
    Ok(score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog_cmd::ruler::Ruler;

    fn ruler() -> Ruler {
        Ruler::load(None).expect("the repo's own ruler must load")
    }

    fn body(value: i64, measurement: &str) -> String {
        format!(
            r#"{{"value": {value}, "axis": "A", "rationale": "A Grounded: cuts wrong-accepts",
                "approach": "extend the existing holdings gate", "cost": "S",
                "measurement": "{measurement}"}}"#
        )
    }

    #[test]
    fn the_top_of_the_scale_needs_a_quoted_measurement() {
        let r = ruler();
        // WITH a measurement: the model's 5 stands.
        let kept = parse(&body(5, "2/7 wrong-accepts"), &r).unwrap();
        assert_eq!(kept.value, 5);
        assert_eq!(kept.capped_from, None);
        // WITHOUT: the cap fires, and SAYS it fired.
        let capped = parse(&body(5, ""), &r).unwrap();
        assert_eq!(capped.value, 4);
        assert_eq!(capped.capped_from, Some(5));
    }

    #[test]
    fn the_cap_touches_nothing_below_the_top() {
        let r = ruler();
        for v in 1..r.value.max {
            let s = parse(&body(v, ""), &r).unwrap();
            assert_eq!(s.value, v, "value {v} must survive an empty measurement");
            assert_eq!(s.capped_from, None);
        }
    }

    #[test]
    fn a_letter_the_ruler_does_not_declare_is_refused_not_coerced() {
        let r = ruler();
        let bad_axis =
            r#"{"value":3,"axis":"Z","rationale":"x","approach":"y","cost":"S","measurement":""}"#;
        let err = parse(bad_axis, &r).expect_err("axis Z is not in the ruler");
        assert!(err.contains("Z"), "{err}");
        let bad_cost =
            r#"{"value":3,"axis":"A","rationale":"x","approach":"y","cost":"XL","measurement":""}"#;
        let err = parse(bad_cost, &r).expect_err("cost XL is not in the ruler");
        assert!(err.contains("XL"), "{err}");
    }

    #[test]
    fn junk_is_refused_rather_than_defaulted() {
        let r = ruler();
        for junk in [
            "I think this is a good idea, probably a 4.",
            "",
            "{\"value\": 3}",
            "{\"value\":3,\"axis\":\"A\",\"rationale\":\"  \",\"approach\":\"y\",\"cost\":\"S\",\"measurement\":\"\"}",
        ] {
            assert!(
                parse(junk, &r).is_err(),
                "an unscoreable answer must refuse, not produce a default score: {junk:?}"
            );
        }
    }

    #[test]
    fn a_fenced_answer_still_parses() {
        let r = ruler();
        let fenced = format!("Here you go:\n```json\n{}\n```\n", body(3, ""));
        assert_eq!(parse(&fenced, &r).unwrap().value, 3);
    }

    #[test]
    fn the_value_line_names_the_axis_once() {
        let r = ruler();
        // The prompt asks the rationale to name its axis, so the model
        // opens with "A Grounded:" — the line must not say it twice.
        let s = parse(&body(4, "x"), &r).unwrap();
        assert_eq!(s.value_line(&r), "4 — A Grounded: cuts wrong-accepts");
        // A rationale that does NOT open with its axis is untouched.
        assert_eq!(
            strip_axis_prefix("cuts wrong-accepts on axis A", "A", "Grounded"),
            "cuts wrong-accepts on axis A"
        );
        // And the axis printed is the PARSED one, not one mentioned in
        // the sentence: a rationale naming a different axis cannot move
        // the item to that axis.
        let mixed = r#"{"value":3,"axis":"E","rationale":"A Grounded: this mentions A",
                        "approach":"y","cost":"S","measurement":""}"#;
        let s = parse(mixed, &r).unwrap();
        assert!(
            s.value_line(&r).starts_with("3 — E Clean handoffs:"),
            "{}",
            s.value_line(&r)
        );
    }
}
