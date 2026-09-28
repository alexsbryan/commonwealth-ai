use super::*;
use crate::projects::WatcherToggles;

// ── Structural watcher overlay (hot-path merge) ──

fn mem_graph() -> ScipGraphHandle {
    Arc::new(ArcSwap::from_pointee(
        ScipGraph::open_in_memory("overlay-test").unwrap(),
    ))
}

#[tokio::test]
async fn overlay_merge_refreshes_symbol_defs_from_disk() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("lib.rs"),
        "fn hello() {}\nfn world() {\n  let x=1;\n}\n",
    )
    .unwrap();
    let graph = mem_graph();

    let (files, syms) = run_overlay_merge(
        &graph,
        "overlay-test",
        tmp.path(),
        tmp.path(),
        &[PathBuf::from("lib.rs")],
    )
    .await;
    assert_eq!(files, 1);
    assert_eq!(syms, 2);

    // The end-to-end proof: symbols() finds functions that only exist on
    // disk, with NO rust-analyzer, purely via the tree-sitter overlay.
    let g = graph.load();
    assert!(!g
        .find_symbols_by_name("hello", None, 8)
        .await
        .unwrap()
        .is_empty());
    let world = g.find_symbols_by_name("world", None, 8).await.unwrap();
    assert_eq!(world.len(), 1);
    assert_eq!(world[0].file_path, "lib.rs");
}

#[tokio::test]
async fn overlay_merge_skips_non_source_files() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("README.md"), "# not code\nfn nope() {}").unwrap();
    let graph = mem_graph();
    let (files, syms) = run_overlay_merge(
        &graph,
        "overlay-test",
        tmp.path(),
        tmp.path(),
        &[PathBuf::from("README.md"), PathBuf::from("Cargo.toml")],
    )
    .await;
    assert_eq!(
        files, 0,
        "non-source paths belong to the full export, not the overlay"
    );
    assert_eq!(syms, 0);
}

#[tokio::test]
async fn overlay_merge_drops_defs_for_deleted_file() {
    let tmp = tempfile::tempdir().unwrap();
    let graph = mem_graph();
    // Seed a def for gone.rs, then "delete" it (never write the file).
    std::fs::write(tmp.path().join("gone.rs"), "fn ghost() {}").unwrap();
    run_overlay_merge(
        &graph,
        "overlay-test",
        tmp.path(),
        tmp.path(),
        &[PathBuf::from("gone.rs")],
    )
    .await;
    assert!(!graph
        .load()
        .find_symbols_by_name("ghost", None, 8)
        .await
        .unwrap()
        .is_empty());

    std::fs::remove_file(tmp.path().join("gone.rs")).unwrap();
    let (files, syms) = run_overlay_merge(
        &graph,
        "overlay-test",
        tmp.path(),
        tmp.path(),
        &[PathBuf::from("gone.rs")],
    )
    .await;
    assert_eq!(
        files, 1,
        "deleted source file is still processed (to drop its rows)"
    );
    assert_eq!(syms, 0);
    assert!(
        graph
            .load()
            .find_symbols_by_name("ghost", None, 8)
            .await
            .unwrap()
            .is_empty(),
        "deleted file's defs must be gone"
    );
}

fn sample_entry(id: &str, root: PathBuf) -> ProjectEntry {
    ProjectEntry {
        corpus_id: id.into(),
        root,
        registered_at: "2026-04-17T00:00:00Z".into(),
        watchers: WatcherToggles {
            scip_debounce_ms: 30,
            git_poll_secs: 0,
            ..WatcherToggles::default()
        },
    }
}

#[test]
fn reason_string_mapping_is_stable() {
    assert_eq!(RebuildReason::Startup.as_str(), "startup");
    assert_eq!(RebuildReason::FsChange.as_str(), "fs_change");
    assert_eq!(
        RebuildReason::GitHead {
            old: "a".into(),
            new: "b".into()
        }
        .as_str(),
        "git_poll"
    );
    assert_eq!(RebuildReason::Lazy.as_str(), "lazy");
    assert_eq!(RebuildReason::Explicit.as_str(), "explicit");
}

