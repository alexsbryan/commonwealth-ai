// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic numeric-provenance audit — Layer 3 of the "no
//! confabulated numbers" guarantee (SF-LVT demo; extended for the SEC
//! financial corpora, spec `sovereign/docs/specs/FINANCIAL_CORPORA.md` §6).
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
//! **Default scope** (`uncited_numerics`): `$…` and `…%` tokens only. Bare
//! integers (years, parcel counts, list indices) are NOT audited, so the
//! gate doesn't false-positive on "in 2024" or "874 parcels". This is the
//! behaviour every general turn gets, unchanged.
//!
//! **Opt-in bare scope** (`uncited_numerics_including_bare`): financial
//! answers are full of bare figures — `416,161` (millions), EPS `7.46` —
//! that the default scope cannot audit (FINANCIAL_CORPORA §6.3). A
//! figure-emitting tool whose figures may be bare declares the opt-in per
//! turn (see `handlers/complex_task.rs` harvest of the `numeric_audit`
//! step-output key); ONLY those turns audit bare numerals. The allowed set
//! is extended with tool-declared tokens so period components and
//! accession digits (`2024-09-29`, `0000320193-25-000079`) trace instead
//! of flagging. Within bare scope, plain integers of 3 or fewer digits are
//! still never audited (day-of-month, small counts) — a material financial
//! figure carries 4+ digits, a thousands separator, a decimal point, or a
//! magnitude word.
//!
//! Glassbox: every numeral considered emits a `numeric_audit`-target debug
//! event with its traceability verdict and the allowed-set member it
//! matched. Pure otherwise: no I/O, no inference — unit-tested.

use std::collections::HashSet;

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
    audit(answer, cited, raw_values, None)
}

/// Opt-in bare-numeral audit (FINANCIAL_CORPORA §6.3(b)). Same contract
/// as [`uncited_numerics`] plus: bare numeric tokens (4+ digits, or a
/// separator / decimal / magnitude word), ISO dates, and SEC accession
/// numbers in `answer` must also trace. `allowed_tokens` is the tool's
/// declared traceable-token set (its periods, fiscal years, accession,
/// filed dates, and every numeral in its own emitted text — see
/// [`numeric_tokens`] for the mechanical way a tool builds it).
///
/// Unlike the default scope this does NOT early-return when `cited` and
/// `raw_values` are empty: the opt-in itself is the audit basis. A
/// refusal turn emits no figures, and precisely there a model reciting a
/// figure from pretraining must be flagged — an empty allowed set means
/// every audited numeral in the answer is unattributable.
pub fn uncited_numerics_including_bare(
    answer: &str,
    cited: &[String],
    raw_values: &[f64],
    allowed_tokens: &[String],
) -> Vec<String> {
    audit(answer, cited, raw_values, Some(allowed_tokens))
}

