// SPDX-License-Identifier: AGPL-3.0-or-later
//! `quote_verification`'s tests, beside it under `#[path]` so their names
//! are unchanged (the file was past its size band, ARCH §3.1).

use super::*;

#[test]
fn empty_evidence_leaves_answer_unchanged() {
    // Parametric / retrieval-miss path: doc_context is empty. Even a
    // long quoted span must NOT be demoted — there is no source to
    // verify against, so demotion would be a false accusation.
    let answer = r#"Kant argues that "the categorical imperative binds all rational agents unconditionally and without exception.""#;
    let r = verify_answer_against_evidence(answer, "");
    assert_eq!(r.demoted_count, 0);
    assert_eq!(r.verified_count, 0);
    assert_eq!(r.rewritten, answer);
    // Whitespace-only evidence is treated the same as empty.
    let r2 = verify_answer_against_evidence(answer, "   \n  ");
    assert_eq!(r2.rewritten, answer);
    assert_eq!(r2.demoted_count, 0);
}

#[test]
fn fabricated_quote_against_evidence_is_demoted() {
    // SEP-shaped evidence: a real passage the model was shown. The
    // answer fabricates a verbatim-looking quote that never appears.
    let evidence = "Compatibilism is the thesis that free will is compatible with determinism. \
         Classical compatibilists analyse the freedom to do otherwise as a hypothetical: \
         an agent could have done otherwise if she had chosen to.";
    let answer = r#"On this view, Frankfurt holds that "moral responsibility floats entirely free of any ability to do otherwise whatsoever.""#;
    let r = verify_answer_against_evidence(answer, evidence);
    assert_eq!(r.demoted_count, 1);
    assert!(r.rewritten.contains("[unverified excerpt:"));
}

#[test]
fn verbatim_quote_against_evidence_passes() {
    let evidence = "Compatibilism is the thesis that free will is compatible with determinism. \
         Classical compatibilists analyse the freedom to do otherwise as a hypothetical.";
    let answer = r#"The entry defines it directly: "free will is compatible with determinism" is the core claim."#;
    let r = verify_answer_against_evidence(answer, evidence);
    assert_eq!(r.demoted_count, 0);
    assert_eq!(r.verified_count, 1);
    assert_eq!(r.rewritten, answer);
}

#[test]
fn verified_quote_passes_through_unchanged() {
    let source = "Stevie sat at a deal table, drawing circles, circles, circles; innumerable circles, concentric, eccentric.".to_string();
    let answer = r#"The narrator says "drawing circles, circles, circles; innumerable circles, concentric, eccentric" in chapter one."#;
    let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
    assert_eq!(r.demoted_count, 0);
    assert_eq!(r.verified_count, 1);
    assert!(r.rewritten.contains(
        r#""drawing circles, circles, circles; innumerable circles, concentric, eccentric""#
    ));
}

