// SPDX-License-Identifier: AGPL-3.0-or-later
//! OICP v0.5's evidence reads over real listeners on 127.0.0.1
//! (ADDRESSED_TEXT §5.2, §5.4): `GET /oicp/v1/text/{sha}` and the
//! `document` every search hit carries, locally and from a peer.
//!
//! The indexes are written the way ingest writes them — one
//! `TextWriter::store_document` per document, its chunks stamped with the
//! name — and the declared metadata is the conformance fixture's, as its
//! recipe declares it (corpus-engine's `inline_recipe_e2e` proves ingest
//! stores those bytes; this proves the wire returns them).
//!
//! The named failing inputs, each watched red:
//! - empty the document in `oicp_evidence::knowledge_results` and
//!   `a_hit_carries_its_declared_metadata_byte_for_byte` goes red;
//! - empty it on the peer route only and
//!   `a_peer_served_hit_keeps_its_catalog_metadata` goes red;
//! - cut the slice one code point long and
//!   `a_text_reads_back_by_its_name_in_code_points` goes red;
//! - read every installed corpus regardless of the caller and
//!   `a_text_held_only_outside_the_callers_scope_is_not_held` goes red;
//! - decide the read by the turn's `PrincipalScope` alone, the caller's kind
//!   unasked, and `a_member_reads_only_the_texts_of_query_sharing_corpora`
//!   goes red.

use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

use corpus_index::index::{CorpusIndex, DocSource, DocumentInput, InsertChunk, TextWriter};
use corpus_index::types::EmbedFn;
use kernel_types::{NodeId, Sha256Hash};
use oicp_types::evidence::TextSlice;
use oicp_types::{features, KnowledgeResult, ProviderManifest};
use sovereign_contracts::daemon_wire::mesh::MemberStatus;
use sovereign_contracts::traits::MeshKnowledgeSource;
use sovereign_daemon::api_keys::{self, KeyedOwners};
use sovereign_daemon::client_tokens::{
    client_tokens_dir, ClientTokenStore, Loopback, LoopbackPosture, KEY_ADMIN_GROUP,
};
use sovereign_daemon::server::{client_router, internal_router};
use sovereign_daemon::state::{AppState, NodeSeed};

use crate::common::{self, spawn_router};
use crate::knowledge_fanout_e2e::caps_with_hosted;

const DIM: usize = 8;
const FIXTURE: &str =
    include_str!("../../../../../cmnwlth/crates/oicp-conformance/fixture/library.recipe.toml");
const EXTRACTOR: &str = "plaintext@test";

fn embed() -> EmbedFn {
    Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; DIM]) }))
}

/// One document as ingest stores it.
struct Doc<'a> {
    source_id: &'a str,
    text: &'a str,
    metadata: Option<&'a str>,
}

/// `<indexes>/<id>` holding `docs`, each stored through the one writer and
/// cut into one chunk per paragraph that names it. `store` is the recipe's
/// `store_texts`; `None` writes no store at all (an index from before it).
async fn install(indexes: &Path, id: &str, docs: &[Doc<'_>], store: Option<bool>) {
    install_sharing(indexes, id, docs, store, None).await;
}

/// [`install`], with the recipe's `query_sharing` (`None`: undeclared).
async fn install_sharing(
    indexes: &Path,
    id: &str,
    docs: &[Doc<'_>],
    store: Option<bool>,
    query_sharing: Option<bool>,
) {
    let idx = CorpusIndex::create_with_sharing(
        &indexes.join(id),
        id,
        id,
        "qwen3-embedding-0.6b",
        DIM,
        true,
        query_sharing,
        "CC0",
    )
    .await
    .unwrap();
    let mut rows = Vec::new();
    let mut writer = match store {
        Some(s) => Some(TextWriter::open(&idx, EXTRACTOR, s).await.unwrap()),
        None => None,
    };
    for d in docs {
        let source = DocSource::Hashed {
            sha256: Sha256Hash::of_str(d.text),
            extractor: EXTRACTOR.into(),
        };
        let name = match writer.as_mut() {
            Some(w) => w
                .store_document(DocumentInput {
                    text: d.text,
                    source_id: d.source_id,
                    ordinal: 0,
                    source: &source,
                    metadata: d.metadata.map(Cow::Borrowed),
                })
                .unwrap(),
            None => None,
        };
        for para in d.text.split("\n\n").filter(|p| !p.trim().is_empty()) {
            rows.push((
                InsertChunk {
                    content: para.to_string(),
                    title: Some(d.source_id.to_string()),
                    url: None,
                    metadata: None,
                    content_hash: Some(kernel_types::ContentHash::of_str(para).to_hex()),
                    source_doc_id: Some(d.source_id.to_string()),
                    source_file: None,
                    code: Default::default(),
                    unit_id: None,
                    text_sha256: name,
                },
                vec![0.0_f32; DIM],
            ));
        }
    }
    if let Some(w) = writer.as_mut() {
        w.flush(&idx).await.unwrap();
    }
    idx.insert_batch(&rows).await.unwrap();
    idx.mark_ingestion_complete().unwrap();
}

/// The fixture's documents: `(name, text, declared metadata)`, read from the
/// committed recipe exactly as it declares them.
fn fixture() -> Vec<(String, String, Option<String>)> {
    let recipe: toml::Value = toml::from_str(FIXTURE).unwrap();
    recipe["document"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["name"].as_str().unwrap().to_string(),
                d["text"].as_str().unwrap().to_string(),
                d.get("metadata")
                    .and_then(|m| m.as_str())
                    .map(str::to_string),
            )
        })
        .collect()
}

