// SPDX-License-Identifier: AGPL-3.0-or-later
//! Split from daemon_cmd/mod.rs for the §3.2 size ceiling (behaviour-preserving move).

use std::sync::Arc;

use corpus_engine_atlas_reader::ports::AtlasPort;
use corpus_index::ingest_port::daemon::IngestPort;
use sovereign_contracts::launch::Launch;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;

use super::log_rotation;
use super::memory_watch;
use super::rlimit;
use super::shutdown_daemon;
use super::sovereign_root;
use super::start::start;

use crate::bootstrap;
use crate::tool_registry::build_tool_registry;
use crate::workspace::resolve_workspace_dir;

pub(super) async fn run_daemon(
    launch: &Launch,
    args: &[String],
    hosted: Option<crate::serve_client::HostedServe>,
    code: Option<crate::hosted_code::HostedCode>,
    ingest: Option<crate::hosted_ingest::HostedIngest>,
    mesh: Option<crate::hosted_mesh::HostedMesh>,
    posture: crate::posture::Posture,
) -> i32 {
    #[cfg(unix)]
    rlimit::raise_open_file_limit();
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
    let _run_lock = match host_kit::RunLock::acquire(
        &config.data.dir,
        sovereign_contracts::rebrand::DAEMON_LOCK_FILE,
    ) {
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
            if matches!(e, host_kit::RunLockError::Held { .. }) {
                eprintln!(
                    "  Check `svrn daemon status`; stop it with `svrn daemon stop`.\n  \
                     A harness that wants a second daemon gives it its OWN data \
                     dir (`--config` with a distinct `[data] dir`, or \
                     SVRNMESH_DATA_DIR) — the lock is per data root, not per HOME."
                );
            }
            return 1;
        }
    };

    let super::serving_boot::ServingBoot {
        provider,
        resolved_embed_family,
        reload,
        deferred_daemon,
        ner,
        ranked,
    } = match super::serving_boot::boot_serving(&config, args, &config_override, hosted).await {
        Ok(s) => s,
        Err(code) => return code,
    };

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

    // svrn's memory notes (lessons, the tool_decision dossier, the commissive
    // handler's commitments and todos) and its MCP call log live in this same
    // store (pb-notes-memory); `notes.db` is the code program's. Once per
    // root, svrn's rows move out of it here, before code (when composed)
    // opens it: a copy of notes.db first, a marker last. A failed move leaves
    // every row where it was and is retried at the next boot; svrn serves on
    // meanwhile, and the rows it has not moved stay readable through code.
    let notes_db = data_dir.join("notes.db");
    match state_store_concrete.migrate_notes_db(&notes_db).await {
        Ok(m) if m.already_done => {
            tracing::debug!("daemon: svrn's rows already moved out of notes.db")
        }
        Ok(m) => tracing::info!(
            rows_before = m.rows_before,
            moved = m.moved,
            kept = m.kept,
            backup = ?m.backup,
            "daemon: svrn's memory notes moved out of notes.db into svrn's store"
        ),
        Err(e) => {
            tracing::error!(error = %e, notes_db = %notes_db.display(),
                "daemon: svrn's rows could not be moved out of notes.db; retried next boot");
            eprintln!(
                "warning: svrn's memory notes could not be moved out of {}: {e}",
                notes_db.display()
            );
        }
    }

    // ── Workspace (for the code program's watchers, when composed) ──
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
    // Stores dial this and nothing here brings cw-rails up: `svrn mesh up`
    // does, on the operator's word (pb-rails-untether, phase-b-31).
    let rails_base = crate::rails_client::resolve_rails_base(&config.daemon);
    tracing::debug!(
        rails_base,
        "boot: cw-rails is dialed, never brought up; absent, rail surfaces name `svrn mesh up`"
    );
    // The daemon's ONE `RailsKv` (five-programs fp-88): handed on
    // `HeadlessRails` to `AppState`'s KV port, so it writes the store cw-rails
    // holds and pumps onto the ring. Construction checks no presence; a
    // cw-rails that is down surfaces on the first call. The work atlas
    // (pb-code-daemon-exit) and the notes sink and poller (pb-notes-memory)
    // that also wrote here are the code program's, dialing the same store.
    let work_atlas_mesh_store: Arc<dyn sovereign_contracts::peer::ReplicatedKv> =
        Arc::new(crate::rails_client::kv::RailsKv::new(rails_base));

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
    // The node's mesh through cw-rails, as the distribution composed it; svrn
    // alone reads none, named there. Composed here, ahead of the notes roster,
    // because the roster reads members' names through its reader (seat
    // phase-b-84).
    let mesh_access = match mesh {
        Some(m) => m.compose(&crate::rails_client::resolve_rails_base(&config.daemon)),
        None => crate::hosted_mesh::MeshAccess::absent(),
    };
    // Whose name a reader of code's notes sees on each author (including
    // gossiped notes from peers), from cw-rails' roster; code's notes rail
    // wires it, with the origin id, onto code's store (pb-notes-memory).
    let notes_roster =
        bootstrap::build_node_roster(mesh_access.membership.as_ref(), self_node_id).await;

    // The served NER kind's handle (loaded once per process), shared by
    // ingest's tiered runner and folder driver, the NoteStore T2 hook and the
    // turn's recipe (below): one model.
    let gliner_raw = ner;

    let (notes_embed, notes_gliner) = bootstrap::notes_tier_fns(&provider, &gliner_raw);
    // The embed model's id, derived ONCE: the engine records it in
    // `_corpus_meta.json` and the Runtime's atlas embedding cache keys on it.
    // A second derivation is how the cache and the shards start disagreeing
    // about which model wrote them (ARCH §10.6).
    let embed_model_id = config
        .local_embed_model_id()
        .unwrap_or_else(|| "unknown-embed-model".to_string());

    // ── Ingest, when the distribution composes it ─────────────────
    // The stock binary hands in ingest's composition (pb-ingest-dial-daemon):
    // svrn links no corpus-engine, so the engine this process holds is built
    // by ingest's face for `IngestHost`. svrn alone gets `None`.
    tracing::info!(
        ?posture,
        withheld = posture.withheld().unwrap_or("nothing"),
        "daemon: svrn's posture (web reach, wikipedia, /mcp), the distribution's choice"
    );
    let (enrich_config, mount, recipe_authoring) = match ingest {
        Some(ingest) => {
            let atlas = ingest.atlas();
            let enrich_config = ingest.enrich_config();
            // Two tiered providers over the one `sovereign.db`: the engine's
            // conv provider and the watched-folder driver's own, independent
            // of it. The shared builder is the desktop's too.
            let tiered_provider = || {
                sovereign_tools::enrichment_bootstrap::build_folder_tiered_provider(
                    &data_dir,
                    Arc::clone(&provider),
                    Arc::clone(&atlas),
                )
            };
            let mount = ingest.compose(crate::hosted_ingest::IngestHost {
                data_dir: data_dir.clone(),
                provider: Arc::clone(&provider),
                embed_model: embed_model_id.clone(),
                node_id: self_node_id.to_string(),
                // The store opened above is the NER adapter's port (no
                // second handle).
                chunk_entity_store: state_store_concrete.clone(),
                ner: gliner_raw.clone(),
                conv_tiered: tiered_provider(),
                folder_tiered: tiered_provider(),
            });
            // The `sec_edgar` custom acquirer (ticker -> installed SEC filings
            // corpus) is svrn's, so ingest stays free of SEC domain knowledge.
            // Registered here, before any route mounts, so a fast install
            // cannot reach `acquire_source` before the acquirer exists.
            sovereign_tools::sec_edgar::register(mount.port.as_ref());
            // features.db — the recipe-author project layer, ingest's
            // (pb-ingest-rehome-daemon), composed over svrn's notes (the
            // store opened above; no second handle) and the mount's seams.
            // `None`: the distribution composed ingest without it (phase-b-87).
            let recipe_authoring = ingest.calls().recipe_authoring.as_ref().map(|compose| {
                compose(
                    &data_dir.join("features.db"),
                    state_store_concrete.clone()
                        as Arc<dyn sovereign_contracts::recipe::notes::RecipeNotes>,
                    mount.recipe_author.clone(),
                )
            });
            tracing::info!(
                recipe_authoring = recipe_authoring.is_some(),
                "daemon: the ingest program is composed in this process"
            );
            (Some(enrich_config), Some((mount, atlas)), recipe_authoring)
        }
        None => {
            tracing::info!(
                "daemon: no ingest program in this process; enrichment-config reads and \
                 writes, and recipe-author projects, report it absent"
            );
            (None, None, None)
        }
    };
    // Ingest's ports, as every consumer below acts on them: the engine's port
    // and its atlas. `None` is svrn alone; each consumer withholds its family
    // or subsystem and says so (pb-ingest-dial-daemon).
    let ingest_ports: Option<(Arc<dyn IngestPort>, Arc<dyn AtlasPort>)> = mount
        .as_ref()
        .map(|(m, atlas)| (Arc::clone(&m.port), Arc::clone(atlas)));
    let ingest_port = ingest_ports.as_ref().map(|(port, _)| Arc::clone(port));
    let ingest_atlas = ingest_ports.as_ref().map(|(_, atlas)| Arc::clone(atlas));
    if ingest_ports.is_none() {
        tracing::info!(
            "daemon: no ingest program in this process; its tool families, subsystems and \
             routes are withheld by name, and workflow runs get no corpus/atlas tools"
        );
    }

    // Self-healing corpus maintenance. Continuous appenders (the
    // `wikipedia-newsworthy` freshness daemon, watched folders, mesh pulls)
    // leave rows outside the indexes; lancedb then flat-scans them on every
    // search, which is silent, correct, and progressively slower. A desktop
    // user has no way to notice or fix that, so the daemon owns it. See
    // `crate::corpus_maintenance`.
    match &ingest_port {
        Some(port) => crate::corpus_maintenance::spawn(Arc::clone(port)),
        None => tracing::info!("daemon: no ingest program; corpus maintenance does not run"),
    }

    // The rest of ingest's mount, taken apart once.
    let (ingest_index, recipe_harness, folder_tiered, arm_geometry) = match mount {
        Some((m, _)) => {
            // Idempotent one-shot, supervised (DAEMON_RESILIENCE.md P0.4);
            // the chore is ingest's, the supervision this process's.
            let stamp = m.lazy_stamp;
            crate::supervise::spawn_supervised("lazy_stamp_fingerprints", move || stamp());
            (
                Some(m.index),
                Some(m.harness),
                m.folder_tiered,
                Some(m.arm_geometry),
            )
        }
        None => (None, None, None, None),
    };
    // Reads go through the one leaf reader: the engine's own cached one when
    // ingest is composed, `FsIndexSource` over the same root when not. Either
    // way the geometry gate is armed from this daemon's embed probe (below).
    let (read_index, arm_geometry): (
        Arc<dyn corpus_index::source::IndexSource>,
        Box<dyn Fn(usize) + Send + Sync>,
    ) = match (ingest_index, arm_geometry) {
        (Some(index), Some(arm)) => (index, arm),
        _ => {
            let fs = Arc::new(
                corpus_index::fs_source::FsIndexSource::new(data_dir.join("indexes"))
                    .with_embedding_model(&embed_model_id),
            );
            let armed = Arc::clone(&fs);
            (
                fs,
                Box::new(move |dims| armed.set_expected_embedding_dimensions(dims)),
            )
        }
    };

    // ── Folder tiered deps ───────────────────────────────────────
    // Watched-folder corpora reuse the conv-tiered table shape
    // (`conv_*` tables, conv_uuid = corpus_id) via the
    // `FolderTieredProvider`, over its own SqliteStateStore handle on the
    // shared db file (`~/.svrnmesh/sovereign.db`). Installed on the manager
    // via `set_tiered_deps`; without them `enable_enrichment` falls back to
    // the legacy subprocess.
    let folder_tiered_deps = folder_tiered.map(|tiered| {
        tracing::info!(
            target: "sovereign_tools::enrichment_bootstrap",
            "enrichment_bootstrap: folder tiered deps constructed — FolderTieredProvider wired"
        );
        sovereign_tools::local_corpus::watched::enrich::TieredDeps { tiered }
    });

    // ── Tool registry (svrn's) ────────────────────────────────────
    // svrn's own `/mcp` tools. Code's tools are code's (pb-code-daemon-exit),
    // the solver's among them (pb-meshapp-solve): they mount beside these
    // when a distribution composes the code program into this process, and
    // svrn alone names `svrn code mcp` for them.
    let tools = build_tool_registry(ingest_ports.clone()).await;

    // Notes-rail convergence recorder (order commons-fluency fix 9):
    // ONE shared instance — named on the daemon's `HeadlessRails` so `/status`
    // reads it, and handed to code's notes rail (its publish sink and ingest
    // poller, pb-notes-memory) so the writers' stamps are what `/status`
    // reports. A second copy would let the status section disagree with the
    // sink — never. With no code program here nothing stamps it, and
    // `/status` reports the rail as never having converged.
    let convergence_recorder = Arc::new(crate::convergence::MeshConvergence::new());

    // ── The code program, when the distribution composes it ───────
    // The stock binary hands code's composition in (F2 (a), phase-b-30):
    // code's tools on the one `/mcp`, `/v1/projects/*`, its editor door, its Reindexer,
    // its watchers and work atlas, over this root's `indexes/` and
    // `notes.db`. A composition that fails refuses boot by name.
    let code_mount = match code {
        Some(code) => match code
            .compose(crate::hosted_code::CodeHost {
                data_dir: data_dir.clone(),
                workspace: workspace_dir.clone(),
                index: Arc::clone(&read_index),
                notes_embed,
                notes_gliner,
                node_id: self_node_id,
                roster: notes_roster,
                convergence: Arc::clone(&convergence_recorder)
                    as Arc<dyn sovereign_contracts::peer::Convergence>,
            })
            .await
        {
            Ok(mount) => {
                tracing::info!("daemon: the code program is composed in this process");
                Some(mount)
            }
            Err(e) => {
                tracing::error!(error = %e, "daemon: the code program could not be composed");
                eprintln!("error: the code program could not be composed in this process: {e}");
                return 1;
            }
        },
        None => {
            tracing::info!(
                code_server = crate::hosted_code::CODE_SERVER,
                "daemon: no code program in this process; /mcp names the code server for code tools"
            );
            None
        }
    };
    let (code_tools, project_http, edit_door, code_yield, _code_runtime) = match code_mount {
        Some(m) => (
            Some(m.tools),
            m.routes,
            Some(m.edit_routes),
            Some(m.yield_to),
            Some(m.hold),
        ),
        None => (
            None,
            crate::hosted_code::projects_absent_router()
                .merge(crate::hosted_code::solve_absent_router()),
            None,
            None,
            None,
        ),
    };

    // ── Assemble the daemon's services, THEN commission the daemon ────
    //
    // Order is load-bearing and is the point of daemon-convergence Phase 2:
    // every dependency is built first and handed over in one total value, so
    // there is no window in which a request can reach a half-wired daemon and
    // no slot this bootstrap can forget. `DeferredDaemon` breaks the one
    // genuine cycle — the daemon serves peers through a provider that routes
    // to peers — and carries no capability of its own.
    // Ranked by serve's router where the distribution composed one; svrn
    // alone relays (pb-serve-ranks, `ServingBoot::ranked`).
    let routed_provider: Arc<dyn InferenceProvider> = Arc::clone(&ranked.provider);

    if let Some(port) = &ingest_port {
        bootstrap::spawn_vector_index_readiness_sweep(Arc::clone(port));
    }

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
        arm_geometry(info.dimensions);
    }

    // The landscape-digest surface. The Reindexer and `/v1/projects/*` that
    // were built beside it are the code program's (`code_mount` above).
    let knowledge_view_http = bootstrap::build_knowledge_view_http(
        &data_dir,
        ingest_port.clone(),
        state_store_concrete.clone(),
    )
    .await;

    match &ingest_port {
        Some(port) => {
            super::corpus_registry::reconcile_corpus_registry(port.as_ref(), state_store.as_ref())
                .await
        }
        None => tracing::info!("daemon: no ingest program; the corpus registry is not reconciled"),
    }

    // The watched-folder singleton must be installed before the daemon starts
    // serving, but the ROUTE is now part of the daemon's declared capability
    // rather than something this call installs — so a failed subsystem yields
    // handlers that answer 503 with a named reason, not routes that 404.
    let _watched_subsystem = bootstrap::setup_watched_folders(
        ingest_ports.clone(),
        Arc::clone(&state_store),
        &data_dir,
        &config,
        folder_tiered_deps,
        enrich_config.clone(),
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

    // features.db — the recipe-author project layer, composed with ingest
    // above. Warn-and-skip on failure, the same graceful-degrade posture the
    // desktop's bootstrap takes: the daemon still serves turns without it,
    // and the authoring tools report their own named degradation. svrn
    // alone has no store and says so by name (pb-ingest-rehome-daemon).
    type Projects =
        Result<Arc<dyn sovereign_contracts::recipe::project::RecipeProjectPort>, String>;
    let (features_store, recipe_tools): (Projects, _) = match recipe_authoring {
        Some(authoring) => {
            let projects = match authoring.projects {
                Ok(port) => {
                    tracing::info!("daemon: recipe-author features.db opened");
                    Ok(port)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "daemon: features.db unavailable — recipe-author tooling will degrade"
                    );
                    Err(crate::features_http::NO_FEATURES_DB.to_string())
                }
            };
            (projects, Some(authoring.tools))
        }
        None => (
            Err(match ingest_ports {
                Some(_) => crate::hosted_ingest::NO_RECIPE_AUTHORING,
                None => crate::hosted_ingest::NO_RECIPE_PROJECTS,
            }
            .to_string()),
            None,
        ),
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
    let notes_port: Arc<dyn sovereign_contracts::notes::AgentNotes> = state_store_concrete.clone();
    let common = sovereign_runtime_recipe::common_parts(
        sovereign_runtime_recipe::RecipeInputs {
            inference: Arc::clone(&routed_provider),
            store: Arc::clone(&state_store),
            conv_tiered: Some(Arc::clone(&state_store_concrete)
                as Arc<dyn sovereign_core::conv_tiered::ConvTieredReader>),
            corpus_engine: ingest_port.clone().map(|port| port as _),
            atlas: ingest_atlas.clone(),
            enrich_config,
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
            // Derived ONCE, above, and handed to ingest's engine too. The
            // atlas embedding cache keys on it.
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
                        corpus_engine: ingest_port.clone().map(|port| port as _),
                        // The daemon opened this above; wiring it here is what
                        // gives `knowledge_lookup` its notes channel. It ran
                        // with that channel dark until 2026-08-26 while the
                        // desktop, which wired it by hand, did not.
                        note_store: Some(&notes_port),
                        // The distribution's posture decides (phase-b-87): a
                        // sealed one builds no egress client at all.
                        web: match posture.withheld() {
                            None => sovereign_tools::bundles::WebReach::Granted(
                                sovereign_core::egress::search_client()
                                    .expect("egress boundary search client build"),
                            ),
                            Some(why) => sovereign_tools::bundles::WebReach::Withheld(why),
                        },
                        // No operator switch on a daemon, and escalating to the
                        // open web without one is a decision nobody made.
                        escalation: sovereign_tools::bundles::WebEscalation::Disabled,
                    },
                );
                b.push(match (&ingest_ports, posture.withheld()) {
                    (_, Some(why)) => Box::new(sovereign_contracts::tool_bundle::Withheld::new(
                        "wikipedia",
                        why,
                    )),
                    (Some((port, atlas)), None) => {
                        Box::new(sovereign_tools::bundles::WikipediaTools::new(
                            Arc::clone(port) as _,
                            Arc::clone(atlas),
                        ))
                    }
                    (None, None) => Box::new(sovereign_contracts::tool_bundle::Withheld::new(
                        "wikipedia",
                        "no ingest program is composed in this process, and \
                         wikipedia_fetch reads its catalog corpus",
                    )),
                });
                // Recipe-authoring, the desktop's twin (rung 6 commit B): the
                // same bundle, ingest's since pb-ingest-rehome-daemon, which
                // ingest wired above with the SAME notes adapter + features
                // store — so a conversation tagged `recipe-author` has its
                // tool set whichever host answers. Absent stores are a
                // DEGRADATION the bundle's report names.
                b.push(match recipe_tools {
                    Some(recipe_authoring_bundle) => recipe_authoring_bundle,
                    None if ingest_ports.is_some() => {
                        Box::new(sovereign_contracts::tool_bundle::Withheld::new(
                            "recipe-authoring",
                            crate::hosted_ingest::NO_RECIPE_AUTHORING,
                        ))
                    }
                    None => Box::new(sovereign_contracts::tool_bundle::Withheld::new(
                        "recipe-authoring",
                        "no ingest program is composed in this process, and recipe \
                         testing runs ingest's pipeline",
                    )),
                });
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
            // This daemon's own rerank kind — the engine's slot or a rerank
            // compute child, reached through the routed provider — when the
            // serving assembly installed one. Never a second load of the GGUF.
            rerank: sovereign_contracts::rerank_kind::serves_rerank(routed_provider.as_ref())
                .then(|| Arc::clone(&routed_provider)),
            // The served NER kind's handle, the one the ingest paths hold.
            ner: gliner_raw.clone(),
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
    // A KEYED daemon (on-prem API keys, `crate::api_keys`): a conversation is
    // owned by the `{sub}:` prefix its key wrote, every key's grant is
    // `[retrieval] corpora`, and the mesh leg is not wired — it dials this
    // daemon's own `/v1/knowledge/search`, whose hits fold in past the
    // ceiling. `start_daemon` refuses a keyed store over an unkeyed Runtime.
    let keyed = crate::client_tokens::ClientTokenStore::load(Some(
        crate::client_tokens::client_tokens_dir(&data_dir),
    ))
    .is_keyed();
    let (corpus_principal, mesh_knowledge): (
        Arc<dyn sovereign_core::traits::PrincipalResolver>,
        _,
    ) = if keyed {
        tracing::info!(
            grant = ?config.retrieval.corpora,
            "daemon: API keys present — turns are owned by key and bounded by [retrieval] corpora; no mesh knowledge leg"
        );
        let grant = config.retrieval.corpora.clone();
        (Arc::new(crate::api_keys::KeyedOwners { grant }), None)
    } else {
        (
            Arc::new(crate::principal::LocalOwnerPrincipal),
            sovereign_turn_client::knowledge_client::daemon_knowledge_source(&format!(
                "http://127.0.0.1:{}",
                config.daemon.client_port
            )),
        )
    };
    let runtime = sovereign_runtime_recipe::commission(sovereign_core::RuntimeParts {
        sensitive_corpora,
        corpus_principal: Some(corpus_principal),
        mesh_knowledge,
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
                    corpus_engine: ingest_port.clone(),
                    atlas: ingest_atlas.clone(),
                    recipe_harness,
                    inference_provider: Arc::clone(&routed_provider),
                    local_inference: Some(Arc::clone(&ranked.service)),
                    // The gauge the router above was built with, so AppState
                    // holds the same counter the provider's guards write
                    // (`quality/DAEMON_CORE.md` §4.2 "Where an install slot
                    // breaks a cycle"); `None` where nothing ranks here.
                    in_flight_gauge: ranked.in_flight.clone(),
                    // Phase 3: the headless daemon's own `sovereign.db`,
                    // opened at the top of this function. `reading_http` now
                    // resolves conversation titles on this variant too.
                    state_store: Arc::clone(&state_store),
                    // Phase 5c: the thing that answers. Commissioned just
                    // above, from the one shared recipe.
                    runtime: Arc::clone(&runtime),
                    insights: Some(insight_service),
                    // sv-surface D6: the recipe-author `features.db` opened
                    // above. Already warn-and-skip, so the `Result` here says
                    // the same thing the log line did — and `features_http`
                    // now renders it as a named 503 instead of a route that
                    // silently is not there.
                    features: features_store,
                },
                capability: crate::ServingCapability {
                    mcp: bootstrap::build_mcp_surface(
                        tools,
                        Arc::clone(&state_store_concrete),
                        code_tools,
                    ),
                    posture,
                    project_http,
                    edit_door,
                    corpus_watch_http: crate::corpus_watch_http::corpus_watch_router(),
                    // sv-surface rung 5: workflow execution is a daemon job
                    // surface (`/internal/workflows/*`). The runtime routes
                    // `model:`/`embed:` steps back through this daemon's own
                    // loopback, and injects the corpus/atlas tools the base
                    // registry omits.
                    workflow_http: sovereign_workflow_host::workflow_http_router(
                        format!("http://127.0.0.1:{}", config.daemon.client_port),
                        {
                            let atlas = ingest_atlas.clone();
                            std::sync::Arc::new(move || match &atlas {
                                Some(atlas) => {
                                    sovereign_tools::workflow_corpus_tools(Arc::clone(atlas))
                                }
                                // Named at boot, beside `ingest_ports`.
                                None => Vec::new(),
                            })
                        },
                    ),
                },
                advertise_embed,
                // The node's mesh through cw-rails, composed above.
                mesh: mesh_access,
            },
            headless: Some(crate::HeadlessExtras {
                rails: crate::HeadlessRails {
                    // Rebuilds the provider when `models.*` changes on disk. Holds
                    // the same deferred handle, bound below.
                    provider_factory: Arc::new(crate::provider::LlamaCppFactory {
                        daemon: Arc::clone(&deferred_daemon),
                        reload,
                        routed: Arc::clone(&routed_provider),
                        slot_aliases: ranked.slot_aliases.clone(),
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

    bootstrap::spawn_slot_alias_push(Arc::clone(&daemon), ranked.slot_aliases);

    // ── Start: svrn holds no mesh; cw-rails is the node's endpoint ───
    if let Some(exit_code) = start(&daemon, &config).await {
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

    bootstrap::install_foreground_yield_hook(Arc::clone(&daemon), code_yield);

    eprintln!(
        "svrn daemon running — http://localhost:{}/v1 + /mcp",
        config.daemon.client_port
    );

    let (pid_path, self_pid) = bootstrap::write_pidfile();

    // ── Block until SIGINT/SIGTERM, then drain, persist, and exit ──
    shutdown_daemon(daemon, &pid_path, self_pid).await
}
