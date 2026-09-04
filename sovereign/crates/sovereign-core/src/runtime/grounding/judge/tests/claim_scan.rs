//! Claim-scan and veto tests: which spans of an answer become claims,
//! and which the gate refuses to hold against it.
//!
//! Paired with `prefix_family`, which asserts the bytes on the wire.
//! Split out of a single 1,162-line `tests.rs` so the module stays
//! inside ARCH §3.1's 800-line line.

use super::super::*;

#[test]
fn structural_specificity_fires_on_numbers_and_quotes_only() {
    // Numbers and quotations are form-level specificity — factual
    // regardless of vocabulary. (Semantic class for everything else
    // is the embed classifier's job — see claim_class_classifier
    // tests; no vocabulary assertions here by design.)
    assert!(claim_has_structural_specificity(
        "The text discusses the 1894 Greenwich bombing."
    ));
    assert!(claim_has_structural_specificity(
        "The section argues that \"esse est percipi\" grounds idealism."
    ));
    assert!(!claim_has_structural_specificity(
        "The text explores the theme of betrayal within the family."
    ));
    assert!(!claim_has_structural_specificity("Verloc runs a shop."));
}

#[test]
fn batched_verdicts_align_by_number_and_fallback_on_gaps() {
    // Clean case: all rows present, mixed A/B, tolerant separators.
    let v = parse_batched_verdicts("1: A\n2. B\n3) A", 3);
    assert_eq!(v, vec![Some(true), Some(false), Some(true)]);
    // Out-of-order lines still land on the right claim (numbering, not position).
    let v = parse_batched_verdicts("2: B\n1: A", 2);
    assert_eq!(v, vec![Some(true), Some(false)]);
    // A missing row stays None (caller re-verifies with the calibrated pass);
    // a bullet-prefixed / prose-wrapped line is tolerated.
    let v = parse_batched_verdicts("- 1: A\n3: B", 3);
    assert_eq!(v, vec![Some(true), None, Some(false)]);
    // Out-of-range index is ignored (no panic, no shifted verdict).
    let v = parse_batched_verdicts("1: A\n9: B", 2);
    assert_eq!(v, vec![Some(true), None]);
    // Ambiguous verdict token → None, not a coin-flip.
    let v = parse_batched_verdicts("1: maybe\n2: B", 2);
    assert_eq!(v, vec![None, Some(false)]);
}

/// The artifact gate is a WORD gate, not a substring gate.
///
/// Watched failing on a live desktop turn 2026-08-13: "Harry Frankfurt
/// designed cases intended to prove moral responsibility does not require
/// alternate possibilities" was vetoed as a fabricated in-world
/// attribution. The gate opened because "de-SIGNED" contains "signed", and
/// the bigram check then flagged "Harry Frankfurt" — a philosopher named
/// in four of the turn's own chunks — because the corpus writes the
/// surname alone. That single veto was the only thing between that turn
/// and a zero-failure turn.
///
/// Every string below is ordinary essay prose. Before the fix each one
/// opened a veto meant for claims about emails, letters and source files.
#[test]
fn artifact_gate_matches_whole_words_not_substrings() {
    let hay = "frankfurt cases are the primary compatibilist response.";
    // "designed" ⊃ "signed" — the live case.
    assert_eq!(
        absent_name_attribution("Harry Frankfurt designed cases about responsibility.", hay),
        None,
        "\"designed\" must not open the artifact gate via \"signed\""
    );
    // "present" / "represent" / "consent" / "absent" / "sentence" ⊃ "sent"
    for prose in [
        "Peter Strawson present arguments about reactive attitudes.",
        "Galen Strawson represent the basic-argument position.",
        "Susan Wolf absent from this particular debate entirely.",
        "Robert Kane sentence structures favour event-causal accounts.",
    ] {
        assert_eq!(
            absent_name_attribution(prose, hay),
            None,
            "ordinary prose must not open the artifact gate: {prose:?}"
        );
    }
    // "classical" ⊃ "class", "denotes" ⊃ "notes" — identifier sibling.
    assert_eq!(
        absent_identifier_attribution(
            "Classical compatibilism denotes the Hobbes-Hume position.",
            hay
        ),
        None,
        "\"classical\"/\"denotes\" must not open the identifier gate"
    );
    // ...and the gate still OPENS on the real thing it was built for.
    assert_eq!(
        absent_name_attribution(
            "Betty Alexander sent an email about the schedule.",
            "unrelated evidence with no such person"
        ),
        Some("Betty Alexander".to_string()),
        "a genuine in-world artifact attribution must still be vetoed"
    );
}

