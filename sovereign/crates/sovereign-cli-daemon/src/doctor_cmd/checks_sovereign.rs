// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Sovereign layer of `svrn doctor` — this host's own server, indexes,
//! config and runners. One `check_*` per question, each returning a
//! [`CheckResult`] carrying its own [`Repair`]; the layer split is the one
//! the module doc in `mod.rs` has always declared.

use sovereign_cli_shared::dirs::sovereign_root;

use super::probe::{http_get_json, http_post_json, tcp_connectable};
use super::{registered_root, CheckResult, CheckStatus, Layer, Repair};

// ── Checks ────────────────────────────────────────────────────────────────────

/// Does the embed slot actually EMBED?
///
/// Liveness is not readiness. On 2026-08-26 this daemon's embed slot returned
/// `Embed decode failed: Decode Error -3: unknown` for roughly five hours
/// while `/v1/models` listed it and `/status` was green — so every check we
/// had said "up". Downstream, silently: the router's exemplar re-embed failed,
/// all four classifiers came back `None`, atlas grounding fell from 1082 loads
/// to zero, and turns kept answering — worse. Measured cost on SEP overview
/// questions: title-coverage 1.00 -> 0.83 (note `f4972e1b`).
///
/// So this check asks for a vector and looks at it. A slot that accepts the
/// request and returns an error body is DOWN, and nothing else we run can see
/// that (ARCH §18.1: a gate you have not watched fail is not a gate — this one
/// was watched failing in production before it was written).
/// What kind of participant this node is, and whether that is a coherent state.
///
/// Reported because nothing else on this page can distinguish the two ways a
/// node ends up advertising no models. Since routing candidacy began keying on
/// residency, a `terminal` and a holder whose slots failed to load look
/// identical from the mesh — and they want opposite repairs (§18.2: a
/// could-not-judge is not a failure, and a working-as-configured is neither).
///
/// Local config only, so it answers with the daemon down. `terminal` PASSES:
/// holding no weights is the configured state, not a defect.
pub(super) fn check_node_class() -> CheckResult {
    use sovereign_core::setup_config::{NodeClass, SetupConfig};
    const NAME: &str = "node_class";

    let cfg = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            return CheckResult {
                name: NAME,
                layer: Layer::Sovereign,
                status: CheckStatus::Skipped,
                message: format!("could not read config: {e}"),
                repair: Repair::None,
            }
        }
    };

    match cfg.node_class() {
        NodeClass::Holder => CheckResult {
            name: NAME,
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "holder — serves turns from its own model slots".to_string(),
            repair: Repair::None,
        },
        NodeClass::Terminal => CheckResult {
            name: NAME,
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: format!(
                "terminal — holds no models by design; every turn and embedding \
                 routes to {}. An empty model lineup here is correct, not broken.",
                cfg.node.entry.as_deref().unwrap_or("<unset>"),
            ),
            repair: Repair::None,
        },
        NodeClass::Unconfigured => CheckResult {
            name: NAME,
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "unconfigured — this node names neither a usable `[models]` \
                      primary nor a `[node] entry`, so it can neither serve a turn \
                      nor route one"
                .to_string(),
            repair: Repair::Executable(
                "svrn setup   # or: svrn setup --terminal <entry> to route to a peer".to_string(),
            ),
        },
    }
}

