// SPDX-License-Identifier: AGPL-3.0-or-later
//! Verify by meaning (OICP v0.6 §2.15, platform row PC1) through the real
//! client router, over the conformance suite's claims shelf
//! (`cmnwlth/crates/oicp-conformance/fixture/claims.json`, a port's ledger of
//! figure sentences), its sentences embedded by the word-list embedder
//! (`common::concept_embedder`).
//!
//! The checker's judge is a test double that backs a claim exactly when the
//! sentence it is shown carries every number of the claim, so a restated
//! figure is `supported` and another figure is not; what the route makes of
//! "not backed" (a contradiction naming the pair, or not) is the route's.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use futures::Stream;
use oicp_types::verify::{VerifiedClaim, VerifyResponse, VerifyVerdict};
use oicp_types::ClaimSpecificKind;
use serde::Deserialize;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, CompletionResponse, ProviderCapabilities};
use sovereign_daemon::routes_oicp_verify::VerifyJudge;
use sovereign_daemon::sentence_index::config::DATA_FILE;
use sovereign_daemon::sentence_index::SentenceSearchConfig;
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::AppState;
use tower::ServiceExt;

use crate::common::concept_embedder::{
    sentences_indexed, state_with_concepts, state_with_concepts_logged, AskedLog,
};
use crate::common::fixture_library::{install, Doc};

const CLAIMS: &str =
    include_str!("../../../../../cmnwlth/crates/oicp-conformance/fixture/claims.json");

#[derive(Deserialize)]
struct Fixture {
    shelf: Shelf,
    claims: Vec<Case>,
}

#[derive(Deserialize)]
struct Shelf {
    corpus_id: String,
    documents: Vec<Doc>,
}

#[derive(Deserialize)]
struct Case {
    unit: String,
    specifics: Vec<String>,
    #[serde(default)]
    none_within: Vec<String>,
}

fn fixture() -> Fixture {
    serde_json::from_str(CLAIMS).expect("the claims shelf loads")
}

/// The ledger's claims, by what each says.
const RESTATED: &str = "The port recorded 412 sailings in 2019 (Okafor 2019).";
const OTHER_FIGURE: &str = "The port recorded 512 sailings in 2019.";
const PERCENT_SIGN: &str = "Late sailings rose to 7.5% of the total in 2020.";
const ABSENT: &str = "Lighthouse keepers were paid 300 shillings a year (page 14).";

