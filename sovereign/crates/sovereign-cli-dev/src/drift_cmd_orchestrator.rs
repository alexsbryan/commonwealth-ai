//! `sovereign drift detect` — narrative-vs-code drift in one command.
//!
//! Wraps the eight primitives (code index → structural atlas → for
//! each narrative: recipe stamp + corpus install + enrich init +
//! enrich build + cross-corpus → drift report) as a **resilience
//! layer**, not just a convenience wrapper. New users invoke a single
//! command; power users keep using the primitives.
//!
//! ## Resilience guarantees (the load-bearing reason this exists)
//!
//! 1. **Chat-model auto-resolution.** Probes `/v1/chat/completions`
//!    with the operator-supplied or `primary` slot id. If the request
//!    fails (model registered but not loaded; daemon unreachable),
//!    surfaces a concrete error before kicking off any work.
//!
//! 2. **Auto-skip seed when narrative opening lacks entities.**
//!    `enrich build` is invoked with `--skip seed --skip configure`
//!    by default, because principle-shaped opening sections fail
//!    stage 1a with "no valid entity entries". The orchestrator
//!    bakes this in so the user never sees the stage-1a complaint.
//!
//! 3. **Tolerate skip-only extract failures.** When `enrich build`
//!    exits 1 because some chapters are too short to analyze (a soft
//!    failure — the run-file already has the successful chapters
//!    cached), the orchestrator detects this and continues with
//!    `cluster + name + resolve` directly. Today `enrich build`
//!    halts the entire pipeline on partial extract; the orchestrator
//!    fixes that operationally.
//!
//! 4. **Idempotent at every step.** Re-running on unchanged inputs
//!    short-circuits with `· skipped (cached)` per step. Failure
//!    mid-pipeline leaves state recoverable; the next invocation
//!    picks up from the failure point.
//!
//! 5. **Concrete remediation on failure.** No bare exit codes. Each
//!    failure prints which step failed and what to do.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tracing::{debug, info, warn};

