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

/// AND-match: every gold keyword must appear in the answer (case-insensitive).
/// An empty keyword set vacuously matches — callers gate on the witness being
/// non-empty (the fairness contract guarantees answerable probes carry one).
pub fn gold_match(answer: &str, keywords: &[String]) -> bool {
    let low = answer.to_lowercase();
    keywords.iter().all(|k| low.contains(&k.to_lowercase()))
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
}
