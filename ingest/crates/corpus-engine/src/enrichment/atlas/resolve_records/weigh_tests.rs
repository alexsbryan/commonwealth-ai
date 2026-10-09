use super::*;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn a_lone_source_is_weighed_at_its_own_precision() {
    for (candidates, p) in [(1, 0.83), (4, 0.618), (9, 0.3)] {
        let w = weigh(candidates, &[(0, p)], 0.5);
        assert!(
            close(w.posterior[0], p),
            "K={} p={p}: {:?}",
            candidates + 1,
            w
        );
    }
    match weigh(3, &[(1, 0.6)], 0.5).zone {
        Zone::Link(1, p) => assert!(close(p, 0.6)),
        z => panic!("{z:?}"),
    }
}

#[test]
fn two_sources_below_the_bar_that_agree_link() {
    // Each alone is unsettled; together, under independence, they clear .5.
    let alone = weigh(4, &[(2, 0.45)], 0.5);
    assert_eq!(alone.zone, Zone::Unsettled);
    let both = weigh(4, &[(2, 0.45), (2, 0.45)], 0.5);
    match both.zone {
        Zone::Link(2, p) => assert!(p > 0.7, "{p}"),
        z => panic!("{z:?}"),
    }
}

#[test]
fn sources_that_disagree_weigh_against_each_other() {
    // .618 for A, .572 for B: alone either links; together neither reaches .5.
    let w = weigh(3, &[(0, 0.618), (1, 0.572)], 0.5);
    assert_eq!(w.zone, Zone::Unsettled);
    assert!(w.posterior[0] > w.posterior[1]);
    // A far more precise source still carries it.
    match weigh(3, &[(0, 0.95), (1, 0.572)], 0.5).zone {
        Zone::Link(0, _) => {}
        z => panic!("{z:?}"),
    }
}

#[test]
fn silence_or_a_source_below_one_in_k_is_no_link() {
    assert_eq!(weigh(3, &[], 0.5).zone, Zone::NoLink);
    assert_eq!(weigh(0, &[], 0.5).zone, Zone::NoLink);
    // 1 of 13 naming one of five alternatives is evidence against it.
    let w = weigh(4, &[(1, 1.0 / 13.0)], 0.5);
    assert_eq!(w.zone, Zone::NoLink);
    assert!(w.posterior[1] < 0.2 && w.none > 0.2);
    // ... and it pulls a model choice on the same record below the bar.
    assert_eq!(
        weigh(4, &[(1, 0.572), (1, 1.0 / 13.0)], 0.5).zone,
        Zone::Unsettled
    );
}

#[test]
fn a_tie_at_the_top_is_unsettled_and_the_posterior_sums_to_one() {
    let w = weigh(2, &[(0, 0.8), (1, 0.8)], 0.3);
    assert_eq!(w.zone, Zone::Unsettled);
    let w = weigh(5, &[(0, 0.7), (3, 0.6), (3, 0.2), (4, 1.0)], 0.5);
    assert!(close(w.posterior.iter().sum::<f64>() + w.none, 1.0));
    assert!(w.posterior.iter().all(|p| p.is_finite()));
}
