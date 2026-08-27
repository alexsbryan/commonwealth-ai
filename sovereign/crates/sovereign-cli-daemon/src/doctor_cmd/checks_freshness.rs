// SPDX-License-Identifier: AGPL-3.0-or-later
//! The freshness pipeline of `svrn doctor` — watchers, SCIP integrity, index
//! orphans and rebuild outcomes.
//!
//! Its own layer because these checks answer one question the other three do
//! not: is what the tools READ still the code on disk? A green server serving
//! a stale graph passes every other check in this command.

use std::time::Duration;

use sovereign_cli_shared::dirs::sovereign_root;

use super::probe::http_get_json;
use super::{CheckResult, CheckStatus, Layer, Repair};

// ── Freshness-pipeline checks ────────────────────────────────

/// Query the daemon's `/v1/projects` endpoint and surface the
/// aggregate watcher health. Passes when every registered project
/// has all its watchers healthy; downgrades to Warning when any
/// watcher is Crashed; Failed when any watcher is Disabled (the
/// daemon has given up auto-restarting and needs operator action).
pub(super) async fn check_project_watchers() -> CheckResult {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            return CheckResult {
                name: "project_watchers",
                layer: Layer::Sovereign,
                status: CheckStatus::Warning,
                message: "could not build HTTP client".into(),
                repair: Repair::None,
            };
        }
    };

    let resp = match client.get("http://127.0.0.1:9741/v1/projects").send().await {
        Ok(r) => r,
        Err(_) => {
            return CheckResult {
                name: "project_watchers",
                layer: Layer::Sovereign,
                status: CheckStatus::Warning,
                message: "daemon unreachable — /v1/projects did not answer".into(),
                repair: Repair::executable("svrn daemon restart"),
            };
        }
    };

    if resp.status().as_u16() == 404 {
        return CheckResult {
            name: "project_watchers",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message:
                "/v1/projects returned 404 — project_http_router not mounted (restart the daemon)"
                    .into(),
            repair: Repair::executable("svrn daemon restart"),
        };
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => {
            return CheckResult {
                name: "project_watchers",
                layer: Layer::Sovereign,
                status: CheckStatus::Warning,
                message: "/v1/projects returned unexpected shape".into(),
                repair: Repair::Manual("inspect daemon logs".into()),
            };
        }
    };

    let Some(projects) = body["projects"].as_array() else {
        return CheckResult {
            name: "project_watchers",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "/v1/projects returned unexpected shape".into(),
            repair: Repair::Manual("inspect daemon logs".into()),
        };
    };
    if projects.is_empty() {
        return CheckResult {
            name: "project_watchers",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "daemon is running but no projects registered".into(),
            repair: Repair::Manual(
                "cd <repo-root> && svrn project register  (run from each repo root)".into(),
            ),
        };
    }

    let mut crashed: Vec<String> = Vec::new();
    let mut disabled: Vec<String> = Vec::new();
    let mut active_ok = 0usize;
    for p in projects {
        let id = p["corpus_id"].as_str().unwrap_or("?");
        let Some(status) = p["status"].as_object() else {
            continue;
        };
        for (kind, s) in status {
            let state = s["state"].as_str().unwrap_or("?");
            match state {
                "idle" | "active" | "pending" => active_ok += 1,
                "crashed" => crashed.push(format!("{id}:{kind}")),
                "disabled" => disabled.push(format!("{id}:{kind}")),
                _ => {}
            }
        }
    }

    if !disabled.is_empty() {
        return CheckResult {
            name: "project_watchers",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: format!(
                "{} watcher(s) disabled after repeated crashes: {}",
                disabled.len(),
                disabled.join(", ")
            ),
            repair: Repair::Manual("svrn project watch restart <corpus_id>".into()),
        };
    }
    if !crashed.is_empty() {
        return CheckResult {
            name: "project_watchers",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: format!(
                "{} watcher(s) crashed but auto-restarting: {}",
                crashed.len(),
                crashed.join(", ")
            ),
            repair: Repair::Manual(
                "tail ~/.svrnmesh/logs/watch-<id>-<watcher>.log for details".into(),
            ),
        };
    }
    CheckResult {
        name: "project_watchers",
        layer: Layer::Sovereign,
        status: CheckStatus::Passed,
        message: format!(
            "{} project(s), {} watcher(s) healthy",
            projects.len(),
            active_ok
        ),
        repair: Repair::None,
    }
}

