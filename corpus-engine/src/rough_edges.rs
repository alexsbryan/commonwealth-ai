//! Internal contradiction + rough-edge detection over a code corpus.
//!
//! Two complementary signals:
//!
//! - **Markers** (FIXME/TODO/HACK/XXX) — places the team has explicitly
//!   tagged as "not finished" or "known wrong". Pure regex over source
//!   files; no LLM, no SCIP, no atlas dependency. Surfaces the
//!   human-authored inventory of rough edges a new dev should know
//!   about. (Tier 0)
//!
//! - **Doc-vs-signature drift** — places where a function's rustdoc
//!   makes a claim that the signature or body contradicts (e.g., doc
//!   references a parameter that doesn't exist, doc has `# Panics`
//!   section but body has no panic). Structural, no LLM. Lifted onto
//!   the structural atlas's already-extracted `doc_comment` +
//!   signature info. (Tier 1 — separate module path; see
//!   [`scan_doc_drift`].)
//!
//! Both streams produce [`RoughEdgeFinding`]s into the same JSON
//! sidecar so the drift-report renderer can fold them into one
//! "Internal" section without caring which detector found them.

#![cfg(feature = "treesitter")]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// One actionable observation about the code that someone may want to
/// fix. Both marker comments and doc-drift findings flow through this
/// shape so the renderer is uniform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoughEdgeFinding {
    pub kind: FindingKind,
    pub severity: Severity,
    pub file: PathBuf,
    pub line: u32,
    pub symbol: Option<String>,
    pub message: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(tag = "kind")]
pub enum FindingKind {
    Marker(MarkerKind),
    DocDrift(DocDriftKind),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum MarkerKind {
    Todo,
    Fixme,
    Hack,
    Xxx,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DocDriftKind {
    /// `# Returns`/`# Errors`/`# Panics`-section keyword listed but
    /// the function body or signature contradicts it.
    SectionMismatch,
    /// Rustdoc names a parameter that the signature lacks (rename or
    /// removal drift).
    MissingParam,
    /// Inline-code identifier in doc that's absent from the workspace
    /// symbol set.
    UnknownIdent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Note,
    Likely,
    Critical,
}

/// Match a marker comment: `// TODO …`, `/* FIXME(name): … */`,
/// `/// HACK …`, `* XXX:`. The marker must be a standalone token
/// (word boundary), and the line must look comment-shaped — we don't
/// match `let todo = …;`. Captures: 1=marker token, 2=comment body.
fn marker_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?ix)
            (?:^|\s|\*)
            (?: //+ | /\*+ | \*+ )
            \s*
            ( TODO | FIXME | HACK | XXX )
            \b
            [^\w]?
            \s*
            ( .* )
            ",
        )
        .expect("marker_re compile")
    })
}

/// Walk `source_root` for source files and emit one finding per
/// marker comment. Skips standard junk directories (target, .git,
/// node_modules, dist, build, __pycache__) and non-source extensions.
///
/// Rust-only in v1. Other languages share comment markers but
/// language-aware comment-leader patterns belong in a follow-up.
pub fn scan_markers(source_root: &Path) -> Vec<RoughEdgeFinding> {
    let mut out: Vec<RoughEdgeFinding> = Vec::new();
    for entry in walkdir::WalkDir::new(source_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !(e.depth() > 0
                && (name == ".git"
                    || name == "node_modules"
                    || name == "target"
                    || name == "dist"
                    || name == "build"
                    || name == "__pycache__"
                    || name.starts_with(".")
                        && name != ".cargo"))
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !is_source_file(path) {
            continue;
        }
        let raw = match std::fs::read_to_string(path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        // Cap per-file scan size at 1 MiB. Source files larger than
        // that are almost certainly generated and produce noise.
        if raw.len() > 1024 * 1024 {
            continue;
        }
        for (i, line) in raw.lines().enumerate() {
            let Some(caps) = marker_re().captures(line) else {
                continue;
            };
            let marker_str = &caps[1];
            let marker = marker_to_kind(marker_str);
            let body = caps
                .get(2)
                .map(|m| m.as_str().trim())
                .unwrap_or("")
                .to_string();
            // Strip trailing `*/` on block comments.
            let body = body.trim_end_matches("*/").trim().to_string();
            let snippet = line.trim().to_string();
            // Truncate excessively long snippets — keep at most 160
            // chars so the renderer's column shape stays consistent.
            let snippet = if snippet.len() > 160 {
                let mut s = snippet[..160].to_string();
                s.push_str("…");
                s
            } else {
                snippet
            };
            out.push(RoughEdgeFinding {
                kind: FindingKind::Marker(marker),
                severity: marker_severity(marker),
                file: path.to_path_buf(),
                line: (i as u32) + 1,
                symbol: None,
                message: if body.is_empty() {
                    format!("{marker_str} marker")
                } else {
                    body
                },
                snippet,
            });
        }
    }
    // Stable sort: kind, then file path, then line.
    out.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });
    out
}

