// SPDX-License-Identifier: AGPL-3.0-or-later
//! boundary-gate — enforces the extractable-PACKAGE boundaries declared in
//! `quality/ARCH_LAYERS.toml`: a package crate may reach only its own package
//! plus the shared `[[package_leaf]]` set, and each leaf has a hand-pinned
//! budget of its own. Kept green, each package stays liftable out of the
//! monorepo against just the OICP contract.
//!
//! Until 2026-09-03 the boundary lived in Rust consts here (`PACKAGE_SET` /
//! `SHARED_LEAVES` / `allowed_leaf_deps`) and could describe exactly one
//! package — the studio one. It is policy, so it moved to the policy file
//! beside the layer map, behind the same parser (`quality/arch-layers`) that
//! layer-gate and the code-intel `arch_report` already share. That is ARCH
//! §10.6 (one decider, one name) and it is what makes N packages a TOML edit
//! rather than a code change.
//!
//! Overlap with layer-gate is deliberate and partial: layer-gate governs
//! DIRECTION for the whole workspace; this gate pins an exact allowlist per
//! declared package, and adds the three rules a dependency edge cannot express
//! — no `build.rs`, no crate-escaping `include_str!`, and no RUNTIME reach-out
//! past the crate root.
//!
//! The third one is younger than the other two and exists because they were
//! not enough. Both are COMPILE-TIME rules, and a test that resolves the repo
//! root from `CARGO_MANIFEST_DIR` at runtime — or shells `git` wherever the
//! harness happens to stand — is neither a build script nor an `include_str!`.
//! The commonwealth lift of 2026-09-04 priced the gap: `kernel-types` came
//! back 490 passed / 2 failed, both failures leaf-side, both invisible here,
//! and a green gate the whole time. See [`scan_runtime_escapes`].
//!
//! WHAT THIS GATE CANNOT SEE. Its unit is the CRATE. Where package-shaped code
//! shares a crate with everything else — `sovereign-core`, `sovereign-tools`,
//! `sovereign-cli-llm`, `sovereign-mesh`, which together are ~41% of the
//! workspace — declaring a package says nothing, because there is no edge to
//! check. Containment there is a module rule enforced by a test in the crate
//! itself; `sovereign-cli-llm/src/lib.rs`'s
//! `bench_cmd_is_the_only_module_naming_the_eval_harness` is the worked
//! example, and it is honest about being strictly weaker: Cargo still links
//! the crate either way.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::common;
use crate::manifests;

