// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tree-sitter-backed code extractor.
//!
//! Language-agnostic: one pipeline handles every language with a registered
//! Tree-sitter grammar. Adding a new language is strictly additive — one
//! grammar crate in Cargo.toml, one entry in [`all_languages()`], one
//! `queries/{lang}/symbols.scm` file. Nothing else changes.
//!
//! Every query file uses the same capture conventions:
//!
//! - `@definition` captures the full symbol node (function, class, etc.)
//! - `@name`       captures the identifier within that node
//!
//! The extractor reads only those two captures — any language-specific
//! information (node kind, symbol classification) is derived from the
//! captured node's type string, not from hardcoded language branches.
//!
//! This module is gated behind the `treesitter` Cargo feature so default
//! builds skip the tree-sitter dependency entirely.
//!
//! ## Output shape
//!
//! One [`ExtractedDoc`] is yielded per symbol (function, class, struct,
//! etc.). The doc's `source_id` is the repo-relative file path — every
//! symbol from the same file shares a source_id so file-level reindex
//! via [`CorpusIndex::delete_chunks_by_source_doc`] nukes them all in one
//! call.
//!
//! Metadata is attached as a JSON object that will be promoted into the
//! typed columns added in the same change (`symbol_name`, `symbol_kind`,
//! `file_path`, `line_start`, `line_end`, `language`, `mtime`). Storing
//! the same values in the metadata JSON AND in the typed columns keeps
//! JSON-reading legacy callers working while new filter-pushdown callers
//! get the sub-10ms lookup path.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};
use tree_sitter_language::LanguageFn;

use crate::error::{Error, Result};
use crate::extractors::{ExtractedDoc, Extractor};

// ─── Language registry ────────────────────────────────────────

/// Per-language hook that pulls (visibility, doc-comment) from a
/// captured `@definition` node. Each language registers its own
/// implementation because the AST conventions differ — Rust's
/// `visibility_modifier` child + `///` outer doc siblings, TS/JS's
/// `export` keyword + leading `/** */` JSDoc, Python's docstring as
/// first string in the body, Go's lowercase/uppercase + leading `//`
/// godoc lines.
///
/// Returning `(false, None)` is the safe fallback for languages that
/// haven't been wired up yet — the atlas walker will emit those
/// items with no description and treat them as private (so they're
/// excluded from the default atlas without `--include-private`).
pub type MetadataExtractor = fn(Node, &[u8]) -> (bool, Option<String>);

/// One entry per supported grammar. Captures are read uniformly from
/// `@definition` and `@name`, so there is nothing language-specific
/// beyond the grammar handle, its query source, and its
/// metadata-extractor hook.
pub struct LanguageConfig {
    /// Short ID for the language, stored in the `language` metadata column.
    pub id: &'static str,
    /// Tree-sitter grammar handle.
    pub lang: LanguageFn,
    /// Tree-sitter S-expression query compiled on first use.
    pub symbol_query: &'static str,
    /// File extensions (without leading dot) that select this language.
    pub extensions: &'static [&'static str],
    /// Pulls visibility + doc comment for an `@definition` node.
    /// Languages without bespoke logic point at
    /// [`metadata_extractor_default`].
    pub metadata_extractor: MetadataExtractor,
}

/// Static list of supported languages. Add a new language by appending to
/// this list — `language_for_extension()` builds its lookup map from it.
pub fn all_languages() -> &'static [LanguageConfig] {
    &[
        LanguageConfig {
            id: "rust",
            lang: tree_sitter_rust::LANGUAGE,
            symbol_query: include_str!("../../../queries/rust/symbols.scm"),
            extensions: &["rs"],
            metadata_extractor: rust_metadata_extractor,
        },
        LanguageConfig {
            id: "typescript",
            lang: tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            symbol_query: include_str!("../../../queries/typescript/symbols.scm"),
            extensions: &["ts", "tsx"],
            metadata_extractor: jsdoc_metadata_extractor,
        },
        LanguageConfig {
            id: "javascript",
            lang: tree_sitter_javascript::LANGUAGE,
            symbol_query: include_str!("../../../queries/javascript/symbols.scm"),
            extensions: &["js", "jsx", "mjs", "cjs"],
            metadata_extractor: jsdoc_metadata_extractor,
        },
        LanguageConfig {
            id: "go",
            lang: tree_sitter_go::LANGUAGE,
            symbol_query: include_str!("../../../queries/go/symbols.scm"),
            extensions: &["go"],
            metadata_extractor: godoc_metadata_extractor,
        },
        LanguageConfig {
            id: "python",
            lang: tree_sitter_python::LANGUAGE,
            symbol_query: include_str!("../../../queries/python/symbols.scm"),
            extensions: &["py", "pyi"],
            metadata_extractor: python_metadata_extractor,
        },
    ]
}

