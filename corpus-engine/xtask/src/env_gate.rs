// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cargo xtask env-gate` — the env-knob plane obeys `quality/env-flags.toml`.
//!
//! The registry is DECLARATION, not plumbing: `std::env::var` read sites stay
//! exactly where they are. This gate censuses them — Rust literals
//! (`env::var`/`var_os`/`set_var`/`remove_var`), the `svrnmesh_env` suffix
//! wrapper, `EnvFlag { name: … }` table literals, and `${SOVEREIGN_*}`
//! expansions/assignments in `scripts/` + `.claude/hooks/` — and diffs the
//! observed names against the declared map.
//! A NEW name that is neither registered, third-party-allowlisted, nor in the
//! legacy baseline fails the gate with the exact fix; the pre-existing debt
//! rides the shrink-only baseline (`quality/baselines/env_unregistered.txt`,
//! `--update-baseline` / `--tighten` — the uniform ratchet contract in
//! `common.rs`).
//!
//! A `removed` row is a var nothing reads any more. The gate fails if one is
//! still read, and pins `kernel_types::env_bridge::REMOVED_ENV` (the table
//! `promote_legacy_env` warns from at startup) to the removed rows, name and
//! `replacement` text both, so the warning a user sees is the registry's.
//!
//! `docs/ENV_FLAGS.md` is rendered from the registry and freshness-checked
//! here (regenerate: `--update-doc`) — same generated-doc contract as
//! `sovereign-core/docs/retrieval-pipeline.md`, with xtask as the renderer because
//! the source of truth is repo data, not crate code.
//!
//! `SVRNMESH_*` observations canonicalize to `SOVEREIGN_*`: the rebrand
//! mirror (`sovereign-contracts/src/rebrand.rs::promote_legacy_env`) makes
//! them one name. This census is also the rebrand bridge's deletion
//! predicate: the bridge can be dropped once zero sites read the legacy
//! prefix only.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::common;

const REGISTRY_PATH: &str = "quality/env-flags.toml";
const BASELINE_FILE: &str = "env_unregistered.txt";
const DOC_PATH: &str = "docs/ENV_FLAGS.md";
const VALID_STATUSES: [&str; 5] = ["guard", "shipped", "experiment", "deprecated", "removed"];
const REMOVED: &str = "removed";

