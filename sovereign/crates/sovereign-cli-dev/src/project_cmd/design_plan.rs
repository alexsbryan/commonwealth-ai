// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn project design` / `plan` — the agent-collaborative design workflow.
//! `cmd_design` primes DESIGN.md (delegating to `crate::design_session`);
//! `cmd_plan` composes IMPLEMENTATION_PLAN.md + the `.sovereign/plan.db`
//! rows (via `crate::plan_composer` / `plan_enricher`). Split out of
//! `project_cmd` (2026-07-13); pure move. Shared plumbing via `use super::*`.

use super::*;

const HELP_DESIGN: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn project design",
    summary: "Agent-collaborative DESIGN.md session. opencode is the blessed path; --solo and --stopgap are fallbacks.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn project design [--import <path>] [--via <agent>]\n    \
             [--solo] [--stopgap] [--port <port>]",
        ),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            ("--import <path>",  "Copy <path> into <repo>/DESIGN.md (diff-confirms if one already exists)"),
            ("--via <agent>",    "Choose the agent: opencode (default) | claude-code | cursor"),
            ("--solo",           "Skip the agent; walk structural gaps with CLI prompts, write OPEN_QUESTIONS.md"),
            ("--stopgap",        "Provisional embedded CLI chat (banner-labelled; install opencode for the real experience)"),
            ("--port <port>",    "Commonwealth daemon port (default: 9741)"),
        ]),
        sovereign_cli_shared::help::HelpSection::Examples(&[
            ("svrn project design",                         "Launch opencode with the session brief primed"),
            ("svrn project design --import ./design.md",    "Import an existing doc, then start the session"),
            ("svrn project design --solo",                  "No agent — CLI prompts driven by the structural parser"),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Requires the Commonwealth daemon (start it with `commonwealth daemon start`).\n\
             The session writes DESIGN.md and OPEN_QUESTIONS.md at repo root; artifacts live in\n\
             .sovereign/.atos/design/<session-id>/.",
        ),
    ],
};

const HELP_PLAN: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn project plan",
    summary: "Compose IMPLEMENTATION_PLAN.md from DESIGN.md + OPEN_QUESTIONS.md; upsert rows into .sovereign/plan.db.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn project plan [--allow-open] [--no-enrich] [--enrich-model <id>] [--daemon-url <url>]",
        ),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            ("--allow-open",        "Proceed even if OPEN_QUESTIONS.md has unanswered entries (they surface as open risks on the matching phase)"),
            ("--no-enrich",         "Skip the inference-driven phase enrichment pass; produce the deterministic skeleton only"),
            ("--enrich-model <id>", "Override the chat model used for enrichment (default: Qwen3.6-35B-A3B-UD-MTP-IQ4_NL)"),
            ("--daemon-url <url>",  "Override the daemon URL for enrichment (default: http://localhost:9741)"),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Phase 0 = Skeleton (language-specific build+test stop).\n\
             Phases 1..N come from H2 sections in DESIGN.md, in order (skipping Anchors / Open questions).\n\
             Each phase 1..N is enriched by one chat call: the model rewrites the body and proposes an executable stop_hint. \
             If the daemon is unreachable, enrichment is skipped silently and the deterministic placeholders survive.\n\
             Answered OPEN_QUESTIONS.md entries surface as `Resolved (for the record)` on the matching phase.\n\
             Unanswered OQs block the plan unless --allow-open is set; they then surface as open risks.\n\
             Stale plan_items (from an older DESIGN.md) are marked `deferred` rather than deleted, preserving references.",
        ),
    ],
};

// agent-collaborative main event — init scaffolds the project;
// design is where the user + agent iterate on DESIGN.md together.
//
// Most of the work lives in `crate::design_session`; `cmd_design`
// parses args, resolves the repo root + project id, and hands off.
pub(crate) async fn cmd_design(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_DESIGN);
        return 0;
    }
    let mut import_path: Option<PathBuf> = None;
    let mut port: u16 = 9741;
    let mut transport = crate::design_session::TransportChoice::Default;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--import" => {
                i += 1;
                import_path = args.get(i).map(PathBuf::from);
            }
            "--via" => {
                i += 1;
                transport = match args.get(i).map(String::as_str) {
                    Some("opencode") => crate::design_session::TransportChoice::Opencode,
                    Some("claude-code") => crate::design_session::TransportChoice::ClaudeCode,
                    Some("cursor") => {
                        eprintln!("warning: --via cursor is not yet implemented; falling back to default.");
                        crate::design_session::TransportChoice::Default
                    }
                    Some(other) => {
                        eprintln!(
                            "error: --via expects opencode | claude-code | cursor; got `{other}`"
                        );
                        return 1;
                    }
                    None => {
                        eprintln!("error: --via requires an agent name");
                        return 1;
                    }
                };
            }
            "--solo" => transport = crate::design_session::TransportChoice::Solo,
            "--stopgap" => transport = crate::design_session::TransportChoice::Stopgap,
            "--port" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    match v.parse::<u16>() {
                        Ok(p) => port = p,
                        Err(_) => {
                            eprintln!("error: --port must be a number");
                            return 1;
                        }
                    }
                }
            }
            flag if flag.starts_with("--") => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            _ => {}
        }
        i += 1;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let project_id = derive_project_id(&repo_root);

    let req = crate::design_session::SessionRequest {
        repo_root,
        transport,
        import_path,
        daemon_port: port,
        project_id,
    };

    crate::design_session::run(req).await
}

