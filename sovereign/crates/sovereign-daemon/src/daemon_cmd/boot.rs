// SPDX-License-Identifier: AGPL-3.0-or-later
//! Split from daemon_cmd/mod.rs for the §3.2 size ceiling (behaviour-preserving move).

use std::sync::Arc;

use corpus_engine::CorpusEngine;
use corpus_engine_notes::NoteStore;
use corpus_engine_watchers::{LintResultStore, TestResultStore};
use sovereign_contracts::launch::Launch;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::embedded::EmbeddedLlamaCpp;

use super::log_rotation;
use super::memory_watch;
use super::mesh_resume::resume_or_bootstrap_mesh;
use super::rlimit;
use super::shutdown_daemon;
use super::sovereign_root;

use crate::bootstrap;
use crate::solve_http;
use crate::tool_registry;
use crate::tool_registry::build_tool_registry;
use crate::worker::run_worker_daemon;
use crate::workspace::resolve_workspace_dir;

pub(super) async fn run_daemon(launch: &Launch, args: &[String]) -> i32 {
    #[cfg(unix)]
    rlimit::raise_open_file_limit();
    // ── Worker-mode branch (ephemeral pod) ────────────────────────
    //
    // `svrn daemon run --worker-mode` runs an ephemeral worker
    // daemon (see `sovereign/docs/EPHEMERAL_WORKER_PODS.md`) instead
    // of a full persistent peer. The worker boots with a bootstrap
    // blob (env `SOVEREIGN_BOOTSTRAP` or `--bootstrap-blob <file>`),
    // serves the four owner-only routes on `:9742` over a
    // seed-derived self-signed TLS cert, and exits when the owner
    // sends `DELETE /internal/worker/job` (or process is signalled).
    //
    // Worker mode skips every persistent-peer surface: no SetupConfig
    // (no inference models), no mesh state machine, no
    // /v1/chat/completions exposure. The binary is the same, but the
    // wiring branches here and stays in worker_daemon.rs from this
    // point forward.
    //
    // THE LAUNCH ANSWERS THIS, not a second argv scan. Until 2026-08-25 this
    // line read `args.iter().any(|a| a == "--worker-mode")` — the last
    // surviving launch-mode READER outside `Launch::parse`, and a §10.6
    // duplicate created by the refactor that introduced `Launch`: `dispatch`
    // collapsed `Daemon` and `Worker` into one `daemon_cmd::run` call, so
    // `Launch` answered and this function asked again. Threading the `Launch`
    // itself (rather than re-deriving from `args`) is what makes the two
    // agree by construction — and it sidesteps the arg-shape mismatch that
    // deferred this fix, since `Launch::Worker` carries argv INCLUDING the
    // `run` subcommand while `run_worker_daemon` wants it stripped.
    // Falsifier 1, readers: 1 -> 0.
    if matches!(launch, Launch::Worker { .. }) {
        return run_worker_daemon(args).await;
    }

    // ── Flag parsing ──────────────────────────────────────────────
    //
    // The first-boot wizard (`--setup-only`) did not move with the run
    // path: it lives in the CLI tree's `setup_cmd` (~5.4k lines, svrn
    // package surface), which this crate may not depend on. A named
    // refusal, not a silent one — see the seam named in the module doc.
    if args.iter().any(|a| a == "--setup-only") {
        eprintln!(
            "error: --setup-only moved with the first-boot wizard — \
             run `svrn setup` instead"
        );
        return 1;
    }

    // `--config <path>` overrides the default `~/.svrnmesh/config.toml`
    // path. Phase 2 of EPHEMERAL_WORKER_PODS uses this to point the
    // child daemon spawned by `SubprocessRunner` at the auto-generated
    // pod-side config (written by `worker_http::write_child_daemon_config`).
    // Production launchd/systemd units don't pass `--config`; they
    // continue to use the canonical path. The existence check below
    // is skipped when `--config` is set — that's intentional: if the
    // operator passes `--config` they're telling us they have a
    // config, so we surface a clean error if the file is missing.
    let config_override: Option<std::path::PathBuf> = {
        let mut path: Option<std::path::PathBuf> = None;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if a == "--config" {
                if let Some(p) = it.next() {
                    path = Some(std::path::PathBuf::from(p));
                }
            }
        }
        path
    };

    // ── Config existence ──────────────────────────────────────────
    //
    // The CLI tree's `daemon run` used to inline the interactive setup
    // wizard here on first boot (TTY permitting). The wizard is
    // `setup_cmd`, which did not move with the run path — so a missing
    // config is a clean, named error pointing at `svrn setup`, the
    // same hint a launchd-spawned daemon always got (no TTY there
    // either).
    // When `--config <path>` is passed, the operator owns the config
    // file's existence — skip this check entirely and surface a clean
    // error below if the file is missing.
    if config_override.is_none() && !sovereign_core::setup_config::SetupConfig::exists() {
        eprintln!(
            "error: no config at {}",
            SetupConfig::default_path().display()
        );
        eprintln!("hint: run `svrn setup` to create one.");
        return 1;
    }

    // ── Log rotation ──────────────────────────────────────────────
    //
    // launchd holds the FDs on `daemon.log` / `daemon.err` (set via
    // the plist's StandardOutPath / StandardErrorPath) and never
    // re-opens them, so rename-style rotation would leak the inode.
    // Instead we copy-truncate at startup (cheap if under cap, safe
    // for in-flight launchd writes — the FD continues into the
    // now-empty file) and again on a 30-minute timer for long-running
    // daemon processes. See `log_rotation` for the contract.
    //
    // Ordered FIRST so a daemon that's been running for days and
    // produced a 5-GB log doesn't make the operator's `tail -f` drop
    // dead before the new daemon prints its first useful line.
    let log_dir = sovereign_root().join("logs");
    log_rotation::rotate_daemon_logs(
        &log_dir,
        log_rotation::DEFAULT_SIZE_CAP_BYTES,
        log_rotation::DEFAULT_KEEP_N_BAKS,
    );
    // 30-minute periodic rotation so a daemon that runs continuously
    // for days stays bounded between launchd restarts. The interval is
    // a knob — shorter cadence catches bursts faster but adds I/O
    // wakeups; 30 min is comfortably long for a stat() + size check.
    // Supervised: a panic must not silently stop rotation for the rest
    // of the process's life (DAEMON_RESILIENCE.md P0.4).
    let _rotation_handle = crate::supervise::spawn_supervised("log_rotation", {
        let log_dir = log_dir.clone();
        move || {
            log_rotation::rotation_loop(
                log_dir.clone(),
                log_rotation::DEFAULT_SIZE_CAP_BYTES,
                log_rotation::DEFAULT_KEEP_N_BAKS,
                std::time::Duration::from_secs(30 * 60),
            )
        }
    });

    // ── Memory watch ──────────────────────────────────────────────
    // 60s RSS sampler: publishes the latest sample and warns above the
    // soft limit. The hard limit (self-SIGTERM with a non-zero exit so a
    // service manager relaunches a clean process before jetsam SIGKILLs
    // mid-write) is OFF by default — it only helps under a supervisor, so
    // it must be opted into via `SOVEREIGN_RSS_HARD_LIMIT_MB=<mb>|auto`
    // (`scripts/daemon-supervised.sh` sets it). See `memory_watch`.
    // Supervised: a panicked sampler used to silently disarm the OOM
    // defense (DAEMON_RESILIENCE.md P0.4).
    let _memory_watch_handle = crate::supervise::spawn_supervised("memory_watch", || {
        memory_watch::watch_loop(std::time::Duration::from_secs(60))
    });

    // ── Load config ───────────────────────────────────────────────
    let config = match config_override.as_ref() {
        Some(path) => match SetupConfig::load_from(path) {
            Ok(c) => {
                eprintln!("[daemon] loaded config from {}", path.display());
                c
            }
            Err(e) => {
                eprintln!("error: --config {}: {e}", path.display());
                return 1;
            }
        },
        None => match SetupConfig::load() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: {e}");
                eprintln!("hint: run `svrn setup` to (re-)create the config.");
                return 1;
            }
        },
    };

    // ── Is our data root the one that holds this machine's data? ──
    //
    // Four directories have been a data root across releases, and
    // `data_dir()` always returns a plausible one — which is what made a
    // wrong answer invisible (note `b2aa9fb8`). Classify before claiming:
    // starting fresh on top of live data somewhere else is a silent
    // substitution, and the daemon is the surface where it is worst (a
    // service that boots into an empty universe and reports healthy).
    match sovereign_contracts::data_roots::classify(&config.data.dir) {
        v if v.is_refusal() => {
            eprintln!("error: data root {}: {v}", config.data.dir.display());
            return 1;
        }
        sovereign_contracts::data_roots::RootConflict::Clear => {}
        v => tracing::warn!(
            target: "daemon",
            root = %config.data.dir.display(),
            "data roots: {v}"
        ),
    }

    // ── Single-instance guard (DAEMON_RESILIENCE.md P0.5) ─────────
    //
    // Taken as early as the thing it protects is KNOWN — which is here,
    // right after the config parse, not before it. The lock is keyed on the
    // DATA ROOT (`RunLock`), and until 2026-08-24 it was keyed on `$HOME`
    // and therefore had to be taken before the config was read; that key
    // refused three soak nodes with three data dirs under one HOME and
    // admitted two processes onto one data dir from two HOMEs. A TOML parse
    // is the only thing that now happens first, and nothing heavy — no
    // model, no listener, no store — has been touched.
    //
    // Held for the process lifetime: the kernel releases it on any exit,
    // including SIGKILL, so there is no stale-lock cleanup path.
    let _run_lock = match sovereign_contracts::run_lock::RunLock::acquire(&config.data.dir) {
        Ok(lock) => {
            tracing::debug!(
                target: "daemon",
                lock = %lock.path().display(),
                enforced = lock.is_enforced(),
                "run lock: claimed the data root"
            );
            lock
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    // Shared-model cluster role → RPC env contract. The desktop fleet
    // sets `[shared_model] role` instead of SOVEREIGN_RPC_* by hand;
    // translate it here, once, before any RPC consumer reads the env
    // (the inference serve call_once, the discovery loop below, and
    // commonwealth-api's /status advertise). An explicit env var wins.
    // `--rpc-worker` first: it is the operator saying it out loud on this
    // invocation, and the role translation below only fills in what is unset.
    bootstrap::apply_rpc_worker_flag(args);
    bootstrap::apply_shared_model_role_to_env(&config.shared_model);

    // Route llama.cpp's internal log into our tracing layer. Without
    // this, gguf load failures and ggml backend diagnostics print to a
    // dropped stderr (the daemon's child-style stdio capture swallows
    // them) — the operator gets a bare "null result from llama cpp"
    // with no actionable detail. Installed exactly once per process.
    sovereign_inference::llama::install_log_tracing();

    // VRAM capacity preflight — ADVISORY by default: warns and starts
    // anyway on overcommit (so CPU-only / low-VRAM machines aren't
    // hard-blocked). Only refuses under SOVEREIGN_STRICT_VRAM_CHECK=1 or
    // when a model file is unreadable. Full rationale on
    // `build::preflight::check_vram`.
    // Name the config the operator actually passed, not the default one —
    // a `--config` start used to be told to edit a file it never read.
    let config_path_in_use = config_override
        .clone()
        .unwrap_or_else(sovereign_core::setup_config::SetupConfig::default_path);
    if !crate::build::preflight::check_vram_reporting(&config, &config_path_in_use) {
        return 1;
    }

    // ── Force-tool-calls config → process env ─────────────────────
    //
    // The inference adapter reads `SOVEREIGN_FORCE_TOOL_CALLS` per
    // request to decide whether to upgrade `tool_choice="auto"` to
    // `"required"` (which engages the JSON-Schema tool-envelope
    // grammar). When the operator sets `[daemon] force_tool_calls =
    // true` in setup_config.toml, we propagate that into the process
    // env at boot so the existing per-request lookup picks it up.
    // Caller-supplied env wins — `std::env::set_var` only overrides
    // when nothing was set on the CLI invocation. Operators who want
    // a one-shot test (`SOVEREIGN_FORCE_TOOL_CALLS=0 svrn daemon
    // run`) can still do so without editing the config file.
    if config.daemon.force_tool_calls && std::env::var("SOVEREIGN_FORCE_TOOL_CALLS").is_err() {
        std::env::set_var("SOVEREIGN_FORCE_TOOL_CALLS", "1");
        tracing::info!(
            "daemon: force_tool_calls=true — grammar engaged on every \
             tools-using request (set via setup_config.toml)"
        );
    }

    // ── Alternation-grammar config → process env ──────────────────
    //
    // Same propagation pattern as force_tool_calls. The inference
    // adapter reads `SOVEREIGN_ALTERNATION_GRAMMAR` per request to
    // route tool-envelope requests through llguidance's canonical
    // `TopLevelGrammar::from_json_schema` path instead of the
    // in-house `JsonConstraint` mask. Caller-supplied env wins so
    // operators can A/B test (`SOVEREIGN_ALTERNATION_GRAMMAR=0
    // svrn daemon run` ignores the config).
    //
    // launchd-spawned daemons don't inherit caller env, so flipping
    // this in setup_config.toml is the load-bearing path on macOS
    // hosts running the daemon via `svrn daemon start`.
    if config.daemon.alternation_grammar && std::env::var("SOVEREIGN_ALTERNATION_GRAMMAR").is_err()
    {
        std::env::set_var("SOVEREIGN_ALTERNATION_GRAMMAR", "1");
        tracing::info!(
            "daemon: alternation_grammar=true — llguidance schema path \
             engaged on tools-using requests (set via setup_config.toml)"
        );
    }

    // Inference provider — load the embedded llama.cpp provider (3 GGUF
    // slots + extras/idle/rerank wiring); full rationale on
    // `crate::build::inference::load_provider`. `engine_handle` (concrete) feeds
    // the RPC-worker auto-reload path; `resolved_embed_family` feeds the
    // mesh embed-model advertisement.
    //
    // Minted HERE, before the provider, because a terminal's provider binds to
    // its entry node THROUGH this handle: the bind is a mesh identity, resolved
    // per turn, and the mesh view does not exist yet. `DeferredDaemon` answers
    // exactly as a commissioned-but-stopped daemon until `bind` — no peers — so
    // a terminal booting ahead of gossip reports its entry node unreachable
    // rather than inventing an address for it.
    let deferred_daemon = Arc::new(crate::DeferredDaemon::new());
    let (provider, raw_engine, resolved_embed_family, distributed_primary_slot) =
        match crate::build::inference::load_provider(&config, Arc::clone(&deferred_daemon)) {
            Ok(t) => t,
            Err(()) => return 1,
        };
    // `None` whenever nothing in this process owns llama slots — TWO ways in
    // now, and the engine-only paths (RPC-worker auto-reload, slot hot-swap)
    // must see the absence rather than a stub either way:
    //   - a `terminal`, which holds no weights at all and forwards instead;
    //   - an engine configured with no local llama slots, where the
    //     RPC-worker reload below is llama's own and simply does not arm.
    // Already an `Option` before either existed; both make the `None` reachable.
    let engine_handle: Option<Arc<EmbeddedLlamaCpp>> = raw_engine;

    // ── Note store (for MCP notes tools + ring-buffer logging) ────
    let data_dir = config.data.dir.clone();
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!("error: cannot create data dir {}: {e}", data_dir.display());
        return 1;
    }
    // ── The state store — `sovereign.db` (daemon-convergence Phase 3) ────
    //
    // Until now `sovereign daemon run` opened this file NOWHERE, while still
    // mounting `reading_http`: every `conversation-history` chunk it served
    // came back with `title: null`, because the handler resolved the title
    // through a `state_store()` that answered `None` on this variant. That was
    // the single crossing in an otherwise nesting variant lattice — Desktop
    // carried a store, Headless did not, and neither was a superset of the
    // other (`quality/TOPOLOGY.md` §3.5, class D).
    //
    // It is opened HERE, beside `notes.db`, and a failure is fatal rather than
    // degraded: the store is `ServingCore` now, and CORE means the process
    // cannot serve at all without it. Falling back to `InMemoryStateStore`
    // would reproduce exactly the defect being closed — a daemon that answers
    // every conversation lookup with a well-formed nothing (ARCH §18.3).
    //
    // Safe to open unconditionally because of Phase 1: `RunLock` above is
    // keyed on THIS data root, so at most one process is writing this file.
    let state_db_path = data_dir.join("sovereign.db");
    // The CONCRETE handle is kept as well as the trait object: the same
    // `SqliteStateStore` is also the `ConvTieredReader` the turn's enrichment
    // lane reads briefings through (spec CONV_TIERED_PORT.md), and that view
    // is not reachable from `dyn StateStore`. One open, two views — never two
    // opens (TOPOLOGY phase 1: one writer per data root).
    let state_store_concrete = match sovereign_store::sqlite::SqliteStateStore::open(&state_db_path)
    {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!(
                "error: cannot open state db {}: {e}",
                state_db_path.display()
            );
            return 1;
        }
    };
    let state_store: Arc<dyn sovereign_core::traits::StateStore> = state_store_concrete.clone();

    let notes_path = data_dir.join("notes.db");
    let notes_store = match NoteStore::open(&notes_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("error: cannot open notes db {}: {e}", notes_path.display());
            return 1;
        }
    };
    // NoteStore is built early (other subsystems take it as
    // `Arc<NoteStore>`), but `embed_fn`, `origin_node_id`, and
    // `propagation_sink` aren't known yet. They wire post-Arc
    // via the OnceLock setters at three later seams in this
    // function — search this file for `set_origin_node_id`,
    // `set_embed_fn`, and `set_propagation_sink` to find the
    // wiring sites.

    // ── Lint / test result stores ─────────────────────────────────
    // Always opened so the agent-facing `lint_status` / `test_status`
    // tools have a backing store to read from. When no watcher is
    // configured (no workspace resolved, or sovereign.toml has no
    // [lint_runner]/[test_runner]), the tools report `never_run` —
    // accurate and unambiguous.
    let lint_store: Arc<LintResultStore> =
        match LintResultStore::open(&data_dir.join("lint_results.db")) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                eprintln!(
                    "error: cannot open lint results db {}: {e}",
                    data_dir.join("lint_results.db").display()
                );
                return 1;
            }
        };
    let test_store: Arc<TestResultStore> =
        match TestResultStore::open(&data_dir.join("test_results.db")) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                eprintln!(
                    "error: cannot open test results db {}: {e}",
                    data_dir.join("test_results.db").display()
                );
                return 1;
            }
        };

    // Wipe orphan rows left by a previous daemon process that was
    // SIGKILLed mid-run. Without this, `lint_status` / `test_status`
    // can return `running` indefinitely against a row whose owning
    // process is long dead. Best-effort — cleanup failure shouldn't
    // block daemon startup.
    if let Ok(n) = lint_store.clear_orphan_runs().await {
        if n > 0 {
            tracing::info!(
                purged = n,
                "lint_results: cleared orphan rows from prior daemon process"
            );
        }
    }
    if let Ok(n) = test_store.clear_orphan_runs().await {
        if n > 0 {
            tracing::info!(
                purged = n,
                "test_results: cleared orphan rows from prior daemon process"
            );
        }
    }

    // ── Workspace-driven watchers (optional) ──────────────────────
    // The daemon has no inherent project. When the user wants the
    // background lint/test watcher running, they point us at a
    // workspace via either:
    //   1. SOVEREIGN_WORKSPACE_DIR env var (preferred for launchd —
    //      set in the plist's EnvironmentVariables block), or
    //   2. ~/.svrnmesh/workspace — a single-line text file with
    //      the workspace path (handy for users who can't easily
    //      edit launchd plists).
    //
    // Inside that workspace, `.sovereign/sovereign.toml` declares
    // `[lint_runner]` / `[test_runner]`. The default sovereign.toml
    // committed at the workspace root points at
    // `scripts/sovereign-lint.sh` which fan-runs `cargo check` over
    // sovereign + commonwealth + corpus-engine in parallel. So one
    // env var lights up coverage for all three.
    let workspace_dir = resolve_workspace_dir();
    let bootstrap::WatcherAtlasSetup {
        watcher_heartbeat,
        lint_watcher,
        test_watcher,
        watched_lint_scope,
        watched_test_scope,
        watcher_monitor: _watcher_monitor,
        work_atlas_mesh_store,
        work_atlas_store,
        work_atlas_broadcaster,
        work_atlas_cfg,
        work_atlas_repo_root,
        work_atlas_repo_id,
        work_atlas_branch,
    } = bootstrap::setup_watchers_and_work_atlas(
        &workspace_dir,
        &data_dir,
        Arc::clone(&lint_store),
        Arc::clone(&test_store),
    );

    // ── CorpusEngine ──────────────────────────────────────────────
    // Single shared instance: powers both the `/mcp` tool registry
    // (find_callers, code_search, etc.) AND — now that we wired
    // the engine into the mesh daemon's AppState — the
    // `corpus_collaborate` handler that runs the daemon's share of
    // a partitioned Wikipedia/etc. ingest via
    // `engine.ingest_with_overrides`.
    //
    // The embed function MUST be real. Earlier this session the
    // daemon shipped a zero-vector stub here (correct for SCIP
    // code graphs, which don't embed), and when collaborative
    // ingestion started calling `ingest_with_overrides` it wrote
    // ~4 million 768-dim all-zeros vectors into the partition's
    // `chunks.lance` at "60,000 chunks/sec" — nonsense embeddings
    // at the speed of the Lance writer, not the embed model. Any
    // merge of that partition into the canonical index would have
    // poisoned retrieval with zero vectors.
    //
    // Route EmbedFn through the already-loaded `provider`. Same
    // llama.cpp embed slot the desktop's ingest uses, same 1024
    // dims, same pooling. Also wire the batch variant: Wikipedia
    // throughput is ~5× higher with batched embed calls on
    // M-series Metal compared to per-chunk.
    // Resolve the persistent node_id up front so the engine's
    // `partition_path(corpus_id)` returns the same
    // `<corpus>-partition-node-<hex>` the daemon itself will expect.
    // Without this the engine defaults to `self_node_id = "local"` and
    // every partition-of-self lookup misses — `in_progress_ingestions`
    // returns 0 for a partition dir that's sitting right there on
    // disk with `ingestion_in_progress=true`.
    //
    // Resolution order mirrors what `EmbeddedDaemon::start_daemon`
    // does on resume vs. create:
    //   1. `<data_dir>/node_id` file, if present.
    //   2. `self_node_id` baked into `mesh.json` (the common case —
    //      existing meshes carry the id inside the mesh snapshot even
    //      when the standalone node_id file was never materialised).
    //   3. Generate a fresh id and persist it (fresh install).
    // `load_or_generate_self_node_id` covers (1) and (3) but would
    // ignore (2), which is exactly the bug we're fixing: the user's
    // daemon resumes with mesh.json's id while the engine had been
    // minting a mismatched fresh one.
    let self_node_id = bootstrap::resolve_self_node_id(&data_dir);
    // Stamp outbound NoteStore propagation events with this node
    // id. `content_hash` is the dedup primary key on the gossip
    // wire so `origin_node_id` rotation (toolbx rebuilds without
    // ~/.svrnmesh bind-mount) doesn't create duplicates — this
    // field is informational, surfaced in the audit display.
    if let Err(e) = notes_store.set_origin_node_id(self_node_id.to_string()) {
        tracing::warn!(
            target = "notes",
            error = e,
            "notes: origin_node_id already set — wiring race?"
        );
    }
    // The reading half of the same identity. `set_origin_node_id` above
    // decides whose name goes ON outbound notes; this decides whose name
    // a reader sees on the notes coming back — including gossiped ones
    // from peers. Wired together deliberately: a store with only the
    // first renders its own notes as an unrecognised node.
    //
    // The roster is INJECTED rather than read by the notes crate, which
    // is the knowledge layer and holds no mesh types. `persist::load`
    // stays the single reader of mesh.json.
    match bootstrap::build_node_roster(&data_dir, self_node_id) {
        Some(roster) => {
            let self_name = roster.self_name().unwrap_or("<unnamed>").to_string();
            if let Err(e) = notes_store.set_node_roster(roster) {
                tracing::warn!(
                    target = "notes",
                    error = e,
                    "notes: node_roster already set"
                );
            } else {
                tracing::debug!(
                    target = "notes",
                    self_node = %self_node_id,
                    self_name = %self_name,
                    "notes: node roster wired — authors resolve to mesh names"
                );
            }
        }
        None => {
            // Solo node, or mesh.json absent/unparseable. Attribution
            // degrades to the raw id rather than to a guess, so say so
            // once at boot instead of leaving the operator to wonder why
            // every note reads "unrecognised node".
            tracing::debug!(
                target = "notes",
                self_node = %self_node_id,
                "notes: no mesh roster — note authors will render as raw node ids"
            );
        }
    }

    // GliNER per-chunk entity extractor — hoisted out of the engine
    // block so both the engine's tiered runner (conv corpora) AND the
    // folder_tiered_deps below can share the same Arc<dyn> handle
    // (the underlying GlinerExtractor is ~150MB ONNX; one load only).
    //
    // The raw `Arc<GlinerExtractor>` is hoisted alongside the
    // trait-object wrapper so the NoteStore T2 path can install
    // it as a `GlinerFn` adapter without re-loading the model.
    // The store opened above is handed to gliner as a port (no second handle).
    let chunk_entity_store: Arc<dyn sovereign_core::daemon_wire::conv_tiered::ChunkEntityStore> =
        state_store_concrete.clone();
    let (gliner_raw, chunk_entity_extractor) = bootstrap::load_gliner_extractor(chunk_entity_store);

    let (engine, embed_model_id): (Arc<CorpusEngine>, String) = bootstrap::build_corpus_engine(
        &data_dir,
        Arc::clone(&provider),
        Arc::clone(&notes_store),
        &gliner_raw,
        &config,
        self_node_id,
        &chunk_entity_extractor,
    );

    // Self-healing corpus maintenance. Continuous appenders (the
    // `wikipedia-newsworthy` freshness daemon, watched folders, mesh pulls)
    // leave rows outside the indexes; lancedb then flat-scans them on every
    // search, which is silent, correct, and progressively slower. A desktop
    // user has no way to notice or fix that, so the daemon owns it. See
    // `crate::corpus_maintenance`.
    crate::corpus_maintenance::spawn(Arc::clone(&engine));

    // ── Folder tiered deps ───────────────────────────────────────
    // Watched-folder corpora reuse the conv-tiered table shape
    // (`conv_*` tables, conv_uuid = corpus_id) via the
    // `FolderTieredProvider`. The driver opens its own
    // SqliteStateStore handle so this block is independent of the
    // engine-side conv provider; both share the underlying db file
    // (`~/.svrnmesh/sovereign.db`).
    //
    // Installed on the manager via `set_tiered_deps` after the
    // manager is constructed (~line 1593 below). Without these,
    // `enable_enrichment` falls back to the legacy subprocess.
    let folder_tiered_deps = bootstrap::build_folder_tiered_deps(
        &data_dir,
        Arc::clone(&provider),
        chunk_entity_extractor,
    );

    // ── Solve job table ───────────────────────────────────────────
    // Shared between the /v1/solve/jobs HTTP router (installed in
    // install_http_and_mcp below) and the solve/solve_status/
    // solve_cancel MCP tools (registered in build_tool_registry) —
    // an MCP agent and a curl session see the same jobs. The solver
    // calls back into this daemon's own /v1/chat/completions over
    // loopback.
    let solve_jobs = Arc::new(solve_http::SolveJobs::new(config.daemon.client_port));

    // ── Shared merged SCIP graph ──────────────────────────────────
    // Built ONCE here and handed to BOTH the tool registry (below) and the
    // project reindexer (`start_freshness_pipeline`), so the reindexer's live
    // updates — the tree-sitter overlay on every save and the periodic full
    // rebuild — are visible to `symbols`/`callers`/`blast` immediately, with no
    // daemon restart. Previously each side built its own snapshot and the
    // reindexer's graph had no readers, so the tool surface was frozen at
    // startup — the deepest cause of "the watcher is always stale."
    let merged_scip_handle: corpus_engine_watchers::reindexer::ScipGraphHandle =
        std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
            tool_registry::build_merged_scip_graph(&data_dir.join("indexes")).await,
        ));

    // ── Tool registry (code intelligence + notes) ─────────────────
    // The embedded daemon serves /mcp for all locally-indexed corpora
    // under data_dir/indexes/. Tools return helpful errors when no
    // index is installed yet (first boot after setup, pre-project-init).
    let tools = build_tool_registry(
        &data_dir,
        Arc::clone(&engine),
        Arc::clone(&notes_store),
        Arc::clone(&lint_store),
        Arc::clone(&test_store),
        test_watcher.clone(),
        watched_lint_scope.clone(),
        watched_test_scope.clone(),
        Arc::clone(&watcher_heartbeat),
        workspace_dir.clone(),
        Arc::clone(&work_atlas_store),
        work_atlas_cfg.clone(),
        Arc::clone(&work_atlas_broadcaster),
        work_atlas_repo_root.clone(),
        work_atlas_repo_id.clone(),
        work_atlas_branch.clone(),
        Arc::clone(&solve_jobs),
        Arc::clone(&merged_scip_handle),
    )
    .await;

    // ── Assemble the daemon's services, THEN commission the daemon ────
    //
    // Order is load-bearing and is the point of daemon-convergence Phase 2:
    // every dependency is built first and handed over in one total value, so
    // there is no window in which a request can reach a half-wired daemon and
    // no slot this bootstrap can forget. `DeferredDaemon` breaks the one
    // genuine cycle — the daemon serves peers through a provider that routes
    // to peers — and carries no capability of its own.
    let (deferred_daemon, mesh_provider, in_flight_gauge) =
        bootstrap::build_mesh_provider(Arc::clone(&provider), deferred_daemon).await;
    let routed_provider: Arc<dyn InferenceProvider> = mesh_provider.clone();

    // Notes-rail convergence recorder (order commons-fluency fix 9):
    // ONE shared instance — named on the daemon's `HeadlessRails` so `/status`
    // reads it, and handed to BOTH the outbound publish sink and the inbound
    // ingest poller so the writers' stamps are what `/status` reports. A second
    // copy would let the status section disagree with the sink — never.
    let convergence_recorder = Arc::new(sovereign_mesh::peer_adapter::MeshConvergence::new());

    bootstrap::wire_note_propagation_sink(
        Arc::clone(&notes_store),
        Arc::clone(&work_atlas_mesh_store) as Arc<dyn sovereign_contracts::peer::ReplicatedKv>,
        self_node_id,
        Arc::clone(&convergence_recorder) as Arc<dyn sovereign_contracts::peer::Convergence>,
    );

    bootstrap::spawn_notes_tier_backfill(Arc::clone(&notes_store));

    bootstrap::spawn_notes_ingest_poller(
        Arc::clone(&work_atlas_mesh_store) as Arc<dyn sovereign_contracts::peer::ReplicatedKv>,
        Arc::clone(&notes_store),
        self_node_id,
        Arc::clone(&convergence_recorder) as Arc<dyn sovereign_contracts::peer::Convergence>,
    );

    bootstrap::spawn_lazy_stamp_fingerprints(Arc::clone(&engine));

    bootstrap::spawn_vector_index_readiness_sweep(Arc::clone(&engine));

    bootstrap::spawn_tier2_enrichment_resume(&data_dir);

    let advertise_embed =
        bootstrap::advertise_embed_model(Arc::clone(&provider), &config, resolved_embed_family)
            .await;

    // Arm clause ST-8's geometry gate from the width the probe just measured.
    // Without this the daemon's `open_index` cannot tell a 768-dim corpus from
    // a 1024-dim one and admits both — and the model NAME cannot substitute:
    // on the maintainer's host `oicp-types` is 768-dim while recording the
    // same `qwen-embedding-0.6b` string as the 1024-dim corpora.
    if let Some(info) = advertise_embed.info() {
        engine.set_expected_embedding_dimensions(info.dimensions);
    }

    // Keep the reindexer alive for the lifetime of the daemon.
    // The variable binding is load-bearing — dropping the Arc
    // stops every supervised watcher.
    let (_reindexer_handle, project_http, knowledge_view_http) =
        bootstrap::start_freshness_pipeline(
            &data_dir,
            Arc::clone(&notes_store),
            Arc::clone(&engine),
            Arc::clone(&provider),
            Arc::clone(&merged_scip_handle),
        )
        .await;

    // The watched-folder singleton must be installed before the daemon starts
    // serving, but the ROUTE is now part of the daemon's declared capability
    // rather than something this call installs — so a failed subsystem yields
    // handlers that answer 503 with a named reason, not routes that 404.
    let _watched_subsystem = bootstrap::setup_watched_folders(
        Arc::clone(&engine),
        Arc::clone(&state_store),
        &data_dir,
        &config,
        folder_tiered_deps,
    )
    .await;

    // ── The skills + authoring layer the desktop carries (rung 6 B) ──────
    //
    // The daemon's Runtime used to commission with an EMPTY skill registry
    // and no recipe-authoring tools, while the desktop shipped both. A
    // conversation tagged `skill_id = "recipe-author"` therefore routed
    // into its agent loop from the desktop and ran as plain chat from the
    // daemon — the C2 divergence on the skill axis, silently. Both loads
    // now come from the same homes the desktop uses: the compiled-in
    // builtin set (sovereign_contracts::skills) and the notes+features
    // backed RecipeAuthoringTools bundle.
    //
    // NAMED DELTA, not silent: the desktop additionally overlays USER
    // skills from its `DesktopConfig.skills_dir` (an app-support path a
    // daemon must not read). A user-authored custom skill therefore routes
    // its agent loop in Local mode only until the daemon grows a skills
    // dir of its own — recorded on the rung 6 row, and visible in attach
    // as an untagged-shaped answer, not a crash.
    let mut skills = sovereign_core::SkillRegistry::new();
    sovereign_contracts::skills::register_builtin_skills(&mut skills);
    tracing::info!(
        skills = skills.list().len(),
        "daemon: builtin skills registered (the desktop's shared set)"
    );
    let skills = Arc::new(skills);

    // features.db — the recipe-author project layer. Warn-and-skip on
    // failure, the same graceful-degrade posture the desktop's bootstrap
    // takes: the daemon still serves turns without it, and the authoring
    // tools report their own named degradation.
    let features_store: Option<
        Arc<sovereign_tools::recipe_author::recipe_project_store::RecipeProjectStore>,
    > = match sovereign_tools::recipe_author::recipe_project_store::RecipeProjectStore::open(
        &data_dir.join("features.db"),
    ) {
        Ok(s) => {
            tracing::info!("daemon: recipe-author features.db opened");
            Some(Arc::new(s))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "daemon: features.db unavailable — recipe-author tooling will degrade"
            );
            None
        }
    };

    // ── The daemon commissions the ONE Runtime ────────────────────────────
    //
    // `quality/TOPOLOGY.md` §3.5: "DAEMON — the only process that assembles a
    // Runtime". Until now `sovereign daemon run` held every ingredient — the
    // corpus engine, the state store, the routed inference provider — and no
    // thing that ANSWERS, so a turn could only be served by a host that had
    // built its own `Runtime` around its own copy of the recipe. That is what
    // made "the daemon serves the turn" impossible to state in the type.
    //
    // The recipe is `sovereign-runtime-recipe`, the same one `svrn chat` uses,
    // so the daemon cannot acquire a private dialect of the retrieval stack.
    // Two host inputs differ from the CLI's and both are decisions, not
    // defaults:
    //
    //   * shell WITHHELD — §10 "Decisions taken" 1. Shell execution does not
    //     move into a long-lived daemon running as a different user with a
    //     different cwd. Named in `tool_bundles` as a `Withheld` family, so it
    //     reads as a decision rather than an omission.
    //   * `mesh_knowledge` is the loopback §3.5 names: this daemon's own
    //     `/v1/knowledge/search`. It was left at the recipe's `None` until
    //     2026-09-18, and no daemon-served turn fanned out to a peer.
    // The turn path and the baseline bundles name the PORT, not the store's
    // crate — one coercion here is the whole seam.
    let notes_port: Arc<dyn sovereign_contracts::notes::AgentNotes> = notes_store.clone();
    let common = sovereign_runtime_recipe::common_parts(
        sovereign_runtime_recipe::RecipeInputs {
            inference: Arc::clone(&routed_provider),
            store: Arc::clone(&state_store),
            conv_tiered: Some(Arc::clone(&state_store_concrete)
                as Arc<dyn sovereign_core::conv_tiered::ConvTieredReader>),
            corpus_engine: Arc::clone(&engine),
            note_store: Some(Arc::clone(&notes_port)),
            // The same compiled-in skill set the desktop ships (rung 6
            // commit B) — built just above from the ONE shared home, so a
            // tagged conversation routes the same agent loop whichever
            // host answers. User-skill overlays are a named delta on the
            // rung row.
            skills,
            // Approvals are out of scope for v1 of the turn protocol — the
            // same posture `sovereign-server` ships. `TurnRequest::Answer`
            // exists on the wire; routing it to a daemon-side session owner is
            // Phase 5's remaining work (hazard 12).
            approval: Arc::new(sovereign_core::executor::AutoApprovalChannel),
            inference_config: sovereign_core::types::InferenceConfig::default(),
            indexes_dir: data_dir.join("indexes"),
            // Derived ONCE, by the corpus-engine builder, and handed here —
            // see `build_corpus_engine`. The atlas embedding cache keys on it.
            embed_model: embed_model_id.clone(),
            // The families this daemon's turn registry carries. Shell is
            // named as WITHHELD rather than simply absent, so the decision is
            // a value a reader finds here (TOPOLOGY §10 "Decisions taken" 1;
            // ARCH §18.3).
            tool_bundles: {
                let mut b = sovereign_runtime_recipe::baseline_bundles(
                    sovereign_runtime_recipe::BaselineDeps {
                        store: &state_store,
                        inference: &routed_provider,
                        corpus_engine: &engine,
                        // The daemon opened this above; wiring it here is what
                        // gives `knowledge_lookup` its notes channel. It ran
                        // with that channel dark until 2026-08-26 while the
                        // desktop, which wired it by hand, did not.
                        note_store: Some(&notes_port),
                        web: sovereign_tools::bundles::WebReach::Granted(
                            sovereign_core::egress::search_client()
                                .expect("egress boundary search client build"),
                        ),
                        // No operator switch on a daemon, and escalating to the
                        // open web without one is a decision nobody made.
                        escalation: sovereign_tools::bundles::WebEscalation::Disabled,
                    },
                );
                b.push(Box::new(sovereign_tools::bundles::WikipediaTools::new(
                    Arc::clone(&engine),
                )));
                // Recipe-authoring, the desktop's twin (rung 6 commit B): the
                // same bundle the desktop's bootstrap pushes, wired with the
                // SAME notes adapter + features store — so a conversation
                // tagged `recipe-author` has its tool set whichever host
                // answers. Absent stores are a DEGRADATION the bundle's
                // report names, matching the desktop's posture.
                b.push(Box::new({
                    let mut ra = sovereign_tools::bundles::RecipeAuthoringTools::new();
                    if let Some(fs) = features_store.as_ref() {
                        ra = ra.with_notes(Arc::clone(&notes_store)
                            as Arc<dyn sovereign_contracts::recipe::notes::RecipeNotes>);
                        ra = ra.with_features(Arc::clone(fs));
                    }
                    ra
                }));
                b.push(Box::new(sovereign_contracts::tool_bundle::Withheld::new(
                    "shell",
                    "no shell in a long-lived daemon running as a different user \
                     with a different cwd (TOPOLOGY §10 decision 1)",
                )));
                b
            },
            // No settings panel on this host, so nothing to consult: every
            // family composed above registers.
            switches: sovereign_runtime_recipe::ToolSwitches::Ungoverned,
            // No config file of its own: the canonical `[[mcp_servers]]` array
            // is the whole declaration on this host.
            mcp_extra: Vec::new(),
            // A service must reach `listening` promptly. The meta-atlas is a
            // ~1 GB JSON parse (981 MB on the authoring host) and blocking
            // boot on it is a daemon that looks hung to `svrn daemon start`;
            // the desktop reached the same conclusion in 2026-06 and has
            // warmed it in the background ever since.
            warmth: sovereign_runtime_recipe::LaneWarmth::Deferred,
            // Serves every installed corpus, so every lane member is
            // reachable. Byte-identical to the behaviour this host had
            // before `LaneScope` existed.
            scope: sovereign_runtime_recipe::LaneScope::All,
            // `crate::build::inference::load_provider` above already installed a
            // rerank slot INSIDE the embedded engine from the same
            // `SOVEREIGN_RERANK_MODEL_PATH`. A standalone one here would put
            // the same GGUF in this process twice, and the VRAM pre-flight
            // would not catch it — it plans one rerank slot.
            rerank: sovereign_runtime_recipe::RerankWiring::AlreadyInProvider,
        },
        &sovereign_runtime_recipe::TracingProgress,
    )
    .await;
    // ── The turn's Scope: resolved, not absent ───────────────────────────
    //
    // `quality/DAEMON_CORE.md` §3.3 measured BOTH slots at named absence on
    // this host, and absence was permissive: `sensitive_corpora: None` meant
    // "no sensitivity gate, all corpora eligible" and `corpus_principal: None`
    // meant the turn's `corpus_ceiling` was never computed. The daemon is the
    // host that answers turns, so both are wired here.
    //
    //   * `sensitive_corpora` — the daemon's OWN watched-folder manager (the
    //     canonical `SensitiveCorpusOracle`, the same handle `lc_http` serves
    //     over). A corpus the user marked sensitive is structurally absent
    //     from ambient retrieval on the daemon, not only in the desktop's old
    //     in-process build.
    //   * `corpus_principal` — the local owner. The daemon is single-user, so
    //     every conversation it serves resolves and `build_context` produces a
    //     `Some(..)` ceiling rather than an absent one. A resolver that cannot
    //     name a caller returns `None`, and that turn REFUSES
    //     (`PrincipalScope::Unresolved`) instead of seeing every corpus.
    let sensitive_corpora: Option<Arc<dyn sovereign_core::traits::SensitiveCorpusOracle>> =
        crate::watched_folder_runtime::manager()
            .map(|m| m as Arc<dyn sovereign_core::traits::SensitiveCorpusOracle>);
    if sensitive_corpora.is_none() {
        // Named, not silent: the subsystem failed to install above, so there is
        // no oracle to consult. `None` here still means "no sensitivity gate"
        // (the pre-v1 behaviour) — reported so the gap is visible (ARCH §18.3).
        tracing::warn!(
            "daemon: no LocalCorpusManager installed — the sensitivity gate is \
             absent; sensitive watched-folder corpora will not be excluded from \
             ambient retrieval"
        );
    }
    let runtime = sovereign_runtime_recipe::commission(sovereign_core::RuntimeParts {
        sensitive_corpora,
        corpus_principal: Some(Arc::new(crate::principal::LocalOwnerPrincipal)),
        mesh_knowledge: sovereign_turn_client::knowledge_client::daemon_knowledge_source(&format!(
            "http://127.0.0.1:{}",
            config.daemon.client_port
        )),
        ..common.parts
    });
    tracing::info!(
        tools = runtime.tools.count(),
        "daemon: Runtime commissioned — this process can serve a turn"
    );

    // sv-surface rung 6: the insight surface. The SAME `InsightService`
    // the desktop builds beside its state store, over THIS daemon's
    // `sovereign.db` connection and routed provider — so an attached
    // desktop's clip/list/search/delete answer from one service, not from
    // a second store the client process would have had to open. No sinks
    // yet on this host (the desktop's registry is empty too); the field
    // exists so the shape does not change when one lands.
    let insight_service = {
        Arc::new(sovereign_core::insight::InsightService::new(
            Arc::new(sovereign_store::insight_store::SqliteInsightStore::new(
                state_store_concrete.connection(),
            )),
            Arc::new(sovereign_core::insight::InsightSinkRegistry::new()),
            Arc::clone(&routed_provider),
        ))
    };

    // ── Commission, through THE assembler ─────────────────────────────
    //
    // This bootstrap no longer names its own variant. It hands its parts to
    // `crate::assemble`, the one exhaustive match over `Launch` that
    // constructs anything (`quality/TOPOLOGY.md` §10, Falsifier 3), and that
    // match decides what `sovereign daemon run` composes into. A refusal is
    // fatal and names both sides — a daemon that came up as the wrong shape is
    // the hazard, so there is nothing to degrade to (§18.3).
    let services = match crate::assemble(
        launch,
        crate::LaunchParts::Serving {
            serving: crate::ServingProfile {
                core: crate::ServingCore {
                    // The engine the auto_ingest loop and the
                    // /internal/corpus/* surface both read.
                    corpus_engine: Arc::clone(&engine),
                    inference_provider: Arc::clone(&routed_provider),
                    // The gauge the router above was built with, so AppState
                    // holds the same counter the provider's guards write
                    // (`quality/DAEMON_CORE.md` §4.2 "Where an install slot
                    // breaks a cycle").
                    in_flight_gauge: Some(in_flight_gauge),
                    // Phase 3: the headless daemon's own `sovereign.db`,
                    // opened at the top of this function. `reading_http` now
                    // resolves conversation titles on this variant too.
                    state_store: Arc::clone(&state_store),
                    // Phase 5c: the thing that answers. Commissioned just
                    // above, from the one shared recipe.
                    runtime: Arc::clone(&runtime),
                    insights: Some(insight_service),
                    // sv-surface D6: the recipe-author `features.db` opened
                    // above. Already warn-and-skip, so the `Option` here says
                    // the same thing the log line did — and `features_http`
                    // now renders it as a named 503 instead of a route that
                    // silently is not there.
                    features: features_store,
                },
                capability: crate::ServingCapability {
                    mcp: bootstrap::build_mcp_surface(tools, Arc::clone(&notes_store)),
                    project_http,
                    corpus_watch_http: crate::corpus_watch_http::corpus_watch_router(),
                    // sv-surface rung 5: workflow execution is a daemon job
                    // surface (`/internal/workflows/*`). The runtime routes
                    // `model:`/`embed:` steps back through this daemon's own
                    // loopback, and injects the corpus/atlas tools the base
                    // registry omits.
                    workflow_http: sovereign_workflow_host::workflow_http_router(
                        format!("http://127.0.0.1:{}", config.daemon.client_port),
                        std::sync::Arc::new(sovereign_tools::workflow_corpus_tools),
                    ),
                },
                advertise_embed,
            },
            headless: Some(crate::HeadlessExtras {
                rails: crate::HeadlessRails {
                    // Rebuilds the provider when `models.*` changes on disk. Holds
                    // the same deferred handle, bound below.
                    provider_factory: Arc::new(crate::provider::LlamaCppFactory {
                        daemon: Arc::clone(&deferred_daemon),
                    }),
                    // The work atlas writes into THIS store, so its entries reach
                    // the store's outbox and ride the ring rail (cw-lift 4b; the
                    // gossip enumeration this comment used to name was deleted at
                    // 2e). Without it the daemon builds a private in-memory store
                    // and atlas data is invisible across the mesh.
                    mesh_store: Arc::clone(&work_atlas_mesh_store),
                    convergence_recorder: Arc::clone(&convergence_recorder),
                },
                knowledge_view_http,
                solve_http: solve_http::solve_router(Arc::clone(&solve_jobs)),
            }),
        },
    ) {
        Ok(s) => s,
        Err(refusal) => {
            eprintln!("error: {refusal}");
            return 1;
        }
    };
    let daemon = crate::EmbeddedDaemon::new(data_dir.clone(), config.clone(), services);
    deferred_daemon.bind(Arc::clone(&daemon));

    // Host side of distributed-inference auto-warm. When this node distributes a
    // large primary across mesh workers, the embedded engine calls this seam to
    // seed each worker's shard BEFORE loading — so the load is all cache hits and
    // never streams a large weight share (the upload deadlock). This retires the
    // manual `SOVEREIGN_RPC_ASSUME_WARMED` for the common case. Installed
    // unconditionally (harmless on a node that never distributes) so both
    // auto-discovered and manual (`SOVEREIGN_RPC_WORKERS`) hosts auto-warm.
    crate::rpc_warm_http::install_rpc_warm_orchestrator(Arc::clone(&daemon));

    // Must be installed BEFORE discovery starts spawning the child: the
    // manifest is a boot-time snapshot taken while the slot is still unspawned,
    // so without this the node never advertises the model its child ends up
    // serving, and every request that names it 503s.
    bootstrap::spawn_self_manifest_refresh(
        Arc::clone(&mesh_provider),
        distributed_primary_slot.clone(),
    );

    bootstrap::spawn_rpc_worker_discovery(
        Arc::clone(&daemon),
        engine_handle,
        distributed_primary_slot,
    );

    bootstrap::spawn_slot_alias_push(Arc::clone(&daemon), mesh_provider);

    // ── Resume or bootstrap a solo mesh ───────────────────────────
    if let Some(exit_code) = resume_or_bootstrap_mesh(&daemon, &config).await {
        return exit_code;
    }

    // The log is append-only across restarts by design, so this line is the
    // KEY every later line in this generation joins against: which binary,
    // built when, under which run id (sovereign_contracts::run_identity).
    // The serve task's bind is best-effort by design (the default-port
    // integration tests must not be stranded), so "running" is a claim this
    // process may make only AFTER reading the bind outcome. Until 2026-09-10
    // the line below fired before the bind finished: the desktop e2e
    // harness's fixture daemon lost `:9741` to the operator's
    // launchd-relaunched daemon, logged "is running", served nothing, and
    // the harness's port probe was answered by the stranger — a fixture
    // ingest landed in the operator's real home. A daemon with no client
    // API is not running; it exits non-zero so the service manager retries.
    let client_addr = match daemon
        .client_listener(std::time::Duration::from_secs(60))
        .await
    {
        crate::ClientListener::Bound(addr) => addr,
        crate::ClientListener::Failed(e) => {
            eprintln!("error: the client API is not listening — {e}");
            return 1;
        }
        crate::ClientListener::Pending => {
            eprintln!(
                "error: the client API bind did not settle within 60s — \
                 refusing to report a daemon that may be serving nothing"
            );
            return 1;
        }
    };

    let build = sovereign_contracts::run_identity::build();
    tracing::info!(
        client_addr = %client_addr,
        client_port = config.daemon.client_port,
        internal_port = config.daemon.internal_port,
        run = sovereign_contracts::run_identity::run_id(),
        pid = build.pid,
        exe = %build.exe,
        exe_mtime = build.exe_mtime.as_deref().unwrap_or("unreadable"),
        version = env!("CARGO_PKG_VERSION"),
        "svrn daemon is running"
    );

    // ── Listener watchdog (DAEMON_RESILIENCE.md P0.5) ─────────────
    // Closes the phantom-Running hole (process alive, no client
    // listener) from OUTSIDE the deliberately best-effort bind path.
    // Exit-code contract: 104 (see `shutdown_daemon`).
    let _listener_watch_handle = {
        let bind = config.daemon.client_bind.clone();
        let port = config.daemon.client_port;
        crate::supervise::spawn_supervised("listener_watch", move || {
            crate::listener_watch::watch_loop(bind.clone(), port)
        })
    };

    let _work_atlas_gc_handle = bootstrap::finalize_work_atlas(
        Arc::clone(&daemon),
        Arc::clone(&work_atlas_broadcaster),
        Arc::clone(&work_atlas_store),
        work_atlas_cfg.clone(),
    );

    // Measurement history onto the ring journal: a migration for records filed
    // before the namespace moved to the rail, and the closure loop for a run
    // taken before this node was in a mesh. Deferred — it needs `app_state`.
    bootstrap::reconcile_local_measurements(Arc::clone(&daemon));

    bootstrap::install_foreground_yield_hook(
        Arc::clone(&daemon),
        lint_watcher.clone(),
        test_watcher.clone(),
    );

    eprintln!(
        "svrn daemon running — http://localhost:{}/v1 + /mcp",
        config.daemon.client_port
    );

    let (pid_path, self_pid) = bootstrap::write_pidfile();

    // ── Block until SIGINT/SIGTERM, then drain, persist, and exit ──
    shutdown_daemon(daemon, &pid_path, self_pid).await
}
