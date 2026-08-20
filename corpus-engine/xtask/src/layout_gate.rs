// SPDX-License-Identifier: AGPL-3.0-or-later
//! layout-gate — a ratchet on hand-spelled knowledge of corpus-engine's
//! on-disk layout.
//!
//! # The leak, measured
//!
//! `scripts/nc-boundary.py` counts distinct TYPES crossing a domain edge. The
//! widest dependency the other two domains have on corpus-engine is not a type
//! — it is where corpus-engine puts its files, carried by string literals, and
//! no type-counting instrument can see any of it. Counted 2026-08-20, before
//! `corpus_engine::Corpus` existed:
//!
//! | convention | corpus-engine | sovereign | commonwealth |
//! |---|---|---|---|
//! | `"_corpus_meta.json"` | 62 | 63 | 22 |
//! | `format!("{id}-partition-{node}")` | 6 | 16 | 13 |
//!
//! One filename, a hundred and forty-seven deciders, no constant anywhere
//! (ARCH §10.6: one decider, one name). Renaming that file was a 147-site
//! change across three Cargo workspaces.
//!
//! # What this gate is for
//!
//! [`corpus_engine::Corpus`] is now the one decider — `root()`,
//! `partition(node)`, `partition_prefix()`, `meta_path()`, `meta_in(dir)`.
//! Without a ratchet the sites grow back: an agent extending a corpus feature
//! reaches for the literal because the literal is what every neighbouring line
//! already says. This freezes what is left so the only allowed direction is
//! down (the same shape as `arch-gate`), and it is the closure loop for the
//! migration rather than a hope that the next reader remembers.
//!
//! `corpus-engine/src/corpus.rs` is exempt: it is where the spelling lives.

use std::collections::BTreeMap;
use std::path::Path;

use crate::common;

/// The file that is ALLOWED to spell the layout, because it is the decider.
const DECIDER: &str = "corpus-engine/src/corpus.rs";

/// Repo-relative prefixes whose layout knowledge this gate governs. The gate
/// covers the whole source tree; this list exists only so the message can name
/// which domain a new site is in.
const DOMAINS: &[(&str, &str)] = &[
    ("corpus-engine", "corpus-engine"),
    ("sovereign/", "sovereign"),
    ("commonwealth/", "commonwealth"),
];

/// One hand-spelled layout fact.
fn layout_hits(line: &str) -> usize {
    let trimmed = line.trim_start();
    // Doc and line comments describe the layout; they do not depend on it.
    if trimmed.starts_with("//") {
        return 0;
    }
    let mut n = line.matches("\"_corpus_meta.json\"").count();
    // A partition directory name being BUILT — `{id}-partition-` or
    // `-partition-{node}`. Prose mentions of `<corpus>-partition-*` do not
    // interpolate, so they do not match.
    n += line.matches("}-partition-").count();
    n += line.matches("-partition-{").count();
    n
}

fn domain_of(rel: &str) -> &'static str {
    DOMAINS
        .iter()
        .find(|(prefix, _)| rel.starts_with(prefix))
        .map(|(_, name)| *name)
        .unwrap_or("workspace")
}

fn collect(dir: &Path, root: &Path, scope: &common::SourceTree, out: &mut Vec<(String, usize)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = common::rel_path(&path, root);
        if path.is_dir() {
            if !scope.excludes_dir(&rel) {
                collect(&path, root, scope, out);
            }
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") || rel == DECIDER {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let n: usize = text.lines().map(layout_hits).sum();
        if n > 0 {
            out.push((rel, n));
        }
    }
}

const WHAT: &str = "hand-spelled corpus-engine layout facts per file \
                    (`_corpus_meta.json`, `<id>-partition-<node>`) — \
                    `corpus_engine::Corpus` is the decider; frozen so debt can only shrink";

