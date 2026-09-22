// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the loopback_parity e2e suite — split from loopback_parity.rs for the §3.2 size ceiling (behaviour-preserving move).

use crate::common::TestProvider;

use std::sync::Arc;

use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::types::Role;
use sovereign_daemon::daemon::EmbeddedDaemon;
use sovereign_daemon::insight_http::insight_router;
use sovereign_daemon::turn_http::turn_router;

use super::conversation_fixture;

/// The insight fixture: a serving daemon whose `ServingCore` carries a REAL
/// `InsightService` — the same construction `daemon_cmd` performs over the
/// daemon's `sovereign.db`, here over a tempdir sqlite the test also holds.
async fn insight_fixture() -> (
    tempfile::TempDir,
    Arc<EmbeddedDaemon>,
    Arc<sovereign_core::insight::InsightService>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let state_store =
        sovereign_store::sqlite::SqliteStateStore::open(&tmp.path().join("sovereign.db")).unwrap();
    let insight_store: Arc<dyn sovereign_contracts::traits::InsightStore> = Arc::new(
        sovereign_store::insight_store::SqliteInsightStore::new(state_store.connection()),
    );
    let service = Arc::new(sovereign_core::insight::InsightService::new(
        insight_store,
        Arc::new(sovereign_core::insight::InsightSinkRegistry::new()),
        // embed is load-bearing here: `clip` embeds the passage before it
        // persists, so the provider must answer embeddings — a bare
        // TestProvider refuses with NotImplemented and the clip 500s.
        Arc::new(TestProvider::new().with_embed_marker(|_| vec![0.5_f32; 4])),
    ));
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        tmp.path().join("recipes"),
        tmp.path().join("indexes"),
        Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
    ));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        crate::common::desktop_services_with_insights(
            engine,
            Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
            Arc::new(TestProvider::new()),
            Some(Arc::clone(&service)),
        ),
    );
    (tmp, daemon, service)
}

/// A sink that reports whatever the test told it to. `InsightSink`'s
/// four other methods are unused here — this exists to prove `id`,
/// `display_name` and `connected` CROSS, which is exactly what the
/// desktop's `get_sink_status` could not do: it hard-codes `sinks:
/// vec![]` and answers only the boolean.
struct StubSink {
    id: &'static str,
    display_name: &'static str,
    connected: bool,
}

#[async_trait::async_trait]
impl sovereign_contracts::traits::InsightSink for StubSink {
    fn id(&self) -> &str {
        self.id
    }
    fn display_name(&self) -> &str {
        self.display_name
    }
    async fn is_connected(&self) -> bool {
        self.connected
    }
    async fn push(
        &self,
        _node: &sovereign_contracts::types::InsightNode,
    ) -> sovereign_contracts::error::Result<()> {
        Ok(())
    }
    async fn push_batch(
        &self,
        _nodes: &[sovereign_contracts::types::InsightNode],
    ) -> sovereign_contracts::error::Result<()> {
        Ok(())
    }
}

/// `GET /v1/insights/sinks` — the route D2 left owed (sv-surface).
///
/// Red-watch 2026-09-10 (run, not asserted): the route line was taken
/// back out of `insight_router` with `sink_status` left in place, and
/// both sink cases failed — this one on a decode EOF (405, empty body,
/// because `/v1/insights/{id}` still matches the path with no GET), the
/// 503 case on `left: 405 right: 503`. `pass: 0 fail: 2`.
///
/// The three SPOOF legs above are deliberately NOT part of that
/// evidence: they exercise the router-level guard, which runs before
/// routing, so an emptied router still answers 403. They guard the
/// posture, not the routes, and each says so by naming the guard.
///
/// Two sinks, one reachable and one not. The assertion that matters is
/// the PER-SINK row: `any_connected` alone is what the desktop command
/// answers today, and it cannot tell a settings pane WHICH vault is
/// down. A route that returned the right boolean with an empty list
/// would pass a boolean-only check and ship the same blind spot.
#[tokio::test]
async fn insight_sink_status_names_each_sink_and_its_reachability() {
    let tmp = tempfile::tempdir().unwrap();
    let state_store =
        sovereign_store::sqlite::SqliteStateStore::open(&tmp.path().join("sovereign.db")).unwrap();
    let insight_store: Arc<dyn sovereign_contracts::traits::InsightStore> = Arc::new(
        sovereign_store::insight_store::SqliteInsightStore::new(state_store.connection()),
    );
    let mut sinks = sovereign_core::insight::InsightSinkRegistry::new();
    sinks.register(Arc::new(StubSink {
        id: "obsidian",
        display_name: "Obsidian vault",
        connected: true,
    }));
    sinks.register(Arc::new(StubSink {
        id: "logseq",
        display_name: "Logseq graph",
        connected: false,
    }));
    let service = Arc::new(sovereign_core::insight::InsightService::new(
        insight_store,
        Arc::new(sinks),
        Arc::new(TestProvider::new()),
    ));
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        tmp.path().join("recipes"),
        tmp.path().join("indexes"),
        Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
    ));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        crate::common::desktop_services_with_insights(
            engine,
            Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
            Arc::new(TestProvider::new()),
            Some(service),
        ),
    );
    let addr = crate::common::spawn_router(insight_router(daemon)).await;

    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{addr}/v1/insights/sinks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["any_connected"], true,
        "one of the two sinks is reachable: {body}"
    );
    let rows = body["sinks"].as_array().expect("the per-sink list");
    assert_eq!(
        rows.len(),
        2,
        "BOTH registered sinks are named — the desktop command answers an \
         empty list here, which is the blind spot this route closes: {body}"
    );
    let logseq = rows
        .iter()
        .find(|s| s["id"] == "logseq")
        .expect("the unreachable sink is still listed");
    assert_eq!(logseq["display_name"], "Logseq graph");
    assert_eq!(
        logseq["connected"], false,
        "the pane must be able to say WHICH vault is down: {body}"
    );
}