static EXT_INDEX: OnceLock<HashMap<&'static str, &'static LanguageConfig>> = OnceLock::new();

fn ext_index() -> &'static HashMap<&'static str, &'static LanguageConfig> {
    EXT_INDEX.get_or_init(|| {
        let mut map = HashMap::new();
        for cfg in all_languages() {
            for ext in cfg.extensions {
                map.insert(*ext, cfg);
            }
        }
        map
    })
}

/// Look up a language by file extension (case-sensitive, without leading dot).
pub fn language_for_extension(ext: &str) -> Option<&'static LanguageConfig> {
    ext_index().get(ext).copied()
}

/// True iff the path has an extension that maps to a supported language.
pub fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(language_for_extension)
        .is_some()
}

// ─── Symbol kinds ──────────────────────────────────────────────

/// Kind of symbol, inferred from the tree-sitter node type. No
/// language-specific string parsing — the mapping is a pure function of
/// the node's grammar rule name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Interface,
    Impl,
    Type,
    Const,
    Module,
    Unknown,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Impl => "impl",
            Self::Type => "type",
            Self::Const => "const",
            Self::Module => "module",
            Self::Unknown => "unknown",
        }
    }
}

fn kind_from_node_type(node_type: &str) -> SymbolKind {
    match node_type {
        "function_item"
        | "function_declaration"
        | "function_definition"
        | "lexical_declaration"
        | "variable_declaration" => SymbolKind::Function,

        "method_declaration" | "method_definition" => SymbolKind::Method,

        "class_declaration" | "class_definition" => SymbolKind::Class,

        "struct_item" => SymbolKind::Struct,

        "enum_item" | "enum_declaration" => SymbolKind::Enum,

        "trait_item" => SymbolKind::Trait,

        "interface_declaration" => SymbolKind::Interface,

        "impl_item" => SymbolKind::Impl,

        "type_item" | "type_alias_declaration" | "type_declaration" | "type_spec" => {
            SymbolKind::Type
        }

        "const_item" | "const_declaration" | "const_spec" | "static_item" | "var_spec"
        | "var_declaration" => SymbolKind::Const,

        "mod_item" => SymbolKind::Module,

        "decorated_definition" => SymbolKind::Function, // refined below for classes

        _ => SymbolKind::Unknown,
    }
}

// ─── Code chunk ────────────────────────────────────────────────

/// A single symbol-level chunk extracted from a source file. Emitted one
/// per symbol. Oversize symbols are split into multiple chunks along blank
/// lines by [`CodeExtractor::split_large`].
#[derive(Debug, Clone)]
pub struct CodeChunk {
    pub content: String,
    pub symbol_name: String,
    pub symbol_kind: SymbolKind,
    pub file_path: String,
    pub line_start: usize,
    pub line_end: usize,
    pub language: &'static str,
    pub content_hash: String,
    pub mtime: i64,
    /// True when the symbol is exported from its module (Rust `pub`,
    /// TypeScript `export`, Go capitalised name, Python non-underscore).
    /// Currently populated only for Rust; defaults to `false` for
    /// other languages until their visibility extraction lands.
    pub is_public: bool,
    /// Leading rustdoc / JSDoc / docstring text attached to the
    /// symbol, if any. Currently populated only for Rust (`///` and
    /// `/** */` siblings preceding the item); other languages default
    /// to `None`. Used by the atlas code-walk to source entity
    /// descriptions without an LLM call.
    pub doc_comment: Option<String>,
}

impl CodeChunk {
    /// Serialize the chunk's code-specific metadata to a JSON object.
    /// This is what lands in the `metadata` column; the same fields are
    /// also promoted to typed columns by the insert path.
    pub fn metadata_json(&self) -> serde_json::Value {
        let mut obj = serde_json::json!({
            "symbol_name": self.symbol_name,
            "symbol_kind": self.symbol_kind.as_str(),
            "file_path":   self.file_path,
            "line_start":  self.line_start,
            "line_end":    self.line_end,
            "language":    self.language,
            "mtime":       self.mtime,
            "content_hash": self.content_hash,
            "is_public":   self.is_public,
        });
        if let Some(doc) = &self.doc_comment {
            obj.as_object_mut()
                .expect("metadata JSON is an object")
                .insert(
                    "doc_comment".to_string(),
                    serde_json::Value::String(doc.clone()),
                );
        }
        obj
    }
}

// ─── Extractor ─────────────────────────────────────────────────