/// Path to the `sovereign` binary used for subprocess fan-out.
///
/// Resolved at orchestrator entry from, in order: `--sovereign-bin` CLI
/// arg, `SOVEREIGN_BIN` env var, `std::env::current_exe()` (this same
/// binary). The current-exe default is the right answer in production:
/// the drift orchestrator is itself a subcommand of sovereign-cli, so
/// re-invoking the same binary is portable across machines.
fn resolve_sovereign_bin(cli_override: Option<&str>) -> PathBuf {
    if let Some(p) = cli_override {
        return PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("SOVEREIGN_BIN") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    std::env::current_exe()
        .ok()
        .unwrap_or_else(|| PathBuf::from("sovereign"))
}

#[derive(Debug, Default)]
struct DetectArgs {
    code_path: Option<PathBuf>,
    narrative_paths: Vec<PathBuf>,
    output: Option<PathBuf>,
    project_id: Option<String>,
    chat_model: Option<String>,
    sovereign_bin: Option<String>,
}

pub async fn cmd_detect(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            crate::util::help::print(&HELP);
            return 2;
        }
    };
    let Some(code_path) = parsed.code_path.as_deref() else {
        eprintln!("error: --code <path> is required");
        return 2;
    };
    if parsed.narrative_paths.is_empty() {
        eprintln!("error: at least one --narrative <path> is required");
        return 2;
    }
    if !code_path.exists() {
        eprintln!("error: code path does not exist: {}", code_path.display());
        return 1;
    }
    for n in &parsed.narrative_paths {
        if !n.exists() {
            eprintln!("error: narrative path does not exist: {}", n.display());
            return 1;
        }
    }

    let project_id = parsed
        .project_id
        .clone()
        .unwrap_or_else(|| basename_id(code_path).unwrap_or_else(|| "drift-target".into()));
    let structural_atlas_id = format!("{project_id}-self-atlas");
    let output_path = parsed
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from("./drift_report.md"));
    let sovereign_bin = resolve_sovereign_bin(parsed.sovereign_bin.as_deref());
    let sovereign_bin_str = sovereign_bin.to_string_lossy().into_owned();
    info!(
        code_path = %code_path.display(),
        narrative_count = parsed.narrative_paths.len(),
        project_id = %project_id,
        structural_atlas_id = %structural_atlas_id,
        sovereign_bin = %sovereign_bin_str,
        output = %output_path.display(),
        "drift_orchestrator:start"
    );

    println!("=== sovereign drift detect ===");
    println!("  code         = {}", code_path.display());
    for n in &parsed.narrative_paths {
        println!("  narrative    = {}", n.display());
    }
    println!("  project_id   = {project_id}");
    println!("  structural   = {structural_atlas_id}");
    println!("  output       = {}", output_path.display());
    println!();

    // ── Step 1: probe + resolve chat model ───────────────────
    info!("drift_orchestrator:step_chat_model_probe_start");
    let chat_model = match resolve_chat_model(parsed.chat_model.as_deref()).await {
        Ok(m) => m,
        Err(msg) => {
            warn!(error = %msg, "drift_orchestrator:step_chat_model_probe_failed");
            eprintln!("✗ chat-model probe failed: {msg}");
            eprintln!();
            eprintln!("  Remediation: confirm `sovereign daemon status` is running and");
            eprintln!("  at least one chat slot is loaded. Try: curl http://localhost:9741/v1/models");
            return 1;
        }
    };
    info!(chat_model = %chat_model, "drift_orchestrator:step_chat_model_resolved");
    println!("  ✓ chat model = {chat_model}");

    // ── Step 2: code index (idempotent, with retry on flaky
    //              partition-local race — see task #33/#35) ─────
    if !corpus_exists(&project_id) {
        info!(project_id = %project_id, "drift_orchestrator:step_code_index_start");
        println!();
        println!("  → indexing code corpus '{project_id}'…");
        if !run_code_index_with_retry(&sovereign_bin_str, &code_path, &project_id) {
            warn!(project_id = %project_id, "drift_orchestrator:step_code_index_failed");
            eprintln!("✗ `sovereign code index` failed after retry.");
            eprintln!("  Remediation: re-run manually:");
            eprintln!("    sovereign code index {} --corpus-id {}", code_path.display(), project_id);
            return 1;
        }
        info!(project_id = %project_id, "drift_orchestrator:step_code_index_done");
    } else {
        debug!(project_id = %project_id, "drift_orchestrator:step_code_index_cached");
        println!("  · code index '{project_id}' already exists (cached)");
    }

    // ── Step 3: structural atlas (idempotent) ────────────────
    let source_corpus = pick_source_corpus(&project_id);
    debug!(source_corpus = %source_corpus, "drift_orchestrator:source_corpus_resolved");
    if !atlas_has_content(&structural_atlas_id) {
        info!(atlas_id = %structural_atlas_id, source_corpus = %source_corpus, "drift_orchestrator:step_structural_atlas_start");
        println!("  → building structural atlas '{structural_atlas_id}'…");
        // `--include-functions` promotes `pub fn` / `pub method` items
        // into Entity atoms alongside the default struct/enum/trait
        // set. Without it the atlas tops out at module + type
        // granularity, so a normative claim anchored to a function
        // name (e.g. `open_index_for_corpus`) has nothing to ground
        // against — the drift report's fuzzy matcher returns
        // `None` and the finding lands in the critical bucket as
        // "anchor not in atlas." Cost: roughly doubles atom count
        // (~3.4k → ~6.2k on commonwealth-ai), adds ~5-10 sec to
        // the deterministic ingest walk (no LLM), negligible
        // downstream pipeline cost. The structural-atlas step is
        // cached after first build; flipping this on requires
        // `rm -rf ~/.sovereign/indexes/<project>-self-atlas/` to
        // force a fresh ingest.
        if !run_step(&sovereign_bin_str, &["enrich", "ingest", &structural_atlas_id, "--source-corpus", &source_corpus, "--include-functions"]) {
            warn!(atlas_id = %structural_atlas_id, "drift_orchestrator:step_structural_atlas_failed");
            eprintln!("✗ structural atlas ingest failed.");
            eprintln!("  Remediation: try `sovereign enrich ingest {} --source-corpus {} --include-functions` and inspect the error.",
                structural_atlas_id, source_corpus);
            return 1;
        }
        // Stub minimal enrichment config so atlas-cross-corpus
        // accepts this atlas as a peer.
        ensure_structural_enrich_config(&structural_atlas_id);
        info!(atlas_id = %structural_atlas_id, "drift_orchestrator:step_structural_atlas_done");
    } else {
        debug!(atlas_id = %structural_atlas_id, "drift_orchestrator:step_structural_atlas_cached");
        println!("  · structural atlas '{structural_atlas_id}' already exists (cached)");
    }

    // ── Step 3.5: git archaeology (additive, graceful failure) ──
    // Walks the code repo's git history once and produces a per-atom
    // provenance + co-evolution sidecar. Independent of the narrative
    // pipeline; failure here logs a warning and continues so the
    // primary drift report still renders.
    let archaeology_md = output_path.with_extension("archaeology.md");
    let archaeology_json = archaeology_md.with_extension("json");
    println!();
    println!("  → walking git history → {}", archaeology_md.display());
    let archaeology_args = [
        "git-archaeology",
        &structural_atlas_id,
        "--source-corpus",
        &source_corpus,
        "--source-path",
        &code_path.to_string_lossy(),
        "--output",
        &archaeology_md.to_string_lossy(),
    ];
    info!(atlas_id = %structural_atlas_id, output = %archaeology_md.display(), "drift_orchestrator:step_archaeology_start");
    let archaeology_ok = run_step(&sovereign_bin_str, &archaeology_args);
    if !archaeology_ok {
        warn!(atlas_id = %structural_atlas_id, "drift_orchestrator:step_archaeology_failed");
        eprintln!(
            "⚠ git-archaeology failed — drift report will skip the Provenance \
             & Evolution section."
        );
    } else {
        info!(atlas_id = %structural_atlas_id, "drift_orchestrator:step_archaeology_done");
    }

    // ── Step 4: per-narrative pipeline ───────────────────────
    let mut narrative_atlas_ids: Vec<String> = Vec::new();
    for narrative_path in &parsed.narrative_paths {
        let nid = basename_id(narrative_path)
            .unwrap_or_else(|| "narrative".into());
        let nid = format!("{project_id}-{nid}");
        info!(narrative_id = %nid, narrative_path = %narrative_path.display(), "drift_orchestrator:narrative_start");
        println!();
        println!("  ── narrative `{nid}` ──");

        // 4a: stamp recipe (idempotent).
        if !ensure_recipe(&nid, narrative_path) {
            warn!(narrative_id = %nid, "drift_orchestrator:narrative_recipe_stamp_failed");
            eprintln!("✗ failed to stamp recipe for {nid}.");
            return 1;
        }

        // 4b: install corpus (idempotent).
        if !corpus_exists(&nid) {
            info!(narrative_id = %nid, "drift_orchestrator:narrative_corpus_install_start");
            println!("    → installing narrative corpus '{nid}'…");
            if !run_step(&sovereign_bin_str, &["corpus", "install", &nid]) {
                warn!(narrative_id = %nid, "drift_orchestrator:narrative_corpus_install_failed");
                eprintln!("✗ corpus install failed.");
                eprintln!("  Remediation: `sovereign corpus install {nid}` and inspect.");
                return 1;
            }
            // The install is async; wait until the meta lands.
            wait_for_corpus(&nid, 60);
            info!(narrative_id = %nid, "drift_orchestrator:narrative_corpus_install_done");
        } else {
            debug!(narrative_id = %nid, "drift_orchestrator:narrative_corpus_cached");
            println!("    · corpus '{nid}' already exists (cached)");
        }

        // 4c: enrich init (idempotent).
        // BOTH config.json and chapters.json must exist to skip init.
        // A prior run that errored after writing config.json but before
        // chapters.json fools a config-only check, and the next `enrich
        // build` then errors on the missing manifest.
        let cfg_path = enrich_config_path(&nid);
        let chapters_present = chapters_path(&nid).exists();
        if !cfg_path.exists() || !chapters_present {
            info!(narrative_id = %nid, "drift_orchestrator:narrative_enrich_init_start");
            println!("    → init enrichment for '{nid}'…");
            // engineering_atlas extracts ONLY claims-with-code-anchors,
            // which is exactly what the drift matcher reads. literary_atlas
            // also extracted entities/events/relations/questions — work
            // the renderer discarded and the LLM cost ~30 min/document.
            // See `pipelines/engineering_atlas.rs` for the schema and
            // eval calibration.
            if !run_step(&sovereign_bin_str, &["enrich", "init", &nid, "--from-corpus", &nid, "--pipeline", "engineering_atlas"]) {
                warn!(narrative_id = %nid, "drift_orchestrator:narrative_enrich_init_failed");
                eprintln!("✗ enrich init failed.");
                return 1;
            }
            // Pin the resolved chat model so build doesn't fall
            // back to a registered-but-not-loaded id.
            patch_chat_model_in_config(&cfg_path, &chat_model);
            info!(narrative_id = %nid, chat_model = %chat_model, "drift_orchestrator:narrative_enrich_init_done");
        } else {
            debug!(narrative_id = %nid, chat_model = %chat_model, "drift_orchestrator:narrative_enrich_init_cached");
            println!("    · enrichment config + chapters exist — pinning chat_model={chat_model}");
            patch_chat_model_in_config(&cfg_path, &chat_model);
        }

        // 4d: build atlas (with auto-recovery for partial extract).
        if !atlas_has_content(&nid) {
            info!(narrative_id = %nid, "drift_orchestrator:narrative_atlas_build_start");
            println!("    → building narrative atlas (LLM, ~5-30 min)…");
            let build_status = run_step_capture(&sovereign_bin_str, &[
                "enrich", "build", &nid, "--full", "--skip", "seed", "--skip", "configure",
            ]);
            if !build_status.success {
                // Two failure modes share the "step `extract` exited"
                // signature:
                //   1. Real partial extract — some chapters succeeded,
                //      some skipped. Cluster/name/resolve over the
                //      cached partials produces a usable atlas.
                //   2. Cold extract failure — no chapter manifest, no
                //      cached chapter outputs. Recovery has nothing to
                //      work with and fails downstream with a confusing
                //      "phase X cache missing" error.
                // Distinguish via the explicit "no chapter manifest"
                // signal in stdout.
                if build_status.stdout_combined.contains("no chapter manifest") {
                    warn!(narrative_id = %nid, recovery_attempted = false, "drift_orchestrator:narrative_atlas_build_failed_no_manifest");
                    eprintln!("✗ enrich build for {nid} failed: chapter manifest missing.");
                    eprintln!("  Likely cause: a prior orchestrator run errored between writing");
                    eprintln!("  config.json and chapters.json. Wipe and retry:");
                    eprintln!("    rm -rf ~/.sovereign/enrichment/{nid} ~/.sovereign/indexes/{nid}/chapters.json");
                    eprintln!("    sovereign drift detect ...");
                    return 1;
                }
                if build_status.stdout_combined.contains("step `extract` exited") {
                    info!(narrative_id = %nid, "drift_orchestrator:narrative_atlas_build_recovery_start");
                    println!("    ⚠ build halted on partial extract — recovering via cluster + name + resolve…");
                    if !run_step(&sovereign_bin_str, &["enrich", "cluster", &nid])
                        || !run_step(&sovereign_bin_str, &["enrich", "name", &nid])
                        || !run_step(&sovereign_bin_str, &["enrich", "resolve", &nid, "--phase", "all"])
                    {
                        warn!(narrative_id = %nid, "drift_orchestrator:narrative_atlas_build_recovery_failed");
                        eprintln!("✗ recovery from partial extract failed.");
                        eprintln!("  Remediation: `sovereign enrich errors {nid}` for diagnostics.");
                        return 1;
                    }
                    info!(narrative_id = %nid, "drift_orchestrator:narrative_atlas_build_recovery_done");
                } else {
                    warn!(narrative_id = %nid, "drift_orchestrator:narrative_atlas_build_failed");
                    eprintln!("✗ enrich build failed for {nid}.");
                    eprintln!("  Remediation: `sovereign enrich errors {nid}` for diagnostics.");
                    return 1;
                }
            } else {
                info!(narrative_id = %nid, "drift_orchestrator:narrative_atlas_build_done");
            }
        } else {
            debug!(narrative_id = %nid, "drift_orchestrator:narrative_atlas_build_cached");
            println!("    · narrative atlas exists (cached)");
        }
        narrative_atlas_ids.push(nid.clone());

        // 4e: cross-corpus matching.
        let cross_path = cross_corpus_path(&nid);
        if !cross_path.exists() {
            info!(narrative_id = %nid, structural_atlas_id = %structural_atlas_id, "drift_orchestrator:narrative_cross_corpus_start");
            println!("    → matching '{nid}' ↔ '{structural_atlas_id}'…");
            if !run_step(&sovereign_bin_str, &["enrich", "atlas-cross-corpus", &nid, &structural_atlas_id]) {
                warn!(narrative_id = %nid, "drift_orchestrator:narrative_cross_corpus_partial");
                eprintln!("⚠ cross-corpus match returned non-zero — drift report will continue with whatever edges landed.");
            } else {
                info!(narrative_id = %nid, "drift_orchestrator:narrative_cross_corpus_done");
            }
        } else {
            debug!(narrative_id = %nid, "drift_orchestrator:narrative_cross_corpus_cached");
            println!("    · cross-corpus edges exist (cached)");
        }
    }

    // ── Step 4.5: rough-edges scan (markers + future doc-drift) ──
    // Skip on failure — rough edges are additive context, not
    // load-bearing. The narrative-vs-code drift is the primary
    // signal.
    let rough_edges_md = output_path.with_extension("rough.md");
    let rough_edges_json = rough_edges_md.with_extension("json");
    println!();
    println!("  → scanning rough edges → {}", rough_edges_md.display());
    let rough_args = [
        "rough-edges",
        &project_id,
        "--source-path",
        &code_path.to_string_lossy(),
        "--output",
        &rough_edges_md.to_string_lossy(),
    ];
    info!(project_id = %project_id, output = %rough_edges_md.display(), "drift_orchestrator:step_rough_edges_start");
    let rough_ok = run_step(&sovereign_bin_str, &rough_args);
    if !rough_ok {
        warn!(project_id = %project_id, "drift_orchestrator:step_rough_edges_failed");
        eprintln!("⚠ rough-edges scan failed — drift report will skip the Internal section.");
    } else {
        info!(project_id = %project_id, "drift_orchestrator:step_rough_edges_done");
    }

    // ── Step 5: render drift report ──────────────────────────
    println!();
    println!("  → rendering drift report → {}", output_path.display());
    let mut drift_args: Vec<String> = vec![
        "enrich".into(),
        "atlas-drift-report".into(),
        "--structural".into(),
        structural_atlas_id.clone(),
        "--output".into(),
        output_path.to_string_lossy().into_owned(),
    ];
    for nid in &narrative_atlas_ids {
        drift_args.push("--narrative".into());
        drift_args.push(nid.clone());
    }
    if rough_ok && rough_edges_json.exists() {
        drift_args.push("--rough-edges".into());
        drift_args.push(rough_edges_json.to_string_lossy().into_owned());
    }
    if archaeology_ok && archaeology_json.exists() {
        drift_args.push("--git-archaeology".into());
        drift_args.push(archaeology_json.to_string_lossy().into_owned());
    }
    let drift_args_refs: Vec<&str> = drift_args.iter().map(String::as_str).collect();
    info!(
        narrative_count = narrative_atlas_ids.len(),
        archaeology = archaeology_ok,
        rough_edges = rough_ok,
        output = %output_path.display(),
        "drift_orchestrator:step_report_render_start"
    );
    if !run_step(&sovereign_bin_str, &drift_args_refs) {
        warn!(output = %output_path.display(), "drift_orchestrator:step_report_render_failed");
        eprintln!("✗ drift report rendering failed.");
        return 1;
    }
    info!(output = %output_path.display(), "drift_orchestrator:step_report_render_done");

    // ── Step 6: write fingerprint sidecar + mirror to canonical path ──
    // The freshness-gate model (sibling to lint_status / test_status).
    // `drift_posture` reads this fingerprint to answer "is the report
    // current against the narrative docs?" without re-running the LLM
    // pipeline.
    //
    // Two write destinations:
    //
    //   (1) Next to `--output` (back-compat): the operator's
    //       chosen markdown path keeps its fingerprint sidecar,
    //       so pre-existing workflows that point drift at a repo-
    //       local path still see `<output>.fingerprint`.
    //
    //   (2) The canonical agent path `~/.sovereign/drift/`: this
    //       is where `drift_posture` and the new `drift_findings`
    //       MCP tools look by default. Without (2), any drift run
    //       that didn't explicitly set `--output ~/.sovereign/...`
    //       was invisible to the agent surface — the very gap
    //       the user flagged. Mirroring the report + its JSON
    //       sidecar + its fingerprint closes that.
    //
    // The canonical mirror writes `latest.md`, `latest.md.json`,
    // and `.fingerprint`. Multiple project drift runs overwrite
    // each other at this path — one repo per `~/.sovereign/drift/`
    // is the v1 contract; per-project subdirs are a future change
    // if the user runs drift against multiple workspaces.
    let drift_dir = output_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let narrative_abs: Vec<PathBuf> = parsed
        .narrative_paths
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect();
    match sovereign_tools::write_fingerprint(&drift_dir, &narrative_abs, &output_path) {
        Ok(fp_path) => {
            info!(
                fingerprint = %fp_path.display(),
                narratives = narrative_abs.len(),
                "drift_orchestrator:fingerprint_written"
            );
            println!("  ✓ fingerprint: {}", fp_path.display());
        }
        Err(e) => {
            warn!(error = %e, "drift_orchestrator:fingerprint_write_failed");
            eprintln!("⚠ failed to write drift fingerprint: {e}");
            eprintln!("   (drift_posture will report `never_run` until this succeeds)");
        }
    }

    // Mirror to the canonical agent path so `drift_posture` /
    // `drift_findings` can find the latest report without the
    // operator having to remember to point `--output` at it.
    let canonical_dir = dirs::home_dir()
        .map(|h| h.join(".sovereign").join("drift"))
        .unwrap_or_else(|| PathBuf::from(".sovereign/drift"));
    match mirror_to_canonical(&canonical_dir, &output_path, &narrative_abs) {
        Ok(canonical_md) => {
            info!(
                canonical = %canonical_md.display(),
                "drift_orchestrator:canonical_mirror_written"
            );
            println!("  ✓ canonical mirror: {}", canonical_md.display());
        }
        Err(e) => {
            warn!(error = %e, "drift_orchestrator:canonical_mirror_failed");
            eprintln!(
                "⚠ failed to mirror drift report to {}: {e}",
                canonical_dir.display()
            );
            eprintln!("   (drift_posture / drift_findings will only see the explicit --output copy)");
        }
    }

    info!(output = %output_path.display(), "drift_orchestrator:complete");

    println!();
    println!("✓ drift report ready: {}", output_path.display());
    0
}