/// Source-file allow-list. Rust-only in v1; expand cautiously since
/// each new language wants its own comment-leader regex.
fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| matches!(ext, "rs"))
        .unwrap_or(false)
}

fn marker_to_kind(s: &str) -> MarkerKind {
    match s.to_ascii_uppercase().as_str() {
        "TODO" => MarkerKind::Todo,
        "FIXME" => MarkerKind::Fixme,
        "HACK" => MarkerKind::Hack,
        "XXX" => MarkerKind::Xxx,
        _ => MarkerKind::Todo,
    }
}

/// Severity assignment is intentionally simple. `XXX` is the alarm
/// marker; `FIXME` and `HACK` are explicit rough edges someone
/// already knows about. `TODO` is forward-looking intent — note
/// rather than likely.
fn marker_severity(m: MarkerKind) -> Severity {
    match m {
        MarkerKind::Xxx => Severity::Critical,
        MarkerKind::Fixme | MarkerKind::Hack => Severity::Likely,
        MarkerKind::Todo => Severity::Note,
    }
}

// ── Tier 1: rustdoc-vs-signature drift ─────────────────────────
//
// Detects places where a function's rustdoc makes a structural
// claim its signature/body contradicts. Two classes in v1:
//
//   `# Panics` claim → body has no panic-shaped call
//   `# Errors` claim → signature doesn't return Result
//
// Both are likely-severity findings: real bugs in a fraction, but
// dwarfed by churn ("removed the panic, forgot to update the doc").
// Either way, surfaces the rough edge for review.
//
// Two more classes are stubbed in [`DocDriftKind`] (`MissingParam`,
// `UnknownIdent`) and will land in a follow-up — they need symbol-
// set bookkeeping and are noisier on first pass.

/// Walk `source_root` and detect rustdoc-vs-signature drift in
/// every Rust source file. Reuses [`crate::extractors::code::CodeExtractor`]
/// for the per-symbol parse so doc/signature/body slicing is
/// already done.
pub fn scan_doc_drift(source_root: &Path) -> Vec<RoughEdgeFinding> {
    use crate::extractors::code::CodeExtractor;
    let extractor = CodeExtractor::default();
    let mut out: Vec<RoughEdgeFinding> = Vec::new();
    for entry in walkdir::WalkDir::new(source_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !(e.depth() > 0
                && (name == ".git"
                    || name == "node_modules"
                    || name == "target"
                    || name == "dist"
                    || name == "build"
                    || name == "__pycache__"
                    || name.starts_with(".")
                        && name != ".cargo"))
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !is_source_file(path) {
            continue;
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if content.len() > 1024 * 1024 {
            continue;
        }
        let rel = path
            .strip_prefix(source_root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());
        let chunks = match extractor.extract_file(&content, &rel, 0) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for chunk in chunks {
            if chunk.language != "rust" {
                continue;
            }
            let Some(doc) = chunk.doc_comment.as_deref() else {
                continue;
            };
            // Strip the doc comment lines from the chunk body before
            // scanning for panic/Result signals — otherwise the doc's
            // own mention of `Result<...>` or `panic!` would mask
            // real findings.
            let body_only = strip_doc_lines(&chunk.content);

            if doc_has_section(doc, "Panics") && !body_has_panic(&body_only) {
                out.push(RoughEdgeFinding {
                    kind: FindingKind::DocDrift(DocDriftKind::SectionMismatch),
                    severity: Severity::Likely,
                    file: path.to_path_buf(),
                    line: (chunk.line_start as u32) + 1,
                    symbol: Some(chunk.symbol_name.clone()),
                    message: format!(
                        "doc claims `# Panics` but body has no panic/unwrap/expect/assert call (`{}`)",
                        chunk.symbol_name
                    ),
                    snippet: first_line_of_body(&body_only),
                });
            }
            if doc_has_section(doc, "Errors") && !returns_result(&body_only) {
                out.push(RoughEdgeFinding {
                    kind: FindingKind::DocDrift(DocDriftKind::SectionMismatch),
                    severity: Severity::Likely,
                    file: path.to_path_buf(),
                    line: (chunk.line_start as u32) + 1,
                    symbol: Some(chunk.symbol_name.clone()),
                    message: format!(
                        "doc claims `# Errors` but signature doesn't return `Result<…>` (`{}`)",
                        chunk.symbol_name
                    ),
                    snippet: first_line_of_body(&body_only),
                });
            }
        }
    }
    out.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });
    out
}

