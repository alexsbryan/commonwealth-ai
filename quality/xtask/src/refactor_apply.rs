// SPDX-License-Identifier: AGPL-3.0-or-later
//! refactor-apply — execute a named move-recipe against the working tree.
//!
//! The separation-of-concerns half of the refactor factory (2026-09-03):
//! a PLAN is data — exact spans and destinations, derived by a human or
//! `suggest-seams` — and this verb applies it MECHANICALLY. No model in
//! the mutation loop: the supervised judge.rs run showed the model's
//! emission compliance is the failure surface, while the recipe itself
//! (cut lines A..B, patch the opener) is deterministic.
//!
//! Plan shape (TOML; line numbers 1-based, inclusive; steps apply in
//! order, each seeing the file after the previous — the author accounts
//! for the shift, exactly as in hand surgery):
//!
//! ```toml
//! [plan]
//! subject = "sovereign/crates/sovereign-core/src/runtime/grounding/judge.rs"
//! verify_cmd = "cargo test -p sovereign-core --lib"   # optional, run after each step
//!
//! [[move]]
//! start = 1846
//! end = 3010
//! dest = "src/runtime/grounding/judge/tests.rs"       # created; existing dest is appended to
//!
//! [[patch]]
//! file = "sovereign/crates/sovereign-core/src/runtime/grounding/judge.rs"  # optional, defaults to subject
//! start = 1845
//! end = 1846
//! body = """#[cfg(test)]
//! mod tests;"""
//! ```
//!
//! Pairing note: `sovereign-tdd`'s `EditAction::MoveLines` mirrors
//! these semantics for MODEL-driven rounds. This verb exists so the same
//! recipe executes with NO model at all. If the two ever diverge, the
//! tests in this file and in tdd's apply.rs are the two halves of the
//! contract.
//!
//! On a failed verify: stop, name the step, exit 1. Steps already
//! applied stay applied — git, not this tool, is the rollback.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Paths in a plan are repo-relative and must stay under the repo —
/// an absolute path or a `..` escape is a plan bug, refused before any
/// byte moves. `..` is normalized LEXICALLY (component-wise): a naive
/// `join` + `starts_with` lets `../x` through, because the prefix
/// check compares components in order and `..` is just a later
/// component (watched in this file's own tests).
fn resolve(root: &Path, p: &str) -> Result<PathBuf, String> {
    if p.starts_with('/') {
        return Err(format!("absolute path {p:?} — plans are repo-relative"));
    }
    let mut norm = root.to_path_buf();
    for comp in Path::new(p).components() {
        match comp {
            std::path::Component::ParentDir => {
                if !norm.pop() {
                    return Err(format!("path {p:?} escapes the repo root"));
                }
            }
            std::path::Component::CurDir => {}
            c => norm.push(c.as_os_str()),
        }
    }
    if !norm.starts_with(root) {
        return Err(format!("path {p:?} escapes the repo root"));
    }
    Ok(norm)
}

fn cut_and_append(
    root: &Path,
    src: &str,
    start: usize,
    end: usize,
    dest: &str,
) -> Result<(), String> {
    let src_path = resolve(root, src)?;
    let dest_path = resolve(root, dest)?;
    let existing = std::fs::read_to_string(&src_path).map_err(|e| format!("read {src}: {e}"))?;
    let mut lines: Vec<&str> = existing.lines().collect();
    let s = start.saturating_sub(1);
    let e = end.min(lines.len());
    if start < 1 || s >= lines.len() || e < s {
        return Err(format!(
            "move range {start}..{end} out of range for {src} ({} lines)",
            lines.len()
        ));
    }
    let moved = lines[s..e].join("\n");
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let mut dest_content = std::fs::read_to_string(&dest_path).unwrap_or_default();
    if !dest_content.is_empty() && !dest_content.ends_with('\n') {
        dest_content.push('\n');
    }
    dest_content.push_str(&moved);
    dest_content.push('\n');
    std::fs::write(&dest_path, dest_content).map_err(|e| format!("write {dest}: {e}"))?;
    lines.drain(s..e);
    std::fs::write(&src_path, lines.join("\n") + "\n")
        .map_err(|e| format!("rewrite {src}: {e}"))?;
    Ok(())
}