pub fn run(args: &[String]) -> i32 {
    let flags = common::baseline_flags(args);
    let update_doc = args.iter().any(|a| a == "--update-doc");
    let root = common::repo_root();

    let registry = match load_registry(&root.join(REGISTRY_PATH)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("env-gate: {REGISTRY_PATH}: {e}");
            return 1;
        }
    };

    let scope = match common::SourceTree::discover(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("env-gate: cannot resolve this repo's source tree: {e}");
            return 1;
        }
    };

    let observed = census(&root, &scope);
    let baseline_path = common::baselines_dir(&root).join(BASELINE_FILE);
    let baseline = common::load_line_set(&baseline_path);

    let registered: BTreeSet<&str> = registry.flags.iter().map(|f| f.name.as_str()).collect();
    let unregistered: BTreeMap<&String, &Vec<String>> = observed
        .iter()
        .filter(|(name, _)| {
            !registered.contains(name.as_str()) && !registry.allowlist.contains(*name)
        })
        .collect();

    // ── Ratchet maintenance ─────────────────────────────────────────
    if flags.update {
        let set: BTreeSet<String> = unregistered.keys().map(|s| (*s).clone()).collect();
        if let Err(e) = common::write_line_set(
            &baseline_path,
            "env-gate",
            "observed env-var names not yet declared in quality/env-flags.toml",
            &set,
        ) {
            eprintln!("env-gate: {e}");
            return 1;
        }
        eprintln!(
            "env-gate: baseline updated — {} unregistered name(s) accepted as legacy debt",
            set.len()
        );
        return 0;
    }
    if flags.tighten {
        let kept: BTreeSet<String> = baseline
            .iter()
            .filter(|n| unregistered.contains_key(n))
            .cloned()
            .collect();
        let dropped = baseline.len() - kept.len();
        if let Err(e) = common::write_line_set(
            &baseline_path,
            "env-gate",
            "observed env-var names not yet declared in quality/env-flags.toml",
            &kept,
        ) {
            eprintln!("env-gate: {e}");
            return 1;
        }
        eprintln!(
            "env-gate: tightened — {dropped} name(s) left the baseline (registered or no longer read), {} remain",
            kept.len()
        );
        return 0;
    }

    // ── Generated doc ───────────────────────────────────────────────
    let doc_path = root.join(DOC_PATH);
    let rendered = render_doc(&registry);
    if update_doc {
        if let Err(e) = std::fs::write(&doc_path, &rendered) {
            eprintln!("env-gate: write {DOC_PATH}: {e}");
            return 1;
        }
        eprintln!("env-gate: wrote {DOC_PATH}");
        // fall through: still run the census checks below
    }

    // ── Checks ──────────────────────────────────────────────────────
    let mut failures = 0usize;

    let new_names: Vec<(&&String, &&Vec<String>)> = unregistered
        .iter()
        .filter(|(name, _)| !baseline.contains(**name))
        .collect();
    for (name, sites) in &new_names {
        failures += 1;
        eprintln!("env-gate: NEW unregistered env var `{name}`:");
        for site in sites.iter().take(3) {
            eprintln!("    {site}");
        }
        if sites.len() > 3 {
            eprintln!("    … and {} more site(s)", sites.len() - 3);
        }
    }
    if !new_names.is_empty() {
        eprintln!(
            "  Declare each in {REGISTRY_PATH} (a [[flag]] entry with cluster/default/purpose/status,\n  \
             or `third_party_allowlist` for vars owned by other software). To accept as legacy debt instead:\n  \
             cargo run -p xtask -- env-gate --update-baseline"
        );
    }

    let undeclared = flag_literals(&root, &scope)
        .into_iter()
        .filter(|(name, _)| !registered.contains(name.as_str()))
        .collect::<BTreeMap<_, _>>();
    for (name, sites) in &undeclared {
        failures += 1;
        eprintln!("env-gate: `EnvFlag` literal `{name}` is not declared in {REGISTRY_PATH}:");
        for site in sites.iter().take(3) {
            eprintln!("    {site}");
        }
    }
    if !undeclared.is_empty() {
        eprintln!(
            "  A runtime flag table and the registry must not drift. Add a [[flag]] entry\n               (cluster/default/purpose/status) for each — the baseline does NOT cover these."
        );
    }

    for f in registry.flags.iter().filter(|f| f.status == REMOVED) {
        if let Some(sites) = observed.get(&f.name) {
            failures += 1;
            eprintln!(
                "env-gate: `{}` is declared removed but is still read:",
                f.name
            );
            for site in sites.iter().take(3) {
                eprintln!("    {site}");
            }
        }
    }

    let declared_removed: BTreeMap<&str, &str> = registry
        .flags
        .iter()
        .filter(|f| f.status == REMOVED)
        // load_registry refuses a removed row without a replacement.
        .filter_map(|f| f.replacement.as_deref().map(|r| (f.name.as_str(), r)))
        .collect();
    let table = removed_literals(&root, &scope);
    for drift in removed_table_drift(&declared_removed, &table) {
        failures += 1;
        eprintln!("env-gate: {drift}");
    }

    let committed = std::fs::read_to_string(&doc_path).unwrap_or_default();
    if committed != rendered {
        failures += 1;
        eprintln!(
            "env-gate: {DOC_PATH} is stale — the registry changed. Regenerate:\n  \
             cargo run -p xtask -- env-gate --update-doc"
        );
    }

    // ── Summary ─────────────────────────────────────────────────────
    let now_registered = baseline
        .iter()
        .filter(|n| !unregistered.contains_key(n))
        .count();
    eprintln!(
        "env-gate: {} observed name(s) · {} registered · {} allowlisted · {} riding the baseline \
         (censused over source only, {} non-source trees excluded)",
        observed.len(),
        registry.flags.len(),
        registry.allowlist.len(),
        baseline.len().saturating_sub(now_registered),
        scope.ignored_dir_count()
    );
    if now_registered > 0 {
        eprintln!(
            "env-gate: {now_registered} baseline name(s) are now registered or gone — bank the improvement:\n  \
             cargo run -p xtask -- env-gate --tighten"
        );
    }

    if failures == 0 {
        eprintln!("env-gate: OK");
        0
    } else {
        1
    }
}

// ─── Registry ───────────────────────────────────────────────────────

struct Registry {
    flags: Vec<FlagRow>,
    allowlist: BTreeSet<String>,
}

