// SPDX-License-Identifier: AGPL-3.0-or-later
//! `check_doc_paths` — verify that file-path references in markdown docs still exist.
//!
//! Scans a markdown document for inline-code spans (`` `path/to/file` ``) that
//! look like local file-system paths, then checks each against the project root
//! (and the workspace root one level up, for cross-project refs). Returns three
//! buckets:
//!
//! - `valid`     — path was resolved to an existing file or directory
//! - `not_found` — path was checked but did not exist at any resolution base
//! - `skipped`   — token could not be meaningfully checked (URL, shell command,
//!                 Rust qualified name, or no base directory available)
//!
//! ## Primary use-case
//!
//! Keeping `SYSTEM_OVERVIEW.md` and `SOVEREIGN.md` honest after directory
//! renames, module splits, or dead code removal. A typical workflow:
//!
//! ```text
//! check_doc_paths(doc_path: "SYSTEM_OVERVIEW.md")
//! # → not_found: ["sovereign/src/main.rs", …]
//! # Fix the stale references, then re-run to confirm valid_count equals total.
//! ```
//!
//! ## Resolution order
//!
//! For each extracted path candidate:
//! 1. **Project root** — the root of the repository (`sovereign/`)
//! 2. **Workspace root** — one level up (`commonwealth-ai/`), for cross-project refs
//!    like `oicp-types/src/lib.rs` or `corpus-engine/src/notes.rs`
//! 3. **Doc directory** — the folder containing the markdown file
//!
//! The first base where the path exists wins. `not_found` entries show the
//! project-root path that was tried, so the developer knows what was checked.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

pub struct CheckDocPathsTool {
    project_root: Option<PathBuf>,
}

impl CheckDocPathsTool {
    pub fn new() -> Self {
        Self { project_root: None }
    }

    pub fn with_project_root(mut self, root: PathBuf) -> Self {
        self.project_root = Some(root);
        self
    }
}

impl Default for CheckDocPathsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for CheckDocPathsTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "check_doc_paths".to_string(),
            name: "Check Doc Paths".to_string(),
            description: "Scan a markdown document for inline-code path references \
                          (backtick spans like `path/to/file`) and verify each one \
                          exists on disk. Returns three buckets: `valid` (path exists), \
                          `not_found` (path missing — likely stale after a rename or \
                          deletion), and `skipped` (URL, Rust qualified name, or otherwise \
                          unresolvable). Resolution tries project root first, then the \
                          workspace root one level up (for cross-project refs), then the \
                          document's own directory. \
                          Use this to audit SYSTEM_OVERVIEW.md, SOVEREIGN.md, or any \
                          architecture doc after a directory rename, module split, or \
                          file removal."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "doc_path": {
                        "type": "string",
                        "description": "Path to the markdown file to check. \
                                        Relative to project root, or absolute."
                    }
                },
                "required": ["doc_path"]
            }),
            examples: vec![
                ToolExample {
                    situation: "You just renamed a module, moved a file, or deleted dead code. Run this on SYSTEM_OVERVIEW.md to find any path references that are now stale — before someone else reads a doc that points at files that no longer exist.".into(),
                    call: serde_json::json!({ "doc_path": "SYSTEM_OVERVIEW.md" }),
                },
                ToolExample {
                    situation: "You're updating architecture docs and want to verify every path you wrote actually resolves. Catches typos and wrong relative paths before they land in the repo.".into(),
                    call: serde_json::json!({ "doc_path": "sovereign/.opencode/skills/sovereign-code/SKILL.md" }),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "doc_path":  { "type": "string" },
                    "valid":     { "type": "array", "items": { "type": "string" } },
                    "not_found": { "type": "array", "items": { "type": "string" } },
                    "skipped":   { "type": "array", "items": { "type": "string" } }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        params
            .get("doc_path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::InvalidInput("check_doc_paths requires 'doc_path'".to_string())
            })?;
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let doc_path_str = params
            .get("doc_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'doc_path'".to_string()))?;

        // Resolve the document itself.
        let doc_path =
            resolve_doc_path(doc_path_str, self.project_root.as_deref()).ok_or_else(|| {
                Error::Tool {
                    tool_id: "check_doc_paths".to_string(),
                    message: format!(
                        "could not resolve doc path '{doc_path_str}' — \
                     provide an absolute path or start the server from your project root"
                    ),
                }
            })?;

        let doc_dir = doc_path.parent().map(|p| p.to_path_buf());

        let content = std::fs::read_to_string(&doc_path).map_err(|e| Error::Tool {
            tool_id: "check_doc_paths".to_string(),
            message: format!("could not read '{}': {e}", doc_path.display()),
        })?;

        // Pre-compute resolution bases (ordered: project root, workspace root, doc dir).
        let bases = build_bases(self.project_root.as_deref(), doc_dir.as_deref());

        let candidates = extract_path_candidates(&content);

        let mut valid = Vec::new();
        let mut not_found = Vec::new();
        let mut skipped = Vec::new();

        for c in &candidates {
            classify_candidate(c, &bases, &mut valid, &mut not_found, &mut skipped);
        }

        Ok(StepOutput::Json(json!({
            "doc": doc_path.to_string_lossy(),
            "total_candidates": candidates.len(),
            "valid_count":     valid.len(),
            "not_found_count": not_found.len(),
            "skipped_count":   skipped.len(),
            "not_found": not_found,
            "valid":     valid,
            "skipped":   skipped
        })))
    }
}

