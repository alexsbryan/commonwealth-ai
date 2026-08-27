// SPDX-License-Identifier: AGPL-3.0-or-later
//! Match primitives and the precision/recall arithmetic.
//!
//! The one place a `PhaseScore` is computed, so every axis and every atom kind
//! counts a hit the same way (§10.6: one decider, one name).

// The eval surface is ONE cooperating unit split for size, not a set of
// independent modules: the golden schema, the snapshot, the match primitives
// and the scorers all name each other's types. `use super::*` keeps that one
// import surface in `mod.rs` rather than duplicating it eight ways.
use super::*;

// ── Match primitives ───────────────────────────────────────────────

pub(super) fn matches_any(haystack: &str, needles: &[String]) -> bool {
    if needles.is_empty() {
        return true; // "no constraint" → trivially satisfied
    }
    let lower = normalize_for_match(haystack);
    // Fast path: case-insensitive substring.
    if needles
        .iter()
        .any(|n| lower.contains(&normalize_for_match(n)))
    {
        return true;
    }
    // Token-presence fallback for multi-token needles. Handles
    // surface-form variance the substring check can't see — e.g.
    // golden's `"hard incompatibilism"` matching corpus's
    // `"incompatibilism (hard)"` (paren reorders the tokens but
    // both tokens are present). Single-token needles fall through
    // (no improvement).
    let haystack_tokens: std::collections::HashSet<String> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    needles.iter().any(|n| {
        let n_norm = normalize_for_match(n);
        let n_tokens: Vec<String> = n_norm
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        n_tokens.len() >= 2 && n_tokens.iter().all(|t| haystack_tokens.contains(t))
    })
}

/// `matches_any` plus a 7-char common-prefix fallback used only
/// for fault-line position-name matching.
///
/// Rationale: golden authors write academic surface forms
/// (`aristotelian`, `situationism`, `sentimentalist`) but the
/// corpus's atom inventory often uses the proponent's name
/// (`Aristotle`) or a related concept (`situational variables`).
/// Plain substring fails — `aristotle` doesn't contain
/// `aristotelian` (the suffix `-elian` versus the proper noun's
/// `-tle` ending diverge at index 7) and `situational` doesn't
/// contain `situationism`. A 7-char common-prefix rule across
/// haystack tokens captures these without admitting the
/// false-positive cases (`polis` vs `police` share only 4 chars,
/// `stoic` vs `stoicism` share 5; both stay below threshold).
///
/// Why 7: empirically threads the needle between
/// `aristotle/aristotelian` (7) and `aristotle/aristocracy` (6).
/// At 6 we'd over-match across academic root families that share
/// a Greek prefix; at 8+ we'd lose the load-bearing
/// philosopher/school bridge. 7 is the smallest threshold
/// preserving the bridge without admitting the family confusions
/// the bench corpora actually contain.
///
/// Scoped to fault-line position matching specifically so the
/// rule's slightly looser stance doesn't propagate into entity /
/// claim / question matching where the strict substring rule has
/// served well.
pub(super) fn matches_any_with_morphology(haystack: &str, needles: &[String]) -> bool {
    if matches_any(haystack, needles) {
        return true;
    }
    if needles.is_empty() {
        return false;
    }
    const MIN_PREFIX: usize = 7;
    let h_lower = normalize_for_match(haystack);
    let h_tokens: Vec<String> = h_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= MIN_PREFIX)
        .map(str::to_string)
        .collect();
    if h_tokens.is_empty() {
        return false;
    }
    needles.iter().any(|n| {
        let n_lower = normalize_for_match(n);
        // Restrict to single-token needles ≥ MIN_PREFIX chars long;
        // multi-token needles already get the token-presence path.
        if n_lower.len() < MIN_PREFIX || n_lower.chars().any(|c| !c.is_alphanumeric()) {
            return false;
        }
        h_tokens.iter().any(|t| {
            let common: usize = n_lower
                .chars()
                .zip(t.chars())
                .take_while(|(a, b)| a == b)
                .count();
            common >= MIN_PREFIX
        })
    })
}

/// Lowercase + fold the four common Unicode "smart" punctuation marks
/// to ASCII so that golden keywords like `O'Rourke` (typed in
/// straight ASCII) match the actual atom name `O'Rourke` (Project
/// Gutenberg / Word-style curly apostrophe). Keeps everything else
/// unchanged. Without this fold, every passage of curly-quoted prose
/// silently fails substring matching against ASCII goldens.
pub(super) fn normalize_for_match(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' => '\'',
            '\u{201C}' | '\u{201D}' => '"',
            other => other,
        })
        .collect::<String>()
        .to_ascii_lowercase()
}

