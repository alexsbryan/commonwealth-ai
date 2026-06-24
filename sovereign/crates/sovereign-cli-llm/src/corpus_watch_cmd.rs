// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign corpus watch …` subcommand handlers.
//!
//! Thin HTTP clients for the daemon's `/internal/corpus/watch/*`
//! routes (mounted by `sovereign-mesh::corpus_watch_http`). All
//! commands are local-only — the loopback guard on the daemon side
//! enforces that.
//!
//! No subcommand requires a running model; they're metadata
//! operations on the watched-folder registry the daemon already
//! holds. The `watch` register call DOES kick off an initial ingest
//! though, which needs the daemon's embed model — that runs async
//! by default.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::json;

use sovereign_core::setup_config::SetupConfig;

const DEFAULT_CLIENT_PORT: u16 = 9741;

fn daemon_base_url() -> String {
    let port = SetupConfig::load()
        .map(|c| c.daemon.client_port)
        .unwrap_or(DEFAULT_CLIENT_PORT);
    format!("http://127.0.0.1:{port}")
}

/// Default HTTP timeout for the lightweight metadata routes
/// (`list`, `status`, `state`, `pause`, `resume`, …). The
/// register-with-`sync_initial=true` path uses a much longer
/// timeout because the daemon blocks the response on the entire
/// initial sweep, which can run for minutes on a sizable folder.
const METADATA_TIMEOUT_SECS: u64 = 30;

/// Long timeout used by the register path when `sync_initial=true`.
/// The daemon waits for the full initial sweep (walk + chunk +
/// embed) before responding — a few hundred markdown files on a
/// CPU-bound embedding pass can take several minutes. Setting this
/// to 30 minutes covers the realistic worst case for personal-vault
/// onboarding without leaving the CLI hanging forever if the daemon
/// genuinely wedges.
const SYNC_INITIAL_TIMEOUT_SECS: u64 = 30 * 60;

fn build_client() -> reqwest::Client {
    build_client_with_timeout(METADATA_TIMEOUT_SECS)
}

fn build_client_with_timeout(secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(secs))
        .build()
        .expect("reqwest client builds")
}

/// Format a reqwest error into a human hint, distinguishing the
/// "couldn't connect at all" case from the "connected but the
/// response did not arrive in time" case. The historical generic
/// "Could not contact the daemon" message was actively misleading
/// on a timed-out register call against a running daemon — it sent
/// users chasing a launchd issue when the real cause was a slow
/// initial sweep.
fn describe_request_error(err: &reqwest::Error, url: &str) -> String {
    if err.is_timeout() {
        format!(
            "Request to {url} timed out — the daemon accepted the connection but did not respond \
             in time. For a `watch` register with sync_initial=true this usually means the \
             initial sweep is still running. Try `sovereign corpus watch-status <id>` to check \
             progress, or re-register without --sync-initial to return immediately."
        )
    } else if err.is_connect() {
        format!(
            "Could not connect to the daemon at {url}: {err}\n\n\
             Is `sovereign daemon` running? Try: sovereign daemon status"
        )
    } else {
        format!("Request to {url} failed: {err}")
    }
}

// ─── `sovereign corpus watch <PATH> [flags]` ────────────────

