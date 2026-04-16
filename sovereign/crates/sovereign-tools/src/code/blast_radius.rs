//! `blast_radius` — transitive impact analysis for a symbol.
//!
//! Performs a BFS traversal over the SCIP call graph to find all callers
//! at every depth level. Separates production callers from test callers
//! and groups each by module for readability.
//!
//! Use before modifying a function signature, removing a method, or
//! changing a trait definition to understand the full scope of impact.
//!
//! ## Macro augmentation
//!
//! SCIP captures compiler-resolved references in the unexpanded AST. Macro
//! invocations (`register_tool!(MyType)`, `#[derive(MyTrait)]`, etc.) don't
//! generate SCIP symbol references, so they are invisible to the SCIP BFS.
//!
//! When `with_project_root` is provided, `blast_radius` runs a supplementary
//! text scan over source files after the SCIP pass. Any line containing the
//! symbol name AND a macro indicator (`!(` or `#[`) is collected into the
//! `macro_hints` field with a clear "unverified (text scan)" label. False
//! positives (e.g. comments) are possible — the label makes the confidence
//! explicit.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::scip_graph::{BlastEntry, ScipGraph, StalenessCaution};

use super::is_valid_symbol_name;

pub type ScipGraphHandleRef = Arc<ArcSwap<ScipGraph>>;

pub struct BlastRadiusTool {
    graph: ScipGraphHandleRef,
    /// Optional project root for the supplementary macro text scan.
    project_root: Option<PathBuf>,
}

impl BlastRadiusTool {
    pub fn new(graph: ScipGraphHandleRef) -> Self {
        Self { graph, project_root: None }
    }

    /// Enable the supplementary macro text scan. Call this when the project
    /// root is known (i.e. from `project serve`). Without it, `macro_hints`
    /// is absent from the output.
    pub fn with_project_root(mut self, root: PathBuf) -> Self {
        self.project_root = Some(root);
        self
    }
}

