//! pc-partial-decline-verdict: a released turn that declines the asked fact
//! while restating adjacent corpus facts earns `CannotKnowFromHere`; a turn
//! that declines and then answers keeps the verdict its holdings earn.
//!
//! The prose is the on-prem acceptance probe's ("What hourly rate does the
//! Halvorsen Marine retainer agreement set for paralegal time?" over a memo
//! giving partner and associate rates only), in the shapes phase-b-95 read.
use super::*;

fn claim(text: &str, supported: bool) -> GateClaim {
    GateClaim {
        text: text.into(),
        supported,
        failed_once: !supported,
        unjudged: false,
        violation_prob: Some(if supported { 0.02 } else { 0.9 }),
        address: None,
    }
}

fn rates() -> Vec<GateClaim> {
    vec![
        claim("The memo sets the partner rate at $412 per hour", true),
        claim("The memo sets the associate rate at $265 per hour", true),
    ]
}

fn pool() -> PoolContext {
    PoolContext {
        corpora: vec!["halvorsen-matter".into()],
        members: vec![],
    }
}

fn verdict_of(action: &str, claims: &[GateClaim], answer: Option<&str>) -> EpistemicState {
    let meta = serde_json::json!({ "action": action });
    assemble_epistemic_state(EpistemicInputs {
        gate_meta: Some(&meta),
        gate_claims: Some(claims),
        answer,
        ..EpistemicInputs::over(pool())
    })
}

/// The 35B's shape: facts first, the decline last, in an absence phrasing
/// the pure-decline zoo does not hold.
const RESTATE_THEN_DECLINE: &str = "The retainer memo sets partner time at $412 per hour \
     and associate time at $265 per hour, and does not mention a paralegal rate.";
/// The decline first, then the adjacent facts, with no contrast between.
const DECLINE_THEN_RESTATE: &str = "The sources do not contain a paralegal rate. \
     The memo sets partner time at $412 per hour and associate time at $265 per hour.";

#[test]
fn a_release_restating_verified_facts_while_declining_the_asked_one_cannot_know_from_here() {
    for prose in [RESTATE_THEN_DECLINE, DECLINE_THEN_RESTATE] {
        let state = verdict_of("released", &rates(), Some(prose));
        assert_eq!(
            state.verdict,
            TurnVerdict::CannotKnowFromHere,
            "declined the asked fact: {prose:?}"
        );
        // The restated facts are true and stay on the ledger.
        assert_eq!(state.holdings.len(), 2, "{prose:?}");
    }
}

/// The 35B's `unverified` readings: a release with no retained claims whose
/// prose declines in an absence phrasing.
#[test]
fn a_claimless_release_that_declines_the_asked_fact_cannot_know_from_here() {
    let state = verdict_of("released", &[], Some(RESTATE_THEN_DECLINE));
    assert_eq!(state.verdict, TurnVerdict::CannotKnowFromHere);
}

/// The other direction. Each of these declines and then answers, and must
/// keep the verdict its holdings earn.
#[test]
fn a_release_that_declines_and_then_answers_keeps_its_verdict() {
    // A contrast after the decline: the answer that follows is grounded.
    let pivot = "The sources do not contain a separate paralegal schedule, but the memo \
         bills paralegal time at the associate rate of $265 per hour.";
    let claims = vec![claim(
        "The memo bills paralegal time at the associate rate of $265 per hour",
        true,
    )];
    assert_eq!(
        verdict_of("released", &claims, Some(pivot)).verdict,
        TurnVerdict::Grounded
    );

    // A general-knowledge pivot: the turn answered from outside the sources.
    let gk = "The memo does not mention a paralegal rate. From general knowledge: \
         paralegals commonly bill about $150 per hour.";
    assert_ne!(
        verdict_of("released", &rates(), Some(gk)).verdict,
        TurnVerdict::CannotKnowFromHere
    );

    // An answer the corpus did not verify: the turn asserted past its sources.
    let mut claims = rates();
    claims.push(claim("Paralegal time is billed at $150 per hour", false));
    let unsupported = "The memo does not mention a paralegal rate; paralegal time is \
         billed at $150 per hour.";
    assert_eq!(
        verdict_of("released", &claims, Some(unsupported)).verdict,
        TurnVerdict::Mixed
    );
}

/// A plain grounded answer is untouched, and so is every turn the ledger
/// cannot read the text of, or that no gate audited.
#[test]
fn a_grounded_answer_and_an_unread_or_ungated_turn_are_untouched() {
    let answer = "The memo sets partner time at $412 per hour and associate time at $265.";
    assert_eq!(
        verdict_of("released", &rates(), Some(answer)).verdict,
        TurnVerdict::Grounded
    );
    assert_eq!(
        verdict_of("released", &rates(), None).verdict,
        TurnVerdict::Grounded
    );
    let ungated = assemble_epistemic_state(EpistemicInputs {
        answer: Some(RESTATE_THEN_DECLINE),
        ..EpistemicInputs::over(pool())
    });
    assert_eq!(ungated.verdict, TurnVerdict::Unverified);
}

#[test]
fn the_decline_predicate_reads_absence_phrasings_and_vetoes_answer_pivots() {
    use crate::runtime::grounding::{declines_asked_fact, released_pure_decline};
    // Absence phrasing: a partial decline, and still not a pure one, so the
    // gate's action decider is unchanged by this row.
    assert!(declines_asked_fact(RESTATE_THEN_DECLINE));
    assert!(!released_pure_decline(RESTATE_THEN_DECLINE));
    // Every pure decline is also a partial one.
    assert!(declines_asked_fact(
        "The sources do not contain that detail."
    ));
    // Declines-then-answers.
    assert!(!declines_asked_fact(
        "I'm not certain, but I think the paralegal rate is $150."
    ));
    assert!(!declines_asked_fact(
        "Not in your sources — from general knowledge: about $150 per hour."
    ));
    // An answer with no decline in it.
    assert!(!declines_asked_fact("The partner rate is $412 per hour."));
}
