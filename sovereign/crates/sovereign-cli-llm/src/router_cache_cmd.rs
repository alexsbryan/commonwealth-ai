// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn router-cache {check,rebuild}` — manage the pre-built router
//! exemplar embedding cache (`sovereign/router/router-embed-cache.json`).
//!
//! - `check`  — pure, no-inference freshness gate over the WORKING TREE
//!   (exemplars + models.toml + the committed cache). Exit 0 = fresh, 3 = stale,
//!   2 = error. The `scripts/bump-desktop-version.sh` hook keys off exit 3.
//! - `rebuild` — regenerate the artifact against the prescribed embed model,
//!   driving the SAME `BootEmbedCache` + classifier path the runtime uses (via a
//!   chat-model-free [`EmbedOnlyProvider`]) so the cache is byte-identical to
//!   what a shipped app produces, then stamp its `built_for` fingerprint.
//!
//! Why this lives in `sovereign-cli-llm`: `rebuild` loads a GGUF and runs
//! inference, so it needs the `sovereign-inference` engine the LLM cluster
//! already links. `check` is pure but rides along so both verbs share one home.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sovereign_core::current_info_classifier::CurrentInfoClassifier;
use sovereign_core::effort_classifier::EffortClassifier;
use sovereign_core::models_manifest::ModelsManifest;
use sovereign_core::router_bootstrap::exemplar_specs;
use sovereign_core::router_embed::EmbedRouter;
use sovereign_core::router_embed_cache::{check_cache_fresh, BootEmbedCache};
use sovereign_core::scope_classifier::PersonalScopeClassifier;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::embedded::EmbedOnlyProvider;

const CACHE_REL: &str = "sovereign/router/router-embed-cache.json";
const MODELS_REL: &str = "sovereign/models.toml";
const ROUTER_DIR: &str = "sovereign/router";

const HELP: &str = "\
sovereign router-cache — pre-built router exemplar embedding cache

USAGE:
  sovereign router-cache check                 freshness gate (no inference; exit 3 = stale)
  sovereign router-cache rebuild [--embed-model <path>]
                                               regenerate the committed cache

The cache lets first launch HIT instead of re-embedding ~310 router exemplars
(minutes on a CPU-only embed slot). `check` is the CI/bump tripwire; `rebuild`
is run by scripts/bump-desktop-version.sh when stale, or by hand.";

pub async fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("check") => cmd_check(),
        Some("rebuild") => cmd_rebuild(&args[1..]).await,
        Some("--help") | Some("-h") | Some("help") | None => {
            println!("{HELP}");
            0
        }
        Some(other) => {
            eprintln!("router-cache: unknown subcommand '{other}'\n");
            println!("{HELP}");
            2
        }
    }
}

/// Walk up from CWD to the repo root (the dir holding `sovereign/models.toml`).
fn repo_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(MODELS_REL).is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Load the four exemplar TOMLs + models.toml + the committed cache from disk.
fn read_tree(root: &Path) -> std::io::Result<TreeFiles> {
    let rd = |rel: &str| std::fs::read_to_string(root.join(rel));
    Ok(TreeFiles {
        cache_json: rd(CACHE_REL)?,
        router: rd(&format!("{ROUTER_DIR}/exemplars.toml"))?,
        scope: rd(&format!("{ROUTER_DIR}/scope_examples.toml"))?,
        effort: rd(&format!("{ROUTER_DIR}/effort_examples.toml"))?,
        current_info: rd(&format!("{ROUTER_DIR}/current_info_examples.toml"))?,
        models: rd(MODELS_REL)?,
    })
}

struct TreeFiles {
    cache_json: String,
    router: String,
    scope: String,
    effort: String,
    current_info: String,
    models: String,
}

/// Resolve `(specs, fingerprint)` from the working tree — shared by check + the
/// post-rebuild verification so they agree by construction.
fn specs_and_fingerprint(t: &TreeFiles) -> Result<(Vec<(&'static str, String)>, String), String> {
    let specs = exemplar_specs(&t.router, &t.scope, &t.effort, &t.current_info)
        .map_err(|e| format!("parse exemplars: {e}"))?;
    let manifest =
        ModelsManifest::from_toml_str(&t.models).map_err(|e| format!("parse models.toml: {e}"))?;
    let fp = manifest
        .prescribed_embed_fingerprint()
        .ok_or_else(|| "models.toml declares no `default`-profile embed slot".to_string())?;
    Ok((specs, fp))
}

fn cmd_check() -> i32 {
    let Some(root) = repo_root() else {
        eprintln!("router-cache check: not inside a sovereign checkout (no {MODELS_REL} found)");
        return 2;
    };
    let tree = match read_tree(&root) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("router-cache check: reading router/models files: {e}");
            return 2;
        }
    };
    let (specs, fp) = match specs_and_fingerprint(&tree) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("router-cache check: {e}");
            return 2;
        }
    };
    match check_cache_fresh(&tree.cache_json, &specs, &fp) {
        Ok(()) => {
            println!(
                "router-cache: FRESH — {} exemplars covered, built for {fp}",
                specs.len()
            );
            0
        }
        Err(reason) => {
            eprintln!("router-cache: STALE — {reason}");
            eprintln!("  fix: sovereign router-cache rebuild");
            3
        }
    }
}