/// The numbers written in `s`.
fn numbers(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|w| w.trim_matches('.'))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Backs a claim (label A at 0.9) exactly when the sentence it is shown
/// carries every number the claim writes, and keeps each claim it is asked.
#[derive(Default)]
struct FigureJudge {
    asked: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl InferenceProvider for FigureJudge {
    async fn complete(
        &self,
        r: &CompletionRequest,
    ) -> sovereign_core::error::Result<CompletionResponse> {
        let passage = r
            .prompt
            .split("\"\"\"")
            .nth(1)
            .expect("the judge prompt quotes its passage");
        let claim = r
            .prompt
            .split("CLAIM: ")
            .nth(1)
            .and_then(|c| c.split("\n\n").next())
            .expect("the judge prompt names its claim");
        self.asked.lock().unwrap().push(claim.to_string());
        let held = numbers(passage);
        let backs = numbers(claim).iter().all(|n| held.contains(n));
        let text = if backs {
            r#"{"A":0.9,"B":0.1}"#
        } else {
            r#"{"A":0.1,"B":0.9}"#
        };
        Ok(serde_json::from_value(serde_json::json!({
            "text": text, "tokens_used": 1, "model_id": "figure-judge", "latency_ms": 0,
        }))
        .expect("a completion response"))
    }

    async fn complete_stream(
        &self,
        _r: &CompletionRequest,
    ) -> sovereign_core::error::Result<
        Pin<Box<dyn Stream<Item = sovereign_core::error::Result<String>> + Send>>,
    > {
        unimplemented!("the judge never streams")
    }

    async fn embed(&self, _i: &str) -> sovereign_core::error::Result<Vec<f32>> {
        unimplemented!("the judge never embeds: the host's embedder does")
    }

    fn capabilities(&self) -> ProviderCapabilities {
        unimplemented!()
    }

    fn model_id_for(&self, _speed: sovereign_core::types::Speed) -> String {
        "figure-judge".into()
    }
}

/// A host holding the claims shelf, its sentences indexed.
async fn shelf_host() -> (tempfile::TempDir, AppState, Fixture) {
    let fx = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install(&indexes, &fx.shelf.corpus_id, &fx.shelf.documents, true).await;
    let state = state_with_concepts(indexes).await;
    sentences_indexed(&state, &fx.shelf.corpus_id).await;
    (tmp, state, fx)
}

/// `unit` verified at `tier` against the shelf, its one claim.
async fn verify_one(
    state: &AppState,
    fx: &Fixture,
    unit: &str,
    tier: &str,
) -> (VerifiedClaim, VerifyResponse) {
    let (c, r, _) = verify_asking(state, fx, unit, tier).await;
    (c, r)
}

/// [`verify_one`], and each claim the checker's judge was asked.
async fn verify_asking(
    state: &AppState,
    fx: &Fixture,
    unit: &str,
    tier: &str,
) -> (VerifiedClaim, VerifyResponse, Vec<String>) {
    let judge = Arc::new(FigureJudge::default());
    let router = client_router(state.clone()).layer(axum::Extension(VerifyJudge(judge.clone())));
    let mut req = Request::post("/oicp/v1/verify")
        .header("host", "127.0.0.1:9741")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"unit": unit, "corpora": [fx.shelf.corpus_id], "tier": tier})
                .to_string(),
        ))
        .unwrap();
    let addr: SocketAddr = "127.0.0.1:55008".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let r: VerifyResponse = serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&bytes)));
    let [c] = r.claims.as_slice() else {
        panic!("one sentence is one claim: {r:?}")
    };
    let asked = judge.asked.lock().unwrap().clone();
    (c.clone(), r, asked)
}

/// `s`'s code points `[a, b)`.
fn cp(s: &str, a: u64, b: u64) -> String {
    s.chars().skip(a as usize).take((b - a) as usize).collect()
}

/// Platform row PC1's pass bar, offline: a claim with the library's figure is
/// `supported`; another figure `contradicted`, naming the claim's and the
/// source sentence's figure, the source's range reading back from its text;
/// a concept the library lacks `unsupported` with no evidence.
#[tokio::test]
async fn a_restated_figure_is_supported_and_another_contradicted_naming_the_pair() {
    let (_tmp, state, fx) = shelf_host().await;
    let ledger = &fx.shelf.documents[0].text;

    for unit in [RESTATED, PERCENT_SIGN] {
        let (c, _) = verify_one(&state, &fx, unit, "checker").await;
        assert_eq!(c.verdict, VerifyVerdict::Supported, "{unit}: {c:?}");
        assert_eq!(c.rung, "checker/sentence-support");
        assert!(c.evidence.iter().all(|p| p.differing.is_empty()), "{c:?}");
    }

    let (c, r) = verify_one(&state, &fx, OTHER_FIGURE, "checker").await;
    assert_eq!(c.verdict, VerifyVerdict::Contradicted, "{r:?}");
    let p = c
        .evidence
        .iter()
        .find(|p| p.verdict == VerifyVerdict::Contradicted)
        .unwrap_or_else(|| panic!("a contradicted sentence: {c:?}"));
    let [d] = p.differing.as_slice() else {
        panic!("one specific differs: {p:?}")
    };
    assert_eq!(
        (
            d.claim.kind,
            d.claim.exact.as_str(),
            d.source.kind,
            d.source.exact.as_str()
        ),
        (
            ClaimSpecificKind::Quantity,
            "512",
            ClaimSpecificKind::Quantity,
            "412"
        )
    );
    assert_eq!(cp(OTHER_FIGURE, d.claim.start, d.claim.end), "512");
    assert_eq!(cp(ledger, d.source.start, d.source.end), "412");
    let span = p.span.as_ref().expect("the source sentence's span");
    assert_eq!(span.exact, cp(ledger, span.start, span.end));

    for tier in ["fast", "checker"] {
        let (c, _) = verify_one(&state, &fx, ABSENT, tier).await;
        assert_eq!(c.verdict, VerifyVerdict::Unsupported, "{tier}: {c:?}");
        assert!(c.evidence.is_empty(), "{tier}: {c:?}");
        assert_eq!(c.rung, "sentences/floor");
    }
}

