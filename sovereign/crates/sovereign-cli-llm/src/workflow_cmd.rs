// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn workflow` — run a user-authored `Step · Artifact · Runner`
//! workflow (model + MCP + tool + transform steps, authored as TOML).
//!
//! Assembles a *light* stack — daemon-routed inference (no per-process model
//! load), a minimal tool registry, and the MCP servers from `~/.sovereign/
//! config.toml` — then runs the workflow in-process over its source items.
//! P0+P1 of `docs/specs/WORKFLOW_SUBSTRATE.md`; durable/distributed execution
//! is P2 (the pipeline tool as an outer loop).

use corpus_engine::RecipeRegistry;
use sovereign_core::traits::Tool;
use sovereign_workflow::Workflow;
// The registry assembly, in-process runner, and workflow catalog now live in
// `sovereign-workflow-host` (so the daemon can run workflows too); the CLI is a
// thin presenter on top.
use sovereign_workflow_host::{
    first_comment_line, resolve_workflow_source, run_workflow_in_process, workflows_dir,
    SHIPPED_WORKFLOWS,
};

// Inc3 surface unification: `workflow run <recipe-id>` delegates to the *same*
// install client `corpus install` uses, and shapes its `--param` values the same
// way. Two backends, one surface — the recipe install path stays intact.
use crate::corpus_cmd::{param_json_value, submit_install_request};

const DEFAULT_DAEMON: &str = "http://localhost:9741";

/// A name on the unified `workflow` surface resolves to one of two artifact kinds,
/// each with its own backend: a **workflow** (run in-process by the workflow host)
/// or a **recipe** (a corpus ingest/enrich, delegated to the daemon's install
/// path). One `list`/`run` surface; the two backends stay intact underneath.
#[derive(Debug)]
enum ResolvedArtifact {
    Workflow { toml: String, origin: String },
    Recipe { id: String, name: String },
}

/// Pure classifier (no I/O): a name is a `Workflow` when the workflow catalog
/// resolved it, else a `Recipe` when the recipe registry knows the id, else
/// unresolved. A workflow **shadows** a same-named recipe — local/authored intent
/// wins, the same precedence `resolve_workflow_source` already applies between a
/// user workflow and a shipped starter. Kept data-in/data-out (§5.4) so it tests
/// without the ambient `~/.sovereign` filesystem or a live registry fetch.
fn classify_artifact(
    name: &str,
    workflow: Option<(String, String)>,
    registry: &RecipeRegistry,
) -> std::result::Result<ResolvedArtifact, String> {
    if let Some((toml, origin)) = workflow {
        return Ok(ResolvedArtifact::Workflow { toml, origin });
    }
    if let Some(e) = registry.find_entry(name) {
        return Ok(ResolvedArtifact::Recipe {
            id: e.id.clone(),
            name: e.name.clone(),
        });
    }
    Err(format!(
        "no workflow or recipe named `{name}` — not a workflow file/catalog entry, \
         not a recipe in the registry.\n  See `svrn workflow list`."
    ))
}

/// Resolve a name against both backends — the workflow catalog
/// (`resolve_workflow_source`) and the recipe registry.
fn resolve_artifact(name: &str) -> std::result::Result<ResolvedArtifact, String> {
    let workflow = resolve_workflow_source(name).ok();
    classify_artifact(name, workflow, &recipe_registry())
}

/// The recipe catalog: the compiled-in bundled snapshot plus the user's published
/// `~/.sovereign/recipes/registry.toml`. No network — `find_entry`/`list_entries`
/// read the bundled snapshot; `with_local_registry` silently no-ops when the user
/// has none. Mirrors `recipe_cmd`'s construction so the two surfaces see the same
/// catalog.
fn recipe_registry() -> RecipeRegistry {
    let local_dir = RecipeRegistry::default_local_recipes_dir();
    let mut registry = RecipeRegistry::from_bundled(local_dir.clone());
    if let Some(d) = &local_dir {
        registry = registry.with_local_registry(&d.join("registry.toml"));
    }
    registry
}

pub async fn run_workflow(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP);
        return 0;
    }
    if args.is_empty() {
        sovereign_cli_shared::help::print(&HELP);
        return 1;
    }
    match args[0].as_str() {
        "run" => cmd_run(&args[1..]).await,
        "author" => cmd_author(&args[1..]).await,
        "list" | "ls" => cmd_list(),
        "copy" | "cp" => cmd_copy(&args[1..]),
        "new" => cmd_new(&args[1..]),
        other => {
            eprintln!("Unknown workflow subcommand: {other}");
            sovereign_cli_shared::help::print(&HELP);
            1
        }
    }
}

/// `svrn workflow author "<describe what you want>"` — natural-language
/// authoring. Tags a daemon conversation with the `workflow-author` skill and
/// sends the description; the daemon runs the compose→validate→test agent loop
/// server-side and returns the partner-facing reply. One authoring turn — re-run
/// with more detail (or edit the saved TOML) to iterate.
async fn cmd_author(args: &[String]) -> i32 {
    let mut daemon = DEFAULT_DAEMON.to_string();
    let mut desc: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--daemon" => {
                i += 1;
                match args.get(i) {
                    Some(u) => daemon = u.clone(),
                    None => {
                        eprintln!("--daemon needs a URL");
                        return 1;
                    }
                }
            }
            s if !s.starts_with('-') && desc.is_none() => desc = Some(s.to_string()),
            other => {
                eprintln!("Unknown argument: {other}");
                return 1;
            }
        }
        i += 1;
    }
    let Some(desc) = desc else {
        eprintln!("Usage: svrn workflow author \"<describe the workflow you want>\"");
        eprintln!(
            "Example: svrn workflow author \"fetch a web page and write a 3-sentence summary\""
        );
        return 1;
    };

    let base = daemon.trim_end_matches('/').to_string();
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(240))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("http client: {e}");
            return 1;
        }
    };

    // Tag a conversation with the workflow-author skill so the daemon routes it
    // into the authoring agent loop.
    let conv = match client
        .post(format!("{base}/v1/conversations"))
        .json(&serde_json::json!({ "skill_id": "workflow-author" }))
        .send()
        .await
    {
        Ok(r) => match r.error_for_status() {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "Daemon rejected the conversation ({e}). It may predate workflow-author \
                     support — rebuild + restart the daemon."
                );
                return 1;
            }
        },
        Err(e) => {
            eprintln!("Daemon not reachable at {daemon} ({e}). Start it with `svrn daemon`.");
            return 1;
        }
    };
    let conv_id = match conv.json::<serde_json::Value>().await {
        Ok(v) => v
            .get("id")
            .and_then(|x| x.as_str())
            .map(String::from)
            .unwrap_or_default(),
        Err(e) => {
            eprintln!("parse create-conversation response: {e}");
            return 1;
        }
    };
    if conv_id.is_empty() {
        eprintln!("create-conversation response missing `id`");
        return 1;
    }
    eprintln!("Authoring (conversation {conv_id}) — composing, validating, and checking what it can do…\n");

    // One partner turn; the daemon's whole server-side tool loop runs before this returns.
    let reply = match client
        .post(format!("{base}/v1/conversations/{conv_id}/messages"))
        .json(&serde_json::json!({ "content": desc }))
        .send()
        .await
    {
        Ok(r) => match r.error_for_status() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("daemon returned an error running the authoring turn: {e}");
                return 1;
            }
        },
        Err(e) => {
            eprintln!("send authoring message: {e}");
            return 1;
        }
    };
    let content = reply
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|v| v.get("content").and_then(|x| x.as_str()).map(String::from))
        .unwrap_or_default();
    println!("{}", content.trim());
    eprintln!("\n— authored workflows land in ~/.sovereign/workflows/ —");
    eprintln!("  svrn workflow list                 # see them");
    eprintln!("  svrn workflow run <name> --folder <dir>   # run it");
    0
}

