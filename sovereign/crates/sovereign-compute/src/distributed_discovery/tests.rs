// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

#[derive(Default)]
struct Engine {
    reloads: AtomicUsize,
}

#[async_trait::async_trait]
impl PrimaryReload for Engine {
    async fn reload_primary(&self) -> Result<(), String> {
        self.reloads.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// pb-serving-proofs (b): the discovery loop reads the engine through the
/// process's reload cell. A reload swaps the cell, and the next worker-set
/// change redistributes the NEW engine, never the one the loop was spawned
/// with. Failing input: an `engine_source` that reads the cell once, at
/// construction, and the second change reloads the first engine.
#[tokio::test]
async fn a_worker_set_change_after_a_reload_redistributes_the_new_engine() {
    let before = Arc::new(Engine::default());
    let cell: EngineCell = Arc::new(std::sync::RwLock::new(Some(
        Arc::clone(&before) as Arc<dyn PrimaryReload>
    )));
    let source = engine_source(Arc::clone(&cell));
    let mut last_loaded = Vec::new();

    reload_in_process(&source, &["10.0.0.2:50052".to_string()], &mut last_loaded).await;
    assert_eq!(before.reloads.load(Ordering::SeqCst), 1);

    // The reload: the cell now holds the engine the assembly rebuilt.
    let after = Arc::new(Engine::default());
    *cell.write().unwrap() = Some(Arc::clone(&after) as Arc<dyn PrimaryReload>);

    let workers = vec!["10.0.0.2:50052".to_string(), "10.0.0.3:50052".to_string()];
    reload_in_process(&source, &workers, &mut last_loaded).await;
    assert_eq!(
        after.reloads.load(Ordering::SeqCst),
        1,
        "the new engine redistributes"
    );
    assert_eq!(
        before.reloads.load(Ordering::SeqCst),
        1,
        "the old engine is not touched"
    );
    assert_eq!(last_loaded, workers);
}

/// No engine (the provider build failed): the snapshot stays fresh, so a
/// later manual load picks the workers up.
#[tokio::test]
async fn no_engine_keeps_the_worker_snapshot_fresh() {
    let source = engine_source(Arc::new(std::sync::RwLock::new(None)));
    let mut last_loaded = Vec::new();
    reload_in_process(&source, &["10.0.0.2:50052".to_string()], &mut last_loaded).await;
    assert_eq!(last_loaded, vec!["10.0.0.2:50052".to_string()]);
}