/// Configuration for [`CodeExtractor`]. Defaults match the recipe
/// spec and target typical repo shapes without tuning.
#[derive(Debug, Clone)]
pub struct CodeExtractor {
    /// Lines of context included before and after each symbol. Gives the
    /// AI enough surrounding text (imports, decorators) to understand the
    /// symbol without a second fetch.
    pub context_lines: usize,
    /// Hard upper bound on chunk length. Symbols larger than this are
    /// split at blank-line boundaries so a single huge function doesn't
    /// become a single huge embedding.
    pub max_lines_per_chunk: usize,
}

impl Default for CodeExtractor {
    fn default() -> Self {
        Self {
            context_lines: 3,
            max_lines_per_chunk: 150,
        }
    }
}

impl CodeExtractor {
    /// Extract symbol-level chunks from a single source file.
    ///
    /// Returns `Ok(vec![])` when the extension is unsupported — this is
    /// deliberately not an error so the directory-walking impl can
    /// continue past every unknown file without failing the whole run.
    pub fn extract_file(
        &self,
        content: &str,
        rel_path: &str,
        mtime: i64,
    ) -> Result<Vec<CodeChunk>> {
        let ext = Path::new(rel_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let lang_cfg = match language_for_extension(ext) {
            Some(c) => c,
            None => return Ok(Vec::new()),
        };

        let language: tree_sitter::Language = lang_cfg.lang.into();
        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| Error::Extraction(format!("tree-sitter set_language: {e}")))?;

        let Some(tree) = parser.parse(content, None) else {
            return Err(Error::Extraction(format!(
                "tree-sitter failed to parse {rel_path}"
            )));
        };

        let query = Query::new(&language, lang_cfg.symbol_query)
            .map_err(|e| Error::Extraction(format!("tree-sitter query compile: {e}")))?;

        let def_idx = query
            .capture_index_for_name("definition")
            .ok_or_else(|| Error::Extraction("symbol query missing @definition capture".into()))?;
        let name_idx = query
            .capture_index_for_name("name")
            .ok_or_else(|| Error::Extraction("symbol query missing @name capture".into()))?;

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();
        let mut chunks = Vec::new();
        let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

        while let Some(m) = matches.next() {
            let def_node: Option<Node> = m.nodes_for_capture_index(def_idx).next();
            let name_node: Option<Node> = m.nodes_for_capture_index(name_idx).next();
            let (def_node, name_node) = match (def_node, name_node) {
                (Some(d), Some(n)) => (d, n),
                _ => continue,
            };

            let symbol_name = match name_node.utf8_text(content.as_bytes()) {
                Ok(s) => s.to_string(),
                Err(_) => continue,
            };
            let symbol_kind = kind_from_node_type(def_node.kind());

            let line_start = def_node.start_position().row;
            let line_end = def_node.end_position().row;
            if !seen.insert((line_start, line_end)) {
                // Same node captured by multiple query patterns (e.g. a
                // class definition matched by both a bare and a decorated
                // rule). Dedupe by span.
                continue;
            }

            let ctx_start = line_start.saturating_sub(self.context_lines);
            let ctx_end = (line_end + self.context_lines).min(total_lines.saturating_sub(1));
            if total_lines == 0 || ctx_start > ctx_end {
                continue;
            }

            let chunk_content = lines[ctx_start..=ctx_end].join("\n");
            let content_hash = blake3::hash(chunk_content.as_bytes()).to_hex().to_string();

            let (is_public, doc_comment) =
                (lang_cfg.metadata_extractor)(def_node, content.as_bytes());

            chunks.push(CodeChunk {
                content: chunk_content,
                symbol_name,
                symbol_kind,
                file_path: rel_path.to_string(),
                line_start: ctx_start,
                line_end: ctx_end,
                language: lang_cfg.id,
                content_hash,
                mtime,
                is_public,
                doc_comment,
            });
        }

        Ok(self.split_large(chunks))
    }

