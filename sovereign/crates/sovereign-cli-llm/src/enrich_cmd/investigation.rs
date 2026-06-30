// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich investigation` — drive the investigation
//! enrichment pipeline against an installed corpus.
//!
//! Two subcommands:
//!
//! - `build <corpus_id>` — extract typed relationships from every
//!   chunk in `~/.sovereign/indexes/<corpus_id>/`, run the
//!   recipe-declared pattern detectors, and persist
//!   `entities.json` / `relationships.json` /
//!   `pattern_findings.json` under
//!   `~/.sovereign/indexes/<corpus_id>/investigation/`. Hits the
//!   running daemon's `/v1/chat/completions` endpoint for the
//!   per-chunk extraction; respects per-phase chat-model routing
//!   from `EnrichConfig` if the operator has configured one.
//! - `show <corpus_id>` — render the persisted findings as plain
//!   text. Read-only, no network.
//!
//! Distinct from `svrn enrich build` which dispatches the
//! atlas pipeline. The investigation pipeline has different shape
//! (typed entity / relationship graph + graph-pattern detectors)
//! so it lives behind its own verb. See
//! `corpus_engine::enrichment::investigation` for the runtime.

use std::collections::BTreeMap;

use std::path::Path;

use corpus_engine::{
    enrichment::investigation::{
        graph as investigation_graph, normalize::Normalizer, recoalesce, run_investigation,
        ChunkInput, INVESTIGATION_DIRNAME,
    },
    CorpusIndex, RecipeRegistry,
};

use super::config::EnrichConfig;
use super::inference_client::DaemonInferenceClient;

/// Default daemon base URL used when the operator hasn't pinned one
/// in `EnrichConfig`. Mirrors the rest of the enrich CLI.
const DEFAULT_DAEMON_BASE: &str = "http://127.0.0.1:9741";

/// Default chat model used when the operator hasn't selected one.
/// Investigation extraction is JSON-grammar-constrained so the
/// shape is stable regardless of model choice; pick a chat-capable
/// id and the daemon serves the request.
const DEFAULT_CHAT_MODEL: &str = "primary";

/// Fall-back per-request output cap. Investigation extraction
/// emits a JSON array of relationships — typically tens of tokens
/// per relationship — so 8k headroom covers densely-relationship
/// chunks without prematurely truncating.
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 8192;

pub async fn cmd_investigation(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("Usage: svrn enrich investigation <build|show> <corpus_id> [args]");
        return 2;
    }
    match args[0].as_str() {
        "--help" | "-h" | "help" => {
            print_help();
            0
        }
        "build" => cmd_build(&args[1..]).await,
        "show" => cmd_show(&args[1..]).await,
        "recoalesce" => cmd_recoalesce(&args[1..]).await,
        other => {
            eprintln!("Unknown investigation subcommand: {other}");
            print_help();
            1
        }
    }
}

fn print_help() {
    println!(
        "Usage: svrn enrich investigation <subcommand> <corpus_id> [args]\n\
         \n\
         Subcommands:\n\
           build <id>        Run the investigation pipeline (extract → coalesce → detect)\n\
           show <id>         Render findings persisted under the corpus's investigation/\n\
           recoalesce <id>   Re-fold the persisted graph under current rules \
                             (no inference; backs up originals)\n\
         \n\
         build flags:\n\
           --params k=v[,...]      Recipe parameters (multi-supply with --params \
                                   k=v --params k2=v2)\n\
           --chat-model <id>       Override the default chat model\n\
           --base-url <url>        Override the daemon base URL\n\
           --limit <n>             Process only the first N chunks (debugging)\n\
           --config <corpus_id>    Read EnrichConfig under <corpus_id> for per-phase \
                                   chat_models\n"
    );
}

// ---------------------------------------------------------------------------
// build
// ---------------------------------------------------------------------------