/// Failing input (platform row PC1): the floor read from anywhere but the
/// data file. The shipped file's floor and the same file with its floor
/// raised past the claim's best sentence: the verdict moves from `abstained`
/// with evidence to `unsupported` with none, and the verifier names each.
#[tokio::test]
async fn the_floor_is_the_data_files_and_a_verdict_moves_with_it() {
    let (_tmp, state, fx) = shelf_host().await;
    let (c, r) = verify_one(&state, &fx, RESTATED, "fast").await;
    assert_eq!(c.verdict, VerifyVerdict::Abstained, "{r:?}");
    assert!(!c.evidence.is_empty(), "{c:?}");
    let floor = SentenceSearchConfig::parse(DATA_FILE)
        .expect("the shipped data file parses")
        .verify_floor;
    assert!(
        r.verifier.contains(&format!(" floor={floor} ")),
        "{}",
        r.verifier
    );

    let raised = DATA_FILE.replace(&format!("verify_floor = {floor}"), "verify_floor = 0.999");
    assert_ne!(raised, DATA_FILE, "the data file names its floor");
    state
        .inner
        .node
        .sentences
        .set_config(SentenceSearchConfig::parse(&raised).expect("the edited file parses"));
    for tier in ["fast", "checker"] {
        let (c, r) = verify_one(&state, &fx, RESTATED, tier).await;
        assert_eq!(c.verdict, VerifyVerdict::Unsupported, "{tier}: {r:?}");
        assert!(c.evidence.is_empty(), "{tier}: {c:?}");
        assert_eq!(c.rung, "sentences/floor");
        assert!(r.verifier.contains(" floor=0.999 "), "{}", r.verifier);
    }
}

/// Failing inputs (platform row PC1): a citation's year made a figure, and a
/// figure inside parentheses left out of the claim's specifics. Every unit of
/// the shelf names its listed figures by kind at their ranges, nothing inside
/// its citation or locator; at `fast` above the floor the tier abstains,
/// whatever its miss.
#[tokio::test]
async fn each_claim_names_its_figures_by_kind_and_none_inside_a_citation() {
    let (_tmp, state, fx) = shelf_host().await;
    for case in &fx.claims {
        let unit = &case.unit;
        let (c, _) = verify_one(&state, &fx, unit, "fast").await;
        for s in &c.specifics {
            assert_eq!(cp(unit, s.start, s.end), s.exact, "{unit}: {s:?}");
        }
        for want in &case.specifics {
            let at = unit[..unit.find(want.as_str()).unwrap()].chars().count() as u64;
            let end = at + want.chars().count() as u64;
            assert!(
                c.specifics
                    .iter()
                    .any(|s| s.kind != ClaimSpecificKind::Name && s.start <= at && end <= s.end),
                "no figure, date or quantity covers `{want}` in `{unit}`: {:?}",
                c.specifics
            );
        }
        for cited in &case.none_within {
            let at = unit[..unit.find(cited.as_str()).unwrap()].chars().count() as u64;
            let end = at + cited.chars().count() as u64;
            assert!(
                c.specifics.iter().all(|s| s.end <= at || end <= s.start),
                "a specific inside `{cited}`: {:?}",
                c.specifics
            );
        }
        if c.evidence.is_empty() {
            assert_eq!(c.verdict, VerifyVerdict::Unsupported, "{unit}: {c:?}");
        } else {
            assert_eq!(c.verdict, VerifyVerdict::Abstained, "{unit}: {c:?}");
        }
    }
    let (c, _) = verify_one(&state, &fx, OTHER_FIGURE, "fast").await;
    assert_eq!(c.verdict, VerifyVerdict::Abstained, "{c:?}");
    assert_eq!(c.specifics_absent, vec!["512".to_string()], "{c:?}");
}

