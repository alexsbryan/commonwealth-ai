// SPDX-License-Identifier: AGPL-3.0-or-later
//! Direct-invocation E2E tests for the Phase 3 filesystem watcher.
//!
//! These mirror the MCP-layer tests T-12/T-13/T-14/T-20 from the
//! Code Intelligence v1 e2e spec, but call `CorpusEngine::reindex_file`
//! and `CodeWatcher` directly — no MCP server, no tool registration,
//! no inference provider. They gate the Phase 3 deliverables on the
//! same assertions Phase 5 will ultimately exercise through HTTP.
//!
//! Run with:
//!     cargo test --features treesitter --test watcher_e2e -- --test-threads=1

#![cfg(feature = "treesitter")]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use corpus_engine::engine::reindex::ReindexResult;
use corpus_engine::update::watch::CodeWatcher;
use corpus_engine::{CorpusEngine, CorpusIndex, CorpusSpec, EmbedFn};

// ─── Fixture writer ───────────────────────────────────────────

struct Fixture {
    /// Root directory — the codebase we ask sovereign to watch.
    root: PathBuf,
    /// Location of LanceDB indexes; distinct from `root`.
    data_dir: PathBuf,
    /// Temporary directory guard — released on drop.
    _tmp: tempfile::TempDir,
}

impl Fixture {
    async fn new(test_name: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("repo");
        let data_dir = tmp.path().join("indexes");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("web")).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();

        // executor.rs — will be backdated to 30 days ago in a separate step
        std::fs::write(
            root.join("src/executor.rs"),
            r#"/// Executes a planned step.
pub async fn execute_step() -> Result<(), ExecutorError> {
    Ok(())
}

pub fn validate_preconditions() -> Result<(), ExecutorError> {
    Ok(())
}

#[derive(Debug)]
pub enum ExecutorError { NoNodes, ValidationFailed }
"#,
        )
        .unwrap();

        // scheduler.rs
        std::fs::write(
            root.join("src/scheduler.rs"),
            r#"pub async fn apply_shard_plan(plan: ShardPlan) { let _ = plan; }
pub fn validate_plan(plan: &ShardPlan) { let _ = plan; }
pub struct ShardPlan { pub shards: Vec<Shard> }
pub struct Shard { pub node_id: String }
"#,
        )
        .unwrap();

        // planner.rs — current mtime, used in watcher tests
        std::fs::write(
            root.join("src/planner.rs"),
            r#"pub fn plan_next_step(context: &ConversationContext) -> StepPlan {
    StepPlan { kind: 0 }
}
pub struct StepPlan { pub kind: i32 }
pub struct ConversationContext { pub messages: Vec<String> }
"#,
        )
        .unwrap();

        // types.rs
        std::fs::write(
            root.join("src/types.rs"),
            r#"pub struct MeshState { pub nodes: Vec<Node> }
pub struct Node { pub id: String }
"#,
        )
        .unwrap();

        // web/store.ts — used in deletion test (T-13)
        std::fs::write(
            root.join("web/store.ts"),
            r#"const createMeshStore = () => {
    let nodes: string[] = [];
    return { nodes };
};
"#,
        )
        .unwrap();

        eprintln!("[{test_name}] fixture root: {}", root.display());

        Self {
            root,
            data_dir,
            _tmp: tmp,
        }
    }

    fn corpus_id(&self) -> &str {
        "test-code"
    }

    /// Build an engine pointing at `data_dir` with a zero-vector embed fn.
    fn engine(&self) -> Arc<CorpusEngine> {
        let embed: EmbedFn = Arc::new(|_text: &str| {
            Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 768]) })
        });
        // `recipes_dir` is irrelevant for this test — the ingest path
        // uses a recipe we synthesize directly via a TOML file.
        // `with_embedding_model` is a hard precondition of `ingest()`
        // (see `engine/ingest.rs` for the rationale).
        Arc::new(
            CorpusEngine::new(self.data_dir.join("_recipes"), self.data_dir.clone(), embed)
                .with_embedding_model("test-mock"),
        )
    }

    /// Synthesize a code recipe pointing at `root` and run ingest.
    async fn initial_index(&self) {
        let recipe_toml = format!(
            r#"[corpus]
id = "{cid}"
name = "{cid}"
description = "fixture"
license = "private"
mesh_sharing = false
size_compressed_gb = 0
size_indexed_gb = 0

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "code"
context_lines = 3
max_lines_per_chunk = 150

[chunk]
type = "passthrough"

[index]
fts = true
vector = false
"#,
            cid = self.corpus_id(),
            path = self.root.display()
        );

        let recipes_dir = self.data_dir.join("_recipes");
        std::fs::create_dir_all(&recipes_dir).unwrap();
        let recipe_path = recipes_dir.join(format!("{}.toml", self.corpus_id()));
        std::fs::write(&recipe_path, recipe_toml).unwrap();

        let engine = self.engine();
        let spec = CorpusSpec::RecipePath(recipe_path);
        engine.ingest(&spec, None).await.expect("initial ingest");
    }

    /// Open the corpus for in-test assertions.
    async fn open(&self) -> CorpusIndex {
        let path = self.data_dir.join(self.corpus_id());
        CorpusIndex::open(&path).await.expect("open corpus")
    }
}

