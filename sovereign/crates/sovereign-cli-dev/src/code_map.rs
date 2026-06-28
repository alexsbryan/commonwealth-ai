// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign code map <path> [--spec <spec.md>]` — one-shot orchestrator that
//! collapses the whole capability/spec pipeline into a single command with every
//! prerequisite handled. The "just works on my codebase" experience.
//!
//! Today the pipeline is a string of manual verbs spread across two binaries:
//!
//!   sovereign code index <path> --corpus-id <id>     # chunks.lance (this binary)
//!   <build the SCIP call graph>                       # scip_graph.db
//!   <hand-write enrichment/<id>/config.json>          # the worst gotcha
//!   sovereign enrich code-intel <id>                  # per-fn summaries (cli-llm)
//!   sovereign code capability-map <id>                # cluster the call graph
//!   sovereign enrich capability-doc <id>              # narrate each capability
//!   sovereign enrich capability-reconcile <id>        # derived vs the docs
//!   sovereign enrich spec-intel <spec> --corpus <id>  # (optional) spec claims
//!   sovereign enrich spec-reconcile <id> --spec <stem># (optional) spec vs code
//!
//! `code map` runs them in order, deriving the corpus id from the path, probing
//! the daemon, building both index artifacts when they're missing, auto-writing
//! the enrichment config, and printing a clear `[N/M] …` line before each stage
//! so the operator can see movement. It is idempotent + resumable: an already
//! indexed/enriched corpus skips the expensive work and just refreshes the map.
//!
//! ## A seam worth knowing
//!
//! `code index` builds ONLY the LanceDB chunk index — it deliberately leaves
//! `scip_graph.db` alone (that file is owned by the daemon's Reindexer). But
//! `capability-map` needs the SCIP graph. So the index stage here does two
//! things: it runs `code index` for the chunks AND builds the call graph
//! in-process via the same `corpus_engine_scip::scip_export` API that
//! `project init` uses. That is what makes the downstream stages actually work
//! on a fresh repo (the "missing SCIP toolchain" failure mode the operator
//! cares about surfaces here, with install hints).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sovereign_cli_shared::help::{self, Help, HelpSection};

const MAP_HELP: Help = Help {
    command: "sovereign code map",
    summary: "One-shot: index a codebase, then derive + narrate + reconcile its capabilities.",
    sections: &[
        HelpSection::Usage("sovereign code map [<path>] [--spec <spec.md>]"),
        HelpSection::Flags(&[
            (
                "<path>",
                "Repository to map. Default: the current directory. The corpus id is the \
                 directory's base name, lowercased with non-alphanumerics turned into `-`.",
            ),
            (
                "--spec <spec.md>",
                "Also reconcile a written spec against the code: extract its conditioned \
                 claims (spec-intel) and adjudicate each against what the code does \
                 (spec-reconcile).",
            ),
        ]),
        HelpSection::Notes(
            "The flow (each stage prints a `[N/M] …` line):\n\
             1. index      — chunks.lance (via `code index`) + scip_graph.db (in-process SCIP export)\n\
             2. code-intel — a plain-English summary of every function\n\
             3. capability-map      — cluster the call graph into capabilities\n\
             4. capability-doc      — narrate each capability, citing file:line\n\
             5. capability-reconcile— derived capabilities vs the repo's architecture docs\n\
             6-7. (with --spec) spec-intel + spec-reconcile\n\n\
             Requires the Sovereign daemon (run `sovereign setup` once). The enrichment \
             config at <data_dir>/enrichment/<corpus>/config.json is written automatically \
             if absent. Re-running skips indexing/cached work and just refreshes the map.",
        ),
    ],
};