async fn install_fixture(indexes: &Path, id: &str) {
    let docs = fixture();
    let docs: Vec<Doc<'_>> = docs
        .iter()
        .map(|(n, t, m)| Doc {
            source_id: n,
            text: t,
            metadata: m.as_deref(),
        })
        .collect();
    install(indexes, id, &docs, Some(true)).await;
}

fn state_over(indexes: std::path::PathBuf, seed: NodeSeed) -> AppState {
    AppState::new_with_platform_and_engine_and_gauge_and_fabric_and_serving_and_node(
        NodeId::from_u128(0xE5),
        Some(Arc::new(common::reading_double(indexes, embed()))),
        None,
        Default::default(),
        Default::default(),
        seed,
    )
}

async fn search(
    base: &str,
    query: &str,
    corpus: &str,
    bearer: Option<&str>,
) -> (String, Vec<KnowledgeResult>) {
    let mut req = reqwest::Client::new()
        .post(format!("{base}/v1/knowledge/search"))
        .json(&serde_json::json!({
            "query_embedding": vec![0.0_f32; DIM],
            "query_text": query,
            "corpora": [corpus],
            "limit": 10,
        }));
    if let Some(b) = bearer {
        req = req.bearer_auth(b);
    }
    let resp = req.send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let raw = resp.text().await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let hits = serde_json::from_value(body["results"].clone()).unwrap();
    (raw, hits)
}

async fn get(url: &str, bearer: Option<&str>) -> (u16, serde_json::Value) {
    let mut req = reqwest::Client::new().get(url);
    if let Some(b) = bearer {
        req = req.bearer_auth(b);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(serde_json::Value::Null))
}

#[tokio::test]
async fn a_hit_carries_its_declared_metadata_byte_for_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    install_fixture(&indexes, "fixture").await;
    let addr = spawn_router(client_router(state_over(indexes, NodeSeed::default()))).await;
    let base = format!("http://{addr}");

    for (name, text, metadata) in fixture() {
        let (raw, hits) = search(&base, &text[..40], "fixture", None).await;
        let own: Vec<&KnowledgeResult> = hits
            .iter()
            .filter(|h| h.source_doc_id.as_deref() == Some(name.as_str()))
            .collect();
        assert!(!own.is_empty(), "`{name}` is found: {raw}");
        for hit in &own {
            let doc = hit
                .document
                .as_ref()
                .unwrap_or_else(|| panic!("`{name}`'s hit carries no document: {raw}"));
            assert_eq!(doc.text_sha256, Sha256Hash::of_str(&text).to_hex());
            assert_eq!(doc.source.id, name);
            assert_eq!(doc.source.sha256, Some(Sha256Hash::of_str(&text).to_hex()));
            assert_eq!(doc.extractor, EXTRACTOR);
            assert!(hit.metadata.is_empty(), "the v0.2 map stays empty");
        }
        match metadata {
            Some(m) => assert!(
                raw.contains(&format!("\"metadata\":{m}")),
                "`{name}`'s declared metadata reaches the wire byte for byte: {m} not in {raw}"
            ),
            None => assert!(own
                .iter()
                .all(|h| h.document.as_ref().unwrap().metadata.is_none())),
        }
    }
}