// The catalog (SHIPPED_WORKFLOWS, workflows_dir, resolve_workflow_source,
// first_comment_line) moved to `sovereign-workflow-host` so the daemon's trigger
// runtime resolves a watched folder's workflow the same way the CLI does.

const HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn workflow",
    summary: "Run a Step·Artifact·Runner workflow — model, MCP, and tool steps authored as TOML.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn workflow run <name|file.toml> [--folder <dir>] [--corpus <id>] [--glob <patterns>] [--param k=v]... [--params-file <json>] [--concurrency N] [--daemon <url>] [--no-cache]",
        ),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[
            (
                "run <name|file>",
                "Run a workflow over its items, OR install a recipe corpus — by name (or a .toml path)",
            ),
            (
                "author \"<description>\"",
                "Describe a workflow in plain language; your local model composes, validates, and saves it",
            ),
            ("list", "List what you can run: workflows (shipped + your own) + recipe corpora"),
            (
                "copy <name> [new]",
                "Copy a workflow into ~/.sovereign/workflows/ so you can edit it",
            ),
            (
                "new <name> [--from <starter>]",
                "Scaffold a new editable workflow from a starter (default: notebook)",
            ),
        ]),
    ],
};

async fn cmd_run(args: &[String]) -> i32 {
    let mut file: Option<String> = None;
    let mut concurrency = 4usize;
    let mut daemon = DEFAULT_DAEMON.to_string();
    let mut no_cache = false;
    let mut params: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-cache" => no_cache = true,
            "--concurrency" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(n) if n >= 1 => concurrency = n,
                    _ => {
                        eprintln!("--concurrency needs a positive number");
                        return 1;
                    }
                }
            }
            "--daemon" => {
                i += 1;
                match args.get(i) {
                    Some(u) => daemon = u.clone(),
                    None => {
                        eprintln!("--daemon needs a URL");
                        return 1;
                    }
                }
            }
            // Run-time parameters, readable in the workflow as `{param.key}` and in
            // the source path/glob. `--param k=v` is the general form; `--folder`,
            // `--corpus`, `--glob` are ergonomic aliases for the flagship one-liner.
            "--param" => {
                i += 1;
                match args.get(i).and_then(|s| s.split_once('=')) {
                    Some((k, v)) if !k.is_empty() => {
                        params.insert(k.to_string(), v.to_string());
                    }
                    _ => {
                        eprintln!("--param needs key=value");
                        return 1;
                    }
                }
            }
            "--params-file" => {
                i += 1;
                let Some(p) = args.get(i) else {
                    eprintln!("--params-file needs a path");
                    return 1;
                };
                match std::fs::read_to_string(p)
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                {
                    Some(serde_json::Value::Object(map)) => {
                        for (k, v) in map {
                            let s = match v {
                                serde_json::Value::String(s) => s,
                                other => other.to_string(),
                            };
                            params.insert(k, s);
                        }
                    }
                    _ => {
                        eprintln!("--params-file must be a JSON object of string values: {p}");
                        return 1;
                    }
                }
            }
            "--folder" | "--corpus" | "--glob" => {
                let key = args[i].trim_start_matches('-').to_string();
                i += 1;
                match args.get(i) {
                    Some(v) => {
                        params.insert(key, v.clone());
                    }
                    None => {
                        eprintln!("--{key} needs a value");
                        return 1;
                    }
                }
            }
            s if !s.starts_with('-') && file.is_none() => file = Some(s.to_string()),
            other => {
                eprintln!("Unknown argument: {other}");
                return 1;
            }
        }
        i += 1;
    }

    let Some(file) = file else {
        eprintln!("Usage: svrn workflow run <name|file.toml> [--folder <dir>] …");
        eprintln!("       svrn workflow list   # to see available workflows");
        return 1;
    };

    // Resolve a bare name (or path) against BOTH backends — a workflow to run, or
    // a recipe to install. One surface; the two backends stay intact underneath.
    match resolve_artifact(&file) {
        Ok(ResolvedArtifact::Workflow { toml, origin }) => {
            // Echo the origin for a name so it's clear which file ran.
            if !origin.contains('/') {
                eprintln!("workflow: {origin}");
            }
            let wf = match Workflow::parse(&toml) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("workflow: {e}");
                    return 1;
                }
            };
            // Default the corpus id to the folder's basename when --folder is given
            // without --corpus, so the flagship one-liner needs only --folder. (A
            // workflow-only convenience — a recipe install never sees this.)
            if params.contains_key("folder") && !params.contains_key("corpus") {
                if let Some(base) = std::path::Path::new(&params["folder"])
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|b| !b.is_empty())
                {
                    params.insert("corpus".to_string(), base.to_string());
                }
            }
            run_assembled(&wf, &daemon, concurrency, no_cache, params).await
        }
        Ok(ResolvedArtifact::Recipe { id, name }) => {
            // A recipe id installs/updates a corpus via the daemon's install path —
            // the same client `corpus install` uses (which remains a permanent
            // alias). The collected `--param`s become the recipe's install
            // parameters, shaped by the one shared `--param` convention.
            eprintln!("recipe: {id} ({name}) — installing via the corpus path …");
            let install_params: std::collections::BTreeMap<String, serde_json::Value> = params
                .iter()
                .map(|(k, v)| (k.clone(), param_json_value(v)))
                .collect();
            submit_install_request(&id, install_params).await
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// `svrn workflow list` — the gallery: shipped starters + the user's own
/// (`~/.sovereign/workflows/`), user shadowing a same-named starter.
fn cmd_list() -> i32 {
    let mut rows: Vec<(String, &'static str, String)> = Vec::new();

    // Workflows: the user's own (shadowing a same-named starter) + shipped starters.
    let dir = workflows_dir();
    let mut user_names = std::collections::HashSet::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("toml") {
                continue;
            }
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                let desc = std::fs::read_to_string(&p)
                    .map(|t| first_comment_line(&t))
                    .unwrap_or_default();
                user_names.insert(stem.to_string());
                rows.push((stem.to_string(), "workflow", desc));
            }
        }
    }
    for (name, toml) in SHIPPED_WORKFLOWS {
        if !user_names.contains(*name) {
            rows.push((name.to_string(), "workflow", first_comment_line(toml)));
        }
    }

    // Recipes: the corpus catalog (bundled snapshot + the user's published
    // recipes). Surfaced on the same list so the user sees the full menu of names
    // `run` accepts — the surface is unified even though the backends stay separate.
    for e in recipe_registry().list_entries() {
        rows.push((e.id.clone(), "recipe", e.description.clone()));
    }

    rows.sort();
    if rows.is_empty() {
        println!("No workflows or recipes found.");
        return 0;
    }
    println!("{:<24} {:<9} DESCRIPTION", "NAME", "KIND");
    for (name, kind, desc) in &rows {
        let d: String = desc.chars().take(64).collect();
        println!("{name:<24} {kind:<9} {d}");
    }
    println!("\nRun:   svrn workflow run <name> [--folder <dir>] [--param k=v]");
    println!("       a workflow name runs its steps; a recipe name installs that corpus.");
    println!("Edit:  svrn workflow copy <workflow> <new-name>   # → ~/.sovereign/workflows/");
    0
}

