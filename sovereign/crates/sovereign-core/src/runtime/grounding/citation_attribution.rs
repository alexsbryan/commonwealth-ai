// SPDX-License-Identifier: AGPL-3.0-or-later
//! Citation-attribution: the SUPPORTING-specifics half of groundedness.
//!
//! `value_presence` checks the answer's top-line VALUE; this checks the answer's
//! `[Source: title]` CITATIONS. The 1h faithfulness audit (2026-06-30) found the
//! gate's blind spot: a grounded top-line value propped up by FABRICATED example
//! citations — `[Source: Re: Advertising Campaign - NASCAR]` over an Enron corpus
//! that never mentions NASCAR. The value check passes (the thesis — "a diverse
//! corpus, no single key fact" — IS grounded); the four invented supporting
//! citations sail straight through. A reader takes them as real retrieved content.
//!
//! Scope is deliberately NARROW — explicit `[Source: …]` markers ONLY, never every
//! noun. `value_presence.rs` documents why: checking every mentioned specific
//! exploded the false-positive rate 0.09 -> 0.40 (it swept in the model's framing —
//! author, year, section headings — none of which are corpus-world claims). A
//! `[Source: X]` marker is categorically different: it is a STRUCTURED, intentional
//! claim ("a retrieved passage titled X exists"), and the synthesis prompt itself
//! (`prompts.rs`) tells the model that `title` must match a `[Source: …]` header
//! from the passages. So a title whose distinctive words appear nowhere in the
//! evidence is a fabricated attribution by the prompt's own contract.
//!
//! Deterministic, no LLM. Each cited title runs an ORDERED decision (stop at the
//! first match):
//!
//! 1. Exact match against a real source label → keep.
//! 2. Verbatim multi-word phrase from the evidence body (a real in-body header,
//!    e.g. "4.16 Architectural correctness tooling") → keep.
//! 3. SNAP: char-similar to exactly one real label → rewrite the citation to that
//!    label. The chaos rebaseline (2026-07-01, steps 21/105) showed the model
//!    cannot reliably copy opaque hash ids: it cited seven corruptions of the one
//!    real corpus id `watched-959ee8a8f330` (`watched-959ee8a67210`, …). Every
//!    observed garble sits at 0.80–0.95 Levenshtein similarity to the true label
//!    while fabricated titles measure ≤ 0.53 against any label — so a unique
//!    near-miss is a garbled COPY of a real source, and correcting it preserves
//!    the citation instead of destroying a genuine attribution.
//! 4. ID-token VETO: a title carrying an ID-shaped token (≥6 chars with a digit)
//!    that matches no COMPLETE token in the evidence is stripped. Same principle
//!    as the gate's exact-value fix (`quote_has_number_token`): identifiers match
//!    completely or not at all. Without this, a garbled hash passes the word
//!    floor below at exactly 0.5 — the shared prefix ("watched") carries half the
//!    weight and the corruption ships.
//! 5. Word floor: measure the fraction of the title's significant words present
//!    in the evidence; strip when the majority are absent. A reformatted-but-real
//!    title ("Federalist 51 (Madison)" for the header "Federalist No. 51") keeps
//!    most of its words and is released; an invented one ("Advertising Campaign -
//!    NASCAR") keeps about none and is stripped.
//!
//! Snap or strip, don't gate — a false strip removes one marker, it does not
//! refuse a good answer; that asymmetry is why the floor is forgiving (0.5). The
//! per-answer fabrication rate is reported for the glassbox so a
//! confabulation-heavy answer is visible to telemetry and the desktop panel even
//! though we don't gate on it yet.

/// Below this fraction of a title's significant words present in the evidence, the
/// citation is treated as a fabricated attribution. 0.5 = "more invented than
/// grounded". Forgiving on purpose: the cost of a miss (a bogus marker survives) is
/// far lower than a false strip on a real, paraphrased title. A single-word title
/// misquote (`Commonwealth Spaces` for `Common Spaces`, 6/7 words still present) is
/// below this threshold BY DESIGN — catching it would require flagging any absent
/// word, which over-strips legitimately reworded headers.
const SUPPORT_FLOOR: f32 = 0.5;