/// Mirror the drift report + its JSON sidecar + a freshly-stamped
/// fingerprint into the canonical `~/.sovereign/drift/` directory.
/// `drift_posture` (and the forthcoming `drift_findings` tool)
/// default to that path; without this mirror, the agent surface
/// reports `never_run` for any run that targeted a repo-local
/// `--output`. Best-effort: failures are surfaced but don't fail
/// the parent drift run.
fn mirror_to_canonical(
    canonical_dir: &Path,
    output_md: &Path,
    narrative_abs: &[PathBuf],
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(canonical_dir)?;
    let dest_md = canonical_dir.join("latest.md");
    std::fs::copy(output_md, &dest_md)?;

    // The JSON sidecar's filename is `<md-stem>.md.json`. Mirror
    // it alongside as `latest.md.json` so consumers don't have to
    // sniff the layout. If the sidecar isn't present (failed
    // render step earlier), skip silently — fingerprint write
    // below will still record the attempt.
    let src_json = {
        let stem = output_md
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("report.md");
        output_md.with_file_name(format!("{stem}.json"))
    };
    if src_json.exists() {
        let dest_json = canonical_dir.join("latest.md.json");
        std::fs::copy(&src_json, &dest_json)?;
    }

    // Stamp a fresh fingerprint AT the canonical path. The
    // sibling write next to `--output` is what feeds the
    // operator's repo-local view; this one is what
    // `drift_posture::compute_posture(~/.sovereign/drift/, ...)`
    // reads. Using `dest_md` as the recorded `output_path` so
    // the posture report points at the canonical mirror.
    let _ = sovereign_tools::write_fingerprint(canonical_dir, narrative_abs, &dest_md)?;
    Ok(dest_md)
}