#[tokio::test]
async fn a_peer_served_hit_keeps_its_catalog_metadata() {
    // A catalog corpus on the founder: its extractor's metadata, the shape
    // `gutenberg_catalog` gives (author, title, date).
    let catalog = r#"{"title":"Persuasion","author":"Austen, Jane","date":"1817","language":"en"}"#;
    let text =
        "Persuasion is the last novel Jane Austen completed.\n\nAnne Elliot is twenty-seven.";
    let tmp = tempfile::tempdir().unwrap();
    let indexes_a = tmp.path().join("a");
    install(
        &indexes_a,
        "catalog",
        &[Doc {
            source_id: "pg105",
            text,
            metadata: Some(catalog),
        }],
        Some(true),
    )
    .await;
    let id_a = NodeId::from_u128(0xA5);
    let state_a = AppState::new_with_platform_and_engine_and_gauge_and_fabric(
        id_a,
        Some(Arc::new(common::reading_double(indexes_a, embed()))),
        None,
        sovereign_daemon::state::FabricSeed {
            peer_transport: sovereign_daemon::double::address_transport(),
            ..Default::default()
        },
    );
    let addr_a = spawn_router(internal_router(state_a)).await;

    let id_b = NodeId::from_u128(0xB5);
    let state_b = common::state_over_roster(
        id_b,
        "evidence-fanout",
        vec![
            common::peer_row(
                id_b,
                "Joiner",
                MemberStatus::Online,
                caps_with_hosted(&[]),
                vec!["127.0.0.1:0".parse().unwrap()],
            ),
            common::peer_row(
                id_a,
                "Founder",
                MemberStatus::Online,
                caps_with_hosted(&["catalog"]),
                vec![addr_a],
            ),
        ],
    );
    let addr_b = spawn_router(client_router(state_b)).await;
    let base_b = format!("http://{addr_b}");

    let (raw, hits) = search(&base_b, "Jane Austen", "catalog", None).await;
    assert!(!hits.is_empty(), "the peer served the corpus: {raw}");
    for hit in &hits {
        assert_eq!(
            hit.peer_name.as_deref(),
            Some("Founder"),
            "served by the peer"
        );
        let doc = hit
            .document
            .as_ref()
            .unwrap_or_else(|| panic!("a peer's hit lost its document: {raw}"));
        assert_eq!(doc.source.id, "pg105");
    }
    assert!(
        raw.contains(&format!("\"metadata\":{catalog}")),
        "byte for byte across the peer: {raw}"
    );

    // The turn's own seam, which used to drop it a third time.
    let client = sovereign_turn_client::knowledge_client::MeshKnowledgeClient::new(base_b).unwrap();
    let corpora = vec!["catalog".to_string()];
    let outcome = client
        .search("Jane Austen", &vec![0.0_f32; DIM], 10, Some(&corpora))
        .await;
    assert!(!outcome.chunks.is_empty());
    for chunk in &outcome.chunks {
        let doc = chunk
            .document
            .as_ref()
            .expect("the mesh client keeps the document");
        assert_eq!(serde_json::to_string(&doc.metadata).unwrap(), catalog);
    }
}

#[tokio::test]
async fn a_text_reads_back_by_its_name_in_code_points() {
    // Multi-byte on purpose: a byte range and a code-point range disagree here.
    let text = "Café society — naïve, «quoted» words.\n\nA second paragraph: déjà vu.";
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    install(
        &indexes,
        "c",
        &[Doc {
            source_id: "s",
            text,
            metadata: None,
        }],
        Some(true),
    )
    .await;
    let addr = spawn_router(client_router(state_over(indexes, NodeSeed::default()))).await;
    let name = Sha256Hash::of_str(text).to_hex();
    let url = format!("http://{addr}/oicp/v1/text/{name}");

    let (status, body) = get(&url, None).await;
    assert_eq!(status, 200, "{body}");
    let whole: TextSlice = serde_json::from_value(body).unwrap();
    assert_eq!(whole.text, text);
    assert_eq!(
        Sha256Hash::of_str(&whole.text).to_hex(),
        name,
        "the whole text hashes to its name"
    );
    assert_eq!((whole.start, whole.end), (0, text.chars().count() as u64));
    assert_eq!((whole.before.as_str(), whole.after.as_str()), ("", ""));
    assert_eq!(whole.document.text_sha256, name);

    // "naïve" by code points, computed here and not by the host.
    let chars: Vec<char> = text.chars().collect();
    let start = text.split("naïve").next().unwrap().chars().count();
    let end = start + "naïve".chars().count();
    let (status, body) = get(&format!("{url}?start={start}&end={end}&context=3"), None).await;
    assert_eq!(status, 200, "{body}");
    let slice: TextSlice = serde_json::from_value(body).unwrap();
    assert_eq!(slice.text, "naïve", "exactly the code points asked for");
    assert_eq!(slice.text, chars[start..end].iter().collect::<String>());
    assert_eq!(
        slice.before,
        chars[start - 3..start].iter().collect::<String>()
    );
    assert_eq!(slice.after, chars[end..end + 3].iter().collect::<String>());
    assert_eq!((slice.start, slice.end), (start as u64, end as u64));

    // The named refusals.
    let len = chars.len();
    let (status, body) = get(&format!("{url}?start={len}&end={}", len + 1), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("range outside text"))
    );
    let (status, body) = get(&format!("{url}?start=5&end=4"), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("range outside text"))
    );
    let unknown = format!("http://{addr}/oicp/v1/text/{}", "0".repeat(64));
    let (status, body) = get(&unknown, None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (404, Some("text not held"))
    );
    let (status, _) = get(
        &format!("http://{addr}/oicp/v1/text/{}", "A".repeat(64)),
        None,
    )
    .await;
    assert_eq!(status, 400, "not a text name");
}

