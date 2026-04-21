//! `sovereign tools` — invoke code-intelligence tools directly from
//! the CLI.
//!
//! The MCP surface (`sovereign project serve` / `sovereign daemon`)
//! exposes the same 24 tools over JSON-RPC; terminal agents see them
//! as *an API* and often write Python wrappers to probe unfamiliar
//! schemas. This surface exposes the same tools as *primitives* the
//! agent can call the way it calls `rg` or `cargo check`: one
//! command, plain-text output, self-documenting via `--help`.
//!
//! ## Subcommands
//!
//! - `tools list` — print the manifest, grouped by
//!   Effect × Scope from Phase 1's behavioural properties. Generated
//!   at runtime from `ToolRegistry::descriptors()` so there's no
//!   hand-maintained table to drift.
//! - `tools describe <id>` — render a single tool's descriptor,
//!   including parameter schema and example invocations.
//! - `tools call <id> [--k=v ...]` — build a JSON params object from
//!   `--key value` pairs, invoke the tool, print the `StepOutput` as
//!   plain text (or `--format json` for pipelines).
//!
//! Generic dispatch means adding a new tool gets CLI surface for free
//! — no per-tool code here.

use std::collections::BTreeMap;

use serde_json::Value;
use sovereign_core::types::{Effect, Scope, ToolContext, ToolDescriptor};

mod args;
mod format;
mod registry;

use args::{get_flag, split_args};
use format::{behaviour_tag, render_step_output, OutputMode};

pub async fn run_tools(raw_args: &[String]) -> i32 {
    let Some(first) = raw_args.first() else {
        print_help();
        return 1;
    };

    if matches!(first.as_str(), "--help" | "-h" | "help") {
        print_help();
        return 0;
    }

    let rest = &raw_args[1..];
    match first.as_str() {
        "list" => cmd_list(rest).await,
        "describe" => cmd_describe(rest).await,
        "call" => cmd_call(rest).await,
        other => {
            eprintln!("tools: unknown subcommand '{other}'");
            print_help();
            2
        }
    }
}

fn print_help() {
    eprintln!(
        "sovereign tools — invoke code-intelligence tools as CLI primitives\n\
         \n\
         USAGE\n    sovereign tools <subcommand> [flags]\n\
         \n\
         SUBCOMMANDS\n\
         \x20   list                              Print the tool manifest (grouped by effect + scope)\n\
         \x20   describe <id>                     Show a tool's full descriptor + examples\n\
         \x20   call <id> [--key=value ...]       Invoke a tool with JSON params built from flags\n\
         \n\
         FLAGS (for `call`)\n\
         \x20   --format text|json                Output format (default: text)\n\
         \n\
         NOTES\n\
         \x20   Tools with a 'Write' effect are audited but not approval-gated on\n\
         \x20   the CLI path (same semantics as the MCP path). The executor's\n\
         \x20   approval gate only fires via StepKind::Tool inside a Runtime task.\n"
    );
}

// ─── list ───────────────────────────────────────────────────────────

async fn cmd_list(_args: &[String]) -> i32 {
    let env = match registry::open_tools_registry().await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("tools list: {e}");
            return 1;
        }
    };

    // Group by (Effect, Scope) so the table matches how an operator
    // thinks about tools ("read-only code intel" / "persistent note
    // writes" / "session-scoped watchers"). Sort within each group by
    // id for stable output.
    let mut grouped: BTreeMap<(String, String), Vec<ToolDescriptor>> = BTreeMap::new();
    for d in env.registry.descriptors() {
        let effect = effect_label(d.effect).to_string();
        let scope = scope_label(d.scope).to_string();
        grouped
            .entry((effect, scope))
            .or_default()
            .push(d);
    }

    // Print groups in a stable, human-ordered sequence rather than
    // BTreeMap's lexicographic one: reads before writes, session
    // before persistent before external.
    let order = [
        ("Read", "Session"),
        ("Read", "Persistent"),
        ("Read", "External"),
        ("Write", "Session"),
        ("Write", "Persistent"),
        ("Write", "External"),
        ("ReadWrite", "Session"),
        ("ReadWrite", "Persistent"),
        ("ReadWrite", "External"),
    ];

    println!("sovereign tools — {} tool(s) available\n", env.registry.count());
    for (effect, scope) in order {
        let key = (effect.to_string(), scope.to_string());
        let Some(mut tools) = grouped.remove(&key) else {
            continue;
        };
        tools.sort_by(|a, b| a.id.cmp(&b.id));
        println!("{effect} · {scope}");
        for d in &tools {
            let desc = first_sentence(&d.description);
            println!("  {:<22} {desc}", d.id);
        }
        println!();
    }
    // Anything left in `grouped` is a tool with an unexpected combo —
    // still print it rather than silently hide.
    for ((effect, scope), mut tools) in grouped {
        tools.sort_by(|a, b| a.id.cmp(&b.id));
        println!("{effect} · {scope}");
        for d in &tools {
            println!("  {:<22} {}", d.id, first_sentence(&d.description));
        }
        println!();
    }

    println!("Run `sovereign tools describe <id>` for details.");
    0
}

/// First sentence of a multi-sentence description, bounded at 80
/// chars. Matches the `rg`/`cargo` style where each help line stays
/// on one terminal row.
fn first_sentence(desc: &str) -> String {
    let cleaned: String = desc.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = cleaned.find(". ").map(|i| &cleaned[..i]).unwrap_or(&cleaned);
    if cut.len() > 80 {
        format!("{}…", &cut[..77])
    } else {
        cut.to_string()
    }
}

// ─── describe ───────────────────────────────────────────────────────

