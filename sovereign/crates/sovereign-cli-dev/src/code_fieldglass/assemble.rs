// SPDX-License-Identifier: AGPL-3.0-or-later
//! Data assembly for the fieldglass page — the walkers, joiners and
//! reducers between the load stage and the renderer. Pure-ish helpers
//! (filesystem walk, subprocess shells, sidecar diff); split from `mod.rs`
//! per ARCH §3.2 (helpers first) when it crossed the §3.1 ceiling.

use std::collections::BTreeMap;
use std::path::Path;

use super::layout::strip_treemap;
use super::*;

// ── Assembly helpers ─────────────────────────────────────────────────────────

/// Walk the workspace for `.rs` files, attributing each to its crate by
/// longest `member_dirs` prefix. Excluded directories mirror
/// `walk_filesystem_sections` PLUS `target*` (its `target`-only match let
/// `target-xwin/` pollute the offender list — live finding, 2026-08-06).
/// Returns (per-file (path, crate, lines) sorted by path, count outside any
/// crate dir — reported, not silently dropped).
pub(super) fn walk_rs_files(root: &Path, member_dirs: &BTreeMap<String, String>) -> (Vec<(String, String, usize)>, usize) {
    let mut dirs_by_len: Vec<(&String, &String)> = member_dirs.iter().collect();
    dirs_by_len.sort_by_key(|(_, dir)| std::cmp::Reverse(dir.len()));

    let mut out = Vec::new();
    let mut outside = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy().into_owned();
            if path.is_dir() {
                if name.starts_with("target")
                    || matches!(name.as_str(), "vendor" | ".git" | "node_modules" | ".sovereign" | "dist")
                {
                    continue;
                }
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                let lines = text.lines().count();
                match dirs_by_len
                    .iter()
                    .find(|(_, d)| rel.starts_with(&format!("{d}/")))
                    .map(|(name, _)| (*name).clone())
                {
                    Some(crate_name) => out.push((rel, crate_name, lines)),
                    None => outside += 1,
                }
            }
        }
    }
    out.sort();
    (out, outside)
}

