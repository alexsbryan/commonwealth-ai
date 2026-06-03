//! Exact symbol-name lookup backed by the SCIP SQLite call graph.
//!
//! Trust contract: this tool is labelled "always correct" in the skill
//! prompt. It must never return a guess or a semantically-similar match —
//! if the exact name isn't found, it says so plainly and suggests the
//! approximate tool instead.
//!
//! Source of truth is `~/.sovereign/indexes/<corpus>/scip_graph.db`,
//! populated by `rust-analyzer scip`. The chunk-level LanceDB index is
//! NOT consulted: it carries a redundant `symbol_name`/`symbol_kind`
//! projection that goes missing whenever the chunk index gets wiped or
//! a rebuild dies mid-flight, and tying exact-name lookup to that
//! projection silently bricks the tool while the call-graph stays
//! healthy. Lance is now strictly for embedding + content + mtime; this
//! tool reads the file contents directly off disk after SCIP gives us
//! the `file:line_start..line_end` span.

use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::CorpusEngine;
use corpus_engine_scip::scip_graph::SymbolRow;

use super::callees::ScipGraphHandle;
use super::is_valid_symbol_name;

/// Look up a symbol by exact name across every corpus in the merged
/// SCIP graph.
pub struct SymbolLookupTool {
    #[allow(dead_code)]
    engine: Arc<CorpusEngine>,
    graph: ScipGraphHandle,
}

impl SymbolLookupTool {
    pub fn new(engine: Arc<CorpusEngine>, graph: ScipGraphHandle) -> Self {
        Self { engine, graph }
    }
}

#[async_trait]
impl Tool for SymbolLookupTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "symbols".to_string(),
            name: "Symbol Lookup".to_string(),
            description: "Exact lookup of a named symbol (function, struct, trait, type). \
                          Use this when you know the name of what you are looking for. \
                          Returns definition location, signature, and doc comments. \
                          Faster and cheaper than grep or file search. \
                          If you do not know the exact name, use code_search instead."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Exact symbol name (function, class, struct, trait, etc.)"
                    },
                    "kind": {
                        "type": "string",
                        "description": "Optional kind filter: function, method, class, struct, enum, trait, interface, impl, type, const, module",
                        "default": ""
                    }
                },
                "required": ["name"]
            }),
            examples: vec![
                ToolExample {
                    situation: "You're about to grep for a struct or read an entire file to check its fields before writing code that uses it. Don't — this returns the exact definition, fields, and doc comments in one call.".into(),
                    call: serde_json::json!({ "name": "ToolRegistry" }),
                },
                ToolExample {
                    situation: "You need a function's exact signature before calling it. Reading the whole file wastes context. This returns only the definition line and its docs.".into(),
                    call: serde_json::json!({ "name": "record_call" }),
                },
                ToolExample {
                    situation: "You want to find all trait impls for a type. Pass kind='impl' to filter to implementation blocks only.".into(),
                    call: serde_json::json!({ "name": "InferenceProvider", "kind": "trait" }),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "string",
                "description": "Fenced code blocks, one per match. Each block is prefixed \
                                with `// <file>:<start>-<end>  [<kind>]  (<corpus>)` \
                                so downstream steps can extract locations via regex \
                                or pipe to reasoning."
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        // Read-only query over the local SCIP DB plus on-disk source
        // file reads. No shell, no network.
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("symbol_lookup requires 'name'".to_string()))?;
        if !is_valid_symbol_name(name) {
            return Err(Error::InvalidInput(format!(
                "invalid symbol name '{name}': must be alphanumeric plus _, ::, or $, and ≤256 chars"
            )));
        }
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'name'".to_string()))?;
        if !is_valid_symbol_name(name) {
            return Err(Error::InvalidInput(format!("invalid symbol name '{name}'")));
        }
        let kind = params
            .get("kind")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let graph = self.graph.load_full();
        let rows = graph
            .find_symbols_by_name(name, kind, 8)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "symbols".to_string(),
                message: e.to_string(),
            })?;

        if rows.is_empty() {
            // Distinguish "no SCIP graph at all" from "SCIP graph
            // doesn't contain this name". Stat count is cheap (one
            // SQL `COUNT(*)`); doing the probe gives the user a
            // remediation hint when the call-graph itself is empty.
            let total = graph.symbol_count().await;
            if total == 0 {
                return Ok(StepOutput::Text(format!(
                    "No symbol named `{name}` found — SCIP call graph is empty.\n\n\
                     The graph builds from `rust-analyzer scip` exports. \
                     If this repo has never been indexed, run \
                     `sovereign project init` or `sovereign project refresh` \
                     to populate it. While the graph populates, fall back \
                     to `code_search` with a description of what you're \
                     looking for — semantic search runs against the chunk \
                     index and doesn't depend on SCIP."
                )));
            }
            return Ok(StepOutput::Text(format!(
                "No symbol named `{name}` found in any installed code corpus.\n\n\
                 Try `code_search` with a description of what you're looking \
                 for — it does semantic search (approximate) instead of exact \
                 name matching."
            )));
        }

        Ok(StepOutput::Text(format_symbol_rows(&rows).await))
    }
}

