// SPDX-License-Identifier: AGPL-3.0-or-later
//! docs-gate — SYSTEM_OVERVIEW §1.1 declares itself a contract: every claim
//! verifiable against the commit. This gate mechanizes the path half of that
//! contract — every repo path the narrative docs cite (inline `code` spans
//! and markdown link targets) must resolve on disk. It exists because doc rot
//! of exactly this class shipped: a `~/.claude/plans/…` machine-local
//! citation survived in §2 until an external review caught it (fixed
//! 2026-07-01).

use std::collections::BTreeSet;
use std::path::Path;

use crate::common;

/// The narrative contracts whose citations are gated, plus the newcomer
/// tour (a compressed rendering of the contract — same rot exposure).
const DOCS_GATE_DOCS: &[&str] = &[
    "sovereign/SYSTEM_OVERVIEW.md",
    "sovereign/ARCH_PRINCIPLES.md",
    "docs/ARCHITECTURE_TOUR.md",
];

/// Extensions worth resolving. Deliberately EXCLUDES runtime artifacts
/// (`.db`, `.gguf`, `.lance`, `.json`, `.jsonl`) — those citations name
/// files materialized under `~/.sovereign`/`target`, not the repo.
const DOCS_GATE_EXTS: &[&str] = &[
    ".rs", ".md", ".toml", ".sh", ".py", ".mjs", ".ts", ".yml", ".txt",
];

pub fn run() -> i32 {
    let root = common::repo_root();
    let mut index: Vec<Vec<String>> = Vec::new();
    build_path_index(&root, &root, &mut index);
    let allowlist_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("doc_path_allowlist.txt");
    let allow: BTreeSet<String> = std::fs::read_to_string(&allowlist_path)
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let mut fails: BTreeSet<String> = BTreeSet::new();
    let mut checked = 0usize;

    for doc in DOCS_GATE_DOCS {
        let path = root.join(doc);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("error: cannot read {}: {e}", path.display());
                return 1;
            }
        };
        for raw in candidate_spans(&text) {
            let raw = raw.trim();
            if allow.contains(raw) {
                continue;
            }
            if is_machine_local(raw) {
                fails.insert(format!(
                    "{doc}: cites machine-local path `{raw}` — unverifiable by any other reader"
                ));
                continue;
            }
            let norm = normalize_span(raw);
            if allow.contains(norm.as_str()) || !looks_like_repo_path(&norm) {
                continue;
            }
            checked += 1;
            if !subsequence_resolves(&index, &norm) {
                fails.insert(format!(
                    "{doc}: cites `{raw}` but no file/dir in the repo matches — \
                     fix the citation, or allowlist it in xtask/doc_path_allowlist.txt \
                     if the reference to a removed/external path is intentional"
                ));
            }
        }
    }

    let overview =
        std::fs::read_to_string(root.join("sovereign/SYSTEM_OVERVIEW.md")).unwrap_or_default();
    let enum_fails = enumeration_failures(&root, &overview, &allow);
    let n_enum = enum_fails.len();
    fails.extend(enum_fails);

    eprintln!(
        "docs-gate: {checked} cited paths + enumeration sync (extractors, workspace \
         crates; {n_enum} failing) across {} docs ({} allowlisted spans)",
        DOCS_GATE_DOCS.len(),
        allow.len()
    );
    for f in &fails {
        eprintln!("  ✗ {f}");
    }
    if fails.is_empty() {
        eprintln!("  ✓ every cited path resolves");
        0
    } else {
        eprintln!();
        eprintln!(
            "docs-gate FAILED ({} unresolved citations). The narrative docs are a \
             contract (SYSTEM_OVERVIEW §1.1) — update the doc with the code.",
            fails.len()
        );
        1
    }
}

/// Build the resolution index: every file and directory path in the repo
/// (skipping build/vendor/vcs trees) as component vectors. Citations are
/// matched as ordered component subsequences against this index, because
/// the docs deliberately cite in shorthand — `runtime/prompts.rs` for
/// `sovereign/crates/sovereign-core/src/runtime/prompts.rs`,
/// `commonwealth-api/admission.rs` skipping the `src/`. Subsequence
/// matching tolerates that while still failing on a renamed, moved-away,
/// or deleted terminal file.
fn build_path_index(dir: &Path, root: &Path, out: &mut Vec<Vec<String>>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if matches!(
                name.as_str(),
                "target" | "vendor" | ".git" | "node_modules" | ".sovereign" | "dist"
            ) {
                continue;
            }
            push_components(&path, root, out);
            build_path_index(&path, root, out);
        } else {
            push_components(&path, root, out);
        }
    }
}

fn push_components(path: &Path, root: &Path, out: &mut Vec<Vec<String>>) {
    if let Ok(rel) = path.strip_prefix(root) {
        out.push(
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect(),
        );
    }
}

/// True when `span`'s components appear in order (gaps allowed) in some
/// indexed path, ending exactly at the indexed path's last component.
/// Anchoring the tail means `foo/bar.rs` never matches `foo/bar.rs.bak`,
/// and a citation of a directory must name a real terminal directory.
fn subsequence_resolves(index: &[Vec<String>], span: &str) -> bool {
    let want: Vec<&str> = span
        .split('/')
        .filter(|c| !c.is_empty() && *c != "." && *c != "..")
        .collect();
    let Some(last) = want.last() else {
        return false;
    };
    index.iter().any(|path| {
        if path.last().map(String::as_str) != Some(*last) {
            return false;
        }
        let mut it = path.iter();
        want.iter().all(|w| it.by_ref().any(|c| c == w))
    })
}

