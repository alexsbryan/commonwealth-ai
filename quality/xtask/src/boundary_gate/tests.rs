// SPDX-License-Identifier: AGPL-3.0-or-later
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

    // `oicp-types` WAS empty by contract and is no longer, as of
    // 2026-09-09 (cw-lift 5b, operator decision). `JobRequirements`
    // carries `kernel_types::quality::Precondition`, and the
    // alternatives were a second spelling of `Precondition` in this
    // leaf (§10.6) or one noun with halves in two crates. The widening
    // is on `understanding-vocab`'s precedent below and costs the
    // closure ZERO third-party crates: `kernel-types` is itself an
    // empty leaf, and `toml` — the only dep its `quality-registry`
    // feature adds — was already an `oicp-types` dependency.
    assert_eq!(budget("oicp-types"), ["kernel-types"]);
    assert!(budget("kernel-types").is_empty());
    assert!(budget("corpus-engine-sections").is_empty());
    assert!(budget("sovereign-time").is_empty());
    assert_eq!(budget("understanding-vocab"), ["kernel-types"]);
    // `sovereign-time` joined 2026-09-07 — an EMPTY-dependency leaf, so the
    // closure widens by zero crates. Pinned here because this test is the
    // structural half of "widen deliberately": the gate binary goes green
    // the moment ARCH_LAYERS.toml changes, and only this line makes the
    // widening something a human had to type twice.
    assert_eq!(
        budget("sovereign-contracts"),
        ["oicp-types", "kernel-types", "sovereign-time"]
    );
    assert_eq!(budget("oicp-client"), ["sovereign-contracts", "oicp-types"]);
    // The host kit's cap is fixed, not ratcheted (pb-hostkit): changing
    // it is typed here and in ARCH_LAYERS.toml, and it is the operator's.
    let cap = |name: &str| {
        map.package_leaves
            .iter()
            .find(|l| l.name == name)
            .and_then(|l| l.max_code_lines)
    };
    assert_eq!(cap("host-kit"), Some(2500));
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
doc = "clients/studio/BOUNDARY.md"
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

/// Rule 3b's positive controls. The first is the shape that actually
/// ships and that the per-line rule scored ZERO on for as long as it
/// existed: the macro is on one line and the climb is two lines below it,
/// so no single line satisfies both halves.
#[test]
fn include_scan_sees_an_embed_that_wraps_over_three_lines() {
    // sovereign-contracts/src/recipe/registry.rs's exact shape, with the
    // path swapped so the escape is the only variable.
    let wrapped = r#"
pub const OTHER_TOML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../elsewhere/registry.toml"
));
"#;
    let hits = scan_include_escapes(wrapped, Path::new("src"));
    assert_eq!(hits.len(), 1, "the three-line embed must be seen");
    assert!(hits[0].evidence.contains("elsewhere/registry.toml"));
}

/// The `sovereign-recipes/` carve-out is GONE: the exact embed the leaf
/// used to carry now reports as an escape. This is the check that would
/// have failed had the carve-out stayed (ARCH §18.1 — a gate you have not
/// watched fail is not a gate).
#[test]
fn include_scan_no_longer_carves_out_sovereign_recipes() {
    let recipe_embed = r#"
pub const RECIPE_REGISTRY_TOML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../ingest/crates/sovereign-recipes/registry.toml"
));
"#;
    let hits = scan_include_escapes(recipe_embed, Path::new("src"));
    assert_eq!(hits.len(), 1, "a sovereign-recipes embed must now be seen");
    assert!(hits[0]
        .evidence
        .contains("ingest/crates/sovereign-recipes/registry.toml"));
}

#[test]
fn include_scan_still_sees_a_single_line_embed() {
    let inline = r#"const X: &str = include_str!("../../elsewhere/x.json");"#;
    assert_eq!(scan_include_escapes(inline, Path::new("src")).len(), 1);
    let bytes = r#"const X: &[u8] = include_bytes!("../../elsewhere/x.bin");"#;
    assert_eq!(scan_include_escapes(bytes, Path::new("src")).len(), 1);
}