/// Shell the sibling `sovereign-cli`'s `cache-audit --by-file` — the ONE
/// transcript parser (§10.6) — and reduce its JSON to repo-relative
/// per-file stats. Files outside the repo (scratchpads, memory files) are
/// dropped here; the map only draws the workspace.
pub(super) fn agent_activity(
    root: &Path,
) -> std::result::Result<(BTreeMap<String, AgentStat>, u64, i64, i64), String> {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("sovereign-cli")))
        .filter(|p| p.exists());
    let bin = sibling
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sovereign".to_string());
    let out = std::process::Command::new(&bin)
        .args(["cache-audit", "--by-file", "--json", "--project"])
        .arg(root)
        .output()
        .map_err(|e| format!("spawn {bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{bin} cache-audit --by-file: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("parse by-file json: {e}"))?;
    let root_prefix = format!("{}/", root.display());
    let mut map = BTreeMap::new();
    if let Some(files) = v.get("files").and_then(|f| f.as_array()) {
        for f in files {
            let Some(path) = f.get("path").and_then(|p| p.as_str()) else { continue };
            let Some(rel) = path.strip_prefix(&root_prefix) else { continue };
            let g = |k: &str| f.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
            map.insert(
                rel.replace('\\', "/"),
                AgentStat {
                    reads: g("reads"),
                    read_tokens: g("read_tokens"),
                    edits: g("edits"),
                    sessions: g("sessions"),
                },
            );
        }
    }
    let gi = |k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
    Ok((map, v.get("sessions").and_then(|s| s.as_u64()).unwrap_or(0), gi("first_mtime"), gi("last_mtime")))
}

/// Diff the current file set against the PREVIOUS render's sidecar. Reads
/// defensively via `Value` — older sidecars predate several fields, and a
/// missing/unreadable previous render yields `None`, shown as "first
/// render", never as "no change" (§18.2: absence is reported, not defaulted).
pub(super) fn compute_delta(prev_sidecar: &Path, files: &[FileLeaf]) -> Option<Delta> {
    let text = std::fs::read_to_string(prev_sidecar).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let prev_unix = v.get("generated_unix")?.as_i64()?;
    let prev: BTreeMap<&str, (i64, bool)> = v
        .get("files")?
        .as_array()?
        .iter()
        .filter_map(|f| {
            Some((
                f.get("path")?.as_str()?,
                (
                    f.get("lines")?.as_i64()?,
                    f.get("offender").and_then(|o| o.as_bool()).unwrap_or(false),
                ),
            ))
        })
        .collect();
    let cur: BTreeMap<&str, &FileLeaf> = files.iter().map(|f| (f.path.as_str(), f)).collect();

    let mut grown: Vec<(String, i64)> = Vec::new();
    let mut new_offenders = Vec::new();
    let mut new_files = 0usize;
    for (path, f) in &cur {
        match prev.get(path) {
            Some((prev_lines, prev_off)) => {
                let d = f.lines as i64 - prev_lines;
                if d != 0 {
                    grown.push(((*path).to_string(), d));
                }
                if f.offender && !prev_off {
                    new_offenders.push((*path).to_string());
                }
            }
            None => {
                new_files += 1;
                grown.push(((*path).to_string(), f.lines as i64));
                if f.offender {
                    new_offenders.push((*path).to_string());
                }
            }
        }
    }
    let removed_files = prev.keys().filter(|p| !cur.contains_key(*p)).count();
    grown.sort_by(|x, y| y.1.abs().cmp(&x.1.abs()).then(x.0.cmp(&y.0)));
    grown.truncate(12);
    new_offenders.sort();
    Some(Delta { prev_unix, grown, new_offenders, new_files, removed_files })
}

/// Two-level strip treemap: crates (ordered by layer then name) → files
/// (ordered by path). Fixed order at both levels — see `layout.rs`.
#[allow(clippy::too_many_arguments)]
pub(super) fn layout_treemap(
    walked: &[(String, String, usize)],
    crate_layer: &BTreeMap<&str, i32>,
    file_fan_in: &BTreeMap<&str, usize>,
    file_community: &BTreeMap<String, i32>,
    bridges: &BTreeMap<String, f32>,
    agent: &BTreeMap<String, AgentStat>,
    churn: &BTreeMap<String, u32>,
) -> (Vec<CrateRect>, Vec<FileLeaf>) {
    let mut by_crate: BTreeMap<&str, Vec<&(String, String, usize)>> = BTreeMap::new();
    for row in walked {
        by_crate.entry(row.1.as_str()).or_default().push(row);
    }
    let mut crate_order: Vec<(&str, usize)> = by_crate
        .iter()
        .map(|(name, files)| (*name, files.iter().map(|f| f.2.max(1)).sum()))
        .collect();
    crate_order.sort_by_key(|(name, _)| {
        (crate_layer.get(name).copied().unwrap_or(-1), name.to_string())
    });

    let crate_items: Vec<(String, f64)> = crate_order
        .iter()
        .map(|(name, lines)| ((*name).to_string(), *lines as f64))
        .collect();
    let crate_rects_raw = strip_treemap(&crate_items, 0.0, 0.0, CANVAS_W, CANVAS_H);

    let mut crate_rects = Vec::new();
    let mut files = Vec::new();
    for rect in &crate_rects_raw {
        let name = rect.key.as_str();
        crate_rects.push(CrateRect {
            name: name.to_string(),
            layer: crate_layer.get(name).copied().unwrap_or(-1),
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
        });
        let inner_y = rect.y + CRATE_LABEL_PAD.min(rect.h * 0.3);
        let inner_h = (rect.h - CRATE_LABEL_PAD).max(rect.h * 0.5);
        let members = &by_crate[name];
        let file_items: Vec<(String, f64)> =
            members.iter().map(|(path, _, lines)| (path.clone(), *lines as f64)).collect();
        for leaf in strip_treemap(&file_items, rect.x + 1.0, inner_y, (rect.w - 2.0).max(1.0), inner_h)
        {
            let (path, _, lines) =
                members.iter().find(|(p, _, _)| *p == leaf.key).expect("leaf from members");
            let a = agent.get(path).cloned().unwrap_or_default();
            files.push(FileLeaf {
                path: path.clone(),
                crate_name: name.to_string(),
                x: leaf.x,
                y: leaf.y,
                w: leaf.w,
                h: leaf.h,
                lines: *lines,
                fan_in: file_fan_in.get(path.as_str()).copied().unwrap_or(0),
                community: file_community.get(path).copied().unwrap_or(-1),
                bridge: bridges.get(path).copied().unwrap_or(0.0),
                offender: *lines > 1200,
                reads: a.reads,
                read_tokens: a.read_tokens,
                edits: a.edits,
                agent_sessions: a.sessions,
                commits_90d: churn.get(path).copied().unwrap_or(0),
            });
        }
    }
    (crate_rects, files)
}

/// Reduce the dry-report's clusters to renderable cross-file arcs plus the
/// attention-queue summaries. The global cap keeps the arcs with the MOST
/// duplicated lines, not whichever clusters happened to serialize first —
/// the original first-come cap silently kept small clones and dropped big
/// ones (caught in the 2026-08-06 render audit). The cut count lands in the
/// honesty footer.
pub(super) fn dup_arcs_from(
    report: &sovereign_tools::code::dry_report::DryReport,
    root: &Path,
) -> (Vec<DupArc>, usize, Vec<DupClusterSummary>) {
    let rel = |p: &str| -> String {
        let root_s = format!("{}/", root.display());
        p.strip_prefix(&root_s).unwrap_or(p).replace('\\', "/")
    };
    let mut arcs = Vec::new();
    let mut summaries = Vec::new();
    let mut collect =
        |members: &[sovereign_tools::code::dry_report::SymbolRef], sim: f32, exact: bool, lines: usize| {
            let mut files: Vec<String> = members.iter().map(|m| rel(&m.file)).collect();
            files.sort();
            files.dedup();
            summaries.push(DupClusterSummary {
                label: members.first().map(|m| m.symbol.clone()).unwrap_or_default(),
                members: members.len(),
                redundant: lines * members.len().saturating_sub(1),
                files,
                lines,
                exact,
            });
            let mut n = 0usize;
            for (i, a) in members.iter().enumerate() {
                for b in &members[i + 1..] {
                    if a.file == b.file || n >= DUP_ARCS_PER_CLUSTER {
                        continue;
                    }
                    arcs.push(DupArc {
                        a: rel(&a.file),
                        a_line: a.line_start,
                        b: rel(&b.file),
                        b_line: b.line_start,
                        sim,
                        exact,
                        lines,
                    });
                    n += 1;
                }
            }
        };
    for c in &report.exact_clones {
        collect(&c.members, 1.0, true, c.lines);
    }
    for c in &report.near_clusters {
        collect(&c.members, c.min_sim, false, c.unit_lines);
    }
    arcs.sort_by(|x, y| {
        y.lines.cmp(&x.lines).then_with(|| x.a.cmp(&y.a)).then_with(|| x.b.cmp(&y.b))
    });
    let dropped = arcs.len().saturating_sub(DUP_ARCS_TOTAL);
    arcs.truncate(DUP_ARCS_TOTAL);
    summaries.sort_by(|x, y| {
        y.redundant.cmp(&x.redundant).then_with(|| x.label.cmp(&y.label))
    });
    summaries.truncate(12);
    (arcs, dropped, summaries)
}


/// `git rev-list --count <indexed>..HEAD` — how far the SCIP index lags.
/// `None` when the indexed head is unknown or not an ancestor git can count
/// (e.g. after a force-push); the page then reports "unknown", never "fresh".
pub(super) fn commits_behind(root: &Path, indexed_head: &str) -> Option<u64> {
    if indexed_head.is_empty() {
        return None;
    }
    std::process::Command::new("git")
        .args(["rev-list", "--count", &format!("{indexed_head}..HEAD")])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
}

/// Age of the chunk-embedding index in days, from `_corpus_meta.json`'s
/// `last_updated` (unix seconds). `None` when unreadable.
pub(super) fn chunk_index_age_days(index_dir: &Path, now_unix: i64) -> Option<f64> {
    let text = std::fs::read_to_string(index_dir.join("_corpus_meta.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let updated = v.get("last_updated")?.as_i64()?;
    Some(((now_unix - updated).max(0) as f64) / 86_400.0)
}

pub(super) fn git_head(root: &Path) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

