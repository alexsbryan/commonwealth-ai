// SPDX-License-Identifier: AGPL-3.0-or-later
//! Records by identifier (OICP v0.6 §2.16) through the real client router:
//! an install of the conformance suite's records shelf
//! (`fixture/records.json`) and two books declaring an ISBN, stored as
//! ingest stores it (`common::fixture_library::install`), its lookups
//! answered by the replies recorded from Crossref and Open Library
//! (`RecordedFetch::recorded_2026_10_15`), so nothing leaves the host.

use std::sync::Arc;

use corpus_index::corpus::{recipe_sha256, Corpus};
use corpus_index::ingest_port::daemon::{
    DocumentOutcome, DocumentState, IngestResult, PreparedInstall,
};
use kernel_types::{NodeId, Sha256Hash};
use oicp_types::record::DECLARED;
use oicp_types::{IngestDocumentState, LookupOutcome, MergedRecord};
use sovereign_daemon::records_by_identifier::ResolverTable;
use sovereign_daemon::records_by_identifier::{FetchReply, RecordFetch, RecordedFetch};
use sovereign_daemon::state::{fabric, node, serving, AppState};

use crate::common::fixture_library::{install as store, Doc, DIM};
use crate::oicp_install_recipe_e2e::{install, progress, settled, RECIPE_A};
use crate::oicp_search_e2e::{hits, search};

const CORPUS: &str = "notes";

/// The records shelf and two books: one Open Library holds, one it does not.
fn shelf() -> Vec<Doc> {
    let fx: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../cmnwlth/crates/oicp-conformance/fixture/records.json"
    ))
    .unwrap();
    let mut docs: Vec<Doc> = serde_json::from_value(fx["shelf"]["documents"].clone()).unwrap();
    docs.extend(
        serde_json::from_value::<Vec<Doc>>(serde_json::json!([
            {"name": "sicp", "metadata": {"id": "sicp", "ISBN": "978-0-262-51087-5"},
             "text": "Programs must be written for people to read.\n"},
            {"name": "no-such-isbn", "metadata": {"id": "no-such-isbn", "ISBN": "9780999999990"},
             "text": "A book no catalogue holds.\n"},
        ]))
        .unwrap(),
    );
    docs
}

/// A node whose install stores the shelf and whose lookups `fetch` answers.
fn node_over(indexes: std::path::PathBuf, fetch: Arc<dyn RecordFetch>) -> AppState {
    let embed: corpus_index::types::EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; DIM]) }));
    let (at, removes) = (indexes.clone(), indexes.clone());
    let engine = crate::common::reading_double(indexes, embed)
        .on_prepare_recipe_install(move |id, toml| {
            let (dir, id, toml) = (at.clone(), id.to_string(), toml.to_string());
            Ok(PreparedInstall {
                opts_out_of_auto_enrichment: true,
                run: Box::new(move |_progress| {
                    Box::pin(async move {
                        let docs = shelf();
                        store(&dir, &id, &docs, true).await;
                        let corpus = Corpus::named(&dir, &id).expect("an id");
                        corpus.stamp_recipe_sha256(&recipe_sha256(&toml)).unwrap();
                        let documents = docs
                            .iter()
                            .map(|d| DocumentOutcome {
                                source: Some(d.name.clone()),
                                state: DocumentState::Ready {
                                    text_sha256: Some(Sha256Hash::of_str(&d.text)),
                                },
                            })
                            .collect();
                        Ok(IngestResult {
                            corpus_id: id,
                            chunks_created: docs.len() as u64,
                            index_size_bytes: 0,
                            duration_secs: 0,
                            docs_skipped: 0,
                            documents,
                            documents_unlisted: 0,
                        })
                    })
                }),
            })
        })
        .on_remove_corpus_everything(move |id| {
            let _ = std::fs::remove_dir_all(removes.join(id));
            Ok(())
        });
    AppState::new_with_platform_and_engine_and_gauge_and_fabric_and_serving_and_node(
        NodeId::from_u128(1),
        Some(Arc::new(engine)),
        None,
        fabric::FabricSeed::default(),
        serving::ServingSeed::default(),
        node::NodeSeed {
            record_fetch: Some(fetch),
            ..Default::default()
        },
    )
}

