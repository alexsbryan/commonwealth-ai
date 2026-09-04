use super::*;

fn chunks() -> Vec<String> {
    vec![
        "The blended noises of the enormous town sank to a murmur. Chief Inspector \
             Heat of the Special Crimes Department changed his tone. His wife, examining \
             the sharp edge of the carving knife, placed it on the dish."
            .to_string(),
        "Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc.".to_string(),
    ]
}

#[test]
fn parses_quote_and_answer() {
    let r = "QUOTE: Chief Inspector Heat of the Special Crimes Department changed his tone.\nANSWER: Heat is a Chief Inspector.";
    let (q, a) = parse_quote_answer(r).unwrap();
    assert!(q.starts_with("Chief Inspector Heat"));
    assert_eq!(a, "Heat is a Chief Inspector.");
}

#[test]
fn unparseable_is_none() {
    assert!(parse_quote_answer("I think the answer is Chief Inspector").is_none());
}

#[test]
fn none_sentinel_detected() {
    let (q, a) = parse_quote_answer("QUOTE: NONE\nANSWER: NONE").unwrap();
    assert!(is_none(&q) && is_none(&a));
}

#[test]
fn verbatim_quote_present() {
    // Exact copy of the sentence whose answer ("Chief Inspector") the
    // STOP-list verifier wrongly killed — the quote itself is present.
    assert!(locate_quote_in_chunks(
        "Chief Inspector Heat of the Special Crimes Department changed his tone.",
        &chunks()
    )
    .is_some());
    assert!(locate_quote_in_chunks(
        "Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc.",
        &chunks()
    )
    .is_some());
}

#[test]
fn trimmed_edges_still_match_via_run() {
    // Model dropped the leading clause but copied a long verbatim run.
    assert!(locate_quote_in_chunks(
        "Heat of the Special Crimes Department changed his tone today",
        &chunks()
    )
    .is_some());
}

#[test]
fn fabricated_quote_rejected() {
    // A plausible but invented sentence shares no 6-word run.
    assert!(locate_quote_in_chunks(
        "Winnie killed Verloc with a blowpipe in the parlour.",
        &chunks()
    )
    .is_none());
    // A paraphrase of a real sentence also fails (not verbatim).
    assert!(locate_quote_in_chunks(
        "Stevie was the younger sibling of Winnie Verloc.",
        &chunks()
    )
    .is_none());
}

#[test]
fn answer_must_be_in_its_own_quote() {
    // Title/name answers — present in their quote (the light stop keeps
    // "chief"/"inspector"/"doctor", unlike the all-chunks value check).
    assert!(answer_supported_by_quote(
        "Chief Inspector",
        "Chief Inspector Heat of the Special Crimes Department changed his tone."
    ));
    assert!(answer_supported_by_quote(
        "the Doctor",
        "Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc."
    ));
    // The measured moat break: a confabulated value ("Russian") pinned to a
    // real-but-insufficient quote that never names the country.
    assert!(!answer_supported_by_quote(
        "Russian embassy",
        "Ever since the time of the late Baron Stott-Wartenheim, employed by the Embassy."
    ));
    // Space-dropped copies are grounded content wearing a typo: verification
    // is space-tolerant (the old space-strict rule turned a CORRECT
    // lighthouse answer into a decline), and respace_answer_from_quote
    // repairs the surface from the quote before release.
    assert!(answer_supported_by_quote(
        "dancinggirls",
        "photographs of more or less undressed dancing girls in the window"
    ));
    assert_eq!(
        respace_answer_from_quote(
            "dancinggirls",
            "photographs of more or less undressed dancing girls in the window"
        )
        .as_deref(),
        Some("dancing girls")
    );
    // Numeric truncation (measured 2026-07-01): a TRUNCATED number must not
    // ground against a quote that contains a *longer* number sharing its
    // leading digits. "289494" is a prefix substring of "28949423" but a
    // different value.
    assert!(!answer_supported_by_quote(
            "289494",
            "U.S. Air Force Project Blue Book UFO case file (15 scanned pages; NARA fileUnit 28949423)."
        ));
    // The complete, correct number still grounds.
    assert!(answer_supported_by_quote(
            "28949423",
            "U.S. Air Force Project Blue Book UFO case file (15 scanned pages; NARA fileUnit 28949423)."
        ));
    // A whole-token year grounds normally.
    assert!(answer_supported_by_quote(
        "Deloitte 2025",
        "review of Deloitte's performance during the engagement for the 2025 audit"
    ));
    // Single-digit answer (measured 2026-07-08 class-A evidence-denial): "4"
    // is a valid COMPLETE number token in the quote. The old >=2-char word
    // filter dropped it, emptied the word list, and returned false → a false
    // abstain that surfaced as "the sources don't cover it".
    assert!(answer_supported_by_quote(
        "4",
        "assert!(result.chunks_created >= 4);"
    ));
    // …but a single digit that is NOT a complete token in the quote (or is a
    // prefix of a longer number) still fails — no free pass from the exemption.
    assert!(!answer_supported_by_quote(
        "5",
        "assert!(result.chunks_created >= 4);"
    ));
    assert!(!answer_supported_by_quote(
        "2",
        "the NARA fileUnit 28949423 has 24 pages"
    ));
}

