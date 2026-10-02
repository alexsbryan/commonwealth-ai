// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon acts on ingest only through its ports (pb-ingest-dial-daemon-ports).
//!
//! Since pb-ingest-dial-daemon its production source names `corpus_engine`
//! nowhere: the engine and its implementors are built by ingest's face in the
//! stock binary (`process::HostedIngest`), and every site reaches ingest
//! through `IngestPort`, `AtlasPort` or `RecipeHarnessPort`. The crate links
//! no corpus-engine, which boundary-gate holds; this census holds the text,
//! so a re-added link cannot bring an engine call back with it (ARCH
//! principle 10).
//!
//! Test code is exempt: files named `tests.rs` / `*_tests.rs` are not read, and
//! each `#[cfg(test)]` item inside a production file is skipped to its closing
//! brace, after which the scan resumes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Files that may name the engine anywhere: they construct it. Empty since
/// pb-ingest-dial-daemon moved the construction to ingest's face.
const CONSTRUCTION: &[(&str, &str)] = &[];

/// Files that name the engine for a reason another row owns.
/// Empty since pb-ingest-dial-daemon: the recipe parse went to
/// `IngestPort::validate_recipe_toml`.
const OWNED_ELSEWHERE: &[(&str, &str)] = &[];

/// The engine crate's path and import, spelled apart so a grep of this
/// crate's tests for the engine counts real uses only.
const ENGINE_PATH: &str = concat!("corpus", "_engine::");
const ENGINE_USE: &str = concat!("use corpus", "_engine");

/// `.rs` files under `dir` whose own name does not say they are tests.
fn production_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if name != "tests" {
                production_sources(&path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
            out.push(path);
        }
    }
}

/// `{` minus `}` on one line, outside string literals and a trailing comment.
fn brace_delta(line: &str) -> i64 {
    let (mut delta, mut in_str, mut escaped) = (0, false, false);
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'/' if bytes.get(i + 1) == Some(&b'/') => break,
            b'\'' if bytes.get(i + 2) == Some(&b'\'') => {}
            b'{' if i == 0 || bytes[i - 1] != b'\'' => delta += 1,
            b'}' if i == 0 || bytes[i - 1] != b'\'' => delta -= 1,
            _ => {}
        }
    }
    delta
}

/// The 1-based lines of `text` outside comments and `#[cfg(test)]` items.
fn production_lines(text: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            // Skip the attributes, then the item: one line if it ends before
            // opening a block, else to the brace that closes it.
            i += 1;
            while i < lines.len() && lines[i].trim_start().starts_with("#[") {
                i += 1;
            }
            let (mut depth, mut opened) = (0, false);
            while i < lines.len() {
                depth += brace_delta(lines[i]);
                opened |= depth > 0;
                i += 1;
                if depth <= 0 && (opened || lines[i - 1].trim_end().ends_with(';')) {
                    break;
                }
            }
            continue;
        }
        if !trimmed.starts_with("//") {
            out.push((i + 1, lines[i]));
        }
        i += 1;
    }
    out
}

/// `code` names the engine crate: `corpus_engine::` or `use corpus_engine`,
/// not a `corpus_engine_*` leaf and not an identifier ending in it.
fn names_engine(code: &str) -> bool {
    let code = code.split("//").next().unwrap_or("");
    let at_boundary = |i: usize| {
        i == 0 || {
            let c = code.as_bytes()[i - 1];
            !(c.is_ascii_alphanumeric() || c == b'_')
        }
    };
    code.match_indices(ENGINE_PATH).any(|(i, _)| at_boundary(i))
        || code
            .match_indices(ENGINE_USE)
            .any(|(i, m)| at_boundary(i) && !code[i + m.len()..].starts_with('_'))
}

fn listed(list: &[(&str, &str)], rel: &str) -> bool {
    list.iter().any(|(f, _)| *f == rel)
}

/// THE failing input: any executing site that names the engine again. The
/// plant that proved it, reverted: reading_http.rs's status route calling
/// `corpus_engine::engine::status::scan_corpus_rows` in place of
/// `IngestPort::corpus_status_rows`.
#[test]
fn the_daemon_names_the_engine_only_where_it_builds_it() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    production_sources(&src, &mut files);
    assert!(
        files.len() > 100,
        "the walk found only {} files under {} — it is not scanning the tree \
         it claims to, which would make this census pass vacuously",
        files.len(),
        src.display()
    );

    let mut offenders = Vec::new();
    let mut named = BTreeSet::new();
    for path in files {
        let rel = path
            .strip_prefix(&src)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&path).expect("read a daemon source file");
        for (n, line) in production_lines(&text) {
            if !names_engine(line) {
                continue;
            }
            named.insert(rel.clone());
            let allowed = listed(CONSTRUCTION, &rel) || listed(OWNED_ELSEWHERE, &rel);
            if !allowed {
                offenders.push(format!("src/{rel}:{n}: {}", line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these daemon sites name corpus-engine outside its construction; reach \
         ingest through IngestPort / AtlasPort / RecipeHarnessPort instead:\n{}",
        offenders.join("\n")
    );
    for (file, why) in CONSTRUCTION.iter().chain(OWNED_ELSEWHERE) {
        assert!(
            named.contains(*file),
            "src/{file} no longer names corpus_engine; take it off the census \
             list ({why})"
        );
    }
}

/// The shim's spellings, apart so this file does not count itself.
const RECIPE_AUTHOR_SHIM: &[&str] = &[
    concat!("sovereign_tools::", "recipe_author"),
    concat!("bundles::", "RecipeAuthoringTools"),
];

/// The daemon reaches the recipe project only through ingest's port
/// (pb-ingest-rehome-daemon): no source or test of this crate names the
/// recipe-author shim sovereign-tools re-exports, so the parent row can drop
/// it. THE failing input: a route importing
/// `sovereign_tools::recipe_author::recipe_project_store::RecipeProjectStore`.
#[test]
fn the_daemon_reaches_the_recipe_project_only_through_ingests_port() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    production_sources(&root.join("src"), &mut files);
    let before = files.len();
    all_sources(&root.join("tests"), &mut files);
    assert!(
        before > 100 && files.len() > before + 50,
        "the walk found {before} source and {} test files — it is not scanning \
         the tree it claims to",
        files.len() - before
    );
    let mut offenders = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("read a daemon file");
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if RECIPE_AUTHOR_SHIM
                .iter()
                .any(|needle| code.contains(needle))
            {
                offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these daemon sites name the recipe-author shim; reach the recipe \
         project through `RecipeProjectPort` (EmbeddedDaemon::features_store) \
         and the bundle through `IngestCalls::recipe_authoring`:\n{}",
        offenders.join("\n")
    );
}

/// Every `.rs` file under `dir`.
fn all_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            all_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The skip resumes after a `#[cfg(test)]` item: an engine call placed below
/// a test module is still production code, and still counted.
#[test]
fn a_cfg_test_item_is_skipped_and_the_scan_resumes_after_it() {
    let text = "use {E}::A;\n\
                #[cfg(test)]\n\
                mod tests {\n    fn f() { let s = \"}\"; {E}::B; }\n}\n\
                // {E}::C in a comment\n\
                fn g() { {E}::D; }\n\
                #[cfg(test)]\n\
                mod more;\n\
                fn h() { {E}_yield::E; }\n"
        .replace("{E}", concat!("corpus", "_engine"));
    let text = text.as_str();
    let hits: Vec<usize> = production_lines(text)
        .into_iter()
        .filter(|(_, l)| names_engine(l))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(hits, vec![1, 7]);
}