#[test]
fn ignore_filter_excludes_hard_excludes_and_non_source_extensions() {
    let tmp = tempfile::tempdir().unwrap();
    let filter = build_ignore_filter(tmp.path(), &[]);
    assert!(filter.is_ignored(&tmp.path().join("target/debug/foo.rs")));
    assert!(filter.is_ignored(&tmp.path().join("node_modules/x/index.js")));
    assert!(filter.is_ignored(&tmp.path().join("README.md")));
    assert!(filter.is_ignored(&tmp.path().join("docs/.git/HEAD")));
    // .sovereign is NOT in HARD_EXCLUDE — it's a deployment convention,
    // not universal noise. The project registry seeds it as a default
    // ignore_path so it's still filtered for newly-registered projects.
    assert!(!filter.is_ignored(&tmp.path().join(".sovereign/build.rs")));
    assert!(!filter.is_ignored(&tmp.path().join("src/main.rs")));
    assert!(!filter.is_ignored(&tmp.path().join("app/server.ts")));
}

#[test]
fn ignore_filter_honours_extra_ignores() {
    let tmp = tempfile::tempdir().unwrap();
    let extras = vec![".sovereign".to_string(), "my-cache".to_string()];
    let filter = build_ignore_filter(tmp.path(), &extras);
    // Project-local daemon state — SQLite WALs here would slip through
    // any `.gitignore` that wasn't loaded, hence the explicit ignore.
    assert!(filter.is_ignored(&tmp.path().join(".sovereign/notes.db-wal")));
    assert!(filter.is_ignored(&tmp.path().join(".sovereign/build.rs")));
    // A user-configured custom name applies the same way.
    assert!(filter.is_ignored(&tmp.path().join("my-cache/some.rs")));
    // Without the extras, a non-matching project shape isn't penalised.
    assert!(!filter.is_ignored(&tmp.path().join("src/main.rs")));
    let bare = build_ignore_filter(tmp.path(), &[]);
    assert!(!bare.is_ignored(&tmp.path().join("my-cache/some.rs")));
}

#[test]
fn ignore_filter_honours_gitignore() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "secret.rs\n").unwrap();
    let filter = build_ignore_filter(tmp.path(), &[]);
    assert!(filter.is_ignored(&tmp.path().join("secret.rs")));
    assert!(!filter.is_ignored(&tmp.path().join("src/main.rs")));
}

#[test]
fn read_git_head_returns_none_for_non_repo() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(read_git_head(tmp.path()).is_none());
}

/// Initialise a git repo with one commit; return (entry, current_head).
/// Used by the `needs_startup_rebuild` cases below.
fn init_repo_with_commit(corpus_id: &str) -> (tempfile::TempDir, ProjectEntry, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("git invocation");
        assert!(status.success(), "git {:?} failed", args);
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "test"]);
    std::fs::write(root.join("src.rs"), "fn main() {}\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "initial"]);
    let head = read_git_head(root).expect("HEAD after commit");
    let entry = sample_entry(corpus_id, root.to_path_buf());
    (tmp, entry, head)
}

#[tokio::test]
async fn needs_startup_rebuild_true_when_graph_has_no_recorded_head() {
    let (_tmp, entry, _head) = init_repo_with_commit("no-head");
    // Fresh in-memory graph — never had `record_rebuild` called,
    // so `last_indexed_head()` returns None.
    let graph = ScipGraph::open_in_memory(&entry.corpus_id).unwrap();
    assert!(needs_startup_rebuild(&entry, &graph).await);
}

#[tokio::test]
async fn needs_startup_rebuild_true_when_root_is_not_a_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = sample_entry("no-git", tmp.path().to_path_buf());
    let graph = ScipGraph::open_in_memory(&entry.corpus_id).unwrap();
    // Pretend the graph was indexed at some prior HEAD, but the
    // directory isn't a git repo — we can't verify freshness.
    graph
        .record_rebuild("startup", Some("deadbeef"), None)
        .await;
    assert!(needs_startup_rebuild(&entry, &graph).await);
}

#[tokio::test]
async fn needs_startup_rebuild_true_when_head_drifted() {
    let (_tmp, entry, _head) = init_repo_with_commit("drift");
    let graph = ScipGraph::open_in_memory(&entry.corpus_id).unwrap();
    graph
        .record_rebuild(
            "startup",
            Some("0000000000000000000000000000000000000000"),
            None,
        )
        .await;
    assert!(needs_startup_rebuild(&entry, &graph).await);
}

#[tokio::test]
async fn needs_startup_rebuild_true_when_working_tree_dirty() {
    let (tmp, entry, head) = init_repo_with_commit("dirty");
    let graph = ScipGraph::open_in_memory(&entry.corpus_id).unwrap();
    graph.record_rebuild("startup", Some(&head), None).await;
    // Touch a tracked file so `git status --porcelain` is non-empty.
    std::fs::write(tmp.path().join("src.rs"), "fn main() { let _ = (); }\n").unwrap();
    assert!(needs_startup_rebuild(&entry, &graph).await);
}