/// Failing input (platform row PC1, live 2026-10-15): the checker judging a
/// citation as content the source must back. On the daemon at :9741
/// `The port recorded 412 sailings in 2019 (Okafor 2019).` was `unsupported`
/// at 0.387 while the same sentence bare was `supported` at 1.0. What the
/// checker is asked is the claim less its citations and locators, the same
/// ones its specifics leave out; parentheses that are no citation stay, and
/// the wire's claim is still the unit's own.
#[tokio::test]
async fn the_checker_judges_the_claim_without_its_citations() {
    let (_tmp, state, fx) = shelf_host().await;
    let bare = "The port recorded 412 sailings in 2019.";
    for unit in [
        RESTATED,
        "The port recorded 412 sailings in 2019 [@okafor2019].",
        "The port recorded 412 sailings in 2019 (p. 12).",
        "The port recorded 412 sailings in 2019 [3].",
    ] {
        let (c, _, asked) = verify_asking(&state, &fx, unit, "checker").await;
        assert_eq!(c.exact, unit, "{c:?}");
        assert!(!asked.is_empty(), "{unit} reached no checker: {c:?}");
        assert!(asked.iter().all(|a| a == bare), "{unit} asked as {asked:?}");
    }
    let kept = "Late sailings rose to 7.5 percent of the total in 2020 (against 6.1 percent the year before).";
    let (c, _, asked) = verify_asking(&state, &fx, kept, "checker").await;
    assert_eq!(c.exact, kept, "{c:?}");
    assert!(!asked.is_empty(), "{kept} reached no checker: {c:?}");
    assert!(asked.iter().all(|a| a == kept), "asked as {asked:?}");
}

/// The same exposure one step earlier: sentence search embedded the claim
/// with its citations, which can sink a backed claim under `verify_floor`.
/// The test embedder scores by concept words so it cannot be sunk, but it
/// records what it is asked: the search query is the claim less its
/// citations and locators, the text the checker judges, and a figure in
/// other parentheses is searched unchanged.
#[tokio::test]
async fn sentence_search_is_asked_the_claim_without_its_citations() {
    let fx = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install(&indexes, &fx.shelf.corpus_id, &fx.shelf.documents, true).await;
    let log = AskedLog::default();
    let state = state_with_concepts_logged(indexes, Some(log.clone())).await;
    sentences_indexed(&state, &fx.shelf.corpus_id).await;
    let bare = "The port recorded 412 sailings in 2019.";
    for unit in [
        "The port recorded 412 sailings in 2019 (Okafor 2019).",
        "The port recorded 412 sailings in 2019 [@okafor2019].",
        "The port recorded 412 sailings in 2019 (p. 12).",
        "The port recorded 412 sailings in 2019 [3].",
    ] {
        log.lock().unwrap().clear();
        let (c, _, _) = verify_asking(&state, &fx, unit, "checker").await;
        assert_eq!(c.exact, unit, "{c:?}");
        let asked = log.lock().unwrap().clone();
        assert!(
            asked.iter().any(|q| q == bare),
            "{unit} searched as {asked:?}"
        );
        assert!(
            !asked
                .iter()
                .any(|q| q.contains("2019 (") || q.contains("2019 [")),
            "{unit} searched with its citation: {asked:?}"
        );
    }
    let kept = "Late sailings rose to 7.5 percent of the total in 2020 (against 6.1 percent the year before).";
    log.lock().unwrap().clear();
    let (c, _, _) = verify_asking(&state, &fx, kept, "checker").await;
    assert_eq!(c.exact, kept, "{c:?}");
    assert!(
        log.lock().unwrap().iter().any(|q| q == kept),
        "{:?}",
        log.lock().unwrap()
    );
}