#[tokio::test]
async fn texts_not_stored_and_text_not_stored_are_refused_by_name() {
    let text = "Kept as chunks, never as a text.";
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    install(
        &indexes,
        "old",
        &[Doc {
            source_id: "s",
            text,
            metadata: None,
        }],
        None,
    )
    .await;
    install(
        &indexes,
        "optout",
        &[Doc {
            source_id: "s",
            text: "Opted out.",
            metadata: None,
        }],
        Some(false),
    )
    .await;
    let addr = spawn_router(client_router(state_over(indexes, NodeSeed::default()))).await;
    let base = format!("http://{addr}/oicp/v1/text");

    let old = Sha256Hash::of_str(text).to_hex();
    let (status, body) = get(&format!("{base}/{old}?corpus=old"), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (404, Some("texts not stored")),
        "{body}"
    );
    let (status, body) = get(&format!("{base}/{old}"), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (404, Some("text not held")),
        "no hint: {body}"
    );
    let optout = Sha256Hash::of_str("Opted out.").to_hex();
    let (status, body) = get(&format!("{base}/{optout}"), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (404, Some("text not stored")),
        "{body}"
    );
}

#[tokio::test]
async fn a_text_several_documents_share_is_served_with_the_lowest_record() {
    let text = "The same words, filed twice.";
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    install(
        &indexes,
        "b",
        &[
            Doc {
                source_id: "m",
                text,
                metadata: Some(r#"{"n":"b/m"}"#),
            },
            Doc {
                source_id: "k",
                text,
                metadata: Some(r#"{"n":"b/k"}"#),
            },
        ],
        Some(true),
    )
    .await;
    let addr = spawn_router(client_router(state_over(
        indexes.clone(),
        NodeSeed::default(),
    )))
    .await;
    let url = format!(
        "http://{addr}/oicp/v1/text/{}",
        Sha256Hash::of_str(text).to_hex()
    );
    let (_, body) = get(&url, None).await;
    let slice: TextSlice = serde_json::from_value(body).unwrap();
    assert_eq!(
        slice.document.source.id, "k",
        "lowest source id within a corpus"
    );

    install(
        &indexes,
        "a",
        &[Doc {
            source_id: "z",
            text,
            metadata: Some(r#"{"n":"a/z"}"#),
        }],
        Some(true),
    )
    .await;
    let (_, body) = get(&url, None).await;
    let slice: TextSlice = serde_json::from_value(body).unwrap();
    assert_eq!(
        slice.document.metadata,
        Some(serde_json::json!({"n": "a/z"})),
        "the lowest corpus first"
    );
}

const IT: &str = "it-key-000000000000000000000000000000000000000000000000000000000";

#[tokio::test]
async fn a_text_held_only_outside_the_callers_scope_is_not_held() {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    install(
        &indexes,
        "firm-docs",
        &[Doc {
            source_id: "f",
            text: "Granted words.",
            metadata: None,
        }],
        Some(true),
    )
    .await;
    install(
        &indexes,
        "secret",
        &[Doc {
            source_id: "s",
            text: "Secret words.",
            metadata: None,
        }],
        Some(true),
    )
    .await;
    let dir = client_tokens_dir(tmp.path());
    // As `svrn daemon key --add` writes it, declared `none`; the daemon then
    // reads the keys on disk with no declaration of its own.
    let installer = ClientTokenStore::load(
        Some(dir.clone()),
        LoopbackPosture {
            loopback: Loopback::None,
            declared: true,
        },
    );
    installer
        .mint("it", &[KEY_ADMIN_GROUP.to_string()], IT.to_string())
        .unwrap();
    let posture = LoopbackPosture::resolve(None, Some(&dir)).unwrap();
    let seed = NodeSeed {
        named_client_tokens: Arc::new(ClientTokenStore::load(Some(dir), posture)),
        corpus_principal: Some(Arc::new(KeyedOwners {
            grant: vec!["firm-docs".into()],
        })),
        ..Default::default()
    };
    let state = state_over(indexes, seed);
    let addr = spawn_router(api_keys::seal(client_router(state.clone()), &state)).await;
    let base = format!("http://{addr}/oicp/v1/text");

    let granted = Sha256Hash::of_str("Granted words.").to_hex();
    let (status, body) = get(&format!("{base}/{granted}"), Some(IT)).await;
    assert_eq!(status, 200, "{body}");
    let secret = Sha256Hash::of_str("Secret words.").to_hex();
    let (status, body) = get(&format!("{base}/{secret}"), Some(IT)).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (404, Some("text not held")),
        "held only outside the key's grant: {body}"
    );
    let (status, body) = get(&format!("{base}/{secret}?corpus=secret"), Some(IT)).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (404, Some("text not held")),
        "a hint grants nothing: {body}"
    );
}

/// A mesh member reads the texts of the corpora declared shared with the mesh
/// (`query_sharing`) and no others, the reach its federated search has; the
/// owner reads both. Red against a read scope that asks only the turn's
/// `PrincipalScope`, which admits every corpus on an unkeyed daemon.
#[tokio::test]
async fn a_member_reads_only_the_texts_of_query_sharing_corpora() {
    use axum::extract::{Path as UrlPath, Query, State};
    use axum::Extension;
    use sovereign_contracts::principal::{AttachedPrincipal, Principal};
    use sovereign_daemon::routes_oicp_text::{text, TextQuery};

    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    for (id, words, shared) in [
        ("open", "Shared words.", true),
        ("own", "Private words.", false),
    ] {
        let doc = [Doc {
            source_id: "s",
            text: words,
            metadata: None,
        }];
        install_sharing(&indexes, id, &doc, Some(true), Some(shared)).await;
    }
    let state = state_over(indexes, NodeSeed::default());
    let read = |who: Principal, words: &str| {
        let state = state.clone();
        let name = Sha256Hash::of_str(words).to_hex();
        async move {
            let attached = Some(Extension(AttachedPrincipal(who)));
            let resp = text(
                State(state),
                attached,
                UrlPath(name),
                Query(TextQuery::default()),
            )
            .await;
            let status = resp.status().as_u16();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            (status, body["error"].as_str().map(str::to_string))
        }
    };
    let member = || Principal::Member {
        node_id: NodeId::from_u128(7),
    };
    let owner = || Principal::LocalOwner { sub_identity: None };

    assert_eq!(read(member(), "Shared words.").await, (200, None));
    assert_eq!(
        read(member(), "Private words.").await,
        (404, Some("text not held".into())),
        "a corpus not shared with the mesh is not held for a member"
    );
    assert_eq!(read(owner(), "Private words.").await, (200, None));
    assert_eq!(
        read(Principal::Unverified, "Shared words.").await,
        (404, Some("text not held".into())),
        "an identity the node could not verify reads nothing"
    );
}

#[tokio::test]
async fn the_manifest_advertises_the_text_read_and_documents_on_hits() {
    let tmp = tempfile::tempdir().unwrap();
    let addr = spawn_router(client_router(state_over(
        tmp.path().join("indexes"),
        NodeSeed::default(),
    )))
    .await;
    let m: ProviderManifest = reqwest::get(format!("http://{addr}/oicp/v1/capabilities"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(m.has_feature(features::EVIDENCE_TEXT));
    assert!(m.has_feature(features::KNOWLEDGE_DOCUMENT));
    let ev = m
        .knowledge
        .as_ref()
        .and_then(|k| k.evidence.as_ref())
        .expect("knowledge.evidence");
    assert_eq!(ev.text_endpoint, "/oicp/v1/text");
}