pub async fn run_register(args: &[String]) -> i32 {
    if args.is_empty() || sovereign_cli_shared::help::wants_help(args) {
        print_register_help();
        return if args.is_empty() { 1 } else { 0 };
    }

    let mut path: Option<PathBuf> = None;
    let mut display_name: Option<String> = None;
    let mut sweep_secs: Option<u64> = None;
    let mut grace_secs: Option<u64> = None;
    let mut abs_threshold: Option<usize> = None;
    let mut frac_threshold: Option<f32> = None;
    let mut exclude_globs: Vec<String> = Vec::new();
    let mut follow_symlinks = false;
    let mut no_guard = false;
    let mut sync_initial = false;
    let mut with_ocr = false;
    let mut manual = false;
    let mut sensitive = false;
    let mut on_change: Option<String> = None;
    let mut allow_trigger = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--name" => display_name = iter.next().cloned(),
            "--sweep-secs" => sweep_secs = iter.next().and_then(|s| s.parse().ok()),
            "--grace-secs" => grace_secs = iter.next().and_then(|s| s.parse().ok()),
            "--abs-threshold" => abs_threshold = iter.next().and_then(|s| s.parse().ok()),
            "--frac-threshold" => frac_threshold = iter.next().and_then(|s| s.parse().ok()),
            "--exclude" => {
                if let Some(g) = iter.next() {
                    exclude_globs.push(g.clone());
                }
            }
            "--follow-symlinks" => follow_symlinks = true,
            "--no-deletion-guard" => no_guard = true,
            "--sync-initial" => sync_initial = true,
            "--ocr" => with_ocr = true,
            "--manual" => manual = true,
            "--sensitive" => sensitive = true,
            // Living trigger: a workflow (name or .toml path) to run automatically
            // whenever a sweep changes files. `--allow` skips the consent prompt
            // (for scripting). See `attach_consent`.
            "--on-change" => on_change = iter.next().cloned(),
            "--allow" => allow_trigger = true,
            other if !other.starts_with("--") && path.is_none() => {
                path = Some(PathBuf::from(other));
            }
            other => {
                eprintln!("Unknown flag for `corpus watch`: {other}");
                print_register_help();
                return 1;
            }
        }
    }

    let Some(path) = path else {
        eprintln!("Missing folder path. Usage: sovereign corpus watch <PATH> [flags]");
        return 1;
    };
    let abs_path = match std::fs::canonicalize(&path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "Could not canonicalize {}: {e}\n\
                 The folder must exist on disk before registering.",
                path.display()
            );
            return 1;
        }
    };
    if !abs_path.is_dir() {
        eprintln!("Not a directory: {}", abs_path.display());
        return 1;
    }

    // Build the request body — only include knobs the user
    // explicitly set. Fields the user omitted fall through to
    // WatchedFolderConfig::default in the daemon (which itself
    // honours `[watched_folders]` defaults from the daemon's
    // SetupConfig).
    let mut config = json!({});
    if let Some(s) = sweep_secs {
        config["sweep_interval_secs"] = json!(s);
    }
    if let Some(s) = grace_secs {
        config["soft_delete_grace_secs"] = json!(s);
    }
    if !exclude_globs.is_empty() {
        config["exclude_globs"] = json!(exclude_globs);
    }
    if follow_symlinks {
        config["follow_symlinks"] = json!(true);
    }
    if with_ocr {
        // Mirrors WatchedFolderConfig.with_ocr — projected onto
        // LocalCorpusConfig.ocr_pdfs by the factory. Requires the
        // daemon to have an OcrCtx installed; without one, scanned
        // PDFs land in failed_files with a "OCR enabled but ctx not
        // installed" reason.
        config["with_ocr"] = json!(true);
    }
    if manual {
        // Folder-ingest v1 §3.5: opt out of periodic sweeps. The
        // corpus only sweeps when `sovereign corpus watch-sync-now`
        // is called.
        config["sync_mode"] = json!("manual");
    }
    if sensitive {
        // Folder-ingest v1 §3.4: structurally exclude this folder
        // from the agent's ambient situated-context assembly. The
        // folder remains searchable on explicit query and via Inner
        // Work mode.
        config["sensitive"] = json!(true);
    }
    if abs_threshold.is_some() || frac_threshold.is_some() || no_guard {
        let mut guard = json!({});
        if let Some(t) = abs_threshold {
            guard["absolute_threshold"] = json!(t);
        }
        if let Some(t) = frac_threshold {
            guard["fractional_threshold"] = json!(t);
        }
        if no_guard {
            guard["enabled"] = json!(false);
        }
        config["deletion_guard"] = guard;
    }

    // Living trigger: attach a workflow to run on every change. Because it runs
    // unattended (shell / network / writes are possible), surface what it can do
    // and require explicit consent before arming it.
    if let Some(workflow) = &on_change {
        match attach_consent(workflow, allow_trigger).await {
            Ok(true) => config["run_on_changes"] = json!(workflow),
            Ok(false) => {
                eprintln!("Not arming the trigger; the folder will still be watched.");
            }
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        }
    }

    let body = json!({
        "path": abs_path,
        "display_name": display_name,
        "config": config,
        "sync_initial": sync_initial,
    });

    let url = format!("{}/internal/corpus/watch/register", daemon_base_url());
    // Pick the timeout based on whether the daemon will block the
    // response on the initial sweep. The default 30s metadata
    // timeout is right for every other watched-folder route, but
    // sync_initial=true on a vault with hundreds of files routinely
    // takes minutes to embed — the old 30s timeout produced a
    // misleading "Could not contact the daemon" error even though
    // the daemon was happily working.
    let client = if sync_initial {
        build_client_with_timeout(SYNC_INITIAL_TIMEOUT_SECS)
    } else {
        build_client()
    };
    let resp = match client.post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}", describe_request_error(&e, &url));
            return 1;
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        eprintln!("Daemon rejected the register call ({status}): {text}");
        return 1;
    }
    let parsed: RegisterResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Daemon returned an unparseable response: {e}");
            return 1;
        }
    };
    println!("Registered watched folder:");
    println!("  corpus_id    = {}", parsed.corpus_id);
    println!("  display_name = {}", parsed.display_name);
    println!("  initial_sweep = {:?}", parsed.initial_sweep);
    println!();
    println!(
        "Track progress with:  sovereign corpus watch-status {}",
        parsed.corpus_id
    );
    0
}

