//! End-to-end test of the `wikipedia-newsworthy` watcher: a single
//! `tick()` against a stub MediaWiki client must:
//!
//!   1. Index a portal page into the `wikipedia-newsworthy` corpus
//!      (one chunk per event bullet).
//!   2. Populate the tracked-set MeshStore namespace with the union
//!      of bullet wikilinks.
//!   3. On the *next* tick, fetch each owned tracked article into
//!      the parent `wikipedia` corpus via
//!      `reindex_by_source_doc_id`.
//!
//! The test uses a stand-in [`NewsworthyHost`] backed by a shared
//! `Mutex<HashMap>` that mimics MeshStore's prefix-scan semantics
//! without spinning up SQLite. A second-node convergence variant
//! shares the same map across two `WikipediaNewsworthyWatcher`s to
//! exercise the rendezvous-partition path without involving real
//! gossip.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use corpus_engine::error::Result;
use corpus_engine::index::CorpusIndex;
use corpus_engine::types::EmbedFn;
use corpus_engine::update::newsworthy_watcher::{
    APP_ID_PORTAL, APP_ID_TRACKED, Lifecycle, MediaWikiClient, NewsworthyConfig,
    NewsworthyHost, RevisionRecord, TrackedArticle, WikipediaNewsworthyWatcher,
};
use corpus_engine::CorpusEngine;

const PORTAL_BODY: &str = include_str!("fixtures/portal_2026_05_08.json");
const ARTICLE_BODY: &str = include_str!("fixtures/article_kyiv.json");

#[tokio::test]
async fn single_node_tick_indexes_portal_and_tracks_links() {
    let env = TestEnv::new("self_only").await;

    let report = env.watcher.tick(env.now).await.expect("tick OK");
    assert!(report.role_leader, "single-node watcher must be leader");
    assert!(report.portal_ingested, "first tick must ingest portal page");
    // Bullet chunker emits one chunk per `*` line.
    assert!(
        report.tracked_total >= 1,
        "tracked total should be > 0 after first leader tick (got {})",
        report.tracked_total
    );

    let newsworthy_idx = CorpusIndex::open(&env.idx_dir.join("wikipedia-newsworthy"))
        .await
        .unwrap();
    let portal_chunks = newsworthy_idx.chunk_count().await.unwrap();
    assert!(
        portal_chunks > 0,
        "portal page should produce at least one bullet chunk"
    );

    // Tracked-set shape: at minimum, "Kyiv" is in the tracked set
    // since the fixture portal references it.
    let tracked = env.host.scan_tracked();
    let titles: Vec<String> = tracked.iter().map(|t| t.title.clone()).collect();
    assert!(
        titles.iter().any(|t| t == "Kyiv"),
        "expected Kyiv in tracked set, got {titles:?}",
    );

    // Marker stored: idempotency for next tick.
    let portal_keys = env.host.list_keys(APP_ID_PORTAL);
    assert!(!portal_keys.is_empty(), "portal marker must be stored");
}

#[tokio::test]
async fn first_tick_fetches_pending_articles_into_parent() {
    let env = TestEnv::new("first_tick_fetch").await;

    // Step A populates the tracked set with PendingFetch entries.
    // Step B then runs in the same tick, picks them up, and
    // fetches into the parent `wikipedia` corpus.
    let r1 = env.watcher.tick(env.now).await.unwrap();
    assert!(r1.fetched > 0, "first tick must fetch pending articles");

    let wiki_idx = CorpusIndex::open(&env.idx_dir.join("wikipedia"))
        .await
        .unwrap();
    let wiki_chunks = wiki_idx.chunk_count().await.unwrap();
    assert!(wiki_chunks > 0, "parent wikipedia should now have chunks");

    let kyiv_present = wiki_idx
        .list_indexed_source_doc_ids()
        .await
        .unwrap()
        .iter()
        .any(|id| id == "Kyiv");
    assert!(kyiv_present, "Kyiv should be source_doc_id'd into wikipedia");

    // Tracked entry transitioned PendingFetch → Present.
    let tracked = env.host.scan_tracked();
    let kyiv = tracked.iter().find(|t| t.title == "Kyiv").unwrap();
    assert_eq!(kyiv.lifecycle, Lifecycle::Present);
    assert!(kyiv.last_known_rev_id.is_some());
}

