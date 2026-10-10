use super::*;

/// Every alternative with evidence above 0 raised by an agreeing source.
fn post(evidence: &[f64], prior: f64, bar: Option<f64>) -> Weighed {
    let raised: Vec<bool> = evidence.iter().map(|&e| e > 0.0).collect();
    weigh(evidence, &raised, prior, bar)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn evidence_and_the_prior_are_the_alternatives_log_odds_against_none() {
    // One alternative, evidence ln 4, even prior: odds 4 to 1.
    let w = post(&[4f64.ln()], 0.0, Some(0.5));
    assert!(close(w.posterior[0], 0.8) && close(w.none, 0.2), "{w:?}");
    assert!(matches!(w.zone, Zone::Link(0, p) if close(p, 0.8)));
    // A prior of 1 in 5 (ln 1/4) cancels it: even.
    let w = post(&[4f64.ln()], 0.25f64.ln(), Some(0.5));
    assert!(close(w.posterior[0], 0.5));
}

#[test]
fn two_sources_that_agree_add_and_a_disagreeing_one_subtracts() {
    let alone = post(&[0.5, 0.0, 0.0], -1.0, Some(0.5));
    assert_eq!(alone.zone, Zone::Unsettled);
    match post(&[0.5 + 2.5, 0.0, 0.0], -1.0, Some(0.5)).zone {
        Zone::Link(0, p) => assert!(p > 0.6, "{p}"),
        z => panic!("{z:?}"),
    }
    // A disagreeing source (a read value that differs) pulls it back below.
    assert_ne!(
        post(&[0.5 + 2.5 - 2.0, 0.0, 0.0], -1.0, Some(0.5)).zone,
        Zone::Link(0, 0.0)
    );
}

#[test]
fn silence_or_only_disagreement_is_no_link_and_a_confident_none_opens() {
    assert_eq!(post(&[], 0.0, Some(0.5)).zone, Zone::NoLink);
    assert_eq!(post(&[0.0, 0.0], -1.0, Some(0.5)).zone, Zone::NoLink);
    assert_eq!(post(&[-2.0, -0.5], -1.0, Some(0.5)).zone, Zone::NoLink);
    // Raised a little against a low prior: none holds .5 or more, so it opens.
    let w = post(&[0.3], -3.0, Some(0.5));
    assert!(w.none > 0.9);
    assert_eq!(w.zone, Zone::NoLink);
}

#[test]
fn with_no_bar_the_most_probable_decides_and_nothing_is_held() {
    // .45 / .45 / .10 none: a tie, so nothing strictly ahead: opens.
    assert_eq!(post(&[1.5, 1.5], 0.0, None).zone, Zone::NoLink);
    // Ahead of none and of the other, under .5: links with no bar.
    match post(&[1.0, 0.5, 0.2], 0.0, None).zone {
        Zone::Link(0, p) => assert!(p < 0.5, "{p}"),
        z => panic!("{z:?}"),
    }
    // Raised but none still ahead: opens.
    assert_eq!(post(&[0.5], -2.0, None).zone, Zone::NoLink);
}

#[test]
fn the_posterior_sums_to_one_and_stays_finite() {
    let w = post(&[40.0, -30.0, 3.0, 0.0], -2.0, Some(0.5));
    assert!(close(w.posterior.iter().sum::<f64>() + w.none, 1.0));
    assert!(w.posterior.iter().all(|p| p.is_finite()));
}

#[test]
fn an_alternative_no_source_agreed_with_never_links() {
    // Evidence above 0 (a source disagreeing at a positive weight), yet
    // nothing agreed with it: it may rise, it does not link.
    let lone = weigh(&[1.5, -3.0], &[false, false], 0.0, None);
    assert_eq!(lone.zone, Zone::NoLink);
    let other = weigh(&[1.5, 0.2], &[false, true], 0.0, Some(0.5));
    assert!(!matches!(other.zone, Zone::Link(0, _)), "{other:?}");
}
