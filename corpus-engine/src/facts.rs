//! Deterministic code-fact base — the spec↔code drift substrate.
//!
//! Extracts three tree-sitter fact primitives that generalize across claim shapes:
//!   - construction-field  `Type { field: VALUE }`  (the data-flow fact, e.g. `tools: None`)
//!   - string-literal       literal + enclosing fn    (endpoints, prompt markers, formats)
//!   - function-definition  name + location           (existence)
//!
//! The fact *schema* (`Facts`, `CtorField`, `StrLit`, `FnDef`) and every extraction
//! loop are **language-agnostic**. The only per-language surface is a [`LangPack`]:
//! a grammar plus four tree-sitter queries. Facts serialize to `facts.json` in the
//! corpus dir and are loaded by the query/dispatch layer (`facts_check`).
//! See `docs/internal/FACT_BASE_SCALE_OUT.md`.
//!
//! ─────────────────────────────────────────────────────────────────────────────
//! # Adding a language — the ONE extension point
//!
//! Fact extraction reads whatever languages have a pack in [`lang_packs`], and
//! nothing else in this file (walker, dispatch, extraction) is language-specific.
//! To add a language:
//!
//! 1. Add its `tree-sitter-<lang>` grammar crate to `corpus-engine/Cargo.toml`
//!    under the `treesitter` feature (Rust, Python, TS, JS, Go are already there).
//! 2. Append one [`LangPack`] literal to [`lang_packs`] below. Fill in five things:
//!      - `id` / `extensions`         — trivial
//!      - `fn_query` + `fn_node_kind` — the function-definition node for this grammar
//!      - `str_query`                 — the string-literal node
//!      - `ctor_query`                — **the one modeling decision** (see below)
//! 3. That's it. No dispatch, walker, or loop changes — they read the pack.
//!
//! ## The `ctor_query` is where the judgment lives
//!
//! The construction-field fact answers "is a typed value built here with FIELD set
//! to VALUE?" — the data-flow fact behind CONFIG-style claims ("`tools` defaults to
//! `None`"). "Constructing a typed value with named fields" is spelled differently
//! in every language, so this query is a *design choice*, not a mechanical port.
//! Whatever you write MUST capture exactly three names: `@s` (the type/constructor),
//! `@f` (the field/argument name), `@v` (the value node). The extraction loop keys
//! off those captures and is otherwise blind to syntax.
//!
//!   - Rust   → struct literal      `Type { field: value }`
//!   - Python → constructor kwargs  `Type(field=value)`  (plain ctors, `@dataclass`,
//!              pydantic, and attrs all construct through keyword args, so one query
//!              covers them). Dict literals `{"field": value}` are DELIBERATELY not
//!              matched — a dict isn't a *typed* value, so treating it as one would
//!              flood the fact base with false config sites. That's a coverage
//!              trade-off worth making explicit, not a bug.
//!
//! A language whose config never takes a "named field on a typed thing" shape can
//! leave `ctor_query` empty (`""`); it simply contributes no CONFIG facts, and those
//! claims fall to the fuzzy layer. Fewer deterministic facts, never wrong ones.
//! ─────────────────────────────────────────────────────────────────────────────

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

// ─── Language packs — the extension point (see the module doc above) ─────────

/// Everything language-specific about fact extraction, in one place. Add a
/// language by appending a `LangPack` to [`lang_packs`]; nothing else changes.
///
/// Query capture-name contract (the extraction loops depend on it):
///   - `ctor_query` MUST capture `@s` (type), `@f` (field), `@v` (value); or be `""`.
///   - `str_query`  MUST capture `@s` (the string node).
///   - `fn_query`   MUST capture `@n` (the function-name identifier).
#[cfg(feature = "treesitter")]
pub struct LangPack {
    /// Stable identifier, e.g. `"rust"`, `"python"`. Appears in logs/diagnostics.
    pub id: &'static str,
    /// Source-file extensions (no leading dot) that select this pack.
    pub extensions: &'static [&'static str],
    /// The tree-sitter grammar (`tree_sitter_<lang>::LANGUAGE`).
    pub lang: tree_sitter_language::LanguageFn,
    /// Construction-field query — the data-flow fact. Captures `@s @f @v`.
    /// Empty string = this language contributes no CONFIG facts (see module doc).
    pub ctor_query: &'static str,
    /// String-literal query. Captures `@s`.
    pub str_query: &'static str,
    /// Function-definition query. Captures `@n`.
    pub fn_query: &'static str,
    /// Tree-sitter node KIND that denotes a function body — used to attribute a
    /// fact to its enclosing function (`enclosing_fn`).
    pub fn_node_kind: &'static str,
}

