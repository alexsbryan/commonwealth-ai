use super::*;
use understanding_vocab::ontology::WalkPolicy;

/// Three orthogonal centroids and a query sitting on one of them: the
/// classifier picks it. Failing input: swap `dot` for a distance.
#[test]
fn a_query_on_a_centroid_classifies_to_its_kind() {
    let c = QuestionKindClassifier::from_centroids(vec![
        (QuestionKind::Thematic, vec![1.0, 0.0, 0.0]),
        (QuestionKind::Tension, vec![0.0, 1.0, 0.0]),
        (QuestionKind::Lookup, vec![0.0, 0.0, 1.0]),
    ]);
    let (kind, score) = c.classify(&[0.0, 1.0, 0.0]);
    assert_eq!(kind, Some(QuestionKind::Tension));
    let score = score.unwrap();
    assert!(score.sim > 0.99, "sim {}", score.sim);
    assert!(score.margin > 0.99, "margin {}", score.margin);
}

/// A query equidistant between two kinds ABSTAINS rather than picking the
/// one that sorts first. This is the gate that keeps a between-kinds
/// question on the unfiltered row instead of walking the wrong edges.
/// Failing input: drop the `min_margin` term from `classify`.
#[test]
fn a_question_between_two_kinds_abstains() {
    let c = QuestionKindClassifier::from_centroids(vec![
        (QuestionKind::Thematic, vec![1.0, 0.0]),
        (QuestionKind::Trajectory, vec![0.0, 1.0]),
    ]);
    let (kind, score) = c.classify(&[1.0, 1.0]);
    assert_eq!(kind, None, "an equidistant query must not be forced");
    let score = score.unwrap();
    assert!(score.margin.abs() < 1e-5, "margin {}", score.margin);
}

/// Far from every centroid is also an abstain — the absolute floor, not
/// just the margin. Failing input: drop the `min_sim` term.
#[test]
fn a_query_far_from_every_kind_abstains() {
    let c = QuestionKindClassifier::from_centroids(vec![
        (QuestionKind::Thematic, vec![1.0, 0.0, 0.0]),
        (QuestionKind::Tension, vec![0.0, 1.0, 0.0]),
    ])
    .with_gates(0.34, 0.05);
    // Closer to the first by a wide margin — the margin gate PASSES — so
    // only the similarity floor can reject it. (A first attempt used
    // [0.2, 0.02, 5.0], whose margin is 0.036 and which therefore failed
    // on the wrong gate: it would have passed even with `min_sim`
    // deleted.)
    let (kind, score) = c.classify(&[0.3, 0.0, 1.0]);
    let score = score.unwrap();
    assert!(score.margin > 0.05, "margin {} must clear", score.margin);
    assert!(
        score.sim < 0.34,
        "sim {} must be under the floor",
        score.sim
    );
    assert_eq!(kind, None);
}

/// A query embedded in a DIFFERENT space is unanswerable, not
/// low-scoring. Failing input: let `best` zip mismatched widths.
#[test]
fn a_width_mismatch_is_unanswerable_not_a_low_score() {
    let c =
        QuestionKindClassifier::from_centroids(vec![(QuestionKind::Thematic, vec![1.0, 0.0, 0.0])]);
    assert!(c.best(&[1.0, 0.0]).is_none());
    let (kind, score) = c.classify(&[1.0, 0.0]);
    assert!(kind.is_none());
    assert!(score.is_none(), "a mismatch has no score to report either");
}

/// A map with no exemplars on any row yields NO classifier — not an
/// empty one that would classify everything to the first kind.
/// Failing input: return `Some` with zero centroids from `build`.
#[tokio::test]
async fn a_map_with_no_exemplars_builds_no_classifier() {
    let mut policy = NavigationPolicy::default();
    for row in [
        &mut policy.thematic,
        &mut policy.trajectory,
        &mut policy.tension,
        &mut policy.enumeration,
        &mut policy.lookup,
    ] {
        row.exemplars.clear();
    }
    assert!(policy.classifiable().is_empty());
    let embed: EmbedFn = std::sync::Arc::new(|_: &str| {
        Box::pin(async { Ok(vec![1.0_f32, 0.0, 0.0]) })
            as std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<f32>>> + Send>>
    });
    assert!(QuestionKindClassifier::build(&policy, &embed)
        .await
        .unwrap()
        .is_none());
}

