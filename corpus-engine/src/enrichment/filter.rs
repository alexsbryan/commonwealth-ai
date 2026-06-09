// SPDX-License-Identifier: AGPL-3.0-or-later
//! Chunk eligibility filter — runs in microseconds, zero inference cost.
//!
//! `is_chunk_eligible()` decides whether a chunk is worth sending to the
//! claim-extraction model.  Filtering 35–50 % of chunks before inference
//! saves the same fraction of wall-clock time in Phase 1.

/// Return `true` if `passage` is worth sending to the claim-extraction model.
///
/// Checks are ordered cheapest-first and short-circuit on the first failure.
///
/// # Arguments
/// * `passage`         — raw chunk text.
/// * `section_heading` — optional section title (e.g. the chunk's `title` field).
pub fn is_chunk_eligible(passage: &str, section_heading: Option<&str>) -> bool {
    // 1. Section heading check — skip known non-substantive sections.
    if let Some(heading) = section_heading {
        let lc = heading.to_lowercase();
        const SKIP_HEADINGS: &[&str] = &[
            "bibliography",
            "references",
            "further reading",
            "related entries",
            "academic tools",
        ];
        if SKIP_HEADINGS.iter().any(|h| lc.contains(h)) {
            return false;
        }
    }

    // 2. Length check — too short to contain substantive claims.
    let word_count = passage.split_whitespace().count();
    if word_count < 80 {
        return false;
    }

    // 3. Fictional scenario markers.
    let lc_passage = passage.to_lowercase();
    const FICTIONAL_PHRASES: &[&str] = &[
        "suppose that",
        "imagine that",
        "consider a case",
        "assume that",
        "let's say",
        "take as given",
    ];
    if FICTIONAL_PHRASES.iter().any(|p| lc_passage.contains(p)) {
        return false;
    }

    // 4. Named fictional agents — space-padded to avoid false positives on
    //    "Timothy", "harmless", "Smithsonian", etc.
    const FICTIONAL_NAMES: &[&str] = &[" tim ", " harry ", " mary ", " jones ", " smith "];
    // Pad the lowercased passage on both sides so names at the very start or
    // end of the text are also caught by the space-pad check.
    let padded = format!(" {} ", lc_passage);
    if FICTIONAL_NAMES.iter().any(|n| padded.contains(n)) {
        return false;
    }

    // 5. Bibliographic density — more than 40 % of non-empty lines contain a
    //    year-in-parentheses pattern like "(1984)" or "(2023)".
    {
        let non_empty_lines: Vec<&str> = passage.lines().filter(|l| !l.trim().is_empty()).collect();
        if !non_empty_lines.is_empty() {
            let bib_lines = non_empty_lines
                .iter()
                .filter(|line| has_year_in_parens(line))
                .count();
            if bib_lines as f32 / non_empty_lines.len() as f32 > 0.40 {
                return false;
            }
        }
    }

    // 6. Logical formalisation density — too many logic symbols relative to
    //    word count suggests a passage of formal derivations, not prose claims.
    {
        const LOGIC_SYMBOLS: &[char] = &['∀', '∃', '→', '↔', '¬'];
        let symbol_count = passage
            .chars()
            .filter(|c| LOGIC_SYMBOLS.contains(c))
            .count();
        // Threshold: more than 1 symbol per 30 words.
        if symbol_count as f32 / word_count as f32 > 1.0 / 30.0 {
            return false;
        }
    }

    true
}

/// Return `true` if `line` contains a 4-digit year inside parentheses,
/// e.g. "(1984)" or "(2023)".
fn has_year_in_parens(line: &str) -> bool {
    let bytes = line.as_bytes();
    let len = bytes.len();
    if len < 6 {
        return false;
    }
    // Scan for '(' followed by 4 ASCII digits followed by ')'.
    for i in 0..len.saturating_sub(5) {
        if bytes[i] == b'('
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
            && bytes[i + 3].is_ascii_digit()
            && bytes[i + 4].is_ascii_digit()
            && bytes[i + 5] == b')'
        {
            return true;
        }
    }
    false
}