/// Install the shelf, allowing lookups or not; whether it spawned.
async fn installed(state: &AppState, allow: bool) -> bool {
    let mut body = serde_json::json!({"corpus_id": CORPUS, "recipe_toml": RECIPE_A});
    if allow {
        body["allow_lookups"] = serde_json::json!(true);
    }
    let (status, reply) = install(state.clone(), body).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{reply}");
    settled(state, CORPUS).await;
    reply["spawned"] == true
}

/// Each listed document's record, by its name.
async fn listed(state: &AppState) -> std::collections::BTreeMap<String, Option<MergedRecord>> {
    let p = progress(state.clone()).await;
    p.progress[CORPUS]
        .documents
        .iter()
        .map(|d| match &d.state {
            IngestDocumentState::Ready { record, .. } => {
                (d.source.clone().unwrap(), record.clone())
            }
            other => panic!("{:?} is listed {other:?}", d.source),
        })
        .collect()
}

fn golden(text: &str) -> MergedRecord {
    serde_json::from_str(text).unwrap()
}

/// PASS BAR: progress lists the golden records, for documents no search
/// has hit, and names the identifiers that did not resolve.
#[tokio::test]
async fn progress_lists_each_record_for_a_document_never_hit() {
    let tmp = tempfile::tempdir().unwrap();
    let fetch = Arc::new(RecordedFetch::recorded_2026_10_15());
    let state = node_over(tmp.path().to_path_buf(), fetch.clone());
    assert!(installed(&state, true).await);

    let r = listed(&state).await;
    assert_eq!(
        r["fair-principles"].as_ref(),
        Some(&golden(include_str!(
            "../fixtures/records/golden_fair-principles.json"
        )))
    );
    assert_eq!(
        r["sicp"].as_ref(),
        Some(&golden(include_str!(
            "../fixtures/records/golden_sicp.json"
        )))
    );
    for name in ["no-such-doi", "no-such-isbn"] {
        let rec = r[name].as_ref().expect("an identifier, so a record");
        assert_eq!(rec.lookups[0].outcome, LookupOutcome::Unresolved, "{name}");
        assert!(
            rec.origins.values().all(|o| o == DECLARED),
            "{name}: {rec:?}"
        );
    }
    assert_eq!(r["plain-note"], None, "it declares nothing");
    assert_eq!(fetch.asked().len(), 4, "{:?}", fetch.asked());
}