pub fn run(args: &[String]) -> i32 {
    let root = common::repo_root();
    let baseline_path = common::baselines_dir(&root).join("corpus_layout.txt");
    let flags = common::baseline_flags(args);

    let scope = match common::SourceTree::discover(&root) {
        Ok(s) => s,
        Err(e) => {
            // ARCH §18.3 — a gate that cannot resolve its own scope refuses
            // rather than rendering a verdict on the wrong tree.
            eprintln!("layout-gate: cannot resolve this repo's source tree: {e}");
            return 1;
        }
    };

    let mut sites: Vec<(String, usize)> = Vec::new();
    collect(&root, &root, &scope, &mut sites);
    sites.sort();
    let current: BTreeMap<String, usize> = sites.iter().cloned().collect();
    let total: usize = current.values().sum();

    if flags.update {
        if let Err(e) = common::write_count_map(&baseline_path, "layout-gate", WHAT, &current) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "wrote {} ({total} layout sites across {} files frozen)",
            baseline_path.display(),
            current.len()
        );
        return 0;
    }

    let baseline = common::load_count_map(&baseline_path);
    if baseline.is_empty() {
        eprintln!(
            "error: no baseline at {}.\n  Run: cargo run -p xtask -- layout-gate --update-baseline",
            baseline_path.display()
        );
        return 1;
    }

    if flags.tighten {
        let tightened: BTreeMap<String, usize> = baseline
            .iter()
            .filter_map(|(rel, &b)| current.get(rel).map(|&n| (rel.clone(), n.min(b))))
            .collect();
        if tightened == baseline {
            eprintln!(
                "layout-gate --tighten: baseline already tight ({} files)",
                baseline.len()
            );
            return 0;
        }
        let dropped = baseline.len() - tightened.len();
        let lowered = tightened
            .iter()
            .filter(|(k, &v)| baseline.get(*k).is_some_and(|&b| v < b))
            .count();
        if let Err(e) = common::write_count_map(&baseline_path, "layout-gate", WHAT, &tightened) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "layout-gate --tighten: {dropped} files cleared, {lowered} lowered → {}",
            baseline_path.display()
        );
        return 0;
    }

    let mut failures: Vec<String> = Vec::new();
    for (rel, n) in &sites {
        match baseline.get(rel) {
            None => failures.push(format!(
                "NEW layout site: {rel} ({n} in {}). Reach `corpus_engine::Corpus` \
                 instead — `Corpus::meta_in(dir)`, `corpus.partition(node)`, \
                 `corpus.partition_prefix()`, `corpus.root()`.",
                domain_of(rel)
            )),
            // No slack: unlike a file's line count, a literal does not drift
            // in by accident. One more is one more decider.
            Some(&b) if *n > b => failures.push(format!(
                "GREW: {rel} {b} → {n} hand-spelled layout facts. \
                 Reach `corpus_engine::Corpus` instead."
            )),
            _ => {}
        }
    }

    eprintln!(
        "layout-gate: {total} hand-spelled layout sites across {} files vs baseline \
         ({} files, {} sites) — {} failure(s)",
        current.len(),
        baseline.len(),
        baseline.values().sum::<usize>(),
        failures.len()
    );
    if failures.is_empty() {
        return 0;
    }
    for f in &failures {
        eprintln!("  FAIL {f}");
    }
    eprintln!("{}", common::fix_footer("layout-gate"));
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hand_joined_meta_filename_is_a_hit() {
        assert_eq!(
            layout_hits(r#"    let p = dir.join("_corpus_meta.json");"#),
            1
        );
    }

    #[test]
    fn a_built_partition_name_is_a_hit_but_prose_about_one_is_not() {
        assert_eq!(
            layout_hits(r#"    let d = format!("{corpus_id}-partition-{node}");"#),
            2,
            "both halves of the interpolation are layout knowledge"
        );
        assert_eq!(
            layout_hits(r#"    let prefix = format!("{corpus_id}-partition-");"#),
            1
        );
        assert_eq!(
            layout_hits("    /// Merge every <corpus>-partition-*/ dir on this node."),
            0,
            "a comment describing the layout does not depend on it"
        );
        assert_eq!(
            layout_hits("// let p = dir.join(\"_corpus_meta.json\");"),
            0,
            "commented-out code is not a call site"
        );
    }

    #[test]
    fn reaching_the_decider_is_not_a_hit() {
        assert_eq!(layout_hits("    let p = Corpus::meta_in(&dir);"), 0);
        assert_eq!(layout_hits("    let d = corpus.partition(&node_id);"), 0);
        assert_eq!(layout_hits("    let x = corpus.partition_prefix();"), 0);
    }

    #[test]
    fn the_domain_label_names_where_a_new_site_appeared() {
        assert_eq!(
            domain_of("sovereign/crates/sovereign-mesh/src/x.rs"),
            "sovereign"
        );
        assert_eq!(domain_of("commonwealth/crates/a/src/x.rs"), "commonwealth");
        assert_eq!(domain_of("corpus-engine/src/x.rs"), "corpus-engine");
        assert_eq!(domain_of("corpus-engine-scip/src/x.rs"), "corpus-engine");
        assert_eq!(domain_of("studio/crates/a/src/x.rs"), "workspace");
    }
}