/// Run `PRAGMA integrity_check` (via `ScipGraph::open_with_integrity`)
/// on every per-corpus `scip_graph.db`. A corrupt DB is quarantined
/// by `open_with_integrity` as a side effect; we surface that to
/// the operator so they can trigger an immediate rebuild. This is
/// the doctor-level complement of the daemon's automatic
/// quarantine-on-open behaviour.
pub(super) fn check_scip_integrity() -> CheckResult {
    let indexes_dir = sovereign_root().join("indexes");
    let Ok(entries) = std::fs::read_dir(&indexes_dir) else {
        return CheckResult {
            name: "scip_integrity",
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: format!("no indexes dir at {}", indexes_dir.display()),
            repair: Repair::None,
        };
    };

    let mut checked = 0usize;
    let mut corrupt: Vec<String> = Vec::new();
    let mut stale_schema: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let db = entry.path().join("scip_graph.db");
        if !db.exists() {
            continue;
        }
        let corpus = entry.file_name().to_string_lossy().to_string();
        match corpus_engine_scip::ScipGraph::open_with_integrity(&db, &corpus) {
            Ok(_) => {
                checked += 1;
            }
            Err(corpus_engine_scip::OpenError::Corrupt { .. }) => {
                corrupt.push(corpus);
            }
            Err(corpus_engine_scip::OpenError::SchemaMismatch { .. }) => {
                stale_schema.push(corpus);
            }
            Err(_) => {
                // Other errors (IO, transient DB issues) shouldn't
                // trip the integrity check — the daemon will log
                // them on its next open.
            }
        }
    }

    if !corrupt.is_empty() {
        return CheckResult {
            name: "scip_integrity",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: format!(
                "{} SCIP DB(s) were corrupt and moved aside: {}",
                corrupt.len(),
                corrupt.join(", ")
            ),
            repair: Repair::MultiExecutable(
                corrupt
                    .iter()
                    .map(|id| format!("svrn project refresh --name {id} --local"))
                    .collect(),
            ),
        };
    }
    if !stale_schema.is_empty() {
        return CheckResult {
            name: "scip_integrity",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: format!(
                "{} SCIP DB(s) have an outdated schema: {}",
                stale_schema.len(),
                stale_schema.join(", ")
            ),
            repair: Repair::MultiExecutable(
                stale_schema
                    .iter()
                    .map(|id| format!("svrn project refresh --name {id} --local"))
                    .collect(),
            ),
        };
    }
    CheckResult {
        name: "scip_integrity",
        layer: Layer::Sovereign,
        status: CheckStatus::Passed,
        message: format!("{checked} SCIP DB(s) integrity OK"),
        repair: Repair::None,
    }
}

/// Per-project snapshot of "is a rebuild trying right now" pulled
/// from `/v1/projects`. The freshness check uses this to tell apart
/// two failure modes that look identical from disk state alone:
///
/// - **Wedged**: source-tree mtime is past `last_export_at`, AND no
///   rebuild is running. The watcher fired nothing in response to
///   recent edits.
///
/// - **Slow rebuild**: source-tree mtime is past `last_export_at`,
///   AND a rebuild IS in flight. The watcher fired correctly — the
///   exporter is the slow link (or stuck on the cargo target lock,
///   or hung in rust-analyzer).
///
/// Without `rebuild_in_flight`, a single Failed message would fire
/// every time you save during the debounce-plus-rebuild window
/// (~2.5 min on this monorepo), which would make the check
/// effectively useless.
#[derive(Debug, Default)]
pub(super) struct ProjectLiveness {
    rebuild_in_flight: bool,
}

pub(super) async fn fetch_project_liveness() -> std::collections::HashMap<String, ProjectLiveness> {
    let mut out = std::collections::HashMap::new();
    let v = match http_get_json("http://127.0.0.1:9741/v1/projects").await {
        Some(v) => v,
        None => return out,
    };
    let projects = match v.get("projects").and_then(|p| p.as_array()) {
        Some(arr) => arr,
        None => return out,
    };
    for proj in projects {
        let Some(id) = proj.get("corpus_id").and_then(|s| s.as_str()) else {
            continue;
        };
        let rebuild_in_flight = proj
            .get("rebuild_in_flight")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        out.insert(id.to_string(), ProjectLiveness { rebuild_in_flight });
    }
    out
}