/// Rule 3b's negative controls — the embeds that lift fine because they
/// never leave the crate.
#[test]
fn include_scan_leaves_what_stays_inside() {
    // Inside the crate, and one level up from a file in `src/` is still
    // the crate root — only a two+ level climb leaves.
    let local = r#"const X: &str = include_str!("fixtures/x.json");"#;
    assert!(scan_include_escapes(local, Path::new("src")).is_empty());
    let crate_root = r#"const X: &[u8] = include_bytes!("../README.md");"#;
    assert!(scan_include_escapes(crate_root, Path::new("src")).is_empty());
}

/// The false positive this rule carried until 2026-09-21: a climb that
/// LANDS inside the crate root. corpus-engine's tree-sitter queries live
/// at `ingest/crates/corpus-engine/queries/` and are embedded from
/// `src/extractors/code/mod.rs`, so the literal reads `../../../queries/…`
/// and resolves to the crate root — six red lines, and a burn-down item on
/// the `corpus-mcp -> corpus-engine` exception, for work already done.
#[test]
fn include_scan_resolves_a_climb_that_lands_inside_the_crate() {
    let inside = r#"const Q: &str = include_str!("../../../queries/rust/symbols.scm");"#;
    assert!(scan_include_escapes(inside, Path::new("src/extractors/code")).is_empty());
    // Same literal from a shallower file DOES leave.
    assert_eq!(scan_include_escapes(inside, Path::new("src")).len(), 1);
}

/// The window is a statement and not a fixed lookahead, so the embed above
/// cannot hide the one below. A `[i, i+8]` window would have let the second
/// statement go unreported.
#[test]
fn include_scan_reports_each_escaping_statement() {
    let two = r#"
pub const FIRST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../elsewhere/a.toml"
));
pub const SECOND: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sovereign-recipes/b.toml"
));
"#;
    let hits = scan_include_escapes(two, Path::new("src"));
    assert_eq!(hits.len(), 2, "both embeds escape");
    assert!(hits[0].evidence.contains("elsewhere/a.toml"));
    assert!(hits[1].evidence.contains("sovereign-recipes/b.toml"));
}

/// Rule 3c's positive controls — the three shapes that actually shipped,
/// each taken verbatim from the file it broke. A rule whose failing input
/// nobody can name is not a rule (ARCH §18.1).
#[test]
fn runtime_scan_flags_the_shapes_the_lift_priced() {
    // shared/crates/understanding-vocab/tests/atoms_file_census.rs — the read and the
    // climb are on different lines, which is why this is windowed.
    let vocab = r#"
fn roots() -> Vec<PathBuf> {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    vec![ws.join("ingest/crates/corpus-engine/src")]
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

    // shared/crates/kernel-types/tests/conformance_tags.rs — `git` at a derived root.
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

    // sovereign-daemon/tests/main/daemon_variant_census.rs — the
    // ancestor walk to the repo root.
    let ancestors = r#"
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("sovereign-daemon lives three levels under the repo root")
        .to_path_buf()
}
"#;
    assert_eq!(scan_runtime_escapes(ancestors).len(), 1);
}

/// `#[path]` mounts: sovereign-daemon/tests/main.rs mounted
/// sovereign-tools' test double by climbing into it; tests/rail_e2e/main.rs
/// mounts a sibling test dir of its own crate, which lifts.
#[test]
fn path_mounts_are_judged_against_the_crate_root() {
    let neighbour = r#"
#[path = "../../sovereign-tools/tests/main/local_corpus_port_double.rs"]
mod local_corpus_port_double;
"#;
    let hits = scan_path_mounts(neighbour, Path::new("tests"));
    assert_eq!(hits.len(), 1);
    assert!(matches!(hits[0].kind, EscapeKind::PathMount));
    assert!(hits[0].line == 2 && hits[0].describe().contains("test-doubles"));

    let own = "#[path = \"../main/common/work_rails.rs\"]\nmod work_rails;\n";
    assert!(scan_path_mounts(own, Path::new("tests/rail_e2e")).is_empty());
    assert!(scan_path_mounts("#[path = \"main/x.rs\"]\n", Path::new("tests")).is_empty());
}