// ─── Assertion helpers ────────────────────────────────────────

async fn has_symbol(index: &CorpusIndex, name: &str) -> bool {
    use futures::TryStreamExt;
    use lancedb::query::{ExecutableQuery, QueryBase};

    let safe = name.replace('\'', "''");
    let filter = format!("symbol_name = '{safe}'");
    let batches: Vec<_> = index
        .table()
        .query()
        .only_if(filter)
        .limit(1)
        .execute()
        .await
        .expect("query")
        .try_collect()
        .await
        .expect("collect");
    batches.iter().any(|b| b.num_rows() > 0)
}

// ─── T-12 — new symbol findable after file save ───────────────

#[tokio::test]
async fn t12_new_symbol_findable_after_save() {
    let fx = Fixture::new("t12").await;
    fx.initial_index().await;

    let engine = fx.engine();
    let watcher = CodeWatcher::new(
        Arc::clone(&engine),
        fx.corpus_id().to_string(),
        fx.root.clone(),
    )
    .with_debounce(Duration::from_millis(300));
    let _handle = watcher.start().await.expect("start watcher");

    // Verify the symbol does not exist yet.
    let idx = fx.open().await;
    assert!(!has_symbol(&idx, "orchestrate_recovery").await);
    drop(idx);

    // Append a new function. Use a unique marker so no earlier test
    // state can mask the failure.
    let planner = fx.root.join("src/planner.rs");
    let mut content = std::fs::read_to_string(&planner).unwrap();
    content.push_str("\npub fn orchestrate_recovery(node_id: &str) {\n    let _ = node_id;\n}\n");
    std::fs::write(&planner, content).unwrap();

    // Give the watcher time to debounce (300ms) + reindex + write.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let idx = fx.open().await;
    assert!(
        has_symbol(&idx, "orchestrate_recovery").await,
        "orchestrate_recovery missing after file save"
    );
}

// ─── T-13 — deleted file removes its symbols ──────────────────

#[tokio::test]
async fn t13_deleted_file_removes_symbols() {
    let fx = Fixture::new("t13").await;
    fx.initial_index().await;

    let idx = fx.open().await;
    assert!(
        has_symbol(&idx, "createMeshStore").await,
        "createMeshStore must exist before deletion"
    );
    drop(idx);

    let engine = fx.engine();
    let watcher = CodeWatcher::new(
        Arc::clone(&engine),
        fx.corpus_id().to_string(),
        fx.root.clone(),
    )
    .with_debounce(Duration::from_millis(300));
    let _handle = watcher.start().await.expect("start watcher");

    std::fs::remove_file(fx.root.join("web/store.ts")).unwrap();

    tokio::time::sleep(Duration::from_secs(3)).await;

    let idx = fx.open().await;
    assert!(
        !has_symbol(&idx, "createMeshStore").await,
        "Deleted symbol still present"
    );
}

// ─── T-14 — rapid saves produce a single coherent re-index ────