// ─── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn long_passage() -> String {
        // 100 words of philosophical prose — should pass all filters.
        "Aristotle's virtue ethics grounds moral evaluation in character rather than \
         consequences or duties. The virtuous agent acts from stable dispositions acquired \
         through habituation, not merely in conformity with rules. Practical wisdom \
         (phronesis) plays a central role: the person of good character perceives the \
         salient features of a situation and responds appropriately. This stands in \
         contrast to Kantian deontology, which grounds rightness in universalisable \
         maxims, and to consequentialism, which evaluates acts solely by outcomes. \
         Contemporary virtue ethicists argue that the Aristotelian framework better \
         captures moral psychology and the role of emotion in ethical life. Debates \
         persist over whether virtue ethics can generate action guidance comparable \
         to rival frameworks, or whether that demand itself misunderstands the nature \
         of ethical reasoning."
            .to_string()
    }

    #[test]
    fn passes_good_chunk() {
        assert!(is_chunk_eligible(&long_passage(), Some("Virtue Ethics")));
    }

    #[test]
    fn blocks_bibliography_heading() {
        assert!(!is_chunk_eligible(&long_passage(), Some("Bibliography")));
        assert!(!is_chunk_eligible(&long_passage(), Some("References")));
        assert!(!is_chunk_eligible(&long_passage(), Some("Further Reading")));
        assert!(!is_chunk_eligible(&long_passage(), Some("Related Entries")));
        assert!(!is_chunk_eligible(&long_passage(), Some("Academic Tools")));
    }

    #[test]
    fn blocks_short_chunk() {
        // Only a few words — under 80.
        assert!(!is_chunk_eligible("This is far too short.", None));
    }

    #[test]
    fn blocks_fictional_phrase() {
        let passage = format!("Suppose that {}", &long_passage()[..300]);
        assert!(!is_chunk_eligible(&passage, None));

        let passage2 = format!("Imagine that an agent {}", &long_passage()[..300]);
        assert!(!is_chunk_eligible(&passage2, None));

        let passage3 = format!("Let's say we have {}", &long_passage()[..300]);
        assert!(!is_chunk_eligible(&passage3, None));
    }

    #[test]
    fn blocks_fictional_name() {
        // " tim " as a standalone word.
        let passage = format!("As Tim argued, {}", &long_passage());
        assert!(!is_chunk_eligible(&passage, None));

        let passage2 = format!("Harry and Mary disagree. {}", &long_passage());
        assert!(!is_chunk_eligible(&passage2, None));
    }

    #[test]
    fn does_not_block_on_partial_name() {
        // "Timothy" should not trigger the " tim " check.
        let passage = format!("Timothy Williamson argues that {}", &long_passage());
        assert!(is_chunk_eligible(&passage, None));
    }

    #[test]
    fn blocks_bib_heavy_passage() {
        // Construct a passage where >40 % of lines have year-in-parens.
        let bib = "Armstrong, D. M. (1983). What is a Law of Nature? Cambridge UP.\n\
                   Hempel, C. G. (1965). Aspects of Scientific Explanation. Free Press.\n\
                   Lewis, D. (1973). Counterfactuals. Blackwell.\n\
                   van Fraassen, B. (1980). The Scientific Image. Oxford UP.\n\
                   Tooley, M. (1977). The nature of laws. Canadian Journal of Philosophy.\n\
                   Dretske, F. (1977). Laws of nature. Philosophy of Science.";
        // All 6 lines have years — 100 % > 40 %.
        assert!(!is_chunk_eligible(bib, None));
    }

    #[test]
    fn blocks_logic_heavy_passage() {
        // word count ≈ 20, but 3 symbols → 3/20 = 0.15 > 1/30 ≈ 0.033
        // We need enough words to reach the 80-word minimum first, so we pad
        // with prose words and then saturate with symbols.
        let base = "For all x there exists a y such that if P then Q and not R. ".repeat(5);
        let symbols = "∀x ∃y (P(x) → Q(y)) ↔ ¬R(x) ".repeat(10);
        let passage = format!("{base}{symbols}");
        // word count should be well above 80 but symbol density high.
        assert!(!is_chunk_eligible(&passage, None));
    }
}