/// `svrn workflow copy <name> [new]` — copy any workflow into the user dir
/// for editing.
fn cmd_copy(args: &[String]) -> i32 {
    let Some(id) = args.first() else {
        eprintln!("Usage: svrn workflow copy <name> [new-name]");
        return 1;
    };
    let (toml, _) = match resolve_workflow_source(id) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let new = args
        .get(1)
        .map(|s| s.as_str())
        .unwrap_or(id)
        .trim_end_matches(".toml");
    write_user_workflow(new, &toml)
}

/// `svrn workflow new <name> [--from <starter>]` — scaffold a new editable
/// workflow from a starter (default `notebook`).
fn cmd_new(args: &[String]) -> i32 {
    let mut name: Option<&str> = None;
    let mut from = "notebook";
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                i += 1;
                match args.get(i) {
                    Some(f) => from = f,
                    None => {
                        eprintln!("--from needs a starter name");
                        return 1;
                    }
                }
            }
            s if name.is_none() => name = Some(s),
            other => {
                eprintln!("unexpected argument: {other}");
                return 1;
            }
        }
        i += 1;
    }
    let Some(name) = name else {
        eprintln!("Usage: svrn workflow new <name> [--from <starter>]");
        return 1;
    };
    let (toml, _) = match resolve_workflow_source(from) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    write_user_workflow(name.trim_end_matches(".toml"), &toml)
}

/// Write a workflow into `~/.sovereign/workflows/<name>.toml`, refusing to clobber
/// an existing one. Prints the path + the run one-liner.
fn write_user_workflow(name: &str, toml: &str) -> i32 {
    let dir = workflows_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("create {}: {e}", dir.display());
        return 1;
    }
    let dest = dir.join(format!("{name}.toml"));
    if dest.exists() {
        eprintln!("{} already exists — pick another name", dest.display());
        return 1;
    }
    if let Err(e) = std::fs::write(&dest, toml) {
        eprintln!("write {}: {e}", dest.display());
        return 1;
    }
    println!("Created {}", dest.display());
    println!("Edit it, then:  svrn workflow run {name} --folder <dir>");
    0
}

/// Assemble the light stack — daemon-routed inference (only when a `model:` or
/// `embed:` step is present, via a `SplitInferenceProvider` so embed hits the
/// embed slot and chat the chat slot), a tool registry, MCP servers, and the
/// content cache — then run `wf` and print a per-item summary. Shared by
/// `workflow run <file>` and `corpus ingest <folder>` so the assembly lives in
/// one place.
pub(crate) async fn run_assembled(
    wf: &Workflow,
    daemon: &str,
    concurrency: usize,
    no_cache: bool,
    params: std::collections::BTreeMap<String, String>,
) -> i32 {
    // Assembly + run live in `sovereign-workflow-host` (so the daemon can run
    // workflows too). `standard_registry` now carries only the pure tools; the
    // corpus/atlas tools moved out of the base bundle, so inject them here to
    // preserve the pre-extraction surface, alongside the CLI's own
    // enrichment-authoring tools.
    // B:P9a: the embed-slot query-instruction prefix + chat context window are
    // now sourced by the runner from the daemon's OICP capabilities manifest,
    // so no `DEFAULT_MANIFEST` closure is threaded through here.
    let mut extra = sovereign_tools::workflow_corpus_tools();
    extra.extend(enrich_tools());
    let report =
        match run_workflow_in_process(wf, daemon, concurrency, no_cache, params.clone(), extra)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        };

    eprintln!(
        "\n— {} — {} ok, {} failed · {} steps ran, {} cached —",
        report.workflow,
        report.ok_count(),
        report.failed_count(),
        report.ran_total(),
        report.cached_total()
    );
    for item in &report.items {
        match &item.result {
            Ok(text) => println!("\n## {}\n{}", item.item, text.trim()),
            Err(e) => eprintln!("✗ {}: {e}", item.item),
        }
    }
    // Handoff: if this workflow built a corpus (a `tool:corpus_store` step) and at
    // least one item succeeded, tell the user how to query it — the flagship's
    // "now chat with it" payoff.
    if report.ok_count() > 0 && wf.steps.iter().any(|s| s.uses == "tool:corpus_store") {
        if let Some(corpus) = params.get("corpus").filter(|c| !c.is_empty()) {
            eprintln!(
                "\n✓ Notebook \"{corpus}\" is searchable — nothing left your machine.\n\n  \
                 Ask it (cited, instant):\n    svrn chat inspect --corpus {corpus} \"your question\"\n  \
                 Or chat with full answers:\n    svrn chat ask \"your question\"\n"
            );
        }
    }
    i32::from(report.failed_count() > 0)
}

