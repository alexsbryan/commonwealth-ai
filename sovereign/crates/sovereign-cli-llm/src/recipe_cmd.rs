// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign recipe` subcommand handlers.
//!
//! Provides two commands that don't require a loaded inference model:
//!
//!   sovereign recipe test <path>      [--sample-size N] [--output path]
//!                                     [--no-embed] [--verbose] [--offline]
//!   sovereign recipe validate <path>  [--offline]
//!
//! Both commands use a stub `EmbedFn` that returns zero-vectors. Embedding
//! is always disabled (`--no-embed`) in this code path because loading an
//! inference model requires `--model`, which is handled by the main REPL
//! entry point. Run `sovereign recipe test --embed` with a model to enable
//! the embed + search phase — that workflow is not yet supported here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::{CorpusEngine, EmbedFn, RecipeRegistry, TestOptions};

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
    command: "sovereign recipe",
    summary: "Run corpus ingestion recipes: test, validate, list.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("sovereign recipe <subcommand> [args]"),
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
             `publish` writes to ~/.sovereign/recipes/registry.toml; pass \
             --submit-pr to also draft a community-registry PR via `gh`.",
        ),
    ],
};

// ── `recipe test` ───────────────────────────────────────────────────────────

async fn cmd_test(args: &[String]) -> i32 {
    let mut recipe_path: Option<PathBuf> = None;
    let mut sample_size: usize = 100;
    let mut output: Option<PathBuf> = None;
    let mut offline = false;
    let mut verbose = false;
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
            "--no-embed" => { /* default — embed is always false here */ }
            "--offline" => offline = true,
            "--verbose" | "-v" => verbose = true,
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
                "Usage: sovereign recipe test <path> [--sample-size N] [--output path] \
                 [--params k=v[,...]]... [--params-file <json>] [--offline] [--verbose]"
            );
            return 1;
        }
    };

    // Derive the default output path from the recipe's directory.
    let output = output.unwrap_or_else(|| {
        recipe_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("TEST_REPORT.md")
    });

    let engine = build_stub_engine();
    let options = TestOptions {
        sample_size,
        embed: false,
        queries: None,
        output: Some(output.clone()),
        offline,
        verbose,
        parameters,
    };

    eprintln!("Testing recipe: {}", recipe_path.display());
    eprintln!("Sample size:    {sample_size}");
    eprintln!("Output:         {}", output.display());

    match engine.test_recipe(&recipe_path, &options).await {
        Ok(report) => {
            let markdown = report.to_markdown();

            if let Err(e) = std::fs::write(&output, &markdown) {
                eprintln!("error: failed to write report to {}: {e}", output.display());
                return 1;
            }

            eprintln!();
            eprintln!("Report written to: {}", output.display());

            let warnings = report.warnings();
            if !warnings.is_empty() {
                eprintln!();
                eprintln!("Warnings:");
                for w in &warnings {
                    eprintln!("  ⚠  {w}");
                }
            }

            if !report.validation.errors.is_empty() {
                eprintln!();
                eprintln!("Errors:");
                for e in &report.validation.errors {
                    eprintln!("  ✗  {e}");
                }
            }

            eprintln!();
            if report.passed() {
                eprintln!("Result: PASS");
                0
            } else {
                eprintln!("Result: FAIL");
                1
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
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
            eprintln!("Usage: sovereign recipe validate <path> [--offline]");
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
                eprintln!("✓ Validation passed");
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
    let stub_embed: EmbedFn = Arc::new(|_text| Box::pin(async { Ok(vec![0f32; 768]) }));

    // Use a temporary location for downloads; the engine's index_dir is
    // unused since we never write a production index.
    let tmp = std::env::temp_dir().join("sovereign-recipe-test");
    CorpusEngine::new(tmp.clone(), tmp, stub_embed)
}

// ── `recipe publish` ─────────────────────────────────────────────────────────

/// `sovereign recipe publish <path> [--submit-pr]`
///
/// Adds a recipe to the user's local registry at
/// `~/.sovereign/recipes/registry.toml` and copies the recipe
/// TOML to `~/.sovereign/recipes/<id>/recipe.toml`. The next
/// `sovereign corpus install <id>` (or desktop "Add Knowledge
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
                    "Usage: sovereign recipe publish <path> [--submit-pr] [--force]\n\n\
                     Adds a recipe to ~/.sovereign/recipes/registry.toml and copies \
                     the TOML to ~/.sovereign/recipes/<id>/recipe.toml. The recipe \
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
        eprintln!("Usage: sovereign recipe publish <path> [--submit-pr] [--force]");
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
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("warning: HOME not set; skipping publish marker");
        return 0;
    };
    let markers_path = home.join(".sovereign").join("published_recipes.json");
    if let Err(e) = record_publish_marker(&markers_path, &recipe.corpus.id, &sha256) {
        eprintln!("warning: failed to record publish marker: {e}");
    }

    println!("Published `{}` to local registry.", recipe.corpus.id);
    println!("  Recipe TOML:  {}", dest_recipe_path.display());
    println!("  Registry:     {}", registry_path.display());
    println!("  SHA-256:      {sha256}");
    println!();
    println!("Install with:");
    println!("  sovereign corpus install {}", recipe.corpus.id);

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

/// Insert (or update) an entry in `~/.sovereign/recipes/registry.toml`
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

/// Record a publish marker so `sovereign project audit` doesn't
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