    /// Split any chunk exceeding `max_lines_per_chunk` at a blank-line
    /// boundary. Preserves the original `line_start`/`line_end` offsets
    /// on the split pieces so callers can navigate back to the file.
    fn split_large(&self, chunks: Vec<CodeChunk>) -> Vec<CodeChunk> {
        let max = self.max_lines_per_chunk;
        chunks
            .into_iter()
            .flat_map(|chunk| {
                let lines: Vec<&str> = chunk.content.lines().collect();
                if lines.len() <= max {
                    return vec![chunk];
                }

                let mut result = Vec::new();
                let mut start = 0;
                while start < lines.len() {
                    let cap = (start + max).min(lines.len());
                    // Find the last blank line within [start, cap), or
                    // fall back to the hard cap.
                    let split = (start..cap)
                        .rev()
                        .find(|&i| lines[i].trim().is_empty())
                        .unwrap_or(cap);
                    let end = if split <= start { cap } else { split };
                    let body = lines[start..end].join("\n");
                    let hash = blake3::hash(body.as_bytes()).to_hex().to_string();
                    result.push(CodeChunk {
                        content: body,
                        line_start: chunk.line_start + start,
                        line_end: chunk.line_start + end.saturating_sub(1),
                        content_hash: hash,
                        symbol_name: chunk.symbol_name.clone(),
                        symbol_kind: chunk.symbol_kind,
                        file_path: chunk.file_path.clone(),
                        language: chunk.language,
                        mtime: chunk.mtime,
                        is_public: chunk.is_public,
                        doc_comment: chunk.doc_comment.clone(),
                    });
                    if end == start {
                        break;
                    }
                    start = end;
                }
                result
            })
            .collect()
    }
}

impl Extractor for CodeExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let root = source_path.to_path_buf();
        let extractor = self.clone();

        // Walk the tree eagerly, but yield lazily — one `ExtractedDoc` per
        // symbol. We stream rather than collect so large repos don't blow
        // memory before ingest can consume them.
        let walker = walkdir::WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                // Skip common junk directories. Keep this list tight —
                // anything missed here is cheap (it just gets filtered
                // out at the is_source_file() check below).
                let name = e.file_name().to_string_lossy();
                !(e.depth() > 0
                    && (name == ".git"
                        || name == "node_modules"
                        || name == "target"
                        || name == "dist"
                        || name == "build"
                        || name == "__pycache__"
                        || name.starts_with('.')))
            });

        let iter = walker
            .filter_map(|r| r.ok())
            .filter(|e: &walkdir::DirEntry| e.file_type().is_file())
            .filter(|e: &walkdir::DirEntry| is_source_file(e.path()))
            .flat_map(move |entry: walkdir::DirEntry| {
                let abs_path = entry.path().to_path_buf();
                let rel = abs_path
                    .strip_prefix(&root)
                    .unwrap_or(&abs_path)
                    .to_string_lossy()
                    .into_owned();

                let mtime = file_mtime_secs(&abs_path).unwrap_or(0);
                let content = match std::fs::read_to_string(&abs_path) {
                    Ok(c) => c,
                    Err(_) => return Vec::new().into_iter(),
                };

                let chunks = match extractor.extract_file(&content, &rel, mtime) {
                    Ok(cs) => cs,
                    Err(e) => {
                        tracing::warn!(file = %rel, error = %e, "code extractor failed");
                        return Vec::new().into_iter();
                    }
                };

                chunks
                    .into_iter()
                    .map(|chunk| {
                        Ok(ExtractedDoc {
                            title: Some(chunk.symbol_name.clone()),
                            // Every symbol from the same file shares a
                            // source_id so file-level reindex nukes the
                            // whole file's chunks in one delete call.
                            source_id: chunk.file_path.clone(),
                            url: None,
                            metadata: Some(chunk.metadata_json()),
                            content: chunk.content,
                            source_file: None,
                            embed_text: None,
                        })
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
            });

        Ok(Box::new(iter))
    }
}

/// Walk preceding siblings of a `@definition` node, collecting any
/// comments whose first non-whitespace text starts with one of the
/// supplied prefixes. Stops at the first non-comment sibling. Returns
/// the joined text in source order, with the language-specific marker
/// stripped per `strip_fn`.
///
/// Common shape: most languages put doc comments as `line_comment`
/// or `block_comment` siblings preceding the declaration.
fn collect_preceding_doc_comments(
    def_node: Node,
    source: &[u8],
    accept: impl Fn(&str) -> bool,
    strip: impl Fn(&str) -> String,
) -> Option<String> {
    let mut docs: Vec<String> = Vec::new();
    let mut cursor = def_node.prev_sibling();
    while let Some(node) = cursor {
        let kind = node.kind();
        let is_comment = kind == "line_comment" || kind == "block_comment" || kind == "comment";
        if !is_comment {
            break;
        }
        let text = match node.utf8_text(source) {
            Ok(t) => t,
            Err(_) => break,
        };
        let trimmed = text.trim_start();
        if !accept(trimmed) {
            break;
        }
        docs.push(strip(text).trim_end().to_string());
        cursor = node.prev_sibling();
    }
    docs.reverse();
    if docs.is_empty() {
        None
    } else {
        let joined = docs.join("\n").trim().to_string();
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    }
}

