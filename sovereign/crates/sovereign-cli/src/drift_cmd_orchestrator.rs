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
use std::process::Command;

const TEMPLATE_RECIPE_DIR: &str = "/Users/alexsbryan/dev/commonwealth-ai/sovereign-recipes/_templates/narrative-markdown";
const SOVEREIGN_BIN: &str = "/Users/alexsbryan/.local/bin/sovereign";

#[derive(Debug, Default)]
struct DetectArgs {
    code_path: Option<PathBuf>,
    narrative_paths: Vec<PathBuf>,
    output: Option<PathBuf>,
    project_id: Option<String>,
    chat_model: Option<String>,
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
    let chat_model = match resolve_chat_model(parsed.chat_model.as_deref()).await {
        Ok(m) => m,
        Err(msg) => {
            eprintln!("✗ chat-model probe failed: {msg}");
            eprintln!();
            eprintln!("  Remediation: confirm `sovereign daemon status` is running and");
            eprintln!("  at least one chat slot is loaded. Try: curl http://localhost:9741/v1/models");
            return 1;
        }
    };
    println!("  ✓ chat model = {chat_model}");

    // ── Step 2: code index (idempotent, with retry on flaky
    //              partition-local race — see task #33/#35) ─────
    if !corpus_exists(&project_id) {
        println!();
        println!("  → indexing code corpus '{project_id}'…");
        if !run_code_index_with_retry(&code_path, &project_id) {
            eprintln!("✗ `sovereign code index` failed after retry.");
            eprintln!("  Remediation: re-run manually:");
            eprintln!("    sovereign code index {} --corpus-id {}", code_path.display(), project_id);
            return 1;
        }
    } else {
        println!("  · code index '{project_id}' already exists (cached)");
    }