/// The two shapes pb-distribution-svrn-lift-2 priced. A `.git` lookup from the
/// manifest dir, verbatim from sovereign-daemon's mesh_principal_gate.rs; and a
/// walk up from the CWD, which is a climb in test code and the invoker's
/// business in production code (inner_chaos/personas.rs `resolve_bench_dir`).
#[test]
fn checkout_lookups_and_test_walks_are_climbs() {
    let git = r#"
    fn repo_root() -> PathBuf {
        let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        while !dir.join(".git").exists() {
            assert!(dir.pop(), "no .git above {}", env!("CARGO_MANIFEST_DIR"));
        }
        dir
    }
"#;
    let hits = scan_runtime_escapes(git);
    assert_eq!(hits.len(), 1, "one statement, one report");
    assert!(hits[0].evidence.contains("\".git\""));

    let walk = r#"
    let mut here = std::env::current_dir().unwrap();
    loop {
        if here.join("bench/inner_work").is_dir() {
            break;
        }
        if !here.pop() {
            panic!("no bench");
        }
    }
"#;
    let hits = scan_checkout_walks(walk, true);
    assert_eq!(hits.len(), 1);
    assert!(matches!(hits[0].kind, EscapeKind::CheckoutWalk));
    assert!(hits[0].describe().contains("env knob the lift"));
    // The same walk in production code, and after a `#[cfg(test)]` line.
    assert!(scan_checkout_walks(walk, false).is_empty());
    let in_mod = format!("fn f() {{}}\n#[cfg(test)]\nmod tests {{\n{walk}\n}}\n");
    assert_eq!(scan_checkout_walks(&in_mod, false).len(), 1);
    // A stack's pop is not a climb.
    let stack = "let cwd = std::env::current_dir().unwrap();\nwhile let Some(d) = stack.pop() {}\n";
    assert!(scan_checkout_walks(stack, true).is_empty());
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

/// `git -C <path>` is the SAME property as `.current_dir(path)` — the repo
/// came from the caller — and the rule must read both spellings. It read
/// only `current_dir(` until 2026-09-21 and fired on four `sovereign-code`
/// sites that all take a `&Path` argument, which is the shape the rule's
/// own doc says it deliberately does not flag.
///
/// The pair is the point: the `-C` form passes, the form that names
/// neither still fails (`runtime_scan_flags_git_that_never_says_where`
/// above). Loosening a rule without a control is how a gate stops being one.
/// The plural spelling. `sovereign-eval::manifest::git_head` writes
/// `.args(["-C"]).arg(root)`, which carries the identical property and was
/// flagged because the matcher read `arg("-C")` only.
#[test]
fn runtime_scan_accepts_the_args_slice_spelling_of_git_dash_c() {
    let plural = r#"
fn git_head(root: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
"#;
    assert!(scan_runtime_escapes(plural).is_empty());
}

#[test]
fn runtime_scan_accepts_git_dash_c_as_caller_directed() {
    let dash_c = r#"
fn current_branch(repo_root: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}
"#;
    assert!(
        scan_runtime_escapes(dash_c).is_empty(),
        "`git -C <caller path>` is caller-directed, not ambient"
    );
}

/// Rule 3c's negative controls — every shape that is in a governed crate
/// today and lifts fine. This is the half that keeps the rule at zero
/// `[[exception]]` rows; without it the gate would demand four.
#[test]
fn runtime_scan_leaves_what_actually_lifts() {
    // A COMPILE-TIME embed. It climbs, and it is rule 3b's business —
    // rule 3c does not double-report it.
    let embed = r#"
pub const RECIPE_REGISTRY_TOML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../ingest/crates/sovereign-recipes/registry.toml"
));
"#;
    assert!(scan_runtime_escapes(embed).is_empty());

    // quality/xtask/tests/no_inference_stack.rs — the manifest dir with no
    // climb. The crate's own root is not an escape.
    let own_root = r#"
    let out = Command::new(env!("CARGO"))
        .args(["tree", "-p", package, "-e", "normal", "--prefix", "none"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree runs");
"#;
    assert!(scan_runtime_escapes(own_root).is_empty());

    // shared/crates/sovereign-workflow/tests/substrate.rs — into a
    // subdirectory, not out of the crate.
    let subdir = r#"
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
"#;
    assert!(scan_runtime_escapes(subdir).is_empty());

    // code/crates/corpus-engine-archaeology/src/git_archaeology.rs — git at a path the
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