struct FlagRow {
    name: String,
    cluster: String,
    default: String,
    purpose: String,
    status: String,
    alias_of: Option<String>,
    shadows: Option<String>,
    /// Required on, and only on, a `removed` row: the successor to set, or
    /// why nothing replaces it. The startup warning prints it verbatim.
    replacement: Option<String>,
}

fn load_registry(path: &Path) -> Result<Registry, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read: {e}"))?;
    let value: toml::Value = text.parse().map_err(|e| format!("parse: {e}"))?;

    let allowlist: BTreeSet<String> = value
        .get("third_party_allowlist")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut flags = Vec::new();
    let mut seen = BTreeSet::new();
    let empty = Vec::new();
    let entries = value
        .get("flag")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    for (i, entry) in entries.iter().enumerate() {
        let req = |key: &str| -> Result<String, String> {
            entry
                .get(key)
                .and_then(|v| v.as_str())
                .map(String::from)
                .ok_or_else(|| format!("[[flag]] #{}: missing `{key}`", i + 1))
        };
        let row = FlagRow {
            name: req("name")?,
            cluster: req("cluster")?,
            default: req("default")?,
            purpose: req("purpose")?,
            status: req("status")?,
            alias_of: entry
                .get("alias_of")
                .and_then(|v| v.as_str())
                .map(String::from),
            shadows: entry
                .get("shadows")
                .and_then(|v| v.as_str())
                .map(String::from),
            replacement: entry
                .get("replacement")
                .and_then(|v| v.as_str())
                .map(String::from),
        };
        if !VALID_STATUSES.contains(&row.status.as_str()) {
            return Err(format!(
                "flag `{}`: status `{}` is not one of {VALID_STATUSES:?}",
                row.name, row.status
            ));
        }
        if (row.status == REMOVED) != row.replacement.is_some() {
            return Err(format!(
                "flag `{}`: `replacement` is required on a `removed` row and only there",
                row.name
            ));
        }
        if !seen.insert(row.name.clone()) {
            return Err(format!("flag `{}` declared twice", row.name));
        }
        flags.push(row);
    }
    // Alias targets must themselves be declared.
    for f in &flags {
        if let Some(target) = &f.alias_of {
            if !seen.contains(target) {
                return Err(format!(
                    "flag `{}`: alias_of `{target}` is not itself declared",
                    f.name
                ));
            }
        }
    }
    Ok(Registry { flags, allowlist })
}

// ─── Census ─────────────────────────────────────────────────────────