/// Default metadata hook: report `is_public = false` and no docs.
/// Used as the fallback for any language whose extractor hasn't been
/// implemented yet. Atlas behaviour for these languages: items appear
/// as private (excluded from the default atlas without
/// `--include-private`), descriptions are empty.
#[allow(dead_code)] // Reserved for future languages.
fn metadata_extractor_default(_def_node: Node, _source: &[u8]) -> (bool, Option<String>) {
    (false, None)
}

/// Rust `@definition` metadata hook.
///
/// Inspects the captured node for visibility (`pub`) and the leading
/// rustdoc block. The Rust grammar places `visibility_modifier` as a
/// child of the item node and emits doc comments as `line_comment` /
/// `block_comment` siblings preceding the item — at the same tree
/// depth as the item itself.
fn rust_metadata_extractor(def_node: Node, source: &[u8]) -> (bool, Option<String>) {
    rust_visibility_and_docs(def_node, source)
}

/// TypeScript / JavaScript `@definition` metadata hook.
///
/// Visibility: looks for an `export_statement` parent or an `export`
/// keyword sibling. The query captures the inner declaration (e.g.
/// `function_declaration`); the export wrapper is its parent or an
/// adjacent sibling depending on grammar version.
///
/// Doc comment: JSDoc is a `/** ... */` block_comment immediately
/// preceding the declaration. Line `//` comments are NOT treated as
/// JSDoc, matching the convention.
fn jsdoc_metadata_extractor(def_node: Node, source: &[u8]) -> (bool, Option<String>) {
    let is_public = is_ts_or_js_exported(def_node, source);
    // Climb past an `export_statement` / `export_declaration` parent
    // so JSDoc preceding the export wrapper is visible. Tree-sitter
    // nests the inner declaration under the wrapper; sibling lookup
    // on the inner node alone misses the JSDoc.
    let doc_anchor = match def_node.parent() {
        Some(p) if matches!(p.kind(), "export_statement" | "export_declaration") => p,
        _ => def_node,
    };
    let docs = collect_preceding_doc_comments(
        doc_anchor,
        source,
        |trimmed| trimmed.starts_with("/**"),
        |text| {
            let trimmed = text.trim_start();
            let body = trimmed
                .strip_prefix("/**")
                .map(|rest| rest.trim_end_matches("*/"))
                .unwrap_or(trimmed);
            body.lines()
                .map(|l| {
                    l.trim_start()
                        .trim_start_matches('*')
                        .trim_start_matches(' ')
                })
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string()
        },
    );
    (is_public, docs)
}

fn is_ts_or_js_exported(def_node: Node, source: &[u8]) -> bool {
    // Walk up to the immediate enclosing statement; an
    // `export_statement` parent flags the export.
    let parent = def_node.parent();
    if let Some(p) = parent {
        if matches!(p.kind(), "export_statement" | "export_declaration") {
            return true;
        }
    }
    // Some captures land on the inner declaration; the `export`
    // keyword may also appear as a sibling.
    let mut walker = def_node.walk();
    if let Some(p) = parent {
        for child in p.children(&mut walker) {
            if child.kind() == "export" {
                return true;
            }
            if let Ok(text) = child.utf8_text(source) {
                if text == "export" {
                    return true;
                }
            }
        }
    }
    false
}

/// Go `@definition` metadata hook.
///
/// Visibility: Go uses identifier capitalization. The first capture
/// at `@name` carries the identifier; an uppercase first character
/// = exported. We re-derive that here from the def_node by finding
/// its `identifier` / `field_identifier` / `type_identifier` child.
///
/// Doc comment: godoc convention is a contiguous `//`-line block
/// directly preceding the declaration. We collect those.
fn godoc_metadata_extractor(def_node: Node, source: &[u8]) -> (bool, Option<String>) {
    let is_public = first_identifier_starts_uppercase(def_node, source);
    let docs = collect_preceding_doc_comments(
        def_node,
        source,
        |trimmed| trimmed.starts_with("//"),
        |text| {
            text.trim_start()
                .strip_prefix("//")
                .unwrap_or(text)
                .trim_start_matches(' ')
                .to_string()
        },
    );
    (is_public, docs)
}

fn first_identifier_starts_uppercase(def_node: Node, source: &[u8]) -> bool {
    let mut walker = def_node.walk();
    for child in def_node.children(&mut walker) {
        if matches!(
            child.kind(),
            "identifier" | "field_identifier" | "type_identifier"
        ) {
            if let Ok(text) = child.utf8_text(source) {
                return text.chars().next().is_some_and(|c| c.is_uppercase());
            }
        }
    }
    false
}