/// Does this terminal's entry node address still point at the SAME machine?
///
/// The interim mitigation for a debt we chose not to repay yet. `[node] entry`
/// is a URL, and ARCH §7.5 says a stable thing keyed on a volatile address
/// eventually answers confidently and wrongly — when a DHCP lease moves and
/// another machine takes the address, a terminal forwards there and nothing
/// errors. Resolving by node id was priced and deferred
/// (`sovereign/DEFAULTS_LEDGER.md`), so the posture is: cannot route around it,
/// must not fail to NOTICE it.
///
/// Four verdicts, not two (§18.2). Reaching a different node is a FAILURE.
/// Being unable to ask, or having no recorded id to compare against, is
/// `Skipped` — a could-not-judge, never a pass.
pub(super) async fn check_entry_node_identity() -> CheckResult {
    use sovereign_core::setup_config::{NodeClass, SetupConfig};
    const NAME: &str = "entry_node_identity";

    let Ok(cfg) = SetupConfig::load() else {
        return CheckResult {
            name: NAME,
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: "no readable config".to_string(),
            repair: Repair::None,
        };
    };
    if cfg.node_class() != NodeClass::Terminal {
        return CheckResult {
            name: NAME,
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: "not a terminal — no entry node to verify".to_string(),
            repair: Repair::None,
        };
    }
    let entry = cfg.node.entry.as_deref().unwrap_or_default();
    let Some(recorded) = cfg.node.entry_node_id.as_deref() else {
        return CheckResult {
            name: NAME,
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: format!(
                "no entry node id was recorded for {entry}, so a moved address \
                 cannot be detected — re-run `svrn setup --reset --terminal {entry}` \
                 to record one"
            ),
            repair: Repair::None,
        };
    };

    // `entry` carries the `/v1` suffix the OpenAI clients want; `/status` lives
    // on the bare origin.
    let origin = entry.trim_end_matches("/v1").trim_end_matches('/');
    let Some(body) = http_get_json(&format!("{origin}/status")).await else {
        return CheckResult {
            name: NAME,
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: format!(
                "entry node {origin} did not answer — cannot tell a moved address \
                 from a node that is merely down"
            ),
            repair: Repair::None,
        };
    };
    let seen = body.get("node_id").and_then(|n| n.as_str()).unwrap_or("");
    if seen == recorded {
        return CheckResult {
            name: NAME,
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: format!("{origin} is still node {recorded}"),
            repair: Repair::None,
        };
    }
    CheckResult {
        name: NAME,
        layer: Layer::Sovereign,
        status: CheckStatus::Failed,
        message: format!(
            "{origin} now answers as node {seen}, but this terminal was set up \
             against {recorded}. Every turn and every embedding from this node \
             has been going to a DIFFERENT machine than the one it was bound to, \
             without erroring."
        ),
        repair: Repair::Executable(format!(
            "svrn setup --reset --terminal <entry>   # re-bind, after checking which host you meant ({origin} moved)"
        )),
    }
}

pub(super) async fn check_embed_slot() -> CheckResult {
    const NAME: &str = "embed_slot";
    let repair = Repair::Executable("sovereign daemon stop && sovereign daemon start".to_string());

    let Some(resp) = http_post_json(
        "http://127.0.0.1:9741/v1/embeddings",
        serde_json::json!({ "model": "embed", "input": "doctor embed probe" }),
    )
    .await
    else {
        return CheckResult {
            name: NAME,
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: "daemon not reachable — cannot probe the embed slot".to_string(),
            repair: Repair::None,
        };
    };

    let status = resp.status();
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return CheckResult {
                name: NAME,
                layer: Layer::Sovereign,
                status: CheckStatus::Failed,
                message: format!("embed response was not JSON: {e}"),
                repair,
            }
        }
    };

    // An error BODY on a 200 is the shape that fooled every other check.
    if let Some(err) = body.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("(no message)");
        return CheckResult {
            name: NAME,
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: format!(
                "embed slot is listed but does not embed ({status}): {msg}. \
                 The router's classifiers and all atlas grounding go SILENTLY \
                 off in this state and turns still answer"
            ),
            repair,
        };
    }

    // A vector, and a non-degenerate one: an all-zero embedding is a slot
    // answering without working, which would pass a mere "is it an array" test.
    let dims = body
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|d| d.first())
        .and_then(|e| e.get("embedding"))
        .and_then(|v| v.as_array());
    match dims {
        Some(v) if v.len() >= 2 && v.iter().any(|x| x.as_f64().is_some_and(|f| f != 0.0)) => {
            CheckResult {
                name: NAME,
                layer: Layer::Sovereign,
                status: CheckStatus::Passed,
                message: format!("embed slot returned a {}-dim vector", v.len()),
                repair: Repair::None,
            }
        }
        Some(v) => CheckResult {
            name: NAME,
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: format!(
                "embed slot returned a degenerate {}-dim vector (all zeros or too short)",
                v.len()
            ),
            repair,
        },
        None => CheckResult {
            name: NAME,
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: format!("embed slot returned no vector ({status}): {body}"),
            repair,
        },
    }
}