/// The default map is fully classifiable — all five rows carry
/// exemplars, so no kind is unreachable by construction. Failing input:
/// drop `exemplars` from any default row.
#[test]
fn every_default_row_is_classifiable() {
    let policy = NavigationPolicy::default();
    assert_eq!(policy.classifiable().len(), QuestionKind::ALL.len());
    // …and the unfiltered row is deliberately NOT one of them.
    assert!(WalkPolicy::unfiltered().exemplars.is_empty());
}

/// A corpus that switches one row off by emptying its exemplars keeps the
/// other four, and the switched-off kind can never be classified onto.
#[test]
fn an_emptied_row_is_switched_off_not_deleted() {
    let mut policy = NavigationPolicy::default();
    policy.enumeration.exemplars.clear();
    let kinds: Vec<QuestionKind> = policy.classifiable().into_iter().map(|(k, _)| k).collect();
    assert!(!kinds.contains(&QuestionKind::Enumeration));
    assert_eq!(kinds.len(), 4);
    // The ROW is still there to be walked when a caller names the kind.
    assert_eq!(policy.walk(QuestionKind::Enumeration).hops, 0);
}

/// Two maps with the same exemplars share a cache key; changing one
/// phrase changes it. Failing input: key the cache on a corpus id.
#[test]
fn the_cache_key_is_the_exemplars_not_an_address() {
    let a = NavigationPolicy::default();
    let b = NavigationPolicy::default();
    assert_eq!(exemplar_key(&a), exemplar_key(&b));
    let mut c = NavigationPolicy::default();
    c.thematic.exemplars.push("what is the gist".into());
    assert_ne!(exemplar_key(&a), exemplar_key(&c));
}

/// THE seam test. Build the centroids and classify a question through the
/// same embedder, then look at every string the embedder actually saw:
/// all of them must carry the classifier instruction. This is the one
/// invariant whose violation produced the defect — exemplars in one
/// space, query in another, both sides scoring happily.
///
/// Failing input: embed the query as `(embed)(question)` instead of
/// through `kind_space_embedding`, and the last recorded string is the
/// bare question. That is exactly the shape the code had before
/// 2026-09-08, except that there the ASYMMETRY was hidden one level down
/// in which adapter the caller passed.
#[tokio::test]
async fn the_exemplars_and_the_query_are_embedded_in_one_space() {
    use sovereign_contracts::embed_quirks::CLASSIFIER_INSTRUCTION;
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let embed: EmbedFn = Arc::new(move |t: &str| {
        if let Ok(mut g) = sink.lock() {
            g.push(t.to_string());
        }
        Box::pin(async { Ok(vec![1.0_f32, 0.0, 0.0]) })
            as std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<f32>>> + Send>>
    });
    let policy = NavigationPolicy::default();
    let c = QuestionKindClassifier::build(&policy, &embed)
        .await
        .expect("build")
        .expect("the default map is classifiable");
    c.classify_question("who is this character", &embed)
        .await
        .expect("classify");

    let seen = seen.lock().expect("recorder");
    assert!(
        seen.len() > 1,
        "expected exemplar embeds plus the query, got {}",
        seen.len()
    );
    for text in seen.iter() {
        assert!(
            text.starts_with(CLASSIFIER_INSTRUCTION),
            "every side of the comparison must be in the classifier space; \
             this one was not: {text:?}"
        );
    }
    assert!(
        seen.last()
            .expect("query")
            .ends_with("who is this character"),
        "the query itself must be the last thing embedded"
    );
}