#[tokio::test]
async fn needs_startup_rebuild_false_when_head_matches_and_tree_clean() {
    let (_tmp, entry, head) = init_repo_with_commit("fresh");
    let graph = ScipGraph::open_in_memory(&entry.corpus_id).unwrap();
    graph.record_rebuild("startup", Some(&head), None).await;
    assert!(
        !needs_startup_rebuild(&entry, &graph).await,
        "graph indexed at current HEAD with clean tree must not trigger a rebuild"
    );
}

#[tokio::test]
async fn nudge_sets_dirty_flag_on_project_state() {
    // Verify via ProjectState directly — ProjectHandle's
    // worker task is hard to isolate in a unit test (it tries
    // to spawn a FS watcher + run exporters), and nudge() is
    // pure state manipulation.
    let state = ProjectState::new("test");
    state.mark_dirty(); // simulates what nudge() does internally
    assert!(state.end_rebuild(), "dirty bit should be observable");
}

#[tokio::test]
async fn register_then_unregister_cleans_up_handle() {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    let merged = Arc::new(ArcSwap::from_pointee(
        ScipGraph::open_in_memory("merged").unwrap(),
    ));
    let reindexer = Reindexer::new(indexes.clone(), merged);

    let entry = sample_entry("probe", tmp.path().to_path_buf());
    let _h = reindexer.register(entry.clone()).await;

    assert!(reindexer.get("probe").await.is_some());

    reindexer.unregister("probe").await;
    assert!(reindexer.get("probe").await.is_none());
}

// ── Rebuild wedge regression (order code-intel-reindexer-fix) ──
//
// The live wedge of 2026-08-14: the follow-up pass re-acquired
// the sole cross-project rebuild permit the first pass still
// held, hanging the task forever — status "active" for hours,
// every later nudge coalescing into a silent no-op. These tests
// pin the fixed invariants with injected fake rebuild bodies
// (real `execute_rebuild` needs rust-analyzer + a cargo
// workspace, which unit tests cannot run).

fn test_rebuild_ctx(
    tmp: &tempfile::TempDir,
    id: &str,
) -> (RebuildCtx, Arc<ProjectState>, ScipGraphHandle) {
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    let entry = sample_entry(id, tmp.path().to_path_buf());
    // `ProjectState::new` already returns an `Arc`.
    let state = ProjectState::new(id);
    let graph = mem_graph();
    let ctx = RebuildCtx {
        entry: entry.clone(),
        state: Arc::clone(&state),
        graph: Arc::clone(&graph),
        merged: mem_graph(),
        merged_primer: None,
        indexes_dir: indexes,
        rebuild_permits: Arc::new(Semaphore::new(1)),
    };
    (ctx, state, graph)
}

fn explicit_req() -> RebuildRequest {
    RebuildRequest {
        reason: RebuildReason::Explicit,
        enqueued_at: Instant::now(),
    }
}

fn body_ok<'a>(
    _c: &'a RebuildCtx,
    _r: &'a RebuildRequest,
) -> BoxFuture<'a, Result<RebuildSummary, String>> {
    Box::pin(async {
        Ok(RebuildSummary {
            symbols: 1,
            refs: 1,
            languages: vec!["rust".into()],
            skipped: vec![],
        })
    })
}

async fn wait_for_in_flight_clear(in_flight: &Arc<AtomicBool>) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while in_flight.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("in_flight must clear — the wedge guard");
}

/// THE wedge regression: the follow-up pass must run under the
/// SAME permit the first pass holds. At HEAD the follow-up
/// re-acquired the sole permit and hung forever (live incident
/// 2026-08-14). The test wraps the call in a timeout because the
/// old code deadlocked instead of returning.
///
/// The incident sequence is reproduced directly: the dirty bit is
/// SET while a rebuild is running (a signal arriving mid-pass),
/// so the loop must run exactly one follow-up pass — observable
/// as two "rebuild complete" lines in the per-watcher log.
#[tokio::test]
async fn followup_pass_runs_under_single_permit() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, state, _graph) = test_rebuild_ctx(&tmp, "wedge");
    // A signal arrives during the first pass.
    state.mark_dirty();
    let done = tokio::time::timeout(
        Duration::from_secs(10),
        run_one_rebuild_with(ctx, explicit_req(), body_ok),
    )
    .await;
    assert!(
        done.is_ok(),
        "follow-up pass self-deadlocked on the permit — the wedge"
    );
    let log = tmp.path().join("logs").join("watch-wedge-scip.log");
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(
        text.matches("rebuild complete").count(),
        2,
        "exactly two passes (initial + follow-up) must have run; log:\n{text}"
    );
    assert!(
        !state.is_rebuild_in_flight(),
        "rebuild claim must be released after the loop"
    );
}