// ─── Plan ────────────────────────────────────────────────────
//
// Step 6 of the ATOS onboarding redesign. `cmd_plan` reads
// DESIGN.md + OPEN_QUESTIONS.md, composes IMPLEMENTATION_PLAN.md at
// repo root, and upserts plan_items rows into .sovereign/plan.db.
// Composition lives in `crate::plan_composer` (pure); this handler
// does the IO, ordering, and indexing.
pub(crate) async fn cmd_plan(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_PLAN);
        return 0;
    }

    let mut allow_open = false;
    let mut no_enrich = false;
    let mut enrich_model: Option<String> = None;
    let mut daemon_url = "http://localhost:9741".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--allow-open" => allow_open = true,
            "--no-enrich" => no_enrich = true,
            "--enrich-model" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    enrich_model = Some(v.clone());
                }
            }
            "--daemon-url" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    daemon_url = v.clone();
                }
            }
            flag if flag.starts_with("--") => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            _ => {}
        }
        i += 1;
    }
    let enrich_model = enrich_model.unwrap_or_else(|| "Qwen3.6-35B-A3B-UD-MTP-IQ4_NL".to_string());

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    // Preflight: DESIGN.md must exist. An absent DESIGN.md means the
    // user hasn't run `project design` yet — point them at it
    // rather than silently conjuring a plan from thin air.
    let design_path = repo_root.join("DESIGN.md");
    let design_md = match std::fs::read_to_string(&design_path) {
        Ok(s) => s,
        Err(_) => {
            eprintln!(
                "  \u{2717} No DESIGN.md at repo root ({}).",
                design_path.display()
            );
            eprintln!("    Run `svrn project design` first to author or import one.");
            return 2;
        }
    };
    if design_md.trim().is_empty() {
        eprintln!("  \u{2717} DESIGN.md exists but is empty. Write some content first.");
        return 2;
    }

    // Parse OPEN_QUESTIONS.md (absent is fine — treated as "no
    // outstanding questions"; the plan will contain only
    // non-attached risks, none).
    let oq_path = repo_root.join("OPEN_QUESTIONS.md");
    let oq_text = std::fs::read_to_string(&oq_path).unwrap_or_default();
    let oqs = crate::plan_composer::parse_open_questions(&oq_text);

    // Unanswered-OQ gate. Default behavior is to block, because a
    // plan that papers over known load-bearing gaps is a worse
    // artifact than no plan — the user should see the gaps and
    // either answer them (best) or explicitly --allow-open (if
    // they're accepting the risk knowingly).
    let unanswered: Vec<_> = oqs.iter().filter(|o| !o.is_answered()).collect();
    if !unanswered.is_empty() && !allow_open {
        eprintln!();
        eprintln!(
            "  \u{26a0} {} unanswered question(s) in OPEN_QUESTIONS.md:",
            unanswered.len()
        );
        for oq in &unanswered {
            eprintln!("    · {} ({})", oq.id, oq.anchor);
        }
        eprintln!();
        eprintln!("    Answer them inline, or re-run with --allow-open to surface them");
        eprintln!("    as `Open risks` on the matching phase.");
        return 2;
    }

    // Resolve project id from repo root (same logic `found` uses).
    let project_id = derive_project_id(&repo_root);

    // Primary language: read from project.toml's observation section
    // so `plan` agrees with `init`/`found` without re-scanning. If
    // project.toml isn't present (user ran plan outside an init'd
    // repo), fall through with None — Phase 0 gets a generic stop.
    let project_toml_path = repo_root.join(".sovereign").join("project.toml");
    let primary_language: Option<String> =
        crate::project_toml::ProjectTomlFile::read(&project_toml_path)
            .ok()
            .and_then(|t| t.observation.languages.into_iter().next())
            .map(|l| l.id);

    // Compose (pure).
    let signals = corpus_engine_atos::design_signals::extract(&design_md);
    let today = today_iso();
    let compose_inputs = crate::plan_composer::ComposeInputs {
        project_id: &project_id,
        design_md: &design_md,
        signals: &signals,
        open_questions: &oqs,
        primary_language: primary_language.as_deref(),
        today: &today,
    };
    let mut composed = crate::plan_composer::compose_plan(&compose_inputs);

    // Inference enrichment pass (default-on; --no-enrich opts out).
    // The composer's structural skeleton has placeholder phase bodies
    // and "(fill this in)" stop hints — exactly the output a coding
    // agent can't act on. The enricher calls the daemon's chat slot
    // once per phase to replace those with concrete prose + an
    // executable shell command. Failures are silent: each phase falls
    // back to the composer's deterministic output, the plan still
    // ships, and the operator sees a one-line summary.
    if !no_enrich {
        if crate::plan_enricher::daemon_reachable(&daemon_url).await {
            eprintln!(
                "    \u{2026} enriching {} phase(s) via {} (this can take ~10-30s/phase)…",
                composed.items.len().saturating_sub(1),
                enrich_model
            );
            let outcome = crate::plan_enricher::enrich(
                &mut composed.items,
                &design_md,
                &signals.sections,
                primary_language.as_deref(),
                &daemon_url,
                &enrich_model,
            )
            .await;
            eprintln!(
                "    \u{2026} enrichment: {} enriched, {} skipped (Phase 0), {} failed (deterministic fallback)",
                outcome.enriched, outcome.skipped, outcome.failed
            );
            // Re-render with the mutated items.
            composed.markdown = crate::plan_composer::render(
                &compose_inputs,
                &composed.items,
                &composed.design_hash,
            );
        } else {
            eprintln!(
                "    \u{26a0} enrich: daemon at {} not reachable — keeping deterministic plan. \
                 Pass --no-enrich to silence this warning.",
                daemon_url
            );
        }
    }

    // Persist plan_items rows. Open a plan.db under .sovereign/.
    // The store tolerates running before .sovereign/ exists (creates
    // parent dirs) but in practice init has already built the tree.
    let plan_db = repo_root.join(".sovereign").join("plan.db");
    let store = match corpus_engine_atos::plan_items::PlanStore::open(&plan_db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  \u{2717} could not open plan.db: {e}");
            return 1;
        }
    };
    let now = unix_now_secs();
    // Defer stale rows from prior generations (different design_hash)
    // BEFORE writing fresh ones, so a new plan never collides with
    // open items from a prior DESIGN.md state.
    match store.defer_stale(&composed.design_hash, now).await {
        Ok(n) if n > 0 => {
            eprintln!("    \u{2026} {n} plan_item(s) from an older DESIGN.md state deferred.");
        }
        Ok(_) => {}
        Err(e) => eprintln!("    \u{26a0} defer_stale warning: {e}"),
    }
    for item in &composed.items {
        let depends_on = Vec::<String>::new();
        let stored = corpus_engine_atos::plan_items::PlanItem {
            id: item.id.clone(),
            phase: item.phase,
            title: item.title.clone(),
            body: item.body.clone(),
            realizes: item.realizes.clone(),
            depends_on,
            stop_hint: item.stop_hint.clone(),
            state: corpus_engine_atos::plan_items::PlanItemState::Open,
            design_hash: composed.design_hash.clone(),
            created_at: now,
            updated_at: now,
        };
        if let Err(e) = store.upsert(&stored).await {
            eprintln!("    \u{26a0} plan_items upsert failed for {}: {e}", item.id);
        }
    }

    // Write IMPLEMENTATION_PLAN.md at repo root (discoverable from
    // the repo listing + GitHub file tree, same convention as
    // DESIGN.md and OPEN_QUESTIONS.md).
    let plan_md_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if let Err(e) = std::fs::write(&plan_md_path, &composed.markdown) {
        eprintln!("  \u{2717} could not write IMPLEMENTATION_PLAN.md: {e}");
        return 1;
    }

    // Index the plan into ProjectDocsStore so `project_context`
    // queries surface it. Best-effort — an index failure shouldn't
    // abort; the markdown is the source of truth, the index is a
    // query acceleration.
    let docs_db_path = repo_root.join(".sovereign").join("project_docs.db");
    match corpus_engine_notes::ProjectDocsStore::open(&docs_db_path) {
        Ok(docs) => {
            if let Err(e) = docs.index_file(&plan_md_path, &repo_root).await {
                eprintln!("    \u{26a0} project_docs index failed: {e}");
            }
        }
        Err(e) => eprintln!("    \u{26a0} could not open project_docs.db: {e}"),
    }

    // Summary.
    eprintln!();
    eprintln!(
        "  \u{2713} IMPLEMENTATION_PLAN.md written ({} phase(s), DESIGN.md sha=`{}`).",
        composed.items.len(),
        composed.design_hash
    );
    eprintln!(
        "    plan.db: {} row(s) upserted at {}",
        composed.items.len(),
        plan_db.display()
    );
    if !unanswered.is_empty() {
        eprintln!(
            "    ({} unanswered OPEN_QUESTIONS entry(s) surfaced as open risks via --allow-open.)",
            unanswered.len()
        );
    }
    eprintln!();
    eprintln!("    Next: iterate on DESIGN.md; re-run `svrn project plan` to regenerate.");
    0
}