pub fn run() -> i32 {
    let root = common::repo_root();
    let map_path = root.join("quality/ARCH_LAYERS.toml");

    let map_text = match std::fs::read_to_string(&map_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "error: cannot read {} ({e}).\n  The package boundaries are declared there \
                 — see studio/BOUNDARY.md and docs/CODE_TOOLING_BOUNDARY.md.",
                map_path.display()
            );
            return 1;
        }
    };
    let map = match arch_layers::parse(&map_text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    let members = manifests::workspace_members(&root);
    let names: BTreeSet<String> = members.iter().map(|m| m.name.clone()).collect();
    let dir_of: BTreeMap<&str, &str> = members
        .iter()
        .map(|m| (m.name.as_str(), m.dir.as_str()))
        .collect();
    let edges = manifests::internal_dep_edges(&root, &members);

    let mut fails: Vec<String> = Vec::new();

    // Rules 1/2 — the dependency closure. Evaluated by the shared parser so
    // this gate and arch-report cannot drift on what a package MEANS.
    let dep_fails: Vec<String> = arch_layers::evaluate_packages(&map, &edges)
        .iter()
        .map(|v| v.describe())
        .collect();
    fails.extend(dep_fails.iter().cloned());

    // Rules 3a/3b — the filesystem half, which no manifest can express.
    let mut checked = 0usize;
    for (scope, name) in governed_crates(&map) {
        let Some(rel) = dir_of.get(name) else {
            // Declared but not present yet — reported below, not here. A
            // package is declared BEFORE its extraction finishes.
            continue;
        };
        checked += 1;
        let dir = root.join(rel);

        // A build script is a source-tree reach-in no package boundary
        // survives (see studio B:P0's syn-walk removal).
        if dir.join("build.rs").exists() {
            fails.push(format!(
                "[{scope}] {name}: has a build.rs — package and leaf crates must not \
                 carry one; a third party lifting this crate carries its build scripts"
            ));
        }
        include_escapes(&dir, name, scope, &mut fails);
        runtime_root_escapes(&dir, name, scope, &mut fails);
    }

    // Declared-but-absent crates. Reported rather than skipped in silence:
    // the same shape covers a typo, and a typo'd crate name is a rule that
    // quietly governs nothing (ARCH §18.3 — absence is reported, never
    // defaulted).
    let missing = arch_layers::missing_package_crates(&map, &names);

    // ── Report ────────────────────────────────────────────────────────────────
    eprintln!(
        "boundary-gate: {} package(s) + {} shared leaves, {checked} crate(s) checked \
         (dep closure incl. dev+build edges, build.rs, include_str escapes, \
         runtime root escapes)",
        map.packages.len(),
        map.package_leaves.len()
    );
    for pkg in &map.packages {
        let present = pkg.crates.iter().filter(|c| names.contains(*c)).count();
        eprintln!(
            "  {:<14} {present}/{} crates present   {}",
            pkg.name,
            pkg.crates.len(),
            pkg.doc
        );
    }
    let leaves_present = map
        .package_leaves
        .iter()
        .filter(|l| names.contains(&l.name))
        .count();
    eprintln!(
        "  {:<14} {leaves_present}/{} present",
        arch_layers::SHARED_LEAVES_SCOPE,
        map.package_leaves.len()
    );

    for (scope, name) in &missing {
        eprintln!("  ! [{scope}] {name}: declared but not a workspace member (yet?)");
    }
    for f in &fails {
        eprintln!("  ✗ {f}");
    }

    // The closure, once per offending package. A newly declared package can
    // print a hundred-plus edges, and repeating this on every line buries
    // them; naming it zero times leaves the reader diffing the TOML by hand.
    // Keyed on the DEPENDENCY fails alone: rules 3a-3c are filesystem rules,
    // and printing a package's dependency closure under "this file shells git"
    // sends the reader to diff a TOML that has nothing to do with it.
    if !dep_fails.is_empty() {
        let leaves: Vec<&str> = map.package_leaves.iter().map(|l| l.name.as_str()).collect();
        for pkg in &map.packages {
            if dep_fails
                .iter()
                .any(|f| f.starts_with(&format!("[{}]", pkg.name)))
            {
                eprintln!(
                    "\n  closure for [{}]: {} + shared leaves ({})",
                    pkg.name,
                    pkg.crates.join(", "),
                    leaves.join(", ")
                );
            }
        }
    }

    if fails.is_empty() {
        eprintln!("  ✓ every declared package reaches only itself + the shared leaves");
        0
    } else {
        eprintln!();
        eprintln!(
            "boundary-gate FAILED ({} violation(s)). A package must stay liftable against \
             only the shared leaves. Either move the code that needs the offending \
             dependency outside the package and inject it through a trait at the call \
             site, or — if the boundary is real but not yet clean — grandfather the edge \
             with an [[exception]] carrying `package = \"<name>\"` and a reason in \
             quality/ARCH_LAYERS.toml. Widening a [[package_leaf]] budget widens EVERY \
             package at once; do that deliberately, with the leaf's comment updated.",
            fails.len()
        );
        1
    }
}

/// Every crate the package rules govern, as `(scope, crate)` — package members
/// under their package's name, shared leaves under the pseudo-scope.
fn governed_crates(map: &arch_layers::LayerMap) -> Vec<(&str, &str)> {
    let mut out: Vec<(&str, &str)> = Vec::new();
    for pkg in &map.packages {
        for c in &pkg.crates {
            out.push((pkg.name.as_str(), c.as_str()));
        }
    }
    for leaf in &map.package_leaves {
        out.push((arch_layers::SHARED_LEAVES_SCOPE, leaf.name.as_str()));
    }
    out
}