/// A rebuild body that never returns, for the watchdog test.
/// A plain `fn` so the `RebuildBody` fn pointer needs no capture.
fn body_hang<'a>(
    _c: &'a RebuildCtx,
    _r: &'a RebuildRequest,
) -> BoxFuture<'a, Result<RebuildSummary, String>> {
    Box::pin(async {
        futures::future::pending::<()>().await;
        unreachable!()
    })
}

/// A rebuild body that panics, for the panic test. A plain `fn`
/// so the `RebuildBody` fn pointer needs no capture.
fn body_panic<'a>(
    _c: &'a RebuildCtx,
    _r: &'a RebuildRequest,
) -> BoxFuture<'a, Result<RebuildSummary, String>> {
    Box::pin(async { panic!("boom") })
}

/// A panicked rebuild task must clear both slots (worker
/// `in_flight` + the ProjectState claim) and record a visible
/// failure. At HEAD the flags were only cleared by the task's
/// own tail, which a panic skips — the project then wedged
/// forever.
#[tokio::test]
async fn panic_in_rebuild_clears_flags_and_records_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, state, _graph) = test_rebuild_ctx(&tmp, "panic");
    let in_flight = Arc::new(AtomicBool::new(false));
    assert!(spawn_full_rebuild_with(
        &ctx,
        explicit_req(),
        &in_flight,
        &finish_slot(),
        body_panic,
        Duration::from_secs(30),
    ));
    wait_for_in_flight_clear(&in_flight).await;
    assert!(
        !state.is_rebuild_in_flight(),
        "ProjectState claim must clear after a panic"
    );
    assert!(
        state.rebuild_failure_count() >= 1,
        "panic must be recorded as a failure"
    );
    let snap = state.snapshot().await;
    assert!(
        matches!(
            snap.get(&WatcherKind::Scip),
            Some(WatcherStatus::Crashed { .. })
        ),
        "status must surface Crashed, got: {snap:?}"
    );
}

/// The watchdog: a rebuild that never completes within the wall
/// clock is a WEDGE, not a slow export. It must be aborted,
/// recorded as a named failure, and the slots cleared so the
/// next signal retries instead of coalescing forever.
#[tokio::test]
async fn watchdog_aborts_a_hung_rebuild_and_clears_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, state, _graph) = test_rebuild_ctx(&tmp, "hung");
    let in_flight = Arc::new(AtomicBool::new(false));
    assert!(spawn_full_rebuild_with(
        &ctx,
        explicit_req(),
        &in_flight,
        &finish_slot(),
        body_hang,
        Duration::from_millis(300),
    ));
    wait_for_in_flight_clear(&in_flight).await;
    assert!(!state.is_rebuild_in_flight());
    assert!(state.rebuild_failure_count() >= 1);
    let err = state.last_rebuild_error().await;
    assert!(
        err.as_ref().is_some_and(|(e, _)| e.contains("wedged")),
        "watchdog failure must be named, got: {err:?}"
    );
}

/// The per-watcher log file the CLI's `project watch logs <id>
/// scip` reads must actually be written. Before the fix nothing
/// wrote it, so the CLI's promise ("the daemon writes per-watcher
/// logs here once the first cycle runs") was a promise nothing
/// kept.
#[tokio::test]
async fn rebuild_writes_the_per_watcher_log() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, _state, _graph) = test_rebuild_ctx(&tmp, "logged");
    run_one_rebuild_with(ctx, explicit_req(), body_ok).await;
    let log = tmp.path().join("logs").join("watch-logged-scip.log");
    let text = std::fs::read_to_string(&log)
        .unwrap_or_else(|e| panic!("no per-watcher log at {}: {e}", log.display()));
    assert!(text.contains("rebuild start"), "log: {text}");
    assert!(text.contains("rebuild complete"), "log: {text}");
}

