// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-studio` — a headless CLI that authors + tests recipes and runs
//! workflows against ANY OICP daemon, linking only the studio crates + the
//! shared leaves (no corpus-engine, no sovereign-core). It is the proof that the
//! studio package is independently usable over the OICP contract:
//!
//! - `recipe validate` / `recipe test` ride the daemon's `/oicp/v1/recipe/test`
//!   endpoint via [`HttpRecipeTester`] (B:P9b) — no local corpus engine.
//! - `workflow run` rides the OICP capabilities manifest (context window + embed
//!   prefix, B:P9a; advertised ingest endpoints, B:P9c) and reaches the
//!   corpus/atlas tools over the daemon's `/mcp` surface (B:P9d), re-aliased to
//!   their canonical ids so a `tool:corpus_store` step resolves.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use async_trait::async_trait;

use sovereign_contracts::error::Result as ToolResult;
use sovereign_contracts::recipe::testing::{RecipeTestParams, RecipeTester};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::{
    Permission, RetryConfig, StepOutput, ToolContext, ToolDescriptor,
};
use sovereign_recipe_author::HttpRecipeTester;
use sovereign_tools_base::mcp::auth::McpAuth;
use sovereign_tools_base::mcp::connect_http_mcp_server;
use sovereign_workflow::Workflow;
use sovereign_workflow_host::{
    first_comment_line, resolve_workflow_source, run_workflow_in_process, SHIPPED_WORKFLOWS,
    WORKFLOW_CORPUS_TOOL_IDS,
};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:9741";

const USAGE: &str = "\
sovereign-studio — author + test recipes and run workflows against an OICP daemon

USAGE:
    sovereign-studio <command> [options]

COMMANDS:
    recipe validate <recipe.toml> [--daemon <url>]
        Validate a recipe (schema / regex / placeholders) via the daemon's
        recipe-test endpoint — no source acquisition.

    recipe test <recipe.toml> [--daemon <url>] [--sample <n>]
        Dry-run a recipe end-to-end (acquire → extract → chunk) over a small
        sample and report per-stage counts. Default sample: host default.

    workflow list
        List the shipped + user workflows.

    workflow run <name|file.toml> [--daemon <url>] [--folder <dir>]
                 [--corpus <id>] [--glob <patterns>] [--param k=v]...
                 [--concurrency <n>] [--no-cache]
        Run a workflow against the daemon. Corpus/atlas steps are served over
        the daemon's MCP surface (the studio bin carries no corpus engine).