/// Run `code map`. Returns the process exit code.
pub async fn cmd_map(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&MAP_HELP);
        return 0;
    }

    // ── Parse args: a single positional <path> + optional --spec ──
    let mut path_arg: Option<String> = None;
    let mut spec_arg: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(v) = a.strip_prefix("--spec=") {
            spec_arg = Some(v.to_string());
        } else if a == "--spec" {
            i += 1;
            match args.get(i) {
                Some(v) => spec_arg = Some(v.clone()),
                None => {
                    eprintln!("error: --spec requires a value");
                    return 2;
                }
            }
        } else if a.starts_with('-') {
            eprintln!("warning: unknown flag '{a}' — ignored");
        } else if path_arg.is_none() {
            path_arg = Some(a.clone());
        } else {
            eprintln!("warning: ignoring extra positional arg '{a}'");
        }
        i += 1;
    }

    let path = path_arg.unwrap_or_else(|| ".".to_string());
    let abs_path = match Path::new(&path).canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot resolve path {path}: {e}");
            return 1;
        }
    };

    // Validate the spec up front so a typo fails fast, not 20 minutes in.
    // (abs spec path, spec stem = basename without `.md`).
    let spec_info: Option<(String, String)> = match &spec_arg {
        Some(s) => {
            let p = Path::new(s);
            if !p.is_file() {
                eprintln!("error: --spec file not found: {s}");
                return 1;
            }
            let abs = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
            let name = abs
                .file_name()
                .map(|x| x.to_string_lossy().to_string())
                .unwrap_or_else(|| s.clone());
            let stem = name.strip_suffix(".md").unwrap_or(&name).to_string();
            Some((abs.to_string_lossy().to_string(), stem))
        }
        None => None,
    };

    // ── Prep: corpus id from the directory base name ──────────────
    let corpus = corpus_id_from_path(&abs_path);
    println!("corpus-id: {corpus}   (from {})", abs_path.display());

    // ── Prep: resolve data dir + daemon port from SetupConfig ─────
    // One load drives the data dir, the daemon URL we probe, and the
    // base_url we pin into config.json — so every stage agrees on the
    // same daemon. Defaults match a fresh `~/.sovereign` install.
    let cfg = sovereign_core::setup_config::SetupConfig::load().ok();
    let data_dir = cfg
        .as_ref()
        .map(|c| c.data.dir.clone())
        .unwrap_or_else(|| home_dir().join(".sovereign"));
    let port = cfg.as_ref().map(|c| c.daemon.client_port).unwrap_or(9741);
    let base_url = format!("http://localhost:{port}");

    // ── Prep: daemon check (GET /v1/models) ───────────────────────
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let models_url = format!("{base_url}/v1/models");
    print!("checking daemon at {base_url} … ");
    let _ = std::io::stdout().flush();
    let models_json: serde_json::Value = match http.get(&models_url).send().await {
        Ok(r) if r.status().is_success() => {
            println!("ok");
            r.json().await.unwrap_or_else(|_| serde_json::json!({}))
        }
        _ => {
            println!();
            eprintln!("No Sovereign daemon running — run `sovereign setup` first.");
            return 1;
        }
    };
    let embed_model = pick_embed_model(&models_json);
    println!("models: chat=primary  embed={embed_model}");

    // ── Prep: resolve sibling binaries from the running exe's dir ──
    // No reliance on a `sovereign` symlink: `code …` re-invokes THIS
    // binary; `enrich …` invokes the cli-llm sibling next to it.
    let self_bin = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot resolve current executable: {e}");
            return 1;
        }
    };
    let bin_dir = self_bin
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let llm_bin = bin_dir.join("sovereign-cli-llm");
    if !llm_bin.exists() {
        eprintln!(
            "warning: sibling binary not found at {} — the enrich stages will fail.\n         \
             build it with: cargo build --release -p sovereign-cli-llm",
            llm_bin.display()
        );
    }

    let total: u32 = if spec_info.is_some() { 8 } else { 6 };
    let mut step: u32 = 0;

    // ── Stage 1: index (chunks.lance + scip_graph.db) ─────────────
    step += 1;
    let corpus_index_dir = data_dir.join("indexes").join(&corpus);
    let scip_path = corpus_index_dir.join("scip_graph.db");
    let chunks_path = corpus_index_dir.join("chunks.lance");
    let has_scip = scip_path.exists();
    let has_chunks = chunks_path.exists();
    if has_scip && has_chunks {
        println!("[{step}/{total}] Index present — skipping (chunks.lance + scip_graph.db both exist)");
    } else {
        println!("[{step}/{total}] Indexing {} as corpus '{corpus}'…", abs_path.display());
        // chunks.lance — via `code index` (embeds through the daemon).
        if has_chunks {
            println!("    chunks.lance present — skipping chunk index");
        } else {
            let indexes_dir = data_dir.join("indexes");
            let abs_str = abs_path.to_string_lossy().to_string();
            let indexes_str = indexes_dir.to_string_lossy().to_string();
            let _ = std::io::stdout().flush();
            let st = Command::new(&self_bin)
                .args([
                    "code",
                    "index",
                    abs_str.as_str(),
                    "--corpus-id",
                    corpus.as_str(),
                    "--data-dir",
                    indexes_str.as_str(),
                ])
                .status();
            match st {
                Ok(s) if s.success() => {}
                Ok(s) => {
                    eprintln!(
                        "\n✗ stage {step} failed: `code index` exited with code {} \
                         (see output above — e.g. daemon embedding errors). Stopping.",
                        s.code().unwrap_or(-1)
                    );
                    return s.code().unwrap_or(1);
                }
                Err(e) => {
                    eprintln!("\n✗ stage {step} could not launch `code index`: {e}. Stopping.");
                    return 1;
                }
            }
        }
        // scip_graph.db — built in-process (this is the prerequisite
        // `code index` does NOT produce; see the module docs).
        if has_scip {
            println!("    scip_graph.db present — skipping call-graph build");
        } else if let Err((msg, code)) = build_scip_graph(&abs_path, &corpus, &scip_path).await {
            eprintln!("\n✗ stage {step} failed while building the call graph:\n{msg}");
            return code;
        }
    }

    // ── Prep: auto-write the enrichment config (kills the gotcha) ──
    let enrich_cfg_path = data_dir.join("enrichment").join(&corpus).join("config.json");
    if enrich_cfg_path.exists() {
        println!("enrich config present — keeping {}", enrich_cfg_path.display());
    } else {
        match write_enrich_config(
            &enrich_cfg_path,
            &corpus,
            &abs_path.to_string_lossy(),
            &embed_model,
            &base_url,
        ) {
            Ok(()) => println!("wrote enrich config {}", enrich_cfg_path.display()),
            Err(e) => {
                eprintln!(
                    "✗ could not write enrich config {}: {e}. Stopping.",
                    enrich_cfg_path.display()
                );
                return 1;
            }
        }
    }

    // ── Stage 2: code-intel (per-function summaries) ──────────────
    // SOVEREIGN_ENRICH_SKIP_INDEX=1 keeps this from re-opening the
    // chunk index per symbol (avoids a daemon index-storm); summaries
    // still land in code_intel_cache.json, which capability-doc reads.
    step += 1;
    if let Err(rc) = run_stage(
        step,
        total,
        "Summarizing functions (code-intel)…",
        &llm_bin,
        &["enrich", "code-intel", corpus.as_str()],
        &[("SOVEREIGN_ENRICH_SKIP_INDEX", "1")],
    ) {
        return rc;
    }

    // ── Stage 3: capability-map (cluster the call graph) ──────────
    step += 1;
    if let Err(rc) = run_stage(
        step,
        total,
        "Deriving capability map…",
        &self_bin,
        &["code", "capability-map", corpus.as_str()],
        &[],
    ) {
        return rc;
    }

    // ── Stage 4: capability-doc (narrate each capability) ─────────
    step += 1;
    if let Err(rc) = run_stage(
        step,
        total,
        "Narrating architecture doc…",
        &llm_bin,
        &["enrich", "capability-doc", corpus.as_str()],
        &[],
    ) {
        return rc;
    }

    // ── Stage 5: capability-reconcile (derived vs the docs) ───────
    step += 1;
    if let Err(rc) = run_stage(
        step,
        total,
        "Reconciling capabilities against docs…",
        &llm_bin,
        &["enrich", "capability-reconcile", corpus.as_str()],
        &[],
    ) {
        return rc;
    }

    // ── Stages 6-7: spec reconciliation (optional) ────────────────
    // Stages 6-7 are BEST-EFFORT: a spec that yields no checkable claims (e.g. a
    // doc describing no system behavior) must not sink the capability report, which
    // is the main deliverable. Warn and continue rather than aborting the command.
    if let Some((spec_abs, spec_stem)) = &spec_info {
        step += 1;
        let intel_ok = run_stage(
            step,
            total,
            "Extracting spec claims (spec-intel)…",
            &llm_bin,
            &["enrich", "spec-intel", spec_abs.as_str(), "--corpus", corpus.as_str()],
            &[],
        )
        .is_ok();
        step += 1;
        if intel_ok {
            if run_stage(
                step,
                total,
                "Reconciling spec against code (spec-reconcile)…",
                &llm_bin,
                &["enrich", "spec-reconcile", corpus.as_str(), "--spec", spec_stem.as_str()],
                &[],
            )
            .is_err()
            {
                eprintln!("  ⚠ spec reconcile found nothing to check (the spec may state no checkable behavior); the capability report below still stands.");
            }
        } else {
            eprintln!("  ⚠ spec-intel failed; skipping spec reconcile. The capability report below still stands.");
        }
    }

    // ── Final: report ─────────────────────────────────────────────
    step += 1;
    let caps_dir = data_dir.join("capabilities").join(&corpus);
    let doc_md = caps_dir.join("capability_doc.md");
    let findings_md = caps_dir.join("capability_findings.md");
    println!("[{step}/{total}] Done — capability map for '{corpus}'");
    println!();
    println!("  ─────────────────────────────────────────────");
    println!("  Architecture doc:    {}", doc_md.display());
    println!("  Reconciliation:      {}", findings_md.display());
    if let Some((_, spec_stem)) = &spec_info {
        let spec_findings = data_dir
            .join("specs")
            .join(&corpus)
            .join(spec_stem)
            .join("spec_findings.md");
        println!("  Spec findings:       {}", spec_findings.display());
    }
    if let Some(tally) = tally_from_findings(&findings_md) {
        println!();
        println!("  {tally}");
    }
    println!();
    println!("  Open it:  {}", doc_md.display());
    0
}