pub(super) fn any_match_in_list<'a, I, F>(items: I, needles: &[String], extract: F) -> bool
where
    I: IntoIterator<Item = &'a String>,
    F: Fn(&str) -> bool,
    String: 'a,
{
    let _ = extract; // marker — kept for clarity
    items.into_iter().any(|s| matches_any(s, needles))
}

// ── Scoring ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct PhaseScore {
    pub expected: usize,
    pub matched: usize,
    pub forbidden_total: usize,
    pub forbidden_hit: usize,
    /// Per-expected hit list — names pulled from the golden's
    /// `*_contains_any` field (first entry by convention) so the
    /// report's miss column is human-readable.
    pub misses: Vec<String>,
    pub forbidden_hits: Vec<String>,
    pub notes: Vec<String>,
    /// Total candidate artefacts the scorer saw for this axis — the
    /// extraction VOLUME. `#[serde(default)]` keeps pre-P0.2 baselines
    /// deserializable (they read as 0 candidates → rate `None`).
    #[serde(default)]
    pub candidates: usize,
    /// Candidates explained by NO expected entry and NO forbidden
    /// entry: extraction volume that earns zero credit and, before
    /// P0.2, carried zero cost. The adjudication sampler
    /// (`bench enrichment-adjudicate`) prices how much of it is junk.
    #[serde(default)]
    pub unmatched_count: usize,
    /// Up to [`UNMATCHED_SAMPLE_CAP`] labels of unmatched candidates,
    /// for the human report. The full set is recomputed on demand by
    /// the adjudicator; this is a preview, not the record.
    #[serde(default)]
    pub unmatched_samples: Vec<String>,
}

impl PhaseScore {
    /// Precision = TP / (TP + FP). When the model emitted zero atoms
    /// AND the golden expected zero atoms, precision is undefined and
    /// the phase is genuinely silent (`None`). When the golden expected
    /// atoms but the model produced none, precision is treated as 0.0
    /// — otherwise zero-recall failures fall out of `f1()` as `None`
    /// and never enter the aggregate, hiding the regression. This bit
    /// is the difference between "no scoreable artefacts" (a silent
    /// phase) and "tried and failed" (a recall=0 phase).
    pub(crate) fn precision(&self) -> Option<f32> {
        let denom = self.matched + self.forbidden_hit;
        if denom == 0 {
            // Two sub-cases:
            //  - expected == 0 → genuinely undefined, stay silent
            //  - expected > 0  → zero-recall failure, return 0.0 so
            //    f1() lands in the aggregate
            if self.expected == 0 {
                return None;
            }
            return Some(0.0);
        }
        Some(self.matched as f32 / denom as f32)
    }

    pub(crate) fn recall(&self) -> Option<f32> {
        if self.expected == 0 {
            return None;
        }
        Some(self.matched as f32 / self.expected as f32)
    }

    pub(crate) fn f1(&self) -> Option<f32> {
        let p = self.precision()?;
        let r = self.recall()?;
        if p + r == 0.0 {
            return Some(0.0);
        }
        Some(2.0 * p * r / (p + r))
    }

    /// Fraction of the axis's candidate pool no golden entry explains.
    /// `None` when the pool is empty (nothing extracted ≠ over-
    /// extraction). Deliberately NOT folded into precision: the :30
    /// forbidden-only FP contract stays for baseline compat; this is
    /// the parallel volume signal.
    pub(crate) fn unmatched_rate(&self) -> Option<f32> {
        if self.candidates == 0 {
            return None;
        }
        Some(self.unmatched_count as f32 / self.candidates as f32)
    }
}

/// Cap on unmatched sample labels serialized per axis. Keeps report
/// JSON (and the lane baselines that embed it) bounded on corpora
/// where extraction volume dwarfs the golden.
pub(super) const UNMATCHED_SAMPLE_CAP: usize = 10;

/// Char-boundary-safe label truncation for unmatched samples.
pub(super) fn truncate_label(s: &str) -> String {
    const MAX: usize = 80;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let cut: String = s.chars().take(MAX).collect();
    format!("{cut}…")
}

/// Candidate-centric second pass: count candidates no golden entry
/// (expected OR forbidden) explains. Runs after the expected-centric
/// loops so it never perturbs the existing match/miss/FP accounting.
pub(super) fn tally_unmatched<T>(
    s: &mut PhaseScore,
    candidates: &[T],
    label: impl Fn(&T) -> String,
    explained: impl Fn(&T) -> bool,
) {
    s.candidates = candidates.len();
    for c in candidates {
        if !explained(c) {
            s.unmatched_count += 1;
            if s.unmatched_samples.len() < UNMATCHED_SAMPLE_CAP {
                s.unmatched_samples.push(truncate_label(&label(c)));
            }
        }
    }
}
