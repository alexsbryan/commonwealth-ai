//! Deterministic numeric-provenance audit — Layer 3 of the "no
//! confabulated numbers" guarantee (SF-LVT demo).
//!
//! Layer 1 is the `parcel_analytics` tool emitting pre-cited figure
//! strings; Layer 2 is the synthesis prompt forbidding un-cited
//! numerics; this is the deterministic backstop: after synthesis, every
//! dollar / percentage figure in the answer must appear (verbatim, after
//! light normalization) among the tool-provided cited figures, else it
//! is flagged.
//!
//! Scoped to `$…` and `…%` tokens — the "figures" the guarantee is
//! about. Bare integers (years, parcel counts, list indices) are NOT
//! audited, so the gate doesn't false-positive on "in 2024" or "207,792
//! parcels". Pure: no I/O, no inference — the unit tests pin it.

/// The dollar / percentage tokens in `answer` not traceable to any
/// `cited` figure. Empty ⇒ every figure is cited (clean).
pub fn uncited_numerics(answer: &str, cited: &[String]) -> Vec<String> {
    // No cited figures ⇒ no audit basis ⇒ nothing to flag. Callers only
    // invoke the audit when a cited-figure tool ran, but keep the pure
    // function safe to call unconditionally (it must not flag every
    // figure in an answer that simply wasn't backed by such a tool).
    if cited.is_empty() {
        return Vec::new();
    }
    let cited_norm: Vec<String> = cited.iter().map(|c| normalize(c)).collect();
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for tok in extract_figures(answer) {
        let n = normalize(&tok);
        if n.is_empty() {
            continue;
        }
        let is_cited = cited_norm.iter().any(|c| c.contains(&n));
        if !is_cited && seen.insert(tok.clone()) {
            out.push(tok);
        }
    }
    out
}

/// Extract `$<number>[B|M|K|T]` and `<number>%` tokens. Bare numbers
/// (no `$` prefix, no `%` suffix) are intentionally skipped.
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
            // Optional magnitude suffix.
            if i < chars.len() && matches!(chars[i], 'B' | 'M' | 'K' | 'T' | 'b' | 'm' | 'k' | 't') {
                i += 1;
            }
            // Require at least one digit after the `$`.
            if i > start + 1 {
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

/// Lowercase, drop `$` and thousands separators / spaces so "$172.62B",
/// "172.62 B", and "172,620,000,000"-style variants compare on their
/// significant content. Trailing-magnitude letters are kept so "$1.4B"
/// and "$1.4M" stay distinct.
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '$' && *c != ',' && !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
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
        assert!(uncited_numerics(answer, &cited()).is_empty());
    }

    #[test]
    fn flags_an_invented_dollar_figure() {
        let answer = "The land base is $172.62B, but improvements add a surprising $999.99B.";
        let v = uncited_numerics(answer, &cited());
        assert_eq!(v, vec!["$999.99B".to_string()]);
    }

    #[test]
    fn flags_a_rounded_variant_not_quoted_verbatim() {
        // The model rounded $172.62B → $172.6B; the guarantee wants verbatim.
        let answer = "The base is about $172.6B.";
        let v = uncited_numerics(answer, &cited());
        assert_eq!(v, vec!["$172.6B".to_string()]);
    }

    #[test]
    fn ignores_years_and_bare_counts() {
        // "2024" and "207792" are bare numbers — not $/% figures.
        let answer = "In 2024, across 207792 parcels, the base was $172.62B at 0.81%.";
        assert!(uncited_numerics(answer, &cited()).is_empty());
    }

    #[test]
    fn no_cited_figures_means_nothing_to_audit() {
        // When no cited-figure tool ran, the gate is a no-op (callers
        // only invoke it when cited figures are present, but be safe).
        let answer = "It cost $5.00 and rose 3%.";
        assert!(uncited_numerics(answer, &[]).is_empty());
    }
}
