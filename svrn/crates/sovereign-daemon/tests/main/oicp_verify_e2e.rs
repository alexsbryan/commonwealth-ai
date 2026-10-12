// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /oicp/v1/verify` through the real client router (OICP v0.6 §2.6),
//! over the conformance suite's fixture library
//! (`common::fixture_library`).
//!
//! The fast tier's pair is one sentence of the harbour notes, once with the
//! day the notes give ("Tuesday") and once with a day they never mention
//! ("Friday"): the first is not refused, the second is `unsupported` naming
//! the day. The checker's judge is a test double that supports exactly the
//! passage about the tide tables, so its verdicts are the route's mapping,
//! not a model's.

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
use sovereign_daemon::routes_oicp_verify::VerifyJudge;
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::AppState;
use tower::ServiceExt;

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
        unimplemented!("verify reads lexical candidates only")
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
    let state = state_over(indexes).await;
    (tmp, state)
}

fn reply(body: &serde_json::Value) -> VerifyResponse {
    serde_json::from_value(body.clone()).unwrap_or_else(|e| panic!("{e}: {body}"))
}

/// `s`'s code points `[a, b)`.
fn cp(s: &str, a: u64, b: u64) -> String {
    s.chars().skip(a as usize).take((b - a) as usize).collect()
}

#[tokio::test]
async fn the_fast_tier_refuses_a_day_the_notes_never_give_and_not_the_one_they_do() {
    let (_tmp, state) = fixture_state().await;
    let lib = library();
    let notes = lib.documents.iter().find(|d| d.name == NOTES).unwrap();

    let given = "The harbour master blamed the tide tables on Tuesday.";
    let (status, body) = verify_on(
        client_router(state.clone()),
        serde_json::json!({"unit": given, "tier": "fast"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = reply(&body);
    assert_eq!(r.verifier, "verify/1 fast/value-presence");
    assert_eq!(r.corpora.len(), 1, "{body}");
    let [c] = r.claims.as_slice() else {
        panic!("one sentence is one claim: {body}")
    };
    assert_eq!(c.exact, given);
    assert_eq!((c.start, c.end), (0, given.chars().count() as u64));
    assert_eq!(c.class, Checkability::Checkable);
    assert_eq!(c.verdict, VerifyVerdict::Abstained, "{body}");
    assert!(c.specifics_absent.is_empty(), "{body}");
    assert_eq!(c.rung, "fast/value-presence");
    assert_eq!(c.operating_point, UNCALIBRATED);
    // The passage the claim was read against, as a range of its own text.
    let span = c
        .evidence
        .iter()
        .filter_map(|p| p.span.as_ref())
        .find(|s| s.document.source.id == NOTES)
        .unwrap_or_else(|| panic!("the harbour notes are among the evidence: {body}"));
    assert_eq!(
        span.document.text_sha256,
        Sha256Hash::of_str(&notes.text).to_hex()
    );
    assert_eq!(span.exact, cp(&notes.text, span.start, span.end));
    assert!(span.exact.contains("tide tables"), "{body}");

    let planted = "The harbour master blamed the tide tables on Friday.";
    let (_, body) = verify_on(
        client_router(state),
        serde_json::json!({"unit": planted, "tier": "fast"}),
    )
    .await;
    let r = reply(&body);
    let [c] = r.claims.as_slice() else {
        panic!("{body}")
    };
    assert_eq!(c.verdict, VerifyVerdict::Unsupported, "{body}");
    assert_eq!(c.specifics_absent, vec!["Friday".to_string()], "{body}");
    assert!(c
        .evidence
        .iter()
        .all(|p| p.verdict == VerifyVerdict::Unsupported));
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
    assert_eq!(second.verdict, VerifyVerdict::Unsupported, "{body}");
    assert_eq!(
        second.specifics_absent,
        vec!["14".to_string(), "2019".to_string()],
        "{body}"
    );
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
        "verify/1 checker/chunk-support tau=0.5 model=tide-table-judge"
    );
    let [c] = r.claims.as_slice() else {
        panic!("{body}")
    };
    assert_eq!(c.verdict, VerifyVerdict::Supported, "{body}");
    assert_eq!(c.rung, "checker/chunk-support");
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
            "unit": "Okafor surveyed 14 agreements on the shared gauges.",
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
    assert!(m
        .features
        .iter()
        .any(|f| f == oicp_types::features::EVIDENCE_VERIFY));
}