pub(super) async fn check_server_running() -> CheckResult {
    let up = tcp_connectable("127.0.0.1", 9741).await;
    if up {
        CheckResult {
            name: "server_running",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "svrn server is reachable at :9741".into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "server_running",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "svrn server not reachable at :9741".into(),
            repair: Repair::executable("svrn project serve"),
        }
    }
}

pub(super) async fn check_server_tools() -> CheckResult {
    // MCP uses JSON-RPC 2.0 over a single `/mcp` endpoint. The previous
    // version POSTed to `/mcp/tools/list` (non-existent), which either
    // 404'd or returned an unparseable body — the "Warning:
    // unparseable" message users saw.
    let resp = http_post_json(
        "http://localhost:9741/mcp",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
        }),
    )
    .await;
    match resp {
        None => CheckResult {
            name: "server_tools",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "could not reach /mcp endpoint".into(),
            repair: Repair::executable("svrn project serve"),
        },
        Some(r) => match r.json::<serde_json::Value>().await {
            Ok(json) => {
                let count = json["result"]["tools"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or(0);
                // Canonical daemon registry is 12 tools (symbol/code
                // search, recent_changes, callers, callees, blast_radius,
                // 3× notes, session_reflection, project_context).
                // `svrn project serve` adds the
                // watcher-backed set (test_status, lint_status,
                // run_tests, get_run_output, get_lint_output) for 17.
                if count >= 12 {
                    CheckResult {
                        name: "server_tools",
                        layer: Layer::Sovereign,
                        status: CheckStatus::Passed,
                        message: format!("{count} tools registered"),
                        repair: Repair::None,
                    }
                } else if count > 0 {
                    CheckResult {
                        name: "server_tools",
                        layer: Layer::Sovereign,
                        status: CheckStatus::Warning,
                        message: format!(
                            "only {count} tools registered (expected ≥12) — possible version mismatch"
                        ),
                        repair: Repair::Manual(
                            "Check server logs for tool registration errors".into(),
                        ),
                    }
                } else {
                    CheckResult {
                        name: "server_tools",
                        layer: Layer::Sovereign,
                        status: CheckStatus::Warning,
                        message: "tools/list returned no tools".into(),
                        repair: Repair::Manual("Check server logs".into()),
                    }
                }
            }
            Err(_) => CheckResult {
                name: "server_tools",
                layer: Layer::Sovereign,
                status: CheckStatus::Warning,
                message: "/mcp tools/list returned unparseable response".into(),
                repair: Repair::Manual("Check server logs".into()),
            },
        },
    }
}

