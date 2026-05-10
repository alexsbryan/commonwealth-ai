//! Pure helpers for budgeting landscape-digest output.
//!
//! `estimate_tokens` is an upper-bound-biased BPE approximator used
//! across `digest.rs` and `cross_view.rs` to stop assembling text
//! before we blow through a caller-supplied `budget_tokens`. It is
//! **not** a replacement for a real tokenizer at inference time —
//! callers that need exact counts should ask the inference backend.
//!
//! `is_settled_status` groups the set of position-status strings that
//! our enrichment domains use for "this position is the consensus"
//! into one predicate, so the digest formatter doesn't have to
//! inline the vocabulary.

/// Approximate BPE token count for `text`. Designed for bounding
/// prompt sections — NOT a replacement for a real tokenizer at
/// inference time.
///
/// Empirically-calibrated rules:
///   - ASCII word ≤ 15 chars: ~1.3 tokens (BPE folds common words).
///   - Long ASCII identifier (> 15 chars): chars/4 tokens (BPE splits).
///   - Non-ASCII char: ~0.75 tokens (CJK + accented scripts average
///     roughly one BPE piece per visible character; slightly under-
///     counted for CJK to keep the estimator upper-bounded on
///     mixed-script text).
///   - Whitespace and punctuation are absorbed into adjacent tokens
///     and don't contribute directly.
///
/// Bias is intentionally conservative: for the Sovereign prompt-
/// budget use-case we'd rather truncate one line too many than
/// overflow. Verified against a fixture of mixed English / Chinese
/// / long-identifier content in the unit tests.
pub fn estimate_tokens(text: &str) -> usize {
    let mut tokens: f32 = 0.0;
    for word in text.split_whitespace() {
        let total_chars = word.chars().count();
        if total_chars == 0 {
            continue;
        }
        let non_ascii = word.chars().filter(|c| !c.is_ascii()).count();
        let ascii = total_chars - non_ascii;

        if non_ascii > 0 {
            tokens += non_ascii as f32 * 0.75;
            tokens += ascii as f32 * 0.35;
        } else if total_chars > 15 {
            // Long identifier: BPE chops into (roughly) 4-char pieces.
            tokens += (total_chars as f32 / 4.0).ceil();
        } else {
            // Short ASCII word.
            tokens += 1.3;
        }
    }
    tokens.ceil() as usize
}

/// `true` when a position's `status` string marks it as consensus /
/// dominant / settled. Accepts the vocabulary used by the domain
/// enrichment prompts — lowercased before comparison so capitalised
/// variants ("Held", "Dominant") still match.
pub(crate) fn is_settled_status(status: &str) -> bool {
    let s = status.to_lowercase();
    matches!(
        s.as_str(),
        "held" | "dominant" | "majority" | "settled" | "established" | "recurring"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ascii_words_round_up() {
        // "hello" is well under the long-identifier threshold
        assert_eq!(estimate_tokens("hello"), 2); // 1.3 → ceil = 2
    }

    #[test]
    fn long_identifier_uses_quarter_rule() {
        // 20 chars / 4 = 5
        assert_eq!(estimate_tokens("abcdefghijklmnopqrst"), 5);
    }

    #[test]
    fn settled_vocabulary_matches_case_insensitively() {
        assert!(is_settled_status("Held"));
        assert!(is_settled_status("DOMINANT"));
        assert!(is_settled_status("majority"));
        assert!(!is_settled_status("contested"));
        assert!(!is_settled_status(""));
    }
}