#[async_trait]
impl Tool for BlastRadiusTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "blast_radius".to_string(),
            name: "Blast Radius".to_string(),
            description: "Compute the transitive impact of changing a symbol: \
                          all callers at every depth level up to max_depth. \
                          Use before modifying a function signature, removing a method, \
                          or changing a trait definition. Separates production callers \
                          from test callers and groups by module. Backed by the SCIP \
                          call graph — compiler-resolved, not grep. \
                          IMPORTANT: Before using on a large refactor, call \
                          read_notes(kinds=[\"reflection\"], query=\"blast_radius\") \
                          to check for known limitations recorded by previous sessions \
                          (e.g. macro-generated call sites not traversed by SCIP)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Symbol name to analyse (function, method, struct, trait)"
                    },
                    "max_depth": {
                        "type": "integer",
                        "default": 3,
                        "description": "BFS depth (1=direct callers, 2=callers of callers, …). Capped at 5."
                    },
                    "max_symbols": {
                        "type": "integer",
                        "default": 100,
                        "description": "Maximum total callers to return. Capped at 200."
                    }
                },
                "required": ["symbol"]
            }),
            examples: vec![],
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        let symbol = params
            .get("symbol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("blast_radius requires 'symbol'".to_string()))?;
        if !is_valid_symbol_name(symbol) {
            return Err(Error::InvalidInput(format!(
                "invalid symbol name '{symbol}'"
            )));
        }
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let symbol = params
            .get("symbol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'symbol'".to_string()))?;

        let max_depth = params
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(3);

        let max_symbols = params
            .get("max_symbols")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(100);

        let graph = self.graph.load_full();
        let result = graph
            .blast_radius(symbol, max_depth, max_symbols)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "blast_radius".to_string(),
                message: e.to_string(),
            })?;

        // Supplementary macro scan — runs regardless of SCIP results so
        // agents see macro hints even when SCIP finds callers (the macro
        // call sites are *additional* to the SCIP ones, not a replacement).
        let macro_hints = self.project_root.as_ref()
            .map(|root| macro_scan(symbol, root, 20));

        if result.entries.is_empty() {
            let mut obj = json!({
                "symbol": symbol,
                "production": {},
                "tests": {},
                "total": 0,
                "capped": false,
                "depth_reached": 0,
                "staleness": staleness_label(&result.caution),
                "hint": "No SCIP callers found — symbol may be unused, unexported, \
                         or not yet in the call graph. Public symbols with zero SCIP callers \
                         are often referenced through macros: check macro_hints below, or \
                         run `sovereign project refresh` if the graph is stale."
            });
            if let Some(hints) = macro_hints {
                obj["macro_hints"] = json!(hints);
                obj["macro_hints_note"] = json!(
                    "Unverified (text scan). Lines containing the symbol name adjacent to a \
                     macro indicator (`!(` or `#[`). May include comments or string literals."
                );
            }
            return Ok(StepOutput::Json(obj));
        }

        // Separate production from test callers.
        let (prod_entries, test_entries): (Vec<_>, Vec<_>) =
            result.entries.iter().partition(|e| !e.is_test);

        let production = group_by_module(&prod_entries);
        let tests = group_by_module(&test_entries);

        let mut obj = json!({
            "symbol": symbol,
            "production": production,
            "tests": tests,
            "total": result.entries.len(),
            "capped": result.capped,
            "depth_reached": result.depth_reached,
            "staleness": staleness_label(&result.caution),
            "staleness_note": result.caution.format_note().trim().to_string()
        });
        if let Some(hints) = macro_hints {
            if !hints.is_empty() {
                obj["macro_hints"] = json!(hints);
                obj["macro_hints_note"] = json!(
                    "Unverified (text scan). Lines containing the symbol name adjacent to a \
                     macro indicator (`!(` or `#[`). May include comments or string literals."
                );
            }
        }
        Ok(StepOutput::Json(obj))
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Group blast entries by module name, returning a JSON object.
///
/// Module extraction: strip leading `src/`, `crates/`, `tests/`; take up to
/// two path segments; remove the filename portion.
///
/// Examples:
/// - `crates/foo/src/bar/baz.rs` → `"foo"`
/// - `src/auth/login.rs` → `"auth"`
/// - `main.rs` → `"(root)"`
fn group_by_module(entries: &[&BlastEntry]) -> serde_json::Value {
    let mut by_module: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();

    for entry in entries {
        let module = extract_module(&entry.file_path);
        by_module.entry(module).or_default().push(json!({
            "symbol": entry.symbol_name,
            "file": entry.file_path,
            "line": entry.line
        }));
    }

    serde_json::Value::Object(
        by_module
            .into_iter()
            .map(|(k, v)| (k, serde_json::Value::Array(v)))
            .collect(),
    )
}

fn extract_module(file_path: &str) -> String {
    // Normalise separators.
    let path = file_path.replace('\\', "/");

    // Strip well-known prefixes.
    let stripped = strip_prefix(&path, &["crates/", "src/", "tests/", "test/"]);

    // Take the first component (the crate or top-level dir name).
    let first = stripped
        .split('/')
        .next()
        .unwrap_or("")
        .trim_end_matches(".rs");

    if first.is_empty() {
        "(root)".to_string()
    } else {
        first.to_string()
    }
}

fn strip_prefix<'a>(s: &'a str, prefixes: &[&str]) -> &'a str {
    for prefix in prefixes {
        if let Some(rest) = s.strip_prefix(prefix) {
            return rest;
        }
    }
    s
}

fn staleness_label(caution: &StalenessCaution) -> &'static str {
    match caution {
        StalenessCaution::None => "none",
        StalenessCaution::SomeCallSitesMayBeStale { .. } => "some_files_may_be_stale",
        StalenessCaution::GraphIsAging { .. } => "aging",
        StalenessCaution::GraphIsStale { .. } => "stale",
        StalenessCaution::LanguageNotIndexed { .. } => "stale",
    }
}