#[test]
fn name_sweep_skips_citation_labels_and_boilerplate() {
    // The persona-QA self-indictment class (2026-07-10): label fragments
    // and header bigrams flagged as fabricated names.
    assert_eq!(
        absent_name_attribution(
            "The passage discusses effects as documented [Source: Psilocybin Mushrooms — Effects]",
            "some unrelated evidence text"
        ),
        None
    );
    assert_eq!(
        absent_name_attribution(
            "From Retrieved Sources: the document describes the mechanism in a later section.",
            "some unrelated evidence text"
        ),
        None
    );
    // Heading bigrams and comma-separated name lists are not names
    // (overnight soak receipts).
    assert_eq!(
        absent_name_attribution(
            "**Energy Costs**: The document describes rate changes for households.",
            "unrelated evidence"
        ),
        None
    );
    assert_eq!(
        absent_name_attribution(
            "The letter was signed by Hamilton, Madison and Jay together.",
            "hamilton wrote often. madison replied. jay concurred."
        ),
        None
    );
    // Surname + capitalized pronoun is not a name ("Webber He
    // averaged…" — observed live).
    assert_eq!(
        absent_name_attribution(
            "The document states Webber He averaged 19.1 points per game.",
            "webber averaged 19.1 points"
        ),
        None
    );
    // Positive control: a genuine in-world attribution absent from
    // evidence still trips the veto.
    assert_eq!(
        absent_name_attribution(
            "The email was sent by Betty Alexander to the finance team.",
            "totally different evidence"
        ),
        Some("Betty Alexander".to_string())
    );
    // Unclosed bracket strips to end-of-line, not end-of-answer.
    assert_eq!(
        absent_name_attribution(
            "cited in [Source: Broken Label\nThe letter was written by Elowen Marsh yesterday.",
            "nothing relevant"
        ),
        Some("Elowen Marsh".to_string())
    );
}

#[test]
fn self_referential_declines_are_exempt() {
    // The two live-observed rejection shapes (persona-QA 2026-07-10).
    assert!(is_self_referential_decline(
        "The system does not have access to real-time earthquake or tsunami data for Japan."
    ));
    assert!(is_self_referential_decline(
            "As of 2026-07-10, there is no evidence that the assistant's capabilities include live seismic feeds."
        ));
    assert!(is_self_referential_decline(
        "The provided passages do not contain real-time viewership data."
    ));
    // Markdown-decorated variant (scan findings arrive with emphasis).
    assert!(is_self_referential_decline(
        "**The system does **not** have access to real-time earthquake data"
    ));
}

#[test]
fn world_claims_are_not_exempt() {
    assert!(!is_self_referential_decline(
        "Azelaic acid inhibits tyrosinase and has anti-inflammatory properties."
    ));
    assert!(!is_self_referential_decline(
        "Family Guy remains a consistent driver of engagement on Hulu."
    ));
    // System-subject but AFFIRMATIVE (not a decline) stays in jurisdiction.
    assert!(!is_self_referential_decline(
        "The system retrieves twelve chunks per query."
    ));
}

const ANSWER: &str = "Robinson attacked aggregate production functions and \
        neoclassical production theory more broadly, a task she showed to be \
        circular reasoning [Source: Joan Robinson]. The lighthouse also appears \
        as a title of James Joyce's novel.";

#[test]
fn quoted_answer_span_is_extracted() {
    // The observed live shape: the model wraps the span in quotes and
    // appends judgment chatter after an em-dash.
    let item = "\"and neoclassical production theory more broadly\" — The \
                    evidence does not mention this";
    assert_eq!(
        anchor_scan_item(item, ANSWER).as_deref(),
        Some("and neoclassical production theory more broadly")
    );
}

