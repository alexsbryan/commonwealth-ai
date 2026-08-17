// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn recipe` subcommand handlers.
//!
//! Provides two commands that don't require a loaded inference model:
//!
//!   svrn recipe test <path>      [--sample-size N] [--output path]
//!                                     [--no-embed] [--verbose] [--offline]
//!   svrn recipe validate <path>  [--offline]
//!
//! Both commands use a stub `EmbedFn` that returns zero-vectors. Embedding
//! is always disabled (`--no-embed`) in this code path because loading an
//! inference model requires `--model`, which is handled by the main REPL
//! entry point. Run `svrn recipe test --embed` with a model to enable
//! the embed + search phase — that workflow is not yet supported here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::harness::{capture, FrozenSample, HarnessRunner};
use corpus_engine::{CorpusEngine, EmbedFn, Recipe, RecipeRegistry, TestOptions};
use sovereign_eval::authoring_harness::{render_report, run_deterministic, Declaration};

// ── Public entry points ─────────────────────────────────────────────────────

/// Run a `recipe` subcommand. Returns the exit code.
pub async fn run_recipe(args: &[String]) -> i32 {
    if args.is_empty() {
        sovereign_cli_shared::help::print(&HELP);
        return 1;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        sovereign_cli_shared::help::print(&HELP);
        return 0;
    }

    match args[0].as_str() {
        "test" => cmd_test(&args[1..]).await,
        "validate" => cmd_validate(&args[1..]).await,
        "list" => cmd_list(&args[1..]).await,
        "publish" => cmd_publish(&args[1..]).await,
        other => {
            eprintln!("Unknown recipe subcommand: {other}");
            sovereign_cli_shared::help::print(&HELP);
            1
        }
    }
}

const HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn recipe",
    summary: "Run corpus ingestion recipes: test, validate, list.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn recipe <subcommand> [args]"),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[
            ("list", "List all corpora available in the registry"),
            (
                "test <path>",
                "Run the full test harness against a recipe file",
            ),
            (
                "validate <path>",
                "Validate recipe fields without downloading data",
            ),
            ("publish <path>", "Add a recipe to the local user registry"),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "`list` takes --offline (skip live registry refresh).\n\
             `test` takes --sample-size N, --output <path>, --offline, --verbose, \
             --params k=v[,...], --params-file <json>.\n\
             `validate` takes --offline.\n\
             `publish` writes to ~/.svrnmesh/recipes/registry.toml; pass \
             --submit-pr to also draft a community-registry PR via `gh`.",
        ),
    ],
};

// ── `recipe test` ───────────────────────────────────────────────────────────