// ── Step helpers ─────────────────────────────────────────────

/// Resolve which slot to use for the LLM-bound narrative-atlas build.
///
/// **Default is `fast`**, not `primary`, by design. The drift detector
/// should scale to a fast slot's capabilities so it generalizes to
/// operators without a 36B+ primary model — a 9B fast slot already
/// produces a useful first-pass atlas, and the per-chapter LLM time
/// on `primary` (~3 min/chapter under schema-constrained 16k output)
/// makes a 50-chapter doc effectively a multi-hour run. The
/// `--chat-model primary` override is available when the operator
/// wants peak quality and is willing to pay the time cost.
///
/// Probe order: explicit override (if given) → `fast` → `primary`.
async fn resolve_chat_model(operator_choice: Option<&str>) -> Result<String, String> {
    let candidates: Vec<String> = if let Some(c) = operator_choice {
        vec![c.to_string(), "fast".into(), "primary".into()]
    } else {
        vec!["fast".into(), "primary".into()]
    };
    for candidate in &candidates {
        debug!(candidate = %candidate, "drift_orchestrator:chat_model_probe_candidate");
        if probe_chat(candidate).await {
            debug!(candidate = %candidate, "drift_orchestrator:chat_model_probe_accepted");
            return Ok(candidate.clone());
        }
    }
    Err(format!(
        "no working chat slot among: {}",
        candidates.join(", ")
    ))
}