/// Directory base name → corpus id: lowercase, non-alphanumerics → `-`.
/// e.g. `/home/me/My Repo` → `my-repo`.
fn corpus_id_from_path(p: &Path) -> String {
    let base = p
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("codebase");
    base.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// The embedding model id advertised on `/v1/models` (first id containing
/// "Embed"), falling back to the standard `Qwen3-Embedding-0.6B-Q8_0`.
fn pick_embed_model(models: &serde_json::Value) -> String {
    models
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|s| s.as_str()))
                .find(|id| id.contains("Embed"))
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "Qwen3-Embedding-0.6B-Q8_0".to_string())
}

/// Write `enrichment/<corpus>/config.json` in the exact shape the enrich
/// pipeline expects for a code corpus. `chat_model` is the literal alias
/// `"primary"`, which the daemon resolves to the loaded primary model.
fn write_enrich_config(
    path: &Path,
    corpus: &str,
    source_abs: &str,
    embed_model: &str,
    base_url: &str,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let cfg = serde_json::json!({
        "schema_version": 1,
        "corpus_id": corpus,
        "pipeline_id": "code_intel",
        "source_path": source_abs,
        "chapter_regex": "",
        "chat_model": "primary",
        "embed_model": embed_model,
        "base_url": base_url,
        "created_at": now,
    });
    let body = serde_json::to_string_pretty(&cfg).unwrap_or_else(|_| cfg.to_string());
    std::fs::write(path, body)
}

