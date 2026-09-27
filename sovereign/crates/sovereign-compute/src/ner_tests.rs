// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ner`'s tests: the kind's registration, the one handle per process, and
//! the route's wire as `RemoteNer` reads it.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static LOADS: AtomicUsize = AtomicUsize::new(0);

/// Mentions derived from the text, so a lost or reordered text is visible.
struct Stub;
impl LabeledEntityExtractor for Stub {
    fn model_id(&self) -> &str {
        "stub-ner"
    }
    fn labels(&self) -> Vec<String> {
        vec!["Person".into(), "Work".into()]
    }
    fn threshold(&self) -> f32 {
        0.55
    }
    fn extract_mentions(&self, text: &str) -> Result<Vec<EntityMention>> {
        Ok(vec![EntityMention {
            text: text.to_uppercase(),
            label: "Person".into(),
            char_start: 0,
            char_end: text.chars().count(),
            score: 0.9,
        }])
    }
    fn extract_concept_mentions(&self, text: &str) -> Result<Vec<EntityMention>> {
        Ok(vec![EntityMention {
            text: format!("concept:{text}"),
            label: "Concept".into(),
            char_start: 1,
            char_end: 2,
            score: 0.7,
        }])
    }
    fn generation(&self) -> GlinerGeneration {
        GlinerGeneration::V2
    }
}

fn stub_loader() -> Option<Arc<dyn LabeledEntityExtractor>> {
    LOADS.fetch_add(1, Ordering::SeqCst);
    Some(Arc::new(Stub))
}

/// NER is a served kind with a route and a client; its child role is a
/// named absence.
#[test]
fn ner_is_served_on_its_route_with_a_client_and_no_child() {
    assert!(matches!(NER.loader, KindLoader::Ner(_)));
    assert_eq!(NER.route_path(), Some(NER_PATH));
    assert!(matches!(NER.client, KindClient::Provided { method } if !method.is_empty()));
    assert_eq!(NER.child_role(), None);
    assert!(matches!(NER.child, KindChild::Absent { reason } if !reason.is_empty()));
    assert!(NER.load_provider(std::path::Path::new("/x")).is_err());
}

/// A host that registers NER mounts its route through the one kind mount.
#[test]
fn a_registered_ner_is_mounted_by_the_kind_mount() {
    register().expect("registers");
    register().expect("registering twice is not a second kind");
    let paths: Vec<&str> = crate::server::kind_routes::<()>(
        |_, kind| Err(format!("no provider for {}", kind.role)),
        crate::server::openai_refusal,
    )
    .into_iter()
    .map(|(path, _)| path)
    .collect();
    assert!(paths.contains(&NER_PATH), "{paths:?}");
}

/// Two readers asking for the handle get the SAME `Arc`, loaded once,
/// through the kind's registration.
#[test]
fn every_reader_gets_the_one_handle_loaded_once() {
    let cell = OnceLock::new();
    let kind = ServedKind {
        role: "ner-test-once",
        loader: KindLoader::Ner(stub_loader),
        route: KindRoute::Absent { reason: "test" },
        client: KindClient::Absent { reason: "test" },
        ..NER
    };
    let daemon = load_once(&cell, kind).expect("stub loads");
    let recipe = load_once(&cell, kind).expect("stub loads");
    assert!(Arc::ptr_eq(&daemon, &recipe));
    assert_eq!(LOADS.load(Ordering::SeqCst), 1);
    assert!(served_kind::served_kinds()
        .iter()
        .any(|k| k.role == "ner-test-once"));
}

/// A route answering from `handle` the way `serve_ner` does.
async fn route(handle: Option<Arc<dyn LabeledEntityExtractor>>) -> String {
    use axum::response::IntoResponse;
    use axum::routing::post;
    let app = axum::Router::new().route(
        NER_PATH,
        post(move |axum::Json(req): axum::Json<NerRequest>| {
            let handle = handle.clone();
            async move {
                match answer(handle, req) {
                    Ok(r) => axum::Json(r).into_response(),
                    Err(e) => (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        format!("{e:?}"),
                    )
                        .into_response(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    base
}

/// The port answered over the route is the port served: the extractor's
/// identity, and each pass's mentions per text in order. Called from inside
/// the runtime, as the turn's retrieval calls it: a multi-thread runtime,
/// the daemon's, because the stub route here shares the caller's runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_ner_answers_as_the_extractor_it_dials() {
    let base = route(Some(Arc::new(Stub))).await;
    let remote = RemoteNer::connect(&base)
        .await
        .expect("reachable")
        .expect("an extractor");
    assert_eq!(remote.model_id(), "stub-ner");
    assert_eq!(remote.labels(), Stub.labels());
    assert_eq!(remote.threshold(), 0.55);
    assert_eq!(remote.generation(), GlinerGeneration::V2);

    let texts = ["ada lovelace", "émile"];
    let served = Stub.extract_mentions_batch(&texts).unwrap();
    assert_eq!(remote.extract_mentions_batch(&texts).unwrap(), served);
    assert_eq!(
        remote.extract_mentions("émile").unwrap(),
        Stub.extract_mentions("émile").unwrap()
    );
    assert_eq!(
        remote.extract_concept_mentions("ism").unwrap(),
        Stub.extract_concept_mentions("ism").unwrap()
    );
}

/// A node without the model answers the identity probe with none, and
/// refuses a request with texts by name.
#[tokio::test]
async fn a_node_without_the_model_is_none_and_refuses_texts() {
    let base = route(None).await;
    assert!(RemoteNer::connect(&base)
        .await
        .expect("reachable")
        .is_none());
    let refused = answer(
        None,
        NerRequest {
            texts: vec!["x".into()],
            pass: NerPass::Entities,
        },
    );
    assert!(
        matches!(refused, Err(KindServeError::Backend(ref m)) if m.contains("no NER model")),
        "{refused:?}"
    );
}

#[tokio::test]
async fn an_unreachable_route_is_named() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let err = RemoteNer::connect(&base).await.expect_err("nothing there");
    assert!(
        err.contains("not reachable") && err.contains(NER_PATH),
        "{err}"
    );
}