/// Convenience: run both tier 0 (markers) and tier 1 (doc-drift)
/// and return a single sorted vec. The orchestrator and the
/// standalone CLI both use this.
pub fn scan_all(source_root: &Path) -> Vec<RoughEdgeFinding> {
    let mut all = scan_markers(source_root);
    all.extend(scan_doc_drift(source_root));
    all.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });
    all
}

/// Strip lines that begin with `///` or `//!` (Rust outer/inner doc
/// comments) so downstream content scanning isn't fooled by the doc
/// itself mentioning `Result<…>` or `panic!`.
fn strip_doc_lines(content: &str) -> String {
    content
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("///") || t.starts_with("//!"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// True iff the doc comment has a `# <name>` (or `## <name>`)
/// markdown heading. Match is case-sensitive; rustdoc convention is
/// title-case (`# Panics`, `# Errors`, `# Returns`).
fn doc_has_section(doc: &str, name: &str) -> bool {
    for line in doc.lines() {
        let t = line.trim_start_matches('/').trim_start();
        if let Some(rest) = t.strip_prefix('#') {
            let header = rest.trim_start_matches('#').trim();
            if header == name {
                return true;
            }
        }
    }
    false
}

/// True iff the function body contains a panic-shaped call. Catches
/// the common Rust panic vehicles. Conservative: better to miss a
/// finding (false negative) than spam (false positive).
fn body_has_panic(body: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            (?:
                \bpanic\s*!  |
                \.unwrap\s*\( |
                \.expect\s*\( |
                \bassert\s*! |
                \bassert_eq\s*! |
                \bassert_ne\s*! |
                \bdebug_assert\s*! |
                \bunreachable\s*! |
                \btodo\s*! |
                \bunimplemented\s*!
            )
            ",
        )
        .expect("body_has_panic regex")
    });
    re.is_match(body)
}

/// True iff the signature mentions `Result<` after the function
/// arrow. Catches `-> Result<…>`, `-> std::io::Result<…>`,
/// `-> impl Future<Output = Result<…>>`, etc. Whether the function
/// actually returns Result is the only question; finer details
/// (Result vs Option vs anyhow::Result) are not interesting at v1.
fn returns_result(body: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"->\s*[A-Za-z0-9_:]*Result\s*<").expect("returns_result regex")
    });
    re.is_match(body)
}

