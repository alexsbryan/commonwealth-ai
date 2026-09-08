// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source-file discovery + line-numbered rendering.
//!
//! Used by every phase's prompt builder: the model gets the
//! candidate source verbatim with `N: ` prefixes so it can reference
//! line numbers in its `patch_lines` / `insert_before` actions.

use std::path::{Path, PathBuf};

pub fn discover_source_file(workdir: &Path) -> Option<String> {
    discover_source_files(workdir).into_iter().next()
}

/// All source files under the workdir (same walk/skip rules as
/// [`discover_source_file`]), shallowest-first then lexicographic.
/// Multi-file problems (packages with several modules) need every
/// file visible and addressable — a single discovered file makes
/// edits against the others structurally impossible (5.1-minilang
/// B-arm receipts, 2026-07-06: every `rewrite evaluate_ast` died
/// with "not found in minilang/__init__.py" while evaluator.py sat
/// unrendered).
pub fn discover_source_files(workdir: &Path) -> Vec<String> {
    let exts = [".py", ".rs", ".ts", ".tsx", ".go"];
    let mut hits: Vec<PathBuf> = Vec::new();
    walk_for_sources(workdir, workdir, 0, &exts, &mut hits);
    // Tool configs are infrastructure, not source. They sort FIRST
    // (fewest path components), so without this filter a webapp's
    // `playwright.config.ts` becomes the file the prompt points the
    // model at — and candidates "fix" the test runner instead of the
    // app (live receipts, job 09777dfe, 2026-07-07).
    hits.retain(|p| {
        p.file_name()
            .map(|n| !n.to_string_lossy().contains(".config."))
            .unwrap_or(true)
    });
    hits.sort_by_key(|p| (p.components().count(), p.clone()));
    hits.into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

fn walk_for_sources(root: &Path, dir: &Path, depth: usize, exts: &[&str], out: &mut Vec<PathBuf>) {
    if depth > 3 {
        return;
    }
    const SKIP: &[&str] = &[
        "target",
        "node_modules",
        ".git",
        "__pycache__",
        ".pytest_cache",
        "tests",
        "test",
        "dist",
        "build",
        "vendor",
    ];
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if SKIP.iter().any(|s| *s == name_str) {
            continue;
        }
        let p = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            walk_for_sources(root, &p, depth + 1, exts, out);
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let n = name_str.as_ref();
        if n.starts_with("test_") {
            continue;
        }
        if exts.iter().any(|ext| n.ends_with(ext)) {
            if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_path_buf());
            }
        }
    }
}

pub fn render_with_line_numbers(path: &Path) -> String {
    let Ok(content) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    let width = lines.len().to_string().len().max(1);
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>w$}: {l}", i + 1, w = width))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_pads_to_widest_index() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.py");
        std::fs::write(&path, "a\nb\nc\n").unwrap();
        let out = render_with_line_numbers(&path);
        assert!(out.starts_with("1: a"));
        assert!(out.contains("\n2: b"));
        assert!(out.contains("\n3: c"));
    }

    #[test]
    fn discover_finds_python_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("evaluator.py"), "pass\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
        std::fs::write(tmp.path().join("tests/test_x.py"), "pass\n").unwrap();
        let f = discover_source_file(tmp.path()).unwrap();
        assert_eq!(f, "evaluator.py");
    }

    #[test]
    fn discover_skips_test_files_at_root() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("test_x.py"), "pass\n").unwrap();
        std::fs::write(tmp.path().join("config_applier.py"), "pass\n").unwrap();
        let f = discover_source_file(tmp.path()).unwrap();
        assert_eq!(f, "config_applier.py");
    }

    #[test]
    fn discover_finds_rust_src_lib() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "pub fn x() {}\n").unwrap();
        let f = discover_source_file(tmp.path()).unwrap();
        assert_eq!(f, "src/lib.rs");
    }
}

#[cfg(test)]
mod multi_file_tests {
    use super::*;

    #[test]
    fn discover_source_files_returns_all_shallowest_first() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("minilang")).unwrap();
        std::fs::write(tmp.path().join("minilang/__init__.py"), "x = 1\n").unwrap();
        std::fs::write(
            tmp.path().join("minilang/evaluator.py"),
            "def evaluate_ast():\n    pass\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("minilang/tokenizer.py"),
            "def tokenize():\n    pass\n",
        )
        .unwrap();
        let files = discover_source_files(tmp.path());
        assert_eq!(files.len(), 3);
        assert!(files.contains(&"minilang/evaluator.py".to_string()));
    }

    #[test]
    fn tool_configs_are_not_source_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("playwright.config.ts"), "export default {}").unwrap();
        std::fs::write(tmp.path().join("vite.config.ts"), "export default {}").unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.ts"), "export {}\n").unwrap();
        let files = discover_source_files(tmp.path());
        assert_eq!(files, vec!["src/main.ts".to_string()]);
    }
}
