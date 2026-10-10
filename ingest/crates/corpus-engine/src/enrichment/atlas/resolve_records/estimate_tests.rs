use super::*;

/// A small deterministic generator, so the synthetic corpora are fixed.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// `n` pairs, a `prior` share of them matches, each source `(name, m, u,
/// speaks)` agreeing at its rate given the truth and speaking on a `speaks`
/// share of pairs. Returns the pairs and the labels.
fn synthetic(
    n: usize,
    prior: f64,
    sources: &[(&str, f64, f64, f64)],
    seed: u64,
) -> (Pairs, Vec<(Comparison, bool)>) {
    let mut rng = Lcg(seed);
    let (mut pairs, mut labelled) = (Pairs::default(), Vec::new());
    for _ in 0..n {
        let matched = rng.next() < prior;
        let mut c = Comparison::new();
        for &(s, m, u, speaks) in sources {
            if rng.next() < speaks {
                c.insert(s.to_string(), rng.next() < if matched { m } else { u });
            }
        }
        pairs.add(&c);
        labelled.push((c, matched));
    }
    (pairs, labelled)
}

/// The precision each source's agreement has on the labelled pairs: what
/// field_precision.py and score_resolve.py measure on gold.
fn labelled_precision(labelled: &[(Comparison, bool)], source: &str) -> f64 {
    let agreed: Vec<bool> = labelled
        .iter()
        .filter(|(c, _)| c.get(source) == Some(&true))
        .map(|(_, m)| *m)
        .collect();
    agreed.iter().filter(|&&m| m).count() as f64 / agreed.len() as f64
}

const THREE: [(&str, f64, f64, f64); 3] = [
    ("thread", 0.9, 0.05, 1.0),
    ("model_choice", 0.8, 0.15, 1.0),
    ("kind", 0.95, 0.4, 1.0),
];

#[test]
fn em_recovers_known_agreement_rates_with_no_labels() {
    // Three sources identify the model exactly, so a rate's sampling error
    // at 6,000 pairs is a few points (seeds 7-9 put thread's m at .84-.91);
    // the precision the decider reports stays within .05 of the labelled one.
    for seed in [7, 8, 9] {
        let (pairs, labelled) = synthetic(6000, 0.12, &THREE, seed);
        let e = Estimate::fit(&pairs);
        assert!(
            (e.prior - 0.12).abs() < 0.02,
            "seed {seed}: prior {}",
            e.prior
        );
        for (s, m, u, _) in THREE {
            let w = &e.sources[s];
            assert!((w.m - m).abs() < 0.07, "seed {seed} {s}: m {} vs {m}", w.m);
            assert!((w.u - u).abs() < 0.03, "seed {seed} {s}: u {} vs {u}", w.u);
            let gold = labelled_precision(&labelled, s);
            assert!(
                (w.precision - gold).abs() < 0.05,
                "seed {seed} {s}: estimated precision {} vs labelled {gold}",
                w.precision
            );
        }
    }
}

#[test]
fn a_source_that_agrees_as_often_on_either_class_weighs_nothing() {
    let mut sources = THREE.to_vec();
    sources.push(("document_date", 0.3, 0.3, 1.0));
    let (pairs, _) = synthetic(6000, 0.12, &sources, 11);
    let w = &Estimate::fit(&pairs).sources["document_date"];
    assert!(w.agree.abs() < 0.2 && w.disagree.abs() < 0.2, "{w:?}");
    // ... while a reliable one carries a strong weight each way.
    let t = &Estimate::fit(&pairs).sources["thread"];
    assert!(t.agree > 2.0 && t.disagree < -1.5, "{t:?}");
}

#[test]
fn a_source_silent_on_most_pairs_is_estimated_from_the_pairs_it_spoke_on() {
    let sources = [
        ("thread", 0.9, 0.05, 1.0),
        ("model_choice", 0.8, 0.15, 1.0),
        ("proposed_answer", 0.7, 0.02, 0.2),
    ];
    let (pairs, _) = synthetic(8000, 0.12, &sources, 3);
    let w = &Estimate::fit(&pairs).sources["proposed_answer"];
    assert!(
        (w.m - 0.7).abs() < 0.06 && (w.u - 0.02).abs() < 0.02,
        "{w:?}"
    );
    assert!(w.spoke > 1400 && w.spoke < 1800, "{}", w.spoke);
}

#[test]
fn with_no_pairs_or_few_nothing_weighs_without_bound() {
    let e = Estimate::none();
    assert_eq!(e.prior_log_odds(), 0.0);
    let c: Comparison = [("thread".to_string(), true)].into_iter().collect();
    assert_eq!(e.evidence(&c), 0.0);
    // Two pairs: the pseudo-counts keep every weight finite and small.
    let (pairs, _) = synthetic(2, 0.5, &THREE, 5);
    let e = Estimate::fit(&pairs);
    for w in e.sources.values() {
        assert!(w.agree.is_finite() && w.agree.abs() < 3.0, "{w:?}");
    }
}

#[test]
fn the_match_class_is_the_one_the_sources_agree_on() {
    // Matches are the majority here; EM starting from "matches are rare"
    // must still name the agreeing class the match.
    let (pairs, _) = synthetic(4000, 0.7, &THREE, 13);
    let e = Estimate::fit(&pairs);
    assert!((e.prior - 0.7).abs() < 0.05, "prior {}", e.prior);
    assert!(e.sources.values().all(|w| w.m > w.u), "{:?}", e.sources);
}

#[test]
fn evidence_sums_each_speaking_sources_weight() {
    let (pairs, _) = synthetic(6000, 0.12, &THREE, 7);
    let e = Estimate::fit(&pairs);
    let c: Comparison = [("thread".to_string(), true), ("kind".to_string(), false)]
        .into_iter()
        .collect();
    let want = e.sources["thread"].agree + e.sources["kind"].disagree;
    assert!((e.evidence(&c) - want).abs() < 1e-12);
    // An unknown source weighs nothing.
    let c: Comparison = [("unseen".to_string(), true)].into_iter().collect();
    assert_eq!(e.evidence(&c), 0.0);
}

#[test]
fn a_source_that_agrees_less_on_matches_weighs_nothing_never_against() {
    let mut sources = THREE.to_vec();
    sources.push(("necessary:term", 0.6, 0.9, 1.0));
    let (pairs, _) = synthetic(6000, 0.12, &sources, 17);
    let w = &Estimate::fit(&pairs).sources["necessary:term"];
    assert_eq!((w.agree, w.disagree), (0.0, 0.0), "{w:?}");
}

#[test]
fn sources_identical_on_every_pair_are_fitted_as_one_and_counted_once() {
    let (base, _) = synthetic(6000, 0.12, &THREE, 7);
    // `document_id` says exactly what `thread` says, on every pair.
    let mut twin = Pairs::default();
    for (pattern, &n) in &base.patterns {
        let mut c: Comparison = pattern.iter().cloned().collect();
        if let Some(&a) = c.get("thread") {
            c.insert("document_id".into(), a);
        }
        for _ in 0..n {
            twin.add(&c);
        }
    }
    let one = Estimate::fit(&base);
    let two = Estimate::fit(&twin);
    assert_eq!(two.sources["document_id"].with, ["thread"]);
    assert!((two.sources["thread"].agree - one.sources["thread"].agree).abs() < 1e-9);
    let c: Comparison = [("thread".to_string(), true), ("document_id".to_string(), true)]
        .into_iter()
        .collect();
    let alone: Comparison = [("thread".to_string(), true)].into_iter().collect();
    assert!((two.evidence(&c) - one.evidence(&alone)).abs() < 1e-9);
}
