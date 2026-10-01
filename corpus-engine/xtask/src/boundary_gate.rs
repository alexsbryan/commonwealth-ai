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
//! The `[[forbid]]` table is the one thing the two passes SHARE, through
//! `arch_layers::forbidden_by`, and it was not shared until 2026-09-09. The
//! allowlist this gate pins is per-package plus the GLOBAL shared leaves, so
//! it cannot say "this package, unlike the others, may not reach that leaf" —
//! and `commonwealth-work -> sovereign-contracts` (a declared leaf) was
//! admitted here on membership while layer-gate failed on the forbid row for
//! the same edge. Two gates, one policy file, opposite verdicts, and the one
//! that names itself for lift closure said yes. A forbid row now outranks
//! membership and the leaf allowance alike (ARCH §18.1, §10.6).
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

    // Rules 3a/3b/3c — the filesystem half, which no manifest can express.
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

    // The `[[distribution]]` rules: direct edges, the filesystem rules, the
    // face items and the fixed cap (distribution_gate.rs).
    checked += crate::distribution_gate::check(&root, &map, &edges, &dir_of, &mut fails);

    // Rule 4 — a leaf's fixed size cap (`max_code_lines`), counted by
    // size-gate's own counter. A cap nothing reads would be no cap.
    for leaf in &map.package_leaves {
        let (Some(cap), Some(rel)) = (leaf.max_code_lines, dir_of.get(leaf.name.as_str())) else {
            continue;
        };
        match crate::size_gate::crate_code_lines(&root, &root.join(rel), &leaf.name) {
            Ok(n) if n > cap => fails.push(format!(
                "[{}] {}: {n} code lines over its fixed cap of {cap} — split the \
                 mechanism out or take the cap to the operator; it is never re-pinned",
                arch_layers::SHARED_LEAVES_SCOPE,
                leaf.name
            )),
            Ok(_) => {}
            Err(e) => fails.push(format!(
                "[{}] {}: size cap could not be measured: {e}",
                arch_layers::SHARED_LEAVES_SCOPE,
                leaf.name
            )),
        }
    }

    // Declared-but-absent crates. Reported rather than skipped in silence:
    // the same shape covers a typo, and a typo'd crate name is a rule that
    // quietly governs nothing (ARCH §18.3 — absence is reported, never
    // defaulted).
    let mut missing = arch_layers::missing_package_crates(&map, &names);
    missing.extend(arch_layers::missing_distribution_crates(&map, &names));

    // ── Report ────────────────────────────────────────────────────────────────
    eprintln!(
        "boundary-gate: {} package(s) + {} shared leaves + {} distribution(s), \
         {checked} crate(s) checked (dep closure incl. dev+build edges, build.rs, \
         include_str escapes, runtime root escapes, face items)",
        map.packages.len(),
        map.package_leaves.len(),
        map.distributions.len()
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
                // A declared leaf_budget IS the package's closure; the global
                // leaf list would misstate it.
                let (label, names) = match &pkg.leaf_budget {
                    Some(budget) => ("leaf budget", budget.join(", ")),
                    None => ("shared leaves", leaves.join(", ")),
                };
                eprintln!(
                    "\n  closure for [{}]: {} + {label} ({names})",
                    pkg.name,
                    pkg.crates.join(", "),
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

/// Rule 3b — `include_str!` / `include_bytes!` literals that escape the crate
/// root (climb two+ levels). Grep-level and windowed, recursing `src/`.
///
/// There is NO carve-out. A `sovereign-recipes/` exception used to exist for the
/// shared leaf `sovereign-contracts`; the leaf no longer embeds the tree (the
/// artifact is injected from `corpus-engine` instead), and the exception was the
/// mechanism that let an unliftable embed sit under a green gate. Removing it is
/// the structural fix (ARCH §10 — make it structural, not remembered).
pub(crate) fn include_escapes(dir: &Path, crate_name: &str, scope: &str, fails: &mut Vec<String>) {
    let mut files = Vec::new();
    rs_files(&dir.join("src"), &mut files);
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
        let rel_dir = path
            .parent()
            .and_then(|p| p.strip_prefix(dir).ok())
            .unwrap_or(Path::new("src"))
            .to_path_buf();
        for e in scan_include_escapes(&text, &rel_dir) {
            fails.push(format!(
                "[{scope}] {crate_name}: {}:{} {}",
                path.strip_prefix(dir).unwrap_or(&path).display(),
                e.line,
                e.describe()
            ));
        }
    }
}

/// One compile-time embed that climbs out of the crate root, with the line
/// that proves it.
struct IncludeEscape {
    line: usize,
    evidence: String,
}

impl IncludeEscape {
    /// The fix goes in the message, same as rules 3a and 3c: a violation a
    /// reader has to go look up is a violation that gets grandfathered.
    fn describe(&self) -> String {
        format!(
            "embeds a file outside the crate root at COMPILE TIME (`{}`) — a third \
             party who lifts this package has none of that tree, and the embed makes \
             the monorepo's directory shape a build requirement. Move the data inside \
             the crate, or inject it at the call site",
            self.evidence
        )
    }
}

/// Scan one file's text for embeds that climb two+ levels out of the crate
/// root — WINDOWED over the statement, because the shape that actually ships
/// does not fit on one line.
///
/// Rule 3b tested a single trimmed line until 2026-09-04, so it required the
/// macro and the climbing path to appear TOGETHER. Both of
/// `sovereign-contracts`'s recipe embeds (`src/recipe/{registry,schema}.rs`)
/// spread the macro, the `env!("CARGO_MANIFEST_DIR")` and the
/// `"/../../../sovereign-recipes/…"` literal across three lines, so neither
/// line satisfied both halves and the rule never saw them. It was green by
/// accident — and would have stayed green with the carve-out deleted, which
/// is the tell: a check with no failing input you can name is not a check
/// (ARCH §18.1). The embeds and the carve-out are both gone now; the window
/// stays.
///
/// The window is the STATEMENT rather than a fixed lookahead, so one embed
/// cannot hide the next.
/// Lexically normalise `base/rel`, returning None when it climbs past the root.
fn resolves_inside(base: &Path, rel: &str) -> bool {
    let mut depth: i32 = base
        .components()
        .filter(|c| !c.as_os_str().is_empty())
        .count() as i32;
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => depth += 1,
        }
    }
    true
}

