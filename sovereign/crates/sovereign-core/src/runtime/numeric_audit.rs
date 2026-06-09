// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic numeric-provenance audit — Layer 3 of the "no
//! confabulated numbers" guarantee (SF-LVT demo).
//!
//! The objective is narrow and load-bearing: **the model must never
//! *originate* a number.** Every figure in a synthesized answer is one of
//! two things — a datum read from the corpus, or a value *computed* by a
//! deterministic tool (a sum, a ratio) over a named set of cited atoms.
//! Computed values do not appear in any single source chunk; their
//! provenance is the computation itself (formula + input set), which the
//! tool performed in auditable Rust and emitted. So "traceable" here means
//! **the figure equals a value the tool actually produced** — not that it
//! appears verbatim in a citation string.
//!
//! Layer 1 is the tool emitting pre-cited figures *and* a `derivation`;
//! Layer 2 is the synthesis prompt forbidding model-originated numbers and
//! surfacing the derivation to the reader; this is the deterministic
//! backstop: after synthesis, every dollar / percentage figure in the
//! answer must match — *by value* — a figure the tool emitted, in either
//! its formatted form (`$1.48B`) or its exact form (`$1,477,806,471.00`).
//! A figure matching neither is flagged as model-originated.
//!
//! Scoped to `$…` and `…%` tokens. Bare integers (years, parcel counts,
//! list indices) are NOT audited, so the gate doesn't false-positive on
//! "in 2024" or "874 parcels". Pure: no I/O, no inference — unit-tested.

/// The dollar / percentage figures in `answer` not traceable to any value
/// the tool emitted. The allowed set is the union of (a) the figures
/// parsed out of the tool's formatted `cited` strings and (b) the tool's
/// raw numeric outputs (`raw_values`) — so a *precise* quote like
/// `$1,477,806,471.00` traces to the raw `land_value_total` even though
/// the cited string shows the compact `$1.48B`. Empty result ⇒ every
/// figure traces to the deterministic engine (clean).
pub fn uncited_numerics(answer: &str, cited: &[String], raw_values: &[f64]) -> Vec<String> {
    // Nothing the tool emitted ⇒ no audit basis. Callers only invoke the
    // audit when a figure-emitting tool ran, but keep the pure function
    // safe to call unconditionally (it must not flag every figure in an
    // answer that simply wasn't backed by such a tool).
    if cited.is_empty() && raw_values.is_empty() {
        return Vec::new();
    }

    // Allowed values: the formatted figures the tool cited (parsed back to
    // their numeric value) ∪ the tool's raw numeric outputs.
    let mut allowed: Vec<f64> = Vec::new();
    for c in cited {
        for tok in extract_figures(c) {
            if let Some(v) = parse_value(&tok) {
                allowed.push(v);
            }
        }
    }
    allowed.extend_from_slice(raw_values);

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for tok in extract_figures(answer) {
        let Some(v) = parse_value(&tok) else { continue };
        // A percentage may faithfully relay a rate the tool stored as a
        // fraction (`0.9474` ↔ `94.74%`); accept either reading so quoting
        // the rate as a percent isn't mistaken for a fabrication.
        let is_pct = tok.trim_end().ends_with('%');
        let traceable = allowed.iter().any(|a| values_match(*a, v))
            || (is_pct && allowed.iter().any(|a| values_match(*a, v / 100.0)));
        if !traceable && seen.insert(tok.clone()) {
            out.push(tok);
        }
    }
    out
}

/// Recursively collect every numeric leaf in a JSON value — the raw
/// figures a tool emitted. These are the *exact* (un-rounded) side of the
/// audit's allowed set; the formatted side comes from the tool's
/// `cited_figures` strings. A model quoting the precise `land_value_total`
/// thus traces even though the cited string shows the rounded `$1.48B`.
pub fn json_numeric_leaves(v: &serde_json::Value, out: &mut Vec<f64>) {
    match v {
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                out.push(f);
            }
        }
        serde_json::Value::Array(arr) => {
            for x in arr {
                json_numeric_leaves(x, out);
            }
        }
        serde_json::Value::Object(map) => {
            for x in map.values() {
                json_numeric_leaves(x, out);
            }
        }
        _ => {}
    }
}