// ── mid-token stop compensation (probed deterministically 2026-07-01:
//    finish=Stop at 99/256 tokens, answer cut mid-symbol) ──

#[test]
fn completes_the_mid_symbol_answer_from_the_chunk() {
    // The observed failure: chaos rebaseline step 127 / replay step 21.
    let chunk = "two prompt forms: `RELATIONAL_BASE_SYSTEM_PROMPT` (full) and\n\
                     `RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT` (compact — situated-handler default).";
    let fixed = extend_mid_token_copy("RELATIONAL_EXPRESSIVE_SYSTEM_PROM", std::iter::once(chunk));
    assert_eq!(
        fixed.as_deref(),
        Some("RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT")
    );
}

#[test]
fn completes_the_dangling_formula_operator() {
    // Chaos rebaseline step 173: the answer stopped at a trailing "¬".
    let quote = "Then simply define Hn+1 := ¬H1 ∧ … ∧ ¬Hn and add this new hypothesis.";
    let fixed = extend_mid_token_copy("Hn+1 := ¬H1 ∧ … ∧ ¬", std::iter::once(quote));
    assert_eq!(fixed.as_deref(), Some("Hn+1 := ¬H1 ∧ … ∧ ¬Hn"));
}

#[test]
fn complete_token_anywhere_means_no_extension() {
    // "1968" ends at a boundary in one occurrence — it is a real token; the
    // longer "19685" elsewhere must not trigger an extension.
    let chunk = "launched in 1968. Production reached 19685 units.";
    assert_eq!(extend_mid_token_copy("1968", std::iter::once(chunk)), None);
}

#[test]
fn disagreeing_continuations_do_not_extend() {
    let chunk = "PREFIXalpha here, PREFIXbeta there.";
    assert_eq!(
        extend_mid_token_copy("PREFIX", std::iter::once(chunk)),
        None
    );
}

#[test]
fn truncated_number_completes_to_the_source_value() {
    // The NARA class: "289494" cut from "28949423" — unanimous continuation
    // restores the real value (verification then passes on the full number).
    let quote = "NARA fileUnit 28949423.";
    assert_eq!(
        extend_mid_token_copy("289494", std::iter::once(quote)).as_deref(),
        Some("28949423")
    );
}

#[test]
fn whitespace_runs_are_equivalent() {
    // The answer copies a quote whose source chunk breaks the line mid-span.
    let chunk = "the relational voice\ncontract has two prompt\n  forms in FOOBA";
    let fixed = extend_mid_token_copy(
        "voice contract has two prompt forms in FOO",
        std::iter::once(chunk),
    );
    assert_eq!(
        fixed.as_deref(),
        Some("voice contract has two prompt forms in FOOBA")
    );
}

#[test]
fn oversized_continuation_is_not_guessed() {
    let chunk = "hash watched959ee8a8f330aabbccddeeff00112233445566778899 end";
    assert_eq!(
        extend_mid_token_copy("watched", std::iter::once(chunk)),
        None
    );
}

#[test]
fn absent_text_and_sentinels_are_untouched() {
    assert_eq!(
        extend_mid_token_copy("missing", std::iter::once("no match here")),
        None
    );
    assert_eq!(extend_mid_token_copy("", std::iter::once("anything")), None);
}

// ── case fidelity (gen75 step 115: "¬HN" released for the source's "¬Hn") ──

#[test]
fn case_garbled_formula_snaps_to_quote_casing() {
    let quote = "Then simply define Hn+1 := ¬H1 ∧ … ∧ ¬Hn and add this new hypothesis.";
    let fixed = snap_answer_case_to_quote("Hn+1 := ¬H1 ∧ … ∧ ¬HN", quote);
    assert_eq!(fixed.as_deref(), Some("Hn+1 := ¬H1 ∧ … ∧ ¬Hn"));
}

