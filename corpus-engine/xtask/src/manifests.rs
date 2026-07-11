// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hand-rolled Cargo manifest parsing shared by the gates.
//!
//! Deliberately line-oriented (no full TOML parse): the gates key on the
//! narrow, stable subset of manifest shapes this workspace actually uses —
//! `name = "…"`, `foo = { workspace = true }`, `foo = { path = "…" }` — plus
//! the `[dependencies.foo]` table style. The table style is unused in this
//! workspace, but a boundary gate that misses it would be trivially
//! bypassable, so it parses (the live negative test injects exactly that
//! shape).

use arch_layers::{DepEdge, DepKind};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

/// A workspace member: package `name` + repo-relative `dir`.
#[derive(Debug, Clone)]
pub struct MemberCrate {
    pub name: String,
    pub dir: String,
}

/// Expand the root `members = […]` list (including `dir/*` globs) and read
/// each member's package name.
pub fn workspace_members(root: &Path) -> Vec<MemberCrate> {
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
    let mut dirs: Vec<String> = Vec::new();
    if let Some(start) = manifest.find("members = [") {
        let body = &manifest[start..];
        let body = &body[..body.find(']').unwrap_or(body.len())];
        for m in body.split('"').skip(1).step_by(2) {
            if let Some(parent) = m.strip_suffix("/*") {
                if let Ok(rd) = std::fs::read_dir(root.join(parent)) {
                    for e in rd.flatten() {
                        if e.path().is_dir() {
                            dirs.push(format!("{parent}/{}", e.file_name().to_string_lossy()));
                        }
                    }
                }
            } else {
                dirs.push(m.to_string());
            }
        }
    }
    let mut out = Vec::new();
    for dir in dirs {
        let text = match std::fs::read_to_string(root.join(&dir).join("Cargo.toml")) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if let Some(name) = parse_package_name(&text) {
            out.push(MemberCrate { name, dir });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `name = "…"` inside the `[package]` section.
pub fn parse_package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_package = t == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = t.strip_prefix("name") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                return rest.trim().split('"').nth(1).map(String::from);
            }
        }
    }
    None
}

/// Every internal (member → member) dependency edge in the workspace, with
/// its section kind. Target-specific tables (`[target.'…'.dependencies]`)
/// classify by their suffix. Optional deps count: an edge behind a feature
/// is still a declared edge (the layer map governs the all-features union).
/// Internal renames (`foo = { package = "bar", … }`) are not handled — none
/// exist in this workspace.
pub fn internal_dep_edges(root: &Path, members: &[MemberCrate]) -> Vec<DepEdge> {
    let names: BTreeSet<&str> = members.iter().map(|m| m.name.as_str()).collect();
    let mut edges = Vec::new();
    for m in members {
        let text = match std::fs::read_to_string(root.join(&m.dir).join("Cargo.toml")) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for (dep, kind) in deps_with_kinds(&text) {
            if names.contains(dep.as_str()) && dep != m.name {
                edges.push(DepEdge {
                    from: m.name.clone(),
                    to: dep,
                    kind,
                });
            }
        }
    }
    edges
}

/// (dep name, section kind) for every entry in every dependencies table —
/// both the section style (`[dependencies]` + `foo = …` lines, including
/// `[target.'…'.dependencies]`) and the table style (`[dependencies.foo]`).
pub fn deps_with_kinds(manifest: &str) -> Vec<(String, DepKind)> {
    let mut out = Vec::new();
    let mut kind: Option<DepKind> = None;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            kind = None;
            if let Some((k, inline_name)) = header_dep_context(t) {
                match inline_name {
                    // `[dependencies.foo]` — the header itself IS the dep.
                    Some(name) => out.push((name, k)),
                    None => kind = Some(k),
                }
            }
            continue;
        }
        let Some(k) = kind else { continue };
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((name, _)) = t.split_once('=') else {
            continue;
        };
        let name = name.trim().trim_matches('"');
        if !name.is_empty() {
            out.push((name.to_string(), k));
        }
    }
    out
}

