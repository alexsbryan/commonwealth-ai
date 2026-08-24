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

/// Strip `//` line comments and `/* .. */` block comments. Line-oriented and
/// deliberately simple: flag literals live in match arms, not in strings that
/// also contain `//`, and the false positive this exists to kill is the
/// derive-in-a-comment (`hpr-cost.py` found two in `vault_report.rs`).
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_block = false;
    for line in text.lines() {
        let mut rest = line;
        if in_block {
            match rest.find("*/") {
                Some(i) => {
                    rest = &rest[i + 2..];
                    in_block = false;
                }
                None => {
                    out.push('\n');
                    continue;
                }
            }
        }
        let mut kept = String::new();
        loop {
            let line_c = rest.find("//");
            let block_c = rest.find("/*");
            match (line_c, block_c) {
                (Some(l), None) => {
                    kept.push_str(&rest[..l]);
                    rest = "";
                }
                (Some(l), Some(b)) if l < b => {
                    kept.push_str(&rest[..l]);
                    rest = "";
                }
                (_, Some(b)) => {
                    kept.push_str(&rest[..b]);
                    match rest[b + 2..].find("*/") {
                        Some(e) => rest = &rest[b + 2 + e + 2..],
                        None => {
                            in_block = true;
                            rest = "";
                        }
                    }
                }
                (None, None) => {
                    kept.push_str(rest);
                    rest = "";
                }
            }
            if rest.is_empty() {
                break;
            }
        }
        out.push_str(&kept);
        out.push('\n');
    }
    out
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
        let s = strip_comments("a /* x\ny */ b // tail\nc");
        assert_eq!(s, "a \n b \nc\n");
    }
}
