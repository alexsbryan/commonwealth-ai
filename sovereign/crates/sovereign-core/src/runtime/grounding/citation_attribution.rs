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
//! Deterministic, no LLM: tokenize the cited title, measure the fraction of its
//! significant words present in the evidence, and strip the marker when the
//! majority are absent. A reformatted-but-real title ("Federalist 51 (Madison)"
//! for the header "Federalist No. 51") keeps most of its words and is released; an
//! invented one ("Advertising Campaign - NASCAR") keeps about none and is stripped.
//!
//! Strip, don't gate — a false strip removes one marker, it does not refuse a good
//! answer; that asymmetry is why the floor is forgiving (0.5). The per-answer
//! fabrication rate is reported for the glassbox so a confabulation-heavy answer is
//! visible to telemetry and the desktop panel even though we don't gate on it yet.

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

/// The result of auditing an answer's citations against its evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationAttribution {
    /// The answer with unverifiable `[Source: …]` markers removed.
    pub cleaned: String,
    /// Total individual `Source:` citations seen across the answer.
    pub citations_total: usize,
    /// The titles that were stripped (absent from the evidence).
    pub stripped_titles: Vec<String>,
}

impl CitationAttribution {
    /// How many citations were stripped.
    pub fn citations_stripped(&self) -> usize {
        self.stripped_titles.len()
    }
    /// Did the audit change the answer?
    pub fn changed(&self) -> bool {
        !self.stripped_titles.is_empty()
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
    let chars: Vec<char> = answer.chars().collect();
    let mut out = String::with_capacity(answer.len());
    let mut citations_total = 0usize;
    let mut stripped_titles: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '[' {
            if let Some(rel) = chars[i + 1..].iter().position(|&c| c == ']') {
                let end = i + 1 + rel; // absolute index of ']'
                let inner: String = chars[i + 1..end].iter().collect();
                if inner.trim_start().to_lowercase().starts_with("source:") {
                    let (rebuilt, total, stripped) = process_bracket(&inner, &hay);
                    citations_total += total;
                    stripped_titles.extend(stripped);
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
    CitationAttribution { cleaned: out, citations_total, stripped_titles }
}

/// Process one `[…]` whose inner text begins with `Source:`. Splits on `;` into
/// individual citations, keeps the verifiable ones, and returns the rebuilt inner
/// text (empty = drop the whole bracket), the count of citations seen, and the
/// titles stripped.
fn process_bracket(inner: &str, hay: &str) -> (String, usize, Vec<String>) {
    let mut kept: Vec<String> = Vec::new();
    let mut total = 0usize;
    let mut stripped: Vec<String> = Vec::new();
    for seg in inner.split(';') {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        total += 1;
        let title = strip_source_prefix(seg);
        if title_is_supported(title, hay) {
            kept.push(format!("Source: {title}"));
        } else {
            stripped.push(title.to_string());
        }
    }
    (kept.join("; "), total, stripped)
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

/// A title is supported when too short to judge (kept, conservative) or when at
/// least `SUPPORT_FLOOR` of its significant words are present in the evidence.
fn title_is_supported(title: &str, hay: &str) -> bool {
    // A multi-word title present VERBATIM as a contiguous phrase is grounded by
    // definition — it is literally a header in the evidence.
    let nt: String = title.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    if nt.split(' ').filter(|w| !w.is_empty()).count() >= 2 && hay.contains(&nt) {
        return true;
    }
    let sig = significant_words(title);
    if sig.len() < MIN_TITLE_WORDS {
        return true; // too coarse to judge — keep
    }
    let present = sig.iter().filter(|w| hay.contains(w.as_str())).count();
    (present as f32 / sig.len() as f32) >= SUPPORT_FLOOR
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
}
