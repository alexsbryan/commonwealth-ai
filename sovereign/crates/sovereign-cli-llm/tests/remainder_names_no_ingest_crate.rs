// SPDX-License-Identifier: AGPL-3.0-or-later
//! pb-cli-llm-ingest-move-remainder's census: cli-llm's svrn remainder names
//! no ingest crate. Every module that stays here (phase-b-70's placement
//! rule: it opens svrn's store, calls svrn's routes, or uses svrn's tools or
//! atlas views) reaches ingest through the composed ports or names a leaf's
//! type. The modules pb-cli-llm-ingest-move carries into ingest's CLI are
//! listed below and skipped; everything else under `src/` is the remainder.
//!
//! The ingest crates are read from `quality/ARCH_LAYERS.toml`'s `ingest`
//! package, the one list boundary-gate reads, so a crate joining ingest joins
//! this census too.
//!
//! Watched to fail: re-add `use corpus_engine::IngestAtlas;` to a remainder
//! module and this goes red naming the file and line.

use std::path::{Path, PathBuf};

/// The modules pb-cli-llm-ingest-move moves (FIVE_PROGRAMS §11's ingest
/// list, seat ruling (2) for bench_atlas), as paths under `src/`.
const MOVING: &[&str] = &[
    "enrich_cmd/",
    "corpus_cmd/",
    "corpus_scrub_cmd.rs",
    "corpus_snapshot_cmd.rs",
    "atlas_cmd/",
    "meta_atlas_cmd.rs",
    "pipeline_cmd.rs",
    "recipe_cmd.rs",
    "recipe_cmd/",
    "alignment_cmd.rs",
    "bench_atlas.rs",
];

/// Files inside a moving directory that the placement rule keeps svrn-side
/// (the row's list: raptor, raptor-index and the census raptor reads;
/// atlas status, inspect, budget and typed-extension).
const STAYING: &[&str] = &[
    "enrich_cmd/raptor.rs",
    "enrich_cmd/raptor_index.rs",
    "enrich_cmd/raptor_census.rs",
    "atlas_cmd/status.rs",
    "atlas_cmd/inspect.rs",
    "atlas_cmd/budget.rs",
    "atlas_cmd/typed_extension.rs",
];

fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .find(|d| d.join("quality/ARCH_LAYERS.toml").is_file())
        .unwrap_or_else(|| panic!("no quality/ARCH_LAYERS.toml above {}", manifest.display()))
        .to_path_buf()
}

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

fn is_remainder(rel: &str) -> bool {
    STAYING.contains(&rel)
        || !MOVING
            .iter()
            .any(|m| rel == *m || (m.ends_with('/') && rel.starts_with(m)))
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

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
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
        if !is_remainder(&rel) {
            continue;
        }
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
