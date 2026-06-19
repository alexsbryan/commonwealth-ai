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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gold_match_is_and_and_case_insensitive() {
        let kws = vec!["verloc".to_string(), "brett street".to_string()];
        assert!(gold_match("The Verloc shop on BRETT STREET", &kws));
        assert!(!gold_match("The Verloc shop", &kws), "missing one keyword fails the AND");
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
        assert!(gold_match("the steed bolted", &kws), "synonym 'steed' satisfies the group");
        assert!(gold_match("a HORSE and cart", &kws), "case-insensitive primary form");
        assert!(gold_match("the cab horse", &kws), "multi-word alternate");
        assert!(!gold_match("the dog ran", &kws), "no alternate present → fail");
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
        assert!(gold_match("Verloc rode the steed", &kws), "both entries satisfied");
        assert!(!gold_match("Verloc walked", &kws), "second entry unsatisfied → AND fails");
    }

    #[test]
    fn plain_entry_is_byte_identical_to_old_behaviour() {
        // A keyword with no `|` matches exactly as before — backward-compatible.
        assert!(gold_match("the Verloc shop", &["verloc".to_string()]));
        assert!(!gold_match("the shop", &["verloc".to_string()]));
    }
}