/// A hit's document, and its span's, carry the record progress lists.
#[tokio::test]
async fn a_hit_carries_the_record_progress_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let state = node_over(
        tmp.path().to_path_buf(),
        Arc::new(RecordedFetch::recorded_2026_10_15()),
    );
    assert!(installed(&state, true).await);
    let want = listed(&state).await["fair-principles"].clone();
    assert!(want.is_some());

    let (status, body) = search(
        &state,
        serde_json::json!({"query_embedding": vec![0.0_f32; DIM], "query_text": "findable accessible interoperable",
                           "corpora": [CORPUS], "limit": 10}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let name = Sha256Hash::of_str(&shelf()[0].text).to_hex();
    let hit = hits(&body)
        .into_iter()
        .find(|h| h.document.as_ref().map(|d| &d.text_sha256) == Some(&name))
        .unwrap_or_else(|| panic!("no hit on fair-principles: {body}"));
    assert_eq!(hit.document.unwrap().record, want);
    assert_eq!(hit.span.expect("a range").document.record, want);
}

/// FAILING INPUT, end to end: an install that did not allow lookups sends
/// none, and lists every lookup `not_allowed`; the same recipe allowing
/// them is a new install, which looks them up; asked again alike, it is
/// held.
#[tokio::test]
async fn an_install_that_did_not_allow_lookups_sends_none_and_the_other_allowance_reinstalls() {
    let tmp = tempfile::tempdir().unwrap();
    let fetch = Arc::new(RecordedFetch::recorded_2026_10_15());
    let state = node_over(tmp.path().to_path_buf(), fetch.clone());
    assert!(installed(&state, false).await);
    assert_eq!(fetch.asked(), Vec::<String>::new(), "nothing left the host");
    for (name, rec) in listed(&state).await {
        let Some(rec) = rec else { continue };
        assert!(
            rec.lookups
                .iter()
                .all(|l| l.outcome == LookupOutcome::NotAllowed),
            "{name}: {:?}",
            rec.lookups
        );
        assert_eq!(rec.looked_up().count(), 0, "{name}");
    }

    assert!(
        installed(&state, true).await,
        "another allowance is a new install"
    );
    assert_eq!(fetch.asked().len(), 4);
    let r = listed(&state).await;
    assert_eq!(
        r["fair-principles"].as_ref().unwrap().lookups[0].outcome,
        LookupOutcome::Resolved
    );
    assert!(!installed(&state, true).await, "the same allowance is held");
}

/// Answers through whichever [`RecordedFetch`] it holds now: the network
/// changing between two installs.
struct Switchable(std::sync::Mutex<Arc<RecordedFetch>>);

#[async_trait::async_trait]
impl RecordFetch for Switchable {
    async fn get(&self, url: &str) -> Result<FetchReply, String> {
        let now = self.0.lock().unwrap().clone();
        now.get(url).await
    }
}

/// FAILING INPUT, live 2026-10-15: Crossref answered 429 once during an
/// install, the kept records named the DOI `unreachable`, and every later
/// install of the same recipe and allowance was held, so the lookup was
/// never asked again. `unreachable` is a fact about one moment: the held
/// install asks those identifiers again, and only those.
#[tokio::test]
async fn a_held_install_asks_its_unreachable_lookups_again_and_only_those() {
    let tmp = tempfile::tempdir().unwrap();
    let doi = "10.1038/sdata.2016.18";
    let url = ResolverTable::from_data_file().unwrap().resolvers[0].url_for(doi);
    let busy = Arc::new(RecordedFetch::recorded_2026_10_15().with(url.clone(), 429, "slow down"));
    let later = Arc::new(RecordedFetch::recorded_2026_10_15());
    let net = Arc::new(Switchable(std::sync::Mutex::new(busy.clone())));
    let state = node_over(tmp.path().to_path_buf(), net.clone());

    assert!(installed(&state, true).await);
    let first = listed(&state).await;
    assert_eq!(
        first["fair-principles"].as_ref().unwrap().lookups[0].outcome,
        LookupOutcome::Unreachable
    );
    assert_eq!(
        first["fair-principles"]
            .as_ref()
            .unwrap()
            .looked_up()
            .count(),
        0
    );

    *net.0.lock().unwrap() = later.clone();
    assert!(
        !installed(&state, true).await,
        "same recipe and allowance: held"
    );
    let r = listed(&state).await;
    assert_eq!(
        r["fair-principles"].as_ref(),
        Some(&golden(include_str!(
            "../fixtures/records/golden_fair-principles.json"
        ))),
        "the looked-up fields arrive"
    );
    assert_eq!(
        later.asked(),
        vec![url],
        "only the unreachable one was asked"
    );
    assert_eq!(r["sicp"], first["sicp"], "a resolved lookup stands");
    assert_eq!(
        r["no-such-doi"].as_ref().unwrap().lookups[0].outcome,
        LookupOutcome::Unresolved,
        "an unresolved lookup stands"
    );

    assert!(!installed(&state, true).await);
    assert_eq!(
        later.asked().len(),
        1,
        "nothing left to retry, nothing asked"
    );
}