fn first_line_of_body(body: &str) -> String {
    body.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    #[test]
    fn detects_line_comment_markers() {
        let dir = fixture_dir();
        write(
            dir.path(),
            "src/main.rs",
            "fn foo() {\n    // TODO: handle the empty case\n    // FIXME(alex): off-by-one\n}\n",
        );
        let findings = scan_markers(dir.path());
        assert_eq!(findings.len(), 2);
        assert!(matches!(findings[0].kind, FindingKind::Marker(MarkerKind::Todo) | FindingKind::Marker(MarkerKind::Fixme)));
        assert!(findings.iter().any(|f| f.message.contains("handle the empty case")));
        assert!(findings.iter().any(|f| f.message.contains("off-by-one")));
    }

    #[test]
    fn detects_block_comment_markers() {
        let dir = fixture_dir();
        write(
            dir.path(),
            "lib.rs",
            "/* HACK: this is the wrong type */\nfn x() {}\n",
        );
        let findings = scan_markers(dir.path());
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].kind, FindingKind::Marker(MarkerKind::Hack)));
        assert_eq!(findings[0].severity, Severity::Likely);
    }

    #[test]
    fn detects_doc_comment_markers() {
        let dir = fixture_dir();
        write(
            dir.path(),
            "lib.rs",
            "/// XXX: panics on empty input\nfn parse(s: &str) {}\n",
        );
        let findings = scan_markers(dir.path());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
    }

    #[test]
    fn ignores_non_marker_words() {
        let dir = fixture_dir();
        write(
            dir.path(),
            "lib.rs",
            "let todo = 5;\nlet pseudo_xxx_var = 1;\n",
        );
        assert!(scan_markers(dir.path()).is_empty());
    }

    #[test]
    fn skips_target_directory() {
        let dir = fixture_dir();
        write(dir.path(), "target/debug/build/foo.rs", "// TODO real\n");
        write(dir.path(), "src/lib.rs", "// TODO actual\n");
        let findings = scan_markers(dir.path());
        assert_eq!(findings.len(), 1);
        assert!(findings[0].file.to_string_lossy().contains("src/lib.rs"));
    }

    #[test]
    fn output_is_deterministic() {
        let dir = fixture_dir();
        write(dir.path(), "a.rs", "// TODO a\n// FIXME b\n");
        write(dir.path(), "b.rs", "// HACK c\n");
        let one = scan_markers(dir.path());
        let two = scan_markers(dir.path());
        let one_json = serde_json::to_string(&one).unwrap();
        let two_json = serde_json::to_string(&two).unwrap();
        assert_eq!(one_json, two_json);
    }

    #[test]
    fn doc_section_detector() {
        assert!(doc_has_section("# Panics\n\nempty input", "Panics"));
        assert!(doc_has_section("Some text.\n# Errors\nMore.", "Errors"));
        assert!(doc_has_section("/// # Panics\n", "Panics"));
        assert!(!doc_has_section("# Returns\nfoo", "Panics"));
        assert!(!doc_has_section("Talks about panicking", "Panics"));
    }

    #[test]
    fn body_panic_detector() {
        assert!(body_has_panic("fn x() { panic!(\"oops\"); }"));
        assert!(body_has_panic("let v = x.unwrap();"));
        assert!(body_has_panic("foo.expect(\"missing\")"));
        assert!(body_has_panic("assert!(x > 0);"));
        assert!(body_has_panic("todo!();"));
        assert!(body_has_panic("unimplemented!()"));
        assert!(!body_has_panic("fn x() { Ok(5) }"));
        assert!(!body_has_panic("// panic in this comment doesn't count"));
    }

    #[test]
    fn returns_result_detector() {
        assert!(returns_result("fn f() -> Result<u32, Error> { Ok(0) }"));
        assert!(returns_result("fn f() -> std::io::Result<()> { ... }"));
        assert!(returns_result("fn f() -> Result<T> {"));
        assert!(!returns_result("fn f() -> u32 { 5 }"));
        assert!(!returns_result("fn f() { let r = Result::Ok(5); }"));
    }

    #[test]
    #[cfg(feature = "treesitter")]
    fn detects_panics_claim_without_panic_in_body() {
        let dir = fixture_dir();
        write(
            dir.path(),
            "lib.rs",
            "/// Compute the answer.\n\
             ///\n\
             /// # Panics\n\
             ///\n\
             /// Panics on empty input.\n\
             pub fn compute(s: &str) -> u32 {\n\
                 s.len() as u32\n\
             }\n",
        );
        let findings = scan_doc_drift(dir.path());
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].kind, FindingKind::DocDrift(DocDriftKind::SectionMismatch)));
        assert_eq!(findings[0].severity, Severity::Likely);
        assert!(findings[0].message.contains("Panics"));
    }

    #[test]
    #[cfg(feature = "treesitter")]
    fn no_finding_when_panics_claim_matches_body() {
        let dir = fixture_dir();
        write(
            dir.path(),
            "lib.rs",
            "/// Compute the answer.\n\
             ///\n\
             /// # Panics\n\
             ///\n\
             /// Panics on empty input.\n\
             pub fn compute(s: &str) -> u32 {\n\
                 if s.is_empty() { panic!(\"empty\"); }\n\
                 s.len() as u32\n\
             }\n",
        );
        let findings = scan_doc_drift(dir.path());
        assert_eq!(findings.len(), 0);
    }

    #[test]
    #[cfg(feature = "treesitter")]
    fn detects_errors_claim_with_non_result_return() {
        let dir = fixture_dir();
        write(
            dir.path(),
            "lib.rs",
            "/// Look up a value.\n\
             ///\n\
             /// # Errors\n\
             ///\n\
             /// Returns NotFound when missing.\n\
             pub fn lookup(key: &str) -> Option<u32> {\n\
                 None\n\
             }\n",
        );
        let findings = scan_doc_drift(dir.path());
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("Errors"));
    }

    #[test]
    fn truncates_long_snippets() {
        let dir = fixture_dir();
        let long_body = "x".repeat(500);
        write(
            dir.path(),
            "lib.rs",
            &format!("// TODO {long_body}\n"),
        );
        let findings = scan_markers(dir.path());
        assert_eq!(findings.len(), 1);
        // 160 cap + UTF-8 ellipsis (3 bytes) = 163 max
        assert!(findings[0].snippet.len() <= 163);
        assert!(findings[0].snippet.ends_with('…'));
    }
}