/// Python `@definition` metadata hook.
///
/// Visibility: Python uses leading-underscore convention. We re-derive
/// from the captured node's identifier child — names starting with
/// `_` are treated as non-public (single-underscore + dunder both).
///
/// Doc comment: Python docstrings are not comment nodes — they're a
/// `string` expression statement at the top of the function/class
/// body. We pull the first such expression's text.
fn python_metadata_extractor(def_node: Node, source: &[u8]) -> (bool, Option<String>) {
    let is_public = first_identifier_visible_python(def_node, source);
    let docs = python_docstring(def_node, source);
    (is_public, docs)
}

fn first_identifier_visible_python(def_node: Node, source: &[u8]) -> bool {
    let mut walker = def_node.walk();
    for child in def_node.children(&mut walker) {
        if child.kind() == "identifier" {
            if let Ok(text) = child.utf8_text(source) {
                return !text.starts_with('_');
            }
        }
    }
    // No identifier (rare edge case for module-level captures) —
    // treat as public so the atlas doesn't lose them.
    true
}

fn python_docstring(def_node: Node, source: &[u8]) -> Option<String> {
    let mut walker = def_node.walk();
    let body = def_node
        .children(&mut walker)
        .find(|c| matches!(c.kind(), "block" | "module"))?;
    let mut body_walker = body.walk();
    let first_stmt = body.children(&mut body_walker).next()?;
    if first_stmt.kind() != "expression_statement" {
        return None;
    }
    let mut stmt_walker = first_stmt.walk();
    let str_node = first_stmt
        .children(&mut stmt_walker)
        .find(|c| c.kind() == "string")?;
    let raw = str_node.utf8_text(source).ok()?;
    Some(strip_python_string_quotes(raw).trim().to_string()).filter(|s| !s.is_empty())
}

fn strip_python_string_quotes(raw: &str) -> String {
    let trimmed = raw.trim_start_matches(['r', 'b', 'u', 'f', 'R', 'B', 'U', 'F']);
    for delim in ["\"\"\"", "'''", "\"", "'"] {
        if let Some(rest) = trimmed.strip_prefix(delim) {
            if let Some(body) = rest.strip_suffix(delim) {
                return body.to_string();
            }
        }
    }
    trimmed.to_string()
}

/// Inspect a tree-sitter `@definition` node for a Rust item to recover
/// visibility (`pub`) and the leading rustdoc block. The Rust grammar
/// places `visibility_modifier` as a child of the item node and emits
/// doc comments as `line_comment` / `block_comment` siblings preceding
/// the item — at the same tree depth as the item itself.
///
/// Returns `(is_public, doc_comment)`.
fn rust_visibility_and_docs(def_node: Node, source: &[u8]) -> (bool, Option<String>) {
    // Visibility: scan immediate children for `visibility_modifier`. A
    // `pub` modifier renders as a node whose text contains "pub".
    let mut is_public = false;
    let mut walker = def_node.walk();
    for child in def_node.children(&mut walker) {
        if child.kind() == "visibility_modifier" {
            if let Ok(text) = child.utf8_text(source) {
                if text.contains("pub") {
                    is_public = true;
                }
            }
            break;
        }
    }

    // Doc comments: walk prev_sibling, accumulating only OUTER doc
    // comments (`///` line comments, `/** */` block comments). Stop
    // at any non-comment sibling, OR at any non-doc/inner-doc
    // comment. Inner doc comments (`//!`, `/*! */`) belong to the
    // enclosing module, not the next item, and must NOT be attached
    // here — doing so silently bleeds the crate's `//!` block onto
    // the first item in `lib.rs`. Collect bottom-up (closest sibling
    // first) and reverse for source order.
    let mut docs: Vec<String> = Vec::new();
    let mut cursor = def_node.prev_sibling();
    while let Some(node) = cursor {
        let kind = node.kind();
        let is_comment = kind == "line_comment" || kind == "block_comment";
        if !is_comment {
            break;
        }
        let text = match node.utf8_text(source) {
            Ok(t) => t,
            Err(_) => break,
        };
        let trimmed = text.trim_start();
        let is_outer_doc = trimmed.starts_with("///") || trimmed.starts_with("/**");
        if !is_outer_doc {
            break;
        }
        let stripped = strip_doc_comment_markers(text);
        docs.push(stripped.trim_end().to_string());
        cursor = node.prev_sibling();
    }
    docs.reverse();

    let doc_comment = if docs.is_empty() {
        None
    } else {
        Some(docs.join("\n").trim().to_string())
    };

    (is_public, doc_comment)
}