fn patch_range(
    root: &Path,
    file: &str,
    start: usize,
    end: usize,
    body: &str,
) -> Result<(), String> {
    let path = resolve(root, file)?;
    let existing = std::fs::read_to_string(&path).map_err(|e| format!("read {file}: {e}"))?;
    let mut lines: Vec<&str> = existing.lines().collect();
    let s = start.saturating_sub(1);
    let e = end.min(lines.len());
    if start < 1 || s >= lines.len() || e < s {
        return Err(format!(
            "patch range {start}..{end} out of range for {file} ({} lines)",
            lines.len()
        ));
    }
    let replacement: Vec<&str> = if body.is_empty() {
        Vec::new()
    } else {
        body.lines().collect()
    };
    let mut out: Vec<&str> = Vec::with_capacity(lines.len() - (e - s) + replacement.len());
    out.extend_from_slice(&lines[..s]);
    out.extend_from_slice(&replacement);
    out.extend_from_slice(&lines[e..]);
    std::fs::write(&path, out.join("\n") + "\n").map_err(|e| format!("write {file}: {e}"))?;
    Ok(())
}

pub fn run(args: &[String]) -> i32 {
    let mut plan_path: Option<String> = None;
    let mut land = false;
    for a in args {
        match a.as_str() {
            "--land" => land = true,
            "-h" | "--help" => {
                println!("cargo xtask refactor-apply <plan.toml> [--land]");
                println!();
                println!("Execute a named move-recipe (TOML: [[move]] / [[patch]] steps with");
                println!("1-based inclusive ranges, applied in order) against the working tree,");
                println!("running the plan's verify_cmd after each step. --land chains");
                println!("refactor-land (conformance + arch-gate --tighten) on success.");
                println!("See this file's module docs for the plan shape.");
                return 0;
            }
            other => {
                if plan_path.is_none() {
                    plan_path = Some(other.to_string());
                }
            }
        }
    }
    let Some(plan_rel) = plan_path else {
        eprintln!("error: a plan path is required — `cargo xtask refactor-apply <plan.toml>`");
        return 2;
    };
    let root = crate::common::repo_root();
    let raw = match std::fs::read_to_string(root.join(&plan_rel)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: read {plan_rel}: {e}");
            return 2;
        }
    };
    // serde is not an xtask dep (std + arch-layers + serde_json only):
    // the plan parses into a Value and the fields are read by hand. The
    // shape is three keys and two arrays — a deserializer would be the
    // heavier half. TOML nesting: subject/verify_cmd live under [plan],
    // while [[move]]/[[patch]] are root-level arrays.
    let v: serde_json::Value = match toml::from_str(&raw) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: parse {plan_rel}: {e}");
            return 2;
        }
    };
    let meta = v.get("plan").unwrap_or(&v);
    let subject = match meta.get("subject").and_then(|s| s.as_str()) {
        Some(s) => s.to_string(),
        None => {
            eprintln!("error: plan needs [plan] subject (the file being split)");
            return 2;
        }
    };
    let verify_cmd = meta
        .get("verify_cmd")
        .and_then(|s| s.as_str())
        .map(str::to_string);

    let mut steps: Vec<String> = Vec::new();
    let push_steps = |key: &str, kind: &str, steps: &mut Vec<String>| {
        if let Some(arr) = v.get(key).and_then(|a| a.as_array()) {
            for m in arr {
                let src = m
                    .get("src")
                    .or_else(|| m.get("file"))
                    .and_then(|s| s.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| subject.clone());
                let start = m.get("start").and_then(|s| s.as_u64()).unwrap_or(0);
                let end = m.get("end").and_then(|s| s.as_u64()).unwrap_or(0);
                let payload = m
                    .get("dest")
                    .or_else(|| m.get("body"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                steps.push(format!("{kind}|{src}|{start}|{end}|{payload}"));
            }
        }
    };
    push_steps("move", "MOVE", &mut steps);
    push_steps("patch", "PATCH", &mut steps);
    if steps.is_empty() {
        eprintln!("error: plan carries no steps");
        return 2;
    }

    for (i, step) in steps.iter().enumerate() {
        let mut fields = step.splitn(5, '|');
        let kind = fields.next().unwrap_or_default();
        let file = fields.next().unwrap_or_default();
        let start: usize = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let end: usize = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let payload = fields.next().unwrap_or_default();
        let result = match kind {
            "MOVE" => cut_and_append(&root, file, start, end, payload),
            "PATCH" => patch_range(&root, file, start, end, payload),
            other => Err(format!("unknown step kind {other:?}")),
        };
        if let Err(e) = result {
            eprintln!(
                "refactor-apply: step {} ({kind} {file} {start}..{end}) FAILED: {e}",
                i + 1
            );
            eprintln!("steps already applied stay applied — git is the rollback");
            return 1;
        }
        println!(
            "  [{}/{}] {} {} {}..{} → {}",
            i + 1,
            steps.len(),
            kind,
            file,
            start,
            end,
            if kind == "MOVE" { "" } else { "patched" }
        );
        if let Some(verify) = &verify_cmd {
            let out = Command::new("sh").arg("-c").arg(verify).status();
            match out {
                Ok(s) if s.success() => println!("    verify: green"),
                Ok(s) => {
                    eprintln!(
                        "refactor-apply: verify_cmd failed ({}) after step {} — stopping",
                        s,
                        i + 1
                    );
                    eprintln!("steps already applied stay applied — git is the rollback");
                    return 1;
                }
                Err(e) => {
                    eprintln!("refactor-apply: verify_cmd spawn failed: {e}");
                    return 1;
                }
            }
        }
    }
    println!("refactor-apply: {} steps applied", steps.len());

    if land {
        println!();
        crate::refactor_land::run(&[])
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("xtask-refactor-apply-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir)
    }

    #[test]
    fn move_relocates_a_span_and_creates_the_dest() {
        let unique = "move";
        let (_d, root) = scratch(unique);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("lib.rs"), "a\nb\nc\nd\ne\n").unwrap();
        cut_and_append(&root, "lib.rs", 2, 3, "src/out.rs").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("lib.rs")).unwrap(),
            "a\nd\ne\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("src/out.rs")).unwrap(),
            "b\nc\n"
        );
    }

    #[test]
    fn patch_replaces_a_range_with_the_body() {
        let unique = "patch";
        let (_d, root) = scratch(unique);
        std::fs::write(root.join("f.rs"), "one\ntwo\nthree\n").unwrap();
        patch_range(&root, "f.rs", 2, 2, "TWO\nTWO-B").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("f.rs")).unwrap(),
            "one\nTWO\nTWO-B\nthree\n"
        );
    }

    #[test]
    fn an_empty_patch_body_deletes_the_range() {
        let unique = "delete";
        let (_d, root) = scratch(unique);
        std::fs::write(root.join("f.rs"), "opener\nbody\n}\n").unwrap();
        patch_range(&root, "f.rs", 1, 3, "").unwrap();
        assert_eq!(std::fs::read_to_string(root.join("f.rs")).unwrap(), "\n");
    }

    #[test]
    fn escapes_and_absolute_paths_are_refused_before_any_move() {
        let unique = "escape";
        let (_d, root) = scratch(unique);
        assert!(resolve(&root, "/etc/passwd").is_err());
        assert!(resolve(&root, "../outside.rs").is_err());
        assert!(resolve(&root, "src/ok.rs").is_ok());
    }

    #[test]
    fn out_of_range_spans_are_named_errors() {
        let unique = "range";
        let (_d, root) = scratch(unique);
        std::fs::write(root.join("f.rs"), "one\n").unwrap();
        assert!(cut_and_append(&root, "f.rs", 5, 9, "out.rs").is_err());
        assert!(patch_range(&root, "f.rs", 5, 9, "x").is_err());
    }
}