/// Two figures match when they are equal to within a tight *relative*
/// epsilon — float-representation slack, NOT a rounding tolerance. So a
/// precise quote matches its raw value exactly, a compact quote matches
/// its formatted value exactly, but a model re-rounding `$172.62B` to
/// `$172.6B` (a 0.01% shift) still fails: the model altered the figure.
fn values_match(a: f64, b: f64) -> bool {
    let scale = a.abs().max(b.abs()).max(1.0);
    (a - b).abs() <= 1e-6 * scale
}

/// Extract `$<number>[<magnitude>]` and `<number>%` tokens. `<magnitude>`
/// is a single suffix letter (`B`/`M`/`K`/`T`) or a spelled-out word
/// ("billion" / "million" / "thousand" / "trillion", possibly after one
/// space). Bare numbers (no `$`, no `%`) are intentionally skipped.
fn extract_figures(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '$' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == ',' || chars[i] == '.')
            {
                i += 1;
            }
            // Require at least one digit after the `$`.
            if i > start + 1 {
                i = consume_magnitude(&chars, i);
                out.push(chars[start..i].iter().collect());
            }
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == ',' || chars[i] == '.')
            {
                i += 1;
            }
            if i < chars.len() && chars[i] == '%' {
                i += 1;
                out.push(chars[start..i].iter().collect());
            }
            // else: bare number (year / count) — not a guarded figure.
        } else {
            i += 1;
        }
    }
    out
}

/// Advance `i` past a magnitude suffix following a `$<number>`: a single
/// letter (`B`/`M`/`K`/`T`, not part of a longer word) or a spelled-out
/// word after at most one space. Returns the unchanged index if none.
fn consume_magnitude(chars: &[char], i: usize) -> usize {
    // Single-letter suffix (e.g. `$1.48B`), but only if it's not the start
    // of a longer alphabetic word (so we don't eat the `B` of "Bay").
    if i < chars.len() && matches!(chars[i], 'B' | 'M' | 'K' | 'T' | 'b' | 'm' | 'k' | 't') {
        let next_is_alpha = chars.get(i + 1).map(|c| c.is_ascii_alphabetic()).unwrap_or(false);
        if !next_is_alpha {
            return i + 1;
        }
    }
    // Spelled-out word after an optional single space (e.g. `$1.4 billion`).
    let mut j = i;
    if j < chars.len() && chars[j] == ' ' {
        j += 1;
    }
    let word_start = j;
    while j < chars.len() && chars[j].is_ascii_alphabetic() {
        j += 1;
    }
    let word: String = chars[word_start..j].iter().collect::<String>().to_lowercase();
    if matches!(word.as_str(), "billion" | "million" | "thousand" | "trillion" | "bn") {
        return j;
    }
    i
}