/// Every numeric token in `s` at bare-audit scope — `$…`, `…%`, bare
/// figures, ISO dates, accessions. The mechanical way a figure-emitting
/// tool builds its `allowed_tokens` declaration: run this over every text
/// field it emits (cited figures, summary, derivation, refusal reason),
/// so "allowed" is *by construction* "the tool itself said it".
pub fn numeric_tokens(s: &str) -> Vec<String> {
    extract(s, true).into_iter().map(|t| t.text).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokKind {
    /// `$1.48B`, `$416,161 million` — always audited.
    Dollar,
    /// `94.74%` — always audited.
    Percent,
    /// `416,161`, `7.46`, `2024`, `416,161 million` — audited only at
    /// bare scope.
    Bare,
    /// A composite identifier: ISO date (`2024-09-29`) or SEC accession
    /// (`0000320193-25-000079`). Audited only at bare scope, by string
    /// membership in the allowed-token set (never by numeric value).
    Identifier,
}

struct Tok {
    text: String,
    kind: TokKind,
}

/// Canonical form for string membership: thousands separators stripped,
/// trailing sentence punctuation trimmed.
fn canon(s: &str) -> String {
    s.trim()
        .trim_end_matches(['.', ','])
        .replace(',', "")
        .to_string()
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

fn is_accession(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20
        && b[10] == b'-'
        && b[13] == b'-'
        && b.iter().enumerate().all(|(i, c)| {
            if i == 10 || i == 13 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

/// The shared audit core — ONE decider for both scopes (ARCH §10.6).
/// `bare: None` reproduces the historical `$`/`%`-only behaviour exactly;
/// `bare: Some(tokens)` widens extraction and the allowed set.
fn audit(
    answer: &str,
    cited: &[String],
    raw_values: &[f64],
    bare: Option<&[String]>,
) -> Vec<String> {
    let include_bare = bare.is_some();

    // Allowed values: the formatted figures the tool cited (parsed back to
    // their numeric value) ∪ the tool's raw numeric outputs. At bare scope
    // the cited strings are re-read at bare scope too — a tool-authored
    // `416,161 million` is an allowed value by construction.
    let mut allowed: Vec<f64> = Vec::new();
    for c in cited {
        for tok in extract(c, include_bare) {
            if tok.kind == TokKind::Identifier {
                continue;
            }
            if let Some(v) = parse_value(&tok.text) {
                allowed.push(v);
            }
        }
    }
    allowed.extend_from_slice(raw_values);

    // Allowed strings (bare scope only): tool-declared tokens plus every
    // numeric token of the tool's own cited strings. A date token also
    // admits its year component — "fiscal 2024" is an honest relay of a
    // fact whose period starts 2024-09-29.
    let mut allowed_strings: HashSet<String> = HashSet::new();
    if let Some(tokens) = bare {
        let declared = tokens.iter().map(String::as_str);
        let from_cited: Vec<String> = cited
            .iter()
            .flat_map(|c| extract(c, true))
            .map(|t| t.text)
            .collect();
        for t in declared.chain(from_cited.iter().map(String::as_str)) {
            let c = canon(t);
            if is_iso_date(&c) {
                allowed_strings.insert(c[..4].to_string());
            } else if !is_accession(&c) {
                // A declared plain token is also an allowed VALUE, so
                // "$2,000" traces to a declared "2,000" and vice versa.
                if let Some(v) = parse_value(&c) {
                    allowed.push(v);
                }
            }
            allowed_strings.insert(c);
        }
    }

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for tok in extract(answer, include_bare) {
        let (traceable, matched): (bool, Option<String>) = match tok.kind {
            TokKind::Identifier => {
                let c = canon(&tok.text);
                if allowed_strings.contains(&c) {
                    (true, Some(format!("token:{c}")))
                } else {
                    (false, None)
                }
            }
            _ => {
                let Some(v) = parse_value(&tok.text) else {
                    continue;
                };
                // A percentage may faithfully relay a rate the tool stored
                // as a fraction (`0.9474` ↔ `94.74%`); accept either
                // reading so quoting the rate as a percent isn't mistaken
                // for a fabrication.
                let is_pct = tok.kind == TokKind::Percent;
                let by_value = allowed
                    .iter()
                    .find(|a| values_match(**a, v))
                    .copied()
                    .or_else(|| {
                        if is_pct {
                            allowed
                                .iter()
                                .find(|a| values_match(**a, v / 100.0))
                                .copied()
                        } else {
                            None
                        }
                    });
                if let Some(a) = by_value {
                    (true, Some(format!("value:{a}")))
                } else if include_bare && allowed_strings.contains(&canon(&tok.text)) {
                    (true, Some(format!("token:{}", canon(&tok.text))))
                } else {
                    (false, None)
                }
            }
        };
        tracing::debug!(
            target: "numeric_audit",
            token = %tok.text,
            kind = ?tok.kind,
            traceable,
            matched = matched.as_deref().unwrap_or("-"),
            "numeric_audit: token verdict"
        );
        if !traceable && seen.insert(tok.text.clone()) {
            out.push(tok.text);
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

/// Extract audited tokens from `s`. With `include_bare = false` this is
/// the historical scope — `$<number>[<magnitude>]` and `<number>%` only,
/// bare numbers intentionally skipped. With `include_bare = true` it also
/// yields: SEC accession numbers and ISO dates (as single `Identifier`
/// tokens), and bare numeric runs that look like figures (4+ digits, a
/// comma group, an interior decimal point, or a magnitude word). Plain
/// integers of 1-3 digits are never yielded — that is the
/// false-positive guard for "874 parcels" and day-of-month components.
fn extract(s: &str, include_bare: bool) -> Vec<Tok> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '$' {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_digit() || chars[i] == ',' || chars[i] == '.')
            {
                i += 1;
            }
            // Require at least one digit after the `$`.
            if i > start + 1 {
                i = consume_magnitude(&chars, i);
                out.push(Tok {
                    text: chars[start..i].iter().collect(),
                    kind: TokKind::Dollar,
                });
            }
        } else if c.is_ascii_digit() {
            let start = i;
            if include_bare {
                if let Some(end) = match_composite(&chars, i, &[10, 2, 6]) {
                    out.push(Tok {
                        text: chars[i..end].iter().collect(),
                        kind: TokKind::Identifier,
                    });
                    i = end;
                    continue;
                }
                if let Some(end) = match_composite(&chars, i, &[4, 2, 2]) {
                    out.push(Tok {
                        text: chars[i..end].iter().collect(),
                        kind: TokKind::Identifier,
                    });
                    i = end;
                    continue;
                }
            }
            while i < chars.len()
                && (chars[i].is_ascii_digit() || chars[i] == ',' || chars[i] == '.')
            {
                i += 1;
            }
            if i < chars.len() && chars[i] == '%' {
                i += 1;
                out.push(Tok {
                    text: chars[start..i].iter().collect(),
                    kind: TokKind::Percent,
                });
            } else if include_bare {
                let body: String = chars[start..i].iter().collect();
                let trimmed = body.trim_end_matches(['.', ',']);
                let digits = trimmed.chars().filter(|c| c.is_ascii_digit()).count();
                let has_comma = trimmed.contains(',');
                let has_decimal = trimmed.contains('.');
                let after_magnitude = consume_magnitude(&chars, i);
                let has_magnitude = after_magnitude > i;
                if digits >= 4 || has_comma || has_decimal || has_magnitude {
                    i = after_magnitude;
                    // Bare tokens are trimmed of sentence punctuation so
                    // "7.46." parses and reports as "7.46". ($/% token
                    // text stays byte-identical to the historical scope.)
                    let text: String = chars[start..i].iter().collect();
                    let text = if has_magnitude {
                        text
                    } else {
                        text.trim_end_matches(['.', ',']).to_string()
                    };
                    out.push(Tok {
                        text,
                        kind: TokKind::Bare,
                    });
                }
                // else: 1-3 digit plain integer — never a guarded figure.
            }
            // else: bare number (year / count) — not a guarded figure.
        } else {
            i += 1;
        }
    }
    out
}

/// Match a dash-joined digit composite (`groups` = digits per group,
/// e.g. `[4, 2, 2]` for an ISO date) starting at `i`. Returns the end
/// index only when every group matches exactly and the composite is not
/// embedded in a longer digit run.
fn match_composite(chars: &[char], i: usize, groups: &[usize]) -> Option<usize> {
    let mut j = i;
    for (gi, &g) in groups.iter().enumerate() {
        if gi > 0 {
            if j >= chars.len() || chars[j] != '-' {
                return None;
            }
            j += 1;
        }
        let gs = j;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        if j - gs != g {
            return None;
        }
    }
    Some(j)
}

/// Advance `i` past a magnitude suffix following a `$<number>`: a single
/// letter (`B`/`M`/`K`/`T`, not part of a longer word) or a spelled-out
/// word after at most one space. Returns the unchanged index if none.
fn consume_magnitude(chars: &[char], i: usize) -> usize {
    // Single-letter suffix (e.g. `$1.48B`), but only if it's not the start
    // of a longer alphabetic word (so we don't eat the `B` of "Bay").
    if i < chars.len() && matches!(chars[i], 'B' | 'M' | 'K' | 'T' | 'b' | 'm' | 'k' | 't') {
        let next_is_alpha = chars
            .get(i + 1)
            .map(|c| c.is_ascii_alphabetic())
            .unwrap_or(false);
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
    let word: String = chars[word_start..j]
        .iter()
        .collect::<String>()
        .to_lowercase();
    if matches!(
        word.as_str(),
        "billion" | "million" | "thousand" | "trillion" | "bn"
    ) {
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
        let answer =
            "A flat land levy of 0.81% on the $172.62B base replaces the $1.40B business tax.";
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

    // ── bare-numeral opt-in scope (FINANCIAL_CORPORA §6.3(b)) ────────────

    #[test]
    fn bare_fabricated_figure_is_caught() {
        // The failing input, by name (ARCH §18.1): Services revenue is a
        // dimensional figure the fact store cannot carry; a model reciting
        // it from pretraining emits a bare "109,158 million" no tool
        // produced.
        let raw = vec![416_161_000_000.0_f64, 416_161.0];
        let v = uncited_numerics_including_bare(
            "Services revenue was 109,158 million in fiscal 2025.",
            &[],
            &raw,
            &["2025".to_string()],
        );
        assert_eq!(v, vec!["109,158 million".to_string()]);
    }

    #[test]
    fn ordinary_prose_year_is_not_flagged_without_the_opt_in() {
        // The other direction of the gate (ARCH §18.1): "in 2024" in an
        // ordinary answer stays exactly as unaudited as before — the bare
        // scope is opt-in per turn, never the default.
        let answer = "In 2024 the project moved to a new office at 874 Main St.";
        assert!(uncited_numerics(answer, &cited(), &[]).is_empty());
    }

    #[test]
    fn millions_convention_bare_figure_traces_to_raw_values() {
        // The tool emits both scalings (416161 in millions, raw USD); an
        // answer may faithfully relay either "416,161 million" (magnitude
        // word → raw USD) or the plain "416,161" (the in-millions figure).
        let raw = vec![416_161_000_000.0_f64, 416_161.0];
        assert!(uncited_numerics_including_bare(
            "Net sales were 416,161 million, i.e. 416,161 in millions.",
            &[],
            &raw,
            &[],
        )
        .is_empty());
    }

    #[test]
    fn eps_decimal_traces_and_an_altered_eps_is_flagged() {
        let raw = vec![7.46_f64];
        assert!(
            uncited_numerics_including_bare("Diluted EPS was 7.46.", &[], &raw, &[]).is_empty()
        );
        let v = uncited_numerics_including_bare("Diluted EPS was 7.99.", &[], &raw, &[]);
        assert_eq!(v, vec!["7.99".to_string()]);
    }

    #[test]
    fn dates_years_and_accessions_trace_via_allowed_tokens() {
        // Period components and accession digits appear legitimately in a
        // grounded financial answer; the tool declares them and they trace.
        let allowed = vec![
            "2024-09-29".to_string(),
            "2025-09-27".to_string(),
            "0000320193-25-000079".to_string(),
            "2025".to_string(),
        ];
        let answer = "For FY2025 (2024-09-29 to 2025-09-27), per accession \
                      0000320193-25-000079 — note the period started in 2024.";
        assert!(
            uncited_numerics_including_bare(answer, &[], &[], &allowed).is_empty(),
            "declared dates, their year components, and the accession all trace"
        );
    }

    #[test]
    fn a_fabricated_accession_is_flagged() {
        let allowed = vec!["0000320193-25-000079".to_string()];
        let v = uncited_numerics_including_bare(
            "Reported under accession 0000320193-25-000080.",
            &[],
            &[],
            &allowed,
        );
        assert_eq!(v, vec!["0000320193-25-000080".to_string()]);
    }

    #[test]
    fn small_bare_integers_are_never_audited_even_at_bare_scope() {
        // 1-3 digit plain integers (counts, day-of-month) stay out of
        // scope; a comma-grouped figure of the same magnitude is IN scope
        // — "1,234" is figure-shaped, "874" is not.
        assert!(uncited_numerics_including_bare(
            "3 of the 24 concepts across 874 parcels, filed on the 27th.",
            &[],
            &[],
            &[],
        )
        .is_empty());
        let v = uncited_numerics_including_bare("It rose by 1,234.", &[], &[], &[]);
        assert_eq!(v, vec!["1,234".to_string()]);
    }

    #[test]
    fn refusal_turn_with_empty_allowed_set_flags_any_recited_figure() {
        // The opt-in is the audit basis: on a refusal turn the tool emitted
        // no figures, so a numeral the model volunteers cannot trace.
        let v = uncited_numerics_including_bare(
            "No typed fact exists, but revenue was roughly $391 billion.",
            &[],
            &[],
            &[],
        );
        assert_eq!(v, vec!["$391 billion".to_string()]);
    }

    #[test]
    fn numeric_tokens_extracts_the_tools_own_emissions() {
        let toks = numeric_tokens(
            "revenue (FY2025) = $416,161 million [10-K 0000320193-25-000079 \
             filed 2025-10-31; period 2024-09-29..2025-09-27]",
        );
        assert!(toks.contains(&"$416,161 million".to_string()));
        assert!(toks.contains(&"2025".to_string())); // from FY2025
        assert!(toks.contains(&"0000320193-25-000079".to_string()));
        assert!(toks.contains(&"2025-10-31".to_string()));
        assert!(toks.contains(&"2024-09-29".to_string()));
        assert!(toks.contains(&"2025-09-27".to_string()));
    }

    #[test]
    fn tokens_from_cited_strings_are_allowed_at_bare_scope() {
        // The tool's own cited text is allowed by construction: the date
        // and year it printed trace without a separate declaration.
        let cited =
            vec!["net_income (FY2025) = $112,010 million [10-K filed 2025-10-31]".to_string()];
        assert!(uncited_numerics_including_bare(
            "Net income was $112,010 million, filed 2025-10-31 (fiscal 2025).",
            &cited,
            &[],
            &[],
        )
        .is_empty());
    }
}
