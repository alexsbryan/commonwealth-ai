//! Deterministic code-fact base — the spec↔code drift substrate.
//!
//! Extracts three tree-sitter fact primitives that generalize across claim shapes:
//!   - construction-field  `Type { field: VALUE }`  (the data-flow fact, e.g. `tools: None`)
//!   - string-literal       literal + enclosing fn    (endpoints, prompt markers, formats)
//!   - function-definition  name + location           (existence)
//!
//! The schema is language-agnostic; only the tree-sitter query pack is per-language (Rust for
//! now — additional languages are a config extension, as the code index already does). Facts
//! serialize to `facts.json` in the corpus dir and are loaded by the query/dispatch layer.
//! See `docs/internal/FACT_BASE_SCALE_OUT.md`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A `Type { field: VALUE }` construction-site field — the data-flow fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtorField {
    pub struct_type: String,
    pub field: String,
    pub value: String,
    pub enclosing_fn: String,
    pub file: String,
    pub line: usize,
}

/// A string literal with its enclosing function (for scoping).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrLit {
    pub content: String,
    pub enclosing_fn: String,
    pub file: String,
    pub line: usize,
}

/// A function definition — the existence fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FnDef {
    pub name: String,
    pub file: String,
    pub line: usize,
}

/// The deterministic fact base for one repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Facts {
    pub ctor_fields: Vec<CtorField>,
    pub str_lits: Vec<StrLit>,
    pub fn_defs: Vec<FnDef>,
}

impl Facts {
    pub fn load(path: &Path) -> std::io::Result<Facts> {
        let s = std::fs::read_to_string(path)?;
        serde_json::from_str(&s)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }
}

fn walk_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk_rs(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
}

#[cfg(feature = "treesitter")]
mod ts {
    use super::{CtorField, Facts, FnDef, StrLit};
    use streaming_iterator::StreamingIterator;
    use tree_sitter::{Node, Parser, Query, QueryCursor};

    fn enclosing_fn(mut n: Node, src: &[u8]) -> String {
        while let Some(p) = n.parent() {
            if p.kind() == "function_item" {
                if let Some(name) = p.child_by_field_name("name") {
                    return name.utf8_text(src).unwrap_or("").to_string();
                }
            }
            n = p;
        }
        String::new()
    }

    pub fn extract_file(rel: &str, src: &str, lang: &tree_sitter::Language, f: &mut Facts) {
        let mut parser = Parser::new();
        if parser.set_language(lang).is_err() {
            return;
        }
        let tree = match parser.parse(src, None) {
            Some(t) => t,
            None => return,
        };
        let b = src.as_bytes();
        let root = tree.root_node();

        // construction fields
        if let Ok(q) = Query::new(lang, "(struct_expression name: (_) @s body: (field_initializer_list (field_initializer field: (field_identifier) @f value: (_) @v)))") {
            if let (Some(si), Some(fi), Some(vi)) = (q.capture_index_for_name("s"), q.capture_index_for_name("f"), q.capture_index_for_name("v")) {
                let mut c = QueryCursor::new();
                let mut ms = c.matches(&q, root, b);
                while let Some(m) = ms.next() {
                    if let (Some(s), Some(fl), Some(v)) = (m.nodes_for_capture_index(si).next(), m.nodes_for_capture_index(fi).next(), m.nodes_for_capture_index(vi).next()) {
                        f.ctor_fields.push(CtorField {
                            struct_type: s.utf8_text(b).unwrap_or("").to_string(),
                            field: fl.utf8_text(b).unwrap_or("").to_string(),
                            value: v.utf8_text(b).unwrap_or("").chars().take(60).collect(),
                            enclosing_fn: enclosing_fn(v, b),
                            file: rel.to_string(),
                            line: v.start_position().row + 1,
                        });
                    }
                }
            }
        }

        // string literals
        if let Ok(q) = Query::new(lang, "(string_literal) @s") {
            if let Some(si) = q.capture_index_for_name("s") {
                let mut c = QueryCursor::new();
                let mut ms = c.matches(&q, root, b);
                while let Some(m) = ms.next() {
                    if let Some(n) = m.nodes_for_capture_index(si).next() {
                        let content: String =
                            n.utf8_text(b).unwrap_or("").chars().take(200).collect();
                        if content.len() > 3 {
                            f.str_lits.push(StrLit {
                                content,
                                enclosing_fn: enclosing_fn(n, b),
                                file: rel.to_string(),
                                line: n.start_position().row + 1,
                            });
                        }
                    }
                }
            }
        }

        // function definitions
        if let Ok(q) = Query::new(lang, "(function_item name: (identifier) @n)") {
            if let Some(ni) = q.capture_index_for_name("n") {
                let mut c = QueryCursor::new();
                let mut ms = c.matches(&q, root, b);
                while let Some(m) = ms.next() {
                    if let Some(n) = m.nodes_for_capture_index(ni).next() {
                        f.fn_defs.push(FnDef {
                            name: n.utf8_text(b).unwrap_or("").to_string(),
                            file: rel.to_string(),
                            line: n.start_position().row + 1,
                        });
                    }
                }
            }
        }
    }
}

/// Extract the deterministic fact base from a repository. `roots` are src dirs relative to
/// `repo` (e.g. `["src"]`, or crate paths for a monorepo). Rust-only for now.
#[cfg(feature = "treesitter")]
pub fn extract_facts(repo: &Path, roots: &[String]) -> Facts {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut files = Vec::new();
    for r in roots {
        walk_rs(&repo.join(r), &mut files);
    }
    let mut f = Facts::default();
    for path in &files {
        if let Ok(src) = std::fs::read_to_string(path) {
            let rel = path
                .strip_prefix(repo)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            ts::extract_file(&rel, &src, &lang, &mut f);
        }
    }
    f
}

/// Fallback when the `treesitter` feature is disabled — returns an empty fact base.
#[cfg(not(feature = "treesitter"))]
pub fn extract_facts(_repo: &Path, _roots: &[String]) -> Facts {
    Facts::default()
}