/// Classify a `[…]` header line: `Some((kind, None))` for a dependencies
/// section, `Some((kind, Some(name)))` for a `[dependencies.name]` table,
/// `None` for anything else. Root-manifest `[workspace.dependencies]` is
/// explicitly not a dep table of any crate.
fn header_dep_context(header: &str) -> Option<(DepKind, Option<String>)> {
    let inner = header.strip_prefix('[')?.trim_end_matches(']');
    if inner.starts_with("workspace.") {
        return None;
    }
    let mut search = 0;
    while let Some(off) = inner[search..].find("dependencies") {
        let idx = search + off;
        let end = idx + "dependencies".len();
        // Component boundary before ("[dep…", ".dep…", "dev-dep…",
        // "build-dep…") and after (end of header or ".name").
        let before = &inner[..idx];
        let before_ok = before.is_empty()
            || before.ends_with('.')
            || before.ends_with("dev-")
            || before.ends_with("build-");
        let after_ok = end == inner.len() || inner[end..].starts_with('.');
        if before_ok && after_ok {
            let kind = if inner[..end].ends_with("dev-dependencies") {
                DepKind::Dev
            } else if inner[..end].ends_with("build-dependencies") {
                DepKind::Build
            } else {
                DepKind::Normal
            };
            let name = inner[end..]
                .strip_prefix('.')
                .map(|n| n.trim_matches('"').trim_matches('\'').to_string())
                .filter(|n| !n.is_empty());
            return Some((kind, name));
        }
        search = end;
    }
    None
}

// ── boundary-gate's parsers (predate the layer-gate; kept as-is) ─────────────

/// Parse root `Cargo.toml`'s `[workspace.dependencies]` for `name = { path = … }`
/// entries — the authoritative list of internal (in-repo) crates → their dirs.
pub fn workspace_internal_crates(root: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(root.join("Cargo.toml"))
        .map(|m| parse_workspace_internal_crates(&m))
        .unwrap_or_default()
}

pub fn parse_workspace_internal_crates(manifest: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut in_section = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_section = t == "[workspace.dependencies]";
            continue;
        }
        if !in_section || t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((name, rest)) = t.split_once('=') else {
            continue;
        };
        if let Some(pidx) = rest.find("path") {
            let after = &rest[pidx..];
            if let Some(q1) = after.find('"') {
                let after = &after[q1 + 1..];
                if let Some(q2) = after.find('"') {
                    out.insert(name.trim().to_string(), after[..q2].to_string());
                }
            }
        }
    }
    out
}

/// The internal (in-repo) crate names a crate depends on, across every
/// `…dependencies` table (normal / dev / build / target-specific). Dev and build
/// deps count: a crate a third party lifts carries its tests and build scripts.
pub fn cargo_internal_deps(
    cargo_toml: &Path,
    internal: &HashMap<String, String>,
) -> BTreeSet<String> {
    std::fs::read_to_string(cargo_toml)
        .map(|t| parse_cargo_internal_deps(&t, internal))
        .unwrap_or_default()
}