/// Check that each registered project's SCIP graph reflects the
/// current state of its source tree. Three independent signals:
///
/// 1. `last_indexed_head` vs `git rev-parse HEAD`: catches a
///    watcher that's behind by one or more commits.
///
/// 2. `export_age_secs` vs the newest source-file mtime under the
///    project root: catches a wedged watcher that hasn't picked up
///    *uncommitted* edits. The newest mtime walk is bounded by the
///    same exclusions the FS watcher applies (`target/`, `.git/`,
///    `node_modules/`, the per-project `ignore_paths`).
///
/// 3. `rebuild_in_flight` from `/v1/projects`: when signals 1 or 2
///    trip, this tells us *why*. A rebuild that's running (just
///    slow) gets a Warning; no-rebuild-firing gets a Failed.
///
/// Pairs with `scip_indexed` (row count) and `scip_exporters`
/// (toolchain). Together they answer the "is the SCIP graph
/// honestly reflecting reality?" question that the byte-threshold
/// version of `scip_indexed` was lying about.
/// Corpus ids under `~/.svrnmesh/indexes` that carry a SCIP graph — i.e.
/// things a project watcher OUGHT to be maintaining. Mirrors the daemon's
/// `warn_orphaned_indexes` scan; used to tell "nothing registered because
/// there's nothing to register" apart from "nothing registered and four
/// code indexes are quietly rotting".
pub(super) fn orphaned_index_ids() -> Vec<String> {
    let mut ids: Vec<String> = orphaned_indexes().into_iter().map(|(id, _)| id).collect();
    ids.sort();
    ids
}

/// The same scan, paired with each corpus's originating repo root.
///
/// The daemon's `warn_orphaned_indexes` says it "can't safely auto-register
/// those — we don't know which filesystem path each one came from". For a
/// code corpus we usually DO know: the code-ingest pipeline stamps
/// `source_path` into `_corpus_meta.json` (`CorpusIndex::set_source_path`),
/// which is precisely the repo root `project register --root` wants. That
/// turns doctor's advice from "go figure out the path" into an exact,
/// copy-pasteable command.
pub(super) fn orphaned_indexes() -> Vec<(String, Option<String>)> {
    let indexes_dir = sovereign_root().join("indexes");
    let Ok(entries) = std::fs::read_dir(&indexes_dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Option<String>)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter(|e| e.path().join("scip_graph.db").exists())
        .filter_map(|e| {
            let id = e.file_name().to_str()?.to_string();
            let root = std::fs::read_to_string(corpus_engine::Corpus::meta_in(e.path()))
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v.get("source_path")?.as_str().map(str::to_string));
            Some((id, root))
        })
        .collect();
    out.sort();
    out
}

/// Do the code tools actually SEE any corpus?
///
/// Every other code check verifies an ingredient — the SCIP db exists,
/// `CorpusIndex::open` succeeds — and all of them passed on 2026-07-24
/// while `code_search` returned "0 code corpora" on a healthy 36k-chunk
/// index. The corpus was screened out one layer further in, by a
/// `kind == CorpusKind::Code` filter that repo corpora deliberately don't
/// satisfy. No amount of ingredient-checking catches that; only asking the
/// question the tool asks does.
///
/// So this check runs the REAL predicate the code tools run
/// (`sovereign_tools::code::has_code_graph`) over the REAL corpus list and
/// reports the count. If a corpus has a graph but the tools would skip it,
/// that discrepancy IS the finding.
pub(super) async fn check_code_tools_see_corpora() -> CheckResult {
    let name = "code_tools_visibility";
    let on_disk = orphaned_index_ids();
    if on_disk.is_empty() {
        return CheckResult {
            name,
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: "no code corpora indexed".into(),
            repair: Repair::None,
        };
    }

    // Open each index and ask the REAL predicate about the REAL `IndexInfo`
    // — same `kind` derivation, same `has_code_graph` rule the tools use. No
    // corpus engine (and so no EmbedFn) is needed to answer this.
    let indexes_dir = sovereign_root().join("indexes");
    let mut visible: Vec<String> = Vec::new();
    for id in &on_disk {
        let Ok(index) = corpus_engine::CorpusIndex::open(&indexes_dir.join(id)).await else {
            // `code_indexed` already reports an unreadable Lance table.
            continue;
        };
        let Ok(info) = index.info().await else {
            continue;
        };
        if sovereign_tools::code::has_code_graph(&info) {
            visible.push(info.corpus_id);
        }
    }

    if visible.is_empty() {
        return CheckResult {
            name,
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: format!(
                "{} code corpus(es) on disk ({}) but the code tools can see NONE \
                 of them — symbols / code_search / recent_changes will return \
                 empty while every other check reports green.",
                on_disk.len(),
                on_disk.join(", ")
            ),
            repair: Repair::Manual(
                "A corpus is visible to the code tools when it is tagged \
                 CorpusKind::Code OR has a scip_graph.db beside its chunk table \
                 (sovereign_tools::code::has_code_graph). Check that the index \
                 dir and _corpus_meta.json agree."
                    .into(),
            ),
        };
    }

    CheckResult {
        name,
        layer: Layer::Sovereign,
        status: CheckStatus::Passed,
        message: format!(
            "code tools can see {} corpus(es): {}",
            visible.len(),
            visible.join(", ")
        ),
        repair: Repair::None,
    }
}