// ─── Candidate extraction ─────────────────────────────────────────────────────

struct PathCandidate {
    path: String,
    line: usize,
    context: String,
}

/// Extract all path-like tokens from a markdown document.
///
/// Two sources:
/// 1. **Inline backtick spans** (outside fenced code blocks) whose content
///    looks like a file-system path.
/// 2. **Fenced code-block lines** that look like standalone paths (no tree
///    decoration, starts with a path prefix or ends with a source extension).
fn extract_path_candidates(content: &str) -> Vec<PathCandidate> {
    let mut candidates = Vec::new();
    let mut in_code_block = false;
    let mut fence_char = ' '; // '`' or '~'

    for (idx, line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = line.trim();

        // Detect fence open/close.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let ch = trimmed.chars().next().unwrap_or('`');
            if !in_code_block {
                in_code_block = true;
                fence_char = ch;
            } else if ch == fence_char {
                in_code_block = false;
                fence_char = ' ';
            }
            continue;
        }

        if in_code_block {
            // Only extract lines that look like standalone paths — not shell
            // commands, comments, or tree-drawing decorations.
            if looks_like_standalone_path(trimmed) {
                candidates.push(PathCandidate {
                    path: trimmed.to_string(),
                    line: line_no,
                    context: trimmed.to_string(),
                });
            }
        } else {
            // In prose and table cells, extract backtick spans.
            extract_backtick_paths(line, line_no, &mut candidates);
        }
    }

    candidates
}

