// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the loopback_parity e2e suite — split from loopback_parity.rs for the §3.2 size ceiling (behaviour-preserving move).

use crate::common::TestProvider;

use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::types::projection::{project_epistemic_state, project_message_metadata};
use sovereign_daemon::daemon::EmbeddedDaemon;
use sovereign_daemon::reading_http::reading_router;
use sovereign_daemon::turn_http::{
    turn_router, ConversationListEntry, ConversationListResponse, ConversationResponse,
    MessageEntry,
};

use super::conversation_fixture;

// ── sv-surface rung 1: the corpus-status route serves the one decider ────
//
// The parity instrument for the corpus-status family: the route's bytes and
// `scan_corpus_rows`'s bytes on the SAME fixture are equal, so the wire's
// answer and the CLI's printed answer cannot drift apart. The CLI prints
// from the same function (sovereign-cli-llm corpus_cmd/status.rs imports it
// since rung 1); before the rung it walked the indexes dir privately while
// the desktop walked it through `installed_indexes` — the §10.6 twin.

/// One minimal READY corpus + one in-flight PARTITION, on disk, under the
/// engine's index dir — the two states a status surface must never confuse
/// (a partition named as a corpus is the 2026-08-12 regression recorded in
/// the decider's tests).
fn write_fixture_meta(dir: &std::path::Path, corpus_id: &str, ingestion_in_progress: bool) {
    std::fs::create_dir_all(dir).unwrap();
    let meta = serde_json::json!({
        "corpus_id": corpus_id,
        "corpus_name": format!("{corpus_id} (fixture)"),
        "embedding_model": "qwen-embedding-0.6b",
        "embedding_dimensions": 1024,
        "mesh_sharing": false,
        "license": "private",
        "created_at": 1_786_548_248_u64,
        "last_updated": 1_786_548_248_u64,
        "schema_version": 3,
        "is_shard": false,
        "ingestion_in_progress": ingestion_in_progress,
        "indexes_built": !ingestion_in_progress,
    });
    std::fs::write(
        corpus_index::corpus::Corpus::meta_in(dir),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn corpus_status_route_serves_the_one_deciders_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    write_fixture_meta(&indexes.join("ready-corpus"), "ready-corpus", false);
    write_fixture_meta(
        &indexes.join("building-corpus-partition-node-1"),
        "building-corpus",
        true,
    );

    // A daemon whose ServingCore carries a REAL engine over that fixture —
    // through THE assembler, like every production site.
    let engine = std::sync::Arc::new(corpus_engine::CorpusEngine::new(
        tmp.path().join("recipes"),
        indexes.clone(),
        std::sync::Arc::new(|_t: &str| {
            Box::pin(async { Ok(vec![0.0_f32; 8]) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = corpus_index::Result<Vec<f32>>> + Send>,
                >
        }),
    ));
    let daemon = EmbeddedDaemon::in_memory(
        SetupConfig::unconfigured(),
        crate::common::desktop_services_with_engine(engine),
    );
    let addr = crate::common::spawn_router(reading_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .get(format!("{base}/internal/corpus/status"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        200,
        "the status route must answer on loopback"
    );
    let served: serde_json::Value = resp.json().await.expect("rows serialize as JSON");

    // PARITY, pinned: the route's bytes are the decider's bytes on the same
    // fixture — not merely "also correct", but THE SAME value.
    let rows = corpus_engine::engine::status::scan_corpus_rows(&indexes).unwrap();
    let printed: serde_json::Value = serde_json::to_value(&rows).unwrap();
    assert_eq!(
        served, printed,
        "the route and `svrn corpus status` must serve one decider's rows"
    );

    // And the fixture's two states survive the wire with their labels —
    // the spelling the CLI-contract journey greps for. (Rows arrive in
    // `corpus_id` order — the decider's BTreeMap — so look them up rather
    // than trusting fixture-write order.)
    let by_id = |v: &serde_json::Value, id: &str| {
        v.as_array()
            .expect("rows serve as an array")
            .iter()
            .find(|r| r["corpus_id"] == id)
            .unwrap_or_else(|| panic!("no row for {id} in {v}"))
            .clone()
    };
    let ready = by_id(&served, "ready-corpus");
    assert_eq!(ready["state_label"], "ready");
    let building = by_id(&served, "building-corpus");
    assert_eq!(building["state_label"], "building");
}

/// The list route's bytes are the wire schema's serialization of
/// `store.list_conversations` — same query, same rows, same envelope.
#[tokio::test]
async fn conversation_list_route_serves_the_stores_rows() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 200, "the list route must answer on loopback");
    let raw = resp.text().await.unwrap();

    // PARITY: expected is built in-test from the SAME store call the route
    // makes, through the wire type — anything the route re-derived or dropped
    // shows up as a byte difference.
    let rows = store.list_conversations(20, 0).await.unwrap();
    let expected = serde_json::to_value(ConversationListResponse {
        conversations: rows
            .iter()
            .map(|c| ConversationListEntry {
                id: c.id.clone(),
                title: c.title.clone(),
                created_at: c.created_at,
                updated_at: c.updated_at,
            })
            .collect(),
    })
    .unwrap();
    let served: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        served, expected,
        "the list route must serve the store's rows through the wire envelope"
    );

    // Membership + the two shapes that must never blur: a titled row carries
    // its title, an untitled row OMITS the key (sovereign-server's
    // skip_serializing_if — `"title": null` is the rung-4 drift, caught there
    // only because a test like this one did not exist for reading).
    let ids: Vec<&str> = served["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"alpha") && ids.contains(&"beta"),
        "ids: {ids:?}"
    );
    assert!(
        !raw.contains("\"title\":null") && !raw.contains("\"title\": null"),
        "an untitled conversation must omit the title key, not null it — got {raw}"
    );

    // The server's pagination defaults are the route's: limit/offset reach
    // the store call verbatim.
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations?limit=1&offset=0"))
        .send()
        .await
        .unwrap();
    let served: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(served["conversations"].as_array().unwrap().len(), 1);
}

/// The get route's bytes are the canonical projections of the store's row —
/// the same deciders sovereign-server's `get_conversation` calls, so a
/// resumed conversation renders identically from either host.
#[tokio::test]
async fn conversation_get_route_serves_the_canonical_projection() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/alpha"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 200);
    let raw = resp.text().await.unwrap();

    // PARITY: expected is built in-test by running the CONTRACTS-LAYER
    // projections over the store's row — not by parsing what the route
    // served.
    let convo = store.get_conversation("alpha").await.unwrap();
    let expected = serde_json::to_value(ConversationResponse {
        id: "alpha".to_string(),
        title: convo.title.clone(),
        messages: convo
            .messages
            .iter()
            .map(|m| {
                let role = m.role_str().to_string();
                let (provenance, citations) = project_message_metadata(&m.metadata);
                MessageEntry {
                    id: m.id.clone(),
                    role,
                    content: m.content.clone(),
                    created_at: m.created_at,
                    provenance,
                    citations,
                    epistemic_state: project_epistemic_state(&m.metadata),
                    // The blob rides the route verbatim (svt-3: the desktop
                    // reads its `MessageCompletePayload.metadata` from here
                    // now), so parity means the same blob, not a projection
                    // of it.
                    metadata: m.metadata.clone(),
                }
            })
            .collect(),
        created_at: convo.created_at,
        updated_at: convo.updated_at,
        // Parity means the row's own allow-list, not a projection of it:
        // `enabled_corpora` rides the route verbatim (2026-09-12), because
        // the desktop's `CorpusFilterStrip` renders the chips from it and a
        // route that dropped the field would have rendered every scoped
        // conversation as unscoped.
        enabled_corpora: convo.enabled_corpora.clone(),
    })
    .unwrap();

    // §18.4 — validate the instrument before trusting the equality: the
    // fixture's metadata MUST project to Some/Some/nonempty, else both sides
    // are None and the test passes vacuously.
    let assistant = &expected["messages"][1];
    assert!(
        assistant.get("provenance").is_some(),
        "fixture must project provenance, else this test proves nothing"
    );
    assert!(
        !assistant["citations"].as_array().unwrap().is_empty(),
        "fixture must project citations"
    );
    assert!(
        assistant.get("epistemic_state").is_some(),
        "fixture must project an epistemic ledger"
    );

    let served: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        served, expected,
        "the get route must serve the contracts-layer projection of the row"
    );

    // The allow-list rides the row (2026-09-12). Asserted HERE rather than
    // only in the parity equality above because the parity build reads the
    // same field from the same row — so both sides would be `None` together
    // and the equality would pass with the field absent from the envelope.
    // These two assertions are what actually fail when the field is not on
    // the wire (watched: `enabled_corpora` removed from
    // `ConversationResponse` and the handler, `alpha` fails `left: None,
    // right: Some(["sep", "wikipedia"])`; restored after).
    let served_now: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        served_now["enabled_corpora"],
        serde_json::json!(["sep", "wikipedia"]),
        "the scoped conversation's allow-list must ride the get route — \
         the desktop's CorpusFilterStrip renders these chips, and an absent \
         key reads as `all installed corpora`, which is the wrong answer \
         wearing the default's shape"
    );
    let bare: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/beta"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        bare.get("enabled_corpora").is_none(),
        "an unscoped conversation OMITS the key rather than nulling it — \
         same discipline the title carries; got {bare}"
    );

    // A bare row serves bare: beta has no metadata, so its wire form carries
    // neither provenance nor citations keys at all (absent stays absent).
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/beta"))
        .send()
        .await
        .unwrap();
    let raw = resp.text().await.unwrap();
    assert!(
        !raw.contains("provenance") && !raw.contains("citations") && !raw.contains("title"),
        "a bare row's wire form must omit every optional key — got {raw}"
    );
}