/// Assert on OBSERVED rebuild outcomes, not on prerequisites. The lesson of
/// `code_tools_visibility` applies verbatim: `scip_exporters` can pass in the
/// CLI's shell while the DAEMON's environment cannot resolve a single
/// exporter — which held for a full day on 2026-08-06 (launchd's minimal
/// PATH): every 30s git-poll rebuild exported 0 symbols, the wipe guard
/// preserved the live graph, and every surface stayed green while the graph
/// froze 29 commits behind HEAD. The reindexer now writes the latest failed
/// outcome into the live graph's `scip_meta`; this check reads it from disk,
/// so it works whether or not the daemon is up.
pub(super) async fn check_rebuild_outcomes() -> CheckResult {
    let name = "scip_rebuild_outcomes";
    let registry = match sovereign_mesh::projects::Registry::load() {
        Ok(r) => r,
        Err(_) => {
            return CheckResult {
                name,
                layer: Layer::Sovereign,
                status: CheckStatus::Skipped,
                message: "project registry not loadable".into(),
                repair: Repair::None,
            };
        }
    };
    let indexes_dir = sovereign_root().join("indexes");
    let mut failing: Vec<String> = Vec::new();
    let mut healthy = 0usize;
    for entry in registry.entries() {
        let db = indexes_dir.join(&entry.corpus_id).join("scip_graph.db");
        if !db.exists() {
            continue; // `scip_indexed` owns "never exported"
        }
        let Ok(graph) = corpus_engine_scip::ScipGraph::open(&db, &entry.corpus_id) else {
            continue; // `scip_integrity` owns corruption
        };
        match graph.last_rebuild_failure().await {
            Some((err, at)) => failing.push(format!("{}: {err} (at {at})", entry.corpus_id)),
            None => healthy += 1,
        }
    }
    if failing.is_empty() {
        return CheckResult {
            name,
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: format!("last rebuild succeeded for {healthy} project(s)"),
            repair: Repair::None,
        };
    }
    CheckResult {
        name,
        layer: Layer::Sovereign,
        status: CheckStatus::Failed,
        message: format!(
            "latest SCIP rebuild FAILED for {} project(s) — each graph is frozen at its \
             last indexed commit and drifts further every commit: {}. If the error names \
             missing exporters, the DAEMON's environment (not this shell's) cannot \
             resolve them — re-run `svrn install-service` from a shell where they \
             resolve, then restart the daemon.",
            failing.len(),
            failing.join("; ")
        ),
        repair: Repair::Manual(
            "svrn project watch status  (live failure counts) · svrn install-service  \
             (recapture PATH) · svrn daemon restart"
                .into(),
        ),
    }
}

