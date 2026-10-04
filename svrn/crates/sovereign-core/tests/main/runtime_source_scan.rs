// SPDX-License-Identifier: AGPL-3.0-or-later
//! ONE scanner for every "nothing under `src/runtime/` reaches through `&self`
//! into a `Runtime` member" census (ARCH §10.6 — one implementation per
//! decider). Two censuses ride it today: [`lane_reach_through_census`] for the
//! seven enrichment providers, and [`turn_capability_census`] for the two
//! per-turn capabilities.
//!
//! The subtleties live here so a third census does not have to rediscover
//! them:
//!
//! * **rustfmt splits the chain.** `self` on one line and `.routing_events` on
//!   the next is the SAME defect, and a naive line scan misses it. Every line
//!   is joined to the one after it with its leading whitespace stripped.
//! * **The join double-counts.** A hit lying wholly on line `i+1` is
//!   visible from line `i`'s joined text too; it is reported once, at the
//!   line the `self` is on.
//! * **A prefix is not a match.** `self.meta_atlas_hits` is not
//!   `self.meta_atlas`, and `self.approval_desk` is not `self.approval`.
//! * **Prose about the old shape is not the old shape** — `//` lines are
//!   skipped, so a doc comment naming the banned member does not go red.
//! * **A scan that read nothing reports zero**, which is the answer these
//!   files exist to publish. [`corpus_size`] is the size term that makes a
//!   vacuous pass impossible; [`assert_corpus_is_whole`] is the assertion.

use std::path::{Path, PathBuf};

/// A banned read, as `(repo-relative path, line number, the joined line)`.
pub(crate) type Hit = (String, usize, String);

pub(crate) fn runtime_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read runtime dir") {
        let p = entry.expect("dir entry").path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

fn sources() -> Vec<PathBuf> {
    // `src/runtime/` only — the STAGES. `src/runtime.rs` is the Runtime's own
    // definition and its `with_*` / `install_*` builders; a composition root
    // writing its own field is construction, not a reach-through.
    let mut files = Vec::new();
    rust_files(&runtime_dir(), &mut files);
    files
}

/// Files scanned and bytes read, for the size term.
pub(crate) fn corpus_size() -> (usize, u64) {
    let files = sources();
    let bytes = files
        .iter()
        .filter_map(|f| std::fs::metadata(f).ok())
        .map(|m| m.len())
        .sum();
    (files.len(), bytes)
}

/// ARCH §18.4 — validate the instrument before the result. A `#[cfg(test)]`
/// cut, a moved directory, or a scanner pointed at the wrong root all report
/// the same clean zero a real green does. The floors are ~75% of the tree as
/// measured 2026-09-10 (93 files, 3.03 MB): loose enough that a split or a
/// deletion campaign does not trip them, tight enough that "read nothing"
/// cannot pass.
pub(crate) fn assert_corpus_is_whole() {
    let (files, bytes) = corpus_size();
    assert!(
        files >= 70 && bytes >= 2_000_000,
        "instrument check failed: the census scanned {files} file(s) / {bytes} \
         byte(s) under {}, which is not the runtime tree. Every census riding \
         this scanner would report a clean zero for the wrong reason — fix the \
         scan before trusting any verdict below it.",
        runtime_dir().display()
    );
}

/// Every `self.<field>` read under `src/runtime/`, for each field named, from
/// every file whose basename is not in `exempt`.
pub(crate) fn scan(fields: &[&str], exempt: &[&str]) -> Vec<Hit> {
    let mut hits = Vec::new();
    for f in sources() {
        let name = f
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if exempt.contains(&name.as_str()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let raw: Vec<&str> = text.lines().collect();
        let joined: Vec<String> = raw
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let next = raw.get(i + 1).map(|n| n.trim_start()).unwrap_or("");
                format!("{l}{next}")
            })
            .collect();
        for (i, line) in joined.iter().enumerate() {
            if raw[i].trim_start().starts_with("//") {
                continue;
            }
            for field in fields {
                let needle = format!("self.{field}");
                if let Some(at) = line.find(&needle) {
                    let after = line[at + needle.len()..].chars().next();
                    if after.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                        continue;
                    }
                    // The join makes every hit visible from the line BEFORE it
                    // as well. Report it once, at the line the `self` is
                    // actually on — a match that starts past the end of the
                    // raw line belongs to the next line's own scan.
                    if at >= raw[i].len() {
                        continue;
                    }
                    hits.push((
                        f.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                            .unwrap_or(&f)
                            .display()
                            .to_string(),
                        i + 1,
                        line.trim().to_string(),
                    ));
                }
            }
        }
    }
    hits
}

/// `file:line  text`, one per line — what a failing census prints so the fix
/// needs no second run to locate.
pub(crate) fn render(hits: &[Hit]) -> String {
    hits.iter()
        .map(|(f, l, t)| format!("  {f}:{l}  {t}"))
        .collect::<Vec<_>>()
        .join("\n")
}
