// SPDX-License-Identifier: AGPL-3.0-or-later
//! pb-cli-llm-ingest-move-remainder's census: cli-llm's svrn remainder names
//! no ingest crate. Every module here (phase-b-70's placement rule: it opens
//! svrn's store, calls svrn's routes, or uses svrn's tools or atlas views)
//! reaches ingest through the composed ports or names a leaf's type. Since
//! pb-cli-llm-ingest-move carried ingest's verbs into `svrn-ingest`, every
//! file under `src/` is the remainder, so the census scans them all.
//!
//! The ingest crates are read from `quality/ARCH_LAYERS.toml`'s `ingest`
//! package, the one list boundary-gate reads, so a crate joining ingest joins
//! this census too.
//!
//! Watched to fail: re-add `use corpus_engine::IngestAtlas;` to a remainder
//! module and this goes red naming the file and line.

use std::path::{Path, PathBuf};

#[path = "shared/repo_root.rs"]
mod repo_root;
use repo_root::repo_root;

/// The ingest package's crates, as Rust identifiers.
fn ingest_crates(root: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(root.join("quality/ARCH_LAYERS.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&text).unwrap();
    let ingest = doc["package"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"].as_str() == Some("ingest"))
        .expect("ARCH_LAYERS.toml declares the ingest package");
    ingest["crates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().replace('-', "_"))
        .collect()
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_svrn_remainder_names_no_ingest_crate() {
    let root = repo_root();
    let crates = ingest_crates(&root);
    assert!(crates.contains(&"corpus_engine".to_string()), "{crates:?}");
    // A crate path, `use` of it, or an `extern crate`/`as` alias: the
    // identifier not preceded by a field access or a longer identifier.
    let pattern = format!(
        r"(^|[^A-Za-z0-9_.])({})(\s*::|\s+as\b|\s*;|\s*\{{)",
        crates.join("|")
    );
    let needle = regex::Regex::new(&pattern).unwrap();

    let src = root.join("sovereign/crates/sovereign-cli-llm/src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    files.sort();
    let mut scanned = 0usize;
    let mut hits = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        scanned += 1;
        for (n, line) in std::fs::read_to_string(path).unwrap().lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if needle.is_match(line) {
                hits.push(format!("src/{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        scanned > 50,
        "the census scanned only {scanned} remainder files"
    );
    assert!(
        hits.is_empty(),
        "cli-llm's svrn remainder names an ingest crate ({}); reach ingest through \
         chat_cmd::ingest's composed ports or name the leaf's type \
         (pb-cli-llm-ingest-move-remainder):\n{}",
        crates.join(", "),
        hits.join("\n")
    );
}