fn print_register_help() {
    eprintln!("sovereign corpus watch <PATH> [flags]");
    eprintln!();
    eprintln!("Register a folder the daemon keeps in sync. Adds, edits, and");
    eprintln!("deletes are reflected in the index every ~2 minutes (or whatever");
    eprintln!("`--sweep-secs` says, floored at 60s).");
    eprintln!();
    eprintln!("Flags:");
    eprintln!("  --name <NAME>            Display name (default: folder basename)");
    eprintln!("  --sweep-secs <N>         Sweep cadence in seconds (default: 120, floor: 60)");
    eprintln!("  --grace-secs <N>         Soft-delete grace window in seconds (default: 7d)");
    eprintln!(
        "  --abs-threshold <N>      Pause the sweep if it would delete >= N files (default: 100)"
    );
    eprintln!("  --frac-threshold <F>     Pause the sweep if it would delete >= F of live docs (default: 0.25)");
    eprintln!("  --no-deletion-guard      Disable both deletion thresholds (eager delete)");
    eprintln!("  --exclude <GLOB>         Path glob to exclude (repeatable)");
    eprintln!("  --follow-symlinks        Follow symlinks while walking (default: skip them)");
    eprintln!("  --sync-initial           Wait for the initial ingest to finish before returning");
    eprintln!("  --ocr                    OCR scanned PDFs (requires the daemon's OcrCtx to be installed)");
    eprintln!("  --manual                 Manual sync mode — only sweeps on `watch-sync-now` (default: continuous)");
    eprintln!("  --sensitive              Mark folder sensitive — excluded from ambient situated-context assembly");
    eprintln!("  --on-change <WORKFLOW>   Run a workflow (a `sovereign workflow` name or .toml path)");
    eprintln!("                           automatically on every change — the living trigger. Shows what");
    eprintln!("                           the workflow can do and asks for consent (it runs unattended).");
    eprintln!("  --allow                  Skip the --on-change consent prompt (for scripting)");
}

// ─── list / status / pause / resume / confirm / remove ──────

/// Resolve a trigger workflow, show what it can do, and (unless `allow`) ask the
/// user to confirm running it unattended on every change. Returns whether to arm
/// it. The presence of `run_on_changes` in the stored config IS the recorded
/// consent, so this gate is the single place that decides to set it.
async fn attach_consent(workflow: &str, allow: bool) -> std::result::Result<bool, String> {
    let (toml, origin) = sovereign_workflow_host::resolve_workflow_source(workflow)?;
    let wf = sovereign_workflow::Workflow::parse(&toml)
        .map_err(|e| format!("workflow `{workflow}`: {e}"))?;
    let caps = sovereign_workflow_host::summarize_capabilities(&wf).await;

    eprintln!("\nArming `{origin}` to run automatically on every change to this folder.");
    let phrases = caps.describe();
    if phrases.is_empty() {
        eprintln!("  It only reads and organizes files — no shell, network, or writes.");
    } else {
        eprintln!("  It can:");
        for p in &phrases {
            eprintln!("    • {p}");
        }
    }
    if !caps.unresolved.is_empty() {
        eprintln!(
            "  (couldn't fully check these steps now — e.g. an MCP server not connected: {})",
            caps.unresolved.join(", ")
        );
    }

    if allow {
        eprintln!("  --allow given; arming without prompting.");
        return Ok(true);
    }
    eprint!("\n  Run this unattended on every change? [y/N] ");
    use std::io::Write;
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Ok(false);
    }
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