async fn cmd_build(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut chat_model: Option<String> = None;
    let mut base_url: Option<String> = None;
    let mut limit: Option<usize> = None;
    let mut config_corpus: Option<String> = None;
    let mut params: BTreeMap<String, toml::Value> = BTreeMap::new();

    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--chat-model" => match iter.next() {
                Some(v) => chat_model = Some(v.clone()),
                None => return arg_error("--chat-model requires a model id"),
            },
            "--base-url" => match iter.next() {
                Some(v) => base_url = Some(v.clone()),
                None => return arg_error("--base-url requires a URL"),
            },
            "--limit" => match iter.next().and_then(|v| v.parse::<usize>().ok()) {
                Some(n) => limit = Some(n),
                None => return arg_error("--limit requires a non-negative integer"),
            },
            "--config" => match iter.next() {
                Some(v) => config_corpus = Some(v.clone()),
                None => return arg_error("--config requires a corpus id"),
            },
            "--params" | "--param" => match iter.next() {
                Some(spec) => {
                    if let Err(e) = parse_param_spec(spec, &mut params) {
                        return arg_error(&format!("invalid {a}: {e}"));
                    }
                }
                None => return arg_error(&format!("{a} requires `key=value`")),
            },
            "--help" | "-h" => {
                print_help();
                return 0;
            }
            other if !other.starts_with('-') => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else {
                    return arg_error(&format!("unexpected positional `{other}`"));
                }
            }
            other => return arg_error(&format!("unknown flag `{other}`")),
        }
    }
    let Some(corpus_id) = corpus_id else {
        return arg_error(
            "missing corpus id (e.g. `svrn enrich investigation build sec-investigation`)",
        );
    };

    // ── Resolve recipe (registry + bundled fallback + local user
    //     registry merge so a `svrn recipe publish`-ed recipe
    //     is reachable). ─────────────────────────────────────────
    let local_dir = RecipeRegistry::default_local_recipes_dir();
    let mut registry = RecipeRegistry::from_bundled(local_dir.clone());
    if let Some(d) = &local_dir {
        registry = registry.with_local_registry(&d.join("registry.toml"));
    }
    let recipe = match registry.fetch_recipe(&corpus_id).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: failed to resolve recipe for `{corpus_id}`: {e}");
            return 1;
        }
    };

    let Some(enrichment) = recipe.enrichment.as_ref() else {
        eprintln!(
            "error: recipe `{corpus_id}` has no [enrichment] block. Investigation \
             requires `enrichment.type = \"investigation\"`."
        );
        return 1;
    };
    if enrichment.enrichment_type != "investigation" {
        eprintln!(
            "error: recipe `{corpus_id}` has enrichment.type = \"{}\". Investigation \
             pipeline expects \"investigation\".",
            enrichment.enrichment_type
        );
        return 1;
    }
    if enrichment.entity_types.is_empty() || enrichment.relationship_types.is_empty() {
        eprintln!(
            "error: recipe `{corpus_id}` is missing entity_types and/or \
             relationship_types declarations. The investigation pipeline cannot run \
             without a schema."
        );
        return 1;
    }

    // Apply user-supplied parameters (validates against
    // `[recipe.parameters]` and rejects unknown / missing required
    // keys synchronously).
    let resolved = match recipe.resolve_parameters(&params) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: parameter validation failed: {e}");
            return 1;
        }
    };
    let recipe = recipe.with_resolved_parameters(resolved);

    // ── Open the corpus index ──────────────────────────────────
    let index_dir = sovereign_cli_shared::dirs::sovereign_indexes().join(&corpus_id);
    if !index_dir.is_dir() {
        eprintln!(
            "error: corpus `{corpus_id}` is not installed at {}.\n\
             Run: sovereign corpus install {corpus_id}",
            index_dir.display()
        );
        return 1;
    }
    let index = match CorpusIndex::open(&index_dir).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!(
                "error: failed to open index at {}: {e}",
                index_dir.display()
            );
            return 1;
        }
    };
    let stored = match index.all_chunks().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: failed to read chunks: {e}");
            return 1;
        }
    };
    let stored = match limit {
        Some(n) => stored.into_iter().take(n).collect(),
        None => stored,
    };
    let total = stored.len();
    if total == 0 {
        eprintln!(
            "error: corpus `{corpus_id}` has no chunks indexed yet. Re-run \
             `svrn corpus install {corpus_id}` first."
        );
        return 1;
    }

    // ── Build the chat client ──────────────────────────────────
    // Per-phase model routing comes from EnrichConfig if the
    // operator pinned one for this corpus; otherwise we use the
    // single chat_model.
    let chat_models_by_phase: BTreeMap<String, String> = match config_corpus.as_deref() {
        Some(cid) => match EnrichConfig::load(cid) {
            Ok(Some(cfg)) => cfg.chat_models_by_phase_snapshot(),
            Ok(None) => {
                eprintln!(
                    "note: no EnrichConfig found for `{cid}` — using \
                     --chat-model default for all phases"
                );
                BTreeMap::new()
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to load EnrichConfig for `{cid}`: {e} \
                     — using --chat-model default for all phases"
                );
                BTreeMap::new()
            }
        },
        None => BTreeMap::new(),
    };

    let resolved_chat_model = chat_model
        .clone()
        .unwrap_or_else(|| DEFAULT_CHAT_MODEL.to_string());
    let resolved_base_url = base_url.unwrap_or_else(|| DEFAULT_DAEMON_BASE.to_string());
    let client = match DaemonInferenceClient::new(
        resolved_base_url.clone(),
        resolved_chat_model.clone(),
        // embed model unused by investigation; placeholder.
        "qwen3-embedding-0.6b",
    ) {
        Ok(c) => c
            .with_max_output_tokens(DEFAULT_MAX_OUTPUT_TOKENS)
            .with_chat_models_by_phase(chat_models_by_phase),
        Err(e) => {
            eprintln!("error: failed to build chat client: {e}");
            return 1;
        }
    };
    let (_, chat, _) = client.into_closures_with_tokens();

    // ── Convert StoredChunk → ChunkInput. We hold the strings on
    //     the stack so the borrowed `&str`s in `ChunkInput` stay
    //     alive for the duration of the run. ─────────────────────
    let chunk_id_strings: Vec<String> = stored.iter().map(|c| c.id.to_string()).collect();
    let chunks: Vec<ChunkInput<'_>> = stored
        .iter()
        .zip(chunk_id_strings.iter())
        .map(|(c, id_str)| ChunkInput {
            chunk_id: id_str.as_str(),
            source_title: c.title.as_deref(),
            content: c.content.as_str(),
        })
        .collect();

    eprintln!(
        "Investigating `{corpus_id}` — {total} chunk(s), \
         model `{resolved_chat_model}` at {resolved_base_url}"
    );
    let started = std::time::Instant::now();
    let outcome = run_investigation(&recipe, &chunks, chat, &index_dir).await;
    let elapsed = started.elapsed();

    match outcome {
        Ok(out) => {
            println!();
            println!("Done in {:.1}s", elapsed.as_secs_f32());
            println!(
                "  Entities:        {}\n  Relationships:   {}\n  Pattern findings: {}",
                out.entities.len(),
                out.relationships.len(),
                out.findings.len(),
            );
            println!();
            println!("Outputs: {}/{INVESTIGATION_DIRNAME}/", index_dir.display());
            println!("Inspect with: sovereign enrich investigation show {corpus_id}");
            0
        }
        Err(e) => {
            eprintln!("error: investigation pipeline failed: {e}");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// show
// ---------------------------------------------------------------------------

async fn cmd_show(args: &[String]) -> i32 {
    let Some(corpus_id) = args.first() else {
        return arg_error("missing corpus id");
    };
    let index_dir = sovereign_cli_shared::dirs::sovereign_indexes().join(corpus_id);
    if !index_dir.is_dir() {
        eprintln!(
            "error: corpus `{corpus_id}` is not installed at {}",
            index_dir.display(),
        );
        return 1;
    }
    let invest_dir = index_dir.join(INVESTIGATION_DIRNAME);
    if !invest_dir.is_dir() {
        eprintln!(
            "error: no investigation outputs at {}.\n\
             Run: sovereign enrich investigation build {corpus_id}",
            invest_dir.display(),
        );
        return 1;
    }
    let (entities, relationships, findings) = match investigation_graph::read_outputs(&index_dir) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: failed to read investigation outputs: {e}");
            return 1;
        }
    };

    println!("Investigation: {corpus_id}");
    println!("  Entities:         {}", entities.len());
    println!("  Relationships:    {}", relationships.len());
    println!("  Pattern findings: {}", findings.len());
    println!();

    if findings.is_empty() {
        println!("No pattern matches yet. The relationship graph may still be useful");
        println!("for ad-hoc inspection — check {}/.", invest_dir.display());
        return 0;
    }

    // Quick lookup tables for prettier rendering.
    let entity_name: BTreeMap<&str, &str> = entities
        .iter()
        .map(|e| (e.id.as_str(), e.canonical_name.as_str()))
        .collect();

    println!("── Pattern findings ────────────────────────────────────");
    for (i, f) in findings.iter().enumerate() {
        let entities_pretty: Vec<&str> = f
            .entity_ids
            .iter()
            .map(|id| entity_name.get(id.as_str()).copied().unwrap_or(id.as_str()))
            .collect();
        println!(
            "{:>3}. [{}] `{}` — {}",
            i + 1,
            format!("{:?}", f.pattern_type).to_lowercase(),
            f.pattern_name,
            entities_pretty.join(" → "),
        );
        if !f.attributes.is_empty() {
            for (k, v) in &f.attributes {
                if matches!(k.as_str(), "subject_id" | "counterparty_id") {
                    continue; // already shown via entity_ids
                }
                println!("       · {k} = {}", json_brief(v));
            }
        }
    }
    0
}