pub fn parse_cargo_internal_deps(
    text: &str,
    internal: &HashMap<String, String>,
) -> BTreeSet<String> {
    deps_with_kinds(text)
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| internal.contains_key(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_workspace_internal_crates() {
        let manifest = "\
[workspace]\n\
members = [\"a\"]\n\
[workspace.dependencies]\n\
oicp-types = { path = \"oicp-types\" }\n\
sovereign-contracts = { path = \"sovereign/crates/sovereign-contracts\" }\n\
# external — no path, must be skipped\n\
serde = { version = \"1\", features = [\"derive\"] }\n\
[workspace.lints.clippy]\n\
too_many_arguments = \"allow\"\n";
        let map = parse_workspace_internal_crates(manifest);
        assert_eq!(
            map.get("oicp-types").map(String::as_str),
            Some("oicp-types")
        );
        assert_eq!(
            map.get("sovereign-contracts").map(String::as_str),
            Some("sovereign/crates/sovereign-contracts")
        );
        // The external dep and the lint key are not internal crates.
        assert!(!map.contains_key("serde"));
        assert!(!map.contains_key("too_many_arguments"));
    }

    #[test]
    fn package_name_comes_from_package_section_only() {
        let manifest = "\
[package]\n\
name = \"sovereign-desktop\"\n\
version = \"0.1.0\"\n\
[[bin]]\n\
name = \"not-this-one\"\n";
        assert_eq!(
            parse_package_name(manifest).as_deref(),
            Some("sovereign-desktop")
        );
    }

    #[test]
    fn dep_kinds_classify_by_section_suffix() {
        let manifest = "\
[dependencies]\n\
sovereign-contracts = { workspace = true }\n\
serde = { workspace = true }\n\
[dev-dependencies]\n\
tempfile = { workspace = true }\n\
[build-dependencies]\n\
prost-build = \"0.13\"\n\
[target.'cfg(windows)'.dependencies]\n\
winapi = \"0.3\"\n\
[features]\n\
extra = [\"dep:serde\"]\n";
        let deps = deps_with_kinds(manifest);
        assert!(deps.contains(&("sovereign-contracts".into(), DepKind::Normal)));
        assert!(deps.contains(&("tempfile".into(), DepKind::Dev)));
        assert!(deps.contains(&("prost-build".into(), DepKind::Build)));
        assert!(deps.contains(&("winapi".into(), DepKind::Normal)));
        // [features] table entries are not deps.
        assert!(!deps.iter().any(|(n, _)| n == "extra"));
    }

    #[test]
    fn table_style_deps_cannot_bypass_the_gate() {
        // The evasion shape the live negative test injects: a dep declared
        // as its own table instead of a line in [dependencies].
        let manifest = "\
[package]\n\
name = \"commonwealth-core\"\n\
[dependencies.sovereign-core]\n\
workspace = true\n\
[dev-dependencies.tempfile]\n\
version = \"3\"\n\
[target.'cfg(windows)'.dependencies.winapi]\n\
version = \"0.3\"\n";
        let deps = deps_with_kinds(manifest);
        assert!(deps.contains(&("sovereign-core".into(), DepKind::Normal)));
        assert!(deps.contains(&("tempfile".into(), DepKind::Dev)));
        assert!(deps.contains(&("winapi".into(), DepKind::Normal)));
        // The table's OWN keys (workspace/version) must not read as deps.
        assert!(!deps.iter().any(|(n, _)| n == "workspace" || n == "version"));
        // Root-manifest workspace.dependencies is nobody's dep table.
        let root = "[workspace.dependencies]\nserde = \"1\"\n";
        assert!(deps_with_kinds(root).is_empty());
        // A crate whose NAME contains "dependencies" parses as the dep name,
        // not as a section.
        let odd = "[dependencies.my-dependencies]\nversion = \"1\"\n";
        let deps = deps_with_kinds(odd);
        assert_eq!(deps, vec![("my-dependencies".into(), DepKind::Normal)]);
    }

    #[test]
    fn extracts_internal_deps_across_sections() {
        let internal: HashMap<String, String> = [
            ("sovereign-contracts", "x"),
            ("sovereign-core", "y"),
            ("oicp-types", "z"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let cargo = "\
[package]\n\
name = \"sovereign-tools-base\"\n\
[dependencies]\n\
sovereign-contracts = { workspace = true }\n\
serde = { workspace = true }\n\
[build-dependencies]\n\
sovereign-core = { workspace = true }\n\
[dev-dependencies]\n\
tempfile = { workspace = true }\n";
        let deps = parse_cargo_internal_deps(cargo, &internal);
        // Only the in-repo crates are picked up (serde/tempfile are external).
        assert!(deps.contains("sovereign-contracts"));
        assert!(deps.contains("sovereign-core"));
        assert!(!deps.contains("serde"));
        assert!(!deps.contains("tempfile"));
    }
}