pub(super) async fn check_scip_indexed() -> CheckResult {
    // SCIP graphs are per-corpus: `~/.svrnmesh/indexes/<corpus_id>/scip_graph.db`.
    //
    // The previous version flagged "indexed" whenever the file size
    // crossed 4 KB — which the empty schema alone clears. A SCIP DB
    // that has failed every export for a week (e.g. the rust-analyzer
    // proxy was unresolved) still showed green. Open each DB and ask
    // it directly how many symbols it holds.
    let indexes_dir = sovereign_root().join("indexes");
    let mut populated: Vec<String> = Vec::new();
    let mut empty: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&indexes_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let db = path.join("scip_graph.db");
            if !db.exists() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            match corpus_engine_scip::ScipGraph::open(&db, &name) {
                Ok(graph) => {
                    if graph.symbol_count().await > 0 {
                        populated.push(name);
                    } else {
                        empty.push(name);
                    }
                }
                Err(_) => {
                    // Integrity check owns the corrupt/schema-mismatch reporting;
                    // skip here so a single DB doesn't double-fail across two checks.
                }
            }
        }
    }
    if !empty.is_empty() {
        let list = empty.join(", ");
        let example = empty.first().cloned().unwrap_or_else(|| "<corpus>".into());
        return CheckResult {
            name: "scip_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: format!(
                "SCIP graph DB present but 0 symbols ingested for {} corpus(es): {list}. \
                 Call graph tools (callers/callees/blast) will return empty. \
                 Likely cause: the language exporter (rust-analyzer / scip-typescript / \
                 scip-python / scip-go) failed during the last rebuild — see daemon.err \
                 for the stderr tail, then check `svrn doctor` again for the \
                 scip_exporters finding. next-edit's call-site jump list is silent \
                 for these corpora too — it reads the same graph.",
                empty.len(),
            ),
            repair: Repair::executable(format!("svrn project refresh --name {example} --local")),
        };
    }
    if !populated.is_empty() {
        return CheckResult {
            name: "scip_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: format!(
                "SCIP graph populated for {} corpus index(es): {}",
                populated.len(),
                populated.join(", "),
            ),
            repair: Repair::None,
        };
    }
    CheckResult {
        name: "scip_indexed",
        layer: Layer::Sovereign,
        status: CheckStatus::Failed,
        message: "no SCIP graph DB found — call graph tools (callers/callees/blast) \
                  unavailable, AND next-edit's call-site jump list stays silent in the \
                  editor: it reads this graph, so with no index it declines \
                  `graph_unavailable` and its status-bar item never appears. Index the \
                  repo you edit, then reload the editor."
            .into(),
        // `svrn init` is the current spelling; `svrn project init` still
        // forwards here but announces a deprecation, and a repair line is
        // the last place to hand someone the old name.
        repair: Repair::executable("svrn init"),
    }
}

pub(super) async fn check_code_indexed() -> CheckResult {
    // Distinguishes three states for each code corpus:
    //   - SCIP SQLite present AND `CorpusIndex::open` succeeds → healthy
    //   - SCIP SQLite present but `CorpusIndex::open` fails → Lance corrupt
    //   - neither present → not a code corpus (skipped silently)
    //
    // The previous version only checked that the parent indexes
    // directory had any entries at all, which silently passed when
    // every code corpus had lost its Lance table — `symbols`,
    // `code_search`, and `recent_changes` would return empty while
    // doctor reported all-green. (`callers` / `callees` / `blast`
    // still worked off the SCIP SQLite, which is what made the
    // failure mode look like a tool bug instead of a data outage.)
    let indexes_dir = sovereign_root().join("indexes");
    let Ok(entries) = std::fs::read_dir(&indexes_dir) else {
        return CheckResult {
            name: "code_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "no code indexes found — semantic code search unavailable".into(),
            repair: Repair::executable("svrn code index ."),
        };
    };
    let mut healthy = Vec::new();
    let mut broken = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let has_scip = path.join("scip_graph.db").exists();
        if !has_scip {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        match corpus_engine::CorpusIndex::open(&path).await {
            Ok(_) => healthy.push(name),
            Err(_) => broken.push(name),
        }
    }
    if !broken.is_empty() {
        // Lance is missing for at least one code corpus. The symbol /
        // code_search / recent_changes tools query Lance; without it
        // they silently return empty. Surfacing the corpus list +
        // remediation is the whole point of this check.
        let list = broken.join(", ");
        // Resolve each corpus to its registered root so `--fix` can actually
        // run this. An unregistered corpus gets a Manual telling the operator
        // to register it — never a template pretending to be runnable.
        let mut cmds: Vec<String> = Vec::new();
        let mut unresolved: Vec<String> = Vec::new();
        for id in &broken {
            match registered_root(id) {
                Some(root) => cmds.push(format!("svrn code index {root} --corpus-id {id}")),
                None => unresolved.push(id.clone()),
            }
        }
        let repair = if !cmds.is_empty() && unresolved.is_empty() {
            Repair::MultiExecutable(cmds)
        } else if !cmds.is_empty() {
            let mut all = cmds;
            all.push(format!(
                "# not registered, so no root is known: {}. \
                 Register first: svrn project register --root <repo> --corpus-id <id>",
                unresolved.join(", ")
            ));
            Repair::Manual(all.join("\n"))
        } else {
            Repair::Manual(format!(
                "no registered root for {list} — register the repo first: \
                 svrn project register --root <repo> --corpus-id <id>, \
                 then svrn code index <repo> --corpus-id <id>"
            ))
        };
        return CheckResult {
            name: "code_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: format!(
                "Lance chunk index missing or unreadable for {} corpus(es): {list}. \
                 Symbol lookup / code search / recent changes return empty. \
                 SCIP call graph is unaffected.",
                broken.len()
            ),
            repair,
        };
    }
    if healthy.is_empty() {
        return CheckResult {
            name: "code_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "no code indexes found — semantic code search unavailable".into(),
            repair: Repair::executable("svrn code index ."),
        };
    }
    CheckResult {
        name: "code_indexed",
        layer: Layer::Sovereign,
        status: CheckStatus::Passed,
        message: format!(
            "Lance chunk index readable for {} corpus(es): {}",
            healthy.len(),
            healthy.join(", "),
        ),
        repair: Repair::None,
    }
}