/// A host holding two sentences of prose that write their figures in words
/// (Smith, Wealth of Nations, I.1 and IV.7), indexed.
async fn prose_host() -> (tempfile::TempDir, AppState, Fixture) {
    let fx = Fixture {
        shelf: Shelf {
            corpus_id: "prose-figures".into(),
            documents: vec![Doc {
                name: "wealth-of-nations.txt".into(),
                metadata: None,
                text: format!("{PINS} {BRAZILS}"),
            }],
        },
        claims: Vec::new(),
    };
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install(&indexes, &fx.shelf.corpus_id, &fx.shelf.documents, true).await;
    let state = state_with_concepts(indexes).await;
    sentences_indexed(&state, &fx.shelf.corpus_id).await;
    (tmp, state, fx)
}

const PINS: &str = "Those ten persons, therefore, could make among them upwards of forty-eight thousand pins in a day.";
const BRAZILS: &str = "It was more than a hundred years after the first settlement of the Brazils, before any silver, gold, or diamond mines were discovered there.";

/// Failing inputs (live 2026-10-15, the daemon at :9741, `checker`, corpus
/// zoracite-v1-pull-demo): "Ten workers in a small pin factory made 9,000
/// pins a day between them." against the source's "forty-eight thousand" was
/// `unsupported` with `differing: []`, and the claim restating the source's
/// words was `abstained` at `class/no-specifics`. A figure written in words is
/// a specific on either side, so the digits against the words come back
/// `contradicted`, naming the pair, each side read back from its text.
#[tokio::test]
async fn a_source_figure_in_words_contradicts_a_claim_in_digits() {
    let (_tmp, state, fx) = prose_host().await;
    let ledger = &fx.shelf.documents[0].text;
    for (unit, claim_figure, source_figure) in [
        (
            "Ten workers in a small pin factory made 9,000 pins a day between them.",
            "9,000",
            "forty-eight thousand",
        ),
        (
            "Silver, gold or diamond mines were found in Brazil fewer than 50 years after its first settlement.",
            "50",
            "a hundred",
        ),
    ] {
        let (c, r) = verify_one(&state, &fx, unit, "checker").await;
        assert_eq!(c.verdict, VerifyVerdict::Contradicted, "{unit}: {r:?}");
        let p = c
            .evidence
            .iter()
            .find(|p| p.verdict == VerifyVerdict::Contradicted)
            .unwrap_or_else(|| panic!("a contradicted sentence: {c:?}"));
        let [d] = p.differing.as_slice() else {
            panic!("one specific differs: {p:?}")
        };
        assert_eq!(
            (d.claim.exact.as_str(), d.source.exact.as_str()),
            (claim_figure, source_figure),
            "{unit}"
        );
        assert_eq!((d.claim.kind, d.source.kind), (ClaimSpecificKind::Quantity, ClaimSpecificKind::Quantity));
        assert_eq!(cp(unit, d.claim.start, d.claim.end), claim_figure);
        assert_eq!(cp(ledger, d.source.start, d.source.end), source_figure);
    }

    let words =
        "Ten workers in a small pin factory could make upwards of forty-eight thousand pins a day.";
    let (c, r) = verify_one(&state, &fx, words, "checker").await;
    assert_eq!(c.verdict, VerifyVerdict::Supported, "{r:?}");
    assert!(
        c.specifics
            .iter()
            .any(|s| s.exact == "forty-eight thousand" && s.kind == ClaimSpecificKind::Quantity),
        "{c:?}"
    );
}