/// The `"..."` literals in one window, in order.
fn string_literals(window: &[&str]) -> Vec<String> {
    let joined = window.join("\n");
    let mut out = Vec::new();
    let b: Vec<char> = joined.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '"' {
            let mut j = i + 1;
            let mut lit = String::new();
            while j < b.len() && b[j] != '"' {
                if b[j] == '\\' {
                    j += 1;
                }
                if j < b.len() {
                    lit.push(b[j]);
                }
                j += 1;
            }
            out.push(lit);
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

fn scan_include_escapes(text: &str, rel_dir: &Path) -> Vec<IncludeEscape> {
    /// How far a macro invocation may run before this rule stops looking for
    /// the `;` that ends it. The shipped shapes use four lines.
    const FWD: usize = 8;

    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    // One report per STATEMENT, for the same reason rule 3c dedupes: two
    // findings for one defect is how a burn-down list stops being a count.
    let mut examined_through: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        if !(line.contains("include_str!") || line.contains("include_bytes!")) {
            continue;
        }
        if examined_through.is_some_and(|through| i <= through) {
            continue;
        }
        // The statement: the macro line through the line that terminates it.
        let cap = (i + FWD + 1).min(lines.len());
        let hi = (i..cap)
            .find(|&k| lines[k].contains(';'))
            .map_or(cap, |k| k + 1);
        let window = &lines[i..hi];
        examined_through = Some(hi.saturating_sub(1));

        // RESOLVE, do not pattern-match. `../..` in the text is not an escape:
        // `corpus-engine/src/extractors/code/mod.rs` embeds
        // `../../../queries/rust/symbols.scm`, which lands on
        // `corpus-engine/queries/` — INSIDE the crate root. That false positive
        // stood as six red lines and as burn-down item (2) on the
        // `corpus-mcp -> corpus-engine` exception, for work already done.
        let base: &Path = if window.iter().any(|l| l.contains("CARGO_MANIFEST_DIR")) {
            Path::new("")
        } else {
            rel_dir
        };
        if let Some(hop) = window.iter().find(|l| l.contains("../..")) {
            let escapes = string_literals(window)
                .iter()
                .filter(|l| l.contains("../.."))
                .any(|l| !resolves_inside(base, l.trim_start_matches('/')));
            if escapes {
                out.push(IncludeEscape {
                    line: i + 1,
                    evidence: hop.trim().to_string(),
                });
            }
        }
    }
    out
}

/// Rule 3c — RUNTIME reach-outs past the crate root, in `src/`, `tests/`,
/// `benches/` and `examples/`.
///
/// It cannot stop at `src/` the way rule 3b does: `quality/ARCH_LAYERS.toml`
/// says "a third party who lifts the package carries its tests", and every
/// instance of this defect found so far has been in test code.
pub(crate) fn runtime_root_escapes(
    dir: &Path,
    crate_name: &str,
    scope: &str,
    fails: &mut Vec<String>,
) {
    for sub in ["src", "tests", "benches", "examples"] {
        let mut files = Vec::new();
        rs_files(&dir.join(sub), &mut files);
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rel = path
                .strip_prefix(dir)
                .unwrap_or(&path)
                .display()
                .to_string();
            let rel_dir = path.parent().and_then(|p| p.strip_prefix(dir).ok());
            let mounts = scan_path_mounts(&text, rel_dir.unwrap_or(Path::new("")));
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let test_file = sub != "src" || stem == "tests" || stem.ends_with("_tests");
            let walks = scan_checkout_walks(&text, test_file);
            for e in scan_runtime_escapes(&text)
                .into_iter()
                .chain(mounts)
                .chain(walks)
            {
                fails.push(format!(
                    "[{scope}] {crate_name}: {rel}:{} {}",
                    e.line,
                    e.describe()
                ));
            }
        }
    }
}