pub(super) fn check_notes_db() -> CheckResult {
    let db = sovereign_root().join("notes.db");
    if db.exists() {
        CheckResult {
            name: "notes_db",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "notes.db present".into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "notes_db",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "notes.db not found — note tools unavailable".into(),
            repair: Repair::executable("svrn init"),
        }
    }
}

pub(super) fn check_project_indexed() -> CheckResult {
    // Lives under the indexes directory, not directly in ~/.svrnmesh/.
    let project_db = sovereign_root().join("indexes").join("project_docs.db");
    if project_db.exists() {
        CheckResult {
            name: "project_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "project docs index present".into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "project_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "project docs index not found — project_context search unavailable".into(),
            // `svrn index project` was never a verb in any build. The verb
            // that rebuilds this index is `refresh` (main.rs help: "Rebuild
            // the project code index"), which dispatches to the sibling's
            // `project-refresh`.
            repair: Repair::executable("svrn refresh"),
        }
    }
}

/// Shared wording for the three watcher checks when a workspace has opted
/// out via `[watchers] enabled = false`. Watchers are optional; a workspace
/// that declared them off is CORRECT, not degraded, so these report Passed
/// with no repair. Reporting a permanent warning for a deliberate posture
/// trains the reader to ignore doctor output, which costs far more than the
/// nag ever bought.
pub(super) const WATCHERS_OFF_MSG: &str =
    "watchers disabled by config ([watchers] enabled = false) — \
     scripts/sovereign-lint.sh and scripts/sovereign-test.sh are the gate";

pub(super) fn watchers_opted_out(sovereign_dir: &std::path::Path) -> bool {
    corpus_engine::SovereignConfig::load_or_default(sovereign_dir).watchers_disabled()
}

pub(super) fn check_test_runner(sovereign_dir: &std::path::Path) -> CheckResult {
    let cfg = corpus_engine::SovereignConfig::load_or_default(sovereign_dir);
    if cfg.test_runner.is_some() {
        CheckResult {
            name: "test_runner",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "test_runner configured in sovereign.toml".into(),
            repair: Repair::None,
        }
    } else if cfg.watchers_disabled() {
        CheckResult {
            name: "test_runner",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: WATCHERS_OFF_MSG.into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "test_runner",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "test_runner not configured — test_status tool unavailable".into(),
            repair: Repair::Manual("Add [test_runner] section to .sovereign/sovereign.toml".into()),
        }
    }
}