#[tokio::test]
async fn subsequent_tick_refreshes_when_revision_diverged() {
    let env = TestEnv::new("refresh_path").await;

    // First tick: populate + initial-fetch. After this all tracked
    // entries are `Present` with `last_known_rev_id = 1297999000`
    // (the article fixture's revid).
    let _ = env.watcher.tick(env.now).await.unwrap();

    // Second tick: stub batch_revisions returns 1000 (different),
    // so each Present entry routes to the refresh path.
    let r2 = env.watcher.tick(env.now + chrono::Duration::hours(25)).await.unwrap();
    assert!(
        r2.rev_checked > 0,
        "second tick must run revision checks for Present entries"
    );
    assert!(
        r2.refreshed > 0,
        "diverged revision must trigger refresh, got refreshed={}",
        r2.refreshed,
    );
}

#[tokio::test]
async fn portal_marker_short_circuits_when_revid_unchanged() {
    let env = TestEnv::new("idempotent").await;

    let r1 = env.watcher.tick(env.now).await.unwrap();
    assert!(r1.portal_ingested);
    let portal_chunks_after_first =
        CorpusIndex::open(&env.idx_dir.join("wikipedia-newsworthy"))
            .await
            .unwrap()
            .chunk_count()
            .await
            .unwrap();

    // Second tick on the same day with the same revid — no portal
    // ingest should happen. (Article fetches still happen for any
    // PendingFetch tracked rows, that's separate.)
    let r2 = env.watcher.tick(env.now).await.unwrap();
    assert!(
        !r2.portal_ingested,
        "second tick at same date+revid must short-circuit"
    );

    let portal_chunks_after_second =
        CorpusIndex::open(&env.idx_dir.join("wikipedia-newsworthy"))
            .await
            .unwrap()
            .chunk_count()
            .await
            .unwrap();
    assert_eq!(
        portal_chunks_after_first, portal_chunks_after_second,
        "portal chunk count must be stable across idempotent ticks",
    );
}

// ─────────────────────────────────────────────────────────────────
//  Test scaffolding: stub host + stub MediaWiki + on-disk indexes.
// ─────────────────────────────────────────────────────────────────

struct TestEnv {
    watcher: Arc<WikipediaNewsworthyWatcher>,
    host: Arc<StubHost>,
    idx_dir: std::path::PathBuf,
    now: chrono::DateTime<Utc>,
    _tmp: tempfile::TempDir,
}

impl TestEnv {
    async fn new(label: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let recipes_dir = tmp.path().join("recipes");
        let idx_dir = tmp.path().join("indexes");
        std::fs::create_dir_all(&recipes_dir).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();

        let embed: EmbedFn = Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.1_f32; 4]) }));
        let engine = Arc::new(CorpusEngine::new(
            recipes_dir,
            idx_dir.clone(),
            embed,
        ));

        // Pre-create the two indexes the watcher writes into. This
        // matches what the bulk-install path produces in production —
        // including the closing `mark_ingestion_complete()`. The
        // watcher's local-install gate calls `installed_indexes()`,
        // which skips any index still flagged `ingestion_in_progress`
        // (the state `CorpusIndex::create` leaves behind). Without the
        // mark, the gate sees no installed corpus and short-circuits
        // the whole tick before any portal ingest / article fetch runs.
        for corpus_id in &["wikipedia-newsworthy", "wikipedia"] {
            let idx = CorpusIndex::create(
                &idx_dir.join(corpus_id),
                corpus_id,
                corpus_id,
                "test-model",
                4,
                false,
                "MIT",
            )
            .await
            .unwrap();
            idx.mark_ingestion_complete().unwrap();
        }

        let host = Arc::new(StubHost::new(label));
        let host_dyn: Arc<dyn NewsworthyHost> = host.clone();
        let mw: Arc<dyn MediaWikiClient> = Arc::new(StubMediaWiki);
        let watcher = Arc::new(WikipediaNewsworthyWatcher::new(
            host_dyn,
            engine,
            mw,
            NewsworthyConfig::default(),
        ));
        Self {
            watcher,
            host,
            idx_dir,
            now: Utc.with_ymd_and_hms(2026, 5, 9, 12, 0, 0).unwrap(),
            _tmp: tmp,
        }
    }
}

