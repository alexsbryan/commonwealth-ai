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
/// classify by their suffix. Optional deps count as edges: an edge behind a
/// feature is still a declared edge, and the layer map governs the
/// all-features union. They are also FLAGGED (`DepEdge::optional`), because
/// the `backstage` rule — alone among the map's rules — asks a question only
/// the default build can answer. Internal renames
/// (`foo = { package = "bar", … }`) are not handled — none exist here.
pub fn internal_dep_edges(root: &Path, members: &[MemberCrate]) -> Vec<DepEdge> {
    let names: BTreeSet<&str> = members.iter().map(|m| m.name.as_str()).collect();
    let mut edges = Vec::new();
    for m in members {
        let text = match std::fs::read_to_string(root.join(&m.dir).join("Cargo.toml")) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let absent = deps_absent_from_default_build(&text);
        for (dep, kind) in deps_with_kinds(&text) {
            if names.contains(dep.as_str()) && dep != m.name {
                edges.push(DepEdge {
                    from: m.name.clone(),
                    optional: absent.contains(&dep),
                    to: dep,
                    kind,
                });
            }
        }
    }
    edges
}

/// Dependency names this manifest declares `optional = true` AND does not
/// switch on from `default` — i.e. the crate's default build does not carry
/// them. This is the mechanical form of "does the product ship without it?".
///
/// Conservative by construction: anything it cannot resolve is reported as
/// PRESENT in the default build, so an unparsed shape produces a gate failure
/// to look at rather than a silent pass (ARCH §18.3).
pub fn deps_absent_from_default_build(manifest: &str) -> BTreeSet<String> {
    let optional = optional_dep_names(manifest);
    if optional.is_empty() {
        return BTreeSet::new();
    }
    let features = feature_table(manifest);

    // Transitive closure of `default` over the feature table.
    let mut enabled: BTreeSet<String> = BTreeSet::new();
    let mut queue: Vec<String> = vec!["default".to_string()];
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while let Some(f) = queue.pop() {
        if !seen.insert(f.clone()) {
            continue;
        }
        let Some(entries) = features.get(&f) else {
            // Not a feature name. A bare entry matching an optional dep is
            // Cargo's implicit feature for that dep.
            if optional.contains(&f) {
                enabled.insert(f);
            }
            continue;
        };
        for e in entries {
            if let Some(dep) = e.strip_prefix("dep:") {
                enabled.insert(dep.to_string());
            } else if let Some((head, _)) = e.split_once('/') {
                // `dep/feat` enables `dep`; `dep?/feat` explicitly does not.
                if let Some(weak) = head.strip_suffix('?') {
                    let _ = weak;
                } else {
                    enabled.insert(head.to_string());
                }
            } else {
                queue.push(e.clone());
            }
        }
    }
    optional.difference(&enabled).cloned().collect()
}

/// Dep names declared `optional = true`, in either manifest style.
fn optional_dep_names(manifest: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut table_dep: Option<String> = None;
    let mut in_deps = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            table_dep = None;
            in_deps = false;
            if let Some((_, inline)) = header_dep_context(t) {
                in_deps = true;
                table_dep = inline;
            }
            continue;
        }
        if !in_deps || t.is_empty() || t.starts_with('#') {
            continue;
        }
        // `[dependencies.foo]` table style: `optional = true` on its own line.
        if let Some(name) = &table_dep {
            if strip_comment(t).replace(' ', "") == "optional=true" {
                out.insert(name.clone());
            }
            continue;
        }
        // Section style: `foo = { …, optional = true }` on one line.
        let body = strip_comment(t);
        let Some((name, rest)) = body.split_once('=') else {
            continue;
        };
        if rest.replace(' ', "").contains("optional=true") {
            let name = name.trim().trim_matches('"');
            if !name.is_empty() {
                out.insert(name.to_string());
            }
        }
    }
    out
}