/// Observed canonical name -> `file:line` sites. `SVRNMESH_*` canonicalizes
/// to `SOVEREIGN_*` (one name, mirrored by the rebrand bridge).
// Static regex literals + guaranteed capture groups: a panic here is a
// programmer error in this file, not a runtime condition to handle.
#[allow(clippy::expect_used)]
fn census(root: &Path, scope: &common::SourceTree) -> BTreeMap<String, Vec<String>> {
    let rust_read =
        regex::Regex::new(r#"env::var(?:_os)?\s*\(\s*"((?:SOVEREIGN|SVRNMESH)_[A-Z0-9_]+)""#)
            .expect("rust_read regex");
    let rust_write =
        regex::Regex::new(r#"(?:set_var|remove_var)\s*\(\s*"((?:SOVEREIGN|SVRNMESH)_[A-Z0-9_]+)""#)
            .expect("rust_write regex");
    let rust_wrapper =
        regex::Regex::new(r#"svrnmesh_env\s*\(\s*"([A-Z0-9_]+)""#).expect("wrapper regex");
    let shell_read =
        regex::Regex::new(r"\$\{?((?:SOVEREIGN|SVRNMESH)_[A-Z0-9_]+)").expect("shell_read regex");
    let shell_write =
        regex::Regex::new(r"(?m)^\s*(?:export\s+)?((?:SOVEREIGN|SVRNMESH)_[A-Z0-9_]+)=")
            .expect("shell_write regex");

    let mut observed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut record = |name: &str, file: &Path, offset: usize, content: &str| {
        let canonical = format!(
            "SOVEREIGN_{}",
            name.trim_start_matches("SOVEREIGN_")
                .trim_start_matches("SVRNMESH_")
        );
        let line = content[..offset].matches('\n').count() + 1;
        let rel = file.strip_prefix(root).unwrap_or(file);
        observed
            .entry(canonical)
            .or_default()
            .push(format!("{}:{line}", rel.display()));
    };

    for file in rust_files(root, scope) {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for re in [&rust_read, &rust_write] {
            for cap in re.captures_iter(&content) {
                let m = cap.get(1).expect("group 1");
                record(m.as_str(), &file, m.start(), &content);
            }
        }
        for cap in rust_wrapper.captures_iter(&content) {
            let m = cap.get(1).expect("group 1");
            let name = format!("SOVEREIGN_{}", m.as_str());
            record(&name, &file, m.start(), &content);
        }
    }

    for file in shell_files(root, scope) {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for re in [&shell_read, &shell_write] {
            for cap in re.captures_iter(&content) {
                let m = cap.get(1).expect("group 1");
                record(m.as_str(), &file, m.start(), &content);
            }
        }
    }

    observed
}

/// Declared flag name -> `file:line` sites, from the `EnvFlag { name: "…" }`
/// literals the runtime flag tables are built out of.
///
/// This is the half `census` cannot see: `SOVEREIGN_GROUNDING_GATE_<SURFACE>`
/// is an override GRAMMAR, never an `env::var` literal, so a table row for it
/// could drift from the registry with every read site still declared. Lived in
/// two copy-pasted `flags_table_is_declared_in_env_registry` tests inside
/// sovereign-core until 2026-09-21; they read `quality/env-flags.toml` through
/// `CARGO_MANIFEST_DIR` and climbed out of the crate (`boundary-gate` rule 3c).
// Static regex literal + guaranteed capture group.
#[allow(clippy::expect_used)]
fn flag_literals(root: &Path, scope: &common::SourceTree) -> BTreeMap<String, Vec<String>> {
    let re = regex::Regex::new(r#"EnvFlag\s*\{[^}]*?name:\s*"([A-Z0-9_<>]+)""#)
        .expect("flag literal regex");
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in rust_files(root, scope) {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for cap in re.captures_iter(&content) {
            let m = cap.get(1).expect("group 1");
            let line = content[..m.start()].matches('\n').count() + 1;
            let rel = file.strip_prefix(root).unwrap_or(&file);
            out.entry(m.as_str().to_string())
                .or_default()
                .push(format!("{}:{line}", rel.display()));
        }
    }
    out
}

/// Every `.rs` file in THIS REPO'S SOURCE. Censusing a vendored dependency
/// tree or an agent worktree copy would report third-party (or duplicate)
/// env reads as unregistered knobs of ours — `scope` is the single decider
/// (`common::SourceTree`).
fn rust_files(root: &Path, scope: &common::SourceTree) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, root, scope, &mut |p| {
        if p.extension().is_some_and(|e| e == "rs") {
            out.push(p.to_path_buf());
        }
    });
    out
}

fn shell_files(root: &Path, scope: &common::SourceTree) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in [root.join("scripts"), root.join(".claude/hooks")] {
        walk(&dir, root, scope, &mut |p| {
            if p.extension().is_some_and(|e| e == "sh") {
                out.push(p.to_path_buf());
            }
        });
    }
    out
}

/// Removed name -> (replacement text, `file:line`) from the
/// `RemovedEnv { name: "…", instead: "…" }` literals of the runtime table.
// Static regex literal + guaranteed capture groups.
#[allow(clippy::expect_used)]
fn removed_literals(root: &Path, scope: &common::SourceTree) -> BTreeMap<String, (String, String)> {
    let re = regex::Regex::new(
        r#"RemovedEnv\s*\{\s*name:\s*"([A-Z0-9_]+)",\s*instead:\s*"((?:[^"\\]|\\.)*)""#,
    )
    .expect("removed literal regex");
    let mut out = BTreeMap::new();
    for file in rust_files(root, scope) {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for cap in re.captures_iter(&content) {
            let m = cap.get(1).expect("group 1");
            let line = content[..m.start()].matches('\n').count() + 1;
            let rel = file.strip_prefix(root).unwrap_or(&file);
            out.insert(
                m.as_str().to_string(),
                (
                    cap.get(2).expect("group 2").as_str().to_string(),
                    format!("{}:{line}", rel.display()),
                ),
            );
        }
    }
    out
}

/// Every way the runtime warning table and the registry's `removed` rows
/// disagree: a removed row the table lacks (a user setting it hears nothing),
/// a table entry the registry does not declare removed, or replacement text
/// that differs (the user would read words the registry does not hold).
fn removed_table_drift(
    declared: &BTreeMap<&str, &str>,
    table: &BTreeMap<String, (String, String)>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (name, replacement) in declared {
        match table.get(*name) {
            None => out.push(format!(
                "`{name}` is declared removed but REMOVED_ENV does not warn on it \
                 (add a RemovedEnv row in kernel-types/src/env_bridge.rs)"
            )),
            Some((text, site)) if text != replacement => out.push(format!(
                "`{name}`: REMOVED_ENV's text at {site} differs from the registry's `replacement`"
            )),
            Some(_) => {}
        }
    }
    for (name, (_, site)) in table {
        if !declared.contains_key(name.as_str()) {
            out.push(format!(
                "REMOVED_ENV warns on `{name}` ({site}) but the registry does not declare it removed"
            ));
        }
    }
    out
}

fn walk(dir: &Path, root: &Path, scope: &common::SourceTree, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if scope.excludes_dir(&common::rel_path(&path, root)) {
                continue;
            }
            walk(&path, root, scope, f);
        } else {
            f(&path);
        }
    }
}