pub async fn run_list(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("sovereign corpus watch-list");
        eprintln!();
        eprintln!("List every registered watched-folder corpus and its current status.");
        return 0;
    }
    let url = format!("{}/internal/corpus/watch/list", daemon_base_url());
    let resp = match build_client().get(&url).send().await {
        Ok(r) => r,
        Err(e) => return contact_failed(&url, e),
    };
    if !resp.status().is_success() {
        return reject_failed(resp).await;
    }
    let parsed: ListResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Daemon returned an unparseable response: {e}");
            return 1;
        }
    };
    if parsed.corpora.is_empty() {
        println!("No watched-folder corpora registered.");
        return 0;
    }
    for entry in parsed.corpora {
        println!(
            "{}  ({}) — {}",
            entry.corpus_id,
            entry.display_name,
            status_summary(&entry.status)
        );
        println!("    path: {}", entry.root_path.display());
    }
    0
}

pub async fn run_status(args: &[String]) -> i32 {
    let iter = args.iter();
    let mut corpus_id: Option<String> = None;
    let mut want_skipped = false;
    let mut want_failures = false;
    for a in iter {
        match a.as_str() {
            "--skipped" => want_skipped = true,
            "--failures" => want_failures = true,
            "--help" | "-h" | "help" => {
                eprintln!("sovereign corpus watch-status <CORPUS_ID> [--skipped] [--failures]");
                eprintln!();
                eprintln!("Without flags: prints the top-level status enum.");
                eprintln!("--skipped:  per-extension breakdown of files the walker skipped");
                eprintln!("            (e.g. `.docx`: 12 — no extractor for this format)");
                eprintln!("--failures: per-file detail on corrupt / password-protected /");
                eprintln!("            scanned-without-OCR files seen during the last sweep");
                return 0;
            }
            other if !other.starts_with("--") && corpus_id.is_none() => {
                corpus_id = Some(other.to_string());
            }
            other => {
                eprintln!("Unknown flag: {other}");
                return 1;
            }
        }
    }
    let Some(id) = corpus_id else {
        eprintln!("Missing corpus_id. Usage: sovereign corpus watch-status <CORPUS_ID>");
        return 1;
    };

    // Drill-down flags fetch the richer /state/ payload; bare status
    // calls keep the lighter /status/ payload to avoid sending the
    // full failed-files list on every poll.
    if want_skipped || want_failures {
        let url = format!("{}/internal/corpus/watch/state/{id}", daemon_base_url());
        let resp = match build_client().get(&url).send().await {
            Ok(r) => r,
            Err(e) => return contact_failed(&url, e),
        };
        if !resp.status().is_success() {
            return reject_failed(resp).await;
        }
        let parsed: StateResponse = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Daemon returned an unparseable response: {e}");
                return 1;
            }
        };
        println!("{}: {}", parsed.corpus_id, status_summary(&parsed.status));
        println!(
            "  live_entries = {}   tombstones = {}",
            parsed.live_entries, parsed.tombstones
        );
        if want_skipped {
            println!();
            if parsed.skipped_by_extension.is_empty() {
                println!("Skipped by extension: (none — every walked file matches the allow-list)");
            } else {
                println!("Skipped by extension (no extractor for these formats):");
                let mut entries: Vec<_> = parsed.skipped_by_extension.iter().collect();
                entries.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
                for (ext, count) in entries {
                    println!("  {:<24} {count:>6}", ext);
                }
            }
        }
        if want_failures {
            println!();
            if parsed.failed_files.is_empty() {
                println!("Failed files: (none)");
            } else {
                println!("Failed files (last sweep):");
                for f in &parsed.failed_files {
                    println!(
                        "  [{}] {}  ({}, first seen {})",
                        f.kind,
                        f.doc_id,
                        f.reason,
                        format_relative(f.first_seen_unix)
                    );
                }
            }
        }
        return 0;
    }

    let url = format!("{}/internal/corpus/watch/status/{id}", daemon_base_url());
    let resp = match build_client().get(&url).send().await {
        Ok(r) => r,
        Err(e) => return contact_failed(&url, e),
    };
    if !resp.status().is_success() {
        return reject_failed(resp).await;
    }
    let parsed: StatusResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Daemon returned an unparseable response: {e}");
            return 1;
        }
    };
    println!("{}: {}", parsed.corpus_id, status_summary(&parsed.status));
    println!();
    println!(
        "{}",
        serde_json::to_string_pretty(&parsed.status).unwrap_or_default()
    );
    0
}

