// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-article yield checkpoint's tests (issue #57 rec 4). A child of
//! `newsworthy_watcher` beside `tests`, reusing that module's stub host and
//! no-op MediaWiki client.
use super::tests::{NoopMediaWikiClient, StubHost};
use super::*;

/// Engine whose index dir holds real (empty) indexes for `corpus_ids`,
/// so `tick()` passes the local-install gate and
/// `reindex_by_source_doc_id` has somewhere to write.
async fn make_engine_with_indexes(corpus_ids: &[&str]) -> Arc<CorpusEngine> {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let index_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();
    std::fs::create_dir_all(&index_dir).unwrap();
    for id in corpus_ids {
        let idx = crate::index::CorpusIndex::create(
            &index_dir.join(id),
            id,
            id,
            "test-model",
            4,
            false,
            "MIT",
        )
        .await
        .expect("create index");
        // `installed_indexes` (the tick's local-install gate) skips a
        // partial index; a created-but-unmarked one reads as partial.
        idx.mark_ingestion_complete().expect("mark complete");
    }
    let embed: crate::types::EmbedFn =
        Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.1_f32; 4]) }));
    std::mem::forget(dir);
    Arc::new(CorpusEngine::new(recipes_dir, index_dir, embed))
}

fn pending(title: &str) -> TrackedArticle {
    TrackedArticle {
        title: title.into(),
        lifecycle: Lifecycle::PendingFetch,
        last_known_rev_id: None,
        last_check_at: None,
        first_seen_at: 1,
        last_seen_in_signal_at: 1,
        evict_after_secs: 9_999_999_999,
        redirect_to: None,
    }
}

/// Yield hook that answers "busy" only for the questions numbered in
/// `busy`, so a test can make the foreground wake up between two
/// articles of one tick and go idle again a couple of polls later.
struct FlipHook {
    asked: std::sync::atomic::AtomicUsize,
    busy: std::ops::Range<usize>,
}
impl crate::YieldHook for FlipHook {
    fn should_yield(&self) -> bool {
        let n = self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.busy.contains(&n)
    }
    fn throttle_factor(&self) -> f32 {
        1.0
    }
}

#[tokio::test]
async fn tick_parks_before_each_article_when_the_hook_flips_mid_tick() {
    // Three PendingFetch articles on a follower node. The hook says
    // idle at the tick gate and before article 1, busy before
    // article 2 and for one more poll, idle again after that. The
    // pre-2026-09-02 code asked the hook once per TICK, so this tick
    // would have pushed all three articles through the busy window
    // without parking; now it parks exactly once and still attempts
    // every article (each attempt fails on the no-op MediaWiki stub,
    // which is how the attempts are counted).
    let host: Arc<dyn NewsworthyHost> = Arc::new(StubHost::new("self", false).own_all());
    for title in ["Kyiv", "Lviv", "Odesa"] {
        host.store_set(
            APP_ID_TRACKED,
            &format!("tracked:{title}"),
            serde_json::to_vec(&pending(title)).unwrap(),
        )
        .unwrap();
    }
    let engine = make_engine_with_indexes(&["wikipedia-newsworthy"]).await;
    let hook = Arc::new(FlipHook {
        asked: Default::default(),
        busy: 2..4,
    });
    let as_hook: Arc<dyn crate::YieldHook> = hook.clone();
    engine.set_yield_hook(as_hook);
    let watcher = WikipediaNewsworthyWatcher::new(
        host,
        engine,
        Arc::new(NoopMediaWikiClient),
        NewsworthyConfig {
            yield_poll: Duration::from_millis(10),
            ..NewsworthyConfig::default()
        },
    );
    let report = watcher.tick(Utc::now(), false).await.expect("tick");
    assert_eq!(
        report.errors, 3,
        "every article was still attempted: {report:?}"
    );
    assert_eq!(
        report.yield_deferrals, 1,
        "parked once, before the article that met a busy foreground"
    );
    assert!(
        report.yield_deferred_ms >= 10,
        "parked for at least one poll"
    );
    let asked = hook.asked.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        asked >= 6,
        "tick gate + one ask per article + two polls, got {asked}"
    );
}