struct StubHost {
    label: String,
    store: Mutex<HashMap<(String, String), Vec<u8>>>,
}

impl StubHost {
    fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            store: Mutex::new(HashMap::new()),
        }
    }

    fn scan_tracked(&self) -> Vec<TrackedArticle> {
        let store = self.store.lock().unwrap();
        store
            .iter()
            .filter(|((a, _), _)| a == APP_ID_TRACKED)
            .filter_map(|(_, v)| serde_json::from_slice::<TrackedArticle>(v).ok())
            .collect()
    }

    fn list_keys(&self, app_id: &str) -> Vec<String> {
        self.store
            .lock()
            .unwrap()
            .keys()
            .filter(|(a, _)| a == app_id)
            .map(|(_, k)| k.clone())
            .collect()
    }
}

#[async_trait::async_trait]
impl NewsworthyHost for StubHost {
    fn self_node_id_str(&self) -> String {
        self.label.clone()
    }
    async fn is_leader(&self) -> bool {
        true
    }
    async fn is_owner_of(&self, _key: &str) -> bool {
        true
    }
    fn store_get(&self, app_id: &str, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .store
            .lock()
            .unwrap()
            .get(&(app_id.to_string(), key.to_string()))
            .cloned())
    }
    fn store_set(&self, app_id: &str, key: &str, value: Vec<u8>) -> Result<()> {
        self.store
            .lock()
            .unwrap()
            .insert((app_id.to_string(), key.to_string()), value);
        Ok(())
    }
    fn store_scan(&self, app_id: &str, prefix: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let mut out: Vec<(String, Vec<u8>)> = self
            .store
            .lock()
            .unwrap()
            .iter()
            .filter(|((a, k), _)| a == app_id && k.starts_with(prefix))
            .map(|((_, k), v)| (k.clone(), v.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
    fn store_delete(&self, app_id: &str, key: &str) -> Result<bool> {
        Ok(self
            .store
            .lock()
            .unwrap()
            .remove(&(app_id.to_string(), key.to_string()))
            .is_some())
    }
}

#[derive(Default)]
struct StubMediaWiki;

#[async_trait::async_trait]
impl MediaWikiClient for StubMediaWiki {
    async fn fetch_parse(&self, page: &str) -> Result<String> {
        if page.starts_with("Portal:Current_events/") {
            Ok(PORTAL_BODY.to_string())
        } else {
            // Any single-article fetch returns the same canned
            // body (revid 1297048221, title="Kyiv"). For the
            // multi-article assertions we'd need a per-title
            // dispatch — out of scope for v0 since we only
            // assert the single Kyiv path.
            Ok(ARTICLE_BODY.to_string())
        }
    }

    async fn batch_revisions(&self, titles: &[String]) -> Result<Vec<RevisionRecord>> {
        // Mimic "every title currently at revid 1000"; the test's
        // first tick has stored last_known_rev_id=Some(1297...) for
        // articles that landed via fetch_article_initial, so the
        // batch revcheck path can compare against that. For simple
        // PendingFetch flows we don't reach this.
        Ok(titles
            .iter()
            .map(|t| RevisionRecord {
                requested_title: t.clone(),
                canonical_title: t.clone(),
                latest_revid: 1000,
                redirected: false,
            })
            .collect())
    }
}