/// The `[features]` table as `name -> entries`. Handles the single-line
/// (`f = ["a", "b"]`) and multi-line array shapes this workspace uses.
fn feature_table(manifest: &str) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut in_features = false;
    let mut open: Option<(String, Vec<String>)> = None;
    for line in manifest.lines() {
        let t = line.trim();
        if open.is_none() && t.starts_with('[') {
            in_features = t == "[features]";
            continue;
        }
        if !in_features {
            continue;
        }
        let body = strip_comment(t);
        if let Some((name, items)) = open.as_mut() {
            items.extend(quoted(&body));
            if body.contains(']') {
                out.insert(name.clone(), std::mem::take(items));
                open = None;
            }
            continue;
        }
        if body.is_empty() {
            continue;
        }
        let Some((name, rest)) = body.split_once('=') else {
            continue;
        };
        let name = name.trim().trim_matches('"').to_string();
        if name.is_empty() {
            continue;
        }
        let items = quoted(rest);
        if rest.contains(']') {
            out.insert(name, items);
        } else {
            open = Some((name, items));
        }
    }
    // An unterminated array still contributes what it had — dropping it would
    // under-report enablement, which is the unsafe direction.
    if let Some((name, items)) = open {
        out.insert(name, items);
    }
    out
}

fn strip_comment(line: &str) -> String {
    match line.find('#') {
        Some(i) => line[..i].trim().to_string(),
        None => line.trim().to_string(),
    }
}

fn quoted(s: &str) -> Vec<String> {
    s.split('"').skip(1).step_by(2).map(String::from).collect()
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

    // ── "does the product ship without it?", read off the manifest ────────

    #[test]
    fn optional_dep_not_reachable_from_default_is_absent() {
        let m = r#"
[package]
name = "host"

[dependencies]
sovereign-core = { workspace = true }
sovereign-agent-bench = { workspace = true, optional = true }

[features]
default = []
dev-tools = ["dep:sovereign-agent-bench"]
"#;
        let absent = deps_absent_from_default_build(m);
        assert!(absent.contains("sovereign-agent-bench"));
        assert!(!absent.contains("sovereign-core"), "non-optional is always present");
    }

    #[test]
    fn optional_dep_switched_on_by_default_is_present() {
        let m = r#"
[package]
name = "host"

[dependencies]
sovereign-eval = { workspace = true, optional = true }

[features]
default = ["bench"]
bench = ["dep:sovereign-eval"]
"#;
        assert!(
            deps_absent_from_default_build(m).is_empty(),
            "default turns it on, so the product does NOT ship without it"
        );
    }

    #[test]
    fn default_closure_is_transitive_and_multiline() {
        // The real sovereign-cli shape: a multi-line `default` array whose
        // entries are themselves features.
        let m = r#"
[package]
name = "host"

[dependencies]
a-crate = { workspace = true, optional = true }
b-crate = { workspace = true, optional = true }

[features]
default = [
    "mid",       # a comment mid-array
    "b-crate/some-feat",
]
mid = ["deep"]
deep = ["dep:a-crate"]
"#;
        let absent = deps_absent_from_default_build(m);
        assert!(!absent.contains("a-crate"), "reached via default -> mid -> deep");
        assert!(!absent.contains("b-crate"), "`b-crate/feat` enables b-crate");
    }

    #[test]
    fn weak_dep_feature_does_not_enable_the_dep() {
        let m = r#"
[package]
name = "host"

[dependencies]
a-crate = { workspace = true, optional = true }

[features]
default = ["a-crate?/some-feat"]
"#;
        assert!(
            deps_absent_from_default_build(m).contains("a-crate"),
            "`dep?/feat` is the weak form — it must not turn the dep on"
        );
    }

    #[test]
    fn bare_entry_naming_an_optional_dep_is_cargos_implicit_feature() {
        let m = r#"
[package]
name = "host"

[dependencies]
a-crate = { workspace = true, optional = true }

[features]
default = ["a-crate"]
"#;
        assert!(deps_absent_from_default_build(m).is_empty());
    }

    #[test]
    fn table_style_optional_is_seen_too() {
        // Unused in this workspace, but a gate a `[dependencies.foo]` table
        // slips past is trivially bypassable.
        let m = r#"
[package]
name = "host"

[dependencies.sovereign-eval]
workspace = true
optional = true

[features]
default = []
"#;
        assert!(deps_absent_from_default_build(m).contains("sovereign-eval"));
    }

    #[test]
    fn a_crate_with_no_optional_deps_reports_nothing_absent() {
        let m = r#"
[package]
name = "host"

[dependencies]
sovereign-core = { workspace = true }
"#;
        assert!(deps_absent_from_default_build(m).is_empty());
    }
}