/// The registry of supported languages. **This is the list you extend.**
#[cfg(feature = "treesitter")]
pub fn lang_packs() -> &'static [LangPack] {
    &[
        // ── Rust ──────────────────────────────────────────────────────────
        LangPack {
            id: "rust",
            extensions: &["rs"],
            lang: tree_sitter_rust::LANGUAGE,
            // `Type { field: value }`
            ctor_query: "(struct_expression name: (_) @s body: (field_initializer_list (field_initializer field: (field_identifier) @f value: (_) @v)))",
            str_query: "(string_literal) @s",
            fn_query: "(function_item name: (identifier) @n)",
            fn_node_kind: "function_item",
        },
        // ── Python ────────────────────────────────────────────────────────
        LangPack {
            id: "python",
            extensions: &["py", "pyi"],
            lang: tree_sitter_python::LANGUAGE,
            // `Type(field=value)` — constructor keyword arguments. One query covers
            // plain constructors, @dataclass, pydantic, and attrs (all kwargs-built).
            // Dict literals are intentionally NOT matched (see module doc).
            ctor_query: "(call function: (identifier) @s arguments: (argument_list (keyword_argument name: (identifier) @f value: (_) @v)))",
            str_query: "(string) @s",
            fn_query: "(function_definition name: (identifier) @n)",
            fn_node_kind: "function_definition",
        },
    ]
}

/// Select the pack for a file extension (`"rs"`, `"py"`, …). `None` = unsupported
/// file → skipped, never guessed.
#[cfg(feature = "treesitter")]
pub fn pack_for_extension(ext: &str) -> Option<&'static LangPack> {
    lang_packs().iter().find(|p| p.extensions.contains(&ext))
}

/// Directory names never worth walking for facts: build outputs, VCS, and
/// dependency trees. Recursing `target/` alone stat-walks millions of
/// build-artifact entries — the fact base is source facts, not artifacts, so
/// skipping these makes `code facts` fast and keeps vendored code out.
#[cfg(feature = "treesitter")]
fn is_ignored_dir(name: &str) -> bool {
    matches!(
        name,
        "target" | ".git" | "node_modules" | "vendor" | ".sovereign" | ".svrnmesh"
    ) || name.starts_with('.')
}

/// Recursively collect source files that some pack knows how to read. Dispatch by
/// extension happens in [`extract_facts`]; unsupported files never enter the list.
#[cfg(feature = "treesitter")]
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let skip = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(is_ignored_dir);
                if !skip {
                    walk(&p, out);
                }
            } else if p
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|ext| pack_for_extension(ext).is_some())
            {
                out.push(p);
            }
        }
    }
}

#[cfg(feature = "treesitter")]
mod ts {
    use super::{CtorField, Facts, FnDef, LangPack, StrLit};
    use streaming_iterator::StreamingIterator;
    use tree_sitter::{Node, Parser, Query, QueryCursor};

    /// Per-file SCIP symbol-def extraction (the structural watcher hot path).
    /// Reuses the pack's `fn_query` (name capture `@n`), then walks up to the
    /// enclosing `fn_node_kind` node for the full body span. Lines are 0-based
    /// to match the SCIP protobuf ingest. See the public wrapper's doc for the
    /// fidelity contract (bare name, function-kind only, no cross-file edges).
    pub fn extract_symbol_defs(
        rel: &str,
        src: &str,
        pack: &LangPack,
    ) -> Vec<corpus_engine_scip::ScipSymbolRecord> {
        let lang: tree_sitter::Language = pack.lang.into();
        let mut parser = Parser::new();
        if parser.set_language(&lang).is_err() {
            return Vec::new();
        }
        let tree = match parser.parse(src, None) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let b = src.as_bytes();
        let root = tree.root_node();
        let mut out = Vec::new();

        let q = match Query::new(&lang, pack.fn_query) {
            Ok(q) => q,
            Err(_) => return out,
        };
        let ni = match q.capture_index_for_name("n") {
            Some(i) => i,
            None => return out,
        };
        let mut c = QueryCursor::new();
        let mut ms = c.matches(&q, root, b);
        while let Some(m) = ms.next() {
            if let Some(name_node) = m.nodes_for_capture_index(ni).next() {
                let name = name_node.utf8_text(b).unwrap_or("").to_string();
                if name.is_empty() {
                    continue;
                }
                // Span comes from the enclosing function node (body), not the
                // name identifier — mirrors how the SCIP export reads
                // `enclosing_range` for `line_end`.
                let (line_start, line_end) = fn_span(name_node, pack.fn_node_kind);
                out.push(corpus_engine_scip::ScipSymbolRecord {
                    name,
                    qualified_name: String::new(),
                    kind: "function".to_string(),
                    file_path: rel.to_string(),
                    line_start,
                    line_end,
                    language: pack.id.to_string(),
                });
            }
        }
        out
    }