/// A missing row answers the server's exact 404 sentence — not a generic
/// daemon error, which would be a byte a client could tell hosts apart by.
#[tokio::test]
async fn conversation_get_missing_is_the_servers_404() {
    let (_tmp, daemon, _store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/v1/conversations/nope"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body,
        serde_json::json!({ "error": "Conversation not found" }),
        "the 404 body is sovereign-server's spelling (routes.rs)"
    );
}

/// Delete is the server's 204-no-body, and the row is really gone — the
/// read routes answer 404 for it afterward, through the shared store.
#[tokio::test]
async fn conversation_delete_route_is_204_and_removes_the_row() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .delete(format!("{base}/v1/conversations/beta"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert!(
        resp.text().await.unwrap().is_empty(),
        "204 carries no body (the server's spelling)"
    );

    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/beta"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // And the list still serves parity against the store AFTER the delete —
    // one writer, both reads agree.
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations"))
        .send()
        .await
        .unwrap();
    let served: serde_json::Value = resp.json().await.unwrap();
    let rows = store.list_conversations(20, 0).await.unwrap();
    let expected = serde_json::to_value(ConversationListResponse {
        conversations: rows
            .iter()
            .map(|c| ConversationListEntry {
                id: c.id.clone(),
                title: c.title.clone(),
                created_at: c.created_at,
                updated_at: c.updated_at,
            })
            .collect(),
    })
    .unwrap();
    assert_eq!(served, expected);
}

/// The two SCOPES a sidebar needs. Until sv-surface this route could only
/// page everything, so the desktop listed from its own `SqliteStateStore` —
/// and in attach that is not the store `create`, `rename` and `delete`
/// write, so a conversation never appeared in the list it was created from.
///
/// The three `skill_id` states are the point: absent pages everything (the
/// server's shape, unchanged), empty is the DEFAULT surface, and a named id
/// is that surface. A route that could not say the middle one would have to
/// serve a scoped sidebar from the unscoped listing, which widens
/// cross-surface visibility silently (§18.3).
#[tokio::test]
async fn conversation_list_route_scopes_by_surface_and_by_corpus() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    // alpha + beta are default-surface. Add one tagged surface row and put
    // an allow-list on alpha so the corpus scope has something to find.
    store
        .insert_empty_conversation("gamma", 300, Some("inner-work"))
        .await
        .unwrap();
    store
        .set_conversation_enabled_corpora("alpha", Some(vec!["sep".to_string()]))
        .await
        .unwrap();
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");
    let http = reqwest::Client::new();

    let ids = |v: &serde_json::Value| -> Vec<String> {
        v["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_string())
            .collect()
    };

    // Absent: everything, including the tagged row.
    let all: serde_json::Value = http
        .get(format!("{base}/v1/conversations"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut all_ids = ids(&all);
    all_ids.sort();
    assert_eq!(all_ids, vec!["alpha", "beta", "gamma"]);

    // Empty: the DEFAULT surface only — gamma is not a sidebar row.
    let default: serde_json::Value = http
        .get(format!("{base}/v1/conversations?skill_id="))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut default_ids = ids(&default);
    default_ids.sort();
    assert_eq!(
        default_ids,
        vec!["alpha", "beta"],
        "an empty skill_id must mean `skill_id IS NULL`, not 'no scoping'"
    );

    // Named: that surface only.
    let inner: serde_json::Value = http
        .get(format!("{base}/v1/conversations?skill_id=inner-work"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ids(&inner), vec!["gamma"]);

    // Corpus: the default-surface rows whose allow-list names it. beta has
    // a NULL allow-list — everything-scoped, so not one of this notebook's
    // threads — and gamma is the wrong surface.
    let notebook: serde_json::Value = http
        .get(format!("{base}/v1/conversations?corpus_id=sep"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        ids(&notebook),
        vec!["alpha"],
        "an everything-scoped conversation is not a notebook's thread"
    );

    // Two scopes at once is a refusal, not a coin flip.
    let resp = http
        .get(format!("{base}/v1/conversations?skill_id=&corpus_id=sep"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// Rename crosses the wire, and the three rules that shape a title travel
/// WITH the write rather than with each surface that offers a rename box
/// (§10.6). The desktop carried its own trim + empty-refusal + 200-char
/// clamp until sv-surface, and in attach mode applied all three to a row
/// the served sidebar never reads — so the old name came back on the next
/// list.
#[tokio::test]
async fn conversation_patch_route_renames_the_row_and_owns_the_title_rules() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");
    let http = reqwest::Client::new();

    // Padding is trimmed and a 300-character title clamps to 200 — by the
    // HANDLER, so a caller that sends neither rule gets both.
    let padded = format!("  {}  ", "x".repeat(300));
    let resp = http
        .patch(format!("{base}/v1/conversations/alpha"))
        .json(&serde_json::json!({ "title": padded }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert!(
        resp.text().await.unwrap().is_empty(),
        "204 carries no body — the shape delete already answers"
    );
    let row = store.get_conversation("alpha").await.unwrap();
    assert_eq!(
        row.title.as_deref(),
        Some("x".repeat(200).as_str()),
        "the handler trims and clamps; the row is the proof"
    );

    // And the read route serves what the write landed — one writer, and the
    // list a sidebar renders agrees with it.
    let served: serde_json::Value = http
        .get(format!("{base}/v1/conversations/alpha"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(served["title"], serde_json::json!("x".repeat(200)));

    // An empty title is refused in the host's words rather than written.
    let resp = http
        .patch(format!("{base}/v1/conversations/alpha"))
        .json(&serde_json::json!({ "title": "   " }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // A body naming no updatable field is a 400, not a 204 that changed
    // nothing — absence is reported, never defaulted (§18.3).
    let resp = http
        .patch(format!("{base}/v1/conversations/alpha"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // The row survived both refusals intact.
    assert_eq!(
        store
            .get_conversation("alpha")
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("x".repeat(200).as_str()),
    );

    // A conversation this daemon does not hold is the get route's 404.
    let resp = http
        .patch(format!("{base}/v1/conversations/ghost"))
        .json(&serde_json::json!({ "title": "anything" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// The one-shot messages route drives the SAME driver the WebSocket stream
/// runs (`collect_turn` → `serve_turn`), and the turn it wrote is what the
/// get route then serves — one writer, REST and WS and read all agreeing.
#[tokio::test]
async fn conversation_messages_route_serves_one_collect_turn() {
    let provider =
        TestProvider::new().with_stream_chunks(vec!["one ".to_string(), "two".to_string()]);
    let (_tmp, daemon, store) = conversation_fixture(provider).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/conversations/alpha/messages"))
        .json(&serde_json::json!({ "content": "hello" }))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 200, "the one-shot turn route must answer");
    let reply: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(reply["role"], "assistant", "the server's spelling");
    assert_eq!(reply["content"], "one two");
    let message_id = reply["message_id"].as_str().unwrap().to_string();
    assert!(!message_id.is_empty());

    // The reply IS the persisted row — the route did not fabricate an answer
    // shape the store never saw.
    let convo = store.get_conversation("alpha").await.unwrap();
    let persisted = convo
        .messages
        .iter()
        .find(|m| m.id == message_id)
        .unwrap_or_else(|| panic!("message {message_id} not persisted"));
    assert_eq!(persisted.content, "one two");

    // And the get route serves it: 2 seeded + 1 user + 1 assistant.
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/alpha"))
        .send()
        .await
        .unwrap();
    let served: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(served["messages"].as_array().unwrap().len(), 4);
}

// ── sv-surface rung 6 commit A: search / memory / insight siblings ─────────
//
// The rung-3 parity discipline extended to the routes the attach boot needs
// beside the CRUD surface: message search (the store's own search decider),
// the memory tombstone/weaken pair (the halving formula moves HERE, off the
// desktop command — one decider), and the insight surface (the same
// `InsightService` the desktop builds, served on loopback). Parity means the
// same thing it meant at rung 3: on the SAME fixture, the route's bytes
// equal the canonical value's bytes — the route adds nothing, drops nothing,
// re-derives nothing.

/// The search route's bytes are the store's `search_messages` rows through
/// the wire envelope — the SAME trait call the desktop's in-process command
/// makes, so wire and local answers cannot drift.
#[tokio::test]
async fn conversation_search_route_serves_the_stores_rows() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/search?q=compatibilism"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        200,
        "the search route must answer on loopback"
    );
    let served: serde_json::Value = resp.json().await.unwrap();

    // PARITY: expected built in-test from the SAME store call the route
    // makes, capped the way the route caps (50 — the desktop's own cap,
    // moved into the route so it has one home).
    let rows = store.search_messages("compatibilism").await.unwrap();
    let expected = serde_json::json!({
        "results": rows
            .iter()
            .take(50)
            .map(|m| serde_json::json!({
                "content": m.content,
                "conversation_id": m.conversation_id,
            }))
            .collect::<Vec<_>>(),
    });
    assert_eq!(
        served, expected,
        "the search route must serve the store's rows verbatim"
    );
    // And the fixture must have matched at all — §18.4, the instrument
    // validates itself before the equality above can mean anything.
    assert!(
        !served["results"].as_array().unwrap().is_empty(),
        "fixture must match the query or this test proves nothing"
    );

    // A missing q is a 400 naming it, not an empty 200 — an empty result
    // set would be indistinguishable from "nothing matched" (§18.3).
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/search"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// Weaken halves the daemon's own row's confidence (the ONE decider for the
/// formula — it lived in the desktop command until this route), delete
/// tombstones, and a missing id is a 404 that names it.
#[tokio::test]
async fn memory_routes_weaken_tombstone_and_404() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    use sovereign_contracts::types::Memory;
    let seed = |id: &str, confidence: f64| Memory {
        id: id.to_string(),
        content: format!("memory {id}"),
        source: "conversation_extraction".to_string(),
        confidence,
        created_at: 100,
        last_used: 100,
        ..Memory::default()
    };
    let half = seed("mem-half", 0.8);
    let gone = seed("mem-gone", 0.5);
    futures::future::join_all([store.save_memory(&half), store.save_memory(&gone)]).await;

    // Weaken: the response carries the new confidence AND the daemon's row
    // carries it — the read-modify-write happened server-side, not on a
    // client's snapshot.
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/memories/mem-half/weaken"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["confidence"], 0.4, "0.8 halved once");
    let rows = store.get_all_memories().await.unwrap();
    let row = rows.iter().find(|m| m.id == "mem-half").unwrap();
    assert_eq!(row.confidence, 0.4, "the daemon's row was weakened");

    // The floor: a confidence already at 0 stays 0 (max(0.0) is not a
    // negative-zero surprise).
    store.save_memory(&seed("mem-floor", 0.0)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/memories/mem-floor/weaken"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["confidence"], 0.0);

    // Delete: 204 no body, and the row is gone from the daemon's reads.
    let resp = reqwest::Client::new()
        .delete(format!("{base}/v1/memories/mem-gone"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let rows = store.get_all_memories().await.unwrap();
    assert!(
        rows.iter().all(|m| m.id != "mem-gone"),
        "tombstoned memory must leave the recall set"
    );

    // A missing id is the route's own 404 naming the id — not the store's
    // error string and not a silent 204 (§18.3).
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/memories/no-such-memory/weaken"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("no-such-memory"),
        "the 404 names the missing id: {body}"
    );
}
