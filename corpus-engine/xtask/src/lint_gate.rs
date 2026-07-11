// SPDX-License-Identifier: AGPL-3.0-or-later
//! lint-gate — a per-(crate, lint) count ratchet over `cargo clippy` JSON.
//!
//! The path from "clippy is advisory and ignored" to "-D warnings" without a
//! big-bang sweep: warnings are counted per (package, lint code), today's
//! counts are frozen in `quality/baselines/clippy_counts.tsv`, and the gate
//! fails only when a count GROWS — new debt is blocked while old debt burns
//! down crate by crate (banked weekly by `--tighten`).
//!
//! Input is the raw JSON stream from
//! `cargo clippy --workspace --all-targets --message-format=json`, passed
//! via `--from <file>` (the CI clippy job tees it). Diagnostics are deduped
//! on (file, line, column, code) — the same warning surfaces once per
//! compile target otherwise (lib, test, bin).

use std::collections::{BTreeMap, BTreeSet};

use crate::common;

pub fn run(args: &[String]) -> i32 {
    let root = common::repo_root();
    let flags = common::baseline_flags(args);
    let baseline_path = common::baselines_dir(&root).join("clippy_counts.tsv");

    let from = args
        .iter()
        .position(|a| a == "--from")
        .and_then(|i| args.get(i + 1));
    let Some(from) = from else {
        eprintln!(
            "error: lint-gate needs the clippy JSON stream.\n  \
             cargo clippy --workspace --all-targets --message-format=json > clippy.json\n  \
             cargo run -p xtask -- lint-gate --from clippy.json [--update-baseline|--tighten]"
        );
        return 1;
    };
    let json = match std::fs::read_to_string(from) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {from}: {e}");
            return 1;
        }
    };
    let current = count_lints(&json);
    let what = "clippy warning counts per crate/lint (grandfathered; may never grow)";

    if flags.update {
        if let Err(e) = common::write_count_map(&baseline_path, "lint-gate", what, &current) {
            eprintln!("error: {e}");
            return 1;
        }
        let total: usize = current.values().sum();
        eprintln!(
            "wrote {} ({} crate/lint pairs, {total} warnings frozen)",
            baseline_path.display(),
            current.len()
        );
        return 0;
    }

    let baseline = common::load_count_map(&baseline_path);
    if baseline.is_empty() && !baseline_path.exists() {
        eprintln!(
            "error: no baseline at {}.\n  Run: cargo run -p xtask -- lint-gate --from {from} --update-baseline",
            baseline_path.display()
        );
        return 1;
    }

    if flags.tighten {
        let tightened: BTreeMap<String, usize> = baseline
            .iter()
            .filter_map(|(k, &b)| {
                let now = current.get(k).copied().unwrap_or(0);
                (now > 0).then(|| (k.clone(), now.min(b)))
            })
            .collect();
        if tightened == baseline {
            eprintln!(
                "lint-gate --tighten: baseline already tight ({} entries)",
                baseline.len()
            );
            return 0;
        }
        let cleared = baseline.len() - tightened.len();
        let lowered = tightened
            .iter()
            .filter(|(k, &v)| baseline.get(*k).is_some_and(|&b| v < b))
            .count();
        if let Err(e) = common::write_count_map(&baseline_path, "lint-gate", what, &tightened) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "lint-gate --tighten: {cleared} pairs cleared, {lowered} lowered → {}",
            baseline_path.display()
        );
        return 0;
    }

    let mut failures: Vec<String> = Vec::new();
    for (key, &n) in &current {
        let cap = baseline.get(key).copied().unwrap_or(0);
        if n > cap {
            failures.push(format!(
                "{key}: {cap} → {n} (+{}) — fix the new warnings, or accept them \
                 explicitly by re-baselining",
                n - cap
            ));
        }
    }
    let improved = baseline
        .iter()
        .filter(|(k, &b)| current.get(*k).copied().unwrap_or(0) < b)
        .count();

    let total: usize = current.values().sum();
    eprintln!(
        "lint-gate: {total} warnings across {} crate/lint pairs vs baseline \
         ({improved} pairs improved — bank with --tighten)",
        current.len()
    );
    for f in &failures {
        eprintln!("  ✗ {f}");
    }
    if failures.is_empty() {
        eprintln!("  ✓ no crate/lint count grew");
        0
    } else {
        eprintln!();
        eprintln!("lint-gate FAILED ({} counts grew).", failures.len());
        eprintln!(
            "To accept the current state as intentional (and defend the diff in review):\n  \
             cargo run -p xtask -- lint-gate --from {from} --update-baseline"
        );
        1
    }
}

