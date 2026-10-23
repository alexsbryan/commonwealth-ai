// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /oicp/v1/verify` through the real client router (OICP v0.6 §2.6,
//! §2.15), over the conformance suite's fixture library
//! (`common::fixture_library`), its sentences embedded by the word-list
//! embedder (`common::concept_embedder`) and read through sentence search.
//!
//! The fast tier reads one sentence of the harbour notes with a day they
//! never mention ("Friday"): the miss is a hint, never a verdict. The
//! checker's judge is a test double that supports exactly the sentence about
//! the tide tables, so its verdicts are the route's mapping, not a model's.
//! The figure claims, the floor and the contradiction are
//! `oicp_verify_sentences_e2e`'s.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use futures::Stream;
use kernel_types::Sha256Hash;
use oicp_types::verify::{Checkability, VerifyResponse, VerifyTier, VerifyVerdict, UNCALIBRATED};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, CompletionResponse, ProviderCapabilities};
use sovereign_daemon::routes_oicp_verify::{VerifyJudge, CHECKER_PASSAGES};
use sovereign_daemon::routes_oicp_verify_opinion as opinion;
use sovereign_daemon::sentence_index::config::DATA_FILE;
use sovereign_daemon::sentence_index::SentenceSearchConfig;
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::AppState;
use tower::ServiceExt;

use crate::common::concept_embedder::{sentences_indexed, state_with_concepts};
use crate::common::fixture_library::{install, library, state_over};

const NOTES: &str = "harbour-notes";
/// A phrase of the one passage the test judge supports.
const SUPPORTED_PASSAGE: &str = "printed in a year when the channel was deeper";

/// Supports a claim exactly when the passage it is shown is the tide-table
/// one: label A (supported) at 0.9, else B at 0.9.
struct TideTableJudge;