async fn cmd_describe(args: &[String]) -> i32 {
    let (positional, _flags) = split_args(args);
    let Some(id) = positional.first().cloned() else {
        eprintln!("tools describe: missing <tool-id>");
        return 2;
    };

    let env = match registry::open_tools_registry().await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("tools describe: {e}");
            return 1;
        }
    };

    let tool = match env.registry.get(&id) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("tools describe: unknown tool '{id}'");
            eprintln!("  Run `sovereign tools list` for the full set.");
            return 1;
        }
    };
    let d = tool.descriptor();
    println!("{} {}", d.id, behaviour_tag(&d));
    println!("  Name:        {}", d.name);
    println!("  Idempotent:  {:?}", d.idempotency);
    println!();
    println!("  Description:");
    for line in d.description.lines() {
        println!("    {line}");
    }
    println!();
    println!("  Parameters (JSON Schema):");
    println!(
        "{}",
        indent(
            &serde_json::to_string_pretty(&d.parameters).unwrap_or_else(|_| d.parameters.to_string()),
            "    ",
        )
    );
    if let Some(output_schema) = &d.output_schema {
        println!();
        println!("  Output (schema hint — reference keys via {{N.key}} templates):");
        println!(
            "{}",
            indent(
                &serde_json::to_string_pretty(output_schema)
                    .unwrap_or_else(|_| output_schema.to_string()),
                "    ",
            )
        );
    } else {
        println!();
        println!("  Output: unstructured (shape depends on invocation)");
    }
    if !d.examples.is_empty() {
        println!();
        println!("  Examples:");
        for ex in &d.examples {
            println!("    • {}", ex.situation);
            let call = serde_json::to_string(&ex.call).unwrap_or_else(|_| ex.call.to_string());
            println!("      sovereign tools call {} {}", d.id, json_to_flags(&ex.call));
            println!("      (raw JSON: {call})");
        }
    }
    0
}

fn indent(s: &str, prefix: &str) -> String {
    s.lines().map(|l| format!("{prefix}{l}")).collect::<Vec<_>>().join("\n")
}

/// Render a JSON object's keys as `--key=value` flags for display in
/// example invocations. Non-objects are shown as `--json <value>`.
fn json_to_flags(v: &Value) -> String {
    match v {
        Value::Object(map) => map
            .iter()
            .map(|(k, val)| {
                let s = match val {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!("--{k}={}", shell_quote(&s))
            })
            .collect::<Vec<_>>()
            .join(" "),
        other => format!("--json {}", shell_quote(&other.to_string())),
    }
}

fn shell_quote(s: &str) -> String {
    if s.chars().any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '$' | '&' | '|')) {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_string()
    }
}

// ─── call ───────────────────────────────────────────────────────────

async fn cmd_call(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(id) = positional.first().cloned() else {
        eprintln!("tools call: missing <tool-id>");
        return 2;
    };

    let mode = match OutputMode::parse(get_flag(&flags, "--format").as_deref()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("tools call: {e}");
            return 2;
        }
    };

    // Build JSON params from --key=value flags (excluding --format).
    // Numbers and booleans get coerced; everything else stays String.
    let mut params = serde_json::Map::new();
    for (k, v) in flags.iter().filter(|(k, _)| k != "format") {
        params.insert(k.clone(), coerce_value(v));
    }
    let params_value = Value::Object(params);

    let env = match registry::open_tools_registry().await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("tools call: {e}");
            return 1;
        }
    };

    let tool = match env.registry.get(&id) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("tools call: unknown tool '{id}'");
            eprintln!("  Run `sovereign tools list` for the full set.");
            return 1;
        }
    };

    if let Err(e) = tool.validate(&params_value) {
        eprintln!("tools call: validation failed: {e}");
        return 2;
    }

    // Phase 1.5 parity: audit write-effectful CLI calls too. Same
    // rationale as mcp_router — the CLI is non-interactive, so no
    // blocking approval gate; surface the write in tracing/stderr
    // so the operator (who's literally at the terminal) sees it.
    let d = tool.descriptor();
    if d.effect != Effect::Read {
        eprintln!(
            "  [audit] calling {} (effect: {:?}, idempotency: {:?}) — no approval gate on CLI path",
            d.id, d.effect, d.idempotency
        );
    }

    let ctx = ToolContext {
        conversation_id: "cli-tools".to_string(),
        task_id: None,
        working_directory: std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string()),
        in_reasoning_loop: false,
    };

    match tool.execute(&params_value, &ctx).await {
        Ok(out) => {
            print!("{}", render_step_output(&out, mode));
            if !matches!(mode, OutputMode::Json) {
                println!();
            }
            0
        }
        Err(e) => {
            eprintln!("tools call: tool failed: {e}");
            1
        }
    }
}

/// Coerce a CLI string into a JSON Value. Integers, floats, and
/// `true`/`false` get typed; everything else stays a String.
fn coerce_value(s: &str) -> Value {
    if s == "true" {
        return Value::Bool(true);
    }
    if s == "false" {
        return Value::Bool(false);
    }
    if let Ok(n) = s.parse::<i64>() {
        return Value::Number(n.into());
    }
    if let Ok(f) = s.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    // JSON literal pass-through for arrays/objects passed as a single flag
    if (s.starts_with('[') && s.ends_with(']')) || (s.starts_with('{') && s.ends_with('}')) {
        if let Ok(v) = serde_json::from_str::<Value>(s) {
            return v;
        }
    }
    Value::String(s.to_string())
}

// ─── Shared helpers ─────────────────────────────────────────────────

fn effect_label(e: Effect) -> &'static str {
    match e {
        Effect::Read => "Read",
        Effect::Write => "Write",
        Effect::ReadWrite => "ReadWrite",
    }
}

fn scope_label(s: Scope) -> &'static str {
    match s {
        Scope::Session => "Session",
        Scope::Persistent => "Persistent",
        Scope::External => "External",
    }
}