pub async fn run_pause(args: &[String]) -> i32 {
    let mut iter = args.iter();
    let mut corpus_id: Option<String> = None;
    let mut reason: Option<String> = None;
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--reason" => reason = iter.next().cloned(),
            "--help" | "-h" => {
                eprintln!("sovereign corpus watch-pause <CORPUS_ID> [--reason TEXT]");
                return 0;
            }
            other if !other.starts_with("--") && corpus_id.is_none() => {
                corpus_id = Some(other.to_string());
            }
            other => {
                eprintln!("Unknown flag: {other}");
                return 1;
            }
        }
    }
    let Some(id) = corpus_id else {
        eprintln!("Missing corpus_id. Usage: sovereign corpus watch-pause <CORPUS_ID>");
        return 1;
    };
    let url = format!("{}/internal/corpus/watch/pause/{id}", daemon_base_url());
    let body = json!({ "reason": reason });
    post_ack(&url, body).await
}

pub async fn run_resume(args: &[String]) -> i32 {
    let Some(id) = require_corpus_id(args, "watch-resume") else {
        return 1;
    };
    let url = format!("{}/internal/corpus/watch/resume/{id}", daemon_base_url());
    post_ack(&url, json!({})).await
}

pub async fn run_confirm_deletion(args: &[String]) -> i32 {
    let Some(id) = require_corpus_id(args, "watch-confirm-deletion") else {
        return 1;
    };
    let url = format!(
        "{}/internal/corpus/watch/confirm-deletion/{id}",
        daemon_base_url()
    );
    post_ack(&url, json!({})).await
}

/// `sovereign corpus watch-sync-now <CORPUS_ID>` — request a Manual-
/// mode sweep. Server returns 409 if the corpus is in Continuous
/// mode, since the request would otherwise silently no-op.
pub async fn run_sync_now(args: &[String]) -> i32 {
    let Some(id) = require_corpus_id(args, "watch-sync-now") else {
        return 1;
    };
    let url = format!("{}/internal/corpus/watch/sync-now/{id}", daemon_base_url());
    post_ack(&url, json!({})).await
}

/// `sovereign corpus watch-add-root <CORPUS_ID> <PATH>` —
/// folder-ingest v1 §3.1, layer an additional root onto an
/// existing watched corpus.
pub async fn run_add_root(args: &[String]) -> i32 {
    if args.len() < 2 {
        eprintln!(
            "sovereign corpus watch-add-root <CORPUS_ID> <PATH>\n\n\
             Layer an additional root onto an existing watched corpus."
        );
        return 1;
    }
    let id = &args[0];
    let path = match std::fs::canonicalize(&args[1]) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "Could not canonicalize {}: {e}\n\
                 The folder must exist on disk before attaching.",
                args[1]
            );
            return 1;
        }
    };
    if !path.is_dir() {
        eprintln!("Not a directory: {}", path.display());
        return 1;
    }
    let url = format!("{}/internal/corpus/watch/{id}/roots", daemon_base_url());
    post_ack(&url, json!({ "path": path })).await
}