async fn cmd_test(args: &[String]) -> i32 {
    let mut recipe_path: Option<PathBuf> = None;
    let mut sample_size: usize = 50;
    let mut output: Option<PathBuf> = None;
    let mut recapture = false;
    let mut json = false;
    let mut enrich_flag = false;
    let mut parameters: std::collections::BTreeMap<String, toml::Value> =
        std::collections::BTreeMap::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--sample-size" => {
                i += 1;
                if let Some(n) = args.get(i).and_then(|s| s.parse().ok()) {
                    sample_size = n;
                } else {
                    eprintln!("error: --sample-size requires a number");
                    return 1;
                }
            }
            "--output" => {
                i += 1;
                output = args.get(i).map(PathBuf::from);
            }
            "--params" | "--param" => {
                i += 1;
                let Some(spec) = args.get(i) else {
                    eprintln!("error: {} requires a `key=value` argument", args[i - 1]);
                    return 1;
                };
                if let Err(e) = parse_test_param_spec(spec, &mut parameters) {
                    eprintln!("error: invalid {}: {e}", args[i - 1]);
                    return 1;
                }
            }
            "--params-file" => {
                i += 1;
                let Some(p) = args.get(i) else {
                    eprintln!("error: --params-file requires a path argument");
                    return 1;
                };
                let path = PathBuf::from(p);
                let raw = match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!(
                            "error: failed to read --params-file {}: {e}",
                            path.display()
                        );
                        return 1;
                    }
                };
                let from_file: std::collections::BTreeMap<String, serde_json::Value> =
                    match serde_json::from_str(&raw) {
                        Ok(m) => m,
                        Err(e) => {
                            eprintln!(
                                "error: --params-file {} is not a JSON object: {e}",
                                path.display()
                            );
                            return 1;
                        }
                    };
                for (k, v) in from_file {
                    if let Some(toml_v) = json_value_to_toml(&v) {
                        parameters.entry(k).or_insert(toml_v);
                    }
                }
            }
            "--recapture" => recapture = true,
            "--json" => json = true,
            "--enrich" => enrich_flag = true,
            flag if flag.starts_with('-') => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            path => {
                recipe_path = Some(PathBuf::from(path));
            }
        }
        i += 1;
    }

    let recipe_path = match recipe_path {
        Some(p) => p,
        None => {
            eprintln!("error: missing recipe path");
            eprintln!(
                "Usage: svrn recipe test <path> [--sample-size N] [--recapture] [--json] \
                 [--enrich] [--params k=v[,...]]... [--params-file <json>] [--output path]"
            );
            return 1;
        }
    };

    let engine = build_stub_engine();

    // ── Load + resolve the recipe ────────────────────────────────────────────
    let mut recipe = match Recipe::from_file(&recipe_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "error: failed to load recipe {}: {e}",
                recipe_path.display()
            );
            return 1;
        }
    };
    if !recipe.parameters.is_empty() || !parameters.is_empty() {
        match recipe.resolve_parameters(&parameters) {
            Ok(resolved) => recipe = recipe.with_resolved_parameters(resolved),
            Err(e) => {
                eprintln!("error: parameter resolution failed: {e}");
                eprintln!(
                    "  supply values with --params key=value (repeatable) or --params-file <json>"
                );
                return 1;
            }
        }
    }

    // ── Frozen sample: capture once (the one networked step), then iterate ───
    let Some(harness_root) = harness_root_for(&recipe.corpus.id) else {
        eprintln!("error: cannot resolve home directory for the harness sample store");
        return 1;
    };
    let need_capture = recapture || !harness_root.join("capture.json").exists();
    if need_capture {
        if recapture {
            let _ = std::fs::remove_dir_all(&harness_root);
        }
        eprintln!("❄  Capturing a frozen sample (the one networked step)…");
        match capture(&engine, &recipe, &harness_root, sample_size).await {
            Ok(m) => eprintln!(
                "❄  Froze {} docs from {} — sample {}. Future runs are offline; --recapture to refresh.",
                m.docs.len(),
                m.acquirer,
                short_hash(&m.sample_id),
            ),
            Err(e) => {
                eprintln!("error: capture failed: {e}");
                return 1;
            }
        }
    }

    // ── Run the deterministic rungs over the frozen sample (model-free) ──────
    let frozen = match FrozenSample::load(&harness_root) {
        Ok(Some(f)) => f,
        Ok(None) => {
            eprintln!("error: no frozen sample found after capture");
            return 1;
        }
        Err(e) => {
            eprintln!("error: failed to load frozen sample: {e}");
            return 1;
        }
    };
    let work_dir = std::env::temp_dir().join(format!("harness-run-{}", recipe.corpus.id));
    let runner = HarnessRunner::new(&engine, &recipe, &frozen);
    let outputs = match runner.run(&work_dir, sample_size).await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: harness run failed: {e}");
            return 1;
        }
    };

    // ── Rung 6 (opt-in): ingest+enrich the frozen sample through the REAL
    //    pipeline (SSOT — `engine.ingest`, not a reimplementation) with a
    //    daemon-backed engine, then verify the integrity of the atoms it
    //    produced. Reads the frozen materialized source (I3 — no re-acquire).
    let enrich = if enrich_flag {
        match run_enrich_and_verify(&recipe, &frozen).await {
            Ok(e) => e,
            Err(msg) => {
                eprintln!("ℹ  --enrich skipped: {msg}");
                None
            }
        }
    } else {
        None
    };

    let run = run_deterministic(
        &frozen.manifest,
        &recipe,
        &outputs,
        enrich.as_ref(),
        &Declaration::default(),
    );

    // ── Report ───────────────────────────────────────────────────────────────
    println!("{}", render_report(&run));
    if let Some(path) = output {
        if let Err(e) = std::fs::write(&path, render_report(&run)) {
            eprintln!("warning: failed to write report to {}: {e}", path.display());
        } else {
            eprintln!("Report written to {}", path.display());
        }
    }
    if json {
        match serde_json::to_string(&run) {
            Ok(line) => println!("{line}"),
            Err(e) => eprintln!("warning: failed to serialize harness run to JSON: {e}"),
        }
    }

    if run.green() {
        0
    } else {
        1
    }
}