/// A title needs at least this many significant words to be judged. A one-word
/// title (`[Source: Wikipedia]`) is too coarse to match reliably — keep it rather
/// than risk a false strip.
const MIN_TITLE_WORDS: usize = 2;

/// A cited title at least this char-similar to a real source label is a garbled
/// copy of that label — snap it. Calibrated on the chaos rebaseline: all seven
/// observed hash-id garbles measure 0.80–0.95 against the label they corrupted;
/// fabricated / unrelated titles measure ≤ 0.53 against every label. 0.75 leaves
/// a wide margin on both sides.
const SNAP_FLOOR: f32 = 0.75;

/// A snap must be UNAMBIGUOUS: the best label must beat the runner-up by this
/// margin, UNCONDITIONALLY. Near-twin labels ("Decision — 2026-03-28 — Guest
/// Parking" vs "… — 2026-04-02 — Porch Smoking") separate by ≈0.3, so 0.10 is
/// conservative. An ambiguous near-miss falls through — the veto/floor decide,
/// rather than risk snapping to the wrong source. (An earlier "runner-up below
/// the floor" escape hatch let a barely-over-floor best snap out of a crowded
/// label FAMILY: "Maple House Charter, Articles II–XI" — an aggregate range
/// citation — measured 0.763 vs "Article VI — Pets" with the runner-up article
/// at 0.738, and was wrongly rewritten to the Pets article. The shared family
/// prefix inflates full-string similarity; only the margin catches it.)
const SNAP_MARGIN: f32 = 0.10;

/// A significant word this long that carries a digit is ID-shaped (hash ids,
/// record numbers) and must match a COMPLETE token in the evidence — partial
/// matches are how truncated/garbled identifiers masquerade as grounded. Short
/// digit words ("51", "2001") stay under the plain word rule.
const ID_TOKEN_MIN_LEN: usize = 6;

/// The result of auditing an answer's citations against its evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationAttribution {
    /// The answer with unverifiable `[Source: …]` markers removed and garbled
    /// ones snapped to the real label.
    pub cleaned: String,
    /// Total individual `Source:` citations seen across the answer.
    pub citations_total: usize,
    /// The titles that were stripped (absent from the evidence).
    pub stripped_titles: Vec<String>,
    /// Citations rewritten to the real label they garbled: `(cited, snapped-to)`.
    pub snapped_titles: Vec<(String, String)>,
}

impl CitationAttribution {
    /// How many citations were stripped.
    pub fn citations_stripped(&self) -> usize {
        self.stripped_titles.len()
    }
    /// How many citations were snapped to the real label they garbled.
    pub fn citations_snapped(&self) -> usize {
        self.snapped_titles.len()
    }
    /// Did the audit change the answer?
    pub fn changed(&self) -> bool {
        !self.stripped_titles.is_empty() || !self.snapped_titles.is_empty()
    }
    /// Fraction of the answer's citations that were fabricated. 0.0 when the answer
    /// carried no citations. A high rate is the signal that the whole answer is
    /// confabulated padding (the gate may escalate on it later); today it is
    /// reported, not acted on.
    pub fn fabrication_rate(&self) -> f32 {
        if self.citations_total == 0 {
            0.0
        } else {
            self.citations_stripped() as f32 / self.citations_total as f32
        }
    }
}