pub(super) async fn check_watcher_freshness() -> CheckResult {
    let registry = match sovereign_mesh::projects::Registry::load() {
        Ok(r) => r,
        Err(_) => {
            return CheckResult {
                name: "watcher_freshness",
                layer: Layer::Sovereign,
                status: CheckStatus::Skipped,
                message: "project registry not loadable".into(),
                repair: Repair::None,
            };
        }
    };
    let entries = registry.entries();
    if entries.is_empty() {
        // An empty registry is only a non-event when there is nothing to
        // keep fresh. If code indexes exist on disk, this is the ORPHAN
        // state and it is the single most misleading condition doctor can
        // encounter: freshness is owned by the Reindexer, which builds one
        // ProjectHandle per REGISTERED project, so zero registrations means
        // zero watchers — while `scip_indexed` / `code_indexed` keep
        // reporting green off the stale files sitting right there, and
        // `svrn status` prints ✓/✓ for both.
        //
        // Reported as Skipped ("no projects registered"), it rendered as a
        // neutral dash and cost a full debugging session on 2026-07-24 to
        // rediscover by hand — on a box whose chunk index was 27 days old
        // and whose call graph was 11. The daemon already logs this via
        // `warn_orphaned_indexes`, but into a log nobody reads. Fail loudly
        // instead; the fix is one command.
        let orphans = orphaned_indexes();
        if !orphans.is_empty() {
            let ids: Vec<&str> = orphans.iter().map(|(id, _)| id.as_str()).collect();
            // `source_path` gives an exact command; without it the operator
            // has to supply the root, so say that rather than guess.
            let repairs: Vec<String> = orphans
                .iter()
                .map(|(id, root)| match root {
                    Some(r) => format!("svrn project register --root {r} --name {id}"),
                    None => format!("svrn project register --root <repo> --name {id}"),
                })
                .collect();
            return CheckResult {
                name: "watcher_freshness",
                layer: Layer::Sovereign,
                status: CheckStatus::Failed,
                message: format!(
                    "NO projects registered, but {} code index(es) exist on disk \
                     ({}). Nothing is watching them: no FS watcher, no git-HEAD \
                     poll, no rebuild queue. The index/call-graph checks above \
                     pass off whatever was last built BY HAND and will keep \
                     passing as the code drifts away from them.",
                    orphans.len(),
                    ids.join(", ")
                ),
                repair: Repair::MultiExecutable(repairs),
            };
        }
        return CheckResult {
            name: "watcher_freshness",
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: "no projects registered (and no code indexes to watch)".into(),
            repair: Repair::None,
        };
    }

    let indexes_dir = sovereign_root().join("indexes");
    // Allow this many seconds between source mtime and export time
    // before we flag. Covers normal debounce (2s) + worker latency +
    // the export run itself. Anything past this is a real wedge.
    const FRESHNESS_GRACE_SECS: u64 = 60;
    // Hard cap on the source-tree walk so a misconfigured ignore list
    // can't tip doctor into a multi-minute traversal of a model-checkpoint
    // directory. 25k files is well above any realistic source corpus.
    const WALK_FILE_BUDGET: usize = 25_000;

    // Liveness from /v1/projects. Empty when the daemon isn't reachable;
    // in that case we degrade gracefully — all source-newer-than-export
    // findings render as Failed, since we can't tell apart "wedged" from
    // "rebuild trying".
    let liveness = fetch_project_liveness().await;

    let mut behind_head: Vec<String> = Vec::new();
    let mut wedged: Vec<String> = Vec::new();
    let mut slow_rebuild: Vec<String> = Vec::new();
    let mut healthy: Vec<String> = Vec::new();

    for entry in entries {
        let corpus_id = entry.corpus_id.clone();
        let db = indexes_dir.join(&corpus_id).join("scip_graph.db");
        if !db.exists() {
            // `scip_indexed` already covers the missing-DB case; don't double-fail.
            continue;
        }
        let graph = match corpus_engine_scip::ScipGraph::open(&db, &corpus_id) {
            Ok(g) => g,
            Err(_) => continue,
        };

        // Signal 1: git-head drift. Surfaces a watcher that's a full
        // commit behind. Skip silently when the project isn't a git
        // repo (no HEAD to compare against).
        let current_head = sovereign_mesh::reindexer::read_git_head(&entry.root);
        let indexed_head = graph.last_indexed_head().await;
        if let (Some(cur), Some(idx)) = (current_head.as_ref(), indexed_head.as_ref()) {
            if cur != idx {
                behind_head.push(format!(
                    "{corpus_id}: indexed {short_idx}, HEAD {short_cur}",
                    short_idx = &idx[..idx.len().min(8)],
                    short_cur = &cur[..cur.len().min(8)],
                ));
                continue;
            }
        }

        // Signal 2: source-tree mtime past last export. Walks the
        // project root for the newest source-extension mtime, then
        // compares it to last_export_at.
        let export_age = match graph.export_age_secs().await {
            Some(a) => a,
            None => continue, // no export recorded — `scip_indexed` covers
        };
        let newest_source_age =
            newest_source_age_secs(&entry.root, &entry.watchers.ignore_paths, WALK_FILE_BUDGET);
        match newest_source_age {
            Some(src_age) if src_age + FRESHNESS_GRACE_SECS < export_age => {
                // Source is meaningfully newer than the last completed export.
                // Now ask the daemon: is a rebuild trying right now? If yes,
                // we're not wedged — just slow. If no, the watcher missed it.
                let in_flight = liveness
                    .get(&corpus_id)
                    .map(|l| l.rebuild_in_flight)
                    .unwrap_or(false);
                if in_flight {
                    slow_rebuild.push(format!(
                        "{corpus_id}: rebuild in flight, last successful index {export_age}s ago, source edit {src_age}s ago"
                    ));
                } else {
                    wedged.push(format!(
                        "{corpus_id}: newest source edit {src_age}s ago, last index {export_age}s ago, no rebuild in flight"
                    ));
                }
            }
            _ => {
                healthy.push(corpus_id);
            }
        }
    }

    // Status precedence: Failed (wedged or behind-HEAD) > Warning
    // (slow rebuild) > Passed. A single CheckResult collapses
    // multi-project state to the worst observed level, with all
    // findings listed in the message so nothing's hidden.
    if !behind_head.is_empty() {
        return CheckResult {
            name: "watcher_freshness",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: format!(
                "{} project(s) have a SCIP graph behind git HEAD: {}. \
                 The watcher hasn't rebuilt since a recent commit — check daemon.err for \
                 SCIP export failures or a stuck rebuild lock.",
                behind_head.len(),
                behind_head.join("; ")
            ),
            repair: Repair::MultiExecutable(
                registry
                    .entries()
                    .iter()
                    .map(|e| format!("svrn project refresh --name {} --local", e.corpus_id))
                    .collect(),
            ),
        };
    }
    if !wedged.is_empty() {
        return CheckResult {
            name: "watcher_freshness",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: format!(
                "{} project(s) have uncommitted source edits the watcher hasn't picked up: {}. \
                 fs_change events may not be reaching the reindexer — check daemon.err for \
                 `notify` errors or nudge with `svrn project refresh`.",
                wedged.len(),
                wedged.join("; ")
            ),
            repair: Repair::executable("svrn project refresh"),
        };
    }
    if !slow_rebuild.is_empty() {
        return CheckResult {
            name: "watcher_freshness",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: format!(
                "{} project(s) have a SCIP rebuild in flight but the source has moved past \
                 the last successful index: {}. The watcher is firing correctly — the exporter \
                 is slow (rust-analyzer often blocks on the cargo target lock during a release \
                 build, or stalls on macro-heavy crates). Wait it out, then re-run doctor.",
                slow_rebuild.len(),
                slow_rebuild.join("; ")
            ),
            repair: Repair::None,
        };
    }
    if healthy.is_empty() {
        return CheckResult {
            name: "watcher_freshness",
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: "no projects had enough state to evaluate freshness".into(),
            repair: Repair::None,
        };
    }
    CheckResult {
        name: "watcher_freshness",
        layer: Layer::Sovereign,
        status: CheckStatus::Passed,
        message: format!(
            "SCIP graph current for {}: {}",
            healthy.len(),
            healthy.join(", ")
        ),
        repair: Repair::None,
    }
}

