//! `cargo xtask` — corpus-engine maintenance commands.
//!
//! Usage:
//!   cargo xtask update-registry-snapshot        Fetch live registry and write snapshot
//!   cargo xtask arch-gate [--update-baseline]   Enforce ARCH §3.1 size ratchet + §1 doc-contract

use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");

    let exit_code = match cmd {
        "update-registry-snapshot" => cmd_update_registry_snapshot(),
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

// ── update-registry-snapshot ─────────────────────────────────────────────────

fn cmd_update_registry_snapshot() -> i32 {
    // Locate the snapshot file relative to CARGO_MANIFEST_DIR of xtask,
    // which is corpus-engine/xtask/. The snapshot is one level up.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let snapshot_path = std::path::Path::new(manifest_dir)
        .parent()
        .expect("xtask has no parent dir")
        .join("registry_snapshot.toml");

    eprintln!("Reading bundled snapshot: {}", snapshot_path.display());

    let current_text = match std::fs::read_to_string(&snapshot_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: failed to read snapshot: {e}");
            return 1;
        }
    };

    // Parse current snapshot to get registry_url.
    let current: toml::Value = match toml::from_str(&current_text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: failed to parse snapshot: {e}");
            return 1;
        }
    };

    let registry_url = current
        .get("registry_url")
        .and_then(|v| v.as_str())
        .unwrap_or(
            "https://raw.githubusercontent.com/alexsbryan/sovereign-recipes/main/registry.toml",
        );

    eprintln!("Fetching live registry from: {registry_url}");

    let live_text = match reqwest::blocking::get(registry_url) {
        Ok(resp) => {
            if !resp.status().is_success() {
                eprintln!("error: HTTP {} fetching registry", resp.status());
                return 1;
            }
            match resp.text() {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: failed to read response body: {e}");
                    return 1;
                }
            }
        }
        Err(e) => {
            eprintln!("error: failed to fetch registry: {e}");
            eprintln!();
            eprintln!(
                "If the public repo does not exist yet, you can skip this step.\n\
                 The bundled snapshot is the source of truth until the repo is live."
            );
            return 1;
        }
    };

    // Parse live registry.
    let live: toml::Value = match toml::from_str(&live_text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: failed to parse live registry: {e}");
            return 1;
        }
    };

    // Validate schema_version compatibility.
    let live_version = live
        .get("schema_version")
        .and_then(|v| v.as_integer())
        .unwrap_or(1);
    let current_version = current
        .get("schema_version")
        .and_then(|v| v.as_integer())
        .unwrap_or(1);

    if live_version > current_version {
        eprintln!(
            "warning: live registry schema_version ({live_version}) is newer than \
             bundled ({current_version}). Xtask update may need to be updated too."
        );
    }

    // Summarize changes.
    let live_entries = live
        .get("recipes")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let current_entries = current
        .get("recipes")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    if live_entries != current_entries {
        eprintln!(
            "  entries: {} → {} ({}{})",
            current_entries,
            live_entries,
            if live_entries > current_entries { "+" } else { "" },
            live_entries as i64 - current_entries as i64,
        );
    } else {
        eprintln!("  entries: {live_entries} (unchanged)");
    }

    // Write updated snapshot (preserving header comments is not possible via toml crate,
    // so prepend a standard header).
    let new_snapshot = format!(
        "# Registry snapshot — bundled at compile time.\n\
         # This file is the ONLY compile-time corpus catalog artifact.\n\
         # Keep up to date by running: cargo xtask update-registry-snapshot\n\
         #\n\
         {live_text}"
    );

    match std::fs::write(&snapshot_path, &new_snapshot) {
        Ok(()) => {
            eprintln!("Updated: {}", snapshot_path.display());
            0
        }
        Err(e) => {
            eprintln!("error: failed to write snapshot: {e}");
            1
        }
    }
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
            let trimmed =
                line.trim_start_matches(['│', ' ', '├', '└', '─']);
            if let Some(tok) = trimmed.split_whitespace().next() {
                let dir = tok.trim_end_matches('/');
                if dir.is_empty() || dir == "commonwealth-ai" {
                    continue;
                }
                if !root.join(dir).exists() {
                    fails.push(format!("§1 names project `{dir}/` but it does not exist on disk"));
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
    eprintln!("  update-registry-snapshot       Fetch live registry.toml and update the bundled snapshot");
    eprintln!("  arch-gate [--update-baseline]  Enforce the §3.1 file-size ratchet + §1 doc-contract");
}