#[test]
fn decapitalized_proper_noun_is_restored() {
    let quote = "Chief Inspector Heat of the Special Crimes Department changed his tone.";
    assert_eq!(
        snap_answer_case_to_quote("chief inspector heat", quote).as_deref(),
        Some("Chief Inspector Heat")
    );
}

#[test]
fn exact_case_and_non_span_answers_are_untouched() {
    let quote = "Then simply define Hn+1 := ¬H1 ∧ … ∧ ¬Hn here.";
    assert_eq!(
        snap_answer_case_to_quote("Hn+1 := ¬H1 ∧ … ∧ ¬Hn", quote),
        None
    );
    assert_eq!(
        snap_answer_case_to_quote("something else entirely", quote),
        None
    );
}

#[test]
fn space_dropped_lighthouse_answer_respaces_from_quote() {
    // probe4 verbatim: correct values, spaces eaten by the copy channel.
    let quote = "The light's characteristic signal is one white flash every 18                      seconds, visible for 21 nautical miles in clear weather.";
    let ans = "one white flash every 18seconds; 21nauticalmiles";
    assert!(answer_supported_by_quote(ans, quote));
    assert_eq!(
        respace_answer_from_quote(ans, quote).as_deref(),
        Some("one white flash every 18 seconds; 21 nautical miles")
    );
}

#[test]
fn fabricated_compound_still_fails_space_blind() {
    // "50minutes" has no "50 minutes" in the quote either way.
    let quote = "The sighting was reported at dawn and lasted briefly.";
    assert!(!answer_supported_by_quote("50minutes", quote));
    assert_eq!(respace_answer_from_quote("50minutes", quote), None);
}

#[test]
fn whitespace_differences_still_case_snap() {
    // The answer collapses the quote's line break; casing still restores.
    let quote = "the RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT\n(compact) form";
    assert_eq!(
        snap_answer_case_to_quote(
            "the relational_expressive_system_prompt (compact) form",
            quote
        )
        .as_deref(),
        Some("the RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT\n(compact) form")
    );
}

// ── multi-quote contract (SOVEREIGN_CITATION_MULTIQUOTE) ──────────────
//
// The defect these cover: on a compound question the single-sentence
// contract grounds 0/14 because no ONE sentence answers both halves, so
// the model takes the whole-question NONE exit and a verifiable citation
// for the half it CAN answer is thrown away with the half it cannot.

#[test]
fn parses_one_block_per_part() {
    let r = "PART: the nickname\nQUOTE: Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc.\nANSWER: the Doctor\n\
                 PART: Verloc's first name\nQUOTE: NONE\nANSWER: NONE";
    let parts = parse_parts(r);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].0, "the nickname");
    assert!(parts[0].1.starts_with("Alexander Ossipon"));
    assert_eq!(parts[0].2, "the Doctor");
    assert_eq!(parts[1].0, "Verloc's first name");
    assert!(is_none(&parts[1].2));
}

#[test]
fn reply_without_part_labels_yields_no_parts() {
    // The caller falls back to the single-pair parse, so a model that
    // ignores the format degrades to today's behaviour, not to a refusal.
    let r = "QUOTE: Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc.\nANSWER: the Doctor";
    assert!(parse_parts(r).is_empty());
}

#[test]
fn grounds_the_answerable_part_and_names_the_rest() {
    // This is the compound-inn-and-innkeeper shape: part one is verbatim
    // in the passages, part two is simply not there.
    let parts = vec![
        (
            "the nickname".to_string(),
            "Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc.".to_string(),
            "the Doctor".to_string(),
        ),
        (
            "Verloc's first name".to_string(),
            "NONE".to_string(),
            "NONE".to_string(),
        ),
    ];
    match multiquote_outcome(&parts, &chunks(), &[], &[]) {
        CitationOutcome::Grounded { answer, quotes } => {
            assert!(
                answer.contains("the Doctor"),
                "grounded half must ship: {answer}"
            );
            // The absent half is NAMED, not silently dropped (§18.3).
            assert!(
                answer.contains("The passages do not answer"),
                "gap must be named: {answer}"
            );
            assert!(answer.contains("Verloc's first name"), "{answer}");
            // One span PER verified quote — never pre-joined, or the
            // post-hoc quote_verification pass demotes the whole citation
            // to `[unverified excerpt: ...]`.
            assert_eq!(
                quotes.len(),
                1,
                "only the grounded part contributes a quote"
            );
            assert!(quotes[0].text.starts_with("Alexander Ossipon"));
        }
        _ => panic!("a verbatim-quoted part must ground even when a sibling part cannot"),
    }
}