async fn probe_chat(model: &str) -> bool {
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "ok"}],
        "max_tokens": 4,
    });
    // 120s is generous for a 4-token probe but the daemon may be
    // serving a long-running inference call when we hit it (e.g. the
    // operator has a Claude Code session driving completions through
    // MCP); we want the probe to wait through that, not fail-fast.
    // The probe is a structural correctness check (is a chat slot
    // configured at all?), not a latency benchmark — false negatives
    // here force a `--chat-model` override and waste a setup round.
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let res = client
        .post("http://localhost:9741/v1/chat/completions")
        .json(&body)
        .send()
        .await;
    match res {
        Ok(r) if r.status().is_success() => {
            // Parse response and confirm a non-empty completion came
            // back; some misconfigured slots return 200 with empty
            // content, which would silently break later phases.
            if let Ok(v) = r.json::<serde_json::Value>().await {
                let content = v
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("message"))
                    .and_then(|m| m.get("content"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                return !content.is_empty();
            }
            false
        }
        _ => false,
    }
}

fn ensure_recipe(corpus_id: &str, doc_path: &Path) -> bool {
    let recipe_dir = home_dir().join(".sovereign/recipes").join(corpus_id);
    let recipe_toml = recipe_dir.join("recipe.toml");
    if recipe_toml.exists() {
        println!("    · recipe '{corpus_id}' exists (cached)");
        return true;
    }
    if let Err(e) = std::fs::create_dir_all(&recipe_dir) {
        eprintln!("    ✗ creating recipe dir: {e}");
        return false;
    }
    let display = doc_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(corpus_id);
    let content = format!(
        r#"[corpus]
id = "{corpus_id}"
name = "{display}"
description = "Narrative atlas stamped by `sovereign drift detect`."
license = "private"
# mesh_sharing = true: the auth boundary is Tailscale-IP, so this only
# exposes the corpus to mesh peers the user themselves trust. Replication
# of the atlas sidecar (atoms.json, edges.json, git_archaeology.json)
# rides the partition tar served by GET /internal/index/serve, which lets
# a second machine read the same drift output without re-running the LLM.
mesh_sharing = true

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "markdown"

[chunk]
type = "passthrough"

[index]
fts = true
vector = true

[enrichment]
enabled = true
type = "atlas"
pipeline = "literary_atlas"
"#,
        corpus_id = corpus_id,
        display = display,
        path = doc_path.display(),
    );
    if let Err(e) = std::fs::write(&recipe_toml, content) {
        eprintln!("    ✗ writing recipe: {e}");
        return false;
    }
    println!("    ✓ stamped recipe '{corpus_id}'");
    true
}