    /// 0-based `(start_row, end_row)` of the nearest `fn_node_kind` ancestor of
    /// the name node — the function body span. Falls back to the name node's own
    /// span when no enclosing function node is found (defensive; shouldn't
    /// happen for a well-formed `fn_query`).
    fn fn_span(name_node: Node, fn_node_kind: &str) -> (i32, i32) {
        let mut n = name_node;
        loop {
            if n.kind() == fn_node_kind {
                return (
                    n.start_position().row as i32,
                    n.end_position().row as i32,
                );
            }
            match n.parent() {
                Some(p) => n = p,
                None => break,
            }
        }
        (
            name_node.start_position().row as i32,
            name_node.end_position().row as i32,
        )
    }

    /// Nearest ancestor of `fn_node_kind`, returning its `name` field. The kind is
    /// supplied by the pack so this stays language-agnostic.
    fn enclosing_fn(mut n: Node, src: &[u8], fn_node_kind: &str) -> String {
        while let Some(p) = n.parent() {
            if p.kind() == fn_node_kind {
                if let Some(name) = p.child_by_field_name("name") {
                    return name.utf8_text(src).unwrap_or("").to_string();
                }
            }
            n = p;
        }
        String::new()
    }

    /// Extract all three fact kinds from one file using `pack`'s queries. The loops
    /// key off capture names (`@s @f @v`, `@s`, `@n`), never node kinds, so they are
    /// identical for every language.
    pub fn extract_file(rel: &str, src: &str, pack: &LangPack, f: &mut Facts) {
        let lang: tree_sitter::Language = pack.lang.into();
        let mut parser = Parser::new();
        if parser.set_language(&lang).is_err() {
            return;
        }
        let tree = match parser.parse(src, None) {
            Some(t) => t,
            None => return,
        };
        let b = src.as_bytes();
        let root = tree.root_node();

        // construction fields  (@s type, @f field, @v value) — skipped if empty
        if !pack.ctor_query.is_empty() {
            if let Ok(q) = Query::new(&lang, pack.ctor_query) {
                if let (Some(si), Some(fi), Some(vi)) = (
                    q.capture_index_for_name("s"),
                    q.capture_index_for_name("f"),
                    q.capture_index_for_name("v"),
                ) {
                    let mut c = QueryCursor::new();
                    let mut ms = c.matches(&q, root, b);
                    while let Some(m) = ms.next() {
                        if let (Some(s), Some(fl), Some(v)) = (
                            m.nodes_for_capture_index(si).next(),
                            m.nodes_for_capture_index(fi).next(),
                            m.nodes_for_capture_index(vi).next(),
                        ) {
                            f.ctor_fields.push(CtorField {
                                struct_type: s.utf8_text(b).unwrap_or("").to_string(),
                                field: fl.utf8_text(b).unwrap_or("").to_string(),
                                value: v.utf8_text(b).unwrap_or("").chars().take(60).collect(),
                                enclosing_fn: enclosing_fn(v, b, pack.fn_node_kind),
                                file: rel.to_string(),
                                line: v.start_position().row + 1,
                            });
                        }
                    }
                }
            }
        }

        // string literals  (@s)
        if let Ok(q) = Query::new(&lang, pack.str_query) {
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
                                enclosing_fn: enclosing_fn(n, b, pack.fn_node_kind),
                                file: rel.to_string(),
                                line: n.start_position().row + 1,
                            });
                        }
                    }
                }
            }
        }

        // function definitions  (@n)
        if let Ok(q) = Query::new(&lang, pack.fn_query) {
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

/// Extract the deterministic fact base from a repository. `roots` are src dirs
/// relative to `repo` (e.g. `["src"]`, or crate/package paths for a monorepo).
/// Multi-language: each file is routed to its [`LangPack`] by extension; files no
/// pack claims are skipped.
#[cfg(feature = "treesitter")]
pub fn extract_facts(repo: &Path, roots: &[String]) -> Facts {
    let mut files = Vec::new();
    for r in roots {
        walk(&repo.join(r), &mut files);
    }
    let mut f = Facts::default();
    for path in &files {
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };
        let pack = match pack_for_extension(ext) {
            Some(p) => p,
            None => continue, // unsupported extension — skip, never guess
        };
        if let Ok(src) = std::fs::read_to_string(path) {
            let rel = path
                .strip_prefix(repo)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            ts::extract_file(&rel, &src, pack, &mut f);
        }
    }
    f
}