/// Flag `include_str!` / `include_bytes!` literals that escape the crate root
/// (climb two+ levels) unless they target the checked-in `sovereign-recipes/`
/// tree. Grep-level (per-line), recursing the crate's `src/`.
///
/// The `sovereign-recipes/` carve-out is the one recorded exception, and it is
/// worth knowing that it is not free: when the studio closure was actually
/// lifted to a sandbox (2026-07-21) it built in 36 seconds with zero source
/// edits, but had to preserve the monorepo's directory shape to compile —
/// because of exactly this embed. A green gate is not a proven lift.
fn include_escapes(dir: &Path, crate_name: &str, scope: &str, fails: &mut Vec<String>) {
    fn walk(dir: &Path, crate_name: &str, scope: &str, fails: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, crate_name, scope, fails);
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for line in text.lines() {
                    let t = line.trim();
                    if !(t.contains("include_str!") || t.contains("include_bytes!")) {
                        continue;
                    }
                    // Escapes the crate root iff it climbs two+ levels.
                    if t.contains("../..") && !t.contains("sovereign-recipes") {
                        fails.push(format!(
                            "[{scope}] {crate_name}: {} embeds a file outside the crate root \
                             and outside sovereign-recipes/: `{t}`",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ));
                    }
                }
            }
        }
    }
    walk(&dir.join("src"), crate_name, scope, fails);
}

/// Rule 3c — RUNTIME reach-outs past the crate root, in `src/`, `tests/`,
/// `benches/` and `examples/`.
///
/// It cannot stop at `src/` the way rule 3b does: `quality/ARCH_LAYERS.toml`
/// says "a third party who lifts the package carries its tests", and every
/// instance of this defect found so far has been in test code.
fn runtime_root_escapes(dir: &Path, crate_name: &str, scope: &str, fails: &mut Vec<String>) {
    for sub in ["src", "tests", "benches", "examples"] {
        let mut files = Vec::new();
        rs_files(&dir.join(sub), &mut files);
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            for e in scan_runtime_escapes(&text) {
                fails.push(format!(
                    "[{scope}] {crate_name}: {sub}/{name}:{} {}",
                    e.line,
                    e.describe()
                ));
            }
        }
    }
}

/// Every `.rs` file under `dir`, recursively. A missing directory is empty,
/// not an error — most crates have no `benches/`.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(path);
        }
    }
}

/// One runtime escape, with the line that proves it.
struct RuntimeEscape {
    line: usize,
    kind: EscapeKind,
    evidence: String,
}

enum EscapeKind {
    /// A path derived from `CARGO_MANIFEST_DIR` that then climbs OUT of the
    /// crate.
    ManifestClimb,
    /// A `git` subprocess with no `current_dir(…)`, so it runs wherever the
    /// harness happens to stand.
    AmbientGit,
}

impl RuntimeEscape {
    /// The fix goes in the message, same as rule 3a's: a violation a reader
    /// has to go look up is a violation that gets grandfathered.
    fn describe(&self) -> String {
        match self.kind {
            EscapeKind::ManifestClimb => format!(
                "derives a path from CARGO_MANIFEST_DIR and climbs out of the crate root at \
                 RUNTIME (`{}`) — a third party who lifts this package carries its tests and \
                 has none of that tree. Move the check to a crate in NO package (`xtask`, or \
                 whichever crate owns the data it reads); do not teach it to skip when the \
                 files are absent, which is a gate that cannot fail (ARCH §18.1)",
                self.evidence
            ),
            EscapeKind::AmbientGit => format!(
                "shells `git` with no `current_dir(…)` (`{}`) — it runs wherever the harness \
                 stands, and a third party who unpacks a source tarball has no `.git` at all. \
                 Take the repo path from the CALLER (as corpus-engine-archaeology does), or \
                 move the check to a crate in NO package",
                self.evidence
            ),
        }
    }
}