fn json_brief(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// recoalesce
// ---------------------------------------------------------------------------

/// Re-fold a persisted investigation graph under the *current* coalescing
/// rules — no inference, no daemon. Reuses the (expensive) Phase-1 extraction
/// and only re-runs the deterministic coalesce + aggregate + detect, so
/// tightened fold rules (state-suffix stripping, adjudication-by-category)
/// collapse straggler nodes that escaped the original pass. Idempotent. Backs
/// up the originals to `*.orig` before overwriting (glassbox / reversible).
async fn cmd_recoalesce(args: &[String]) -> i32 {
    let Some(corpus_id) = args.first() else {
        return arg_error(
            "missing corpus id (e.g. `svrn enrich investigation recoalesce uap-blue-book`)",
        );
    };
    let index_dir = sovereign_cli_shared::dirs::sovereign_indexes().join(corpus_id);
    let invest_dir = index_dir.join(INVESTIGATION_DIRNAME);
    if !invest_dir.is_dir() {
        eprintln!(
            "error: no investigation outputs at {}.\n\
             Run: sovereign enrich investigation build {corpus_id}",
            invest_dir.display(),
        );
        return 1;
    }

    // Resolve the recipe for its pattern declarations (re-detect needs them).
    let local_dir = RecipeRegistry::default_local_recipes_dir();
    let mut registry = RecipeRegistry::from_bundled(local_dir.clone());
    if let Some(d) = &local_dir {
        registry = registry.with_local_registry(&d.join("registry.toml"));
    }
    let recipe = match registry.fetch_recipe(corpus_id).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: failed to resolve recipe for `{corpus_id}`: {e}");
            return 1;
        }
    };
    let patterns = recipe
        .enrichment
        .as_ref()
        .map(|e| e.patterns.clone())
        .unwrap_or_default();

    let (entities, relationships, _) = match investigation_graph::read_outputs(&index_dir) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: failed to read investigation outputs: {e}");
            return 1;
        }
    };

    if let Err(e) = backup_investigation_outputs(&invest_dir) {
        eprintln!("error: failed to back up investigation outputs: {e}");
        return 1;
    }

    // The recipe supplies the coalescing vocabulary; the Normalizer applies it.
    let normalizer = Normalizer::from_recipe(&recipe);
    let out = recoalesce::recoalesce_graph(&normalizer, entities, relationships, &patterns);

    if let Err(e) = investigation_graph::write_outputs(
        &index_dir,
        &out.entities,
        &out.relationships,
        &out.findings,
    ) {
        eprintln!("error: failed to write recoalesced outputs: {e}");
        return 1;
    }

    println!("Recoalesced `{corpus_id}`:");
    println!(
        "  Entities:         {} → {}  (merged {})",
        out.entities_before,
        out.entities_after,
        out.entities_before.saturating_sub(out.entities_after),
    );
    println!(
        "  Relationships:    {} → {}  (deduped/dropped {})",
        out.relationships_before,
        out.relationships_after,
        out.relationships_before.saturating_sub(out.relationships_after),
    );
    println!("  Pattern findings: {}", out.findings.len());
    println!();
    println!(
        "Originals preserved as *.orig under {}/. Re-run is idempotent.",
        invest_dir.display(),
    );
    0
}