/// Strip leading `///` / `//!` / `/** */` markers from a Rust doc
/// comment, preserving inner content. Block comments are unwrapped of
/// their `/**` and `*/` framing; line comments have one space removed
/// after the marker so callers see ergonomic prose, not " hello world"
/// with a leading space.
fn strip_doc_comment_markers(text: &str) -> String {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("///") {
        return rest.trim_start_matches(' ').to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("//!") {
        return rest.trim_start_matches(' ').to_string();
    }
    if let Some(rest) = trimmed
        .strip_prefix("/**")
        .or_else(|| trimmed.strip_prefix("/*!"))
    {
        let body = rest.trim_end_matches("*/").trim();
        // Drop common leading " * " on each interior line.
        return body
            .lines()
            .map(|l| {
                l.trim_start()
                    .trim_start_matches('*')
                    .trim_start_matches(' ')
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    text.to_string()
}

fn file_mtime_secs(path: &Path) -> Option<i64> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md.modified().ok()?;
    let secs = mtime.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(secs as i64)
}

// Crate-local alias onto the shared time helper; kept for the watcher's
// file-missing branch (no in-crate callers yet, hence the allow).
#[allow(unused_imports)]
pub(crate) use corpus_engine_yield::time::unix_now as now_unix_secs;

// ─── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_symbols() {
        let src = "\
pub fn foo() {
    println!(\"hi\");
}

pub struct Bar {
    x: i32,
}

pub trait Baz {
    fn quux(&self);
}
";
        let ex = CodeExtractor::default();
        let chunks = ex.extract_file(src, "src/lib.rs", 1_700_000_000).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.symbol_name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"Bar"));
        assert!(names.contains(&"Baz"));
        assert_eq!(chunks[0].language, "rust");
        for chunk in &chunks {
            assert_eq!(chunk.file_path, "src/lib.rs");
            assert_eq!(chunk.mtime, 1_700_000_000);
            assert!(!chunk.content_hash.is_empty());
        }
    }

    #[test]
    fn kinds_are_classified() {
        let src = "pub fn f() {}\npub struct S;\npub trait T {}\npub enum E { A }\n";
        let ex = CodeExtractor::default();
        let chunks = ex.extract_file(src, "src/lib.rs", 0).unwrap();
        let kinds: std::collections::HashMap<_, _> = chunks
            .iter()
            .map(|c| (c.symbol_name.as_str(), c.symbol_kind))
            .collect();
        assert_eq!(kinds.get("f"), Some(&SymbolKind::Function));
        assert_eq!(kinds.get("S"), Some(&SymbolKind::Struct));
        assert_eq!(kinds.get("T"), Some(&SymbolKind::Trait));
        assert_eq!(kinds.get("E"), Some(&SymbolKind::Enum));
    }

    #[test]
    fn unknown_extension_returns_empty_vec() {
        let ex = CodeExtractor::default();
        let chunks = ex.extract_file("some text", "notes.txt", 0).unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn rust_visibility_and_docs_are_captured() {
        let src = "\
/// Public function with one-line rustdoc.
pub fn documented_fn() {}

fn private_fn() {}

/// Multi-line docs.
/// Second line.
pub struct Documented;

pub struct Undocumented;
";
        let ex = CodeExtractor::default();
        let chunks = ex.extract_file(src, "src/lib.rs", 0).unwrap();
        let by_name: std::collections::HashMap<_, _> =
            chunks.iter().map(|c| (c.symbol_name.as_str(), c)).collect();
        let documented_fn = by_name.get("documented_fn").expect("documented_fn");
        assert!(documented_fn.is_public, "documented_fn should be public");
        assert_eq!(
            documented_fn.doc_comment.as_deref(),
            Some("Public function with one-line rustdoc.")
        );
        let private_fn = by_name.get("private_fn").expect("private_fn");
        assert!(!private_fn.is_public, "private_fn should not be public");
        assert!(private_fn.doc_comment.is_none());
        let documented_struct = by_name.get("Documented").expect("Documented");
        assert!(documented_struct.is_public);
        assert_eq!(
            documented_struct.doc_comment.as_deref(),
            Some("Multi-line docs.\nSecond line.")
        );
        let undoc_struct = by_name.get("Undocumented").expect("Undocumented");
        assert!(undoc_struct.is_public);
        assert!(undoc_struct.doc_comment.is_none());
    }

    #[test]
    fn large_symbol_is_split() {
        let mut body = String::from("pub fn huge() {\n");
        for i in 0..300 {
            body.push_str(&format!("    let x{i} = {i};\n"));
            if i % 25 == 24 {
                body.push('\n'); // blank line for the splitter to find
            }
        }
        body.push_str("}\n");

        let ex = CodeExtractor {
            context_lines: 0,
            max_lines_per_chunk: 80,
        };
        let chunks = ex.extract_file(&body, "src/lib.rs", 0).unwrap();
        assert!(chunks.len() > 1, "expected the large symbol to split");
        for c in &chunks {
            let line_count = c.content.lines().count();
            assert!(
                line_count <= 80 + 2,
                "split chunk exceeded max lines: {line_count}"
            );
        }
    }

    #[test]
    fn python_class_and_function() {
        let src = "class Foo:\n    def bar(self):\n        return 1\n\ndef baz():\n    pass\n";
        let ex = CodeExtractor::default();
        let chunks = ex.extract_file(src, "app.py", 0).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.symbol_name.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"bar"));
        assert!(names.contains(&"baz"));
    }

    #[test]
    fn go_function_and_type() {
        let src = "package main\n\nfunc Foo() int { return 1 }\n\ntype Bar struct { X int }\n";
        let ex = CodeExtractor::default();
        let chunks = ex.extract_file(src, "main.go", 0).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.symbol_name.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"Bar"));
    }

    #[test]
    fn go_visibility_uses_capitalization() {
        let src = "package main\n\n\
            // Foo is exported because its name starts uppercase.\n\
            func Foo() int { return 1 }\n\n\
            func bar() int { return 2 }\n";
        let ex = CodeExtractor::default();
        let chunks = ex.extract_file(src, "main.go", 0).unwrap();
        let by_name: std::collections::HashMap<_, _> =
            chunks.iter().map(|c| (c.symbol_name.as_str(), c)).collect();
        let foo = by_name.get("Foo").expect("Foo missing");
        assert!(foo.is_public, "uppercase Foo must be public");
        assert_eq!(
            foo.doc_comment.as_deref(),
            Some("Foo is exported because its name starts uppercase.")
        );
        let bar = by_name.get("bar").expect("bar missing");
        assert!(!bar.is_public, "lowercase bar must not be public");
        assert!(bar.doc_comment.is_none());
    }

    #[test]
    fn python_visibility_uses_underscore_convention() {
        let src = "class Public:\n    \"\"\"Public class doc.\"\"\"\n    pass\n\n\
                   class _Private:\n    pass\n\n\
                   def public_fn():\n    \"\"\"Top-level fn doc.\"\"\"\n    return 1\n\n\
                   def _hidden():\n    pass\n";
        let ex = CodeExtractor::default();
        let chunks = ex.extract_file(src, "app.py", 0).unwrap();
        let by_name: std::collections::HashMap<_, _> =
            chunks.iter().map(|c| (c.symbol_name.as_str(), c)).collect();
        let public_class = by_name.get("Public").expect("Public missing");
        assert!(public_class.is_public);
        assert_eq!(
            public_class.doc_comment.as_deref(),
            Some("Public class doc.")
        );
        let private_class = by_name.get("_Private").expect("_Private missing");
        assert!(!private_class.is_public);
        let public_fn = by_name.get("public_fn").expect("public_fn missing");
        assert!(public_fn.is_public);
        assert_eq!(public_fn.doc_comment.as_deref(), Some("Top-level fn doc."));
        let hidden = by_name.get("_hidden").expect("_hidden missing");
        assert!(!hidden.is_public);
    }

    #[test]
    fn typescript_visibility_uses_export_keyword() {
        let src =
            "/** Documented TS function. */\nexport function greet(): string { return 'hi'; }\n\n\
                   function internal(): string { return 'no'; }\n\n\
                   export class MyClass { do(): void {} }\n";
        let ex = CodeExtractor::default();
        let chunks = ex.extract_file(src, "src/index.ts", 0).unwrap();
        let by_name: std::collections::HashMap<_, _> =
            chunks.iter().map(|c| (c.symbol_name.as_str(), c)).collect();
        let greet = by_name.get("greet").expect("greet missing");
        assert!(greet.is_public, "exported greet must be flagged public");
        assert_eq!(
            greet.doc_comment.as_deref(),
            Some("Documented TS function.")
        );
        let internal = by_name.get("internal").expect("internal missing");
        assert!(!internal.is_public);
        let class = by_name.get("MyClass").expect("MyClass missing");
        assert!(class.is_public, "exported class must be flagged public");
    }

    #[test]
    fn metadata_json_roundtrips() {
        let src = "pub fn greet() { println!(\"hi\"); }\n";
        let ex = CodeExtractor::default();
        let chunks = ex.extract_file(src, "src/lib.rs", 42).unwrap();
        let meta = chunks[0].metadata_json();
        assert_eq!(meta["symbol_name"], "greet");
        assert_eq!(meta["symbol_kind"], "function");
        assert_eq!(meta["file_path"], "src/lib.rs");
        assert_eq!(meta["language"], "rust");
        assert_eq!(meta["mtime"], 42);
    }
}