/// Build the SCIP call graph (`scip_graph.db`) in-process, mirroring what
/// `project init`'s "Building call graph" step does. Returns `Err((message,
/// exit_code))` on a clean, actionable failure (e.g. no exporter installed).
async fn build_scip_graph(
    repo: &Path,
    corpus_id: &str,
    scip_path: &Path,
) -> Result<(), (String, i32)> {
    use corpus_engine_scip::scip_export::{check_exporters, export_all, ScipProgress};

    println!("    Building call graph (SCIP) — runs rust-analyzer; can take a few minutes…");
    let _ = std::io::stdout().flush();

    let roots = vec![repo.to_path_buf()];
    let check = check_exporters(&roots);
    if check.available.is_empty() {
        let mut msg =
            String::from("  no SCIP exporter found in PATH — cannot build the call graph.\n");
        for m in &check.missing {
            msg.push_str(&format!(
                "    {} exporter ({}) missing — {}\n",
                m.language_id, m.command, m.install_hint
            ));
        }
        msg.push_str("  Install one of the above, then re-run `sovereign code map`.");
        return Err((msg, 1));
    }
    for e in &check.available {
        println!("    using {}", e.command);
    }

    if let Some(parent) = scip_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let graph = match corpus_engine_scip::ScipGraph::open(scip_path, corpus_id) {
        Ok(g) => g,
        Err(e) => {
            return Err((
                format!("  cannot open SCIP graph at {}: {e}", scip_path.display()),
                1,
            ))
        }
    };

    let out_dir = std::env::temp_dir().join(format!("sovereign-code-map-{}-scip", std::process::id()));
    let _ = std::fs::create_dir_all(&out_dir);

    let progress = |p: ScipProgress<'_>| match p {
        ScipProgress::Exporting { language } => {
            eprint!("\r    exporting {language}…        ");
            let _ = std::io::stderr().flush();
        }
        ScipProgress::Ingested {
            language,
            symbols,
            refs,
        } => {
            eprintln!("\r    ✓ {language}: {symbols} symbols, {refs} references    ");
        }
        ScipProgress::Skipped { language, reason } => {
            eprintln!("\r    ⚠ {language}: skipped ({reason})    ");
        }
    };

    let result = export_all(repo, &out_dir, &graph, Some(&roots), &progress).await;
    let _ = std::fs::remove_dir_all(&out_dir);
    match result {
        Ok(summary) => {
            println!(
                "    ✓ SCIP graph: {} symbols, {} call edges",
                summary.total_symbols, summary.total_refs
            );
            if summary.total_symbols == 0 {
                eprintln!(
                    "    ⚠ 0 symbols indexed — the capability map will be empty. \
                     Is this a supported language with its exporter installed?"
                );
            }
            Ok(())
        }
        Err(e) => Err((format!("  SCIP export failed: {e}"), 1)),
    }
}