/// Fallback when the `treesitter` feature is disabled — returns an empty fact base.
#[cfg(not(feature = "treesitter"))]
pub fn extract_facts(_repo: &Path, _roots: &[String]) -> Facts {
    Facts::default()
}

/// Extract SCIP symbol-definition rows for ONE file, using tree-sitter only —
/// no rust-analyzer, no embeddings, no I/O beyond the caller-supplied source.
///
/// This is the structural watcher's hot-path primitive: on a file save the
/// daemon re-parses just that file (milliseconds) and merges the result via
/// [`corpus_engine_scip::ScipGraph::replace_files`], so `symbols(name)` finds a
/// newly-added or moved function immediately — without waiting for the heavy,
/// whole-workspace rust-analyzer export that runs later (idle-gated).
///
/// ## Fidelity — deliberately partial, honestly so
///
/// tree-sitter sees a single file, so these rows carry:
///   - `qualified_name: ""` — no cross-crate SCIP descriptor (rust-analyzer's
///     job). Resolution falls back to `name`, which is what the bare-name
///     lookups (`symbols`, `resolve_symbol`) use anyway.
///   - `kind: "function"` — only function definitions, the existence fact.
/// The idle rust-analyzer pass later REPLACES these same file rows with fully
/// qualified, multi-kind symbols and the cross-file reference edges tree-sitter
/// cannot see. So overlay rows are a fresh-but-coarse stand-in between saves,
/// never the final word. Lines are 0-based to match the SCIP protobuf ingest
/// (`scip_export::parse_scip_file`), NOT the 1-based facts convention.
///
/// Returns an empty vec for a file whose extension no [`LangPack`] claims.
/// Extract the full fact set (fn defs + config construction-fields + string
/// literals) for ONE file — the per-file companion to [`extract_facts`], for the
/// structural watcher's overlay. Same tree-sitter machinery, one file's worth,
/// so a save re-extracts ~166 records instead of the whole 280k-record repo.
/// Returns an empty [`Facts`] for a file no [`LangPack`] claims.
#[cfg(feature = "treesitter")]
pub fn extract_facts_for_file(rel: &str, src: &str) -> Facts {
    let mut f = Facts::default();
    let ext = match std::path::Path::new(rel).extension().and_then(|e| e.to_str()) {
        Some(e) => e,
        None => return f,
    };
    if let Some(pack) = pack_for_extension(ext) {
        ts::extract_file(rel, src, pack, &mut f);
    }
    f
}

#[cfg(feature = "treesitter")]
pub fn extract_symbol_defs(rel: &str, src: &str) -> Vec<corpus_engine_scip::ScipSymbolRecord> {
    let ext = match std::path::Path::new(rel).extension().and_then(|e| e.to_str()) {
        Some(e) => e,
        None => return Vec::new(),
    };
    let pack = match pack_for_extension(ext) {
        Some(p) => p,
        None => return Vec::new(),
    };
    ts::extract_symbol_defs(rel, src, pack)
}

#[cfg(all(test, feature = "treesitter"))]
mod tests {
    use super::*;

    // Behaviour-preserving: the Rust pack extracts exactly what the pre-pack code did.
    #[test]
    fn rust_pack_extracts_fn_str_ctor() {
        let src = "fn make_runtime() {\n    let c = Cfg { tools: None, label: \"prod\" };\n    let _ = c;\n}\n";
        let pack = pack_for_extension("rs").expect("rust pack");
        let mut f = Facts::default();
        ts::extract_file("lib.rs", src, pack, &mut f);

        assert!(f.fn_defs.iter().any(|d| d.name == "make_runtime"));
        let tools = f
            .ctor_fields
            .iter()
            .find(|c| c.struct_type == "Cfg" && c.field == "tools")
            .expect("Cfg.tools ctor field");
        assert!(tools.value.contains("None"));
        assert_eq!(tools.enclosing_fn, "make_runtime");
        assert!(f.str_lits.iter().any(|s| s.content.contains("prod")));
    }