/// Strip `[Source: title]` markers whose title is absent from BOTH the evidence
/// body (`chunks`) AND the source labels (`labels` — the chunk titles and corpus
/// ids the synthesis presents as `[Source: …]` headers). The labels are
/// load-bearing: a legitimate citation routinely names the source by its title or
/// corpus name ("institutional-notes", "Decision — 2026-03-28 — Guest Parking"),
/// which lives in the header/metadata, NOT the body — so matching body-only
/// false-positive-strips real citations. Labels only WIDEN what counts as grounded.
/// The gate's entry point — it cleans the held answer before release. Pure and
/// deterministic; UTF-8-safe (operates on chars, the citation titles routinely
/// carry em-dashes and smart quotes).
pub fn attribute_citations(
    answer: &str,
    chunks: &[String],
    labels: &[String],
) -> CitationAttribution {
    let mut raw = chunks.join(" ");
    if !labels.is_empty() {
        raw.push(' ');
        raw.push_str(&labels.join(" "));
    }
    let hay = normalize(&raw);
    // Distinct labels as (original, normalized) — the snap targets. Dedup so the
    // corpus id repeated once per chunk doesn't compete with itself in the
    // uniqueness check.
    let mut seen = std::collections::HashSet::new();
    let label_set: Vec<(String, String)> = labels
        .iter()
        .filter_map(|l| {
            let n = normalize(l);
            (!n.is_empty() && seen.insert(n.clone())).then(|| (l.trim().to_string(), n))
        })
        .collect();
    let chars: Vec<char> = answer.chars().collect();
    let mut out = String::with_capacity(answer.len());
    let mut citations_total = 0usize;
    let mut stripped_titles: Vec<String> = Vec::new();
    let mut snapped_titles: Vec<(String, String)> = Vec::new();
    // An UNCLOSED `[Source:` (the model truncates brackets) must not swallow
    // everything up to some ']' hundreds of chars later in unrelated text —
    // title.rs's scanner caps its marker for the same reason. A real citation
    // bracket is short and single-line; past either bound the '[' is literal.
    const MAX_BRACKET_CHARS: usize = 300;
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '[' {
            if let Some(rel) = chars[i + 1..].iter().position(|&c| c == ']') {
                let end = i + 1 + rel; // absolute index of ']'
                let inner: String = chars[i + 1..end].iter().collect();
                if inner.trim_start().to_lowercase().starts_with("source:")
                    && rel <= MAX_BRACKET_CHARS
                    && !inner.contains('\n')
                {
                    let (rebuilt, total, stripped, snapped) =
                        process_bracket(&inner, &hay, &label_set);
                    citations_total += total;
                    stripped_titles.extend(stripped);
                    snapped_titles.extend(snapped);
                    if rebuilt.is_empty() {
                        // Whole bracket removed — consume one preceding space so
                        // "claim [Source: X]." collapses to "claim." not "claim .".
                        if out.ends_with(' ') {
                            out.pop();
                        }
                    } else {
                        out.push('[');
                        out.push_str(&rebuilt);
                        out.push(']');
                    }
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    CitationAttribution { cleaned: out, citations_total, stripped_titles, snapped_titles }
}

/// Process one `[…]` whose inner text begins with `Source:`. Splits on `;` into
/// individual citations, keeps the verifiable ones (snapping garbled label copies
/// to the real label), and returns the rebuilt inner text (empty = drop the whole
/// bracket), the count of citations seen, the titles stripped, and the
/// `(cited, snapped-to)` rewrites.
fn process_bracket(
    inner: &str,
    hay: &str,
    labels: &[(String, String)],
) -> (String, usize, Vec<String>, Vec<(String, String)>) {
    let mut kept: Vec<String> = Vec::new();
    let mut total = 0usize;
    let mut stripped: Vec<String> = Vec::new();
    let mut snapped: Vec<(String, String)> = Vec::new();
    for seg in inner.split(';') {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        total += 1;
        let title = strip_source_prefix(seg);
        match judge_title(title, hay, labels) {
            TitleVerdict::Keep => kept.push(format!("Source: {title}")),
            TitleVerdict::Snap(label) => {
                kept.push(format!("Source: {label}"));
                snapped.push((title.to_string(), label));
            }
            TitleVerdict::Strip => stripped.push(title.to_string()),
        }
    }
    (kept.join("; "), total, stripped, snapped)
}

/// Drop a leading case-insensitive `Source:` from a citation segment, returning the
/// bare title. A segment without the prefix (the `B` in `[Source: A; B]`) is its
/// own title.
fn strip_source_prefix(seg: &str) -> &str {
    let low = seg.to_lowercase();
    if low.starts_with("source:") {
        seg["source:".len()..].trim()
    } else {
        seg
    }
}

/// The per-title verdict: keep as cited, rewrite to a real label, or strip.
enum TitleVerdict {
    Keep,
    Snap(String),
    Strip,
}

/// The ordered decision procedure documented at the top of the module: exact
/// label → verbatim body phrase → snap-to-label → ID-token veto → word floor.
fn judge_title(title: &str, hay: &str, labels: &[(String, String)]) -> TitleVerdict {
    let nt = normalize(title);
    // 1. The citation names a real source label verbatim.
    if labels.iter().any(|(_, ln)| *ln == nt) {
        return TitleVerdict::Keep;
    }
    // 1b. A trailing parenthetical QUALIFIER minted onto a real label —
    //     "[Source: Wikipedia (contested)]" over the label "wikipedia"
    //     (observed twice) — is the label plus editorializing. Snap to the
    //     exact base label; the qualifier is not part of any source's name.
    if let Some((base, _)) = nt.rsplit_once('(') {
        let base = base.trim();
        if !base.is_empty() {
            if let Some((orig, _)) = labels.iter().find(|(_, ln)| ln == base) {
                return TitleVerdict::Snap(orig.clone());
            }
        }
    }
    // 2. A multi-word title present VERBATIM as a contiguous phrase is grounded
    //    by definition — it is literally a header in the evidence. Bounded, not
    //    substring: "Record 2894942" must not match inside "Record 28949423".
    if nt.split(' ').filter(|w| !w.is_empty()).count() >= 2 && hay_contains_bounded(hay, &nt) {
        return TitleVerdict::Keep;
    }
    // 3. A unique near-miss of one real label is a garbled copy — correct it.
    if let Some(label) = snap_to_label(&nt, labels) {
        return TitleVerdict::Snap(label);
    }
    // 4. An ID-shaped token that matches no complete evidence token is a
    //    corrupted or invented identifier — the word floor must not see it.
    //    Composite hyphen-digit runs ("2026-10-10") are checked WHOLE: a
    //    garbled date splits into fragments too short for the word-level rule
    //    ("2026","10") that all pass the floor individually.
    let sig = significant_words(title);
    if sig.iter().any(|w| id_shaped(w) && !hay_contains_bounded(hay, w)) {
        return TitleVerdict::Strip;
    }
    if hyphen_digit_runs(&nt).iter().any(|run| !hay_contains_bounded(hay, run)) {
        return TitleVerdict::Strip;
    }
    // 5. Word floor.
    if sig.len() < MIN_TITLE_WORDS {
        return TitleVerdict::Keep; // too coarse to judge — keep
    }
    let present = sig.iter().filter(|w| hay.contains(w.as_str())).count();
    if (present as f32 / sig.len() as f32) >= SUPPORT_FLOOR {
        TitleVerdict::Keep
    } else {
        TitleVerdict::Strip
    }
}

/// The unique label the cited title is a garbled copy of, if any: best similarity
/// ≥ `SNAP_FLOOR` and unambiguous (runner-up below the floor or beaten by
/// `SNAP_MARGIN`). Returns the label's ORIGINAL text — the snap restores the real
/// header, not a normalization of it.
fn snap_to_label(nt: &str, labels: &[(String, String)]) -> Option<String> {
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
            dp[j + 1] = (dp[j + 1] + 1).min(dp[j] + 1).min(prev + usize::from(ca != cb));
            prev = cur;
        }
    }
    1.0 - dp[b.len()] as f32 / max as f32
}

/// ID-shaped: long enough to be an identifier and carrying at least one digit.
fn id_shaped(w: &str) -> bool {
    w.chars().count() >= ID_TOKEN_MIN_LEN && w.chars().any(|c| c.is_ascii_digit())
}

/// Maximal `digits(-digits)+` runs of ≥8 chars with ≥6 digits — dates
/// ("2026-10-10") and hyphenated numeric ids. These are identifiers even though
/// each hyphen-separated fragment is too short for `id_shaped`.
fn hyphen_digit_runs(nt: &str) -> Vec<String> {
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
fn hay_contains_bounded(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    for (i, m) in hay.match_indices(needle) {
        let left_ok = hay[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let right_ok = hay[i + m.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        if left_ok && right_ok {
            return true;
        }
    }
    false
}

/// The distinctive content words of a title: ≥2 chars, not a function/honorific
/// word, not email-reply noise (`re`, `fwd`) or the literal `source`. Lowercased.
fn significant_words(title: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "mr", "mrs", "miss", "ms", "the", "of", "a", "an", "and", "sir", "dr",
        "comrade", "chief", "inspector", "lady", "lord", "saint", "st", "re",
        "fwd", "fw", "source", "for", "to", "in", "on", "at", "by", "is", "was",
    ];
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2 && !STOP.contains(&w.to_lowercase().as_str()))
        .map(|w| w.to_lowercase())
        .collect()
}

/// Lowercase and collapse runs of whitespace — the same normalisation the other
/// presence checks use so a title's words match regardless of spacing.
fn normalize(s: &str) -> String {
    s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real enron-sample-tiny evidence from the audit's turn #4 (the six
    /// distinct email threads), trimmed to what the titles need to match against.
    fn enron() -> Vec<String> {
        vec![
            "Re: EnronOnline Executive Summary for April 23, 2001. From Simone La rose.".into(),
            "OK, Jeff, you requested that we be candid about Enron. Rosalee.".into(),
            "Re: Cornell. We would need access to a number of people in the Enron \
             organization. Kodak's new Commercial Group."
                .into(),
            "Enron OnLine. Did you get the wedding pics?".into(),
            "Re: Good-bye. Amy Lee to Kenneth Lay, Jeff Skilling, Rosalee Fleming.".into(),
        ]
    }

    #[test]
    fn strips_the_fabricated_enron_citations() {
        // The four invented [Source:] titles from turn #4 — NASCAR, Aspen, IAEE,
        // BusinessWeek — none of whose distinctive words appear in the evidence.
        let answer = "NASCAR sponsorship [Source: Re: Advertising Campaign - NASCAR] and \
                      Aspen [Source: Re: Materials for Aspen ISIB's Business Leaders Dialogue] \
                      and a keynote [Source: Re: Invitation to deliver 2001 IAEE conference keynote].";
        let r = attribute_citations(answer, &enron(), &[]);
        assert_eq!(r.citations_total, 3);
        assert_eq!(r.citations_stripped(), 3);
        assert!(!r.cleaned.contains("Source:"));
        // The prose claims remain (we strip the marker, not the sentence) but the
        // false attribution is gone.
        assert!(r.cleaned.contains("NASCAR sponsorship"));
        assert!((r.fabrication_rate() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn keeps_the_real_enron_citations() {
        // Cornell and Enron OnLine ARE in the evidence — must not be touched.
        let answer = "Cornell engagements [Source: Re: Cornell] and wedding photos [Source: Enron OnLine].";
        let r = attribute_citations(answer, &enron(), &[]);
        assert_eq!(r.citations_total, 2);
        assert_eq!(r.citations_stripped(), 0);
        assert!(!r.changed());
        assert_eq!(r.cleaned, answer); // byte-identical — no false strip
    }

    #[test]
    fn faithful_codebase_citation_survives() {
        // Turn #0 (faithful): the cited section header IS the evidence header.
        let chunks = vec![
            "4.16 Architectural correctness tooling. Five tools that audit \
             narrative-vs-code drift against ARCH_PRINCIPLES.md."
                .to_string(),
        ];
        let answer = "It orchestrates eight primitives [Source: 4.16 Architectural correctness tooling].";
        let r = attribute_citations(answer, &chunks, &[]);
        assert_eq!(r.citations_stripped(), 0);
        assert_eq!(r.cleaned, answer);
    }

    #[test]
    fn mixed_bracket_keeps_real_drops_fabricated() {
        // A single bracket citing one real and one invented source.
        let answer = "see both [Source: Re: Cornell; Source: Re: Advertising Campaign - NASCAR].";
        let r = attribute_citations(answer, &enron(), &[]);
        assert_eq!(r.citations_total, 2);
        assert_eq!(r.citations_stripped(), 1);
        assert_eq!(r.stripped_titles, vec!["Re: Advertising Campaign - NASCAR"]);
        assert!(r.cleaned.contains("[Source: Re: Cornell]"));
        assert!(!r.cleaned.contains("NASCAR]")); // the marker, not the prose "NASCAR"
    }

    #[test]
    fn whole_bracket_removal_cleans_preceding_space() {
        let answer = "He invented this entirely [Source: Re: Advertising Campaign - NASCAR].";
        let r = attribute_citations(answer, &enron(), &[]);
        // The leading space before the dropped bracket is consumed.
        assert_eq!(r.cleaned, "He invented this entirely.");
    }

    #[test]
    fn no_citations_is_untouched() {
        let answer = "A plain answer with no source markers at all.";
        let r = attribute_citations(answer, &enron(), &[]);
        assert_eq!(r.citations_total, 0);
        assert_eq!(r.fabrication_rate(), 0.0);
        assert_eq!(r.cleaned, answer);
    }

    #[test]
    fn single_word_title_is_kept_conservatively() {
        // Below MIN_TITLE_WORDS — too coarse to judge, so keep even if absent.
        let answer = "general [Source: Wikipedia].";
        let r = attribute_citations(answer, &enron(), &[]);
        assert_eq!(r.citations_stripped(), 0);
    }

    #[test]
    fn reformatted_real_title_is_kept() {
        // A real header cited with extra/reordered words: most words still present
        // => above the floor => released (no over-strip on legitimate rewording).
        let chunks = vec!["Federalist No. 51, by James Madison, on checks and balances.".to_string()];
        let answer = "the structure [Source: Federalist 51 (Madison)].";
        let r = attribute_citations(answer, &chunks, &[]);
        assert_eq!(r.citations_stripped(), 0);
    }

    #[test]
    fn utf8_title_with_em_dash_is_safe() {
        // Smart punctuation in titles must not panic the char scan, and a wholly
        // invented title is still stripped.
        let chunks = vec!["Maple House Charter, Article IV — Common Spaces.".to_string()];
        let answer = "rule [Source: Re: Café — Niño's Zürich Exposé Bösewicht].";
        let r = attribute_citations(answer, &chunks, &[]);
        assert_eq!(r.citations_stripped(), 1);
        assert!(!r.cleaned.contains("Source:"));
    }

    // ── label-matching: the false positives the live run surfaced (2026-06-30) ──

    #[test]
    fn corpus_name_citation_kept_via_label() {
        // Live FP #1: "what's most important in the institutional-notes material"
        // → the model cites [Source: institutional-notes] (the CORPUS NAME). The
        // body is about cmd_design/run_stopgap and never says "institutional";
        // body-only matching wrongly stripped it. The corpus id is a valid label.
        let body = vec![
            "cmd_design MVP (step 4) intentionally defers the embedded stopgap \
             streaming chat loop — run_stopgap prints a placeholder."
                .to_string(),
        ];
        let labels = vec!["institutional-notes".to_string()];
        let answer = "It defers the stopgap loop [Source: institutional-notes].";
        let r = attribute_citations(answer, &body, &labels);
        assert_eq!(r.citations_stripped(), 0, "corpus-name citation must survive");
        assert_eq!(r.cleaned, answer);
    }

    #[test]
    fn section_title_citation_kept_via_label() {
        // Live FP #2: a governance section cited by its TITLE. The title lives in
        // the chunk header (a label), not necessarily the body the gate sees.
        let body = vec![
            "To settle confusion about where visitors leave their cars, the house \
             set aside two marked spaces for guests."
                .to_string(),
        ];
        let labels = vec!["Decision — 2026-03-28 — Guest Parking".to_string()];
        let answer = "Guests park in the two marked spaces \
                      [Source: Decision — 2026-03-28 — Guest Parking].";
        let r = attribute_citations(answer, &body, &labels);
        assert_eq!(r.citations_stripped(), 0, "section-title citation must survive");
    }

    #[test]
    fn fabrication_stripped_despite_real_labels() {
        // The true positive must still fire: a wholly invented title matches
        // NEITHER the body NOR any real source label.
        let body = vec!["We would need access to a number of people.".to_string()];
        let labels = vec!["Re: Cornell".to_string(), "enron-sample-tiny".to_string()];
        let answer = "NASCAR talks [Source: Re: Advertising Campaign - NASCAR] and \
                      Cornell [Source: Re: Cornell].";
        let r = attribute_citations(answer, &body, &labels);
        assert_eq!(r.citations_stripped(), 1);
        assert_eq!(r.stripped_titles, vec!["Re: Advertising Campaign - NASCAR"]);
        assert!(r.cleaned.contains("[Source: Re: Cornell]"));
    }

    // ── snap + ID-token veto: the garbled-hash-id class (chaos rebaseline
    //    2026-07-01, steps 21/105 — 7 corruptions of one real corpus id) ──

    /// The watched-corpus turn: the corpus id is a LABEL (it never appears in the
    /// chunk bodies), and the body legitimately contains the word "watched".
    fn watched_labels() -> Vec<String> {
        vec!["watched-959ee8a8f330".to_string()]
    }
    fn watched_body() -> Vec<String> {
        vec![
            "Because if you never land, you never see what's underneath you. The \
             watched folder mirrors notes as they change."
                .to_string(),
        ]
    }

    #[test]
    fn garbled_hash_id_citation_snaps_to_the_true_label() {
        let answer = "the fox speaks [Source: watched-959ee8a67210].";
        let r = attribute_citations(answer, &watched_body(), &watched_labels());
        assert_eq!(r.citations_snapped(), 1);
        assert_eq!(r.citations_stripped(), 0);
        assert_eq!(
            r.snapped_titles,
            vec![("watched-959ee8a67210".to_string(), "watched-959ee8a8f330".to_string())]
        );
        assert!(r.cleaned.contains("[Source: watched-959ee8a8f330]"));
        assert!(!r.cleaned.contains("959ee8a67210"));
    }

    #[test]
    fn every_observed_garble_snaps_to_the_true_label() {
        // All seven corruptions the rebaseline shipped (steps 21 + 105).
        for garble in [
            "watched-959ee8a67210",
            "watched-959e8a8f33",
            "watched-9598a8f321",
            "watched-9e9ee8aaf320",
            "watched-959ee8a330",
            "watched-959eae8f330",
            "watched-959ee6a8f331",
        ] {
            let answer = format!("claim [Source: {garble}].");
            let r = attribute_citations(&answer, &watched_body(), &watched_labels());
            assert_eq!(r.citations_snapped(), 1, "{garble} must snap");
            assert!(r.cleaned.contains("[Source: watched-959ee8a8f330]"), "{garble}");
        }
    }

    #[test]
    fn exact_id_citation_is_untouched() {
        let answer = "the fox speaks [Source: watched-959ee8a8f330].";
        let r = attribute_citations(answer, &watched_body(), &watched_labels());
        assert!(!r.changed());
        assert_eq!(r.cleaned, answer);
    }

    #[test]
    fn id_shaped_garble_cannot_pass_the_word_floor() {
        // No labels captured (tool-transcript evidence): the pre-fix floor kept
        // this at exactly 0.5 ("watched" present, garbled hash absent). The
        // ID-token veto must strip it.
        let answer = "the fox speaks [Source: watched-959ee8a67210].";
        let r = attribute_citations(answer, &watched_body(), &[]);
        assert_eq!(r.citations_stripped(), 1);
        assert!(!r.cleaned.contains("Source:"));
    }

    #[test]
    fn ambiguous_near_twin_labels_strip_rather_than_missnap() {
        // Two real labels one edit apart from the cited garble: snapping would
        // be a coin flip, so the veto strips instead.
        let labels = vec!["watched-959ee8a8f330".to_string(), "watched-959ee8a8f332".to_string()];
        let answer = "claim [Source: watched-959ee8a8f331].";
        let r = attribute_citations(answer, &watched_body(), &labels);
        assert_eq!(r.citations_snapped(), 0);
        assert_eq!(r.citations_stripped(), 1);
        assert!(!r.cleaned.contains("959ee8a8f331"));
    }

    #[test]
    fn correct_bare_hash_survives_the_veto() {
        // Citing the id without its prefix: too far to snap (0.6), but the hash
        // IS a complete token inside the label — keep, don't strip.
        let answer = "claim [Source: 959ee8a8f330].";
        let r = attribute_citations(answer, &watched_body(), &watched_labels());
        assert_eq!(r.citations_stripped(), 0);
        assert!(r.cleaned.contains("[Source: 959ee8a8f330]"));
    }

    #[test]
    fn truncated_record_number_is_stripped_complete_one_kept() {
        // The NARA class: a record number must match a COMPLETE digit run.
        let body = vec!["Record 28949423 in the NARA index covers the sighting.".to_string()];
        let r = attribute_citations("see [Source: Record 2894942].", &body, &[]);
        assert_eq!(r.citations_stripped(), 1, "truncated number must strip");
        let r = attribute_citations("see [Source: Record 28949423].", &body, &[]);
        assert_eq!(r.citations_stripped(), 0, "complete number must survive");
    }

    #[test]
    fn aggregate_range_citation_is_not_missnapped_out_of_a_label_family() {
        // Live false snap (padfix replay 2026-07-01): the maple labels share a
        // long family prefix, inflating full-string similarity; the cited
        // aggregate RANGE measured 0.763 vs "Article VI — Pets" (runner-up
        // 0.738) and was wrongly rewritten to the Pets article. The margin
        // rule must refuse; the aggregate citation stays as the model wrote it.
        let labels: Vec<String> = [
            "Maple House Charter, Article II — Quiet Hours",
            "Maple House Charter, Article III — Kitchen Cleanup",
            "Maple House Charter, Article IV — Common Spaces",
            "Maple House Charter, Article VI — Pets",
            "Maple House Charter, Article VII — Smoking",
            "Maple House Charter, Article X — House Decisions",
            "maple-house",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let answer = "The Charter's rules [Source: Maple House Charter, Articles II–XI].";
        let r = attribute_citations(answer, &[], &labels);
        assert_eq!(r.citations_snapped(), 0, "must not snap an aggregate range");
        assert!(r.cleaned.contains("Articles II–XI"), "aggregate citation kept verbatim");
    }

    #[test]
    fn date_garbled_label_is_stripped_by_the_composite_veto() {
        // Live sub-floor garble (padfix replay): "2026-10-10" for the real
        // "2026-06-10" scores 0.73 — under the snap floor — and its date
        // fragments ("2026","10") are too short for the word-level ID rule.
        // The whole hyphen-digit run must complete-match or the citation strips.
        let labels = vec!["Decision — 2026-06-10 — Porch Smoking".to_string()];
        let body = vec!["To settle the porch dispute, smoking moved off the porch.".to_string()];
        let r = attribute_citations("rule [Source: Decision — 2026-10-10 — Porch].", &body, &labels);
        assert_eq!(r.citations_snapped(), 0);
        assert_eq!(r.citations_stripped(), 1, "garbled date must strip");
        // The correctly-cited label is untouched (exact match wins first).
        let ok = "rule [Source: Decision — 2026-06-10 — Porch Smoking].";
        let r = attribute_citations(ok, &body, &labels);
        assert!(!r.changed());
    }

    #[test]
    fn unclosed_bracket_never_swallows_following_text() {
        // The model truncates a bracket; the next ']' lives inside LATER text
        // (here a verification-note item). The scanner must leave the whole
        // span untouched rather than parse ~100 chars as one "citation".
        let answer = "grounded [Source: public-goods\n\n---\n*Verification note:*\n\
                      - “supported by [unverified excerpt: Mill argued tolls]”";
        let r = attribute_citations(answer, &[], &["public-goods".to_string()]);
        assert_eq!(r.cleaned, answer);
        assert!(!r.changed());
    }

    #[test]
    fn parenthetical_qualifier_snaps_to_the_exact_base_label() {
        // "[Source: Wikipedia (contested)]" over the real label "wikipedia":
        // the qualifier is editorializing, not a source name. Body containing
        // the qualifier word must not rescue it via the floor.
        let labels = vec!["wikipedia".to_string()];
        let body = vec!["The reliability of Wikipedia is contested by some critics.".to_string()];
        let r = attribute_citations("claim [Source: Wikipedia (contested)].", &body, &labels);
        assert_eq!(r.citations_snapped(), 1);
        assert!(r.cleaned.contains("[Source: wikipedia]"));
        assert!(!r.cleaned.contains("contested)"));
    }

    #[test]
    fn short_year_token_is_not_id_shaped() {
        // "2001" (len 4) stays under the plain word rule — a real reworded title
        // carrying a year must not trip the veto.
        let body = vec!["Re: EnronOnline Executive Summary for April 23, 2001.".to_string()];
        let answer = "summary [Source: EnronOnline Summary 2001].";
        let r = attribute_citations(answer, &body, &[]);
        assert_eq!(r.citations_stripped(), 0);
    }
}
