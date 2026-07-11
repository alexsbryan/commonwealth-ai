// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared gate plumbing: repo-root resolution + the uniform baseline
//! (ratchet) file I/O every count-based gate uses.
//!
//! Baseline contract: files live under `quality/baselines/`, are written ONLY
//! by `--update-baseline` (snapshot current state) or `--tighten` (rewrite
//! only improved entries; never add, never raise), and carry a header naming
//! the exact regeneration command so a red X is always self-serviceable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Repo root = grandparent of `corpus-engine/xtask/`.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("xtask manifest has no grandparent")
        .to_path_buf()
}

/// Directory all machine-written ratchet baselines live in.
pub fn baselines_dir(root: &Path) -> PathBuf {
    root.join("quality/baselines")
}

/// Parse a `<count>\t<key>` baseline (blank lines + `#` comments skipped).
pub fn load_count_map(path: &Path) -> BTreeMap<String, usize> {
    let mut map = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((count, key)) = line.split_once('\t') {
                if let Ok(n) = count.trim().parse::<usize>() {
                    map.insert(key.trim().to_string(), n);
                }
            }
        }
    }
    map
}

/// Write a `<count>\t<key>` baseline, sorted, with the standard header.
pub fn write_count_map(
    path: &Path,
    gate: &str,
    what: &str,
    map: &BTreeMap<String, usize>,
) -> Result<(), String> {
    let mut body = header(gate, what);
    for (key, n) in map {
        body.push_str(&format!("{n}\t{key}\n"));
    }
    write_baseline(path, &body)
}

/// Parse a one-key-per-line baseline (blank lines + `#` comments skipped).
pub fn load_line_set(path: &Path) -> std::collections::BTreeSet<String> {
    std::fs::read_to_string(path)
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Write a one-key-per-line baseline, sorted, with the standard header.
pub fn write_line_set(
    path: &Path,
    gate: &str,
    what: &str,
    set: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    let mut body = header(gate, what);
    for key in set {
        body.push_str(key);
        body.push('\n');
    }
    write_baseline(path, &body)
}

fn header(gate: &str, what: &str) -> String {
    format!(
        "# {gate} baseline — {what}.\n\
         # MACHINE-WRITTEN. Regenerate (snapshot current state — defend the diff in review):\n\
         #   cargo run -p xtask -- {gate} --update-baseline\n\
         # Bank improvements only (never adds, never raises — safe to run anytime):\n\
         #   cargo run -p xtask -- {gate} --tighten\n"
    )
}

fn write_baseline(path: &Path, body: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The uniform failure footer: the exact command that makes the red X
/// self-serviceable.
pub fn fix_footer(gate: &str) -> String {
    format!(
        "To accept the current state as intentional (and defend the diff in review):\n  \
         cargo run -p xtask -- {gate} --update-baseline"
    )
}

/// Shared `--update-baseline` / `--tighten` flag parsing.
pub struct BaselineFlags {
    pub update: bool,
    pub tighten: bool,
}

pub fn baseline_flags(args: &[String]) -> BaselineFlags {
    BaselineFlags {
        update: args.iter().any(|a| a == "--update-baseline"),
        tighten: args.iter().any(|a| a == "--tighten"),
    }
}