/// Print a `[step/total] label` header, run `bin args` (env overlaid) with
/// inherited stdio so the child's progress streams, and translate the exit
/// status into `Ok(())` / `Err(exit_code)`.
fn run_stage(
    step: u32,
    total: u32,
    label: &str,
    bin: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(), i32> {
    println!("[{step}/{total}] {label}");
    let _ = std::io::stdout().flush();
    let mut cmd = Command::new(bin);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => {
            eprintln!(
                "\n✗ stage {step} failed: `{}` exited with code {}. Stopping.\n  command: {} {}",
                label,
                s.code().unwrap_or(-1),
                bin.display(),
                args.join(" "),
            );
            Err(s.code().unwrap_or(1))
        }
        Err(e) => {
            eprintln!(
                "\n✗ stage {step} could not be launched: {e}. Stopping.\n  binary: {}",
                bin.display()
            );
            Err(1)
        }
    }
}

/// Pull the "N corroborated · N undocumented · N drifted" tally out of
/// `capability_findings.md` (written by `enrich capability-reconcile`).
fn tally_from_findings(md_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(md_path).ok()?;
    let line = text
        .lines()
        .find(|l| l.contains("corroborated") && l.contains("drifted"))?;
    let after = line.split_once("— ").map(|(_, b)| b).unwrap_or(line);
    let core = after.split_once(". Regenerate").map(|(a, _)| a).unwrap_or(after);
    Some(core.trim().trim_end_matches('.').trim().to_string())
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}
