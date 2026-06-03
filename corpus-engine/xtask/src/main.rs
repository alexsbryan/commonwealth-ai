//! `cargo xtask` — corpus-engine maintenance commands.
//!
//! Usage:
//!   cargo xtask arch-gate [--update-baseline]   Enforce ARCH §3.1 size ratchet + §1 doc-contract

use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");

    let exit_code = match cmd {
        "arch-gate" => cmd_arch_gate(&args[1..]),
        "help" | "--help" | "-h" => {
            print_usage();
            0
        }
        other => {
            eprintln!("Unknown xtask command: {other}");
            print_usage();
            1
        }
    };

    std::process::exit(exit_code);
}

// ── arch-gate ─────────────────────────────────────────────────────────────────
//
// A ratchet for ARCH_PRINCIPLES §3.1 ("> 1200 lines → split") and §1.1 (the
// SYSTEM_OVERVIEW project map must resolve). It does NOT try to clean existing
// debt — it FREEZES it via a baseline so the only allowed direction is down:
// a NEW oversized file or growth past slack fails CI. Pair with the §10 roadmap
// (where each baselined file gets its deferral rationale).

const ARCH_GATE_LINE_LIMIT: usize = 1200;
const ARCH_GATE_GROWTH_SLACK: usize = 50;

/// Repo root = grandparent of `corpus-engine/xtask/`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("xtask manifest has no grandparent")
        .to_path_buf()
}

/// Collect (repo-relative path, line count) for every `.rs` over the limit,
/// skipping build/vendor/vcs trees.
fn collect_oversized(dir: &Path, root: &Path, out: &mut Vec<(String, usize)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                "target" | "vendor" | ".git" | "node_modules" | ".sovereign"
            ) {
                continue;
            }
            collect_oversized(&path, root, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let lines = text.lines().count();
                if lines > ARCH_GATE_LINE_LIMIT {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((rel, lines));
                }
            }
        }
    }
}

/// Parse the `<count>\t<relpath>` baseline.
fn load_baseline(path: &Path) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((count, rel)) = line.split_once('\t') {
                if let Ok(n) = count.trim().parse::<usize>() {
                    map.insert(rel.trim().to_string(), n);
                }
            }
        }
    }
    map
}

/// §1.1 contract: every project dir named in the SYSTEM_OVERVIEW §1 tree exists.
fn doc_contract_failures(root: &Path) -> Vec<String> {
    let overview = root.join("sovereign/SYSTEM_OVERVIEW.md");
    let text = match std::fs::read_to_string(&overview) {
        Ok(t) => t,
        Err(e) => return vec![format!("cannot read {}: {e}", overview.display())],
    };
    let mut fails = Vec::new();
    // §1 has two fenced blocks: the project tree (first) lists the dirs we
    // verify; the dependency diagram (second) is ASCII art, not paths. Only
    // parse the FIRST fence.
    let (mut in_sec1, mut fences_seen) = (false, 0u8);
    for line in text.lines() {
        if line.starts_with("## 1.") {
            in_sec1 = true;
            continue;
        }
        if in_sec1 && line.starts_with("## ") {
            break;
        }
        if in_sec1 && line.trim_start().starts_with("```") {
            fences_seen += 1;
            continue;
        }
        if in_sec1 && fences_seen == 1 {
            let trimmed = line.trim_start_matches(['│', ' ', '├', '└', '─']);
            if let Some(tok) = trimmed.split_whitespace().next() {
                let dir = tok.trim_end_matches('/');
                if dir.is_empty() || dir == "commonwealth-ai" {
                    continue;
                }
                if !root.join(dir).exists() {
                    fails.push(format!(
                        "§1 names project `{dir}/` but it does not exist on disk"
                    ));
                }
            }
        }
    }
    fails
}

fn cmd_arch_gate(args: &[String]) -> i32 {
    let root = repo_root();
    let baseline_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("oversized_baseline.txt");
    let update = args.iter().any(|a| a == "--update-baseline");

    let mut oversized: Vec<(String, usize)> = Vec::new();
    collect_oversized(&root, &root, &mut oversized);
    oversized.sort();

    if update {
        let mut body = String::from(
            "# arch-gate oversized-file baseline (ARCH §3.1: > 1200 lines).\n\
             # Regenerate ONLY after an intentional split or a documented new big file:\n\
             #   cargo run -p xtask -- arch-gate --update-baseline\n\
             # The gate fails on a NEW oversized file or growth past slack, so existing\n\
             # debt is frozen and can only shrink. Format: <count>\\t<relpath>.\n",
        );
        for (rel, n) in &oversized {
            body.push_str(&format!("{n}\t{rel}\n"));
        }
        if let Err(e) = std::fs::write(&baseline_path, body) {
            eprintln!("error: failed to write baseline: {e}");
            return 1;
        }
        eprintln!(
            "wrote {} ({} oversized files frozen)",
            baseline_path.display(),
            oversized.len()
        );
        return 0;
    }

    let baseline = load_baseline(&baseline_path);
    if baseline.is_empty() {
        eprintln!(
            "error: no baseline at {}.\n  Run: cargo run -p xtask -- arch-gate --update-baseline",
            baseline_path.display()
        );
        return 1;
    }

    let mut failures: Vec<String> = Vec::new();
    for (rel, n) in &oversized {
        match baseline.get(rel) {
            None => failures.push(format!(
                "NEW oversized file: {rel} ({n} lines > {ARCH_GATE_LINE_LIMIT}). \
                 Split it (ARCH §3.2), or — if deferring — add a SYSTEM_OVERVIEW §10 \
                 roadmap entry and re-baseline."
            )),
            Some(&b) if *n > b + ARCH_GATE_GROWTH_SLACK => failures.push(format!(
                "GREW past slack: {rel} {b} → {n} lines (+{}, slack {ARCH_GATE_GROWTH_SLACK}). \
                 Trim or split (ARCH §3.1).",
                n - b
            )),
            _ => {}
        }
    }

    let doc_fails = doc_contract_failures(&root);

    eprintln!(
        "arch-gate: {} oversized files tracked vs baseline; §1 doc-contract checked",
        oversized.len()
    );
    for f in &failures {
        eprintln!("  ✗ size: {f}");
    }
    for f in &doc_fails {
        eprintln!("  ✗ doc:  {f}");
    }
    if failures.is_empty() && doc_fails.is_empty() {
        eprintln!("  ✓ no new oversized files, none grown past slack, §1 project dirs all resolve");
        0
    } else {
        eprintln!();
        eprintln!(
            "arch-gate FAILED ({} size, {} doc). See ARCH_PRINCIPLES.md §3 + SYSTEM_OVERVIEW.md §10.",
            failures.len(),
            doc_fails.len()
        );
        1
    }
}

fn print_usage() {
    eprintln!("Usage: cargo xtask <command>");
    eprintln!();
    eprintln!("Commands:");
    eprintln!(
        "  arch-gate [--update-baseline]  Enforce the §3.1 file-size ratchet + §1 doc-contract"
    );
}