/// Every `.rs` file under `dir`, recursively. A missing directory is empty,
/// not an error — most crates have no `benches/`.
pub(crate) fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
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
    /// A `#[path = "…"]` whose file resolves outside the crate root: the
    /// module is another crate's source, mounted by where the two happen to sit.
    PathMount,
    /// TEST code that walks up from the current directory: under `cargo test`
    /// that is the crate root, so the walk leaves it for the checkout.
    CheckoutWalk,
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
            EscapeKind::PathMount => format!(
                "mounts a module from outside the crate root (`{}`) — a lift carries this \
                 crate's tree, not its neighbour's. Put the shared code in a crate both \
                 depend on (a leaf's test-doubles feature) and import it",
                self.evidence
            ),
            EscapeKind::CheckoutWalk => format!(
                "walks up from the current directory in TEST code (`{}`) — `cargo test` \
                 stands at the crate root, so this reads the checkout above it, which a \
                 lifted package does not have. Name the data with an env knob the lift \
                 carries, or move the check to a crate in NO package",
                self.evidence
            ),
        }
    }
}

/// Rule 3c's fourth shape: a `current_dir()` read that climbs within the same
/// [-2, +8] window as the manifest clause, in test code only. Production CLIs
/// walk up from the INVOKER's directory by design (`svrn eval inner-chaos`
/// finding `bench/inner_work`), which lifts fine; a test doing it stands at
/// the crate root and lands in the monorepo. Test code is a file under
/// `tests/`, `benches/` or `examples/`, a `tests.rs` / `*_tests.rs`, or
/// anything from a file's first `#[cfg(test)]` on.
fn scan_checkout_walks(text: &str, test_file: bool) -> Vec<RuntimeEscape> {
    let lines: Vec<&str> = text.lines().collect();
    let test_from = if test_file {
        0
    } else {
        lines
            .iter()
            .position(|l| l.trim_start().starts_with("#[cfg(test)]"))
            .unwrap_or(lines.len())
    };
    let mut out = Vec::new();
    let mut reported_through: Option<usize> = None;
    for (i, line) in lines.iter().enumerate().skip(test_from) {
        if !line.contains("current_dir()") || reported_through.is_some_and(|t| i <= t) {
            continue;
        }
        let window = &lines[i.saturating_sub(2)..(i + 9).min(lines.len())];
        // A PathBuf's `pop()` is a climb; a stack's `while let Some(_) =
        // stack.pop()` is not.
        let climb =
            |l: &&&str| climbs_out_of_crate(l) || (l.contains(".pop()") && !l.contains("Some("));
        if let Some(hop) = window.iter().find(climb) {
            out.push(RuntimeEscape {
                line: i + 1,
                kind: EscapeKind::CheckoutWalk,
                evidence: hop.trim().to_string(),
            });
            reported_through = Some(i + 8);
        }
    }
    out
}