#[test]
fn unverified_quote_is_demoted() {
    let source = "He walked through the empty streets.".to_string();
    let answer = r#"As Conrad writes, "the professor seized the policeman by the throat with great violence and intent.""#;
    let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
    assert_eq!(r.demoted_count, 1);
    assert_eq!(r.verified_count, 0);
    assert!(r.rewritten.contains("[unverified excerpt:"));
    assert!(!r.rewritten.contains(r#""the professor seized"#));
}

#[test]
fn composite_quote_with_ellipsis_fails_verification() {
    // The two fragments are real; the composite isn't continuous
    // anywhere in the source — exactly the failure mode this
    // module is built to catch.
    let chunk_a = "He smiled no longer his enigmatic and mocking smile.".to_string();
    let chunk_b = "It was a sad-faced, miserable little man who emerged.".to_string();
    let answer = r#"Conrad writes, "He smiled no longer his enigmatic mocking smile... It was a sad-faced, miserable little man who emerged.""#;
    let r = verify_quotes(answer, &[chunk_a, chunk_b], &[], DEFAULT_MIN_QUOTE_CHARS);
    assert_eq!(r.demoted_count, 1, "composite quotes must be demoted");
    assert!(r.rewritten.contains("[unverified excerpt:"));
}

#[test]
fn short_quotes_below_min_chars_pass_through() {
    let source = "The professor walks alone.".to_string();
    let answer = r#"The "professor" is the focus."#;
    let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
    // Quote is shorter than DEFAULT_MIN_QUOTE_CHARS — neither
    // verified nor demoted; just passed through.
    assert_eq!(r.demoted_count, 0);
    assert_eq!(r.verified_count, 0);
    assert!(r.rewritten.contains(r#""professor""#));
}

#[test]
fn whitespace_normalised_quote_verifies() {
    // Source has a hard line break inside the quoted phrase.
    let source = "He found himself walking\nthrough the empty streets at dawn.".to_string();
    let answer =
        r#"Conrad says he was "walking through the empty streets at dawn" — a key moment."#;
    let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
    assert_eq!(r.verified_count, 1);
    assert_eq!(r.demoted_count, 0);
}

#[test]
fn curly_quotes_are_recognised() {
    // Sources use straight quotes; answer uses smart curly quotes.
    let source = "She found the wedding ring hidden in her pocket.".to_string();
    let answer = "He recalls Winnie\u{201C}found the wedding ring hidden in her pocket\u{201D} in chapter twelve.";
    let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
    assert_eq!(r.verified_count, 1);
}

#[test]
fn extra_verbatim_spans_supplement_chunks() {
    // The full chunk doesn't contain the quote, but a RAPTOR
    // quote_span does — verification should still pass.
    let chunk = "Something else entirely from the document.".to_string();
    let raptor_span = "the haunting fear of his sinister loneliness".to_string();
    let answer = r#"The professor is described with "the haunting fear of his sinister loneliness" in the encounter."#;
    let r = verify_quotes(answer, &[chunk], &[raptor_span], DEFAULT_MIN_QUOTE_CHARS);
    assert_eq!(r.verified_count, 1);
    assert_eq!(r.demoted_count, 0);
}

#[test]
fn multiple_quotes_in_one_answer_independent_outcomes() {
    let source =
        "Stevie drew his circles, circles, circles all afternoon long in silence.".to_string();
    let answer = r#"The narrator says "Stevie drew his circles, circles, circles all afternoon" but also "the moon rose over the empty hills above the silent town" later."#;
    let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
    assert_eq!(r.verified_count, 1);
    assert_eq!(r.demoted_count, 1);
    assert!(r
        .rewritten
        .contains(r#""Stevie drew his circles, circles, circles all afternoon""#));
    assert!(r.rewritten.contains("[unverified excerpt: the moon"));
}

#[test]
fn normalise_for_match_folds() {
    assert_eq!(normalise_for_match("a  b\t\nc"), "a b c");
    assert_eq!(
        normalise_for_match("  leading and trailing  "),
        "leading and trailing"
    );
    assert_eq!(normalise_for_match(""), "");
    assert_eq!(
        normalise_for_match("Mrs Verloc\u{2019}s \u{201C}gaze\u{201D} \u{2014} steady"),
        "Mrs Verloc's \"gaze\" - steady"
    );
    assert_eq!(
        normalise_for_match("**bold** and _italic_"),
        "bold and italic"
    );
    assert_eq!(normalise_for_match("wait\u{2026} what"), "wait... what");
}

#[test]
fn curly_apostrophe_in_source_matches_straight_in_quote() {
    // Gutenberg text uses U+2019; models quote with '. Observed
    // false demotion class #1 on the 2026-07-23 eye test.
    let source =
        "Winnie\u{2019}s philosophy consisted in not taking notice of the inside of facts."
            .to_string();
    let answer = r#"The narrator notes that "Winnie's philosophy consisted in not taking notice of the inside of facts.""#;
    let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
    assert_eq!(r.demoted_count, 0);
    assert_eq!(r.verified_count, 1);
}

#[test]
fn markdown_bold_inside_quote_matches_plain_source() {
    let source = "where that spectre took its constitutional crawl every fine morning.".to_string();
    let answer = r#"Conrad writes "that spectre took its **constitutional crawl** every fine morning" of Yundt."#;
    let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
    assert_eq!(r.demoted_count, 0);
    assert_eq!(r.verified_count, 1);
}

#[test]
fn gutenberg_underscore_italics_match_unmarked_quote() {
    let source = "He read the _Morning Post_ with an air of complete detachment.".to_string();
    let answer =
        r#"He is seen reading: "He read the Morning Post with an air of complete detachment.""#;
    let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
    assert_eq!(r.verified_count, 1);
    assert_eq!(r.demoted_count, 0);
}

/// `verified_spans` carries exactly the spans that PASSED — verbatim as
/// written in the answer — and nothing else: demoted spans and spans
/// under the length floor never appear (order authority-guard-at-exit).
/// The failing input, by name: a fabricated figure wrapped in quote
/// marks ("Net sales were $999,999 million and rose despite this")
/// is demoted, so it earns no exemption downstream.
#[test]
fn verified_spans_lists_passed_spans_only() {
    let source = "Mac net sales increased during 2025 compared to 2024 due primarily to higher \
         net sales of MacBook Air."
        .to_string();
    let answer = r#"Per the filing, "Mac net sales increased during 2025 compared to 2024 due primarily to higher net sales of MacBook Air." A short "so-called" aside, and a fake: "Net sales were $999,999 million and rose despite this headwind"."#;
    let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
    assert_eq!(r.verified_count, 1);
    assert_eq!(r.demoted_count, 1);
    assert_eq!(
        r.verified_spans,
        vec![
            "Mac net sales increased during 2025 compared to 2024 due primarily to \
             higher net sales of MacBook Air."
                .to_string()
        ],
        "only the passed span, verbatim; the demoted and short spans are absent"
    );
}

#[test]
fn edge_ellipses_trimmed_interior_composites_still_fail() {
    let source =
        "Jolly lucky for Yundt that she had persisted in coming up time after time.".to_string();
    // Edge elision: honest quoting, must verify.
    let edge =
        r#"As the text says, "...she had persisted in coming up time after time..." throughout."#;
    let r = verify_quotes(edge, &[source.clone()], &[], DEFAULT_MIN_QUOTE_CHARS);
    assert_eq!(r.verified_count, 1, "edge ellipses are not composites");
    assert_eq!(r.demoted_count, 0);
    // Interior splice: still a composite, still demoted.
    let spliced = r#"As the text says, "Jolly lucky for Yundt... coming up time after time again and again.""#;
    let r2 = verify_quotes(spliced, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
    assert_eq!(r2.demoted_count, 1, "interior splices must keep failing");
}

/// THE 600-CHAR SPLIT. Both arms in one test, because the point is not
/// "the new function works" but "the old surface is why correct citations
/// were called unverified". Measured 2026-08-05: 50 of 80 released
/// citations demoted, 55 of 55 of those spans verbatim in the evidence,
/// zero fabrications. The discriminator was purely the quote's offset
/// inside its own chunk (kept at 273; demoted at 792 and 1708).
///
/// Built from the REAL `truncate_chunk_content`, so this cannot drift if
/// `MAX_CHUNK_CHARS` moves — it tests the relationship, not the number.
/// covers: GR-19, GR-20
#[test]
fn a_quote_from_beyond_the_prompt_truncation_is_no_longer_demoted() {
    let filler = "The ledger was kept in a fair hand, and the entries ran on \
                  without remark from one quarter to the next. ";
    let mut chunk = filler.repeat(24);
    let offset = chunk.len();
    let sentence = "Widow Hetch, who kept The Cold Lantern, gave her evidence \
                    at her own bar with her arms folded.";
    chunk.push_str(sentence);
    assert!(
        offset > crate::runtime::text_utils::MAX_CHUNK_CHARS,
        "the fixture must put the sentence past the truncation to test anything"
    );
    let doc_context = crate::runtime::text_utils::truncate_chunk_content(&chunk);
    let answer = format!("It was her own bar.\n\nGrounded in the source:\n  \"{sentence}\"");

    // The prompt rendering genuinely cannot see it — this is the defect.
    let old = verify_answer_against_evidence(&answer, &doc_context);
    assert_eq!(
        old.demoted_count, 1,
        "if this stops demoting, the fixture no longer reproduces the bug"
    );

    // The turn's evidence can.
    let fixed = verify_answer_against_turn_evidence(&answer, &doc_context, &[chunk]);
    assert_eq!(
        fixed.demoted_count, 0,
        "verbatim source text must not be called unverified"
    );
    assert_eq!(fixed.verified_count, 1);
    assert!(fixed.rewritten.contains(&format!("\"{sentence}\"")));
}

/// An unclosed quote must not pair with the next paragraph's opener. The
/// 2026-09-13 chaos soak (step 239), reconstructed from the rewritten
/// answer: the model opened a quote in its value line and never closed it,
/// so the span ran to the excerpt's opening mark and the demotion swallowed
/// the `Grounded in the source:` header.
#[test]
fn an_unclosed_quote_does_not_capture_the_next_paragraph() {
    let excerpt = "Research in Agricultural Engineering Current issue 2026/2 Archive Search";
    let answer = format!(
        "Current issue year and number for \"Research in Agricultural Engineering: \
         2026, Issue 2\n\nGrounded in the source:\n  \"{excerpt}\""
    );
    let r = verify_quotes(
        &answer,
        &[format!("Journal home. {excerpt} Contact.")],
        &[],
        20,
    );
    assert!(
        !r.rewritten
            .contains("[unverified excerpt: Research in Agricultural Engineering: 2026"),
        "the value line must not be demoted as a quote: {}",
        r.rewritten
    );
    assert!(
        r.rewritten.contains("\n\nGrounded in the source:\n"),
        "{}",
        r.rewritten
    );
    assert_eq!(
        r.verified_count, 1,
        "the real excerpt still verifies: {}",
        r.rewritten
    );
    assert_eq!(r.demoted_count, 0, "{}", r.rewritten);
}

/// The widening must not reach the failure this guard exists for. A
/// composite — real fragments spliced with an interior ellipsis — is
/// non-contiguous in the source under any normalisation, so passing the
/// FULL chunks alongside the rendering leaves it demoted.
/// covers: GR-21
#[test]
fn a_composite_quote_is_still_demoted_against_the_full_chunks() {
    let chunk = "The ledger was kept in a fair hand. Many pages later, and after \
                 much else besides, the auditor came out from Saltern Cross."
        .to_string();
    let answer = "As recorded: \"The ledger was kept in a fair hand ... the auditor \
                  came out from Saltern Cross.\"";
    let r = verify_answer_against_turn_evidence(answer, &chunk, &[chunk.clone()]);
    assert_eq!(
        r.demoted_count, 1,
        "a spliced quote is still a spliced quote"
    );
    assert!(r.rewritten.contains("[unverified excerpt:"));
}

/// The parametric / retrieval-miss path must stay untouched: the
/// empty-surface guard is keyed on `evidence`, not on `chunks`.
#[test]
fn empty_evidence_still_leaves_the_answer_alone() {
    let answer = "No sources here, but \"this is a long enough quoted span to check\".";
    let r = verify_answer_against_turn_evidence(answer, "", &["something".to_string()]);
    assert_eq!(r.rewritten, answer);
    assert_eq!(r.demoted_count, 0);
}

/// Defect 6 (ADDRESSED_TEXT appendix): with no surface at all there is
/// nothing a quote could be checked against, so nothing is demoted, as
/// `attached_doc`'s failed-prefetch path always claimed.
#[test]
fn no_surface_demotes_nothing() {
    let answer = "As the text says, \"this is a long enough quoted span to be checked\".";
    for (chunks, spans) in [
        (Vec::new(), Vec::new()),
        (vec!["  \n".to_string()], vec![String::new()]),
    ] {
        let r = verify_quotes(answer, &chunks, &spans, DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(r.rewritten, answer);
        assert_eq!((r.verified_count, r.demoted_count), (0, 0));
    }
}

/// The convergence onto the aligner keeps what the substring test it
/// replaced accepted (ADDRESSED_TEXT §5.3, commit 1): every stretch of a
/// source at least `DEFAULT_MIN_QUOTE_CHARS` long, its edges inside words
/// and gaps the source itself has included, is located, at a range whose
/// fold is the stretch's fold.
///
/// Under `align/1` three were missed ("hop—small, dim… and _quiet_
/// [sic]\n  stoo" and its two neighbours): the edge cut left the one
/// three-word run with no exact 3-gram, and shorter runs did not seed.
/// Since `align/2` every gap-free run seeds at its own length.
#[test]
fn every_stretch_of_a_source_is_located_where_it_stands() {
    let other = "An unrelated passage about the sea, the ships upon it, and the men.";
    let source = "Mr Verloc\u{2019}s shop\u{2014}small, dim\u{2026} and _quiet_ [sic]\n  \
                  stood in a street of \u{201C}grimy\u{201D} brick houses, long ere the \
                  dawn... It was a square box of a place, with the front glazed in small \
                  panes [the window], and the door remained closed all day.";
    let chars: Vec<char> = source.chars().collect();
    let (mut located, mut missed) = (0, Vec::new());
    for len in (DEFAULT_MIN_QUOTE_CHARS..=DEFAULT_MIN_QUOTE_CHARS + 21).step_by(7) {
        for start in 0..=chars.len() - len {
            let quote: String = chars[start..start + len].iter().collect();
            let needle = quote_align::norm_v0(trim_edge_ellipses(&quote)).to_string();
            let Some((text, range)) = locate_verbatim(&quote, &[other, source]) else {
                missed.push(quote);
                continue;
            };
            assert_eq!(text, 1, "{quote:?}");
            let at = quote_align::code_point_slice(source, range).expect("a range in it");
            assert_eq!(quote_align::norm_v0(at).to_string(), needle, "{quote:?}");
            located += 1;
        }
    }
    assert!(missed.is_empty(), "in the source, not located: {missed:?}");
    assert!(located > 600, "the sweep ran: {located}");
}