    // ── Step 3: structural atlas (idempotent) ────────────────
    let source_corpus = pick_source_corpus(&project_id);
    if !atlas_has_content(&structural_atlas_id) {
        println!("  → building structural atlas '{structural_atlas_id}'…");
        if !run_step(SOVEREIGN_BIN, &["enrich", "ingest", &structural_atlas_id, "--source-corpus", &source_corpus]) {
            eprintln!("✗ structural atlas ingest failed.");
            eprintln!("  Remediation: try `sovereign enrich ingest {} --source-corpus {}` and inspect the error.",
                structural_atlas_id, source_corpus);
            return 1;
        }
        // Stub minimal enrichment config so atlas-cross-corpus
        // accepts this atlas as a peer.
        ensure_structural_enrich_config(&structural_atlas_id);
    } else {
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
    let archaeology_ok = run_step(SOVEREIGN_BIN, &archaeology_args);
    if !archaeology_ok {
        eprintln!(
            "⚠ git-archaeology failed — drift report will skip the Provenance \
             & Evolution section."
        );
    }

    // ── Step 4: per-narrative pipeline ───────────────────────
    let mut narrative_atlas_ids: Vec<String> = Vec::new();
    for narrative_path in &parsed.narrative_paths {
        let nid = basename_id(narrative_path)
            .unwrap_or_else(|| "narrative".into());
        let nid = format!("{project_id}-{nid}");
        println!();
        println!("  ── narrative `{nid}` ──");

        // 4a: stamp recipe (idempotent).
        if !ensure_recipe(&nid, narrative_path) {
            eprintln!("✗ failed to stamp recipe for {nid}.");
            return 1;
        }

        // 4b: install corpus (idempotent).
        if !corpus_exists(&nid) {
            println!("    → installing narrative corpus '{nid}'…");
            if !run_step(SOVEREIGN_BIN, &["corpus", "install", &nid]) {
                eprintln!("✗ corpus install failed.");
                eprintln!("  Remediation: `sovereign corpus install {nid}` and inspect.");
                return 1;
            }
            // The install is async; wait until the meta lands.
            wait_for_corpus(&nid, 60);
        } else {
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
            println!("    → init enrichment for '{nid}'…");
            if !run_step(SOVEREIGN_BIN, &["enrich", "init", &nid, "--from-corpus", &nid, "--pipeline", "literary_atlas"]) {
                eprintln!("✗ enrich init failed.");
                return 1;
            }
            // Pin the resolved chat model so build doesn't fall
            // back to a registered-but-not-loaded id.
            patch_chat_model_in_config(&cfg_path, &chat_model);
        } else {
            println!("    · enrichment config + chapters exist — pinning chat_model={chat_model}");
            patch_chat_model_in_config(&cfg_path, &chat_model);
        }

        // 4d: build atlas (with auto-recovery for partial extract).
        if !atlas_has_content(&nid) {
            println!("    → building narrative atlas (LLM, ~5-30 min)…");
            let build_status = run_step_capture(SOVEREIGN_BIN, &[
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
                    eprintln!("✗ enrich build for {nid} failed: chapter manifest missing.");
                    eprintln!("  Likely cause: a prior orchestrator run errored between writing");
                    eprintln!("  config.json and chapters.json. Wipe and retry:");
                    eprintln!("    rm -rf ~/.sovereign/enrichment/{nid} ~/.sovereign/indexes/{nid}/chapters.json");
                    eprintln!("    sovereign drift detect ...");
                    return 1;
                }
                if build_status.stdout_combined.contains("step `extract` exited") {
                    println!("    ⚠ build halted on partial extract — recovering via cluster + name + resolve…");
                    if !run_step(SOVEREIGN_BIN, &["enrich", "cluster", &nid])
                        || !run_step(SOVEREIGN_BIN, &["enrich", "name", &nid])
                        || !run_step(SOVEREIGN_BIN, &["enrich", "resolve", &nid, "--phase", "all"])
                    {
                        eprintln!("✗ recovery from partial extract failed.");
                        eprintln!("  Remediation: `sovereign enrich errors {nid}` for diagnostics.");
                        return 1;
                    }
                } else {
                    eprintln!("✗ enrich build failed for {nid}.");
                    eprintln!("  Remediation: `sovereign enrich errors {nid}` for diagnostics.");
                    return 1;
                }
            }
        } else {
            println!("    · narrative atlas exists (cached)");
        }
        narrative_atlas_ids.push(nid.clone());

        // 4e: cross-corpus matching.
        let cross_path = cross_corpus_path(&nid);
        if !cross_path.exists() {
            println!("    → matching '{nid}' ↔ '{structural_atlas_id}'…");
            if !run_step(SOVEREIGN_BIN, &["enrich", "atlas-cross-corpus", &nid, &structural_atlas_id]) {
                eprintln!("⚠ cross-corpus match returned non-zero — drift report will continue with whatever edges landed.");
            }
        } else {
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
    let rough_ok = run_step(SOVEREIGN_BIN, &rough_args);
    if !rough_ok {
        eprintln!("⚠ rough-edges scan failed — drift report will skip the Internal section.");
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
    if !run_step(SOVEREIGN_BIN, &drift_args_refs) {
        eprintln!("✗ drift report rendering failed.");
        return 1;
    }

    println!();
    println!("✓ drift report ready: {}", output_path.display());
    0
}

// ── Step helpers ─────────────────────────────────────────────

async fn resolve_chat_model(operator_choice: Option<&str>) -> Result<String, String> {
    let candidates: Vec<String> = if let Some(c) = operator_choice {
        vec![c.to_string(), "primary".into(), "fast".into()]
    } else {
        vec!["primary".into(), "fast".into()]
    };
    for candidate in &candidates {
        if probe_chat(candidate).await {
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
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
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
description = "Stamped by `sovereign drift detect` from {tmpl}."
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
        tmpl = TEMPLATE_RECIPE_DIR,
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
fn run_code_index_with_retry(code_path: &Path, project_id: &str) -> bool {
    const MAX_ATTEMPTS: u32 = 2;
    let path_str = code_path.to_string_lossy();
    let args: [&str; 4] = ["code", "index", &path_str, "--corpus-id"];
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let r = run_step_capture(SOVEREIGN_BIN, &[args[0], args[1], args[2], args[3], project_id]);
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
            return false;
        }
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

fn run_step_capture(bin: &str, args: &[&str]) -> StepResult {
    let out = Command::new(bin).args(args).output();
    match out {
        Ok(o) => {
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&o.stdout));
            combined.push_str(&String::from_utf8_lossy(&o.stderr));
            // Echo to operator so they see progress.
            print!("{}", combined);
            StepResult {
                success: o.status.success(),
                stdout_combined: combined,
            }
        }
        Err(e) => StepResult {
            success: false,
            stdout_combined: format!("subprocess spawn failed: {e}"),
        },
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
            ("--chat-model <slot>", "Override chat-slot probe (default: primary, fallback: fast)."),
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
