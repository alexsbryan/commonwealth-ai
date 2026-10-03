// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn recipe publish` — write a validated recipe into the local registry,
//! and optionally open an upstream PR.
//!
//! Split out of [`super`] (ARCH §3.1: `recipe_cmd.rs` was 1155 lines and
//! inside the arch-gate's 800-1200 approach band). The registry upsert, the
//! publish marker, the `gh` PR shim and the two small time/hash helpers only
//! this path uses come with it; `build_stub_engine` stays in the parent, which
//! the validate and test paths also use.

use super::*;

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
pub(super) async fn cmd_publish(args: &[String]) -> i32 {
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

    // The TOML write and the `registry.toml` upsert are
    // `RecipeRegistry::install_local_recipe` — the ONE decider for "a user
    // published a recipe" since 2026-09-11. This verb had its own copy and
    // the desktop's import command had a third; the daemon's
    // `/internal/corpus/recipes/import` route now shares this one.
    let registry_path = local_dir.join("registry.toml");
    let dest_recipe_path =
        match corpus_engine::RecipeRegistry::from_bundled(Some(local_dir.clone()))
            .install_local_recipe(&recipe, &raw)
        {
            Ok(path) => path,
            Err(e) => {
                eprintln!("error: failed to install recipe into the local registry: {e}");
                return 1;
            }
        };

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