// ─── Macro scan ───────────────────────────────────────────────────────────────

/// Source file extensions we scan for macro references.
const SOURCE_EXTS: &[&str] = &["rs", "ts", "tsx", "js", "jsx", "py", "go"];

/// Directories to skip entirely during the walk.
const SKIP_DIRS: &[&str] = &[
    "target", "node_modules", ".git", ".sovereign",
    "dist", "build", ".cache", "__pycache__",
];

/// Walk `root` looking for lines that contain both `symbol` and a macro
/// indicator (`!(` for invocations, `#[` for attributes). Returns up to
/// `limit` hits as JSON objects with `file`, `line`, and `context` fields.
///
/// Each file is capped at 1 MiB to avoid hanging on generated files.
/// The walk stops as soon as `limit` hits are collected.
fn macro_scan(symbol: &str, root: &std::path::Path, limit: usize) -> Vec<serde_json::Value> {
    let mut hits = Vec::new();
    walk_source_files(root, &mut |file_path| {
        if hits.len() >= limit {
            return false; // signal: stop walking
        }
        scan_file_for_macro_refs(file_path, symbol, limit - hits.len(), &mut hits);
        true // continue
    });
    hits
}

/// Recursive directory walk. `visitor` returns `false` to abort the walk early.
fn walk_source_files(
    dir: &std::path::Path,
    visitor: &mut impl FnMut(&std::path::Path) -> bool,
) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return true,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            if !walk_source_files(&path, visitor) {
                return false;
            }
        } else if path.is_file() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if SOURCE_EXTS.contains(&ext) {
                if !visitor(&path) {
                    return false;
                }
            }
        }
    }
    true
}

/// Returns `true` if `symbol` appears in `line` as an isolated identifier —
/// i.e. not as a substring of a longer name like `MySymbolExtra`.
///
/// Rust identifier chars: `[A-Za-z0-9_]`. We require the character
/// immediately before and after the match (if present) to be non-identifier.
/// This eliminates most string-literal false positives for common short names
/// while keeping all the actual type/function reference cases.
fn has_word(line: &str, symbol: &str) -> bool {
    let bytes = line.as_bytes();
    let sym = symbol.as_bytes();
    let sym_len = sym.len();
    if sym_len == 0 {
        return false;
    }
    let mut pos = 0usize;
    while pos + sym_len <= bytes.len() {
        if bytes[pos..pos + sym_len] == *sym {
            let before_ok = pos == 0 || !is_ident_char(bytes[pos - 1]);
            let after_ok = pos + sym_len >= bytes.len()
                || !is_ident_char(bytes[pos + sym_len]);
            if before_ok && after_ok {
                return true;
            }
        }
        pos += 1;
    }
    false
}

#[inline]
fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Scan a single source file for lines containing `symbol` adjacent to a
/// macro indicator. Appends hits to `out` up to `cap`.
fn scan_file_for_macro_refs(
    path: &std::path::Path,
    symbol: &str,
    cap: usize,
    out: &mut Vec<serde_json::Value>,
) {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };

    // Skip files over 1 MiB — they're usually generated.
    if let Ok(meta) = file.metadata() {
        if meta.len() > 1_048_576 {
            return;
        }
    }

    let reader = BufReader::new(file);
    for (idx, line_result) in reader.lines().enumerate() {
        if out.len() >= cap {
            break;
        }
        let line = match line_result {
            Ok(l) => l,
            Err(_) => break,
        };
        // The line must contain the symbol name as a word boundary AND a
        // macro indicator (`!(` for invocations, `#[` for attributes).
        if has_word(&line, symbol) && (line.contains("!(") || line.contains("#[")) {
            let display_path = path.to_string_lossy();
            out.push(json!({
                "file": display_path.as_ref(),
                "line": idx + 1,
                "context": line.trim()
            }));
        }
    }
}