/// The CLI-local enrichment-authoring tools the workflow runner needs in addition
/// to the standard built-ins — the atlas/pipeline leaves the bespoke enrich
/// pipeline composes (`crate::enrich_cmd::*`). Injected into the host runtime as
/// `extra_tools`; the daemon trigger runtime passes none (a living-folder workflow
/// uses only the standard set + MCP).
fn enrich_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(crate::enrich_cmd::atlas_resolve::AtlasResolveTool),
        Box::new(crate::enrich_cmd::atlas_phase_cmd::AtlasClusterTool),
        Box::new(crate::enrich_cmd::workflow_primitives::ExemplarSelectTool),
        Box::new(crate::enrich_cmd::workflow_primitives::PipelineComposeTool),
        Box::new(crate::enrich_cmd::workflow_primitives::PipelineParseTool),
        Box::new(crate::enrich_cmd::workflow_primitives::AtlasChaptersTool),
        Box::new(crate::enrich_cmd::workflow_primitives::AtlasSeedTool),
        Box::new(crate::enrich_cmd::workflow_primitives::AtlasClustersTool),
        Box::new(crate::enrich_cmd::workflow_primitives::AtlasClusterExcerptsTool),
        Box::new(crate::enrich_cmd::workflow_primitives::PipelineAssembleTool),
        Box::new(crate::enrich_cmd::workflow_primitives::AtlasSummaryTool),
        Box::new(crate::enrich_cmd::workflow_primitives::AtlasWriteConfigurationsTool),
    ]
}

#[cfg(test)]
mod artifact_tests {
    use super::*;

    // `classify_artifact` is pure (data-in/data-out), so these run hermetically
    // against the compiled-in bundled registry snapshot — no ~/.sovereign, no
    // network. "sep" is a known bundled recipe id (see corpus-engine registry tests).

    #[test]
    fn workflow_match_classifies_as_workflow() {
        let reg = RecipeRegistry::from_bundled(None);
        let got = classify_artifact(
            "notebook",
            Some((
                "[workflow]\nname = \"x\"\n".into(),
                "shipped:notebook".into(),
            )),
            &reg,
        )
        .expect("resolves");
        assert!(matches!(got, ResolvedArtifact::Workflow { .. }));
    }

    #[test]
    fn known_recipe_id_classifies_as_recipe_when_no_workflow() {
        let reg = RecipeRegistry::from_bundled(None);
        let got = classify_artifact("sep", None, &reg).expect("resolves");
        match got {
            ResolvedArtifact::Recipe { id, .. } => assert_eq!(id, "sep"),
            ResolvedArtifact::Workflow { .. } => panic!("expected the sep recipe, got a workflow"),
        }
    }

    #[test]
    fn workflow_shadows_a_same_named_recipe() {
        // A workflow named "sep" wins over the "sep" recipe — authored/local intent
        // takes precedence, the same way a user workflow shadows a shipped starter.
        let reg = RecipeRegistry::from_bundled(None);
        let got = classify_artifact(
            "sep",
            Some(("[workflow]\nname = \"sep\"\n".into(), "user:sep".into())),
            &reg,
        )
        .expect("resolves");
        assert!(
            matches!(got, ResolvedArtifact::Workflow { .. }),
            "a workflow must shadow a same-named recipe"
        );
    }