fn ensure_structural_enrich_config(atlas_id: &str) {
    let cfg_path = enrich_config_path(atlas_id);
    if cfg_path.exists() {
        return;
    }
    if let Some(parent) = cfg_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let stub = format!(
        r#"{{
  "schema_version": 1,
  "corpus_id": "{atlas_id}",
  "pipeline_id": "structure_first_code",
  "source_path": "corpus:{atlas_id}",
  "chapter_regex": "",
  "chat_model": "primary",
  "embed_model": "qwen-embedding-0.6b",
  "base_url": "http://localhost:9741",
  "min_section_body_words": 0,
  "max_output_tokens": 16384,
  "created_at": "2026-05-07T00:00:00+00:00"
}}"#
    );
    let _ = std::fs::write(&cfg_path, stub);
}

fn patch_chat_model_in_config(cfg_path: &Path, chat_model: &str) {
    let Ok(raw) = std::fs::read_to_string(cfg_path) else { return };
    let Ok(mut value): Result<serde_json::Value, _> = serde_json::from_str(&raw) else { return };
    let Some(obj) = value.as_object_mut() else { return };
    obj.insert("chat_model".into(), serde_json::Value::String(chat_model.to_string()));
    if let Ok(out) = serde_json::to_string_pretty(&value) {
        let _ = std::fs::write(cfg_path, out);
    }
}