    #[test]
    fn walk_skips_build_and_vcs_dirs() {
        assert!(is_ignored_dir("target"));
        assert!(is_ignored_dir(".git"));
        assert!(is_ignored_dir("node_modules"));
        assert!(is_ignored_dir("vendor"));
        assert!(is_ignored_dir(".hidden"));
        assert!(!is_ignored_dir("src"));
        assert!(!is_ignored_dir("corpus-engine"));
    }

    #[test]
    fn extract_facts_for_file_gets_all_kinds() {
        let f = extract_facts_for_file(
            "lib.rs",
            "fn go() {\n    let c = Cfg { on: true };\n    let _ = \"hello world\";\n}\n",
        );
        assert!(f.fn_defs.iter().any(|d| d.name == "go"));
        assert!(f.ctor_fields.iter().any(|c| c.struct_type == "Cfg" && c.field == "on"));
        assert!(f.str_lits.iter().any(|s| s.content.contains("hello world")));
        // Non-source file yields nothing.
        assert!(extract_facts_for_file("README.md", "# hi").fn_defs.is_empty());
    }

    // ── extract_symbol_defs (structural watcher hot-path primitive) ──

    #[test]
    fn symbol_defs_capture_name_and_0based_span() {
        // fn spans lines 0..2 (0-based): `fn foo() {` on row 0, `}` on row 2.
        let src = "fn foo() {\n    let x = 1;\n}\n";
        let defs = extract_symbol_defs("src/lib.rs", src);
        assert_eq!(defs.len(), 1);
        let d = &defs[0];
        assert_eq!(d.name, "foo");
        assert_eq!(d.kind, "function");
        assert_eq!(d.language, "rust");
        assert_eq!(d.file_path, "src/lib.rs");
        assert_eq!(d.qualified_name, ""); // bare — rust-analyzer fills this later
        assert_eq!(d.line_start, 0, "0-based to match SCIP ingest");
        assert_eq!(d.line_end, 2, "end = enclosing fn node, not the name node");
    }

    #[test]
    fn symbol_defs_multiple_functions() {
        let src = "fn a() {}\nfn b() {\n  let z=2;\n}\n";
        let mut names: Vec<String> =
            extract_symbol_defs("m.rs", src).into_iter().map(|d| d.name).collect();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn symbol_defs_python() {
        let defs = extract_symbol_defs("app.py", "def handler(req):\n    return 1\n");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "handler");
        assert_eq!(defs[0].language, "python");
    }

    #[test]
    fn symbol_defs_unsupported_extension_is_empty() {
        assert!(extract_symbol_defs("README.md", "# hi\nfn not_code() {}").is_empty());
        assert!(extract_symbol_defs("noext", "fn x() {}").is_empty());
    }

    // The new Python pack: same three facts out of idiomatic Python.
    #[test]
    fn python_pack_extracts_fn_str_ctor() {
        let src = "\
def build_runtime(name):
    cfg = Config(tools=None, label=\"prod\")
    return cfg
";
        let pack = pack_for_extension("py").expect("python pack");
        let mut f = Facts::default();
        ts::extract_file("app.py", src, pack, &mut f);

        // existence fact
        assert!(
            f.fn_defs.iter().any(|d| d.name == "build_runtime"),
            "fn_defs={:?}",
            f.fn_defs
        );
        // data-flow (CONFIG) fact — Type(field=value)
        let tools = f
            .ctor_fields
            .iter()
            .find(|c| c.struct_type == "Config" && c.field == "tools")
            .unwrap_or_else(|| panic!("Config(tools=...) not found in {:?}", f.ctor_fields));
        assert!(tools.value.contains("None"));
        // enclosing-fn scoping works for Python's function_definition node
        assert_eq!(tools.enclosing_fn, "build_runtime");
        // string-literal fact
        assert!(
            f.str_lits.iter().any(|s| s.content.contains("prod")),
            "str_lits={:?}",
            f.str_lits
        );
    }

    #[test]
    fn pyi_extension_also_resolves_python() {
        assert_eq!(pack_for_extension("pyi").map(|p| p.id), Some("python"));
        assert_eq!(pack_for_extension("rs").map(|p| p.id), Some("rust"));
        assert!(pack_for_extension("java").is_none());
    }
}
