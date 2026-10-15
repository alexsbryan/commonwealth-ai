// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic grounding checks — the GR1 "source-grounded verification"
//! kernel shared by the chaos-monkey live adapter and the flywheel verifier.
//!
//! These are the *witness* checks: given a probe's gold witness (keywords,
//! supporting / distractor signatures) they decide groundedness against the
//! agent's answer and the retrieved passages — no model opinion, no inference.
//! Lifted out of the `sovereign-cli-llm` chaos orchestrator so there is one
//! implementation both the live bench and the flywheel score against.

/// Case-insensitive substring containment.
pub fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// AND-match with OR-groups: every gold entry must be satisfied (AND across the
/// list), and an entry is satisfied if ANY of its `|`-separated alternates
/// appears (case-insensitive substring; OR within the entry). A plain entry
/// (no `|`) is byte-identical to the old single-keyword behaviour — fully
/// backward-compatible, no existing bank changes meaning.
///
/// The OR-groups exist so a probe can accept genuinely-equivalent surface forms
/// ("horse|steed|cab horse", "Winnie|Mrs Verloc") instead of scoring a correct
/// synonym as wrong. These are CORRECT-ANSWER alternates ONLY — widening what
/// counts as correct, never model-coaching vocabulary (see
/// `feedback_no_teaching_to_test`). An empty keyword set vacuously matches —
/// callers gate on the witness being non-empty (the fairness contract).
pub fn gold_match(answer: &str, keywords: &[String]) -> bool {
    let low = answer.to_lowercase();
    keywords.iter().all(|entry| {
        if entry.contains('|') {
            entry
                .split('|')
                .map(str::trim)
                .filter(|alt| !alt.is_empty())
                .any(|alt| low.contains(&alt.to_lowercase()))
        } else {
            // No `|` → exactly the historical single-keyword path.
            low.contains(&entry.to_lowercase())
        }
    })
}

/// Port of the production value-presence primitive — the corruption-site
/// check for constructed Stream B cases (VERIFIER_V0.md §3: "each corruption
/// is mechanically checkable at the known corruption site").
///
/// SOURCE OF TRUTH: `sovereign-core/src/runtime/grounding/value_presence.rs`
/// (`value_present_in_chunks`). Replicated byte-for-byte in logic because the
/// flywheel is deliberately pure (serde + std + rand; no sovereign-core), and
/// the Stream B export path re-validates every case against the REAL
/// production checker where sovereign-core is available — this port gates
/// generation, the original gates export. Keep the two in sync; the parity
/// tests below pin the behaviours corruption-site checking depends on.
///
/// **Synced 2026-09-04 with the veto conversion.** The source dropped its
/// role-word stop list when presence stopped deciding groundedness and became
/// a veto, so this port drops it in the same commit — a port that keeps a
/// rule the original deleted is two implementations of one predicate
/// disagreeing (ARCH §10.6), and the export path would then abort on cases
/// this generator had just certified.
pub fn value_present(value: &str, chunks: &[String]) -> bool {
    if let Some(want) = numeric_value(value) {
        return numbers_in(&chunks.join(" ")).contains(&want);
    }
    let hay: String = chunks
        .join(" ")
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // A multi-word value that appears VERBATIM (a contiguous phrase) in the
    // evidence is grounded by definition. Gated on ≥2 words so a bare
    // honorific cannot self-ground (see the source-of-truth rationale).
    let nval: String = value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if nval.split(' ').filter(|w| !w.is_empty()).count() >= 2 && hay.contains(&nval) {
        return true;
    }
    let sig: Vec<String> = value
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(|w| w.to_lowercase())
        .collect();
    !sig.is_empty() && sig.iter().all(|w| hay.contains(w.as_str()))
}

// Numeric presence (port of grounding/number_presence.rs; keep in step).
// Canonical form of every number in `s`: thousands commas dropped, leading
/// zeros and trailing fractional zeros trimmed ("3.20" == "3.2" == "03.2").
fn numbers_in(s: &str) -> Vec<String> {
    scan(s).into_iter().map(|(_, _, c)| c).collect()
}

/// `Some(canonical)` when `value` is purely a number with optional sign,
/// currency sign and percent marker; `None` leaves it to the word path.
fn numeric_value(value: &str) -> Option<String> {
    let lower = value.trim().to_lowercase();
    let s = lower.trim_start_matches(|c: char| {
        matches!(c, '+' | '-' | '\u{2212}' | '$' | '€' | '£') || c.is_whitespace()
    });
    let s = s.trim_end();
    let s = ["percent", "per cent", "per-cent", "%"]
        .iter()
        .find_map(|m| s.strip_suffix(m))
        .unwrap_or(s)
        .trim_end();
    match scan(s).as_slice() {
        [(0, end, canon)] if *end == s.len() => Some(canon.clone()),
        _ => None,
    }
}