/// `~/.svrnmesh/harness/<recipe-id>/` — the content-addressed frozen-sample
/// store for the authoring harness.
fn harness_root_for(recipe_id: &str) -> Option<PathBuf> {
    Some(
        sovereign_contracts::rebrand::svrnmesh_root()
            .join("harness")
            .join(recipe_id),
    )
}

fn short_hash(h: &str) -> &str {
    if h.len() >= 8 {
        &h[..8]
    } else {
        h
    }
}

/// Ingest + enrich the frozen sample through the real `engine.ingest` pipeline
/// (SSOT — not a reimplementation) with a daemon-backed engine, then verify the
/// integrity of the atoms it produced. Reads the frozen materialized source, so
/// no network runs (I3). Returns `Ok(None)` when there's nothing to verify;
/// `Err(msg)` on a setup/ingest failure (the caller reports + skips the rung).
async fn run_enrich_and_verify(
    recipe: &Recipe,
    frozen: &FrozenSample,
) -> Result<Option<corpus_engine::harness::EnrichOutput>, String> {
    if !recipe.enrichment.as_ref().is_some_and(|e| e.enabled) {
        return Err(format!(
            "recipe '{}' declares no [enrichment] — nothing to enrich or verify",
            recipe.corpus.id
        ));
    }

    // The daemon's loaded models are the SSOT for what to call; the daemon URL
    // comes from the canonical port constant + builder (one place owns the
    // port — see `sovereign_cli_shared::urls`), not a hand-written literal.
    let v1 = sovereign_cli_shared::urls::v1_url(sovereign_cli_shared::urls::DEFAULT_CLIENT_PORT);
    let (chat_model, embed_model) = resolve_daemon_models(&v1).await?;

    // Daemon-backed engine via the canonical provider + adapters (SSOT — the
    // same path `chat` bootstraps).
    let provider: Arc<dyn sovereign_core::traits::InferenceProvider> =
        Arc::new(crate::chat_cmd::bootstrap::SplitInferenceProvider::new(
            &v1,
            chat_model,
            embed_model.clone(),
            8192,
            sovereign_core::models_manifest::DEFAULT_MANIFEST.embed_query_instruction(&embed_model),
        ));
    let embed_fn = sovereign_tools::corpus::inference_to_embed_fn(Arc::clone(&provider));
    let inference_fn = sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&provider));

    // Reconstruct the frozen source (I3 — no network) and point an inline recipe
    // at it, so `ingest` runs over exactly the frozen bytes.
    let work = std::env::temp_dir().join(format!("harness-enrich-{}", recipe.corpus.id));
    let _ = std::fs::remove_dir_all(&work);
    let src_dir = work.join("src");
    std::fs::create_dir_all(&src_dir).map_err(|e| e.to_string())?;
    let materialized = frozen.materialize(&src_dir).map_err(|e| e.to_string())?;

    let mut enrich_recipe = recipe.clone();
    enrich_recipe.acquire = corpus_engine::recipe::AcquirerConfig::LocalFile {
        path: materialized.to_string_lossy().into_owned(),
    };

    let index_root = work.join("indexes");
    let engine = CorpusEngine::new(work.join("recipes"), index_root.clone(), embed_fn)
        .with_embedding_model(&embed_model)
        .with_inference_fn(inference_fn);

    eprintln!(
        "⚙  --enrich: ingesting + enriching the frozen sample via the daemon (corpus '{}')…",
        recipe.corpus.id
    );
    engine
        .ingest(
            &corpus_engine::CorpusSpec::Inline(Box::new(enrich_recipe)),
            None,
        )
        .await
        .map_err(|e| format!("ingest+enrich failed: {e}"))?;

    corpus_engine::harness::verify_atoms_at(&index_root.join(&recipe.corpus.id))
        .await
        .map_err(|e| e.to_string())
}