/// Scan a single prose/table line for `` `...` `` spans whose content could
/// be a file path.
fn extract_backtick_paths(line: &str, line_no: usize, out: &mut Vec<PathCandidate>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            // Skip past opening backtick(s).
            let tick_start = i;
            while i < bytes.len() && bytes[i] == b'`' {
                i += 1;
            }
            let tick_len = i - tick_start;
            let span_start = i;

            // Find matching closing sequence.
            let mut j = span_start;
            'outer: while j < bytes.len() {
                if bytes[j] == b'`' {
                    // Count consecutive closing backticks.
                    let close_start = j;
                    while j < bytes.len() && bytes[j] == b'`' {
                        j += 1;
                    }
                    if j - close_start == tick_len {
                        // Matched.
                        let span = &line[span_start..close_start];
                        if could_be_path(span) {
                            out.push(PathCandidate {
                                path: span.to_string(),
                                line: line_no,
                                context: line.to_string(),
                            });
                        }
                        break 'outer;
                    }
                    // Mismatched tick count — keep scanning.
                } else {
                    j += 1;
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
}

/// Does this string look like a file-system path (not a URL or Rust qualified name)?
fn could_be_path(s: &str) -> bool {
    if s.is_empty() || s.len() > 300 {
        return false;
    }
    if !s.contains('/') {
        return false;
    }
    if s.contains("://") {
        return false;
    }
    // Rust qualified names (Module::Type) are not paths.
    if s.contains("::") {
        return false;
    }
    // Shell argument fragments that start with `-` are flags, not paths.
    if s.starts_with('-') {
        return false;
    }
    // Must contain at least one alphanumeric character.
    s.chars().any(|c| c.is_alphanumeric())
}

/// Is this code-block line a standalone path (not a shell command, tree
/// decoration, comment, or other non-path content)?
///
/// Accepts lines that:
/// - Start with a known path prefix (`crates/`, `src/`, `./`, `../`, `/`)
/// - OR end with a common source extension AND contain a `/`
///
/// Rejects lines that:
/// - Contain tree-drawing characters (`├`, `│`, `└`, `─`)
/// - Contain spaces (shell commands like `cargo build -p foo`)
/// - Contain `://` (URLs)
/// - Contain `::` (Rust qualified names)
fn looks_like_standalone_path(s: &str) -> bool {
    if s.is_empty() || s.len() > 300 {
        return false;
    }
    // Tree-drawing decoration.
    if s.contains('├') || s.contains('│') || s.contains('└') || s.contains('─') || s.contains('┬')
    {
        return false;
    }
    if s.contains(' ') || s.contains('\t') {
        return false;
    }
    if s.contains("://") || s.contains("::") {
        return false;
    }
    if !s.contains('/') {
        return false;
    }

    const PATH_PREFIXES: &[&str] = &["crates/", "src/", "./", "../"];
    const PATH_EXTS: &[&str] = &[
        ".rs", ".toml", ".md", ".json", ".yaml", ".yml", ".sh", ".ts", ".tsx", ".js", ".jsx",
        ".lock", ".sql", ".txt", ".py", ".go", ".proto",
    ];
    let has_prefix = PATH_PREFIXES.iter().any(|p| s.starts_with(p));
    let has_ext = PATH_EXTS.iter().any(|e| s.ends_with(e));

    has_prefix || has_ext
}

// ─── Resolution ───────────────────────────────────────────────────────────────

/// Build an ordered list of (base_path, label) pairs to try when resolving
/// a relative path candidate.
///
/// Order:
/// 1. Project root (e.g. `sovereign/`)
/// 2. `project_root/crates` — for `sovereign-core/src/...` style refs in Cargo workspaces
/// 3. Workspace root — one level up (e.g. `commonwealth-ai/`) for cross-project refs
/// 4. Doc directory
fn build_bases(
    project_root: Option<&Path>,
    doc_dir: Option<&Path>,
) -> Vec<(PathBuf, &'static str)> {
    let mut bases: Vec<(PathBuf, &'static str)> = Vec::new();

    if let Some(root) = project_root {
        bases.push((root.to_path_buf(), "project_root"));
        // Cargo workspaces keep crates under `crates/`. Refs like
        // `sovereign-core/src/traits.rs` (without the `crates/` prefix)
        // resolve correctly with this base.
        let crates_dir = root.join("crates");
        if crates_dir.is_dir() {
            bases.push((crates_dir, "project_root/crates"));
        }
        if let Some(workspace) = root.parent() {
            bases.push((workspace.to_path_buf(), "workspace_root"));
        }
    }

    if let Some(dir) = doc_dir {
        bases.push((dir.to_path_buf(), "doc_dir"));
    }

    bases
}

/// Resolve a doc path string to an absolute `PathBuf`.
fn resolve_doc_path(path_str: &str, project_root: Option<&Path>) -> Option<PathBuf> {
    let p = Path::new(path_str);
    if p.is_absolute() {
        return Some(p.to_path_buf());
    }
    if let Some(root) = project_root {
        let candidate = root.join(p);
        if candidate.exists() {
            return Some(candidate);
        }
        // Also try workspace root.
        if let Some(workspace) = root.parent() {
            let ws_candidate = workspace.join(p);
            if ws_candidate.exists() {
                return Some(ws_candidate);
            }
        }
        // Return the project-root path even if it doesn't exist — execute()
        // will surface a clear error.
        return Some(candidate);
    }
    if let Ok(cwd) = std::env::current_dir() {
        return Some(cwd.join(p));
    }
    None
}

/// Returns `true` if a path string looks like a REST API route rather than a
/// file-system path. Heuristics: starts with `/v{digit}/`, or starts with a
/// well-known API prefix (`/api/`, `/status`, `/health`, `/oicp/`).
fn is_api_route(s: &str) -> bool {
    if !s.starts_with('/') {
        return false;
    }
    // /v1/, /v2/, etc.
    if s.len() >= 4 {
        let mut chars = s.chars();
        chars.next(); // '/'
        if chars.next() == Some('v') {
            if let Some(c) = chars.next() {
                if c.is_ascii_digit() {
                    return true;
                }
            }
        }
    }
    // Well-known non-file endpoints.
    const API_PREFIXES: &[&str] = &["/api/", "/status", "/health", "/oicp/", "/ws/"];
    API_PREFIXES.iter().any(|p| s.starts_with(p))
}

/// Classify a single candidate into one of the three output buckets.
fn classify_candidate(
    c: &PathCandidate,
    bases: &[(PathBuf, &'static str)],
    valid: &mut Vec<serde_json::Value>,
    not_found: &mut Vec<serde_json::Value>,
    skipped: &mut Vec<serde_json::Value>,
) {
    let path_str = &c.path;

    // Skip URLs.
    if path_str.contains("://") {
        skipped.push(json!({
            "path": path_str,
            "line": c.line,
            "reason": "url"
        }));
        return;
    }

    // Skip REST API routes — absolute paths that start with a version segment
    // (`/v1/`, `/v2/`) or well-known non-file endpoints (`/status`, `/health`,
    // `/oicp/`, `/api/`). These are URL path fragments in API documentation,
    // not file-system paths.
    if is_api_route(path_str) {
        skipped.push(json!({
            "path": path_str,
            "line": c.line,
            "reason": "api_route"
        }));
        return;
    }

    // Skip shell fragments and placeholders.
    if path_str.contains(' ')
        || path_str.contains('<')
        || path_str.contains('>')
        || path_str.starts_with('-')
    {
        skipped.push(json!({
            "path": path_str,
            "line": c.line,
            "reason": "shell_or_placeholder"
        }));
        return;
    }

    // Expand `~/` to the user's home directory.
    let expanded: String;
    let effective_path_str: &str = if path_str.starts_with("~/") || path_str == "~" {
        let home = std::env::var("HOME")
            .ok()
            .or_else(|| std::env::var("USERPROFILE").ok());
        if let Some(home) = home {
            expanded = if path_str == "~" {
                home
            } else {
                format!("{}/{}", home, &path_str[2..])
            };
            &expanded
        } else {
            skipped.push(json!({
                "path": path_str,
                "line": c.line,
                "reason": "tilde_no_home"
            }));
            return;
        }
    } else {
        path_str.as_str()
    };

    let p = Path::new(effective_path_str);

    // Absolute paths: check directly.
    if p.is_absolute() {
        if p.exists() {
            valid.push(json!({ "path": path_str, "line": c.line, "resolved_via": "absolute" }));
        } else {
            not_found.push(json!({
                "path": path_str,
                "line": c.line,
                "checked_at": path_str,
                "context": c.context
            }));
        }
        return;
    }

    if bases.is_empty() {
        skipped.push(json!({
            "path": path_str,
            "line": c.line,
            "reason": "no_base_directory"
        }));
        return;
    }

    // Try each base in order; take the first hit.
    for (base, label) in bases {
        let candidate = base.join(p);
        if candidate.exists() {
            valid.push(json!({
                "path": path_str,
                "line": c.line,
                "resolved_via": label
            }));
            return;
        }
    }

    // None of the bases found it — report not_found with what we checked.
    let checked_at: Vec<String> = bases
        .iter()
        .map(|(b, _)| b.join(p).to_string_lossy().into_owned())
        .collect();

    not_found.push(json!({
        "path": path_str,
        "line": c.line,
        "context": c.context,
        "checked_at": checked_at
    }));
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn could_be_path_accepts_relative_paths() {
        assert!(could_be_path("crates/foo/src/lib.rs"));
        assert!(could_be_path("oicp-types/src/lib.rs"));
        assert!(could_be_path("src/notes.rs"));
        assert!(could_be_path("corpus-engine/src/engine/mod.rs"));
    }

    #[test]
    fn could_be_path_rejects_urls_and_qualified_names() {
        assert!(!could_be_path("http://localhost:9741/mcp"));
        assert!(!could_be_path("https://example.com/foo"));
        assert!(!could_be_path("Module::sub::thing"));
        assert!(!could_be_path("no_slash_here"));
        assert!(!could_be_path(""));
    }

    #[test]
    fn looks_like_standalone_path_rejects_tree_lines() {
        assert!(!looks_like_standalone_path("├── sovereign-core/"));
        assert!(!looks_like_standalone_path("│   ├── traits.rs"));
        assert!(!looks_like_standalone_path("└── lib.rs"));
    }

    #[test]
    fn looks_like_standalone_path_accepts_clean_paths() {
        assert!(looks_like_standalone_path(
            "crates/sovereign-cli/src/main.rs"
        ));
        assert!(looks_like_standalone_path("src/notes.rs"));
    }

    #[test]
    fn looks_like_standalone_path_rejects_shell_commands() {
        assert!(!looks_like_standalone_path("cargo build -p sovereign-cli"));
        assert!(!looks_like_standalone_path("cd /path/to/dir && make"));
    }

    #[test]
    fn extract_backtick_paths_finds_paths() {
        let mut out = Vec::new();
        extract_backtick_paths(
            "shared types live in `oicp-types/src/lib.rs` and are re-exported",
            1,
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "oicp-types/src/lib.rs");
    }

    #[test]
    fn extract_backtick_paths_ignores_non_paths() {
        let mut out = Vec::new();
        extract_backtick_paths("call `symbol_lookup(\"TypeName\")` to confirm", 1, &mut out);
        // symbol_lookup("TypeName") has no `/` — should be skipped
        assert_eq!(out.len(), 0);
    }

    #[test]
    fn extract_backtick_paths_ignores_urls() {
        let mut out = Vec::new();
        extract_backtick_paths("server at `http://localhost:9741/mcp`", 1, &mut out);
        assert_eq!(out.len(), 0);
    }

    #[test]
    fn extract_path_candidates_from_prose() {
        let md = "The types are in `oicp-types/src/lib.rs` and `corpus-engine/src/notes.rs`.";
        let candidates = extract_path_candidates(md);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].path, "oicp-types/src/lib.rs");
        assert_eq!(candidates[1].path, "corpus-engine/src/notes.rs");
    }

    #[test]
    fn extract_path_candidates_skips_code_block_tree_lines() {
        let md = "```\n├── crates/\n│   └── main.rs\nsrc/lib.rs\n```";
        let candidates = extract_path_candidates(md);
        // Tree lines rejected, but clean path inside code block passes.
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, "src/lib.rs");
    }

    #[test]
    fn build_bases_includes_workspace_root() {
        // Use a real dir that has no `crates/` subdirectory so the optional
        // `project_root/crates` base is absent — keeps the assertion count stable.
        let project_root = PathBuf::from("/tmp");
        let bases = build_bases(Some(&project_root), None);
        // /tmp has no `crates/` subdir → only project_root + workspace_root
        let labels: Vec<&str> = bases.iter().map(|(_, l)| *l).collect();
        assert!(labels.contains(&"project_root"));
        assert!(labels.contains(&"workspace_root"));
    }

    #[test]
    fn is_api_route_detects_versioned_routes() {
        assert!(is_api_route("/v1/embeddings"));
        assert!(is_api_route("/v2/chat/completions"));
        assert!(is_api_route("/status"));
        assert!(is_api_route("/health"));
        assert!(is_api_route("/oicp/v1/capabilities"));
        assert!(!is_api_route("crates/foo/src/lib.rs"));
        assert!(!is_api_route("src/main.rs"));
    }
}