/// `sovereign corpus watch-remove-root <CORPUS_ID> <IDX>` —
/// folder-ingest v1 §3.1, detach an additional root by 0-based
/// index. The next sweep classifies the removed root's entries
/// as deletions; the deletion guard still applies.
pub async fn run_remove_root(args: &[String]) -> i32 {
    if args.len() < 2 {
        eprintln!(
            "sovereign corpus watch-remove-root <CORPUS_ID> <IDX>\n\n\
             Detach an additional root by 0-based index. List the corpus's\n\
             roots first with `corpus watch-status <id>`."
        );
        return 1;
    }
    let id = &args[0];
    let Ok(idx): std::result::Result<u32, _> = args[1].parse() else {
        eprintln!("idx must be a non-negative integer");
        return 1;
    };
    let url = format!(
        "{}/internal/corpus/watch/{id}/roots/{idx}",
        daemon_base_url()
    );
    let resp = match build_client().delete(&url).send().await {
        Ok(r) => r,
        Err(e) => return contact_failed(&url, e),
    };
    if !resp.status().is_success() {
        return reject_failed(resp).await;
    }
    println!("Detached additional root {idx} from corpus '{id}'.");
    println!("The next sweep will reconcile the removed entries.");
    0
}

pub async fn run_remove(args: &[String]) -> i32 {
    let Some(id) = require_corpus_id(args, "watch-remove") else {
        return 1;
    };
    let url = format!("{}/internal/corpus/watch/{id}", daemon_base_url());
    let resp = match build_client().delete(&url).send().await {
        Ok(r) => r,
        Err(e) => return contact_failed(&url, e),
    };
    if !resp.status().is_success() {
        return reject_failed(resp).await;
    }
    println!("Removed watched-folder corpus '{id}'.");
    println!("(Source folder untouched — Sovereign never writes to a watched folder.)");
    0
}

// ─── HTTP plumbing helpers ───────────────────────────────────

async fn post_ack(url: &str, body: serde_json::Value) -> i32 {
    let resp = match build_client().post(url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => return contact_failed(url, e),
    };
    if !resp.status().is_success() {
        return reject_failed(resp).await;
    }
    let parsed: AckResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Daemon returned an unparseable response: {e}");
            return 1;
        }
    };
    if parsed.ok {
        println!("ok ({})", parsed.corpus_id);
        0
    } else {
        eprintln!("daemon returned ok=false for {}", parsed.corpus_id);
        1
    }
}

fn contact_failed(url: &str, e: reqwest::Error) -> i32 {
    eprintln!("{}", describe_request_error(&e, url));
    1
}

async fn reject_failed(resp: reqwest::Response) -> i32 {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    eprintln!("Daemon rejected the request ({status}): {text}");
    1
}

fn require_corpus_id(args: &[String], cmd: &str) -> Option<String> {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        eprintln!("Usage: sovereign corpus {cmd} <CORPUS_ID>");
        return None;
    }
    Some(args[0].clone())
}

fn status_summary(s: &StatusEnum) -> String {
    use StatusEnum::*;
    match s {
        Idle {
            last_sweep_unix,
            live_docs,
            tombstones,
        } => {
            format!(
                "idle  live_docs={live_docs}  tombstones={tombstones}  last_sweep={}",
                if *last_sweep_unix == 0 {
                    "never".to_string()
                } else {
                    format_relative(*last_sweep_unix)
                }
            )
        }
        Sweeping {
            phase,
            current,
            total,
        } => {
            format!("sweeping  phase={phase:?}  {current}/{total}")
        }
        PausedAwaitingConfirmation {
            diff_summary,
            tripped_rule,
            ..
        } => format!(
            "paused_awaiting_confirmation  rule={tripped_rule:?}  removed={}",
            diff_summary.removed
        ),
        PausedManual { reason, .. } => format!("paused_manual  reason={reason}"),
        Errored { message, .. } => format!("errored  message={message}"),
    }
}

fn format_relative(unix_secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let delta = now.saturating_sub(unix_secs);
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3600)
    } else {
        format!("{}d ago", delta / 86_400)
    }
}

// ─── Wire-type mirrors (Deserialize-only on the CLI side) ────