/// (package/lint) → deduped warning count from a cargo JSON stream.
fn count_lints(json_stream: &str) -> BTreeMap<String, usize> {
    let mut seen: BTreeSet<(String, u64, u64, String)> = BTreeSet::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in json_stream.lines() {
        let Some(diag) = parse_diag_line(line) else {
            continue;
        };
        if seen.insert((diag.file.clone(), diag.line, diag.column, diag.code.clone())) {
            *counts
                .entry(format!("{}/{}", diag.package, diag.code))
                .or_default() += 1;
        }
    }
    counts
}

struct Diag {
    package: String,
    code: String,
    file: String,
    line: u64,
    column: u64,
}

/// Parse one cargo JSON line: keep `compiler-message` entries whose TOP-LEVEL
/// diagnostic has a lint code and a warning/error level. A real JSON parse,
/// deliberately: a first-match string scan mis-attributed the level from
/// `help`/`note` CHILDREN (which precede the top-level `level` field on every
/// clippy lint that carries a suggestion) and silently dropped them all.
fn parse_diag_line(line: &str) -> Option<Diag> {
    // Cheap pre-filter before paying for a full parse of artifact lines.
    if !line.contains("compiler-message") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("reason")?.as_str()? != "compiler-message" {
        return None;
    }
    let msg = v.get("message")?;
    let level = msg.get("level")?.as_str()?;
    if level != "warning" && level != "error" {
        return None;
    }
    let code = msg.get("code")?.get("code")?.as_str()?;
    let spans = msg.get("spans")?.as_array()?;
    let primary = spans
        .iter()
        .find(|s| {
            s.get("is_primary")
                .and_then(|p| p.as_bool())
                .unwrap_or(false)
        })
        .or_else(|| spans.first())?;
    Some(Diag {
        package: package_name(v.get("package_id")?.as_str()?),
        code: code.to_string(),
        file: primary.get("file_name")?.as_str()?.to_string(),
        line: primary.get("line_start")?.as_u64()?,
        column: primary.get("column_start")?.as_u64()?,
    })
}

/// Package name from a cargo `package_id`, across both formats:
/// new `path+file:///…/dir#name@1.0.0` / `path+file:///…/name#1.0.0`,
/// old `name 1.0.0 (path+file:///…)`.
fn package_name(id: &str) -> String {
    if let Some((url, tail)) = id.split_once('#') {
        if let Some((name, _ver)) = tail.split_once('@') {
            return name.to_string();
        }
        // `…/name#1.0.0` — name is the last path segment.
        return url.rsplit('/').next().unwrap_or(url).to_string();
    }
    id.split_whitespace().next().unwrap_or(id).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag_line(pkg_id: &str, code: &str, file: &str, line: u64) -> String {
        format!(
            r#"{{"reason":"compiler-message","package_id":"{pkg_id}","message":{{"code":{{"code":"{code}","explanation":null}},"level":"warning","spans":[{{"file_name":"{file}","line_start":{line},"column_start":5}}]}}}}"#
        )
    }

    #[test]
    fn package_name_handles_all_id_formats() {
        assert_eq!(
            package_name("path+file:///repo/sovereign/crates/sovereign-core#0.1.19"),
            "sovereign-core"
        );
        assert_eq!(
            package_name("path+file:///repo/sovereign-desktop/src-tauri#sovereign-desktop@0.1.19"),
            "sovereign-desktop"
        );
        assert_eq!(
            package_name("sovereign-core 0.1.19 (path+file:///repo)"),
            "sovereign-core"
        );
    }

    #[test]
    fn counts_dedupe_across_targets_and_group_by_crate_lint() {
        let stream = [
            diag_line(
                "path+file:///r/a#0.1.0",
                "unused_imports",
                "a/src/lib.rs",
                5,
            ),
            // Same diagnostic again (second compile target) — deduped.
            diag_line(
                "path+file:///r/a#0.1.0",
                "unused_imports",
                "a/src/lib.rs",
                5,
            ),
            diag_line(
                "path+file:///r/a#0.1.0",
                "unused_imports",
                "a/src/other.rs",
                9,
            ),
            diag_line(
                "path+file:///r/b#0.1.0",
                "clippy::unwrap_used",
                "b/src/lib.rs",
                3,
            ),
            // Non-message lines are ignored.
            r#"{"reason":"compiler-artifact","package_id":"x"}"#.to_string(),
        ]
        .join("\n");
        let counts = count_lints(&stream);
        assert_eq!(counts.get("a/unused_imports"), Some(&2));
        assert_eq!(counts.get("b/clippy::unwrap_used"), Some(&1));
        assert_eq!(counts.len(), 2);
    }

    #[test]
    fn non_warning_levels_are_skipped() {
        let note = r#"{"reason":"compiler-message","package_id":"path+file:///r/a#0.1.0","message":{"code":{"code":"x"},"level":"note","spans":[{"file_name":"f","line_start":1,"column_start":1}]}}"#;
        assert!(count_lints(note).is_empty());
    }
}