#[async_trait::async_trait]
impl InferenceProvider for TideTableJudge {
    async fn complete(
        &self,
        r: &CompletionRequest,
    ) -> sovereign_core::error::Result<CompletionResponse> {
        let text = if r.prompt.contains(SUPPORTED_PASSAGE) {
            r#"{"A":0.9,"B":0.1}"#
        } else {
            r#"{"A":0.1,"B":0.9}"#
        };
        Ok(serde_json::from_value(serde_json::json!({
            "text": text,
            "tokens_used": 1,
            "model_id": "tide-table-judge",
            "latency_ms": 0,
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
        "tide-table-judge".into()
    }
}

async fn verify_on(
    router: axum::Router,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::post("/oicp/v1/verify")
        .header("host", "127.0.0.1:9741")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let addr: SocketAddr = "127.0.0.1:55004".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn fixture_state() -> (tempfile::TempDir, AppState) {
    let lib = library();
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install(&indexes, &lib.corpus_id, &lib.documents, true).await;
    let state = state_with_concepts(indexes).await;
    sentences_indexed(&state, &lib.corpus_id).await;
    (tmp, state)
}

fn reply(body: &serde_json::Value) -> VerifyResponse {
    serde_json::from_value(body.clone()).unwrap_or_else(|e| panic!("{e}: {body}"))
}

/// `s`'s code points `[a, b)`.
fn cp(s: &str, a: u64, b: u64) -> String {
    s.chars().skip(a as usize).take((b - a) as usize).collect()
}

/// Failing input (platform row PC1): a claim settled from the fast hint
/// alone. A day the notes never give is named as the hint it is, and the
/// tier abstains on the sentence the claim is about, carried with its span.
#[tokio::test]
async fn the_fast_tiers_miss_is_a_hint_and_never_a_verdict() {
    let (_tmp, state) = fixture_state().await;
    let lib = library();
    let notes = lib.documents.iter().find(|d| d.name == NOTES).unwrap();

    let planted = "The harbour master blamed the tide tables on Friday.";
    let (status, body) = verify_on(
        client_router(state),
        serde_json::json!({"unit": planted, "tier": "fast"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = reply(&body);
    assert_eq!(
        r.verifier,
        format!(
            "verify/3 scorer=cosine floor={} {} fast/value-presence",
            shipped_floor(),
            opinion::identity()
        )
    );
    assert_eq!(r.corpora.len(), 1, "{body}");
    let [c] = r.claims.as_slice() else {
        panic!("one sentence is one claim: {body}")
    };
    assert_eq!(c.exact, planted);
    assert_eq!((c.start, c.end), (0, planted.chars().count() as u64));
    assert_eq!(c.class, Checkability::Checkable);
    assert_eq!(c.verdict, VerifyVerdict::Abstained, "{body}");
    assert_eq!(c.specifics_absent, vec!["Friday".to_string()], "{body}");
    assert_eq!(c.rung, "fast/value-presence");
    assert_eq!(c.operating_point, UNCALIBRATED);
    let [day] = c.specifics.as_slice() else {
        panic!("{body}")
    };
    assert_eq!(
        (day.kind, day.exact.as_str()),
        (oicp_types::ClaimSpecificKind::Name, "Friday")
    );
    assert_eq!(day.exact, cp(planted, day.start, day.end));
    // The sentence the claim was read against, as a range of its own text.
    let [p] = c.evidence.as_slice() else {
        panic!("one sentence of the library is about the tide tables: {body}")
    };
    assert_eq!(p.verdict, VerifyVerdict::Abstained);
    let span = p.span.as_ref().expect("a sentence with its span");
    assert_eq!(span.document.source.id, NOTES);
    assert_eq!(
        span.document.text_sha256,
        Sha256Hash::of_str(&notes.text).to_hex()
    );
    assert_eq!(span.exact, cp(&notes.text, span.start, span.end));
    assert!(span.exact.contains("tide tables"), "{body}");
}

#[tokio::test]
async fn each_sentence_is_a_claim_and_one_with_nothing_to_look_for_is_not_checked() {
    let (_tmp, state) = fixture_state().await;
    let unit = "rivers keep their own counsel. The survey covered 14 agreements in 2019.";
    let (status, body) = verify_on(
        client_router(state),
        serde_json::json!({"unit": unit, "tier": "fast"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = reply(&body);
    let [first, second] = r.claims.as_slice() else {
        panic!("two sentences are two claims: {body}")
    };
    for c in [first, second] {
        assert_eq!(c.exact, cp(unit, c.start, c.end), "{body}");
    }
    assert_eq!(first.class, Checkability::NotCheckable);
    assert_eq!(first.verdict, VerifyVerdict::Abstained);
    assert_eq!(first.rung, "class/no-specifics");
    assert_eq!(second.verdict, VerifyVerdict::Abstained, "{body}");
    assert_eq!(
        second.specifics_absent,
        vec!["14".to_string(), "2019".to_string()],
        "{body}"
    );
    let kinds: Vec<_> = second
        .specifics
        .iter()
        .map(|s| (s.kind, s.exact.as_str()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (oicp_types::ClaimSpecificKind::Quantity, "14"),
            (oicp_types::ClaimSpecificKind::Date, "2019"),
        ]
    );
}

/// Failing input (platform row PB1): a claim offered as the writer's view
/// carried a figure, so it was searched and judged like a fact. It is
/// `not_checkable` at `class/opinion`, before any sentence is read (no
/// evidence, no specifics), while a fact with a figure beside it is judged.
#[tokio::test]
async fn a_claim_offered_as_opinion_is_not_checked_and_a_fact_beside_it_is() {
    let (_tmp, state) = fixture_state().await;
    let opinion = "I think Smith overstates the 1776 tariff\u{2019}s reach.";
    let fact = "Okafor surveyed 14 agreements on the shared gauges.";
    let (status, body) = verify_on(
        client_router(state).layer(axum::Extension(VerifyJudge(Arc::new(TideTableJudge)))),
        serde_json::json!({"unit": format!("{opinion} {fact}"), "tier": "checker"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = reply(&body);
    let [o, f] = r.claims.as_slice() else {
        panic!("two sentences are two claims: {body}")
    };
    assert_eq!(o.exact, opinion);
    assert_eq!(o.class, Checkability::NotCheckable, "{body}");
    assert_eq!(o.verdict, VerifyVerdict::Abstained, "{body}");
    assert_eq!(o.rung, "class/opinion", "{body}");
    assert!(o.evidence.is_empty() && o.specifics.is_empty(), "{body}");
    assert_eq!(f.class, Checkability::Checkable, "{body}");
    assert_eq!(f.verdict, VerifyVerdict::Contradicted, "{body}");
}

#[tokio::test]
async fn the_checker_maps_its_judges_support_and_names_its_model() {
    let (_tmp, state) = fixture_state().await;
    let judged = || {
        client_router(state.clone()).layer(axum::Extension(VerifyJudge(Arc::new(TideTableJudge))))
    };

    let (status, body) = verify_on(
        judged(),
        serde_json::json!({
            "unit": "The harbour master blamed the tide tables on Tuesday.",
            "tier": "checker",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = reply(&body);
    assert_eq!(
        r.verifier,
        format!(
            "verify/3 scorer=cosine floor={} {} checker/sentence-support tau=0.5 passages=3 \
             model=tide-table-judge",
            shipped_floor(),
            opinion::identity()
        )
    );
    let [c] = r.claims.as_slice() else {
        panic!("{body}")
    };
    assert_eq!(c.verdict, VerifyVerdict::Supported, "{body}");
    assert_eq!(c.rung, "checker/sentence-support");
    assert!(c.score.is_some_and(|s| s >= 0.5), "{body}");
    let backing = c
        .evidence
        .iter()
        .find(|p| p.verdict == VerifyVerdict::Supported)
        .unwrap_or_else(|| panic!("the supporting passage is in the evidence: {body}"));
    assert!(backing
        .span
        .as_ref()
        .is_some_and(|s| s.exact.contains(SUPPORTED_PASSAGE)));

    let (_, body) = verify_on(
        judged(),
        serde_json::json!({
            "unit": "Okafor surveyed 14 rivers on the shared gauges.",
            "tier": "checker",
        }),
    )
    .await;
    let r = reply(&body);
    let [c] = r.claims.as_slice() else {
        panic!("{body}")
    };
    assert_eq!(c.verdict, VerifyVerdict::Unsupported, "{body}");
    assert!(
        !c.evidence.is_empty(),
        "the judged passages are named: {body}"
    );

    // The library counts the agreements in words ("The survey of eleven
    // agreements finds the opposite."), so another count of them differs.
    let (_, body) = verify_on(
        judged(),
        serde_json::json!({
            "unit": "Okafor surveyed 14 agreements on the shared gauges.",
            "tier": "checker",
        }),
    )
    .await;
    let r = reply(&body);
    let [c] = r.claims.as_slice() else {
        panic!("{body}")
    };
    assert_eq!(c.verdict, VerifyVerdict::Contradicted, "{body}");
    let differing: Vec<(&str, &str)> = c
        .evidence
        .iter()
        .flat_map(|p| &p.differing)
        .map(|d| (d.claim.exact.as_str(), d.source.exact.as_str()))
        .collect();
    assert_eq!(differing, vec![("14", "eleven")], "{body}");
}

/// The checker reads its candidates one at a time, so how many it judges is
/// the tier's latency. A claim about the rivers' survey has more sentences
/// above the floor than the cap, and none supports it, so nothing exits
/// early: the judge reads exactly the cap, and each is named.
#[tokio::test]
async fn the_checker_judges_no_more_passages_than_its_cap() {
    let (_tmp, state) = fixture_state().await;
    let (status, body) = verify_on(
        client_router(state).layer(axum::Extension(VerifyJudge(Arc::new(TideTableJudge)))),
        serde_json::json!({
            "unit": "In 2019 the river, the archive, the gauges and the gulls were surveyed.",
            "tier": "checker",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = reply(&body);
    let [c] = r.claims.as_slice() else {
        panic!("{body}")
    };
    assert_eq!(c.evidence.len(), CHECKER_PASSAGES, "{body}");
    assert_eq!(c.verdict, VerifyVerdict::Unsupported, "{body}");
}

/// Every way of not judging is said by name: a listener with no judge, a
/// tier the host does not serve, an empty unit, and a scope with nothing in
/// it the caller may read.
#[tokio::test]
async fn what_the_host_cannot_judge_it_says_by_name() {
    let (_tmp, state) = fixture_state().await;
    let unit = "The harbour master blamed the tide tables on Tuesday.";

    let (status, body) = verify_on(
        client_router(state.clone()),
        serde_json::json!({"unit": unit, "tier": "checker"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        reply(&body).claims[0].verdict,
        VerifyVerdict::CouldNotJudge {
            reason: "no checker".into()
        }
    );

    let (status, body) = verify_on(
        client_router(state.clone()),
        serde_json::json!({"unit": unit, "tier": "judge"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "tier not offered");

    let (status, body) = verify_on(
        client_router(state.clone()),
        serde_json::json!({"unit": "  ", "tier": "fast"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "empty unit");

    let (status, body) = verify_on(
        client_router(state),
        serde_json::json!({"unit": unit, "tier": "fast", "corpora": ["nowhere"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = reply(&body);
    assert!(r.corpora.is_empty());
    assert_eq!(r.corpora_unavailable[0].reason, "corpus not held");
    assert_eq!(
        r.claims[0].verdict,
        VerifyVerdict::CouldNotJudge {
            reason: "no corpus read".into()
        }
    );
}

/// The manifest names the route and its tiers, and the feature travels with
/// them (§2.1, §2.6).
#[tokio::test]
async fn the_manifest_advertises_verify_with_its_endpoint_and_tiers() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state_over(tmp.path().join("indexes")).await;
    let mut req = Request::get("/oicp/v1/capabilities")
        .header("host", "127.0.0.1:9741")
        .body(Body::empty())
        .unwrap();
    let addr: SocketAddr = "127.0.0.1:55005".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = client_router(state).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let m: oicp_types::ProviderManifest = serde_json::from_slice(&bytes).unwrap();
    let evidence = m
        .knowledge
        .and_then(|k| k.evidence)
        .expect("knowledge.evidence");
    assert_eq!(evidence.verify_endpoint.as_deref(), Some("/oicp/v1/verify"));
    assert_eq!(
        evidence.verify_tiers,
        vec![VerifyTier::Fast, VerifyTier::Checker]
    );
    for feature in [
        oicp_types::features::EVIDENCE_VERIFY,
        oicp_types::features::EVIDENCE_VERIFY_SENTENCES,
        oicp_types::features::KNOWLEDGE_SENTENCES,
    ] {
        assert!(m.features.iter().any(|f| f == feature), "{feature}");
    }
}

/// POST `body` to `path` on `router` from loopback.
async fn post_on(
    router: axum::Router,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::post(path)
        .header("host", "127.0.0.1:9741")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let addr: SocketAddr = "127.0.0.1:55004".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

/// The journal's lines, waiting for the append the route spawned.
async fn journal_lines(dir: &std::path::Path, at_least: usize) -> Vec<serde_json::Value> {
    for _ in 0..100 {
        let mut lines = Vec::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with("grounding-") {
                let text = std::fs::read_to_string(e.path()).unwrap();
                lines.extend(text.lines().map(|l| serde_json::from_str(l).unwrap()));
            }
        }
        if lines.len() >= at_least {
            return lines;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!(
        "the journal at {} never reached {at_least} line(s)",
        dir.display()
    );
}

/// OICP v0.6 §2.10: a decided claim is kept in the grounding journal under
/// `oicp_verify`, by handle and count, never by its words, and comes back
/// with the `episode_id` a correction names it by; a correction is kept
/// with what reading its witness back found. A router with no journal
/// keeps nothing and refuses corrections by name.
#[tokio::test]
async fn a_judged_claim_is_journaled_by_reference_and_a_correction_names_it() {
    use sovereign_daemon::routes_oicp_verify::VerifyJournal;

    let (_tmp, state) = fixture_state().await;
    let journal = tempfile::tempdir().unwrap();
    let router = || {
        client_router(state.clone())
            .layer(axum::Extension(VerifyJudge(Arc::new(TideTableJudge))))
            .layer(axum::Extension(VerifyJournal(journal.path().to_path_buf())))
    };
    let unit = "Okafor surveyed 14 rivers on the shared gauges.";
    let (status, body) = post_on(
        router(),
        "/oicp/v1/verify",
        serde_json::json!({"unit": unit, "tier": "checker"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = reply(&body);
    let [c] = r.claims.as_slice() else {
        panic!("{body}")
    };
    let episode = c
        .episode_id
        .clone()
        .unwrap_or_else(|| panic!("an episode: {body}"));

    let lines = journal_lines(journal.path(), 1).await;
    let raw = std::fs::read_dir(journal.path())
        .unwrap()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect::<String>();
    assert!(
        !raw.contains("surveyed"),
        "no claim words in the journal: {raw}"
    );
    let d = &lines[0];
    assert_eq!(d["kind"], "decision");
    assert_eq!(d["surface"], "oicp_verify");
    assert_eq!(d["episode_id"], episode.as_str());
    assert_eq!(d["verdict"], "unsupported");
    assert_eq!(d["action"], "checker/sentence-support");
    assert_eq!(d["chunks"], c.evidence.len());
    // "14" only: a one-word name opening the sentence is not a specific
    // (`sentence_specifics`), and the library counts no rivers, so it is
    // absent.
    assert_eq!(
        d["specifics"],
        serde_json::json!({"checked": 1, "absent": 1}),
        "{d}"
    );

    // A witness read back: inside a held text, and off its end.
    let span = c
        .evidence
        .iter()
        .find_map(|p| p.span.clone())
        .unwrap_or_else(|| panic!("a judged passage with a span: {body}"));
    let witness = |end: u64| {
        serde_json::json!({
            "corpus": span.corpus_id,
            "text_sha256": span.document.text_sha256,
            "start": span.start,
            "end": end,
        })
    };
    for (end, found) in [
        (span.end, "verified"),
        (span.end + 1_000_000, "out_of_range"),
    ] {
        let (status, body) = post_on(
            router(),
            "/oicp/v1/verify/correction",
            serde_json::json!({"episode_id": episode, "expected": "supported", "witness": witness(end)}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["recorded"], true, "{body}");
        assert_eq!(body["witness"], found, "{body}");
    }
    let lines = journal_lines(journal.path(), 3).await;
    let corrections: Vec<_> = lines.iter().filter(|l| l["kind"] == "correction").collect();
    assert_eq!(corrections.len(), 2);
    assert!(corrections
        .iter()
        .all(|l| l["episode_id"] == episode.as_str()));

    let (status, body) = post_on(
        router(),
        "/oicp/v1/verify/correction",
        serde_json::json!({"episode_id": "the third claim", "expected": "supported"}),
    )
    .await;
    assert_eq!(
        (status, &body["error"]),
        (
            StatusCode::BAD_REQUEST,
            &serde_json::json!("not an episode id")
        )
    );

    let (status, body) = post_on(
        client_router(state.clone()),
        "/oicp/v1/verify/correction",
        serde_json::json!({"episode_id": episode, "expected": "supported"}),
    )
    .await;
    assert_eq!(
        (status, &body["error"]),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            &serde_json::json!("no journal")
        )
    );
    let (_, body) = post_on(
        client_router(state.clone()).layer(axum::Extension(VerifyJudge(Arc::new(TideTableJudge)))),
        "/oicp/v1/verify",
        serde_json::json!({"unit": unit, "tier": "checker"}),
    )
    .await;
    assert_eq!(
        reply(&body).claims[0].episode_id,
        None,
        "no journal, no episode"
    );
}

/// A judge whose model queue is full: every call is shed, as the daemon's
/// slot queue sheds past its predicted-wait bound, and each call is counted.
struct ShedJudge(std::sync::atomic::AtomicUsize);

#[async_trait::async_trait]
impl InferenceProvider for ShedJudge {
    async fn complete(
        &self,
        _r: &CompletionRequest,
    ) -> sovereign_core::error::Result<CompletionResponse> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(sovereign_core::error::Error::QueueShed {
            position: 37,
            predicted_wait_ms: 30_715,
            retry_after_secs: 31,
        })
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
        "shed-judge".into()
    }
}

/// A checker the host's queue sheds is backpressure, not a claim it could not
/// judge: the unit is answered as every shed is, `503` with `Retry-After` and
/// the shed body, so a client can come back. The route stops at the first
/// shed rather than queue the unit's other passages and claims behind it, and
/// keeps nothing in the journal for a unit it did not answer. Watched
/// 2026-10-12: zoracite's live S6 demo got 131 of its 176 checker claims back
/// as `checker did not answer` while the daemon logged each judge call
/// `host busy … retry after 31s`.
#[tokio::test]
async fn a_checker_the_queue_sheds_answers_host_busy_with_its_retry_hint() {
    use sovereign_daemon::routes_oicp_verify::VerifyJournal;

    let (_tmp, state) = fixture_state().await;
    let journal = tempfile::tempdir().unwrap();
    let judge = Arc::new(ShedJudge(Default::default()));
    let router = client_router(state)
        .layer(axum::Extension(VerifyJudge(judge.clone())))
        .layer(axum::Extension(VerifyJournal(journal.path().to_path_buf())));
    let mut req = Request::post("/oicp/v1/verify")
        .header("host", "127.0.0.1:9741")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "unit": "Okafor surveyed 14 agreements on the shared gauges. \
                         The harbour master blamed the tide tables on Tuesday.",
                "tier": "checker",
            })
            .to_string(),
        ))
        .unwrap();
    let addr: SocketAddr = "127.0.0.1:55004".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let retry_after = resp.headers().get("retry-after").cloned();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        retry_after.as_ref().map(|v| v.to_str().unwrap()),
        Some("31")
    );
    assert_eq!(body["reason"], "local_queue_full", "{body}");
    assert_eq!(body["retry_after_secs"], 31, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("host busy")),
        "{body}"
    );
    assert_eq!(
        judge.0.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the route stops at the first shed"
    );
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let kept: Vec<_> = std::fs::read_dir(journal.path())
        .unwrap()
        .flatten()
        .collect();
    assert!(
        kept.is_empty(),
        "nothing journaled for an unanswered unit: {kept:?}"
    );
}

/// The floor the shipped data file names, as the verifier writes it.
fn shipped_floor() -> f32 {
    SentenceSearchConfig::parse(DATA_FILE)
        .expect("the shipped data file parses")
        .verify_floor
}