#[test]
fn dash_appended_commentary_is_cut() {
    let item = "a task she showed to be circular reasoning — not stated in the sources";
    assert_eq!(
        anchor_scan_item(item, ANSWER).as_deref(),
        Some("a task she showed to be circular reasoning")
    );
}

#[test]
fn ascii_hyphen_appended_commentary_is_cut() {
    // The shape the live judge actually emitted on the measured turn: a
    // plain " - ", which the em/en-dash list did not cover.
    let item = "a task she showed to be circular reasoning - the evidence does not say this";
    assert_eq!(
        anchor_scan_item(item, ANSWER).as_deref(),
        Some("a task she showed to be circular reasoning")
    );
}

#[test]
fn abstractive_finding_is_not_a_claim() {
    // REVERSED 2026-08-08. This case used to pass through unchanged, on
    // the reasoning that an abstractive finding still guides the
    // corrective search. It does — but the same value is ALSO recorded
    // as a `failed_once` holding and listed in the user's verification
    // note, and there it is the judge talking about the answer rather
    // than a claim the answer made. The search hint is not worth a false
    // holding; see `judge_commentary_never_becomes_a_claim` for the
    // transcript this was measured on.
    let item = "The answer claims there is no single item explicitly labeled";
    assert_eq!(anchor_scan_item(item, ANSWER), None);
}

#[test]
fn curly_quotes_are_handled() {
    let item = "“The lighthouse also appears as a title of James Joyce's novel” — misattributed";
    assert_eq!(
        anchor_scan_item(item, ANSWER).as_deref(),
        Some("The lighthouse also appears as a title of James Joyce's novel")
    );
}

#[test]
fn emphasis_markers_do_not_hide_an_answer_span() {
    // The judge drops the answer's `**bold**` when it re-quotes. Anchoring
    // must see through that, or a real span falls off the ladder.
    let ans = "Corwin Pellow was murdered by **Severin Quenholt**, the broker.";
    let item = "\"Corwin Pellow was murdered by Severin Quenholt\" - not in the evidence";
    assert_eq!(
        anchor_scan_item(item, ans).as_deref(),
        Some("Corwin Pellow was murdered by Severin Quenholt")
    );
}

#[test]
fn an_elided_quote_anchors_on_its_prefix() {
    let ans = "The killing took place at the inn on a pleasant evening in summer, \
                   where he sat with his usual glass and agreed with neighbors.";
    let item = "\"The killing took place at the inn on a pleasant evening in summer, \
                    where he sat with his usual glass...\" - This is fabricated.";
    assert_eq!(
        anchor_scan_item(item, ans).as_deref(),
        Some(
            "The killing took place at the inn on a pleasant evening in summer, \
                 where he sat with his usual glass"
        )
    );
}

#[test]
fn a_stitched_quote_is_not_salvaged_into_a_fragment() {
    // An INTERIOR ellipsis means the judge spliced two spans and appended
    // a verdict. Anchoring must reject it rather than reduce it to the
    // bare name in front — that name is not the claim.
    let ans = "Severin Quenholt was the broker. Corwin Pellow was the harbormaster.";
    let item = "\"Severin Quenholt... As harbormaster, his signature validated salvage \
                    lots.\" (Misattribution: the text identifies Corwin Pellow as harbormaster.)";
    assert_eq!(anchor_scan_item(item, ans), None);
}

#[test]
fn legitimate_em_dash_inside_a_present_item_is_kept() {
    // The whole item occurs in the answer -> no cut at its interior dash.
    let ans = "The rule — quiet hours after ten — is strict.";
    let item = "The rule — quiet hours after ten — is strict.";
    assert_eq!(
        anchor_scan_item(item, ans).as_deref(),
        Some("The rule — quiet hours after ten — is strict.")
    );
}