/// `(start, end, canonical)` per number. A comma joins only before exactly
/// three digits; a dot only before a digit and once.
fn scan(s: &str) -> Vec<(usize, usize, String)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        let (mut int, mut frac, mut dot) = (String::new(), String::new(), false);
        while i < b.len() {
            let c = b[i];
            let three = |j: usize| {
                b.len() >= j + 4
                    && b[j + 1..j + 4].iter().all(u8::is_ascii_digit)
                    && !b.get(j + 4).is_some_and(u8::is_ascii_digit)
            };
            if c.is_ascii_digit() {
                if dot {
                    frac.push(c as char)
                } else {
                    int.push(c as char)
                }
            } else if c == b',' && !dot && three(i) {
            } else if c == b'.' && !dot && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                dot = true;
            } else {
                break;
            }
            i += 1;
        }
        let int = int.trim_start_matches('0');
        let frac = frac.trim_end_matches('0');
        let int = if int.is_empty() { "0" } else { int };
        let canon = if frac.is_empty() {
            int.to_string()
        } else {
            format!("{int}.{frac}")
        };
        out.push((start, i, canon));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gold_match_is_and_and_case_insensitive() {
        let kws = vec!["verloc".to_string(), "brett street".to_string()];
        assert!(gold_match("The Verloc shop on BRETT STREET", &kws));
        assert!(
            !gold_match("The Verloc shop", &kws),
            "missing one keyword fails the AND"
        );
    }

    #[test]
    fn empty_keywords_vacuously_match() {
        assert!(gold_match("anything", &[]));
    }

    #[test]
    fn contains_ci_folds_case() {
        assert!(contains_ci("Mr VLADIMIR of the Embassy", "vladimir"));
        assert!(!contains_ci("the Professor", "ossipon"));
    }

    #[test]
    fn or_group_accepts_any_alternate() {
        let kws = vec!["horse|steed|cab horse".to_string()];
        assert!(
            gold_match("the steed bolted", &kws),
            "synonym 'steed' satisfies the group"
        );
        assert!(
            gold_match("a HORSE and cart", &kws),
            "case-insensitive primary form"
        );
        assert!(gold_match("the cab horse", &kws), "multi-word alternate");
        assert!(
            !gold_match("the dog ran", &kws),
            "no alternate present → fail"
        );
    }

    #[test]
    fn or_group_trims_whitespace_around_pipes() {
        // Authoring ergonomics: spaces around the delimiter are tolerated.
        let kws = vec!["Winnie | Mrs Verloc".to_string()];
        assert!(gold_match("she is called Mrs Verloc", &kws));
        assert!(gold_match("Winnie went out", &kws));
    }

    #[test]
    fn or_groups_and_combine_across_entries() {
        // AND across entries, OR within each.
        let kws = vec!["verloc".to_string(), "horse|steed".to_string()];
        assert!(
            gold_match("Verloc rode the steed", &kws),
            "both entries satisfied"
        );
        assert!(
            !gold_match("Verloc walked", &kws),
            "second entry unsatisfied → AND fails"
        );
    }

    #[test]
    fn plain_entry_is_byte_identical_to_old_behaviour() {
        // A keyword with no `|` matches exactly as before — backward-compatible.
        assert!(gold_match("the Verloc shop", &["verloc".to_string()]));
        assert!(!gold_match("the shop", &["verloc".to_string()]));
    }

    // ---- value_present parity pins (vs sovereign-core value_present_in_chunks) ----

    fn ev(s: &str) -> Vec<String> {
        vec![s.to_string()]
    }

    #[test]
    fn value_present_multiword_verbatim_phrase_grounds() {
        // The ≥2-word verbatim path: a contiguous phrase in the evidence is
        // grounded by definition, even when every word is individually stoppy.
        assert!(value_present(
            "Chief Inspector",
            &ev("Chief Inspector Heat arrived")
        ));
    }

    /// The stop list is gone, so a bare honorific is PRESENT — and that is
    /// the veto's honest answer: "Mr" really is in the text. It grounds
    /// nothing, because presence no longer grounds anything; the source's
    /// probe decides. Pinned so this port and the source keep saying the same
    /// thing about the same string.
    #[test]
    fn value_present_reads_a_bare_honorific_literally() {
        assert!(value_present("Mr", &ev("Mr Verloc kept a shop")));
        assert!(!value_present("Mr", &ev("Verloc kept a shop")));
    }

    #[test]
    fn value_present_and_matches_significant_words() {
        let e = ev("the lock sluice had been opened at half past eleven");
        assert!(value_present("sluice opened at eleven", &e));
        assert!(
            !value_present("sluice opened at nine", &e),
            "one absent significant word fails the AND"
        );
    }

    #[test]
    fn value_present_garbled_form_is_absent() {
        // The OCR-garble site check: the garbled surface must NOT ground.
        let e = ev("the lock sluice had been opened");
        assert!(value_present("lock", &e));
        assert!(!value_present("1ock", &e));
    }

    #[test]
    fn value_present_finds_a_number_by_value_like_production() {
        let one = |t: &str| vec![t.to_string()];
        assert!(value_present("3.2%", &one("It grew 3.2 per cent.")));
        assert!(value_present("3.2%", &one("It grew 3.2%.")));
        assert!(!value_present("4.12%", &one("only 12 here")));
        assert!(!value_present("4.12%", &one("room 412 here")));
        assert!(value_present("3 million", &one("about 3 million people")));
    }
}