/// Resolve the daemon's loaded chat + embed model ids from `/v1/models` (the
/// SSOT for what's actually serving): the embed model's id contains "embed",
/// the chat model is the other.
async fn resolve_daemon_models(v1: &str) -> Result<(String, String), String> {
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{v1}/models"))
        .send()
        .await
        .map_err(|e| format!("daemon /v1/models unreachable ({e}); is the daemon running?"))?
        .json()
        .await
        .map_err(|e| format!("parse /v1/models: {e}"))?;
    let ids: Vec<String> = body["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m["id"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let embed = ids
        .iter()
        .find(|id| id.to_lowercase().contains("embed"))
        .cloned()
        .ok_or_else(|| "daemon advertises no embedding model".to_string())?;
    let chat = ids
        .iter()
        .find(|id| !id.to_lowercase().contains("embed"))
        .cloned()
        .ok_or_else(|| "daemon advertises no chat model".to_string())?;
    Ok((chat, embed))
}

// ── `recipe validate` ───────────────────────────────────────────────────────

async fn cmd_validate(args: &[String]) -> i32 {
    let mut recipe_path: Option<PathBuf> = None;
    let mut offline = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--offline" => offline = true,
            flag if flag.starts_with('-') => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            path => {
                recipe_path = Some(PathBuf::from(path));
            }
        }
        i += 1;
    }

    let recipe_path = match recipe_path {
        Some(p) => p,
        None => {
            eprintln!("error: missing recipe path");
            eprintln!("Usage: svrn recipe validate <path> [--offline]");
            return 1;
        }
    };

    let engine = build_stub_engine();
    let options = TestOptions {
        sample_size: 0, // validation-only
        embed: false,
        offline,
        ..Default::default()
    };

    eprintln!("Validating recipe: {}", recipe_path.display());

    match engine.test_recipe(&recipe_path, &options).await {
        Ok(report) => {
            if report.validation.errors.is_empty() {
                // A VALIDATOR'S VERDICT IS ITS PAYLOAD, so it goes to stdout —
                // the 17th site of the payload-vs-narration census (note
                // f5acdf59). `Validating recipe: …` above stays on stderr
                // (narration) and the failure list below stays on stderr
                // (diagnostics), which is the same seam `mcp test` uses: the
                // inventory on stdout, the progress line on stderr.
                //
                // Not cosmetic. The `recipe-author` journey's first step is a
                // READ, and a read whose whole output is on stderr can only ever
                // be gated on its exit code — which is exactly the class of
                // assertion this repo has learned not to trust.
                println!("✓ Validation passed");
                if !report.validation.warnings.is_empty() {
                    for w in &report.validation.warnings {
                        eprintln!("  ⚠  {w}");
                    }
                }
                0
            } else {
                eprintln!("✗ Validation failed:");
                for e in &report.validation.errors {
                    eprintln!("  - {e}");
                }
                1
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

// ── `recipe list` ───────────────────────────────────────────────────────────

async fn cmd_list(args: &[String]) -> i32 {
    let mut offline = false;

    for arg in args {
        match arg.as_str() {
            "--offline" => offline = true,
            flag if flag.starts_with('-') => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            _ => {}
        }
    }

    // Always merge the user's local registry so published recipes
    // surface alongside upstream entries.
    let local_dir = RecipeRegistry::default_local_recipes_dir();
    let mut registry = RecipeRegistry::from_bundled(local_dir.clone());
    if let Some(dir) = &local_dir {
        let path = dir.join("registry.toml");
        registry = registry.with_local_registry(&path);
    }

    if !offline {
        registry.refresh().await;
    }

    let entries = registry.list_entries();
    if entries.is_empty() {
        eprintln!("No corpora found in registry.");
        return 0;
    }

    // Header — adds an "Origin" column so user-published recipes
    // are visually distinguishable from upstream/bundled.
    println!(
        "{:<16} {:<40} {:<14} {:>8} {:>8}  {:<8}",
        "ID", "Name", "License", "Compressed", "Indexed", "Origin",
    );
    println!("{}", "-".repeat(98));

    for entry in &entries {
        let origin = if registry.is_local_entry(&entry.id) {
            "(local)"
        } else {
            ""
        };
        println!(
            "{:<16} {:<40} {:<14} {:>7.0}GB {:>7.0}GB  {:<8}",
            entry.id,
            if entry.name.len() > 39 {
                &entry.name[..39]
            } else {
                &entry.name
            },
            if entry.license.len() > 13 {
                &entry.license[..13]
            } else {
                &entry.license
            },
            entry.size_compressed_gb,
            entry.size_indexed_gb,
            origin,
        );
    }

    println!();
    println!("{} corpus/corpora in registry", entries.len());
    if offline {
        println!("(offline mode — showing bundled snapshot)");
    }

    0
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Build a `CorpusEngine` with a stub embed function.
///
/// The stub returns zero-vectors; it is never called when `embed = false`.
fn build_stub_engine() -> CorpusEngine {
    let stub_embed: EmbedFn =
        Arc::new(|_text| Box::pin(async { Ok(vec![0f32; corpus_engine::DEFAULT_EMBED_DIM]) }));

    // Use a temporary location for downloads; the engine's index_dir is
    // unused since we never write a production index.
    let tmp = std::env::temp_dir().join("sovereign-recipe-test");
    let engine = CorpusEngine::new(tmp.clone(), tmp, stub_embed);
    // `recipe test`'s one networked step is the frozen-sample capture,
    // which runs the recipe's real acquirer. A recipe naming a custom
    // kind is untestable unless that kind is registered here too.
    sovereign_tools::sec_edgar::register(&engine);
    engine
}

// ── `recipe publish` ─────────────────────────────────────────────────────────

/// `svrn recipe publish <path> [--submit-pr]`
///
/// Adds a recipe to the user's local registry at
/// `~/.svrnmesh/recipes/registry.toml` and copies the recipe
/// TOML to `~/.svrnmesh/recipes/<id>/recipe.toml`. The next
/// `svrn corpus install <id>` (or desktop "Add Knowledge
/// Source → Browse") will pick it up via the
/// [`RecipeRegistry::with_local_registry`] merge.
///
/// Validates the recipe before publishing — a bad regex or
/// undeclared parameter placeholder fails the publish so a broken
/// recipe doesn't pollute the registry.
///
/// `--submit-pr` opens a draft pull request against the upstream
/// `sovereign-recipes` repo via `gh`. Requires `gh` on PATH; the
/// flag is opt-in to avoid surprising GitHub interactions.
async fn cmd_publish(args: &[String]) -> i32 {
    let mut recipe_path: Option<PathBuf> = None;
    let mut submit_pr = false;
    let mut force = false;

    let iter = args.iter();
    for a in iter {
        match a.as_str() {
            "--submit-pr" => submit_pr = true,
            "--force" | "-f" => force = true,
            "--help" | "-h" => {
                println!(
                    "Usage: svrn recipe publish <path> [--submit-pr] [--force]\n\n\
                     Adds a recipe to ~/.svrnmesh/recipes/registry.toml and copies \
                     the TOML to ~/.svrnmesh/recipes/<id>/recipe.toml. The recipe \
                     is validated first; pass --force to skip validation."
                );
                return 0;
            }
            flag if flag.starts_with('-') => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            path => {
                recipe_path = Some(PathBuf::from(path));
            }
        }
    }

    let Some(recipe_path) = recipe_path else {
        eprintln!("error: missing recipe path");
        eprintln!("Usage: svrn recipe publish <path> [--submit-pr] [--force]");
        return 1;
    };

    let recipe = match corpus_engine::Recipe::from_file(&recipe_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: failed to parse {}: {e}", recipe_path.display());
            return 1;
        }
    };

    if !force {
        // Run the same validate path as `recipe validate` to catch
        // bad regexes, undeclared placeholders, etc.
        let engine = build_stub_engine();
        let options = TestOptions {
            sample_size: 0,
            embed: false,
            offline: true,
            ..Default::default()
        };
        match engine.test_recipe(&recipe_path, &options).await {
            Ok(report) if report.validation.errors.is_empty() => {
                if !report.validation.warnings.is_empty() {
                    for w in &report.validation.warnings {
                        eprintln!("  ⚠  {w}");
                    }
                }
            }
            Ok(report) => {
                eprintln!("✗ Validation failed; not publishing:");
                for e in &report.validation.errors {
                    eprintln!("  - {e}");
                }
                eprintln!("Pass --force to publish anyway.");
                return 1;
            }
            Err(e) => {
                eprintln!("error: validation phase failed: {e}");
                return 1;
            }
        }
    }

    let Some(local_dir) = corpus_engine::RecipeRegistry::default_local_recipes_dir() else {
        eprintln!("error: HOME environment variable is not set; cannot resolve local recipes dir");
        return 1;
    };

    let raw = match std::fs::read_to_string(&recipe_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read {}: {e}", recipe_path.display());
            return 1;
        }
    };
    let sha256 = sha256_hex(raw.as_bytes());

    if let Err(e) = std::fs::create_dir_all(&local_dir) {
        eprintln!("error: failed to create {}: {e}", local_dir.display());
        return 1;
    }
    let recipe_dir = local_dir.join(&recipe.corpus.id);
    if let Err(e) = std::fs::create_dir_all(&recipe_dir) {
        eprintln!("error: failed to create {}: {e}", recipe_dir.display());
        return 1;
    }
    let dest_recipe_path = recipe_dir.join("recipe.toml");
    if let Err(e) = std::fs::write(&dest_recipe_path, &raw) {
        eprintln!(
            "error: failed to copy recipe to {}: {e}",
            dest_recipe_path.display()
        );
        return 1;
    }

    let registry_path = local_dir.join("registry.toml");
    if let Err(e) = upsert_local_registry_entry(&registry_path, &recipe, &sha256) {
        eprintln!("error: failed to update registry: {e}");
        return 1;
    }

    // Record the publish marker so the audit-time nudge knows to
    // stop offering this recipe for publishing.
    let markers_path = sovereign_contracts::rebrand::svrnmesh_root().join("published_recipes.json");
    if let Err(e) = record_publish_marker(&markers_path, &recipe.corpus.id, &sha256) {
        eprintln!("warning: failed to record publish marker: {e}");
    }

    println!("Published `{}` to local registry.", recipe.corpus.id);
    println!("  Recipe TOML:  {}", dest_recipe_path.display());
    println!("  Registry:     {}", registry_path.display());
    println!("  SHA-256:      {sha256}");
    println!();
    println!("Install with:");
    println!("  svrn corpus install {}", recipe.corpus.id);

    if submit_pr {
        if let Err(e) = submit_upstream_pr(&recipe, &dest_recipe_path) {
            eprintln!("warning: --submit-pr failed: {e}");
            return 1;
        }
    } else {
        println!();
        println!("Share with the community (optional):");
        println!("  1. Fork the sovereign-recipes repo on GitHub.");
        println!(
            "  2. Copy the recipe to <fork>/{}/recipe.toml",
            recipe.corpus.id
        );
        println!("  3. Add an entry to registry.toml with sha256 = \"{sha256}\".");
        println!("  4. Open a PR. Or pass `--submit-pr` next time to draft it via `gh`.");
    }
    0
}

/// Insert (or update) an entry in `~/.svrnmesh/recipes/registry.toml`
/// for the recipe just published. Reads the existing TOML, removes
/// any prior entry with the same id, appends the new one, writes
/// atomically.
fn upsert_local_registry_entry(
    registry_path: &Path,
    recipe: &corpus_engine::Recipe,
    sha256: &str,
) -> std::io::Result<()> {
    use std::io::Write;

    let existing = std::fs::read_to_string(registry_path).unwrap_or_default();
    let mut snapshot: corpus_engine::RegistrySnapshot = if existing.is_empty() {
        corpus_engine::RegistrySnapshot {
            schema_version: 1,
            generated_at: rfc3339_now(),
            registry_url: String::new(),
            entries: Vec::new(),
        }
    } else {
        toml::from_str(&existing).unwrap_or_else(|_| corpus_engine::RegistrySnapshot {
            schema_version: 1,
            generated_at: rfc3339_now(),
            registry_url: String::new(),
            entries: Vec::new(),
        })
    };

    snapshot.entries.retain(|e| e.id != recipe.corpus.id);
    snapshot.entries.push(corpus_engine::RegistryEntry {
        id: recipe.corpus.id.clone(),
        name: recipe.corpus.name.clone(),
        description: recipe.corpus.description.clone(),
        license: recipe.corpus.license.clone(),
        size_compressed_gb: recipe.corpus.size_compressed_gb,
        size_indexed_gb: recipe.corpus.size_indexed_gb,
        toml_url: format!("file://{}/recipe.toml", recipe.corpus.id),
        sha256: sha256.to_string(),
        enrichment_enabled: recipe
            .enrichment
            .as_ref()
            .map(|e| e.enabled)
            .unwrap_or(false),
        mesh_sharing: recipe.corpus.mesh_sharing,
        prebuilt: None,
        parent_corpus_id: recipe.corpus.parent_corpus_id.clone(),
        catalog_status: None,
    });
    snapshot.generated_at = rfc3339_now();

    let serialized =
        toml::to_string_pretty(&snapshot).map_err(|e| std::io::Error::other(format!("{e}")))?;
    let part = registry_path.with_extension("toml.part");
    {
        let mut f = std::fs::File::create(&part)?;
        f.write_all(serialized.as_bytes())?;
    }
    std::fs::rename(&part, registry_path)?;
    Ok(())
}

/// Record a publish marker so `svrn project audit` doesn't
/// fire the "publish your recipe" nudge again. Stored as a JSON
/// map keyed by recipe id.
fn record_publish_marker(path: &Path, recipe_id: &str, sha256: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = std::fs::read_to_string(path).unwrap_or_else(|_| "{}".to_string());
    let mut map: std::collections::BTreeMap<String, serde_json::Value> =
        serde_json::from_str(&raw).unwrap_or_default();
    map.insert(
        recipe_id.to_string(),
        serde_json::json!({
            "sha256": sha256,
            "published_at": rfc3339_now(),
        }),
    );
    let serialized = serde_json::to_vec_pretty(&map)?;
    std::fs::write(path, serialized)?;
    Ok(())
}

fn submit_upstream_pr(
    _recipe: &corpus_engine::Recipe,
    _dest_recipe_path: &Path,
) -> std::result::Result<(), String> {
    // The full gh-driven flow is intentionally deferred to the
    // recipe-author chat agent (Phase 5) which has more context
    // about the user's GitHub workflow. For v1, just print the
    // template and instructions so the user can run gh manually.
    Err(
        "--submit-pr is not yet wired; see the published recipe on disk and \
         draft the PR manually for now"
            .into(),
    )
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn rfc3339_now() -> String {
    use chrono::Utc;
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Parse a single `--params` / `--param` value into the running
/// parameter map (TOML-typed). Mirror of mesh_cmd's CLI-side parser
/// but produces `toml::Value` directly because that's what the
/// test harness's `TestOptions.parameters` consumes.
///
/// Accepts:
/// - `key=value` — single string value
/// - `key=v1,v2,v3` — list of strings (comma-separated)
fn parse_test_param_spec(
    spec: &str,
    out: &mut std::collections::BTreeMap<String, toml::Value>,
) -> std::result::Result<(), String> {
    let (key, value) = spec
        .split_once('=')
        .ok_or_else(|| format!("expected `key=value`, got `{spec}`"))?;
    let key = key.trim();
    if key.is_empty() {
        return Err("empty parameter name".into());
    }
    let value = if value.contains(',') {
        let items: Vec<toml::Value> = value
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(toml::Value::String)
            .collect();
        toml::Value::Array(items)
    } else {
        toml::Value::String(value.trim().to_string())
    };
    out.insert(key.to_string(), value);
    Ok(())
}

/// Best-effort JSON → TOML value coercion for the
/// `--params-file <json>` path. Drops nulls; rejects nested objects
/// (nothing in the parameter schema accepts them yet).
fn json_value_to_toml(v: &serde_json::Value) -> Option<toml::Value> {
    match v {
        serde_json::Value::String(s) => Some(toml::Value::String(s.clone())),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(toml::Value::Integer(i))
            } else {
                n.as_f64().map(toml::Value::Float)
            }
        }
        serde_json::Value::Bool(b) => Some(toml::Value::Boolean(*b)),
        serde_json::Value::Array(arr) => {
            let items: Vec<toml::Value> = arr
                .iter()
                .filter_map(|v| match v {
                    serde_json::Value::String(s) => Some(toml::Value::String(s.clone())),
                    _ => None,
                })
                .collect();
            Some(toml::Value::Array(items))
        }
        serde_json::Value::Null | serde_json::Value::Object(_) => None,
    }
}

#[cfg(test)]
mod test_param_tests {
    use super::*;

    #[test]
    fn parses_string_param() {
        let mut params = std::collections::BTreeMap::new();
        parse_test_param_spec("start=2022-01-01", &mut params).unwrap();
        match params.get("start") {
            Some(toml::Value::String(s)) => assert_eq!(s, "2022-01-01"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn parses_list_param() {
        let mut params = std::collections::BTreeMap::new();
        parse_test_param_spec("entities=NVDA,MSFT,GOOGL", &mut params).unwrap();
        match params.get("entities") {
            Some(toml::Value::Array(arr)) => assert_eq!(arr.len(), 3),
            other => panic!("expected Array, got {other:?}"),
        }
    }
}

// print_usage replaced by sovereign_cli_shared::help::print(&HELP); see HELP const above.
