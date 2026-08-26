// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic working-tree scans: field-declaration census, mention counts,
//! and the hand-rolled arg-loop scan.
//!
//! Scope and line-shape rules deliberately match the bar instruments so the
//! gate and the bar cannot disagree about what a declaration is:
//!   - declaration regex + mention scope: `scripts/factory-scale.py`
//!   - arg-loop detection (>=2 bare long-flag match arms; a `#[derive(..Parser..)]`
//!     on a struct is `derived`; both in one file is `mixed`, not scored):
//!     `scripts/hpr-cost.py`

use regex::Regex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Path components that put a file out of scope — the same set
/// `scripts/factory-scale.py` uses for MENTIONS. Declaration scans use
/// [`EXCLUDE_DIRS_DECL`], which keeps tests/benches/examples: `--all-targets`
/// compiles them, so their declarations are real seed sites.
pub const EXCLUDE_DIRS_MENTIONS: &[&str] = &[
    "vendor",
    "node_modules",
    ".cargo-container",
    "research",
    "external",
    "target",
    ".claude",
    "tests",
    "benches",
    "examples",
    ".git",
];

pub const EXCLUDE_DIRS_DECL: &[&str] = &[
    "vendor",
    "node_modules",
    ".cargo-container",
    "research",
    "external",
    "target",
    ".claude",
    ".git",
];