pub(super) fn check_lint_runner(sovereign_dir: &std::path::Path) -> CheckResult {
    let cfg = corpus_engine::SovereignConfig::load_or_default(sovereign_dir);
    if cfg.lint_runner.is_some() {
        CheckResult {
            name: "lint_runner",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "lint_runner configured in sovereign.toml".into(),
            repair: Repair::None,
        }
    } else if cfg.watchers_disabled() {
        CheckResult {
            name: "lint_runner",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: WATCHERS_OFF_MSG.into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "lint_runner",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "lint_runner not configured — lint_status tool unavailable".into(),
            repair: Repair::Manual("Add [lint_runner] section to .sovereign/sovereign.toml".into()),
        }
    }
}

/// Probe the *liveness* of the lint/test watcher — distinct from
/// `check_test_runner`/`check_lint_runner`, which only confirm a runner
/// is *configured*. Calls the `lint_status` MCP tool and reads the
/// `watcher` health object it now returns. (`lint_status` and `build`
/// are the watcher tools exposed over the MCP transport; `test_status`
/// is CLI-only. The shared coordinator heartbeat backs all of them, so
/// `lint_status` answers the liveness question for the whole watcher.)
/// This is the check that catches the "configured but the coordinator
/// died/never started" state a config-presence check structurally
/// cannot see — the exact blind spot behind the watcher silently going
/// stale.
pub(super) async fn check_watcher_live(sovereign_dir: &std::path::Path) -> CheckResult {
    // Opted out: don't probe, don't warn. Probing would report
    // `not_configured` and advise restoring a config the operator
    // deliberately removed.
    if watchers_opted_out(sovereign_dir) {
        return CheckResult {
            name: "watcher_live",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: WATCHERS_OFF_MSG.into(),
            repair: Repair::None,
        };
    }

    let resp = http_post_json(
        "http://localhost:9741/mcp",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "lint_status", "arguments": {} },
        }),
    )
    .await;

    let Some(r) = resp else {
        return CheckResult {
            name: "watcher_live",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "daemon /mcp unreachable — cannot probe watcher liveness".into(),
            repair: Repair::executable("svrn daemon restart"),
        };
    };
    let Ok(json) = r.json::<serde_json::Value>().await else {
        return CheckResult {
            name: "watcher_live",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "lint_status returned an unparseable response".into(),
            repair: Repair::Manual("Check daemon logs".into()),
        };
    };

    match find_watcher_health(&json) {
        Some(w) => {
            let live = w["live"].as_bool().unwrap_or(false);
            let reason = w["reason"].as_str().unwrap_or("unknown").to_string();
            let hint = w["hint"].as_str().map(|s| s.to_string());
            if live {
                CheckResult {
                    name: "watcher_live",
                    layer: Layer::Sovereign,
                    status: CheckStatus::Passed,
                    message: format!("lint/test watcher live (reason: {reason})"),
                    repair: Repair::None,
                }
            } else if reason == "not_configured" {
                // The dedicated runner checks already advise on this;
                // keep it a soft note here so we don't double-alarm.
                CheckResult {
                    name: "watcher_live",
                    layer: Layer::Sovereign,
                    status: CheckStatus::Warning,
                    message: "lint/test watcher not configured (see test_runner / lint_runner)"
                        .into(),
                    repair: Repair::Manual(hint.unwrap_or_else(|| {
                        "Restore .sovereign/sovereign.toml.with-watchers and restart the daemon"
                            .into()
                    })),
                }
            } else {
                // Configured but NOT live — the formerly-invisible state.
                CheckResult {
                    name: "watcher_live",
                    layer: Layer::Sovereign,
                    status: CheckStatus::Warning,
                    message: format!(
                        "lint/test watcher NOT live (reason: {reason}) — stored results are orphaned; \
                         the supervisor should restart it shortly"
                    ),
                    repair: Repair::executable("svrn daemon restart"),
                }
            }
        }
        None => CheckResult {
            name: "watcher_live",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message:
                "lint_status returned no `watcher` health object — daemon/tool version mismatch?"
                    .into(),
            repair: Repair::executable("svrn daemon restart"),
        },
    }
}

