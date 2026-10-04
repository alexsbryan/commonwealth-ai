//! Title/label text matching for citation attribution.
//!
//! Pulled out of `citation_attribution.rs` (848 lines, ARCH §3.1's approach
//! band) because these are the module's only pure text predicates: they take
//! strings and return strings, booleans or scores, and know nothing about
//! citations, brackets or verdicts. `judge_title`'s decision procedure stays
//! with the attribution logic; the string work it leans on lives here.
//!
//! Everything is `pub(super)` — this is a private helper module, not a
//! surface, and the thresholds it reads stay owned by the parent (§10.6:
//! one decider per threshold).

use super::{ID_TOKEN_MIN_LEN, SNAP_FLOOR, SNAP_MARGIN};

/// The unique label the cited title is a garbled copy of, if any: best similarity
/// ≥ `SNAP_FLOOR` and unambiguous (runner-up below the floor or beaten by
/// `SNAP_MARGIN`). Returns the label's ORIGINAL text — the snap restores the real
/// header, not a normalization of it.
pub(super) fn snap_to_label(nt: &str, labels: &[(String, String)]) -> Option<String> {
    let mut best: Option<(f32, &str)> = None;
    let mut second = 0.0f32;
    for (orig, norm) in labels {
        let s = char_similarity(nt, norm);
        match best {
            Some((bs, _)) if s <= bs => second = second.max(s),
            _ => {
                if let Some((bs, _)) = best {
                    second = second.max(bs);
                }
                best = Some((s, orig.as_str()));
            }
        }
    }
    let (bs, orig) = best?;
    (bs >= SNAP_FLOOR && bs - second >= SNAP_MARGIN).then(|| orig.to_string())
}

/// Normalized Levenshtein similarity over chars: 1.0 = identical, 0.0 = disjoint.
fn char_similarity(a: &str, b: &str) -> f32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let max = a.len().max(b.len());
    if max == 0 {
        return 0.0;
    }
    let mut dp: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = dp[0];
        dp[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = dp[j + 1];
            dp[j + 1] = (dp[j + 1] + 1)
                .min(dp[j] + 1)
                .min(prev + usize::from(ca != cb));
            prev = cur;
        }
    }
    1.0 - dp[b.len()] as f32 / max as f32
}

/// ID-shaped: long enough to be an identifier and carrying at least one digit.
pub(super) fn id_shaped(w: &str) -> bool {
    w.chars().count() >= ID_TOKEN_MIN_LEN && w.chars().any(|c| c.is_ascii_digit())
}

/// Maximal `digits(-digits)+` runs of ≥8 chars with ≥6 digits — dates
/// ("2026-10-10") and hyphenated numeric ids. These are identifiers even though
/// each hyphen-separated fragment is too short for `id_shaped`.
pub(super) fn hyphen_digit_runs(nt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in nt.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() || c == '-' {
            cur.push(c);
        } else {
            let run = cur.trim_matches('-');
            if run.len() >= 8
                && run.contains('-')
                && run.chars().filter(|c| c.is_ascii_digit()).count() >= 6
            {
                out.push(run.to_string());
            }
            cur.clear();
        }
    }
    out
}

/// Whether `needle` occurs in `hay` bounded by non-alphanumerics — the complete-run
/// rule from the gate's exact-value fix: `2894942` inside `28949423` is NOT a
/// match. Used for both single ID tokens and whole title phrases. Both sides are
/// already lowercase.
pub(super) fn hay_contains_bounded(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    for (i, m) in hay.match_indices(needle) {
        let left_ok = hay[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let right_ok = hay[i + m.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        if left_ok && right_ok {
            return true;
        }
    }
    false
}

/// The distinctive content words of a title: ≥2 chars, not a function/honorific
/// word, not email-reply noise (`re`, `fwd`) or the literal `source`. Lowercased.
pub(super) fn significant_words(title: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "mr",
        "mrs",
        "miss",
        "ms",
        "the",
        "of",
        "a",
        "an",
        "and",
        "sir",
        "dr",
        "comrade",
        "chief",
        "inspector",
        "lady",
        "lord",
        "saint",
        "st",
        "re",
        "fwd",
        "fw",
        "source",
        "for",
        "to",
        "in",
        "on",
        "at",
        "by",
        "is",
        "was",
    ];
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2 && !STOP.contains(&w.to_lowercase().as_str()))
        .map(|w| w.to_lowercase())
        .collect()
}

/// Lowercase and collapse runs of whitespace — the same normalisation the other
/// presence checks use so a title's words match regardless of spacing.
pub(super) fn normalize(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