/// A signal while a rebuild is in flight coalesces into the
/// dirty bit instead of stacking a second task.
#[tokio::test]
async fn second_spawn_coalesces_into_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, state, _graph) = test_rebuild_ctx(&tmp, "coalesce");
    let in_flight = Arc::new(AtomicBool::new(true)); // a rebuild is running
    assert!(!spawn_full_rebuild_with(
        &ctx,
        explicit_req(),
        &in_flight,
        &finish_slot(),
        body_ok,
        Duration::from_secs(30),
    ));
    assert!(
        state.end_rebuild(),
        "coalesced signal must set the dirty bit"
    );
}

// ─── Cooldown clock (the duty-cycle defect) ──────────────────

fn finish_slot() -> FinishClock {
    Arc::new(std::sync::Mutex::new(None))
}

fn read_finish(clock: &FinishClock) -> Option<Instant> {
    clock.lock().ok().and_then(|s| *s)
}

/// A rebuild body slow enough that a completion stamp is
/// distinguishable from the spawn instant.
fn body_slow<'a>(
    _c: &'a RebuildCtx,
    _r: &'a RebuildRequest,
) -> BoxFuture<'a, Result<RebuildSummary, String>> {
    Box::pin(async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok(RebuildSummary {
            symbols: 1,
            refs: 1,
            languages: vec!["rust".into()],
            skipped: vec![],
        })
    })
}

/// THE duty-cycle defect: the cooldown gating the full
/// rust-analyzer export was stamped when the export was SPAWNED,
/// and the cooldown (300s) was shorter than a measured export on
/// this monorepo (257-498s, watch-commonwealth-ai-scip.log
/// 2026-08-14..16). The gate therefore reopened before the
/// exporter had even finished, so continuous editing pinned
/// rust-analyzer at a ~88-90% duty cycle holding ~14GB — measured
/// live as export starts every ~6min each running ~5.3min. The
/// clock must start when the exporter RELEASES the machine.
#[tokio::test]
async fn cooldown_clock_stamps_at_completion_not_spawn() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, _state, _graph) = test_rebuild_ctx(&tmp, "cooldown");
    let in_flight = Arc::new(AtomicBool::new(false));
    let finished = finish_slot();

    let spawned_at = Instant::now();
    assert!(spawn_full_rebuild_with(
        &ctx,
        explicit_req(),
        &in_flight,
        &finished,
        body_slow,
        Duration::from_secs(30),
    ));
    wait_for_in_flight_clear(&in_flight).await;

    let stamp = read_finish(&finished).expect("completion must stamp the cooldown clock");
    assert!(
        stamp.duration_since(spawned_at) >= Duration::from_millis(150),
        "the cooldown clock must start when the export RELEASES, not when \
             it is spawned — otherwise the gate reopens mid-export"
    );
}

/// The stamp must land on EVERY exit path. A panicked or
/// watchdog-aborted export still consumed the exporter slot and
/// still spiked the machine, so the next one must still wait a
/// full cooldown rather than launching immediately.
#[tokio::test]
async fn cooldown_clock_stamps_even_when_the_rebuild_panics() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, _state, _graph) = test_rebuild_ctx(&tmp, "panic-stamp");
    let in_flight = Arc::new(AtomicBool::new(false));
    let finished = finish_slot();

    assert!(spawn_full_rebuild_with(
        &ctx,
        explicit_req(),
        &in_flight,
        &finished,
        body_panic,
        Duration::from_secs(30),
    ));
    wait_for_in_flight_clear(&in_flight).await;

    assert!(
        read_finish(&finished).is_some(),
        "a panicked export must still stamp the cooldown clock"
    );
}

#[test]
fn full_export_due_when_none_has_ever_run() {
    assert!(full_export_due(None, FULL_REBUILD_COOLDOWN));
}

#[test]
fn full_export_not_due_immediately_after_one_finished() {
    assert!(!full_export_due(
        Some(Instant::now()),
        FULL_REBUILD_COOLDOWN
    ));
}

/// The cooldown must exceed a real export, or the gate reopens
/// before the exporter has released and the duty cycle climbs
/// back toward 100% — the defect this constant was raised to fix.
/// Slowest measured export on this monorepo: 498s.
#[test]
fn cooldown_exceeds_the_slowest_measured_export() {
    assert!(
        FULL_REBUILD_COOLDOWN > Duration::from_secs(498),
        "cooldown {FULL_REBUILD_COOLDOWN:?} must exceed the slowest \
             measured export (498s) or the gate reopens mid-export"
    );
}