#[test]
fn quoted_spans_extraction_walks_pairs() {
    let spans = extract_quoted_spans(r#"cites "[Source: x]" for "the atomic idea" here"#);
    assert_eq!(spans, vec!["[Source: x]", "the atomic idea"]);
}

// ---- The judge-prose defect, replayed from the transcript that shipped it.
//
// Provenance and the byte-identity check: `testdata/README.md`.
// `saltgrass_compound_gv_shadow_20260808.transcripts.jsonl`, turn
// `compound-killer-and-lugger`. Three of that turn's five `failed_once`
// holdings were the specifics scan's own commentary, and the user read
// them — in the ledger AND in the appended verification note — as their
// answer's failed claims.

/// The draft body the specifics scan audited (released answer, minus the
/// verification note the gate appended afterwards).
const POLLUTED_ANSWER: &str = include_str!("../../testdata/polluted_answer.md");
/// The scan's raw reply, one judge line per line.
const POLLUTED_SCAN_REPLY: &str = include_str!("../../testdata/polluted_scan_items.txt");
/// The three prose rows exactly as the ledger recorded them.
const POLLUTED_HOLDINGS: &str = include_str!("../../testdata/polluted_holdings.txt");

#[test]
fn judge_commentary_never_becomes_a_claim() {
    let items = scan_items_from_reply(POLLUTED_SCAN_REPLY, POLLUTED_ANSWER, 12);
    for prose in POLLUTED_HOLDINGS.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            !items.iter().any(|i| i == prose),
            "the ledger's judge-prose holding came back as a claim: {:?}\n\
                 items: {items:#?}",
            prose.chars().take(90).collect::<String>()
        );
    }
}