#[derive(Debug, Deserialize)]
struct RegisterResponse {
    corpus_id: String,
    display_name: String,
    initial_sweep: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ListResponse {
    corpora: Vec<ListEntry>,
}

#[derive(Debug, Deserialize)]
struct ListEntry {
    corpus_id: String,
    display_name: String,
    root_path: PathBuf,
    status: StatusEnum,
}

#[derive(Debug, Deserialize)]
struct StatusResponse {
    corpus_id: String,
    status: StatusEnum,
}

#[derive(Debug, Deserialize)]
struct StateResponse {
    corpus_id: String,
    status: StatusEnum,
    skipped_by_extension: std::collections::HashMap<String, usize>,
    failed_files: Vec<FailedFileWire>,
    tombstones: usize,
    live_entries: usize,
}

#[derive(Debug, Deserialize)]
struct FailedFileWire {
    doc_id: String,
    #[allow(dead_code)]
    // path printed only via doc_id today; absolute_path reserved for future drill-down
    absolute_path: PathBuf,
    kind: String,
    reason: String,
    first_seen_unix: u64,
}

#[derive(Debug, Deserialize)]
struct AckResponse {
    corpus_id: String,
    ok: bool,
}

// Mirror of WatchedFolderStatus — kept here so the CLI doesn't drag
// the full `sovereign-tools` surface across module boundaries it
// doesn't otherwise need.
#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StatusEnum {
    Idle {
        last_sweep_unix: u64,
        live_docs: usize,
        tombstones: usize,
    },
    Sweeping {
        phase: SweepPhase,
        current: usize,
        total: usize,
    },
    PausedAwaitingConfirmation {
        diff_summary: DiffSummary,
        tripped_rule: TrippedRule,
        sweep_started_unix: u64,
    },
    PausedManual {
        since_unix: u64,
        reason: String,
    },
    Errored {
        message: String,
        errored_unix: u64,
    },
}

#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum SweepPhase {
    Walking,
    Diffing,
    Deleting,
    Updating,
    Adding,
    GcSoftDeletes,
}

#[derive(Debug, Deserialize, serde::Serialize)]
struct DiffSummary {
    added: usize,
    modified: usize,
    removed: usize,
    live_before: usize,
}

#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(tag = "rule", rename_all = "snake_case")]
enum TrippedRule {
    Absolute { threshold: usize, observed: usize },
    Fractional { threshold: f32, observed: f32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn timeout_error_says_timed_out_not_could_not_contact() {
        // Regression: previously a 30s register timeout against a
        // long-running sync_initial sweep produced the generic
        // "Could not contact the daemon" string, sending users to
        // debug launchd / firewall issues. The fix uses
        // `reqwest::Error::is_timeout()` to surface a timeout-
        // specific hint. We build a 1ms-timeout client and hit a
        // sleeping local server to materialise the timeout.
        use std::time::Duration;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                // Hold the connection open without responding so the
                // client times out. Drop the stream on this thread's
                // exit; the test only needs one accept.
                std::thread::sleep(Duration::from_secs(5));
                drop(stream);
                break;
            }
        });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let url = format!("http://127.0.0.1:{port}/anything");
        let err = client
            .get(&url)
            .send()
            .await
            .expect_err("request must error within the test window");
        let msg = describe_request_error(&err, &url);
        assert!(err.is_timeout(), "expected a timeout error, got {err:?}");
        assert!(
            msg.contains("timed out"),
            "timeout message must say 'timed out', got: {msg}"
        );
        assert!(
            !msg.contains("Could not connect"),
            "timeout must not be reported as a connect failure: {msg}"
        );
    }

    #[tokio::test]
    async fn connect_error_says_could_not_connect() {
        // Pick a port nothing is listening on. 1 is privileged and
        // always refused for unprivileged clients on macOS/Linux.
        let url = "http://127.0.0.1:1/anything";
        let client = build_client();
        let err = client.get(url).send().await.expect_err("connect must fail");
        let msg = describe_request_error(&err, url);
        // We don't require is_connect()==true (reqwest sometimes
        // returns is_request()==true for refused ports), only that
        // the message is NOT mislabelled as a timeout.
        assert!(
            !msg.contains("timed out"),
            "connect refusal must not be reported as a timeout: {msg}"
        );
    }
}