/// Walk the project root and return the seconds-since-last-modified
/// of the newest source file. Mirrors the watcher's `is_source_event`
/// + `IgnoreFilter` logic at directory granularity so the answer
/// reflects what the watcher *would* have seen.
///
/// Bounded by `file_budget` to keep doctor latency predictable on
/// any tree shape. Returns `None` when the root is unreadable, has
/// no source files within the budget, or when system clock skew
/// makes mtimes nonsensical.
pub(super) fn newest_source_age_secs(
    root: &std::path::Path,
    extra_ignores: &[String],
    file_budget: usize,
) -> Option<u64> {
    use std::time::SystemTime;

    const UNIVERSAL_IGNORES: &[&str] = &[
        ".git",
        "node_modules",
        "target",
        "dist",
        "build",
        ".cache",
        ".next",
        "__pycache__",
        ".venv",
        "venv",
    ];
    const SOURCE_EXTS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java",
    ];

    let now = SystemTime::now();
    let mut newest: Option<u64> = None;
    let mut visited = 0usize;
    let mut stack: Vec<std::path::PathBuf> = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if visited >= file_budget {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if visited >= file_budget {
                break;
            }
            let path = entry.path();
            let file_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if UNIVERSAL_IGNORES.contains(&file_name.as_str())
                || extra_ignores.iter().any(|e| e == &file_name)
            {
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            visited += 1;
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !SOURCE_EXTS.contains(&ext.as_str()) {
                continue;
            }
            let mtime = match entry.metadata().ok().and_then(|m| m.modified().ok()) {
                Some(m) => m,
                None => continue,
            };
            let age_secs = now.duration_since(mtime).ok()?.as_secs();
            newest = Some(match newest {
                Some(prev) => prev.min(age_secs),
                None => age_secs,
            });
        }
    }
    newest
}