fn wait_for_corpus(corpus_id: &str, max_seconds: u64) {
    let start = std::time::Instant::now();
    let meta = home_dir()
        .join(".sovereign/indexes")
        .join(corpus_id)
        .join("_corpus_meta.json");
    while start.elapsed().as_secs() < max_seconds {
        if meta.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

fn corpus_exists(corpus_id: &str) -> bool {
    let canonical = home_dir()
        .join(".sovereign/indexes")
        .join(corpus_id)
        .join("_corpus_meta.json");
    if canonical.exists() {
        return true;
    }
    let partition = home_dir()
        .join(".sovereign/indexes")
        .join(format!("{corpus_id}-partition-local"))
        .join("_corpus_meta.json");
    partition.exists()
}

fn pick_source_corpus(project_id: &str) -> String {
    let partition = home_dir()
        .join(".sovereign/indexes")
        .join(format!("{project_id}-partition-local"))
        .join("_corpus_meta.json");
    if partition.exists() {
        format!("{project_id}-partition-local")
    } else {
        project_id.to_string()
    }
}

fn atlas_atoms_path(corpus_id: &str) -> PathBuf {
    home_dir()
        .join(".sovereign/indexes")
        .join(corpus_id)
        .join("atlas")
        .join("atoms.json")
}

/// True only when the atlas has actually been BUILT, not just
/// scaffolded. The corpus-install path writes a stub
/// `{"atoms":[]}` (~44 bytes) into atlas/atoms.json before any
/// extraction has run; skipping the build on that scaffold leaves
/// the orchestrator with an empty atlas downstream. Inspect the
/// atom count for a real signal.
fn atlas_has_content(corpus_id: &str) -> bool {
    let path = atlas_atoms_path(corpus_id);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(value): Result<serde_json::Value, _> = serde_json::from_str(&raw) else {
        return false;
    };
    let count = value
        .get("atoms")
        .and_then(|a| a.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    count > 0
}

fn cross_corpus_path(corpus_id: &str) -> PathBuf {
    home_dir()
        .join(".sovereign/indexes")
        .join(corpus_id)
        .join("atlas")
        .join("cross_corpus_edges.json")
}

fn enrich_config_path(corpus_id: &str) -> PathBuf {
    home_dir()
        .join(".sovereign/enrichment")
        .join(corpus_id)
        .join("config.json")
}

fn chapters_path(corpus_id: &str) -> PathBuf {
    home_dir()
        .join(".sovereign/indexes")
        .join(corpus_id)
        .join("chapters.json")
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

fn basename_id(p: &Path) -> Option<String> {
    let name = p.file_name()?.to_str()?;
    // Strip extension; lowercase; replace spaces/underscores with `-`.
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    let id: String = stem
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let id = id.trim_matches('-').replace("--", "-");
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

// ── Subprocess wrappers ──────────────────────────────────────

#[derive(Debug)]
struct StepResult {
    success: bool,
    stdout_combined: String,
}

fn run_step(bin: &str, args: &[&str]) -> bool {
    let status = Command::new(bin).args(args).status();
    matches!(status, Ok(s) if s.success())
}

/// `code index` with one retry on the flaky partition-local race
/// (gap A — task #33). The race has multiple observed symptoms:
///
///   - "Missing metadata at <id>-partition-local/_corpus_meta.json"
///     after FTS title — meta state gets rolled back.
///   - "lance error: Not found: <…>/chunks.lance/data/<…>.lance"
///     during FTS content build — a Lance data file vanishes mid-build.
///
/// Both happen on the FIRST run after a wipe and both reference the
/// `-partition-local` directory in the error output. We treat any
/// failure that mentions `-partition-local` as the flaky race and
/// retry once — a clean retry succeeds in observed cases.
///
/// Output is captured (not streamed) so we can inspect the error
/// signature. Operator sees the combined stdout/stderr at the end
/// of each attempt.
fn run_code_index_with_retry(sovereign_bin: &str, code_path: &Path, project_id: &str) -> bool {
    const MAX_ATTEMPTS: u32 = 2;
    let path_str = code_path.to_string_lossy();
    let args: [&str; 4] = ["code", "index", &path_str, "--corpus-id"];
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let r = run_step_capture(sovereign_bin, &[args[0], args[1], args[2], args[3], project_id]);
        if r.success {
            return true;
        }
        // Match on `-partition-local` in any error context — both
        // observed gap A variants share this substring and clean
        // success doesn't emit it.
        let is_flaky_partition_race = r.stdout_combined.contains("-partition-local")
            && (r.stdout_combined.contains("Missing metadata at")
                || r.stdout_combined.contains("lance error: Not found"));
        if !is_flaky_partition_race || attempt >= MAX_ATTEMPTS {
            if !is_flaky_partition_race {
                debug!(project_id = %project_id, attempt, "drift_orchestrator:code_index_failure_non_retryable");
            } else {
                warn!(project_id = %project_id, attempt, max = MAX_ATTEMPTS, "drift_orchestrator:code_index_exhausted_retries");
            }
            return false;
        }
        warn!(project_id = %project_id, attempt, max = MAX_ATTEMPTS, "drift_orchestrator:code_index_partition_race_retry");
        eprintln!(
            "    ⚠ code index hit the flaky partition-local race — retrying ({attempt}/{MAX_ATTEMPTS}). \
             See task #33 for root-cause investigation."
        );
        // Wipe both the half-built canonical AND any leftover
        // partition-local directory so the next attempt's
        // create_or_resume + promote-to-canonical don't trip on
        // existing state from the failed attempt.
        for suffix in ["", "-partition-local"] {
            let dir = home_dir()
                .join(".sovereign/indexes")
                .join(format!("{project_id}{suffix}"));
            if dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&dir) {
                    eprintln!(
                        "      (could not wipe {}: {e}; retry may still trip)",
                        dir.display()
                    );
                }
            }
        }
    }
}

/// Spawn a child process and stream its stdout/stderr line-by-line
/// to the parent's stdout/stderr in real time while simultaneously
/// collecting them into a buffer for post-mortem failure-signature
/// inspection.
///
/// Replaces an earlier `Command::output()` capture that blocked
/// silently for the entire child lifetime (~25-30 min on the LLM
/// build step). Glassbox §9.1 — the operator must be able to see
/// progress on long-running phases without attaching a debugger.
///
/// Also fires a heartbeat every 30 seconds when no child output
/// has landed, so a hang vs. legitimate slow phase is
/// distinguishable from the terminal.
fn run_step_capture(bin: &str, args: &[&str]) -> StepResult {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::mpsc::channel;
    use std::thread;
    use std::time::Duration;

    let mut child = match Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return StepResult {
                success: false,
                stdout_combined: format!("subprocess spawn failed: {e}"),
            };
        }
    };

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let (tx, rx) = channel::<String>();
    let tx_stderr = tx.clone();

    // Stream stdout line by line. Each line lands in two places:
    // the parent's stdout (so the operator sees it) and the
    // collection channel (so the orchestrator can grep for failure
    // signatures after the child exits).
    let h_stdout = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(std::result::Result::ok) {
            println!("{line}");
            let _ = std::io::stdout().flush();
            let _ = tx.send(line);
        }
    });
    let h_stderr = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(std::result::Result::ok) {
            eprintln!("{line}");
            let _ = std::io::stderr().flush();
            let _ = tx_stderr.send(line);
        }
    });

    // Heartbeat: when no child output has arrived in ~30s, print
    // a one-liner so a hung child is visibly distinct from "still
    // working but quiet". The heartbeat thread polls the channel
    // with a timeout; child-completion drops both senders, which
    // closes the channel and ends the loop.
    let started = std::time::Instant::now();
    let mut combined = String::new();
    let mut last_output_at = std::time::Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(line) => {
                combined.push_str(&line);
                combined.push('\n');
                last_output_at = std::time::Instant::now();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let quiet = last_output_at.elapsed().as_secs();
                let total = started.elapsed().as_secs();
                let mins = total / 60;
                let secs = total % 60;
                eprintln!(
                    "    ⏱ still in subprocess (quiet for {quiet}s, total {mins}m{secs:02}s elapsed)"
                );
                let _ = std::io::stderr().flush();
                tracing::debug!(
                    quiet_secs = quiet,
                    total_secs = total,
                    "drift_orchestrator:subprocess_heartbeat"
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = h_stdout.join();
    let _ = h_stderr.join();

    let success = child
        .wait()
        .map(|s| s.success())
        .unwrap_or(false);

    StepResult {
        success,
        stdout_combined: combined,
    }
}

// ── Argument parsing ─────────────────────────────────────────

fn parse_args(args: &[String]) -> Result<DetectArgs, String> {
    let mut out = DetectArgs::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--code" => {
                let v = args.get(i + 1).ok_or("--code requires a value")?;
                out.code_path = Some(PathBuf::from(v));
                i += 2;
            }
            "--narrative" => {
                let v = args.get(i + 1).ok_or("--narrative requires a value")?;
                out.narrative_paths.push(PathBuf::from(v));
                i += 2;
            }
            "--output" => {
                let v = args.get(i + 1).ok_or("--output requires a value")?;
                out.output = Some(PathBuf::from(v));
                i += 2;
            }
            "--project-id" => {
                let v = args.get(i + 1).ok_or("--project-id requires a value")?;
                out.project_id = Some(v.clone());
                i += 2;
            }
            "--chat-model" => {
                let v = args.get(i + 1).ok_or("--chat-model requires a value")?;
                out.chat_model = Some(v.clone());
                i += 2;
            }
            "--sovereign-bin" => {
                let v = args.get(i + 1).ok_or("--sovereign-bin requires a value")?;
                out.sovereign_bin = Some(v.clone());
                i += 2;
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(out)
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign drift detect",
    summary: "Generate a narrative-vs-code drift report end-to-end. Resilient: idempotent, auto-recovers from common failures, surfaces concrete remediation.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign drift detect --code <path> --narrative <doc>... [--output <md>] [--project-id <id>] [--chat-model <slot>]",
        ),
        crate::util::help::HelpSection::Flags(&[
            ("--code <path>", "Path to the codebase to compare against. Indexed if not already cached."),
            ("--narrative <doc>", "Path to a markdown narrative document. Repeat for multiple. Each becomes its own atlas."),
            ("--output <md>", "Path for the markdown digest. Default: ./drift_report.md (JSON sidecar at <output>.json)."),
            ("--project-id <id>", "Override the corpus id derived from the code path's basename."),
            ("--chat-model <slot>", "Override chat-slot probe (default: fast, fallback: primary). Drift detect targets `fast` by default so it scales to operators without a heavyweight primary model; pass `--chat-model primary` for peak quality at the cost of ~5-10× LLM wall time."),
            ("--sovereign-bin <path>", "Path to the `sovereign` binary used for subprocess fan-out. Default: env SOVEREIGN_BIN, else this binary's own path."),
        ]),
        crate::util::help::HelpSection::Examples(&[
            (
                "sovereign drift detect --code /path/to/repo --narrative /path/to/ARCH.md --narrative /path/to/OVERVIEW.md",
                "Compare two narrative docs against the repo. Re-runs short-circuit on cached steps.",
            ),
        ]),
        crate::util::help::HelpSection::Notes(
            "Idempotent: every step checks for cached output before running. Re-runs after a failure pick up where they left off. Auto-recovers from `enrich build` halting on too-short-section skips by running cluster+name+resolve directly.",
        ),
    ],
};