// ─── Generated doc ──────────────────────────────────────────────────

fn render_doc(registry: &Registry) -> String {
    let mut md = String::from(
        "<!-- GENERATED FILE — do not edit by hand.\n\
         \x20    Source: quality/env-flags.toml (the declared env-knob registry)\n\
         \x20    Regenerate: cargo run -p xtask -- env-gate --update-doc -->\n\
         \n\
         # Environment-variable knobs — the declared registry\n\
         \n\
         One row per declared knob, grouped by subsystem cluster. Names use the\n\
         canonical `SOVEREIGN_` prefix; every one is mirrored to `SVRNMESH_*` by\n\
         the rebrand bridge (`sovereign-contracts/src/rebrand.rs`), so both\n\
         spellings work. `status` legend: **guard** = safety/kill-switch, keep;\n\
         **shipped** = default-on product behavior; **experiment** = A/B lever,\n\
         default-off unless noted; **deprecated** = scheduled for removal;\n\
         **removed** = nothing reads it, and setting it prints a startup warning\n\
         naming its replacement.\n\
         \n\
         The registry is enforced by `cargo run -p xtask -- env-gate`: a NEW env\n\
         var read anywhere in the workspace must be declared here (or in the\n\
         gate's third-party allowlist); pre-registry debt rides the shrink-only\n\
         baseline `quality/baselines/env_unregistered.txt`.\n",
    );

    let mut by_cluster: BTreeMap<&str, Vec<&FlagRow>> = BTreeMap::new();
    for f in &registry.flags {
        by_cluster.entry(f.cluster.as_str()).or_default().push(f);
    }
    for (cluster, mut rows) in by_cluster {
        rows.sort_by_key(|f| f.name.as_str());
        md.push_str(&format!(
            "\n## {cluster}\n\n| flag | default | status | purpose |\n|---|---|---|---|\n"
        ));
        for f in rows {
            let mut purpose = f.purpose.clone();
            if let Some(t) = &f.alias_of {
                purpose.push_str(&format!(" *(alias of `{t}`)*"));
            }
            if let Some(s) = &f.shadows {
                purpose.push_str(&format!(" *(shadows `SetupConfig.{s}`)*"));
            }
            if let Some(r) = &f.replacement {
                purpose.push_str(&format!(" **Instead:** {r}"));
            }
            md.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                f.name, f.default, f.status, purpose
            ));
        }
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &[(&str, &str)]) -> BTreeMap<String, (String, String)> {
        rows.iter()
            .map(|(n, t)| (n.to_string(), (t.to_string(), "env_bridge.rs:1".into())))
            .collect()
    }

    #[test]
    fn removed_table_agrees_with_the_registry_or_names_each_drift() {
        let declared: BTreeMap<&str, &str> = [("SOVEREIGN_A", "use B"), ("SOVEREIGN_C", "retired")]
            .into_iter()
            .collect();
        assert!(removed_table_drift(
            &declared,
            &table(&[("SOVEREIGN_A", "use B"), ("SOVEREIGN_C", "retired")])
        )
        .is_empty());

        let drift = removed_table_drift(
            &declared,
            &table(&[("SOVEREIGN_A", "use Z"), ("SOVEREIGN_D", "retired")]),
        );
        assert_eq!(drift.len(), 3, "{drift:?}");
        assert!(drift[0].contains("`SOVEREIGN_A`") && drift[0].contains("differs"));
        assert!(drift[1].contains("`SOVEREIGN_C`") && drift[1].contains("does not warn"));
        assert!(drift[2].contains("`SOVEREIGN_D`") && drift[2].contains("does not declare"));
    }
}