/// Blank out comments, preserving line numbering and string literals.
///
/// One implementation, two callers, and it started as two. `arg_loop_scan` had
/// a private line-oriented version whose own doc conceded the gap — *"flag
/// literals live in match arms, not in strings that also contain `//`"* — a
/// true-enough assumption for flags that is false for provenance keys, where
/// `"http://…"` is ordinary data. Rather than mint a second stripper beside it
/// (ARCH §10.6, one decider one name), the lexer replaced it: strictly more
/// correct for the original caller, and correct for the new one.
///
/// The false positive that forced the rewrite: it reported
/// `detector.rs:737` as a production site, and that line is the detector's OWN
/// doc comment explaining the pattern. Prose about a defect is not the defect.
/// Any scanner that reads source as text has this bug, and the file most likely
/// to discuss a pattern is the file implementing its detector — so the false
/// positive lands on the instrument itself and reads as a real finding.
///
/// String literals are KEPT, deliberately: the keys these scans match on
/// (`"source"`, `"custody"`) are literals, so blanking them would blank the
/// signal. That makes the comment scan a real lexer rather than a regex — a
/// `//` inside `"http://…"` does not start a comment.
pub fn strip_comments(src: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum St {
        Code,
        Line,
        Block(u32),
        Str,
        RawStr(usize),
        Ch,
    }
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut st = St::Code;
    let mut i = 0usize;
    let blank = |out: &mut Vec<u8>, at: usize| {
        if out[at] != b'\n' {
            out[at] = b' ';
        }
    };
    while i < b.len() {
        match st {
            St::Code => {
                if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
                    st = St::Line;
                    blank(&mut out, i);
                    blank(&mut out, i + 1);
                    i += 2;
                    continue;
                }
                if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
                    st = St::Block(1);
                    blank(&mut out, i);
                    blank(&mut out, i + 1);
                    i += 2;
                    continue;
                }
                // `r"…"` / `r#"…"#` — the hash count closes it.
                if b[i] == b'r' {
                    let mut j = i + 1;
                    let mut hashes = 0usize;
                    while j < b.len() && b[j] == b'#' {
                        hashes += 1;
                        j += 1;
                    }
                    if j < b.len() && b[j] == b'"' {
                        st = St::RawStr(hashes);
                        i = j + 1;
                        continue;
                    }
                }
                if b[i] == b'"' {
                    st = St::Str;
                } else if b[i] == b'\'' {
                    st = St::Ch;
                }
                i += 1;
            }
            St::Line => {
                if b[i] == b'\n' {
                    st = St::Code;
                } else {
                    blank(&mut out, i);
                }
                i += 1;
            }
            St::Block(depth) => {
                if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
                    st = St::Block(depth + 1);
                    blank(&mut out, i);
                    blank(&mut out, i + 1);
                    i += 2;
                    continue;
                }
                if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                    blank(&mut out, i);
                    blank(&mut out, i + 1);
                    st = if depth == 1 { St::Code } else { St::Block(depth - 1) };
                    i += 2;
                    continue;
                }
                blank(&mut out, i);
                i += 1;
            }
            St::Str => {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if b[i] == b'"' {
                    st = St::Code;
                }
                i += 1;
            }
            St::RawStr(hashes) => {
                if b[i] == b'"' {
                    let closed = (1..=hashes).all(|k| b.get(i + k) == Some(&b'#'));
                    if closed {
                        st = St::Code;
                        i += hashes + 1;
                        continue;
                    }
                }
                i += 1;
            }
            St::Ch => {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if b[i] == b'\'' {
                    st = St::Code;
                }
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

/// Blank out `#[cfg(test)]` items, preserving line numbering.
///
/// [`EXCLUDE_DIRS_MENTIONS`] keeps test DIRECTORIES out; this keeps INLINE test
/// modules out, which is where most of this repo's test code actually lives. A
/// scanner that counts them reports fixtures as production sites — and for a
/// provenance scan that is not noise but an inversion: `index/evidence.rs`'s
/// test module is full of `metadata` maps carrying `"custody"` precisely
/// BECAUSE the production type has already converged off the untyped channel.
/// Counting those would report the converged case as the unconverged one.
///
/// Same scope rule `cargo xtask concept-gate` applies for the same reason: a
/// test helper is not a second home for a noun.
///
/// Every removed byte becomes a space and every newline is kept, so a match
/// offset in the result still maps to the right line in the original.
pub fn strip_test_scope(src: &str) -> String {
    const ATTR: &str = "#[cfg(test)]";
    let bytes = src.as_bytes();
    let mut out: Vec<u8> = bytes.to_vec();
    let mut search = 0usize;
    while let Some(rel) = src[search..].find(ATTR) {
        let at = search + rel;
        // Walk to the item's first `{`, then match braces to its close. An
        // item with no brace body (`#[cfg(test)] use ...;`) ends at the `;`.
        let mut i = at + ATTR.len();
        let mut depth = 0usize;
        let mut started = false;
        while i < bytes.len() {
            match bytes[i] {
                b'{' => {
                    depth += 1;
                    started = true;
                }
                b'}' => {
                    // A `}` before any `{` means this `#[cfg(test)]` was not
                    // introducing an item at all — it is text inside a comment
                    // or a string literal (this file contains one). Stop
                    // rather than underflow, and blank nothing.
                    if !started {
                        i = at + ATTR.len();
                        break;
                    }
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                b';' if !started => {
                    i += 1;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        for b in out.iter_mut().take(i).skip(at) {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        search = i;
    }
    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

pub fn repo_root() -> Result<PathBuf, String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("running git rev-parse: {e}"))?;
    if !out.status.success() {
        return Err("not inside a git repository".to_string());
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// Every `.rs` file under `root` outside the exclusion set, sorted for
/// deterministic output.
pub fn walk_rs_files(root: &Path, exclude: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if exclude.contains(&name) {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if name.ends_with(".rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// The declaration line shape `scripts/factory-scale.py` counts:
/// `[pub[(..)]] <field>: <ty> [,]` alone on its line.
pub fn field_decl_re(field: &str, ty: &str) -> Regex {
    Regex::new(&format!(
        r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?{}:\s*{}\s*,?\s*$",
        regex::escape(field),
        regex::escape(ty)
    ))
    .expect("static shape, escaped dynamic parts")
}

/// One file's seed sites: 1-based line numbers of matching declaration lines.
pub struct DeclSites {
    pub path: PathBuf,
    pub lines: Vec<usize>,
}

pub fn find_decl_sites(files: &[PathBuf], field: &str, ty: &str) -> Vec<DeclSites> {
    let re = field_decl_re(field, ty);
    let mut out = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let lines: Vec<usize> = text
            .lines()
            .enumerate()
            .filter(|(_, l)| re.is_match(l))
            .map(|(i, _)| i + 1)
            .collect();
        if !lines.is_empty() {
            out.push(DeclSites {
                path: path.clone(),
                lines,
            });
        }
    }
    out
}

/// The generic `<ident>: String` declaration regex, capture 1 = field name.
///
/// Exposed (rather than rebuilt inline) so the ledger's field-atom detector
/// can make ONE pass over the tree collecting counts and line numbers together,
/// instead of `string_field_census` followed by `find_decl_sites` per atom —
/// which re-reads every file once per atom. One decider for the decl rule
/// (ARCH §10.6): change it here and both callers move.
pub fn string_field_decl_re() -> Regex {
    Regex::new(r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?([a-z_][a-z0-9_]*):\s*String\s*,?\s*$")
        .expect("static regex")
}

/// `field -> declaration count` over every `<ident>: String` line. The
/// population behind the "stringly-typed field atoms" work-table row.
pub fn string_field_census(files: &[PathBuf]) -> BTreeMap<String, usize> {
    let re = string_field_decl_re();
    let mut census: BTreeMap<String, usize> = BTreeMap::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines() {
            if let Some(c) = re.captures(line) {
                *census.entry(c[1].to_string()).or_default() += 1;
            }
        }
    }
    census
}

/// Word-boundary mentions of `word` across in-scope files — the reach number,
/// same population as `factory-scale.py`'s `mentions`.
pub fn mention_count(files: &[PathBuf], word: &str) -> usize {
    let re = Regex::new(&format!(r"\b{}\b", regex::escape(word))).expect("escaped word");
    files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|text| re.find_iter(&text).count())
        .sum()
}

/// Flag-surface classification of one file, after comment stripping —
/// the `scripts/hpr-cost.py` detection rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagSurface {
    /// `#[derive(..Parser..)]` present, no hand-rolled loop.
    Derived,
    /// >= 2 match arms whose pattern is a bare long-flag literal.
    HandRolled,
    /// Both surfaces in one file — could-not-judge, never scored.
    Mixed,
    /// Neither — not a flag surface.
    None,
}

pub struct ArgLoopScan {
    /// (file, bare long-flag arm count) for every hand-rolled surface.
    pub hand_rolled: Vec<(PathBuf, usize)>,
    pub derived: usize,
    pub mixed: Vec<PathBuf>,
}



pub fn classify_flag_surface(text: &str) -> (FlagSurface, usize) {
    let stripped = strip_comments(text);
    let derive_re = Regex::new(r"#\[derive\([^)]*Parser[^)]*\)\]").expect("static");
    let arm_re =
        Regex::new(r#"^\s*"--[A-Za-z0-9-]+"(\s*\|\s*"-?-?[A-Za-z0-9-]+")*\s*=>"#).expect("static");
    let derived = derive_re.is_match(&stripped);
    let arms = stripped.lines().filter(|l| arm_re.is_match(l)).count();
    let hand = arms >= 2;
    let surface = match (derived, hand) {
        (true, true) => FlagSurface::Mixed,
        (true, false) => FlagSurface::Derived,
        (false, true) => FlagSurface::HandRolled,
        (false, false) => FlagSurface::None,
    };
    (surface, arms)
}

pub fn arg_loop_scan(files: &[PathBuf]) -> ArgLoopScan {
    let mut scan = ArgLoopScan {
        hand_rolled: Vec::new(),
        derived: 0,
        mixed: Vec::new(),
    };
    for path in files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        match classify_flag_surface(&text) {
            (FlagSurface::HandRolled, arms) => scan.hand_rolled.push((path.clone(), arms)),
            (FlagSurface::Derived, _) => scan.derived += 1,
            (FlagSurface::Mixed, _) => scan.mixed.push(path.clone()),
            (FlagSurface::None, _) => {}
        }
    }
    scan.hand_rolled
        .sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The false positive that produced this function: the provenance detector
    /// reported its own doc comment as a production site.
    #[test]
    fn prose_about_a_pattern_is_not_the_pattern() {
        let src = "/// a `metadata.get(\"source\")` downstream is the other half\n\
                   fn real() { metadata.get(\"source\"); }\n";
        let out = strip_comments(src);
        assert_eq!(out.matches("metadata.get").count(), 1, "only the code site survives");
        assert!(out.starts_with("   "), "the doc line is blanked, not deleted");
        assert_eq!(out.lines().count(), src.lines().count());
    }

    /// String literals are the SIGNAL for these scans, so they must survive —
    /// and a `//` inside one must not open a comment. That is the difference
    /// between a lexer and a regex, and it is why this is not a one-liner.
    #[test]
    fn a_slash_slash_inside_a_string_does_not_start_a_comment() {
        let src = "let u = \"http://example.com\"; metadata.get(\"source\");\n";
        let out = strip_comments(src);
        assert!(out.contains("metadata.get(\"source\")"), "got: {out}");
        assert!(out.contains("http://example.com"));
    }

    #[test]
    fn block_comments_nest_and_raw_strings_survive() {
        let src = "/* a /* b */ c */ keep(); let r = r#\"x // y\"#;\n";
        let out = strip_comments(src);
        assert!(out.contains("keep()"), "got: {out}");
        assert!(out.contains("x // y"), "raw string body must survive: {out}");
        assert!(!out.contains('b'), "nested block comment must be gone: {out}");
    }

    /// Found by running the detector over this workspace: a `#[cfg(test)]`
    /// appearing inside a comment or a string literal is not an item, and the
    /// brace walk ran off the end of the enclosing block subtracting from zero.
    /// THIS FILE contains such a literal, so the scanner crashed on itself.
    #[test]
    fn a_cfg_test_inside_text_does_not_run_the_brace_walk_off_the_end() {
        // The attribute here is inside a string, exactly as `strip_test_scope`
        // spells it, and the next brace is a CLOSE.
        let src = "fn f() {\n    let s = \"#[cfg(test)]\";\n}\nfn g() { keep(); }\n";
        let out = strip_test_scope(src);
        assert!(out.contains("keep()"), "nothing after the text may be blanked");
        assert_eq!(out.lines().count(), src.lines().count());
    }

    /// The ordinary case still works: a real test module goes, production stays,
    /// and every line survives so reported line numbers stay true.
    #[test]
    fn a_real_test_module_is_blanked_and_the_line_count_is_preserved() {
        let src = "fn prod() { a(); }\n#[cfg(test)]\nmod t {\n    fn x() { b(); }\n}\nfn after() { c(); }\n";
        let out = strip_test_scope(src);
        assert!(out.contains("a()") && out.contains("c()"));
        assert!(!out.contains("b()"), "the test body must be gone");
        assert_eq!(out.lines().count(), src.lines().count());
    }

    #[test]
    fn decl_regex_matches_the_factory_scale_shapes() {
        let re = field_decl_re("corpus_id", "String");
        for line in [
            "    corpus_id: String,",
            "    pub corpus_id: String,",
            "    pub(crate) corpus_id: String,",
            "        corpus_id: String",
        ] {
            assert!(re.is_match(line), "should match: {line}");
        }
        for line in [
            "    corpus_id: Option<String>,",
            "    corpus_id: &str,",
            "    let corpus_id: String = x;",
            "    other_corpus_id: String,",
            "    corpus_id: String, // trailing comment",
        ] {
            assert!(!re.is_match(line), "should NOT match: {line}");
        }
    }

    #[test]
    fn string_census_groups_by_field_name() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(
            &f,
            "struct A {\n    pub name: String,\n    corpus_id: String,\n    name: String,\n}\n",
        )
        .unwrap();
        let census = string_field_census(&[f]);
        assert_eq!(census.get("name"), Some(&2));
        assert_eq!(census.get("corpus_id"), Some(&1));
    }

    #[test]
    fn flag_surface_classification_matches_hpr_cost_rules() {
        let hand = r#"
fn parse(args: &[String]) {
    match a {
        "--alpha" => {}
        "--beta" | "-b" => {}
        other => {}
    }
}
"#;
        assert_eq!(classify_flag_surface(hand).0, FlagSurface::HandRolled);

        // One arm is not a loop.
        let one = r#"match a { "--alpha" => {}, _ => {} }"#;
        assert_eq!(classify_flag_surface(one).0, FlagSurface::None);

        let derived = "#[derive(clap::Parser)]\nstruct Args { x: u32 }\n";
        assert_eq!(classify_flag_surface(derived).0, FlagSurface::Derived);

        // A derive that only appears in a comment is NOT a converted file —
        // the exact false positive hpr-cost.py strips comments to kill.
        let commented = r#"
// mimics #[derive(clap::Parser)] one day
fn parse() {
    match a {
        "--alpha" => {}
        "--beta" => {}
        _ => {}
    }
}
"#;
        assert_eq!(classify_flag_surface(commented).0, FlagSurface::HandRolled);

        // Both real surfaces -> mixed, never scored.
        let mixed = format!("#[derive(clap::Parser)]\nstruct A {{ x: u32 }}\n{hand}");
        assert_eq!(classify_flag_surface(&mixed).0, FlagSurface::Mixed);
    }

    #[test]
    fn comment_stripping_handles_blocks() {
        // Comments are BLANKED, not deleted. The old line-oriented version
        // removed them, which shifted every column after a comment; a scanner
        // that maps a match offset back to a location then reports the wrong
        // one. Same rule `strip_test_scope` follows, for the same reason.
        let s = strip_comments("a /* x\ny */ b // tail\nc");
        assert_eq!(s, "a     \n     b        \nc");
        assert_eq!(s.len(), "a /* x\ny */ b // tail\nc".len(), "byte offsets must be stable");
    }
}
