// SPDX-License-Identifier: AGPL-3.0-or-later
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
//! - **Smells** (absolute user paths, zero-tracing files) — structural
//!   anti-patterns detectable by pure text scanning. (Tier 2)
//!
//! All streams produce [`RoughEdgeFinding`]s into the same JSON
//! sidecar so the drift-report renderer can fold them into one
//! "Internal" section without caring which detector found them.
//!
//! A tier-1 doc-vs-signature drift scanner (`scan_doc_drift`) existed
//! while this module lived in `corpus-engine` — it needed that crate's
//! tree-sitter `CodeExtractor`, which the 2026-05-23 carve-out
//! deliberately left behind. The cfg-gated remnant could never compile
//! here (the crate declares no features and has no `extractors`
//! module) and was removed 2026-07-12. [`FindingKind::DocDrift`] stays
//! in the schema so existing sidecars and downstream renderers keep
//! deserializing.

// `rough_edges` is a pure-text scanner: walkdir + regex over source
// trees, no tree-sitter parsing. The whole crate compiles
// unconditionally — no features.

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
    Smell(SmellKind),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SmellKind {
    /// A `const _: &str = "/Users/..."` / `"/home/..."` / `"C:\\Users\\..."`
    /// (or any string literal beginning with one of those prefixes)
    /// appearing in a `src/**/*.rs` file outside tests/examples.
    /// Absolute developer-home paths break portability across machines.
    /// ARCH_PRINCIPLES §6.3 anti-pattern.
    AbsoluteUserPath,
    /// A `.rs` file >300 lines containing at least one `fn`/`impl`
    /// declaration but zero `tracing::` calls. Glassbox principle
    /// (§9.1): every non-obvious decision in production code should
    /// emit a tracing event. Pure-data files and trivial helpers are
    /// excluded by the size + `fn`/`impl` gate.
    ZeroTracing,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum MarkerKind {
    Todo,
    Fixme,
    Hack,
    Xxx,
}

/// Kinds of rustdoc-vs-signature drift. No scanner in this crate
/// produces these anymore (the tier-1 detector stayed behind in
/// corpus-engine at the carve-out and its remnant was removed); the
/// enum is kept for sidecar-JSON serde compatibility and downstream
/// renderers that match on it.
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
                    || name.starts_with(".") && name != ".cargo"))
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
                s.push('…');
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

/// Convenience: run tier 0 (markers) and tier 2 (smells: absolute
/// user paths, zero-tracing files) and return a single sorted vec.
/// The orchestrator and the standalone CLI both use this.
pub fn scan_all(source_root: &Path) -> Vec<RoughEdgeFinding> {
    let mut all = scan_markers(source_root);
    all.extend(scan_absolute_user_paths(source_root));
    all.extend(scan_zero_tracing(source_root));
    all.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });
    all
}

/// Match a string literal whose contents begin with an absolute path
/// rooted in a developer-home prefix: `/Users/`, `/home/`, `C:\Users\`.
/// Anchored to the *contents* of the literal, not the surrounding
/// code, so it catches both `const FOO: &str = "/Users/..."` and the
/// dynamic `format!("/Users/...{x}")` shape.
fn absolute_path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#""(/Users/|/home/|C:\\\\Users\\\\)"#).expect("absolute_path_re compile")
    })
}

/// Walk `source_root` and emit a finding for every absolute user
/// path literal found in `src/**/*.rs` files outside `tests/` and
/// `examples/` (test fixtures often need real paths; examples are
/// allowed to be developer-specific).
pub fn scan_absolute_user_paths(source_root: &Path) -> Vec<RoughEdgeFinding> {
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
                    || name.starts_with(".") && name != ".cargo"))
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
        if path_is_in_tests_or_examples(path) {
            continue;
        }
        let raw = match std::fs::read_to_string(path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if raw.len() > 1024 * 1024 {
            continue;
        }
        let re = absolute_path_re();
        for (i, line) in raw.lines().enumerate() {
            // Skip the rough_edges scanner's own regex literal (it
            // contains the prefix strings by construction).
            if line.contains("absolute_path_re") {
                continue;
            }
            if !re.is_match(line) {
                continue;
            }
            let snippet = line.trim().to_string();
            let snippet = if snippet.len() > 160 {
                let mut s = snippet[..160].to_string();
                s.push('…');
                s
            } else {
                snippet
            };
            out.push(RoughEdgeFinding {
                kind: FindingKind::Smell(SmellKind::AbsoluteUserPath),
                severity: Severity::Likely,
                file: path.to_path_buf(),
                line: (i as u32) + 1,
                symbol: None,
                message: "absolute developer-home path in source (breaks portability)".into(),
                snippet,
            });
        }
    }
    out.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.line.cmp(&b.line)));
    out
}

/// Walk `source_root` and emit a finding for every `.rs` file
/// >300 lines that contains a `fn`/`impl` declaration but no
/// > `tracing::` calls. Pure-data files (no `fn`/`impl`) are exempt.
/// > Test files (`tests/`, `examples/`, `#[cfg(test)]` modules) are
/// > exempt by convention — they don't need glassbox tracing.
pub fn scan_zero_tracing(source_root: &Path) -> Vec<RoughEdgeFinding> {
    const MIN_LINES: usize = 300;
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
                    || name.starts_with(".") && name != ".cargo"))
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
        if path_is_in_tests_or_examples(path) {
            continue;
        }
        let raw = match std::fs::read_to_string(path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let line_count = raw.lines().count();
        if line_count < MIN_LINES {
            continue;
        }
        // Stripping doc lines keeps a stray `// tracing::debug!` in
        // a comment from satisfying the gate.
        let body = strip_doc_lines(&raw);
        if !(body.contains("fn ") || body.contains("impl ")) {
            continue;
        }
        if body.contains("tracing::") {
            continue;
        }
        out.push(RoughEdgeFinding {
            kind: FindingKind::Smell(SmellKind::ZeroTracing),
            severity: Severity::Note,
            file: path.to_path_buf(),
            line: 1,
            symbol: None,
            message: format!(
                "{line_count}-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)"
            ),
            snippet: String::new(),
        });
    }
    out.sort_by(|a, b| a.file.cmp(&b.file));
    out
}

/// True iff the path contains a `tests/` or `examples/` directory
/// component. Both are conventionally allowed to be developer-specific
/// or skip glassbox tracing.
fn path_is_in_tests_or_examples(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s == "tests" || s == "examples"
    })
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
        assert!(matches!(
            findings[0].kind,
            FindingKind::Marker(MarkerKind::Todo) | FindingKind::Marker(MarkerKind::Fixme)
        ));
        assert!(findings
            .iter()
            .any(|f| f.message.contains("handle the empty case")));
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
        assert!(matches!(
            findings[0].kind,
            FindingKind::Marker(MarkerKind::Hack)
        ));
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
    fn truncates_long_snippets() {
        let dir = fixture_dir();
        let long_body = "x".repeat(500);
        write(dir.path(), "lib.rs", &format!("// TODO {long_body}\n"));
        let findings = scan_markers(dir.path());
        assert_eq!(findings.len(), 1);
        // 160 cap + UTF-8 ellipsis (3 bytes) = 163 max
        assert!(findings[0].snippet.len() <= 163);
        assert!(findings[0].snippet.ends_with('…'));
    }
}