/// Recursively locate the `watcher` health object inside an MCP
/// `tools/call` response, regardless of how the envelope wraps the tool
/// output (structured content vs a JSON string in `content[].text`).
/// The object is identified by its distinctive key set rather than by
/// path, so envelope changes don't break the probe.
pub(super) fn find_watcher_health(v: &serde_json::Value) -> Option<serde_json::Value> {
    match v {
        serde_json::Value::Object(map) => {
            if map.contains_key("live")
                && map.contains_key("reason")
                && map.contains_key("configured")
            {
                return Some(v.clone());
            }
            map.values().find_map(find_watcher_health)
        }
        serde_json::Value::Array(arr) => arr.iter().find_map(find_watcher_health),
        // Tool output is sometimes embedded as a JSON string in a text
        // content block — parse and recurse.
        serde_json::Value::String(s) => serde_json::from_str::<serde_json::Value>(s)
            .ok()
            .as_ref()
            .and_then(find_watcher_health),
        _ => None,
    }
}

// Commonwealth checks

/// Daemon log directory size. Rotation (copy-truncate, 10 MiB cap,
/// 5 backups per stream) makes >1 GiB nearly impossible — which is
/// exactly why this is a good check: it only fires when the rotation
/// loop itself broke (or something else is dumping into the dir).
pub(super) fn check_log_dir_size() -> CheckResult {
    const WARN_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB
    let log_dir = sovereign_root().join("logs");
    let total: u64 = std::fs::read_dir(&log_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0);
    let total_mb = total / (1024 * 1024);
    if total > WARN_BYTES {
        CheckResult {
            name: "log_dir_size",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: format!(
                "{} holds {total_mb} MiB — rotation should keep this bounded; \
                 the rotation loop may be broken (see log_rotation.rs contract)",
                log_dir.display()
            ),
            repair: Repair::Manual(
                "inspect ~/.svrnmesh/logs for runaway files; rotation covers \
                 daemon.{log,err,out} at 10MiB × 5 backups each"
                    .into(),
            ),
        }
    } else {
        CheckResult {
            name: "log_dir_size",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: format!("log dir at {total_mb} MiB (bounded by rotation)"),
            repair: Repair::None,
        }
    }
}

/// Daemon RSS vs the memory-watch soft limit, read from `/status`'s
/// `process.rss_mb` (the pull surface routes_status exposes). With
/// `doctor --watch` this is a genuine 30s memory pager. Skipped when
/// the field is absent (daemon predates the process block).
pub(super) async fn check_daemon_memory(client_url: &str) -> CheckResult {
    let soft = crate::memory_watch::soft_limit_mb();
    let Some(status) = http_get_json(&format!("{client_url}/status")).await else {
        return CheckResult {
            name: "daemon_memory",
            layer: Layer::Commonwealth,
            status: CheckStatus::Skipped,
            message: "/status unreachable".into(),
            repair: Repair::None,
        };
    };
    let Some(rss_mb) = status
        .get("process")
        .and_then(|p| p.get("rss_mb"))
        .and_then(|v| v.as_u64())
    else {
        return CheckResult {
            name: "daemon_memory",
            layer: Layer::Commonwealth,
            status: CheckStatus::Skipped,
            message: "no process.rss_mb on /status (daemon predates the process block — rebuild + restart)"
                .into(),
            repair: Repair::None,
        };
    };
    if rss_mb > soft {
        CheckResult {
            name: "daemon_memory",
            layer: Layer::Commonwealth,
            status: CheckStatus::Warning,
            message: format!(
                "daemon rss {rss_mb} MiB exceeds soft limit {soft} MiB — jetsam risk; \
                 see RUNBOOK memory section"
            ),
            repair: Repair::Manual(
                "check loaded models vs the canonical config (primary 35B-IQ4 + fast 4B-Q8 \
                 + embed 0.6B on 64GB); `svrn daemon restart` reclaims leaked growth"
                    .into(),
            ),
        }
    } else {
        CheckResult {
            name: "daemon_memory",
            layer: Layer::Commonwealth,
            status: CheckStatus::Passed,
            message: format!("daemon rss {rss_mb} MiB (soft limit {soft} MiB)"),
            repair: Repair::None,
        }
    }
}