#[test]
fn every_scan_item_is_a_span_of_the_answer() {
    // The positive half of the contract: whatever survives must be
    // wording the ANSWER used, not wording the judge used. Compared
    // modulo emphasis markers, because the judge re-quotes
    // `**Severin Quenholt**` as `Severin Quenholt`.
    let strip = |s: &str| -> String {
        s.to_lowercase()
            .chars()
            .filter(|c| !matches!(c, '*' | '_' | '`'))
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let ans = strip(POLLUTED_ANSWER);
    for item in scan_items_from_reply(POLLUTED_SCAN_REPLY, POLLUTED_ANSWER, 12) {
        assert!(
            ans.contains(&strip(&item)),
            "scan item is not a span of the answer: {:?}",
            item.chars().take(90).collect::<String>()
        );
    }
}

#[test]
fn the_turns_real_claims_survive_the_filter() {
    // Guard against over-correcting into silence: the two spans the
    // answer genuinely asserted are still flagged.
    let items = scan_items_from_reply(POLLUTED_SCAN_REPLY, POLLUTED_ANSWER, 12);
    assert_eq!(items.len(), 2, "expected 2 answer spans, got {items:#?}");
    assert!(items
        .iter()
        .any(|i| i == "Corwin Pellow was murdered by Severin Quenholt"));
    assert!(items
        .iter()
        .any(|i| i.starts_with("The killing took place at *The Cold Lantern* inn")));
}

#[test]
fn unverified_excerpt_wrappers_unwrap_to_content() {
    let s = "It holds [unverified excerpt: As Samuelson (1954) noted, free-riding \
                 justifies provision] and more.";
    assert_eq!(
        unwrap_unverified_excerpts(s),
        "It holds As Samuelson (1954) noted, free-riding justifies provision and more."
    );
    // Unclosed wrapper survives verbatim (never destroy text).
    let broken = "tail [unverified excerpt: cut off";
    assert_eq!(unwrap_unverified_excerpts(broken), broken);
    // No wrapper → untouched.
    assert_eq!(unwrap_unverified_excerpts("plain"), "plain");
}

#[test]
fn in_world_attribution_with_absent_name_is_vetoed() {
    let hay = "ok, jeff, you requested that we be candid about enron. rosalee \
                   fleming forwarded this to kenneth lay."
        .to_string();
    // The measured ghost: cleared at vp=0.010 by the joint judge.
    assert_eq!(
        absent_name_attribution(
            "Betty Alexander sent an email to Jeff Skilling on July 7, 2000.",
            &hay
        ),
        Some("Betty Alexander".to_string())
    );
    // A present name passes to the judge.
    assert_eq!(
        absent_name_attribution("Rosalee Fleming forwarded the email to Kenneth Lay.", &hay),
        None
    );
    // No artifact noun → general-knowledge territory → never vetoed
    // (do not shackle the model).
    assert_eq!(
        absent_name_attribution(
            "Noam Cohen called Wikipedia the last best place on the Internet.",
            &hay
        ),
        None
    );
    // Acronyms/date fragments are not name bigrams.
    assert_eq!(
        absent_name_attribution("The email was escalated to HR VP leadership in July.", &hay),
        None
    );
}

#[test]
fn absent_identifier_attribution_is_vetoed() {
    let hay = "the step kind enum defines reason, tool, user, plan, act, and                    awaituserinfo. see planner.rs and cmd_design."
            .to_string();
    // gen75c ghosts: invented snake_case fn + invented file + invented variant.
    assert_eq!(
        absent_identifier_attribution("The material centers on the cmd_init function.", &hay),
        Some("cmd_init".to_string())
    );
    assert_eq!(
        absent_identifier_attribution("The file design_signals.rs defines the gaps.", &hay),
        Some("design_signals.rs".to_string())
    );
    assert_eq!(
        absent_identifier_attribution("The StepKind enum values include ReasonWithTools.", &hay),
        Some("ReasonWithTools".to_string())
    );
    // Present identifiers pass (case-insensitive), including real variants.
    assert_eq!(
        absent_identifier_attribution("The enum defines AwaitUserInfo as a variant.", &hay),
        None
    );
    assert_eq!(
        absent_identifier_attribution("The file planner.rs holds the logic.", &hay),
        None
    );
    // No artifact context → GK territory → untouched.
    assert_eq!(
        absent_identifier_attribution("React's useStateHook pattern is popular.", &hay),
        None
    );
}

#[test]
fn wrapped_scan_item_is_judged_on_content() {
    // A scan item echoing the app's own wrapper must reduce to the span
    // content so the note never lists a double-wrapped self-indictment.
    let answer = "The gate held [unverified excerpt: ships cannot pay tolls at sea] today.";
    let item = "[unverified excerpt: ships cannot pay tolls at sea]";
    assert_eq!(
        anchor_scan_item(item, answer).as_deref(),
        Some("ships cannot pay tolls at sea")
    );
}

/// The scalpel's two arms and — load-bearing — what it must NOT exempt.
/// The step-91 shape (2026-07-21 soak): decline headline + a POSITIVE
/// meta-rider about the passages, which the negation-requiring longform
/// predicate deliberately lets through, burned 16 per-passage checks +
/// a doomed retry. The conjunction (decline headline AND meta subject)
/// exempts it; a world-claim rider keeps its audit.
#[test]
fn decline_rider_exemption_scalpel() {
    let decline_answer = "I don't have reliable information on this. The \
             provided passages are Rust source code snippets from a \
             corpus-engine project.";
    // Arm 2: positive evidence-meta rider under a decline headline → exempt.
    assert!(decline_rider_exempt(
        decline_answer,
        "The provided passages are Rust source code snippets from a corpus-engine project."
    ));
    // World-claim rider under the same decline headline → NOT exempt
    // (subject is the world, must stay audited).
    assert!(!decline_rider_exempt(
        "I don't have reliable information on this. However, John Smith sent the memo.",
        "John Smith sent the memo on May 5."
    ));
    // No decline headline → a positive meta-shaped claim is NOT exempt
    // via arm 2 (the decline supplies the safety).
    assert!(!decline_rider_exempt(
        "The passages are Rust source code snippets.",
        "The passages are Rust source code snippets."
    ));
    // Arm 1: a negated self-referential decline claim is exempt
    // regardless of the answer's headline (longform-established shape).
    assert!(decline_rider_exempt(
        "Summary of what I found.",
        "The sources do not contain information about the lamp mechanism."
    ));
    // Markdown emphasis must not defeat the subject/negation matching.
    assert!(decline_rider_exempt(
        "I don't have reliable information on this.",
        "The **provided** passages are configuration files."
    ));
    // Pronoun-subject world claim under an answer that merely CONTAINS
    // a decline phrase ("does not contain") — the loose "it " prefix is
    // negation-guarded and must NOT satisfy the negation-free rider arm.
    assert!(!decline_rider_exempt(
        "The report does not contain the exact date, but John sent it in May.",
        "It was sent in May."
    ));
}