/// Extract candidate spans: inline `code` (outside fenced blocks — fences
/// hold trees and shell transcripts, not citations) plus markdown link
/// targets `](…)` on any line.
fn candidate_spans(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        let mut rest = line;
        while let Some(open) = rest.find("](") {
            let after = &rest[open + 2..];
            match after.find(')') {
                Some(close) => {
                    out.push(after[..close].to_string());
                    rest = &after[close + 1..];
                }
                None => break,
            }
        }
        if in_fence {
            continue;
        }
        let mut rest = line;
        while let Some(open) = rest.find('`') {
            let after = &rest[open + 1..];
            match after.find('`') {
                Some(close) => {
                    out.push(after[..close].to_string());
                    rest = &after[close + 1..];
                }
                None => break,
            }
        }
    }
    out
}

/// A span citing a machine-local location can never be verified by another
/// reader — hard failure regardless of extension. The bare `~/.claude/`
/// dir is exempt: it appears as a feature's default-config VALUE (the
/// alignment working-set dir), which is legitimate; citing a document
/// *inside* it is not.
fn is_machine_local(span: &str) -> bool {
    (span.starts_with("~/.claude/") && span.len() > "~/.claude/".len())
        || span.starts_with("/Users/")
        || span.starts_with("/home/")
}

/// Conservative "is this meant to be a repo path" filter: slash-bearing,
/// single-token, relative, and ending in a gated extension or a `/`
/// (directory citation). Everything else (shell commands, URLs, routes,
/// `crate::paths`, HF repo slugs) is out of jurisdiction.
fn looks_like_repo_path(span: &str) -> bool {
    if !span.contains('/') || span.is_empty() {
        return false;
    }
    if span.chars().any(char::is_whitespace) {
        return false;
    }
    if ['*', '<', '>', '{', '}', '(', '|', '…', '`', '\\']
        .iter()
        .any(|c| span.contains(*c))
    {
        return false;
    }
    if span.starts_with('/')
        || span.starts_with('~')
        || span.starts_with('$')
        || span.starts_with('#')
        || span.starts_with('.') // ./ links are normalized; other dot-paths (.sovereign/…) are runtime artifacts
        || span.starts_with("http")
        || span.starts_with("mailto:")
        || span.contains("://")
    {
        return false;
    }
    span.ends_with('/') || DOCS_GATE_EXTS.iter().any(|e| span.ends_with(e))
}

/// Strip link/citation decoration down to the resolvable path: leading
/// `./`/`../` hops and a trailing `#anchor`. The trailing `/` of a
/// directory citation is kept — `looks_like_repo_path` keys on it.
fn normalize_span(span: &str) -> String {
    let mut s = span.trim();
    loop {
        if let Some(rest) = s.strip_prefix("./") {
            s = rest;
        } else if let Some(rest) = s.strip_prefix("../") {
            s = rest;
        } else {
            break;
        }
    }
    s.split('#').next().unwrap_or(s).to_string()
}

/// Enumeration-sync half of the gate: registries whose members the
/// overview must at least mention. Direction is code→doc only — a name
/// in code missing from the doc is rot ("we add without amending");
/// doc-side extras stay a human judgment. The 2026-07-02 audit found 10
/// undocumented extractors, 7 undocumented CLI verbs, and a whole
/// unmentioned crate this way; this check keeps that class extinct.
fn enumeration_failures(root: &Path, doc: &str, allow: &BTreeSet<String>) -> Vec<String> {
    let mut fails = Vec::new();

    // 1. Recipe extractors — every `ExtractorConfig` serde rename.
    if let Ok(recipe) = std::fs::read_to_string(root.join("corpus-engine/src/recipe.rs")) {
        if let Some(start) = recipe.find("pub enum ExtractorConfig") {
            let body = &recipe[start..];
            let body = &body[..body.find("\n}").unwrap_or(body.len())];
            for cap in body.split("rename = \"").skip(1) {
                let name = cap.split('"').next().unwrap_or("");
                if !name.is_empty() && !doc.contains(name) && !allow.contains(name) {
                    fails.push(format!(
                        "extractor `{name}` (ExtractorConfig, recipe.rs) is not \
                         mentioned in SYSTEM_OVERVIEW — update the §3 Extractor row"
                    ));
                }
            }
        }
    }

    // 2. Workspace crates — every member's dir-name appears somewhere.
    if let Ok(manifest) = std::fs::read_to_string(root.join("Cargo.toml")) {
        if let Some(start) = manifest.find("members = [") {
            let body = &manifest[start..];
            let body = &body[..body.find(']').unwrap_or(body.len())];
            let mut names: Vec<String> = Vec::new();
            for m in body.split('"').skip(1).step_by(2) {
                if let Some(parent) = m.strip_suffix("/*") {
                    if let Ok(rd) = std::fs::read_dir(root.join(parent)) {
                        names.extend(
                            rd.flatten()
                                .filter(|e| e.path().is_dir())
                                .map(|e| e.file_name().to_string_lossy().to_string()),
                        );
                    }
                } else {
                    names.push(m.rsplit('/').next().unwrap_or(m).to_string());
                }
            }
            for name in names {
                if !doc.contains(&name) && !allow.contains(&name) {
                    fails.push(format!(
                        "workspace crate `{name}` is not mentioned anywhere in \
                         SYSTEM_OVERVIEW — the map has a hole"
                    ));
                }
            }
        }
    }
    fails
}