GLOBAL:
    --daemon <url>   OICP daemon base URL (default: http://127.0.0.1:9741)
    -h, --help       Print this help
";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let rest = &args[1..];
    let result = match args[0].as_str() {
        "recipe" => run_recipe(rest).await,
        "workflow" => run_workflow(rest).await,
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

// ── recipe ────────────────────────────────────────────────────────────────

async fn run_recipe(args: &[String]) -> std::result::Result<ExitCode, String> {
    let Some(sub) = args.first() else {
        return Err(format!(
            "`recipe` needs a subcommand (validate | test)\n\n{USAGE}"
        ));
    };
    let rest = &args[1..];
    match sub.as_str() {
        "validate" => recipe_validate(rest).await,
        "test" => recipe_test(rest).await,
        other => Err(format!("unknown `recipe` subcommand `{other}`")),
    }
}

async fn recipe_validate(args: &[String]) -> std::result::Result<ExitCode, String> {
    let (positional, flags) = parse_flags(args)?;
    let path = positional
        .first()
        .ok_or("recipe validate needs a <recipe.toml> path")?;
    let daemon = flags.daemon();

    let tester = HttpRecipeTester::new(&daemon, None);
    // sample_size = 0 + offline = validation-only: schema / regex-compile /
    // placeholder cross-reference, no acquisition.
    let params = RecipeTestParams {
        sample_size: 0,
        embed: false,
        offline: true,
        ..Default::default()
    };
    let outcome = tester
        .test(Path::new(path), &params)
        .await
        .map_err(|e| format!("{e}"))?;

    for w in &outcome.validation.warnings {
        eprintln!("  ⚠ {w}");
    }
    if outcome.validation.errors.is_empty() {
        println!("✓ {path} — valid");
        Ok(ExitCode::SUCCESS)
    } else {
        for e in &outcome.validation.errors {
            eprintln!("  ✗ {e}");
        }
        println!("✗ {path} — {} error(s)", outcome.validation.errors.len());
        Ok(ExitCode::FAILURE)
    }
}

async fn recipe_test(args: &[String]) -> std::result::Result<ExitCode, String> {
    let (positional, flags) = parse_flags(args)?;
    let path = positional
        .first()
        .ok_or("recipe test needs a <recipe.toml> path")?;
    let daemon = flags.daemon();
    let sample = flags
        .get("sample")
        .map(|s| {
            s.parse::<usize>()
                .map_err(|_| "--sample needs a number".to_string())
        })
        .transpose()?
        .unwrap_or(5);

    let tester = HttpRecipeTester::new(&daemon, None);
    let params = RecipeTestParams {
        sample_size: sample,
        embed: false,
        offline: false,
        ..Default::default()
    };
    let outcome = tester
        .test(Path::new(path), &params)
        .await
        .map_err(|e| format!("{e}"))?;

    println!("recipe test — {path}");
    if !outcome.validation.errors.is_empty() {
        for e in &outcome.validation.errors {
            eprintln!("  ✗ {e}");
        }
    }
    for w in outcome.warnings() {
        eprintln!("  ⚠ {w}");
    }
    if let Some(ext) = &outcome.extraction {
        println!(
            "  extract: {}/{} records ({:.0}%)",
            ext.records_succeeded,
            ext.records_attempted,
            ext.extraction_rate * 100.0
        );
    }
    println!(
        "  {}",
        if outcome.passed {
            "PASSED"
        } else {
            "did not pass"
        }
    );
    Ok(if outcome.validation.errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

// ── workflow ──────────────────────────────────────────────────────────────

async fn run_workflow(args: &[String]) -> std::result::Result<ExitCode, String> {
    let Some(sub) = args.first() else {
        return Err(format!(
            "`workflow` needs a subcommand (list | run)\n\n{USAGE}"
        ));
    };
    let rest = &args[1..];
    match sub.as_str() {
        "list" => workflow_list(),
        "run" => workflow_run(rest).await,
        other => Err(format!("unknown `workflow` subcommand `{other}`")),
    }
}

fn workflow_list() -> std::result::Result<ExitCode, String> {
    println!("Shipped workflows:");
    for (name, toml) in SHIPPED_WORKFLOWS {
        println!("  {name:<20} {}", first_comment_line(toml));
    }
    Ok(ExitCode::SUCCESS)
}

async fn workflow_run(args: &[String]) -> std::result::Result<ExitCode, String> {
    let (positional, flags) = parse_flags(args)?;
    let name = positional
        .first()
        .ok_or("workflow run needs a <name|file.toml>")?;
    let daemon = flags.daemon();
    let concurrency = flags
        .get("concurrency")
        .map(|s| {
            s.parse::<usize>()
                .map_err(|_| "--concurrency needs a number".to_string())
        })
        .transpose()?
        .unwrap_or(4)
        .max(1);
    let no_cache = flags.present("no-cache");

    let (toml, origin) = resolve_workflow_source(name)?;
    eprintln!("workflow: {origin}");
    let wf = Workflow::parse(&toml).map_err(|e| format!("parse workflow: {e}"))?;

    // Build the run-time params. `--folder`/`--corpus`/`--glob` are ergonomic
    // aliases for `--param key=value`; a `folder` source with an empty `glob`
    // matches every file.
    let mut params: BTreeMap<String, String> = flags.params.clone();
    if !params.contains_key("glob") {
        params.insert("glob".to_string(), String::new());
    }

    // The studio bin has no corpus engine, so corpus/atlas/extract steps are
    // served over the daemon's MCP surface (B:P9d). Connect only when the
    // workflow actually references one, and inject them through the runner's
    // `extra_tools` slot — the same slot the monolith fills with locally-built
    // tools.
    let extra = if uses_corpus_tools(&wf) {
        daemon_corpus_tools(&daemon).await
    } else {
        Vec::new()
    };

    let report =
        run_workflow_in_process(&wf, &daemon, concurrency, no_cache, params, extra).await?;

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
    Ok(if report.failed_count() == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Whether the workflow references a corpus/atlas/extract tool step (one of
/// [`WORKFLOW_CORPUS_TOOL_IDS`]) — those are served over the daemon's MCP.
fn uses_corpus_tools(wf: &Workflow) -> bool {
    wf.steps.iter().any(|s| {
        s.uses
            .strip_prefix("tool:")
            .map(|id| WORKFLOW_CORPUS_TOOL_IDS.contains(&id))
            .unwrap_or(false)
    })
}

/// Connect the daemon's `/mcp` and return its corpus/atlas tools re-aliased to
/// their canonical ids. The MCP adapter namespaces every remote tool
/// `mcp_<prefix>_<name>`, but a `tool:corpus_store` step resolves by the
/// canonical id `corpus_store` — so we filter to [`WORKFLOW_CORPUS_TOOL_IDS`]
/// (matching the un-prefixed tool `name`) and wrap each in a [`CanonicalId`]
/// adapter that reports the canonical id. On connect failure we warn and return
/// nothing — the run then fails cleanly at the first corpus step with a
/// "tool not found", rather than silently doing the wrong thing.
async fn daemon_corpus_tools(daemon: &str) -> Vec<Box<dyn Tool>> {
    let mcp_url = format!("{}/mcp", daemon.trim_end_matches('/'));
    match connect_http_mcp_server(&mcp_url, McpAuth::None, "studio").await {
        Ok(tools) => tools
            .into_iter()
            .filter_map(|t| {
                let name = t.descriptor().name;
                WORKFLOW_CORPUS_TOOL_IDS
                    .contains(&name.as_str())
                    .then(|| Box::new(CanonicalId::new(t, name)) as Box<dyn Tool>)
            })
            .collect(),
        Err(e) => {
            eprintln!("warning: could not connect the daemon's MCP surface for corpus tools ({e}); corpus/atlas steps will fail");
            Vec::new()
        }
    }
}

/// Re-expose a wrapped [`Tool`] under a different canonical id, delegating every
/// call. Used to strip the `mcp_studio_` prefix the MCP adapter adds, so a
/// workflow's `tool:<id>` step resolves to the daemon-served tool.
struct CanonicalId {
    inner: Box<dyn Tool>,
    id: String,
}

impl CanonicalId {
    fn new(inner: Box<dyn Tool>, id: String) -> Self {
        Self { inner, id }
    }
}

#[async_trait]
impl Tool for CanonicalId {
    fn descriptor(&self) -> ToolDescriptor {
        let mut d = self.inner.descriptor();
        d.id = self.id.clone();
        d
    }
    fn required_permissions(&self) -> Vec<Permission> {
        self.inner.required_permissions()
    }
    async fn execute(
        &self,
        params: &serde_json::Value,
        ctx: &ToolContext,
    ) -> ToolResult<StepOutput> {
        self.inner.execute(params, ctx).await
    }
    fn validate(&self, params: &serde_json::Value) -> ToolResult<()> {
        self.inner.validate(params)
    }
    fn retry_config(&self) -> Option<RetryConfig> {
        self.inner.retry_config()
    }
}

// ── arg parsing ────────────────────────────────────────────────────────────

/// Parsed flags: `--key value` pairs, bare `--flag` presence, `--param k=v`
/// pairs (accumulated into `params`), and everything else as positionals.
struct Flags {
    kv: BTreeMap<String, String>,
    present: Vec<String>,
    params: BTreeMap<String, String>,
}

impl Flags {
    fn daemon(&self) -> String {
        self.kv
            .get("daemon")
            .cloned()
            .unwrap_or_else(|| DEFAULT_DAEMON.to_string())
    }
    fn get(&self, key: &str) -> Option<&String> {
        self.kv.get(key)
    }
    fn present(&self, key: &str) -> bool {
        self.present.iter().any(|p| p == key)
    }
}

/// Hand-rolled parse (no clap — keeps the studio dependency budget to the
/// package + shared leaves). Recognizes value flags (`--daemon`, `--folder`,
/// `--corpus`, `--glob`, `--sample`, `--concurrency`), the `--param k=v`
/// accumulator (with `--folder`/`--corpus`/`--glob` as ergonomic aliases), and
/// the bare `--no-cache`. Returns `(positionals, flags)`.
fn parse_flags(args: &[String]) -> std::result::Result<(Vec<String>, Flags), String> {
    const VALUE_FLAGS: &[&str] = &["daemon", "sample", "concurrency"];
    const PARAM_ALIASES: &[&str] = &["folder", "corpus", "glob"];
    const BOOL_FLAGS: &[&str] = &["no-cache"];

    let mut positional = Vec::new();
    let mut kv = BTreeMap::new();
    let mut present = Vec::new();
    let mut params = BTreeMap::new();

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(name) = a.strip_prefix("--") {
            if name == "param" {
                i += 1;
                let kvpair = args.get(i).ok_or("--param needs key=value")?;
                let (k, v) = kvpair
                    .split_once('=')
                    .ok_or_else(|| format!("--param must be key=value, got `{kvpair}`"))?;
                params.insert(k.to_string(), v.to_string());
            } else if PARAM_ALIASES.contains(&name) {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| format!("--{name} needs a value"))?;
                params.insert(name.to_string(), v.clone());
            } else if VALUE_FLAGS.contains(&name) {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| format!("--{name} needs a value"))?;
                kv.insert(name.to_string(), v.clone());
            } else if BOOL_FLAGS.contains(&name) {
                present.push(name.to_string());
            } else {
                return Err(format!("unknown flag --{name}"));
            }
        } else {
            positional.push(a.clone());
        }
        i += 1;
    }

    Ok((
        positional,
        Flags {
            kv,
            present,
            params,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_value_flags_param_aliases_and_positionals() {
        let (pos, flags) = parse_flags(&args(&[
            "notebook",
            "--daemon",
            "http://host:9741",
            "--folder",
            "/docs",
            "--corpus",
            "nb",
            "--param",
            "glob=*.md",
            "--no-cache",
        ]))
        .unwrap();
        assert_eq!(pos, vec!["notebook".to_string()]);
        assert_eq!(flags.daemon(), "http://host:9741");
        assert_eq!(flags.params.get("folder").unwrap(), "/docs");
        assert_eq!(flags.params.get("corpus").unwrap(), "nb");
        // `--param glob=*.md` and the `--folder`/`--corpus` aliases all land in
        // the same params map the runner reads as `{param.*}`.
        assert_eq!(flags.params.get("glob").unwrap(), "*.md");
        assert!(flags.present("no-cache"));
    }

    #[test]
    fn daemon_defaults_when_absent() {
        let (_, flags) = parse_flags(&args(&["recipe.toml"])).unwrap();
        assert_eq!(flags.daemon(), DEFAULT_DAEMON);
    }

    #[test]
    fn param_without_equals_is_an_error() {
        assert!(parse_flags(&args(&["--param", "novalue"])).is_err());
    }

    #[test]
    fn unknown_flag_is_an_error() {
        assert!(parse_flags(&args(&["--nope"])).is_err());
    }

    #[test]
    fn value_flag_missing_its_value_errors() {
        assert!(parse_flags(&args(&["--daemon"])).is_err());
    }

    #[test]
    fn uses_corpus_tools_detects_corpus_steps() {
        let with = Workflow::parse(
            "[workflow]\nname=\"w\"\n[[step]]\nid=\"s\"\nuses=\"tool:corpus_store\"\n",
        )
        .unwrap();
        assert!(uses_corpus_tools(&with));
        let without = Workflow::parse(
            "[workflow]\nname=\"w\"\n[[step]]\nid=\"s\"\nuses=\"tool:write_file\"\n",
        )
        .unwrap();
        assert!(!uses_corpus_tools(&without));
    }
}