    #[test]
    fn unknown_name_is_an_error_naming_both_backends() {
        let reg = RecipeRegistry::from_bundled(None);
        let err = classify_artifact("definitely-not-a-thing-xyz", None, &reg).unwrap_err();
        assert!(err.contains("no workflow or recipe"), "got: {err}");
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::Stream;
    use sovereign_core::error::Result as CoreResult;
    use sovereign_core::registry::ToolRegistry;
    use sovereign_core::traits::{InferenceProvider, Tool};
    use sovereign_core::types::{
        CompletionRequest, CompletionResponse, Depth, Effect, Idempotency, Latency, Permission,
        ProviderCapabilities, Scope as ToolScope, Speed, StepOutput, ToolContext, ToolDescriptor,
    };
    use sovereign_tools::mcp::config::{McpAuthConfig, McpServerConfig, McpTransportConfig};
    use sovereign_tools::mcp::McpServerManager;
    use sovereign_workflow::{Runner, StepRegistry, Workflow};

    async fn spawn_demo() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                crate::mcp_demo_server::reference_mcp_router().into_make_service(),
            )
            .await;
        });
        format!("http://{addr}/mcp")
    }

    /// End to end, no daemon: a sealed file is read by a *real* MCP tool step
    /// over HTTP (`mcp:demo:read_memo`), then upper-cased by a `transform` step
    /// fed `{read.output}` — proving `mcp:` resolution + the runner threading an
    /// `Artifact` between steps through the production registry assembly.
    #[tokio::test]
    async fn mcp_read_threads_into_a_downstream_step() {
        let dir = tempfile::tempdir().unwrap();
        let memo = dir.path().join("note.md");
        let sealed = "ship the workflow substrate";
        std::fs::write(&memo, sealed).unwrap();

        let url = spawn_demo().await;
        let cfg = McpServerConfig {
            name: "demo".into(),
            description: None,
            enabled: true,
            transport: McpTransportConfig::Http {
                url,
                auth: McpAuthConfig::None,
            },
            global: true,
        };
        let mut tools = ToolRegistry::new();
        let _mgr = McpServerManager::from_config(std::slice::from_ref(&cfg), &mut tools).await;

        let toml = r#"
[workflow]
name = "read-upper"
[source]
type = "inline"
items = ["__PATH__"]
[[step]]
id = "read"
uses = "mcp:demo:read_memo"
params = { path = "{item.path}" }
[[step]]
id = "up"
uses = "transform:upper"
input = "{read.output}"
"#
        .replace("__PATH__", &memo.to_string_lossy());

        let wf = Workflow::parse(&toml).unwrap();
        let registry = StepRegistry::new(None, Arc::new(tools));
        let report = Runner::new(registry).run(&wf, 1).await.unwrap();

        assert_eq!(report.ok_count(), 1, "{:?}", report.items);
        let out = report.items[0].result.as_ref().unwrap();
        assert!(
            out.contains(&sealed.to_uppercase()),
            "MCP read must thread into the transform: {out}"
        );
    }

    async fn demo_registry(url: &str) -> StepRegistry {
        let cfg = McpServerConfig {
            name: "demo".into(),
            description: None,
            enabled: true,
            transport: McpTransportConfig::Http {
                url: url.to_string(),
                auth: McpAuthConfig::None,
            },
            global: true,
        };
        let mut tools = ToolRegistry::new();
        let _ = McpServerManager::from_config(std::slice::from_ref(&cfg), &mut tools).await;
        StepRegistry::new(None, Arc::new(tools))
    }

    /// The demonstrable payoff: a real `read (MCP) → upper (transform)` workflow
    /// runs once, is fully cached on an unchanged re-run, and re-runs when the
    /// source file changes — proving the content-addressed cache + fingerprint
    /// invalidation end to end.
    #[tokio::test]
    async fn cache_skips_unchanged_and_reruns_on_edit() {
        let dir = tempfile::tempdir().unwrap();
        let memo = dir.path().join("note.md");
        std::fs::write(&memo, "aaa").unwrap();
        let url = spawn_demo().await;
        let cache: Arc<dyn sovereign_workflow::ArtifactCache> = Arc::new(
            sovereign_workflow::FileArtifactCache::new(dir.path().join(".cache")),
        );

        let toml = r#"
[workflow]
name = "read-upper"
[source]
type = "inline"
items = ["__PATH__"]
[[step]]
id = "read"
uses = "mcp:demo:read_memo"
params = { path = "{item.path}" }
[[step]]
id = "up"
uses = "transform:upper"
input = "{read.output}"
"#
        .replace("__PATH__", &memo.to_string_lossy());
        let wf = Workflow::parse(&toml).unwrap();

        // 1st run: both steps run.
        let r1 = sovereign_workflow::Runner::with_cache(demo_registry(&url).await, cache.clone())
            .run(&wf, 1)
            .await
            .unwrap();
        assert_eq!((r1.ran_total(), r1.cached_total()), (2, 0));
        assert!(r1.items[0].result.as_ref().unwrap().contains("AAA"));

        // 2nd run, unchanged file: both steps cached.
        let r2 = sovereign_workflow::Runner::with_cache(demo_registry(&url).await, cache.clone())
            .run(&wf, 1)
            .await
            .unwrap();
        assert_eq!(
            (r2.ran_total(), r2.cached_total()),
            (0, 2),
            "unchanged -> fully cached"
        );

        // Edit the file (different size -> fingerprint changes regardless of
        // mtime granularity) -> the read re-runs, and the transform with it.
        std::fs::write(&memo, "bbbbbb").unwrap();
        let r3 = sovereign_workflow::Runner::with_cache(demo_registry(&url).await, cache.clone())
            .run(&wf, 1)
            .await
            .unwrap();
        assert_eq!(
            r3.cached_total(),
            0,
            "edited file -> re-runs, nothing cached"
        );
        assert!(r3.items[0].result.as_ref().unwrap().contains("BBBBBB"));
    }

    /// The generalization proof. Re-express corpus ingest's `chunk → embed`
    /// stage as a Workflow and diff it, byte for byte, against the **real**
    /// chunker + embed run directly. The `Artifact` never changed — a chunker's
    /// `1→N` output is just a JSON-array collection — only the Runner grew
    /// `for_each` to map `embed` over it. A clean diff means the substrate
    /// subsumes a fifth, previously-bespoke pipeline; the second run shows the
    /// content cache giving free resume on that same pipeline.
    #[tokio::test]
    async fn chunk_then_embed_matches_the_real_corpus_pipeline() {
        // Several paragraphs so the real chunker yields more than one chunk
        // (exercising the fan-out, not a degenerate single element).
        let doc = (0..6)
            .map(|i| {
                format!(
                    "Paragraph {i}. {}",
                    "The quick brown fox jumps over the lazy dog near the riverbank. ".repeat(3)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.txt");
        std::fs::write(&path, &doc).unwrap();

        // ── Oracle: the real corpus steps, run directly ──
        let oracle_chunks = sovereign_tools::rag::chunk::chunk_text(&doc);
        assert!(
            oracle_chunks.len() > 1,
            "doc must split into several chunks to exercise fan-out"
        );
        let provider = DeterministicEmbed;
        let mut oracle = Vec::new();
        for c in &oracle_chunks {
            oracle.push(provider.embed(&c.content).await.unwrap());
        }
        // Shape the oracle exactly as EmbedStep does (f32 → f64 → JSON) so the
        // comparison is byte-identical, not approximate.
        let oracle_json = serde_json::Value::Array(
            oracle
                .iter()
                .map(|v| {
                    serde_json::Value::Array(
                        v.iter()
                            .map(|f| serde_json::Value::from(*f as f64))
                            .collect(),
                    )
                })
                .collect(),
        );

        // ── The same pipeline, authored as a Workflow ──
        let toml = r#"
[workflow]
name = "chunk-embed"
[source]
type = "folder"
path = "__DIR__"
glob = "*.txt"
[[step]]
id = "chunk"
uses = "tool:chunk"
params = { path = "{item.path}" }
[[step]]
id = "embed"
uses = "embed:default"
for_each = "chunk"
input = "{element.text}"
"#
        .replace("__DIR__", &dir.path().to_string_lossy());
        let wf = Workflow::parse(&toml).unwrap();

        let mk_registry = || {
            let mut tools = ToolRegistry::new();
            tools.register(Box::new(ChunkerTool));
            StepRegistry::new(
                Some(Arc::new(DeterministicEmbed) as Arc<dyn InferenceProvider>),
                Arc::new(tools),
            )
        };

        // First run, fresh cache: the chunk step + one embed per chunk run.
        let cache: Arc<dyn sovereign_workflow::ArtifactCache> = Arc::new(
            sovereign_workflow::FileArtifactCache::new(dir.path().join(".cache")),
        );
        let r1 = Runner::with_cache(mk_registry(), cache.clone())
            .run(&wf, 4)
            .await
            .unwrap();
        assert_eq!(r1.ok_count(), 1, "{:?}", r1.items);

        // The byte-identical diff: same chunks, same embeddings, same order.
        // Compare the *serialized* forms, not parsed Values: serde_json's
        // default float parser is 1-ULP lossy (no `float_roundtrip` feature),
        // so parse-then-compare shows phantom last-digit diffs. The Ryū
        // serializer is exact and deterministic, so equal f64 arrays serialize
        // to identical strings — a faithful diff of the two pipelines.
        let wf_text = r1.items[0].result.as_ref().unwrap();
        let oracle_text = serde_json::to_string(&oracle_json).unwrap();
        assert_eq!(
            wf_text, &oracle_text,
            "workflow chunk→embed must equal the real pipeline"
        );
        let wf_json: serde_json::Value = serde_json::from_str(wf_text).unwrap();
        assert_eq!(wf_json.as_array().unwrap().len(), oracle_chunks.len());
        assert_eq!(
            (r1.ran_total(), r1.cached_total()),
            (oracle_chunks.len() + 1, 0),
            "fresh: chunk + one embed per chunk"
        );

        // Re-run, unchanged: every step is a cache hit — free resume on the
        // real pipeline.
        let r2 = Runner::with_cache(mk_registry(), cache.clone())
            .run(&wf, 4)
            .await
            .unwrap();
        assert_eq!(
            (r2.ran_total(), r2.cached_total()),
            (0, oracle_chunks.len() + 1),
            "unchanged re-run is fully cached"
        );
    }

    /// The full enrichment SHAPE as a composition, end to end, no daemon:
    /// `chunk → atoms → write_json`. The real `ChunkTool` fans a document into
    /// passages; a `model:fast` step (a deterministic atoms double) maps over
    /// them under a `structured_output` schema, so each passage becomes a parsed
    /// `{questions: […]}` Json atom; the real `WriteJsonTool` persists the whole
    /// collection to a path. This closes the gap the handoff named "atoms
    /// computed but not persisted": the atoms now land on disk, the pipeline
    /// authored entirely as TOML data (zero enrichment-specific Rust). The write
    /// leaf consumes `{atoms.output}` — the same "non-`for_each` step pairs a
    /// `for_each` step's whole collection" pattern `ingest.toml`'s store uses.
    #[tokio::test]
    async fn chunk_atoms_write_persists_the_atoms_collection() {
        let dir = tempfile::tempdir().unwrap();
        // Long enough that the real chunker yields several passages, so the
        // `for_each` produces several atoms — a real fan-out, not a degenerate
        // single element. (Short paragraphs under ~700 chars coalesce into one
        // chunk; same construction as the chunk→embed diff test.)
        let doc = (0..6)
            .map(|i| {
                format!(
                    "Passage {i}. {}",
                    "The shabby Soho shop concealed more than its modest wares. ".repeat(3)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        std::fs::write(dir.path().join("doc.txt"), &doc).unwrap();
        // A nested out dir the write leaf must create.
        let out = dir.path().join("out/doc.atoms.json");

        let toml = r#"
[workflow]
name = "chunk-atoms-write"
[source]
type = "folder"
path = "__DIR__"
glob = "*.txt"
[[step]]
id = "chunk"
uses = "tool:chunk"
params = { path = "{item.path}" }
[[step]]
id = "atoms"
uses = "model:fast"
for_each = "chunk"
system = "Extract the questions a passage raises as JSON."
prompt = "Passage:\n{element.text}\n\nList the interpretive questions."
structured_output = { type = "object", properties = { questions = { type = "array", items = { type = "string" } } }, required = ["questions"] }
[[step]]
id = "write"
uses = "tool:write_json"
params = { path = "__OUT__", json = "{atoms.output}" }
"#
        .replace("__DIR__", &dir.path().to_string_lossy())
        .replace("__OUT__", &out.to_string_lossy());

        let wf = Workflow::parse(&toml).unwrap();
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(sovereign_tools::rag::chunk::ChunkTool));
        tools.register(Box::new(sovereign_tools::write_json::WriteJsonTool));
        let registry = StepRegistry::new(
            Some(Arc::new(DeterministicAtoms) as Arc<dyn InferenceProvider>),
            Arc::new(tools),
        );
        let report = Runner::new(registry).run(&wf, 4).await.unwrap();
        assert_eq!(report.ok_count(), 1, "{:?}", report.items);

        // The terminal leaf persisted the atoms: the file exists (its missing
        // parent dir was created) and parses to one atom object per passage,
        // each carrying a `questions` array.
        let written =
            std::fs::read_to_string(&out).expect("write_json must persist the atoms to the path");
        let atoms: serde_json::Value = serde_json::from_str(&written).unwrap();
        let arr = atoms
            .as_array()
            .expect("the atoms collection is a JSON array");
        assert!(arr.len() >= 2, "several passages -> several atoms: {arr:?}");
        for a in arr {
            assert!(
                a.get("questions").and_then(|q| q.as_array()).is_some(),
                "each atom carries a `questions` array: {a}"
            );
        }
    }

    /// The `stamp` primitive: a `for_each` map carries each element's identity
    /// into its output — the workflow analog of the real Phase-1 runner stamping
    /// `chapter_id` from the dispatched chapter. `stamp = { chapter_id =
    /// "{element.index}" }` turns the model's `{questions}` into
    /// `{chapter_id, questions}` keyed per element, so the collection is a
    /// `Vec<ExtractedQuestion>`-shaped list rather than an anonymous array. A
    /// *constant* model output + a per-element stamp ⇒ distinct chapter_ids,
    /// which proves the identity comes from the element, not the model.
    #[tokio::test]
    async fn for_each_stamp_carries_element_identity() {
        let dir = tempfile::tempdir().unwrap();
        let doc = (0..6)
            .map(|i| {
                format!(
                    "Passage {i}. {}",
                    "Dense Conradian prose that recurs to fill a chunk. ".repeat(3)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        std::fs::write(dir.path().join("doc.txt"), &doc).unwrap();

        let toml = r#"
[workflow]
name = "stamp-atoms"
[source]
type = "folder"
path = "__DIR__"
glob = "*.txt"
[[step]]
id = "chunk"
uses = "tool:chunk"
params = { path = "{item.path}" }
[[step]]
id = "atoms"
uses = "model:fast"
for_each = "chunk"
stamp = { chapter_id = "{element.index}" }
system = "Extract the questions a passage raises as JSON."
prompt = "Passage:\n{element.text}\n\nList the interpretive questions."
structured_output = { type = "object", properties = { questions = { type = "array", items = { type = "string" } } }, required = ["questions"] }
"#
        .replace("__DIR__", &dir.path().to_string_lossy());

        let wf = Workflow::parse(&toml).unwrap();
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(sovereign_tools::rag::chunk::ChunkTool));
        let registry = StepRegistry::new(
            Some(Arc::new(DeterministicAtoms) as Arc<dyn InferenceProvider>),
            Arc::new(tools),
        );
        let report = Runner::new(registry).run(&wf, 4).await.unwrap();
        assert_eq!(report.ok_count(), 1, "{:?}", report.items);

        // The atoms collection: each element carries its own `chapter_id` (its
        // index, stamped from `{element.index}`) merged with the model's
        // `questions`. Distinct ids per element ⇒ the stamp is per-element.
        let atoms: serde_json::Value =
            serde_json::from_str(report.items[0].result.as_ref().unwrap()).unwrap();
        let arr = atoms.as_array().expect("the collection is a JSON array");
        assert!(
            arr.len() >= 2,
            "several chapters -> several stamped atoms: {arr:?}"
        );
        for (i, a) in arr.iter().enumerate() {
            assert_eq!(
                a.get("chapter_id").and_then(|v| v.as_str()),
                Some(i.to_string()).as_deref(),
                "element {i} is stamped with its own chapter_id: {a}"
            );
            assert!(
                a.get("questions").and_then(|q| q.as_array()).is_some(),
                "element {i} keeps the model's questions: {a}"
            );
        }
    }

    /// Break #2's payoff: the real enrichment Phase-1 SHAPE — a `Phase1Output`
    /// envelope — assembled as pure TOML composition, no daemon. `chunk →
    /// atoms[stamp] → envelope(transform:json) → write_json`: the model's
    /// per-chapter atoms are stamped with `chapter_id`, wrapped in
    /// `{schema_version, pipeline_id, questions_by_chapter: [...]}` by a
    /// JSON-shape transform (the collection **value-splices** into the envelope —
    /// a nested array, not a stringified copy), and persisted. Proves
    /// value-splicing + `transform:json` + `stamp` + `write_json` compose into the
    /// real `Phase1Output` shape with zero enrichment-specific Rust.
    #[tokio::test]
    async fn phase1_shaped_envelope_composes_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let doc = (0..6)
            .map(|i| {
                format!(
                    "Passage {i}. {}",
                    "Conradian prose that recurs to fill a chunk. ".repeat(3)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        std::fs::write(dir.path().join("doc.txt"), &doc).unwrap();
        let out = dir.path().join("questions.json");

        let toml = r#"
[workflow]
name = "enrich-phase1"
[source]
type = "folder"
path = "__DIR__"
glob = "*.txt"
[[step]]
id = "chunk"
uses = "tool:chunk"
params = { path = "{item.path}" }
[[step]]
id = "atoms"
uses = "model:fast"
for_each = "chunk"
stamp = { chapter_id = "{element.index}" }
prompt = "Passage:\n{element.text}\n\nList the interpretive questions."
structured_output = { type = "object", properties = { questions = { type = "array", items = { type = "string" } } }, required = ["questions"] }
[[step]]
id = "envelope"
uses = "transform:json"
params = { schema_version = 1, pipeline_id = "literary", questions_by_chapter = "{atoms.output}" }
[[step]]
id = "write"
uses = "tool:write_json"
params = { path = "__OUT__", json = "{envelope.output}" }
"#
        .replace("__DIR__", &dir.path().to_string_lossy())
        .replace("__OUT__", &out.to_string_lossy());

        let wf = Workflow::parse(&toml).unwrap();
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(sovereign_tools::rag::chunk::ChunkTool));
        tools.register(Box::new(sovereign_tools::write_json::WriteJsonTool));
        let registry = StepRegistry::new(
            Some(Arc::new(DeterministicAtoms) as Arc<dyn InferenceProvider>),
            Arc::new(tools),
        );
        let report = Runner::new(registry).run(&wf, 4).await.unwrap();
        assert_eq!(report.ok_count(), 1, "{:?}", report.items);

        // The persisted file is a Phase1Output-shaped envelope: metadata plus a
        // `questions_by_chapter` ARRAY (spliced as structure, not a stringified
        // copy), each entry an `ExtractedQuestion`-shaped `{chapter_id, questions}`.
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(v.get("schema_version").and_then(|x| x.as_i64()), Some(1));
        assert_eq!(
            v.get("pipeline_id").and_then(|x| x.as_str()),
            Some("literary")
        );
        let qbc = v
            .get("questions_by_chapter")
            .and_then(|x| x.as_array())
            .expect("questions_by_chapter must be a nested ARRAY, not a stringified copy");
        assert!(qbc.len() >= 2, "several chapters: {qbc:?}");
        for (i, e) in qbc.iter().enumerate() {
            assert_eq!(
                e.get("chapter_id").and_then(|c| c.as_str()),
                Some(i.to_string()).as_deref(),
                "entry {i} keyed by its chapter_id: {e}"
            );
            assert!(
                e.get("questions").and_then(|q| q.as_array()).is_some(),
                "entry {i} carries questions: {e}"
            );
        }
    }

    /// `tool:section` composes through the registry: a chaptered document is
    /// sectioned by structure (not 700-char windows), the model maps per chapter,
    /// and `stamp` keys each atom by the section's id — proving the structure-aware
    /// chunk leaf is a drop-in for `tool:chunk` at the real Phase-1 chapter unit.
    #[tokio::test]
    async fn section_leaf_drives_per_chapter_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let book = "Chapter 1\n\nThe shop stood in shabby Soho.\n\n\
                    Chapter 2\n\nThe Commissioner left Scotland Yard at dusk.\n\n\
                    Chapter 3\n\nStevie drew his circles on the paper.";
        std::fs::write(dir.path().join("book.txt"), book).unwrap();

        let toml = r#"
[workflow]
name = "section-extract"
[source]
type = "folder"
path = "__DIR__"
glob = "*.txt"
[[step]]
id = "chapters"
uses = "tool:section"
params = { path = "{item.path}" }
[[step]]
id = "atoms"
uses = "model:fast"
for_each = "chapters"
stamp = { chapter_id = "{element.section_id}" }
prompt = "Chapter \"{element.title}\":\n{element.text}\n\nList the interpretive questions."
structured_output = { type = "object", properties = { questions = { type = "array", items = { type = "string" } } }, required = ["questions"] }
"#
        .replace("__DIR__", &dir.path().to_string_lossy());

        let wf = Workflow::parse(&toml).unwrap();
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(sovereign_tools::rag::section::SectionTool));
        let registry = StepRegistry::new(
            Some(Arc::new(DeterministicAtoms) as Arc<dyn InferenceProvider>),
            Arc::new(tools),
        );
        let report = Runner::new(registry).run(&wf, 4).await.unwrap();
        assert_eq!(report.ok_count(), 1, "{:?}", report.items);

        // Three chapters -> three atoms, each keyed by its chapter's section_id.
        let atoms: serde_json::Value =
            serde_json::from_str(report.items[0].result.as_ref().unwrap()).unwrap();
        let arr = atoms.as_array().expect("the collection is a JSON array");
        assert_eq!(arr.len(), 3, "three chapters -> three atoms: {arr:?}");
        for (i, a) in arr.iter().enumerate() {
            assert_eq!(
                a.get("chapter_id").and_then(|c| c.as_str()),
                Some(format!("sec_{:04}", i + 1)).as_deref(),
                "atom {i} keyed by its chapter's section_id: {a}"
            );
            assert!(
                a.get("questions").is_some(),
                "atom {i} carries questions: {a}"
            );
        }
    }

    // ── doubles: the real chunker as a tool + a deterministic embed ──

    /// Wraps the *production* `chunk_text` so the diff runs the real chunker,
    /// not a stand-in. Reads its `path` param and emits the `1→N` collection.
    struct ChunkerTool;

    #[async_trait]
    impl Tool for ChunkerTool {
        fn descriptor(&self) -> ToolDescriptor {
            ToolDescriptor {
                id: "chunk".to_string(),
                name: "chunk".to_string(),
                description: "split a file into the corpus chunker's chunks".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                }),
                examples: vec![],
                effect: Effect::Read,
                idempotency: Idempotency::Idempotent,
                latency: Latency::Fast,
                scope: ToolScope::Session,
                output_schema: None,
            }
        }
        fn required_permissions(&self) -> Vec<Permission> {
            vec![]
        }
        async fn execute(
            &self,
            params: &serde_json::Value,
            _ctx: &ToolContext,
        ) -> CoreResult<StepOutput> {
            let path = params
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let text = std::fs::read_to_string(path).map_err(|e| {
                sovereign_core::error::Error::Execution(format!("chunk read {path}: {e}"))
            })?;
            let arr: Vec<serde_json::Value> = sovereign_tools::rag::chunk::chunk_text(&text)
                .into_iter()
                .map(|c| serde_json::json!({ "text": c.content, "index": c.index }))
                .collect();
            Ok(StepOutput::Json(serde_json::Value::Array(arr)))
        }
    }

    /// A deterministic 8-dim embedding (identical text → identical vector,
    /// distinct text → distinct vector). Lets the diff assert byte-identity
    /// without a daemon or weights — the claim is about the Runner's fan-out,
    /// not the embedding model.
    struct DeterministicEmbed;

    #[async_trait]
    impl InferenceProvider for DeterministicEmbed {
        async fn complete(&self, _req: &CompletionRequest) -> CoreResult<CompletionResponse> {
            unreachable!("the chunk→embed diff never calls complete()")
        }
        async fn complete_stream(
            &self,
            _req: &CompletionRequest,
        ) -> CoreResult<Pin<Box<dyn Stream<Item = CoreResult<String>> + Send>>> {
            unreachable!("the chunk→embed diff never streams")
        }
        async fn embed(&self, text: &str) -> CoreResult<Vec<f32>> {
            Ok((0..8u64)
                .map(|d| {
                    // FNV-1a over (dim, bytes) → a bounded, text-sensitive scalar.
                    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ d.wrapping_mul(0x0000_0100_0000_01b3);
                    for b in text.bytes() {
                        h ^= b as u64;
                        h = h.wrapping_mul(0x0000_0100_0000_01b3);
                    }
                    ((h % 2000) as f32) / 1000.0 - 1.0
                })
                .collect())
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 8192,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            }
        }
    }

    /// A deterministic `model:` double returning a fixed atoms object as JSON —
    /// lets the `chunk → atoms → write_json` e2e assert the composition (the
    /// `for_each` map + the terminal write) without a daemon or weights. The
    /// claim is the Runner threading structured atoms into the write leaf, not
    /// extraction quality, so a constant well-formed atom suffices.
    struct DeterministicAtoms;

    #[async_trait]
    impl InferenceProvider for DeterministicAtoms {
        async fn complete(&self, _req: &CompletionRequest) -> CoreResult<CompletionResponse> {
            Ok(CompletionResponse {
                text: r#"{"questions":["What does this passage reveal?"]}"#.to_string(),
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "deterministic-atoms".to_string(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }
        async fn complete_stream(
            &self,
            _req: &CompletionRequest,
        ) -> CoreResult<Pin<Box<dyn Stream<Item = CoreResult<String>> + Send>>> {
            unreachable!("the chunk→atoms→write e2e never streams")
        }
        async fn embed(&self, _text: &str) -> CoreResult<Vec<f32>> {
            unreachable!("the chunk→atoms→write e2e never embeds")
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 8192,
                supports_structured_output: true,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            }
        }
    }
}
