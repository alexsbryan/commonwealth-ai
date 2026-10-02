// SPDX-License-Identifier: AGPL-3.0-or-later
//! Derivations over named facts (ratio, change), their formatters, and the
//! question-to-store claim match. Split from `sec_facts/mod.rs` when it moved
//! to this leaf (pb-ingest-dial-tools); re-exported there.

use super::{SecFact, SecFactStore};

/// A quantity computed in Rust over named facts — formula, inputs and
/// result all emitted (§6.2(3)).
#[derive(Debug, Clone)]
pub struct Derived {
    pub value: f64,
    /// `a ÷ b = x` with full-precision inputs — rendered verbatim into
    /// the derivation appendix.
    pub formula: String,
}

/// `numerator ÷ denominator`, as a percentage.
pub fn ratio(num_name: &str, num: &SecFact, den_name: &str, den: &SecFact) -> Option<Derived> {
    if den.value == 0.0 {
        return None;
    }
    let value = num.value / den.value;
    Some(Derived {
        value,
        formula: format!(
            "{num_name} ÷ {den_name} = {} ÷ {} = {}",
            fmt_full(num.value, &num.unit),
            fmt_full(den.value, &den.unit),
            fmt_pct(value)
        ),
    })
}

/// Absolute and percent change from `prior` to `cur`.
pub fn change(name: &str, cur: &SecFact, prior: &SecFact) -> (Derived, Option<Derived>) {
    let delta = cur.value - prior.value;
    let abs = Derived {
        value: delta,
        formula: format!(
            "Δ {name} = {} − {} = {}",
            fmt_full(cur.value, &cur.unit),
            fmt_full(prior.value, &prior.unit),
            fmt_full(delta, &cur.unit)
        ),
    };
    let pct = (prior.value != 0.0).then(|| {
        let p = delta / prior.value;
        Derived {
            value: p,
            formula: format!(
                "Δ% {name} = ({} − {}) ÷ {} = {}",
                fmt_full(cur.value, &cur.unit),
                fmt_full(prior.value, &prior.unit),
                fmt_full(prior.value, &prior.unit),
                fmt_pct(p)
            ),
        }
    });
    (abs, pct)
}

/// Compact figure for cited strings. USD values at millions grain render
/// with an explicit magnitude word so even the DEFAULT numeric-audit
/// scope ($-token + magnitude) can parse them: `$416,161 million`.
pub fn fmt_compact(value: f64, unit: &str) -> String {
    match unit {
        "USD" if value.abs() >= 1_000_000.0 => {
            format!("${} million", group(value / 1_000_000.0, 0))
        }
        "USD" => format!("${}", group(value, 2)),
        u if u.starts_with("USD/") => format!("${}", group(value, 2)),
        "shares" => format!("{} shares", group(value, 0)),
        u => format!("{} {u}", group(value, 4)),
    }
}

/// Full-precision figure for derivation lines.
pub fn fmt_full(value: f64, unit: &str) -> String {
    match unit {
        "USD" => format!("${}", group(value, 2)),
        u if u.starts_with("USD/") => format!("${}", group(value, 2)),
        u => format!("{} {u}", group(value, 4)),
    }
}

/// `0.0830` → `8.30%`.
pub fn fmt_pct(v: f64) -> String {
    format!("{:.2}%", v * 100.0)
}

/// Thousands-grouped decimal with `places` fraction digits (trailing
/// zeros trimmed for places > 2).
fn group(v: f64, places: usize) -> String {
    let neg = v < 0.0;
    let s = format!("{:.*}", places, v.abs());
    let (int_part, frac) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (s, None),
    };
    let mut out = String::new();
    let len = int_part.len();
    for (i, c) in int_part.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    let frac = frac.map(|f| {
        if places > 2 {
            f.trim_end_matches('0').to_string()
        } else {
            f
        }
    });
    let mut out = match frac {
        Some(f) if !f.is_empty() => format!("{out}.{f}"),
        _ => out,
    };
    if neg {
        out = format!("-{out}");
    }
    out
}

/// Does this store claim authority over `question` (§7.3)? Deterministic
/// and enumerable: the question must name the ENTITY (ticker or entity
/// name words) AND at least one concept's `ask_terms` phrase. Matching is
/// word-boundary phrase containment over a normalized form — no
/// embeddings, no threshold. Returns the matched evidence for glassbox
/// logs, or `None`.
///
/// Over-claiming fails safe (the tool refuses naming what IS available),
/// so the vocabulary may be generous; but an entity match is REQUIRED —
/// generic finance wording ("what is gross margin?") never claims.
pub fn store_claims(store: &SecFactStore, question: &str) -> Option<String> {
    let q = normalize_concept_phrase(question);
    // Explanation-shaped questions are OUT of the store's domain: the
    // store is authoritative for FIGURES; "why" answers live in the
    // filing's prose and are best served by the retrieval path with
    // quote verification (measured 2026-08-15: claiming a "why did Mac
    // net sales increase" question pulled it off the DeepQuery path
    // that answered it verbatim, onto a plan whose search step failed).
    if contains_phrase(&q, "why") {
        return None;
    }
    let entity_hit = entity_terms(store)
        .into_iter()
        .find(|t| contains_phrase(&q, t))?;
    for (concept_id, cf) in &store.concepts {
        if let Some(term) = cf
            .ask_terms
            .iter()
            .find(|t| contains_phrase(&q, &normalize_concept_phrase(t)))
        {
            return Some(format!(
                "entity '{entity_hit}' + concept '{concept_id}' term '{term}'"
            ));
        }
    }
    None
}

/// The entity vocabulary: the ticker plus each word of the entity name
/// that is not a corporate suffix ("Apple Inc." → ["aapl", "apple"]).
fn entity_terms(store: &SecFactStore) -> Vec<String> {
    let mut terms = Vec::new();
    if !store.ticker.is_empty() {
        terms.push(store.ticker.to_lowercase());
    }
    for w in normalize_concept_phrase(&store.entity).split_whitespace() {
        if !matches!(
            w,
            "inc" | "corp" | "corporation" | "co" | "ltd" | "plc" | "the"
        ) {
            terms.push(w.to_string());
        }
    }
    terms
}

/// Lowercase, non-alphanumerics to spaces, collapsed. "Apple's" →
/// "apple s", so possessives match the bare entity term.
///
/// PUBLIC because it is the ONE spelling-normalization the concept
/// resolver applies (§10.6: one decider, one name). `sec_facts`'s
/// parameter-vocabulary check must ask "would the resolver accept this
/// spelling?" and it can only answer that by normalizing the SAME way —
/// a second copy in the tools crate would drift the moment either side
/// learned a new rule, and the drift would show up as a concept the
/// schema advertises and the resolver rejects.
pub fn normalize_concept_phrase(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim_end().to_string()
}

/// Word-boundary phrase containment: `phrase` (already normalized)
/// appears in `normalized` as whole words.
pub(super) fn contains_phrase(normalized: &str, phrase: &str) -> bool {
    if phrase.is_empty() {
        return false;
    }
    let padded = format!(" {normalized} ");
    padded.contains(&format!(" {phrase} "))
}