/// Format SCIP rows as fenced code blocks with a `// file:start-end
/// [kind] (corpus)` header. Reads the source file off disk to fill
/// each block — SCIP doesn't store content, but the file paths it
/// records are absolute or workspace-rooted, so we can re-resolve
/// against the corpus root the daemon advertises and pluck the line
/// range directly.
async fn format_symbol_rows(rows: &[SymbolRow]) -> String {
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        let content = read_symbol_body(&row.file_path, row.line_start, row.line_end)
            .await
            .unwrap_or_else(|e| format!("// (couldn't read source: {e})"));
        let lang = if row.language.is_empty() {
            ""
        } else {
            row.language.as_str()
        };
        out.push_str(&format!(
            "```{lang}\n// {file}:{start}-{end}  [{kind}]  ({corpus})\n{content}\n```",
            file = row.file_path,
            // SCIP stores 0-indexed lines; render as 1-indexed.
            start = row.line_start.saturating_add(1),
            end = row.line_end.saturating_add(1),
            kind = row.kind,
            corpus = row.corpus_id,
        ));
    }
    out
}

/// Read a 0-indexed inclusive `[start, end]` line range out of a
/// source file. SCIP records paths workspace-relative; we resolve
/// against the registered project roots if the path isn't absolute.
async fn read_symbol_body(path: &str, line_start: i32, line_end: i32) -> std::io::Result<String> {
    use std::path::PathBuf;

    let candidate = PathBuf::from(path);
    let resolved = if candidate.is_absolute() && candidate.exists() {
        candidate
    } else {
        // SCIP file_path values are recorded relative to the corpus
        // root (e.g. `src/foo.rs`). Try every registered project root
        // recorded by `sovereign project register` (~/.sovereign/projects/).
        // First hit wins; if none match, fall through with the bare
        // path so the error message includes what we looked for.
        resolve_via_project_registry(&candidate)
            .await
            .unwrap_or(candidate)
    };

    let content = tokio::fs::read_to_string(&resolved).await?;
    let start = line_start.max(0) as usize;
    let end = line_end.max(line_start) as usize;
    let slice: Vec<&str> = content
        .lines()
        .enumerate()
        .filter_map(|(i, l)| {
            if i >= start && i <= end {
                Some(l)
            } else {
                None
            }
        })
        .collect();
    if slice.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "line range {start}-{end} out of bounds for {}",
                resolved.display()
            ),
        ));
    }
    Ok(slice.join("\n"))
}

/// Walk the project registry the daemon writes to and return the
/// first `<root>/<relpath>` that exists. Kept fault-tolerant: any IO
/// error on the registry just returns `None` rather than propagating
/// — symbol lookup should degrade gracefully when the registry isn't
/// present (e.g. fresh install before any `sovereign project init`).
async fn resolve_via_project_registry(rel: &std::path::Path) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let projects_dir = std::path::PathBuf::from(home)
        .join(".sovereign")
        .join("projects");
    let mut entries = tokio::fs::read_dir(&projects_dir).await.ok()?;
    while let Some(entry) = entries.next_entry().await.ok().flatten() {
        let toml = entry.path();
        if toml.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let body = tokio::fs::read_to_string(&toml).await.ok()?;
        // Tiny key='value' parser — avoids a serde_toml dep here. The
        // registry file format is one `root = "..."` line per project,
        // written by `sovereign project register`. Anything fancier
        // (interpolation, multi-line strings) isn't used here.
        for line in body.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("root") {
                let rest = rest.trim_start_matches(['=', ' ', '\t']);
                let root = rest.trim_matches('"').trim_matches('\'');
                let candidate = std::path::PathBuf::from(root).join(rel);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}
