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
use std::time::{SystemTime, UNIX_EPOCH};

use tree_sitter::{Node, Parser, Query, QueryCursor};
use tree_sitter_language::LanguageFn;
use streaming_iterator::StreamingIterator;

use crate::error::{Error, Result};
use crate::extractors::{ExtractedDoc, Extractor};

// ─── Language registry ────────────────────────────────────────

/// One entry per supported grammar. Captures are read uniformly from
/// `@definition` and `@name`, so there is nothing language-specific beyond
/// the grammar handle and its query source.
pub struct LanguageConfig {
    /// Short ID for the language, stored in the `language` metadata column.
    pub id: &'static str,
    /// Tree-sitter grammar handle.
    pub lang: LanguageFn,
    /// Tree-sitter S-expression query compiled on first use.
    pub symbol_query: &'static str,
    /// File extensions (without leading dot) that select this language.
    pub extensions: &'static [&'static str],
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
        },
        LanguageConfig {
            id: "typescript",
            lang: tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            symbol_query: include_str!("../../../queries/typescript/symbols.scm"),
            extensions: &["ts", "tsx"],
        },
        LanguageConfig {
            id: "javascript",
            lang: tree_sitter_javascript::LANGUAGE,
            symbol_query: include_str!("../../../queries/javascript/symbols.scm"),
            extensions: &["js", "jsx", "mjs", "cjs"],
        },
        LanguageConfig {
            id: "go",
            lang: tree_sitter_go::LANGUAGE,
            symbol_query: include_str!("../../../queries/go/symbols.scm"),
            extensions: &["go"],
        },
        LanguageConfig {
            id: "python",
            lang: tree_sitter_python::LANGUAGE,
            symbol_query: include_str!("../../../queries/python/symbols.scm"),
            extensions: &["py", "pyi"],
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

        "type_item" | "type_alias_declaration" | "type_declaration" | "type_spec" => SymbolKind::Type,

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
}

impl CodeChunk {
    /// Serialize the chunk's code-specific metadata to a JSON object.
    /// This is what lands in the `metadata` column; the same fields are
    /// also promoted to typed columns by the insert path.
    pub fn metadata_json(&self) -> serde_json::Value {
        serde_json::json!({
            "symbol_name": self.symbol_name,
            "symbol_kind": self.symbol_kind.as_str(),
            "file_path":   self.file_path,
            "line_start":  self.line_start,
            "line_end":    self.line_end,
            "language":    self.language,
            "mtime":       self.mtime,
            "content_hash": self.content_hash,
        })
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

fn file_mtime_secs(path: &Path) -> Option<i64> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md.modified().ok()?;
    let secs = mtime.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(secs as i64)
}

// Used by the watcher's file-missing branch; kept here so the whole
// time helper surface lives alongside the extractor.
#[allow(dead_code)]
pub(crate) fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

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
            assert!(line_count <= 80 + 2, "split chunk exceeded max lines: {line_count}");
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
