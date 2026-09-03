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
use sovereign_authoring_harness::{render::render_report, run_deterministic, Declaration};

mod authoring;
mod publish;

use authoring::{cmd_migrate, cmd_new};
use publish::cmd_publish;

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
        "new" => cmd_new(&args[1..]),
        "migrate" => cmd_migrate(&args[1..]),
        other => {
            eprintln!("Unknown recipe subcommand: {other}");
            sovereign_cli_shared::help::print(&HELP);
            1
        }
    }
}

const HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn recipe",
    summary: "Author and run corpus ingestion recipes: new, validate, test, migrate, list.",
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
            (
                "new --ontology <name>",
                "Scaffold a recipe from a built-in ontology template",
            ),
            (
                "migrate <path>",
                "Rewrite a recipe to a newer ontology version, as a diff",
            ),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "`list` takes --offline (skip live registry refresh).\n\
             `test` takes --sample-size N, --output <path>, --offline, --verbose, \
             --params k=v[,...], --params-file <json>.\n\
             `validate` takes --offline.\n\
             `publish` writes to ~/.svrnmesh/recipes/registry.toml; pass \
             --submit-pr to also draft a community-registry PR via `gh`.\n\
             `new` takes --ontology <name> (required; `--ontology list` names them), \
             --id <corpus-id> (fills corpus.id and corpus.name), --out <path> \
             (default: stdout; refuses to overwrite).\n\
             `migrate` takes --ontology-version N (required) and --dry-run (print the \
             diff, leave the file). Without --dry-run it rewrites the file in place \
             and prints the diff — the diff is the whole change.",
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

/// Resolve the daemon's chat + embed model ids through the ONE decider
/// (`sovereign_workflow_host::daemon_models`, ARCH §10.6): the chat id off
/// `/v1/models`, the embed id by the configured-stem → advertised ladder
/// PROVED with a `/v1/embeddings` probe. This used to be a third copy of an
/// `embed`-substring scan over the listing, and refused a daemon that
/// embedded fine but advertised only chat ids.
async fn resolve_daemon_models(v1: &str) -> Result<(String, String), String> {
    let models = sovereign_workflow_host::discover_models(v1)
        .await
        .map_err(|e| {
            format!("daemon /v1/models at {v1} unreachable ({e}); is the daemon running?")
        })?;
    let chat = models
        .chat
        .ok_or_else(|| format!("daemon at {v1} advertises no chat model"))?;
    let embed = sovereign_workflow_host::resolve_embed_model(v1, None)
        .await?
        .id;
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
                // The derived facets are the POINT of validating a declared
                // ontology, not a footnote: the numismatics template tells the
                // author this command "prints what the ontology derives", and
                // the recipe-author skill tells the model to read them back and
                // re-declare when an inference is wrong. Printing only a tick
                // made both promises false. Not a warning — the recipe is fine;
                // this is what it will do.
                if !report.validation.notes.is_empty() {
                    println!();
                    println!("Derived from your declarations:");
                    for n in &report.validation.notes {
                        println!("  {n}");
                    }
                }
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