/// Rule 3c's third shape: `#[path = "…"]` resolved against the directory of
/// the file that carries it (`rel_dir`, relative to the crate root) and
/// normalised lexically. A `..` that pops past the crate root is the escape;
/// one that stays inside (`tests/rail_e2e` → `../main/common`) is not.
fn scan_path_mounts(text: &str, rel_dir: &Path) -> Vec<RuntimeEscape> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let Some(lit) = line
            .trim_start()
            .strip_prefix("#[path = \"")
            .and_then(|r| r.split('"').next())
        else {
            continue;
        };
        let mut depth = rel_dir.components().count();
        let escapes = Path::new(lit).components().any(|c| match c {
            std::path::Component::ParentDir if depth == 0 => true,
            std::path::Component::ParentDir => {
                depth -= 1;
                false
            }
            std::path::Component::Normal(_) => {
                depth += 1;
                false
            }
            _ => false,
        });
        if escapes {
            out.push(RuntimeEscape {
                line: i + 1,
                kind: EscapeKind::PathMount,
                evidence: line.trim().to_string(),
            });
        }
    }
    out
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
            // TWO spellings of the same property, because the property is
            // "the repo path came from the CALLER", not "this builder names
            // `current_dir`". `git -C <path>` is git's own flag for it and is
            // what `sovereign-code` uses at all four of its sites; reading only
            // `current_dir(` made this rule fire on four call sites that already
            // take a `&Path` argument — exactly the shape the doc above says it
            // deliberately does NOT flag. Watched failing on those four
            // (2026-09-21) before it was fixed. Widened the same day from
            // `arg("-C")` to the bare literal: `sovereign-eval`'s `git_head`
            // spells it `.args(["-C"]).arg(root)` and was flagged while taking
            // the root as an argument. The property is the flag, not the
            // builder method that carries it.
            let caller_directed = lines[i..hi]
                .iter()
                .any(|l| l.contains("current_dir(") || l.contains(r#""-C""#));
            if !caller_directed {
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
    // `.parent()`, `.ancestors()` (`.nth(3)` to the repo, svrn's censuses until
    // pb-distribution-svrn-lift), `join("..")`, `join("../x")`, and a `..`
    // segment inside any path literal — `"/.."`, `"../"`, or a bare `".."`.
    // And a `".git"` lookup: no lifted crate root holds one, so seeking it is
    // seeking the checkout (sovereign-daemon's mesh gates until
    // pb-distribution-svrn-lift-2).
    line.contains(".parent()")
        || line.contains("\".git\"")
        || line.contains(".ancestors()")
        || line.contains("join(\"..")
        || line.contains("\"/..")
        || line.contains("\"../")
        || line.contains("\"..\"")
}

#[cfg(test)]
mod tests;