#[tokio::test]
async fn t14_rapid_saves_debounced() {
    let fx = Fixture::new("t14").await;
    fx.initial_index().await;

    let engine = fx.engine();
    let watcher = CodeWatcher::new(
        Arc::clone(&engine),
        fx.corpus_id().to_string(),
        fx.root.clone(),
    )
    .with_debounce(Duration::from_millis(300));
    let _handle = watcher.start().await.expect("start watcher");

    let planner = fx.root.join("src/planner.rs");
    let original = std::fs::read_to_string(&planner).unwrap();

    // 10 saves within ~200ms — well inside the 300ms debounce window.
    for i in 0..10u32 {
        let content = format!("{original}\n// rapid save {i}");
        std::fs::write(&planner, content).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Wait for debounce to fire and reindex to complete.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // plan_next_step must still be findable — the final state has it.
    let idx = fx.open().await;
    assert!(
        has_symbol(&idx, "plan_next_step").await,
        "plan_next_step missing after rapid saves"
    );

    // And it should appear exactly once per file, not 10× from 10 reindexes.
    // Count rows in the LanceDB table whose symbol_name = 'plan_next_step'.
    use futures::TryStreamExt;
    use lancedb::query::{ExecutableQuery, QueryBase};
    let batches: Vec<_> = idx
        .table()
        .query()
        .only_if("symbol_name = 'plan_next_step'")
        .execute()
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let count: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert!(
        count == 1,
        "plan_next_step appeared {count} times after rapid saves — debounce failed to collapse duplicate reindexes"
    );
}

// ─── reindex_file direct path (feeds into T-20 watcher SLA) ───

#[tokio::test]
async fn reindex_file_updates_and_deletes() {
    let fx = Fixture::new("reindex").await;
    fx.initial_index().await;

    let engine = fx.engine();

    // Append a new symbol and call reindex_file directly — no watcher.
    let planner = fx.root.join("src/planner.rs");
    let mut content = std::fs::read_to_string(&planner).unwrap();
    content.push_str("\npub fn probe_direct_reindex() {}\n");
    std::fs::write(&planner, content).unwrap();

    let result = engine
        .reindex_file(fx.corpus_id(), &planner, &fx.root)
        .await
        .expect("reindex_file Updated");
    assert!(matches!(result, ReindexResult::Updated { .. }));

    let idx = fx.open().await;
    assert!(has_symbol(&idx, "probe_direct_reindex").await);
    drop(idx);

    // Delete the file and reindex — should yield ReindexResult::Deleted.
    std::fs::remove_file(&planner).unwrap();
    let result = engine
        .reindex_file(fx.corpus_id(), &planner, &fx.root)
        .await
        .expect("reindex_file Deleted");
    assert!(matches!(result, ReindexResult::Deleted { .. }));

    let idx = fx.open().await;
    assert!(!has_symbol(&idx, "plan_next_step").await);
    assert!(!has_symbol(&idx, "probe_direct_reindex").await);
}

/// Regression: a file whose symbols sit on ADJACENT lines must not lose
/// symbols when reindexed.
///
/// The extractor pads each symbol with `context_lines`, so tightly-packed
/// functions produce several chunks with byte-identical content — and
/// therefore one shared content hash. `reindex_file` used to map its added
/// chunks back to extractor output through a `HashMap<content_hash, _>`,
/// which kept only the last of those chunks; every added chunk then resolved
/// to that same entry. The count still matched, so the "could not resolve"
/// fallback never fired, and the file's rows were replaced by N copies of its
/// LAST symbol — the others silently vanished from the index.
///
/// Observed in the wild via `svrn code watch`: appending one function to a
/// 3-function file left four identical rows and destroyed the other three
/// symbols. The mapping is positional now, which cannot collapse.
#[tokio::test]
async fn reindex_preserves_every_symbol_when_chunks_share_content() {
    let fx = Fixture::new("adjacent").await;
    fx.initial_index().await;

    // Adjacent definitions — no blank lines. With context padding every
    // chunk here expands to the same text.
    let packed = fx.root.join("src/packed.rs");
    std::fs::write(
        &packed,
        "pub fn packed_one() {}\npub fn packed_two() {}\npub fn packed_three() {}\n",
    )
    .unwrap();
    let engine = fx.engine();
    engine
        .reindex_file(fx.corpus_id(), &packed, &fx.root)
        .await
        .expect("initial reindex of packed.rs");

    let idx = fx.open().await;
    for s in ["packed_one", "packed_two", "packed_three"] {
        assert!(has_symbol(&idx, s).await, "{s} missing after first index");
    }
    drop(idx);

    // Append a fourth. Every prior symbol must survive.
    std::fs::write(
        &packed,
        "pub fn packed_one() {}\npub fn packed_two() {}\npub fn packed_three() {}\n\
         pub fn packed_four() {}\n",
    )
    .unwrap();
    engine
        .reindex_file(fx.corpus_id(), &packed, &fx.root)
        .await
        .expect("reindex after append");

    let idx = fx.open().await;
    for s in [
        "packed_one",
        "packed_two",
        "packed_three",
        "packed_four",
    ] {
        assert!(
            has_symbol(&idx, s).await,
            "{s} was lost by reindex_file — identical-content chunks collapsed again"
        );
    }
    assert_eq!(
        symbol_row_count(&idx, "packed_four").await,
        1,
        "packed_four duplicated — added chunks resolved to the same extractor entry"
    );
}

/// How many rows carry `name` as their `symbol_name`.
async fn symbol_row_count(index: &CorpusIndex, name: &str) -> usize {
    use futures::TryStreamExt;
    use lancedb::query::{ExecutableQuery, QueryBase};

    let safe = name.replace('\'', "''");
    let batches: Vec<_> = index
        .table()
        .query()
        .only_if(format!("symbol_name = '{safe}'"))
        .execute()
        .await
        .expect("query")
        .try_collect()
        .await
        .expect("collect");
    batches.iter().map(|b| b.num_rows()).sum()
}

// ─── source_path round-trip (CLI watch relies on this) ────────

#[tokio::test]
async fn source_path_round_trip() {
    let fx = Fixture::new("source_path").await;
    fx.initial_index().await;

    let idx = fx.open().await;
    let recorded = idx
        .source_path()
        .expect("source_path should be set for code corpora");
    assert_eq!(
        recorded.canonicalize().unwrap(),
        fx.root.canonicalize().unwrap(),
        "source_path round trip mismatch"
    );
}