/// Scan one file's text for the two runtime-escape shapes. Grep-level and
/// windowed, like rule 3b — with the window doing the work `include_str!`'s
/// per-line test does, because neither shape fits on one line.
///
/// **a. `CARGO_MANIFEST_DIR` + a climb.** The read and the `.parent()` /
/// `join("..")` are separate lines in every instance found so far, so the
/// window runs [-2, +8] around the read.
///
/// **b. a `git` subprocess with no `current_dir(…)` within 15 lines.** What
/// this deliberately does NOT flag is git run at a path the CALLER supplied:
/// `corpus-engine-archaeology` and `corpus-engine-scip` shell git at a
/// `&Path` argument seven times between them, which is those crates' job and
/// lifts fine. The defect is deriving the path from where the crate happens to
/// sit — not touching git.
///
/// Compile-time embeds belong to rule 3b, carve-out and all, so a
/// `CARGO_MANIFEST_DIR` inside an `include_str!` / `include_bytes!` is skipped
/// here. Otherwise both rules fire on `sovereign-contracts`'s recipe embeds
/// and only one of them knows about `sovereign-recipes/`.
fn scan_runtime_escapes(text: &str) -> Vec<RuntimeEscape> {
    /// How far a `CARGO_MANIFEST_DIR` read's statement may run.
    const BACK: usize = 2;
    const FWD: usize = 8;
    /// How far a `Command::new("git")` builder chain may run before this rule
    /// gives up looking for the `current_dir(…)` that makes it caller-directed.
    const GIT_FWD: usize = 15;

    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    // One report per STATEMENT, not per matching line. The name shows up twice
    // in the commonest shape — `env::var("CARGO_MANIFEST_DIR")` and the
    // `.expect("CARGO_MANIFEST_DIR should be set…")` under it — and two
    // findings for one defect is how a burn-down list stops being a count.
    let mut climb_reported_through: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        if line.contains("CARGO_MANIFEST_DIR")
            && climb_reported_through.is_none_or(|through| i > through)
        {
            let lo = i.saturating_sub(BACK);
            let hi = (i + FWD + 1).min(lines.len());
            let window = &lines[lo..hi];
            let compile_time_embed = window
                .iter()
                .any(|l| l.contains("include_str!") || l.contains("include_bytes!"));
            if !compile_time_embed {
                if let Some(hop) = window.iter().find(|l| climbs_out_of_crate(l)) {
                    out.push(RuntimeEscape {
                        line: i + 1,
                        kind: EscapeKind::ManifestClimb,
                        evidence: hop.trim().to_string(),
                    });
                    climb_reported_through = Some(i + FWD);
                }
            }
        }

        if line.contains(r#"Command::new("git")"#) {
            let hi = (i + GIT_FWD + 1).min(lines.len());
            if !lines[i..hi].iter().any(|l| l.contains("current_dir(")) {
                out.push(RuntimeEscape {
                    line: i + 1,
                    kind: EscapeKind::AmbientGit,
                    evidence: line.trim().to_string(),
                });
            }
        }
    }
    out
}