/// A query vector from ANOTHER space is refused by the floor rather than
/// ranked. The measured band: scored against speech-act centroids, a
/// retrieval-prefixed query wins at 0.135-0.324 and an unprefixed one at
/// 0.151-0.463, while anything actually in the space wins at 0.765-0.965.
///
/// In the CENTRED space (2026-09-22) a cross-space query's signature is a
/// vector pointing at the cone mean: subtracting the mean leaves ~zero
/// length and every cosine collapses to ~0. Measured on the live banks:
/// in-space winners score 0.13-0.57, a mean-directed query ~0.
///
/// Failing input: `[1.0, 0.0, 0.0]` against two centroids symmetric about
/// it — exactly the mean direction. Delete the `min_sim` term and this
/// zero-vector query ties for every kind and ranks one arbitrarily.
#[test]
fn a_cross_space_query_vector_is_refused_not_ranked() {
    let c = QuestionKindClassifier::from_centroids(vec![
        (QuestionKind::Thematic, vec![1.0, 0.2, 0.0]),
        (QuestionKind::Tension, vec![1.0, -0.2, 0.0]),
    ]);
    // Dead on the shared (mean) axis: centred, it is the zero vector.
    let (kind, score) = c.classify(&[1.0, 0.0, 0.0]);
    let score = score.expect("a score is still reported, so the log can name it");
    assert!(
        score.sim < KIND_MIN_SIM,
        "sim {} must land under the same-space floor",
        score.sim
    );
    assert_eq!(kind, None);
}

/// Centring restores the sim floor's discriminating power — the measured
/// upgrade of 2026-09-22.
///
/// In the UNCENTRED cone this test pinned the opposite: noise and real
/// questions interleaved on `sim` (gibberish 0.75-0.91 against real
/// 0.765-0.923), so no floor could separate them and the margin had to do
/// all the work. Centred, a noise query dead on the shared axis IS the
/// mean direction and collapses to sim ~0, while a real question's
/// off-axis content survives the subtraction at 0.13-0.57. The floor can
/// finally reject one and admit the other, and the margin agrees.
///
/// Failing input: delete the centring in `race`/`from_centroids` and the
/// noise query scores ~0.9 again — the interleaving returns, asserted.
#[test]
fn centring_lets_the_floor_separate_noise_from_a_real_question() {
    let c = QuestionKindClassifier::from_centroids(vec![
        (QuestionKind::Thematic, vec![1.0, 0.2, 0.0]),
        (QuestionKind::Lookup, vec![1.0, -0.2, 0.0]),
    ]);
    // Noise: dead on the shared axis — the cone mean itself.
    let noise = c.best(&[1.0, 0.0, 0.0]).expect("scored");
    // A real question: off the shared axis, carrying content the
    // centroids do not (the third component).
    let real = c.best(&[0.85, 0.10, 0.45]).expect("scored");

    assert!(
        noise.sim < KIND_MIN_SIM,
        "the mean-directed noise must fall under the floor: {}",
        noise.sim
    );
    assert!(
        real.sim >= KIND_MIN_SIM,
        "the real question must clear the floor: {}",
        real.sim
    );
    assert!(
        real.margin >= KIND_MIN_MARGIN,
        "and the margin, both directions: {}",
        real.margin
    );
}

/// The gates are the ones calibrated in the CENTRED classifier space
/// (2026-09-22 refit), not the uncentred pair they replaced. A silent
/// revert to 0.50/0.02 would put the floor back above every measured
/// in-space winner (0.13-0.57) and the margin back under the ANS bank's
/// weakest winning margin (0.032) — refusing real questions wholesale,
/// which is the defect this refit fixed.
#[test]
fn the_gates_are_the_centred_space_pair() {
    let c = QuestionKindClassifier::from_centroids(vec![(QuestionKind::Thematic, vec![1.0, 0.0])]);
    assert_eq!(c.gates(), (KIND_MIN_SIM, KIND_MIN_MARGIN));
    assert_eq!(KIND_MIN_SIM, 0.05);
    assert_eq!(KIND_MIN_MARGIN, 0.025);
    assert!(
        KIND_MIN_SIM < 0.129,
        "the floor must sit under the lowest measured in-space winner (ANS, 0.129)"
    );
    assert!(
        KIND_MIN_MARGIN < 0.032,
        "the margin must admit the ANS bank's weakest winning margin (0.032)"
    );
    assert!(
        KIND_MIN_MARGIN > 0.008,
        "…and refuse the near-boundary control measured at 0.008"
    );
}

/// Every degradation reads as one, and a real classification does not.
#[test]
fn only_the_three_failure_sources_are_degradations() {
    assert!(!KindSource::Classified.is_degradation());
    assert!(!KindSource::Caller.is_degradation());
    assert!(KindSource::Abstained.is_degradation());
    assert!(KindSource::NoClassifier.is_degradation());
    assert!(KindSource::ClassifierUnavailable.is_degradation());
}
