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
        let answer =
            "Cornell engagements [Source: Re: Cornell] and wedding photos [Source: Enron OnLine].";
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
        let answer =
            "It orchestrates eight primitives [Source: 4.16 Architectural correctness tooling].";
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
        // Below MIN_TITLE_WORDS with NO label set — too coarse to judge against a
        // partial body snapshot, so keep even if absent.
        let answer = "general [Source: Wikipedia].";
        let r = attribute_citations(answer, &enron(), &[]);
        assert_eq!(r.citations_stripped(), 0);
    }

    #[test]
    fn single_word_title_absent_with_known_labels_is_stripped() {
        // gen-ceiling step 126: a one-word `[Source: lighthouse]` cited over a
        // KNOWN label set that does not contain it, absent from the body — a
        // fabricated citation that laundered an invented fact. With the
        // authoritative label list in hand, "coarse" is no excuse: strip it.
        let body = vec![
            "Cora Finch's 1889 Fresnel design replaced the fixed lens at Sable \
             Point with a rotating array driven by a falling-weight clockwork."
                .to_string(),
        ];
        let labels = vec![
            "folder-df-fix-drive-4769b5117dd2".to_string(),
            "finch array".to_string(),
        ];
        let answer = "trained by Elias Warde [Source: lighthouse].";
        let r = attribute_citations(answer, &body, &labels);
        assert_eq!(
            r.citations_stripped(),
            1,
            "invented one-word cite must strip"
        );
        assert!(!r.cleaned.contains("lighthouse"));
        // A one-word cite whose word IS present (in the body) still survives —
        // the new strip fires only on FULL absence, exercising the Keep branch.
        let ok = "the design [Source: Finch].";
        assert_eq!(
            attribute_citations(ok, &body, &labels).citations_stripped(),
            0
        );
    }

    #[test]
    fn reformatted_real_title_is_kept() {
        // A real header cited with extra/reordered words: most words still present
        // => above the floor => released (no over-strip on legitimate rewording).
        let chunks =
            vec!["Federalist No. 51, by James Madison, on checks and balances.".to_string()];
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
        assert_eq!(
            r.citations_stripped(),
            0,
            "corpus-name citation must survive"
        );
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
        assert_eq!(
            r.citations_stripped(),
            0,
            "section-title citation must survive"
        );
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
            vec![(
                "watched-959ee8a67210".to_string(),
                "watched-959ee8a8f330".to_string()
            )]
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
            assert!(
                r.cleaned.contains("[Source: watched-959ee8a8f330]"),
                "{garble}"
            );
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
        let labels = vec![
            "watched-959ee8a8f330".to_string(),
            "watched-959ee8a8f332".to_string(),
        ];
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
        assert!(
            r.cleaned.contains("Articles II–XI"),
            "aggregate citation kept verbatim"
        );
    }

    #[test]
    fn date_garbled_label_is_stripped_by_the_composite_veto() {
        // Live sub-floor garble (padfix replay): "2026-10-10" for the real
        // "2026-06-10" scores 0.73 — under the snap floor — and its date
        // fragments ("2026","10") are too short for the word-level ID rule.
        // The whole hyphen-digit run must complete-match or the citation strips.
        let labels = vec!["Decision — 2026-06-10 — Porch Smoking".to_string()];
        let body = vec!["To settle the porch dispute, smoking moved off the porch.".to_string()];
        let r = attribute_citations(
            "rule [Source: Decision — 2026-10-10 — Porch].",
            &body,
            &labels,
        );
        assert_eq!(r.citations_snapped(), 0);
        assert_eq!(r.citations_stripped(), 1, "garbled date must strip");
        // The correctly-cited label is untouched (exact match wins first).
        let ok = "rule [Source: Decision — 2026-06-10 — Porch Smoking].";
        let r = attribute_citations(ok, &body, &labels);
        assert!(!r.changed());
    }

    // ── citation-value alignment (gen75-2026-07-02 NARA misattribution) ──

    /// The real gen75 shapes: an INDEX corpus where every chunk carries its own
    /// `NARA fileUnit N`, plus per-chunk labels (title, then corpus id).
    fn index_chunks() -> (Vec<String>, Vec<Vec<String>>) {
        let chunks = vec![
            "U.S. Air Force Project Blue Book UFO case file. Location: Project BLUE BOOK \
             (USAF SAT, 3 Feb. 1966). Status: see case file. (169 scanned pages; NARA \
             fileUnit 461458900)."
                .to_string(),
            "U.S. Air Force Project Blue Book UFO case file. Location: FALLS CHURCH, VA.. \
             Reported: 1952-01-01. Status: see case file. (1 scanned pages; NARA fileUnit \
             28940827)."
                .to_string(),
        ];
        let labels = vec![
            vec![
                "Project BLUE BOOK (USAF SAT, 3 Feb. 1966) ()".to_string(),
                "uap-blue-book-index".to_string(),
            ],
            vec![
                "FALLS CHURCH, VA. (1952-01-01)".to_string(),
                "uap-blue-book-index".to_string(),
            ],
        ];
        (chunks, labels)
    }

    #[test]
    fn misattributed_value_repoints_to_the_holder_chunk() {
        // The gen75 step-85 failure verbatim: the value belongs to the FALLS
        // CHURCH row, the citation names the SAT row.
        let (chunks, labels) = index_chunks();
        let answer = "The NARA file number for the Project Blue Book UFO case is \
                      **28940827**.\n\n[Source: Project BLUE BOOK (USAF SAT, 3 Feb. 1966) ()]";
        let r = align_citation_values(answer, &chunks, &labels);
        assert_eq!(r.realigned.len(), 1, "must re-point: {:?}", r);
        assert!(r
            .cleaned
            .contains("[Source: FALLS CHURCH, VA. (1952-01-01)]"));
        assert!(!r.cleaned.contains("USAF SAT"));
    }

    #[test]
    fn aligned_citation_is_untouched() {
        let (chunks, labels) = index_chunks();
        let answer = "The NARA file number is **461458900**.\n\n\
                      [Source: Project BLUE BOOK (USAF SAT, 3 Feb. 1966) ()]";
        let r = align_citation_values(answer, &chunks, &labels);
        assert!(!r.changed());
        assert_eq!(r.cleaned, answer);
    }

    #[test]
    fn corpus_id_citation_is_vacuously_aligned() {
        // A corpus-id label names EVERY chunk — any in-corpus value is aligned.
        let (chunks, labels) = index_chunks();
        let answer = "One case file is numbered 28940827 [Source: uap-blue-book-index].";
        let r = align_citation_values(answer, &chunks, &labels);
        assert!(!r.changed());
    }

    #[test]
    fn ambiguous_holder_strips_the_citation() {
        // The segment mixes values from BOTH chunks: no single holder — drop
        // the citation rather than guess.
        let (chunks, labels) = index_chunks();
        let answer = "The files are numbered 28940827 and 461458900 \
                      [Source: FALLS CHURCH, VA. (1952-01-01)].";
        let r = align_citation_values(answer, &chunks, &labels);
        assert_eq!(r.realigned.len(), 0);
        assert_eq!(r.stripped.len(), 1);
        assert!(!r.cleaned.contains("[Source:"));
        assert!(r.cleaned.contains("28940827 and 461458900"));
    }

    #[test]
    fn value_absent_from_all_evidence_is_not_alignments_business() {
        // A fabricated value is the value-presence gate's job — alignment must
        // not touch the citation.
        let (chunks, labels) = index_chunks();
        let answer = "The file is numbered 99999999 [Source: FALLS CHURCH, VA. (1952-01-01)].";
        let r = align_citation_values(answer, &chunks, &labels);
        assert!(!r.changed());
    }

    #[test]
    fn segment_scope_resets_at_the_previous_citation() {
        // The first sentence's value must not leak into the second citation's
        // segment: each citation is judged on ITS OWN claim span.
        let (chunks, labels) = index_chunks();
        let answer = "File 461458900 is the SAT case \
                      [Source: Project BLUE BOOK (USAF SAT, 3 Feb. 1966) ()]. File 28940827 \
                      is the Falls Church case [Source: FALLS CHURCH, VA. (1952-01-01)].";
        let r = align_citation_values(answer, &chunks, &labels);
        assert!(!r.changed(), "both correctly paired: {:?}", r);
    }

    #[test]
    fn unclosed_bracket_never_swallows_following_text() {
        // The model truncates a bracket; the next ']' lives inside LATER text
        // (here a verification-note item). The scanner must not parse ~100
        // chars as one "citation" — it recovers the REAL label at the newline
        // boundary, re-emits it properly closed, and leaves the note intact.
        let answer = "grounded [Source: public-goods\n\n---\n*Verification note:*\n\
                      - “supported by [unverified excerpt: Mill argued tolls]”";
        let r = attribute_citations(answer, &[], &["public-goods".to_string()]);
        assert!(r.cleaned.contains("[Source: public-goods]"));
        assert!(r.cleaned.contains("Verification note"));
        assert!(r
            .cleaned
            .contains("[unverified excerpt: Mill argued tolls]"));
    }

    #[test]
    fn unclosed_invented_citation_is_recovered_and_stripped() {
        // gen75b step 157 verbatim: an INVENTED date label with a forgotten `]`
        // flowed into prose and shipped raw — the bounded scanner skipped it,
        // bypassing the veto. Recovery cuts at the sentence boundary, the
        // composite date veto strips the invented title, and the prose resumes.
        let labels = vec![
            "Decision — 2026-03-28 — Guest Parking".to_string(),
            "maple-house".to_string(),
        ];
        let body = vec![
            "Guests are welcome to leave a vehicle parked in those two spaces \
                         overnight without needing any permit."
                .to_string(),
        ];
        let answer = "no permit needed \
                      [Source: Decision — 2026-12-28 — Visitor and Guest Parking. However, \
                      it is critical to note that parking is unrestricted.";
        let r = attribute_citations(answer, &body, &labels);
        assert_eq!(r.citations_stripped(), 1, "{:?}", r);
        assert!(!r.cleaned.contains("2026-12-28"));
        assert!(r.cleaned.contains("However, it is critical"));
        assert!(!r.cleaned.contains("[Source:"));
    }

    #[test]
    fn phantom_evidence_handles_are_stripped() {
        // gen75c step 126: an invented `[ev-T2-0048]` — an internal
        // evidence-handle format from another surface — reflexed into a chat
        // answer. `[passage N]` is the same family. Prose survives; markers go.
        let answer = "Winnie's reserve is described [ev-T2-0048] and again [passage 3].";
        let r = attribute_citations(answer, &[], &[]);
        assert_eq!(r.citations_stripped(), 2);
        assert_eq!(r.cleaned, "Winnie's reserve is described and again.");
        // Real bracketed prose is untouched.
        let keep = "The label [BLANK] appears in redacted scans.";
        assert_eq!(attribute_citations(keep, &[], &[]).cleaned, keep);
    }

    #[test]
    fn forgotten_close_bracket_on_a_real_label_is_repaired() {
        let labels = vec!["FALLS CHURCH, VA. (1952-01-01)".to_string()];
        let body = vec!["NARA fileUnit 28940827.".to_string()];
        let answer = "the file [Source: FALLS CHURCH, VA. (1952-01-01) has one page.";
        let r = attribute_citations(answer, &body, &labels);
        assert!(r
            .cleaned
            .contains("[Source: FALLS CHURCH, VA. (1952-01-01)]"));
        assert!(r.cleaned.contains(" has one page."));
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