/// Verify that, for every registered project, the SCIP exporter
/// binaries needed by the languages present in its workspace can be
/// resolved. Language-agnostic: it consults
/// `corpus_engine_scip::scip_export::check_exporters`, which iterates
/// every registered exporter (rust-analyzer, scip-typescript,
/// scip-python, scip-go, scip-java) and reports those that are
/// needed but absent. Pairs with `scip_indexed` (row count): an
/// empty graph plus a missing exporter localises the failure.
///
/// **This check runs in the CLI's process but is answering a question
/// about the DAEMON's.** It shares one resolver with the exporter
/// spawn (`corpus_engine_scip::tool_path`), so it can no longer pass
/// here while the daemon fails — that divergence is what let a
/// launchd daemon fail every rebuild for ten days while doctor,
/// run from an interactive shell, reported the same exporters
/// present. Where the two environments could still disagree — a tool
/// found ONLY via this process's PATH, in a directory the shared
/// probe does not search — the verdict is a Warning naming the
/// directory, never a pass.
pub(super) fn check_scip_exporters() -> CheckResult {
    let registry = match sovereign_mesh::projects::Registry::load() {
        Ok(r) => r,
        Err(_) => {
            return CheckResult {
                name: "scip_exporters",
                layer: Layer::Sovereign,
                status: CheckStatus::Skipped,
                message: "project registry not loadable".into(),
                repair: Repair::None,
            };
        }
    };

    let entries = registry.entries();
    if entries.is_empty() {
        return CheckResult {
            name: "scip_exporters",
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: "no projects registered".into(),
            repair: Repair::None,
        };
    }

    // Aggregate workspace roots across every registered project so
    // we ask `check_exporters` once. The function dedupes via its
    // exporter loop; per-project ordering doesn't matter here.
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    for entry in entries {
        let root = entry.root.clone();
        if !root.exists() {
            continue;
        }
        let detected = corpus_engine_scip::scip_export::find_cargo_workspace_roots(&root);
        if detected.is_empty() {
            roots.push(root);
        } else {
            roots.extend(detected);
        }
    }
    roots.sort();
    roots.dedup();

    if roots.is_empty() {
        return CheckResult {
            name: "scip_exporters",
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: "no reachable project roots".into(),
            repair: Repair::None,
        };
    }

    let check = corpus_engine_scip::scip_export::check_exporters(&roots);
    if !check.missing.is_empty() {
        let summary = check
            .missing
            .iter()
            .map(|m| format!("{} ({})", m.language_id, m.command))
            .collect::<Vec<_>>()
            .join(", ");
        let hints: Vec<String> = check
            .missing
            .iter()
            .map(|m| format!("{}: {}", m.language_id, m.install_hint))
            .collect();
        return CheckResult {
            name: "scip_exporters",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: format!(
                "{} SCIP exporter(s) referenced by registered projects could not be resolved: \
                 {summary}. Calling these is what populates the SCIP graph — when they fail \
                 silently the graph stays empty and call-graph tools return nothing.",
                check.missing.len(),
            ),
            // Manual, NOT MultiExecutable: these strings are install
            // HINTS ("python: Install with: pip install scip-python"),
            // not commands. As MultiExecutable, `doctor --fix` tried to
            // exec the prose verbatim and reported the resulting
            // not-found as the repair having run.
            repair: Repair::Manual(hints.join("\n")),
        };
    }

    // Resolved, but only through THIS process's PATH — i.e. the
    // operator's shell. The daemon runs under launchd/systemd with a
    // different environment, so a pass here does not prove the daemon
    // can spawn it. Report the doubt rather than the green
    // (ARCH §18.1: four verdicts, not two).
    // Probed once, not per exporter: `well_known_tool_dirs` stats every
    // candidate dir and read_dir's the versioned ones.
    let well_known = corpus_engine_scip::tool_path::well_known_tool_dirs();
    let shell_only: Vec<String> = check
        .available
        .iter()
        .filter(|e| e.via == corpus_engine_scip::tool_path::ResolvedVia::ProcessPath)
        .filter(|e| {
            !e.path
                .parent()
                .is_some_and(|dir| well_known.iter().any(|w| w == dir))
        })
        .map(|e| format!("{} ({})", e.config.language_id, e.path.display()))
        .collect();

    let available = check
        .available
        .iter()
        .map(|e| format!("{} ({})", e.config.language_id, e.path.display()))
        .collect::<Vec<_>>()
        .join(", ");

    if !shell_only.is_empty() {
        return CheckResult {
            name: "scip_exporters",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: format!(
                "{} SCIP exporter(s) resolve only through this shell's PATH, from a directory \
                 the daemon's probe does not search: {}. The daemon may be unable to spawn \
                 them, which looks exactly like a healthy index that never populates. All \
                 resolved: {available}",
                shell_only.len(),
                shell_only.join(", "),
            ),
            repair: Repair::Manual(
                "Symlink each into ~/.local/bin (searched by both), or re-run \
                 `svrn install-service` from this shell so the unit captures this PATH."
                    .into(),
            ),
        };
    }

    CheckResult {
        name: "scip_exporters",
        layer: Layer::Sovereign,
        status: CheckStatus::Passed,
        message: if available.is_empty() {
            "no language exporters needed for registered projects".into()
        } else {
            format!("SCIP exporters resolved for: {available}")
        },
        repair: Repair::None,
    }
}