#[test]
fn all_parts_ungrounded_abstains() {
    // Floor unchanged: when nothing grounds, the multi-quote contract
    // abstains exactly as the single-sentence one does.
    let parts = vec![
        ("a".to_string(), "NONE".to_string(), "NONE".to_string()),
        ("b".to_string(), "NONE".to_string(), "NONE".to_string()),
    ];
    assert!(matches!(
        multiquote_outcome(&parts, &chunks(), &[], &[]),
        CitationOutcome::Abstain
    ));
}

#[test]
fn a_part_whose_quote_is_not_verbatim_cannot_enter() {
    // The new door is not a fabrication bypass: each part clears the same
    // verify_pair bar, so an invented quote is refused even when a sibling
    // part grounds cleanly.
    let parts = vec![
        (
            "the nickname".to_string(),
            "Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc.".to_string(),
            "the Doctor".to_string(),
        ),
        (
            "his posting".to_string(),
            "Ossipon served as the Russian ambassador to London.".to_string(),
            "Russian ambassador".to_string(),
        ),
    ];
    match multiquote_outcome(&parts, &chunks(), &[], &[]) {
        CitationOutcome::Grounded { answer, .. } => {
            assert!(answer.contains("the Doctor"));
            assert!(
                !answer.contains("Russian ambassador"),
                "a quote absent from the passages must not ship: {answer}"
            );
            assert!(answer.contains("The passages do not answer"), "{answer}");
            assert!(answer.contains("his posting"), "{answer}");
        }
        _ => panic!("the grounded part should still release"),
    }
}

#[test]
fn an_answer_supported_by_the_matched_chunk_grounds_when_the_quote_alone_is_insufficient() {
    // THE LYLE-HANNETT SHAPE (measured 2026-08-10, chaos-saltgrass
    // present-stolen-object, 4 consecutive runs): the draft answers with
    // the full proper name, quotes the ADJACENT sentence that refers back
    // with the bare noun, and quote-local support dropped the part —
    // releasing "The passages do not answer: …" over a correct draft.
    // Support must widen to the chunk the quote matched in.
    let chunk = "The Lyle-Hannett chronometer was the only valuable thing in \
                     the harbormaster's office. Now the walnut case hung open and \
                     the chronometer was gone, and the dust on the shelf below \
                     showed the shape of its base."
        .to_string();
    let parts = vec![(
        "Object stolen from the office".to_string(),
        "Now the walnut case hung open and the chronometer was gone, and the \
             dust on the shelf below showed the shape of its base."
            .to_string(),
        "The Lyle-Hannett chronometer".to_string(),
    )];
    match multiquote_outcome(&parts, &[chunk], &[], &[]) {
        CitationOutcome::Grounded { answer, .. } => {
            assert!(
                answer.contains("Lyle-Hannett chronometer"),
                "an answer verbatim in the matched chunk must ground: {answer}"
            );
            assert!(
                !answer.contains("The passages do not answer"),
                "no false gap over a chunk-supported answer: {answer}"
            );
        }
        _ => panic!("chunk-supported answer must ground"),
    }
}

#[test]
fn the_chunk_widening_is_not_a_confabulation_bypass() {
    // The embassy guard this check was built on: a REAL quote, an answer
    // value the text withholds — absent from the quote AND from the whole
    // matched chunk. The widening must leave this demoted.
    let parts = vec![(
        "his posting".to_string(),
        "Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc.".to_string(),
        "Russian ambassador".to_string(),
    )];
    assert!(
        matches!(
            multiquote_outcome(&parts, &chunks(), &[], &[]),
            CitationOutcome::Abstain
        ),
        "a value absent from the matched chunk must still be refused"
    );
}