async fn cmd_rebuild(args: &[String]) -> i32 {
    // --embed-model <path> override (else the prescribed file under ~/.sovereign/models).
    let mut explicit_model: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--embed-model" => match it.next() {
                Some(p) => explicit_model = Some(PathBuf::from(p)),
                None => {
                    eprintln!("router-cache rebuild: --embed-model requires a path");
                    return 2;
                }
            },
            "--help" | "-h" => {
                println!("{HELP}");
                return 0;
            }
            other => {
                eprintln!("router-cache rebuild: unexpected argument '{other}'");
                return 2;
            }
        }
    }

    let Some(root) = repo_root() else {
        eprintln!("router-cache rebuild: not inside a sovereign checkout (no {MODELS_REL} found)");
        return 2;
    };
    let tree = match read_tree(&root) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("router-cache rebuild: reading router/models files: {e}");
            return 2;
        }
    };
    let manifest = match ModelsManifest::from_toml_str(&tree.models) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("router-cache rebuild: parse models.toml: {e}");
            return 2;
        }
    };
    let Some(slot) = manifest.prescribed_embed_slot() else {
        eprintln!("router-cache rebuild: models.toml declares no `default`-profile embed slot");
        return 2;
    };
    let fingerprint = format!("{}|{}", slot.family, slot.hf_url);
    let embed_family = manifest
        .embed_family_for_file(&slot.file)
        .unwrap_or(sovereign_core::model_family::ModelFamily::Unknown);

    // Resolve the model file: explicit override, else ~/.sovereign/models/<file>.
    let model_path = match explicit_model {
        Some(p) => p,
        None => {
            let home = dirs::home_dir().unwrap_or_default();
            home.join(".sovereign").join("models").join(&slot.file)
        }
    };
    if !model_path.is_file() {
        eprintln!(
            "router-cache rebuild: prescribed embed model not found:\n  {}\n\
             Download it from {} (file {}), or pass --embed-model <path>.",
            model_path.display(),
            slot.hf_url,
            slot.file,
        );
        return 2;
    }

    eprintln!(
        "router-cache rebuild: embedding router exemplars with {} (family {embed_family:?})\n  \
         model: {}\n  fingerprint: {fingerprint}\n  \
         (this can take several minutes on a CPU-only embed slot)",
        slot.file,
        model_path.display()
    );

    // Drive the cache straight at the committed artifact. The flush is atomic
    // (temp + rename), so a mid-run failure leaves the old file intact.
    let target = root.join(CACHE_REL);
    std::env::set_var("SOVEREIGN_ROUTER_EMBED_CACHE", &target);

    let provider: Arc<dyn InferenceProvider> =
        match EmbedOnlyProvider::load(&model_path, embed_family) {
            Ok(p) => Arc::new(p),
            Err(e) => {
                eprintln!("router-cache rebuild: load embed model: {e}");
                return 1;
            }
        };

    // Same path the runtime takes: open the boot cache, run each classifier's
    // `from_toml_str_cached` through it, flush. `BootEmbedCache::open` validates
    // the existing/baked cache via the sentinel probe and reuses matching
    // embeddings (incremental rebuild) or re-embeds on a model change.
    let mut cache = BootEmbedCache::open(&*provider).await;
    let r = async {
        EmbedRouter::from_toml_str_cached(&tree.router, Arc::clone(&provider), Some(&mut cache))
            .await?;
        PersonalScopeClassifier::from_toml_str_cached(
            &tree.scope,
            Arc::clone(&provider),
            Some(&mut cache),
        )
        .await?;
        EffortClassifier::from_toml_str_cached(
            &tree.effort,
            Arc::clone(&provider),
            Some(&mut cache),
        )
        .await?;
        CurrentInfoClassifier::from_toml_str_cached(
            &tree.current_info,
            Arc::clone(&provider),
            Some(&mut cache),
        )
        .await?;
        Ok::<(), sovereign_core::error::Error>(())
    }
    .await;
    if let Err(e) = r {
        eprintln!("router-cache rebuild: embedding failed: {e}");
        return 1;
    }
    cache.flush(); // writes `target` with built_for: None

    // Stamp the model fingerprint onto the freshly-written artifact.
    if let Err(e) = stamp_built_for(&target, &fingerprint) {
        eprintln!("router-cache rebuild: stamping built_for: {e}");
        return 1;
    }

    // Verify what we just wrote actually passes the gate.
    let fresh = std::fs::read_to_string(&target).ok();
    let (specs, _) = match specs_and_fingerprint(&tree) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("router-cache rebuild: re-check: {e}");
            return 1;
        }
    };
    match fresh
        .as_deref()
        .map(|j| check_cache_fresh(j, &specs, &fingerprint))
    {
        Some(Ok(())) => {
            println!(
                "router-cache: rebuilt {} ({} exemplars, built for {fingerprint})",
                target.display(),
                specs.len()
            );
            0
        }
        Some(Err(reason)) => {
            eprintln!("router-cache rebuild: wrote a cache that still fails the gate: {reason}");
            1
        }
        None => {
            eprintln!("router-cache rebuild: could not read back {}", target.display());
            1
        }
    }
}

/// Read the just-flushed cache JSON, set `built_for`, write it back (pretty).
fn stamp_built_for(path: &Path, fingerprint: &str) -> std::io::Result<()> {
    let raw = std::fs::read_to_string(path)?;
    let mut val: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    val["built_for"] = serde_json::Value::String(fingerprint.to_string());
    let pretty = serde_json::to_string_pretty(&val)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, format!("{pretty}\n"))
}