/// Is the daemon registered with (and loaded into) the user's service
/// manager? Unsupervised is a Warning, not a Failure — running the
/// daemon manually in a terminal is a legitimate dev mode — but the
/// consequence is named: no auto-restart after a crash/jetsam. The
/// repair is executable so `doctor --fix` converges the box.
/// Is a distributed primary allowed to run inside the daemon's own process?
///
/// Pure config read, deliberately OUTSIDE the `:9741` reachability gate. Two
/// reasons, both learned the hard way: after the boot guard landed, a hazardous
/// config makes the daemon REFUSE to start — so a check that needed a live
/// daemon would report `Skipped` exactly when the operator needs the answer —
/// and it must also work after an abort, when the daemon is down for the very
/// reason being diagnosed.
///
/// It calls the same pure predicate the boot guard enforces, so doctor and the
/// daemon can never disagree about what is safe.
pub(super) fn check_distributed_primary_contained() -> CheckResult {
    use crate::daemon_cmd::build::containment::{
        classify_containment, ContainmentVerdict, OVERRIDE_ENV,
    };

    let name = "distributed_primary_contained";
    let Ok(config) = sovereign_core::setup_config::SetupConfig::load() else {
        return CheckResult {
            name,
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: "no readable sovereign config — nothing to assess".into(),
            repair: Repair::None,
        };
    };

    let verdict = classify_containment(
        config.compute.enabled && config.compute.distributed_primary,
        config.shared_model.role,
        false, // self node id is not resolved here; the role term carries it
        crate::daemon_cmd::bootstrap::rpc_discovery_armed(),
        std::env::var(OVERRIDE_ENV).is_ok(),
    );

    let armed_toml = "[compute]\nenabled = true\ndistributed_primary = true";
    match verdict {
        ContainmentVerdict::Armed => CheckResult {
            name,
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "distributed primary runs in a supervised compute child — \
                      a worker-loss ggml abort kills the child, not the daemon"
                .into(),
            repair: Repair::None,
        },
        ContainmentVerdict::NotApplicable => CheckResult {
            name,
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "this node does not host a mesh-distributed primary".into(),
            repair: Repair::None,
        },
        ContainmentVerdict::Warn => CheckResult {
            name,
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "shared-model ANCHOR with no compute-child containment — if this \
                      node wins the host election it will hold the split in-process, \
                      where a departing worker aborts the whole daemon (SIGABRT)"
                .into(),
            repair: Repair::Manual(armed_toml.into()),
        },
        ContainmentVerdict::RefuseOverridden => CheckResult {
            name,
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: format!(
                "in-process distributed primary permitted by {OVERRIDE_ENV} — a worker \
                 leaving the mesh will abort this daemon (exit 134)"
            ),
            repair: Repair::Manual(armed_toml.into()),
        },
        ContainmentVerdict::Refuse => CheckResult {
            name,
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "shared-model HOST with the distributed primary IN-PROCESS — a \
                      worker leaving the mesh drives an in-place reload whose teardown \
                      frees buffers on the departed worker, and ggml aborts the daemon \
                      (SIGABRT, exit 134; confirmed live 2026-07-27). The daemon will \
                      refuse to start in this configuration"
                .into(),
            repair: Repair::Manual(armed_toml.into()),
        },
    }
}

pub(super) fn check_daemon_supervised() -> CheckResult {
    if crate::service_install::service_installed() {
        CheckResult {
            name: "daemon_supervised",
            layer: Layer::Commonwealth,
            status: CheckStatus::Passed,
            message: "daemon is service-managed (auto-restarts on crash)".into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "daemon_supervised",
            layer: Layer::Commonwealth,
            status: CheckStatus::Warning,
            message: "daemon is NOT service-managed — it will not auto-restart \
                      after a crash or jetsam/OOM kill"
                .into(),
            repair: Repair::executable("svrn install-service"),
        }
    }
}