#[test]
fn every_grounded_part_keeps_its_own_quote() {
    // Regression, measured on the first arm-C run 2026-08-05: the two
    // verified sentences were joined into ONE string, so the post-hoc
    // quote_verification pass — which checks a `"..."` span as a single
    // contiguous source substring — found no chunk containing the join and
    // demoted a genuinely grounded two-part citation to
    // `[unverified excerpt: ...]`. Quotes must stay separable so each ships
    // as its own span.
    let parts = vec![
        (
            "the nickname".to_string(),
            "Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc.".to_string(),
            "the Doctor".to_string(),
        ),
        (
            "the rank".to_string(),
            "Chief Inspector Heat of the Special Crimes Department changed his tone.".to_string(),
            "Chief Inspector".to_string(),
        ),
    ];
    match multiquote_outcome(&parts, &chunks(), &[], &[]) {
        CitationOutcome::Grounded { quotes, .. } => {
            assert_eq!(quotes.len(), 2, "one span per grounded part");
            // Each is independently verbatim in the passages — which is
            // exactly what the downstream re-check demands.
            // Re-checked with the ACTUAL downstream decider, not with the
            // citation path's own (looser) one. Asserting with
            // `locate_quote_in_chunks` here is what let the two-decider
            // split hide: it agrees with itself by construction.
            for q in &quotes {
                let v = crate::quote_verification::verify_quotes(
                    &format!("\"{}\"", q.text),
                    &chunks(),
                    &[],
                    crate::quote_verification::DEFAULT_MIN_QUOTE_CHARS,
                );
                assert_eq!(
                    v.demoted_count, 0,
                    "each quote must survive the post-hoc verbatim re-check: {:?}",
                    q.text
                );
            }
        }
        _ => panic!("both parts are verbatim — both should ground"),
    }
}

#[test]
fn a_quote_inside_one_chunk_reports_that_chunk() {
    let c = chunks();
    match locate_quote_in_chunks(
        "Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc.",
        &c,
    ) {
        Some(QuoteMatch::Exact { chunk, verbatim }) => {
            assert!(
                c[chunk].contains("Ossipon"),
                "must name the chunk it is actually in"
            );
            assert!(
                c[chunk].contains(&verbatim),
                "the released span must be the source's own text: {verbatim:?}"
            );
        }
        other => panic!("expected Exact, got {other:?}"),
    }
}

/// The whole point of splitting `Exact` from `Partial`: a locator may only
/// ride a span the downstream strict re-check will keep. A run-only match
/// grounds — the decision is unchanged — but the model's span is not a
/// contiguous source substring, so it ships bare.
#[test]
fn a_run_only_match_grounds_but_is_never_attributed() {
    let c = vec![
        "Tabb greased the eastern pawls before the gulls woke properly that morning.".to_string(),
    ];
    // Verbatim in the middle, fabricated at the tail — exactly the shape
    // the tolerant run test accepts and the strict re-check refuses.
    let spliced = "greased the eastern pawls before the gulls woke and then sailed for Antwerp";
    assert!(
        matches!(
            locate_quote_in_chunks(spliced, &c),
            Some(QuoteMatch::Partial { .. })
        ),
        "a run-only match must not be reported as exact"
    );
    let (released, _answer, chunk) =
        verify_pair(None, spliced, "the eastern pawls", &c).expect("still grounds");
    assert_eq!(
        chunk, None,
        "a partial run carries no chunk, hence no locator"
    );
    assert_eq!(released, spliced, "the model's own span still ships");
    assert_eq!(
        locator_at(&[Some("CHAPTER I".into())], chunk),
        None,
        "no heading may be glued to a span the strict re-check will demote"
    );
}

/// Structural, not remembered (ARCH_PRINCIPLES §7): anything that keeps a
/// chunk index — the sole licence to print a locator — is source text, so
/// the post-hoc pass cannot demote it. This is the regression that shipped
/// `CHAPTER III — [unverified excerpt: …]` on 2026-08-05.
#[test]
fn an_attributed_quote_survives_the_strict_post_hoc_recheck() {
    let c = chunks();
    // Case-garbled copy: the citation path's test is case-insensitive, the
    // post-hoc one is not. Before the source span was released, this pair
    // grounded, earned a locator, and was then demoted downstream.
    let garbled = "ALEXANDER OSSIPON, ANARCHIST, NICKNAMED THE DOCTOR, SAT NEAR MR VERLOC.";
    let (released, answer, chunk) = verify_pair(None, garbled, "the doctor", &c).expect("grounds");
    assert!(chunk.is_some(), "a whole-quote match is attributable");
    assert_eq!(
        released, "Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc.",
        "the SOURCE's casing is released, not the model's copy"
    );
    assert_eq!(
        answer, "the Doctor",
        "the answer is re-snapped to the source"
    );
    let v = crate::quote_verification::verify_quotes(
        &format!("\"{released}\""),
        &c,
        &[],
        crate::quote_verification::DEFAULT_MIN_QUOTE_CHARS,
    );
    assert_eq!(v.demoted_count, 0, "the strict re-check must keep it");
    assert_eq!(v.verified_count, 1);
}