/// A daemon with no insight service answers the named 503 on the sink
/// route too — never `any_connected: false`, which is a CLAIM about
/// sinks and would read as "your vault is disconnected" (ARCH §18.3).
#[tokio::test]
async fn insight_sink_status_without_a_service_is_the_named_503() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        tmp.path().join("recipes"),
        tmp.path().join("indexes"),
        Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
    ));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        crate::common::desktop_services_with_insights(
            engine,
            Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
            Arc::new(TestProvider::new()),
            None,
        ),
    );
    let addr = crate::common::spawn_router(insight_router(daemon)).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/v1/insights/sinks"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("no insight service"),
        "the 503 names the absence: {body}"
    );
}

/// Clip → list → search → delete round-trips through the ONE service, and
/// the wire strips the embedding the way the projection promises.
#[tokio::test]
async fn insight_routes_clip_list_search_delete() {
    let (_tmp, daemon, service) = insight_fixture().await;
    let addr = crate::common::spawn_router(insight_router(daemon)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let message_id = uuid::Uuid::new_v4().to_string();
    let resp = client
        .post(format!("{base}/v1/insights/clip"))
        .json(&serde_json::json!({
            "clipped_text": "Compatibilism holds that free will is compatible with determinism.",
            "message_id": message_id,
            "paragraph_index": 3,
            "source": {
                "corpus_id": "sep",
                "article_title": "Free Will",
                "conversation_id": uuid::Uuid::new_v4().to_string(),
            },
            "position": {
                "name": "Compatibilism",
                "style": "Compatibilism",
            },
        }))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 200, "clip must answer on loopback");
    let raw = resp.text().await.unwrap();
    let clipped: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let id = clipped["insight"]["id"].as_str().unwrap().to_string();
    assert!(!id.is_empty());
    assert_eq!(clipped["insight"]["source"]["corpus_id"], "sep");
    // What the clip sent must survive the projection — a field silently
    // dropped here is the wire lying about the row (§18.3).
    assert_eq!(
        clipped["insight"]["position"]["name"], "Compatibilism",
        "the position badge must survive the clip"
    );
    // §18.4 — the projection's own promise: the embedding never crosses.
    assert!(
        !raw.contains("embedding"),
        "the wire projection must strip the embedding: {raw}"
    );

    // A second, non-matching clip — the search assertion below is only
    // discriminating if "everything" and "the query's rows" differ.
    let resp = client
        .post(format!("{base}/v1/insights/clip"))
        .json(&serde_json::json!({
            "clipped_text": "An unrelated note about resawing guitar frets.",
            "message_id": uuid::Uuid::new_v4().to_string(),
            "paragraph_index": 0,
            "source": {
                "corpus_id": null,
                "article_title": null,
                "conversation_id": uuid::Uuid::new_v4().to_string(),
            },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // LIST parity: the route's rows are the service's rows through the
    // projection — same call, same envelope.
    let resp = client
        .get(format!("{base}/v1/insights"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let served: serde_json::Value = resp.json().await.unwrap();
    let expected = serde_json::to_value(sovereign_daemon::insight_http::InsightListResponse {
        insights: service
            .store
            .list(50)
            .await
            .unwrap()
            .into_iter()
            .map(sovereign_daemon::insight_http::InsightEntry::from)
            .collect(),
    })
    .unwrap();
    assert_eq!(
        served, expected,
        "the list route must serve the service's rows through the projection"
    );

    // SEARCH finds the matching clip by text — and ONLY it: the non-matching
    // second clip stays out, which is what separates "searched" from
    // "listed everything".
    let resp = client
        .get(format!("{base}/v1/insights/search?q=compatibilism"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let served: serde_json::Value = resp.json().await.unwrap();
    let hits = served["insights"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "one of two clips matches the query");
    assert!(
        hits[0]["clipped_text"]
            .as_str()
            .unwrap()
            .contains("Compatibilism"),
        "the hit is the matching clip, not just any row"
    );

    // DELETE is 204 and the row leaves the service's own reads — the second
    // clip stays, which is what makes this a delete and not a wipe.
    let resp = client
        .delete(format!("{base}/v1/insights/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let rows = service.store.list(50).await.unwrap();
    assert_eq!(rows.len(), 1, "one writer, both reads agree");
    assert!(
        rows[0].clipped_text.contains("resawing"),
        "the deleted row is the matching one"
    );

    // A malformed UUID is a 400 naming it, not a 500.
    let resp = client
        .delete(format!("{base}/v1/insights/not-a-uuid"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// A daemon with NO insight service commissioned answers the named 503, not
/// a 404-as-unmounted — "this host built no service" is a fact a client can
/// read (§18.3).
#[tokio::test]
async fn insight_routes_without_a_service_answer_the_named_503() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        tmp.path().join("recipes"),
        tmp.path().join("indexes"),
        Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
    ));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        crate::common::desktop_services_with_insights(
            engine,
            Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
            Arc::new(TestProvider::new()),
            None,
        ),
    );
    let addr = crate::common::spawn_router(insight_router(daemon)).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/v1/insights"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("no insight service"),
        "the 503 names the absence: {body}"
    );
}

/// `POST /v1/insights/by-id` — the read `explore_insights` still does
/// in-process (sv-surface D8). Beside its five siblings rather than in
/// `d8_surface_e2e`, because the `InsightService` fixture lives here and a
/// second copy of it would be the twin this campaign deletes.
///
/// The MISSING array is the assertion that earns this route its shape: the
/// store drops ids that name no live row, so a caller comparing lengths
/// learns only that something vanished.
///
/// Watched red 2026-09-10 with ONLY the `/v1/insights/by-id` route removed
/// (the handler and its DTOs left in place, the other five routes still
/// mounted): `loopback_parity.rs:1831` — "the answer decodes", decode EOF
/// at column 0, the empty body of the 404 the method fallback produced.
///
/// The first draft of this watch did not count and is recorded so nobody
/// re-runs it: the clip body omitted `source.conversation_id`, which is a
/// required field, so every clip 422'd and the test could not have passed
/// green either. A red that a correct implementation also produces is not
/// evidence (ARCH §18.1). Fixed, watched green, then re-watched red.
#[tokio::test]
async fn insights_by_id_returns_the_nodes_and_names_the_ones_that_are_gone() {
    let (_tmp, daemon, _service) = insight_fixture().await;
    let addr = crate::common::spawn_router(insight_router(daemon)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Two real clips.
    let mut ids = Vec::new();
    for text in ["Determinism, first passage.", "Compatibilism, second."] {
        let clipped: serde_json::Value = client
            .post(format!("{base}/v1/insights/clip"))
            .json(&serde_json::json!({
                "clipped_text": text,
                "message_id": uuid::Uuid::new_v4().to_string(),
                "paragraph_index": 1,
                "source": {
                    "corpus_id": "sep",
                    "article_title": "Free Will",
                    "conversation_id": uuid::Uuid::new_v4().to_string(),
                },
            }))
            .send()
            .await
            .expect("server reachable")
            .json()
            .await
            .unwrap();
        ids.push(clipped["insight"]["id"].as_str().unwrap().to_string());
    }
    let ghost = uuid::Uuid::new_v4().to_string();

    let body: serde_json::Value = client
        .post(format!("{base}/v1/insights/by-id"))
        .json(&serde_json::json!({ "ids": [ids[0], ghost, ids[1]] }))
        .send()
        .await
        .expect("server reachable")
        .json()
        .await
        .expect("the answer decodes");

    let got = body["insights"].as_array().expect("an insights array");
    assert_eq!(got.len(), 2, "both live rows come back: {body}");
    let returned: std::collections::HashSet<&str> =
        got.iter().filter_map(|n| n["id"].as_str()).collect();
    assert!(
        returned.contains(ids[0].as_str()) && returned.contains(ids[1].as_str()),
        "the two clipped ids are the two returned: {body}"
    );
    assert!(
        got.iter().all(|n| n["embedding"].is_null()),
        "the projection strips the embedding here as it does on list: {body}"
    );
    assert_eq!(
        body["missing"].as_array().map(Vec::len),
        Some(1),
        "the id that named no live row is REPORTED, not silently dropped: {body}"
    );
    assert_eq!(
        body["missing"][0].as_str(),
        Some(ghost.as_str()),
        "and the caller is told WHICH one: {body}"
    );

    // An empty request is a successful empty answer, and both arrays are
    // present — an absent key is indistinguishable from an old host.
    let body: serde_json::Value = client
        .post(format!("{base}/v1/insights/by-id"))
        .json(&serde_json::json!({ "ids": [] }))
        .send()
        .await
        .expect("server reachable")
        .json()
        .await
        .unwrap();
    assert_eq!(body["insights"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["missing"].as_array().map(Vec::len), Some(0));

    // A malformed id is a 400 naming it — one bad id in a batch of thirty
    // is otherwise a mystery.
    let resp = client
        .post(format!("{base}/v1/insights/by-id"))
        .json(&serde_json::json!({ "ids": ["not-a-uuid"] }))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("not-a-uuid"),
        "the 400 names the id it could not parse: {body}"
    );
}

/// `POST /v1/conversations/{id}/messages/record` appends what the CLIENT
/// authored, verbatim, and drives no turn.
///
/// The route exists for the work the daemon deliberately does not do — a web
/// search the surface ran under its own egress custody, an insight preamble
/// gathered from a local tray. The desktop wrote both through its OWN
/// `SqliteStateStore` until 2026-09-12; on an attached boot that is a
/// different file from the one the sidebar lists, so the exchange landed in a
/// conversation nothing would render it in and reported `Ok`.
///
/// Three things are asserted and each has a distinct way to be wrong: the
/// messages land with the roles and metadata the caller sent (a route that
/// re-derived the role would drop `system`), the HOST minted the ids (a
/// client-chosen id is a second writer of the store's key), and NO extra
/// message appeared — a route that fell through to `collect_turn` would have
/// answered the "query" with the model and written a third row.
#[tokio::test]
async fn record_route_appends_client_authored_messages_without_a_turn() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let before = store
        .get_conversation("alpha")
        .await
        .unwrap()
        .messages
        .len();

    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/conversations/alpha/messages/record"))
        .json(&serde_json::json!({
            "messages": [
                { "role": "user", "content": "web: compatibilism" },
                {
                    "role": "assistant",
                    "content": "1. Compatibilism\nhttps://example.test\n",
                    "metadata": { "search_backend": "tavily" }
                },
                { "role": "system", "content": "gathered insight preamble" }
            ]
        }))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let ids: Vec<String> = body["message_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 3, "one id per recorded message, in order");

    let after = store.get_conversation("alpha").await.unwrap();
    assert_eq!(
        after.messages.len(),
        before + 3,
        "exactly the three recorded messages — a route that drove a turn \
         would have written a fourth"
    );
    let tail = &after.messages[after.messages.len() - 3..];
    assert_eq!(tail[0].role, Role::User);
    assert_eq!(tail[0].content, "web: compatibilism");
    assert_eq!(tail[1].role, Role::Assistant);
    assert_eq!(
        tail[1]
            .metadata
            .as_ref()
            .and_then(|m| m["search_backend"].as_str()),
        Some("tavily"),
        "the metadata blob is stored verbatim, not projected"
    );
    assert_eq!(
        tail[2].role,
        Role::System,
        "`system` is a role a client may record — the insight preamble is one"
    );
    // The ids the host minted are the ids the store keys on, so a client can
    // name the message it just recorded.
    let stored: Vec<&str> = tail.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(stored, ids.iter().map(|s| s.as_str()).collect::<Vec<_>>());
}

/// An empty `messages` list is a 400, not `{"message_ids": []}`.
///
/// A caller that meant to record an exchange and sent none has a bug, and a
/// success-shaped answer spends the caller's trust instead of their
/// attention (ARCH principle 6). Watched to fail: with the emptiness check
/// removed the route answers 200 and this test reads `left: 200, right:
/// 400`.
#[tokio::test]
async fn record_route_refuses_an_empty_list() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let before = store
        .get_conversation("alpha")
        .await
        .unwrap()
        .messages
        .len();
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/conversations/alpha/messages/record"))
        .json(&serde_json::json!({ "messages": [] }))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap_or_default().contains("empty"),
        "the refusal says what was wrong with the request: {body}"
    );
    assert_eq!(
        store
            .get_conversation("alpha")
            .await
            .unwrap()
            .messages
            .len(),
        before,
        "a refused record writes nothing"
    );
}

/// An unknown role is serde's 422, not a string match with a fall-through
/// arm — `role` is the closed set `Role` serialises (ARCH principle 9).
#[tokio::test]
async fn record_route_refuses_an_unknown_role() {
    let (_tmp, daemon, _store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/conversations/alpha/messages/record"))
        .json(&serde_json::json!({
            "messages": [{ "role": "tool", "content": "x" }]
        }))
        .send()
        .await
        .expect("server reachable");
    assert!(
        resp.status().is_client_error(),
        "an unknown role must be refused, not defaulted to `user`; got {}",
        resp.status()
    );
}