/// Parse a `$`/`%`/magnitude-suffixed figure token to its numeric value.
/// `$1.48B` → 1.48e9, `$1,477,806,471.0` → 1477806471.0,
/// `$1.4 billion` → 1.4e9, `94.74%` → 94.74, `0.81%` → 0.81. `None` when
/// the token carries no digits.
fn parse_value(token: &str) -> Option<f64> {
    let lower = token.trim().to_ascii_lowercase();
    let is_pct = lower.ends_with('%');
    let mult = if lower.contains("trillion") {
        1e12
    } else if lower.contains("billion") || lower.ends_with("bn") {
        1e9
    } else if lower.contains("million") {
        1e6
    } else if lower.contains("thousand") {
        1e3
    } else {
        // Single trailing magnitude letter (the `b` of `$1.48b`). `%`/`$`
        // and digits are not alphabetic, so this finds only a magnitude.
        match lower.chars().rev().find(|c| c.is_ascii_alphabetic()) {
            Some('t') => 1e12,
            Some('b') => 1e9,
            Some('m') => 1e6,
            Some('k') => 1e3,
            _ => 1.0,
        }
    };
    let digits: String = lower
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() || digits == "." {
        return None;
    }
    let base: f64 = digits.parse().ok()?;
    // Percentages are values as-written (94.74% → 94.74); no magnitude.
    Some(if is_pct { base } else { base * mult })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cited() -> Vec<String> {
        vec![
            "land_value_total = $172.62B [sf-assessor-roll: 207,792 parcels; e.g. atom entity-9f3a]"
                .to_string(),
            "neutral_rate = 0.81% [= business_tax_target $1.40B ÷ land_value_total $172.62B]"
                .to_string(),
        ]
    }

    #[test]
    fn clean_when_every_figure_is_cited() {
        let answer = "A flat land levy of 0.81% on the $172.62B base replaces the $1.40B business tax.";
        assert!(uncited_numerics(answer, &cited(), &[]).is_empty());
    }

    #[test]
    fn flags_an_invented_dollar_figure() {
        let answer = "The land base is $172.62B, but improvements add a surprising $999.99B.";
        let v = uncited_numerics(answer, &cited(), &[]);
        assert_eq!(v, vec!["$999.99B".to_string()]);
    }

    #[test]
    fn flags_a_rounded_variant_not_emitted_by_the_tool() {
        // The model re-rounded $172.62B → $172.6B; the tool emitted neither
        // that formatted form nor that exact value, so it's an alteration.
        let answer = "The base is about $172.6B.";
        let v = uncited_numerics(answer, &cited(), &[]);
        assert_eq!(v, vec!["$172.6B".to_string()]);
    }

    #[test]
    fn ignores_years_and_bare_counts() {
        // "2024" and "207792" are bare numbers — not $/% figures.
        let answer = "In 2024, across 207792 parcels, the base was $172.62B at 0.81%.";
        assert!(uncited_numerics(answer, &cited(), &[]).is_empty());
    }

    #[test]
    fn precise_value_traces_to_a_raw_output_even_when_cited_form_is_compact() {
        // The model quoted the exact land_value_total. The cited string
        // only carries the compact $1.48B, but the raw value is in the
        // tool's JSON output — so it IS the engine's number, not invented.
        let cited = vec!["land_value_total = $1.48B [sf-assessor-roll: 874 parcels]".to_string()];
        let raw = vec![1_477_806_471.0_f64];
        let answer = "The total assessed land value is $1,477,806,471.0 (i.e. $1.48B).";
        assert!(
            uncited_numerics(answer, &cited, &raw).is_empty(),
            "precise value should trace to the raw output"
        );
        // Without the raw value, the precise quote looks foreign — proving
        // the raw-output side of the allowed set is load-bearing.
        let v = uncited_numerics(answer, &cited, &[]);
        assert_eq!(v, vec!["$1,477,806,471.0".to_string()]);
    }

    #[test]
    fn spelled_out_magnitude_matches_compact_form() {
        // "$1.4 billion" (prose) and "$1.40B" (tool) are the same value.
        let cited = vec!["business_tax_target = $1.40B [sf-tax-landscape]".to_string()];
        let answer = "retiring the $1.4 billion business tax";
        assert!(uncited_numerics(answer, &cited, &[]).is_empty());
    }

    #[test]
    fn percentage_traces_to_a_rate_stored_as_a_fraction() {
        // The tool stores neutral_rate as the fraction 0.9473500268628882
        // (a raw output); the answer quotes it as a percentage at full
        // precision. That's a faithful relay, not a fabrication.
        let raw = vec![0.947_350_026_862_888_2_f64];
        let answer = "the revenue-neutral rate is 94.73500268628882%";
        assert!(uncited_numerics(answer, &[], &raw).is_empty());
        // And the compact percentage still traces via the cited string.
        let cited = vec!["neutral_rate = 94.74%".to_string()];
        assert!(uncited_numerics("a 94.74% levy", &cited, &[]).is_empty());
    }

    #[test]
    fn no_emitted_figures_means_nothing_to_audit() {
        let answer = "It cost $5.00 and rose 3%.";
        assert!(uncited_numerics(answer, &[], &[]).is_empty());
    }

    #[test]
    fn json_numeric_leaves_collects_nested_numbers() {
        let v = serde_json::json!({
            "land_value_total": 1_477_806_471.0,
            "neutral_rate": 0.9474,
            "counts": [386, 12],
            "label": "ignored string",
            "nested": {"target": 1_400_000_000.0}
        });
        let mut out = Vec::new();
        json_numeric_leaves(&v, &mut out);
        assert!(out.contains(&1_477_806_471.0));
        assert!(out.contains(&0.9474));
        assert!(out.contains(&386.0));
        assert!(out.contains(&12.0));
        assert!(out.contains(&1_400_000_000.0));
        assert_eq!(out.len(), 5, "only numeric leaves, not the string");
    }
}