/// A quote that exists only across the joined passages still GROUNDS —
/// the historical behaviour — but carries no attribution, so no locator
/// is emitted. Tightening this to per-chunk-only would have moved a
/// fabrication guard while pretending to add a label.
#[test]
fn a_quote_spanning_chunks_grounds_without_attribution() {
    // The straddle has to be genuine: NO single chunk may contain a
    // MIN_VERBATIM_RUN-long window of the quote, or that chunk rightly
    // owns it. Three words each side of the boundary does it.
    let split = vec![
        "Tabb greased the eastern pawls".to_string(),
        "before the gulls woke properly.".to_string(),
    ];
    let spanning = "greased the eastern pawls before the gulls woke";
    assert_eq!(
        locate_quote_in_chunks(spanning, &split).as_ref(),
        Some(&QuoteMatch::AcrossChunks),
        "grounded, but owned by no single passage"
    );
    assert_eq!(
        locator_at(&[Some("CHAPTER I".into()), Some("CHAPTER II".into())], None),
        None,
        "an unattributable quote must never borrow a neighbour's heading"
    );
}

#[test]
fn a_locator_is_read_from_the_matching_chunks_slot() {
    let locs = vec![Some("CHAPTER I".into()), None, Some("CHAPTER III".into())];
    assert_eq!(locator_at(&locs, Some(0)).as_deref(), Some("CHAPTER I"));
    assert_eq!(locator_at(&locs, Some(2)).as_deref(), Some("CHAPTER III"));
    assert_eq!(
        locator_at(&locs, Some(1)),
        None,
        "an unjoined chunk yields nothing"
    );
}

/// A click target is read from the SAME slot as the locator, and every
/// way it can be unavailable collapses to `None` in one accessor.
///
/// The `None` chunk case is the load-bearing one: a `Partial` run
/// releases the MODEL's span rather than the source's characters, so it
/// carries no chunk. Handing it a target would open a passage that does
/// not contain the text the reader was just shown — the citation would
/// disprove itself on click.
#[test]
fn a_target_is_read_from_the_matching_chunks_slot() {
    let t = |c: &str, id: u64| {
        Some(CitationTarget {
            corpus_id: c.into(),
            chunk_id: id,
        })
    };
    let targets = vec![t("ledger", 7), None, t("ledger", 9)];
    assert_eq!(target_at(&targets, Some(0)).unwrap().chunk_id, 7);
    assert_eq!(target_at(&targets, Some(2)).unwrap().chunk_id, 9);
    assert_eq!(
        target_at(&targets, Some(1)),
        None,
        "a chunk with no stable row id yields nothing"
    );
    assert_eq!(
        target_at(&targets, None),
        None,
        "a partial run carries no chunk, hence nothing to open"
    );
    assert_eq!(
        target_at(&[], Some(0)),
        None,
        "a caller that passed no targets gets none back, not a panic"
    );
}

/// The locator flag must NOT silently make citations un-openable.
/// `SOVEREIGN_CITATION_LOCATOR` is the control arm for whether a chapter
/// NAME is displayed; whether the passage exists is a different question,
/// and coupling them would mean running the control arm quietly removed a
/// product affordance as a side effect.
#[test]
fn the_locator_flag_does_not_govern_the_click_target() {
    let targets = vec![Some(CitationTarget {
        corpus_id: "ledger".into(),
        chunk_id: 7,
    })];
    // Whatever the flag is set to in this process, the target survives —
    // `target_at` never consults it (unlike `locator_at`, which returns
    // early on it).
    assert_eq!(target_at(&targets, Some(0)).unwrap().chunk_id, 7);
}

/// Every way the locator can be unavailable collapses to `None` in ONE
/// place, so no call site has to invent its own fallback.
#[test]
fn a_missing_locator_is_none_and_never_fabricated() {
    let locs = vec![Some("CHAPTER I".into())];
    assert_eq!(locator_at(&locs, None), None, "no chunk index");
    assert_eq!(locator_at(&locs, Some(9)), None, "index past the end");
    assert_eq!(
        locator_at(&[], Some(0)),
        None,
        "corpus supplied no locators at all"
    );
}