/// Copy the three graph files to `<name>.orig` before a recoalesce overwrites
/// them — but only if no `.orig` already exists, so the FIRST re-fold
/// preserves the true original and later (idempotent) runs don't clobber it.
fn backup_investigation_outputs(invest_dir: &Path) -> std::io::Result<()> {
    for f in ["entities.json", "relationships.json", "pattern_findings.json"] {
        let src = invest_dir.join(f);
        let dst = invest_dir.join(format!("{f}.orig"));
        if src.exists() && !dst.exists() {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn parse_param_spec(
    spec: &str,
    out: &mut BTreeMap<String, toml::Value>,
) -> std::result::Result<(), String> {
    let (key, value) = spec
        .split_once('=')
        .ok_or_else(|| format!("expected `key=value`, got `{spec}`"))?;
    let key = key.trim();
    if key.is_empty() {
        return Err("empty parameter name".into());
    }
    let value = if value.contains(',') {
        toml::Value::Array(
            value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(toml::Value::String)
                .collect(),
        )
    } else {
        toml::Value::String(value.trim().to_string())
    };
    out.insert(key.to_string(), value);
    Ok(())
}

fn arg_error(msg: &str) -> i32 {
    eprintln!("error: {msg}");
    eprintln!("Run `svrn enrich investigation --help` for usage.");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_param_spec_handles_string_and_list() {
        let mut params = BTreeMap::new();
        parse_param_spec("entities=NVDA,MSFT", &mut params).unwrap();
        parse_param_spec("start=2022-01-01", &mut params).unwrap();
        match params.get("entities") {
            Some(toml::Value::Array(arr)) => assert_eq!(arr.len(), 2),
            other => panic!("expected Array, got {other:?}"),
        }
        match params.get("start") {
            Some(toml::Value::String(s)) => assert_eq!(s, "2022-01-01"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn parse_param_spec_rejects_malformed() {
        let mut params = BTreeMap::new();
        assert!(parse_param_spec("entities", &mut params).is_err());
        assert!(parse_param_spec("=NVDA", &mut params).is_err());
    }
}