/// A line that walks a path OUT of the directory it started in. Deliberately
/// narrow: `..` also appears in ranges (`0..3`) and struct-update syntax
/// (`..Default::default()`), neither of which is a path.
fn climbs_out_of_crate(line: &str) -> bool {
    // `.parent()`, `join("..")`, `join("../x")`, and a `..` segment inside any
    // path literal — `"/.."`, `"../"`, or a bare `".."`.
    line.contains(".parent()")
        || line.contains("join(\"..")
        || line.contains("\"/..")
        || line.contains("\"../")
        || line.contains("\"..\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live map must parse and must actually declare packages. Without
    /// this the whole gate can go quietly vacuous — an empty `packages` list
    /// checks nothing and prints success, which is the one failure mode the
    /// v3 schema bump exists to prevent.
    #[test]
    fn live_map_declares_packages_and_leaves() {
        let root = common::repo_root();
        let text = std::fs::read_to_string(root.join("quality/ARCH_LAYERS.toml"))
            .expect("the layer map must be readable");
        let map = arch_layers::parse(&text).expect("the layer map must parse");

        assert!(
            map.packages.len() >= 2,
            "expected at least the studio and code-intel packages, got {}",
            map.packages.len()
        );
        // The leaf budget is the tip of the DAG — losing it would silently
        // widen every package's contract surface to the whole workspace.
        assert!(
            !map.package_leaves.is_empty(),
            "no [[package_leaf]] declared"
        );

        // Every governed crate names a doc a reader can open.
        for pkg in &map.packages {
            assert!(
                root.join(&pkg.doc).exists(),
                "package `{}` names doc `{}`, which does not exist",
                pkg.name,
                pkg.doc
            );
        }
    }

    /// The budgets that are empty BY CONTRACT — each one's whole basis for
    /// crossing a package boundary is a provably empty closure, so a single
    /// entry here means the promise is broken and the failing test is the
    /// point.
    #[test]
    fn leaf_budgets_stay_pinned() {
        let root = common::repo_root();
        let text = std::fs::read_to_string(root.join("quality/ARCH_LAYERS.toml")).unwrap();
        let map = arch_layers::parse(&text).unwrap();
        let budget = |name: &str| -> Vec<String> {
            map.package_leaves
                .iter()
                .find(|l| l.name == name)
                .unwrap_or_else(|| panic!("leaf `{name}` is no longer declared"))
                .allow
                .clone()
        };

        assert!(budget("oicp-types").is_empty());
        assert!(budget("kernel-types").is_empty());
        assert!(budget("corpus-engine-sections").is_empty());
        assert!(budget("sovereign-time").is_empty());
        assert_eq!(budget("corpus-engine-vocab"), ["kernel-types"]);
        assert_eq!(
            budget("sovereign-contracts"),
            ["oicp-types", "kernel-types"]
        );
        assert_eq!(budget("oicp-client"), ["sovereign-contracts", "oicp-types"]);
    }

    /// A breach is caught, and a build- or dev-edge breach counts. The layer
    /// map ignores dev edges — a dev-dep cannot reach a shipped artifact — but
    /// a third party who lifts a package carries its tests, so this gate does
    /// not get to ignore them.
    #[test]
    fn package_budget_flags_a_breach_on_every_edge_kind() {
        use arch_layers::{DepEdge, DepKind};
        let map = arch_layers::parse(
            r#"
schema_version = 3
backstage = ["xtask"]
[[layer]]
name = "all"
crates = ["*"]
[[package_leaf]]
name = "oicp-types"
allow = []
[[package]]
name = "demo"
doc = "studio/BOUNDARY.md"
crates = ["pkg-a", "pkg-b"]
"#,
        )
        .unwrap();

        let edge = |to: &str, kind| DepEdge {
            from: "pkg-a".to_string(),
            to: to.to_string(),
            kind,
            optional: false,
        };
        // Inside the closure: the sibling crate and the shared leaf.
        assert!(arch_layers::evaluate_packages(
            &map,
            &[
                edge("pkg-b", DepKind::Normal),
                edge("oicp-types", DepKind::Normal)
            ]
        )
        .is_empty());

        // Outside it — on all three edge kinds.
        for kind in [DepKind::Normal, DepKind::Build, DepKind::Dev] {
            let v = arch_layers::evaluate_packages(&map, &[edge("sovereign-core", kind)]);
            assert_eq!(v.len(), 1, "{kind:?} edge should breach the closure");
            assert!(v[0].describe().contains("leaves the package closure"));
        }

        // A crate in no package is not this gate's business.
        let outside = DepEdge {
            from: "sovereign-cli".to_string(),
            to: "sovereign-core".to_string(),
            kind: DepKind::Normal,
            optional: false,
        };
        assert!(arch_layers::evaluate_packages(&map, &[outside]).is_empty());
    }

    /// Rule 3c's positive controls — the three shapes that actually shipped,
    /// each taken verbatim from the file it broke. A rule whose failing input
    /// nobody can name is not a rule (ARCH §18.1).
    #[test]
    fn runtime_scan_flags_the_shapes_the_lift_priced() {
        // corpus-engine-vocab/tests/atoms_file_census.rs — the read and the
        // climb are on different lines, which is why this is windowed.
        let vocab = r#"
fn roots() -> Vec<PathBuf> {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    vec![ws.join("corpus-engine/src")]
}
"#;
        let hits = scan_runtime_escapes(vocab);
        assert_eq!(hits.len(), 1, "expected the manifest climb");
        assert!(matches!(hits[0].kind, EscapeKind::ManifestClimb));
        assert!(hits[0].describe().contains("carries its tests"));

        // sovereign-contracts/src/skills.rs — three lines between the read and
        // the first `..`.
        let skills = r#"
    fn surviving_modes() {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR should be set for tests");
        let modes_dir = std::path::Path::new(&manifest_dir)
            .join("..")
            .join("..")
            .join("modes");
    }
"#;
        assert_eq!(scan_runtime_escapes(skills).len(), 1);

        // kernel-types/tests/conformance_tags.rs — `git` at a derived root.
        // Caught by the CLIMB, not by the git clause: the subprocess is fine,
        // the path it was handed is the defect.
        let tags = r#"
fn tracked_rs_files() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let out = std::process::Command::new("git")
        .args(["ls-files", "-z", "*.rs"])
        .current_dir(root)
        .output()
        .expect("git ls-files");
}
"#;
        let hits = scan_runtime_escapes(tags);
        assert_eq!(
            hits.len(),
            1,
            "the climb, once — not the caller-directed git"
        );
        assert!(matches!(hits[0].kind, EscapeKind::ManifestClimb));
    }

    /// The ambient-`git` clause: no `current_dir(…)` means the harness's CWD,
    /// and a lifted source tarball has no `.git`.
    #[test]
    fn runtime_scan_flags_git_that_never_says_where() {
        let ambient = r#"
fn head() -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    String::from_utf8_lossy(&out.stdout).to_string()
}
"#;
        let hits = scan_runtime_escapes(ambient);
        assert_eq!(hits.len(), 1);
        assert!(matches!(hits[0].kind, EscapeKind::AmbientGit));
        assert!(hits[0].describe().contains("no `.git` at all"));
    }

    /// Rule 3c's negative controls — every shape that is in a governed crate
    /// today and lifts fine. This is the half that keeps the rule at zero
    /// `[[exception]]` rows; without it the gate would demand four.
    #[test]
    fn runtime_scan_leaves_what_actually_lifts() {
        // sovereign-contracts/src/recipe/registry.rs — a COMPILE-TIME embed.
        // It climbs, and it is rule 3b's, carve-out included.
        let embed = r#"
pub const RECIPE_REGISTRY_TOML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sovereign-recipes/registry.toml"
));
"#;
        assert!(scan_runtime_escapes(embed).is_empty());

        // corpus-mcp/tests/no_inference_stack.rs — the manifest dir with no
        // climb. The crate's own root is not an escape.
        let own_root = r#"
    let out = Command::new(env!("CARGO"))
        .args(["tree", "-p", package, "-e", "normal", "--prefix", "none"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree runs");
"#;
        assert!(scan_runtime_escapes(own_root).is_empty());

        // studio/crates/sovereign-workflow/tests/substrate.rs — into a
        // subdirectory, not out of the crate.
        let subdir = r#"
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
"#;
        assert!(scan_runtime_escapes(subdir).is_empty());

        // corpus-engine-archaeology/src/git_archaeology.rs — git at a path the
        // CALLER supplied. Seven of these exist in the code-intel package and
        // every one of them lifts.
        let caller_directed = r#"
pub fn discover_repo_root(source_path: &Path) -> Result<PathBuf, GitArchaeologyError> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(source_path)
        .output()
        .map_err(GitArchaeologyError::GitNotInstalled)?;
}
"#;
        assert!(scan_runtime_escapes(caller_directed).is_empty());

        // …including the one whose `.args([…])` block runs nine lines before
        // the `current_dir`, which is what sets GIT_FWD.
        let long_chain = r#"
    let out = Command::new("git")
        .args([
            "log",
            "--name-only",
            "--format=%x1e%H%x1f%ct%x1f%ae%x1f%s",
            "--reverse",
            "--all",
        ])
        .current_dir(repo_root)
        .output()
"#;
        assert!(scan_runtime_escapes(long_chain).is_empty());

        // `..` that is not a path: a range and struct-update syntax.
        let not_a_path = r#"
    let manifest = env!("CARGO_MANIFEST_DIR");
    for i in 0..3 {}
    let c = Config { root: manifest.into(), ..Default::default() };
"#;
        assert!(scan_runtime_escapes(not_a_path).is_empty());
    }
}