/// Scan registered project roots for legacy `SOVEREIGN_HOOK_V*`
/// post-commit hooks. The daemon owns freshness now, so any
/// surviving hook is a ticking footgun (stale binary path, silent
/// failures into `~/.svrnmesh/hooks.log`). Surface them with a
/// one-shot cleanup hint.
pub(super) fn check_legacy_hooks() -> CheckResult {
    // Best-effort: read the registry directly. If the registry
    // isn't loadable, skip this check — the `warn_orphaned_indexes`
    // path at daemon startup will cover the miss.
    let registry = match sovereign_mesh::projects::Registry::load() {
        Ok(r) => r,
        Err(_) => {
            return CheckResult {
                name: "legacy_hooks",
                layer: Layer::Sovereign,
                status: CheckStatus::Skipped,
                message: "project registry not loadable".into(),
                repair: Repair::None,
            };
        }
    };
    let mut stale: Vec<String> = Vec::new();
    for entry in registry.entries() {
        let hook = entry.root.join(".git/hooks/post-commit");
        let Ok(contents) = std::fs::read_to_string(&hook) else {
            continue;
        };
        if contents.contains("SOVEREIGN_HOOK_V") {
            stale.push(entry.corpus_id.clone());
        }
    }
    if stale.is_empty() {
        return CheckResult {
            name: "legacy_hooks",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "no legacy post-commit hooks found".into(),
            repair: Repair::None,
        };
    }
    CheckResult {
        name: "legacy_hooks",
        layer: Layer::Sovereign,
        status: CheckStatus::Warning,
        message: format!(
            "{} project(s) still carry a legacy sovereign post-commit hook: {}",
            stale.len(),
            stale.join(", ")
        ),
        repair: Repair::executable(
            "svrn project install-hooks  (in the affected repo — removes the legacy hook)",
        ),
    }
}
