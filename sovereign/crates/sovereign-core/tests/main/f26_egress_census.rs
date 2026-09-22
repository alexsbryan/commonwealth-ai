// SPDX-License-Identifier: AGPL-3.0-or-later
//! F26 — the egress-boundary census, enforced as a build gate
//! (bar `dr-egress`, order deep-research-t2a).
//!
//! The bar's instrument (PLAN.md F26): "every remote client
//! construction routes through the boundary, enforced as a build
//! gate". This file IS the census: a deterministic (no model)
//! enumeration of every HTTP-client construction site in the
//! workspace's production `src/` trees, checked against a reviewed
//! registry. A new construction site anywhere fails the gate until
//! it is registered and classified — the review moment is the
//! commit, never a silent pass.
//!
//! Scope decisions (reviewed at landing, order deep-research-t2a):
//! - Scan scope: every workspace member's `src/` tree (the root
//!   Cargo.toml `members` list — a new member auto-enters the
//!   scan). `tests/`, `examples/`, `benches/` and `build.rs` are
//!   outside the scope: the census guards PRODUCTION construction,
//!   which is what F26's "remote client construction" means.
//! - The boundary guards egress to THIRD-PARTY endpoints: remote
//!   model providers (RemotePayload) and search engines
//!   (QueryEgress). Both classes are FORBIDDEN outside the ONE
//!   boundary module (sovereign-contracts/src/egress.rs).
//! - Mesh / peer / pod traffic (Mesh) is the estate's own transport
//!   — its own auth and custody class; daemon-local traffic
//!   (LocalDaemon) never leaves the machine; inbound-only sites
//!   (InboundOnly: downloads, fetches, probes) carry content IN,
//!   never estate payloads out; operator-configured endpoints
//!   (OperatorSurface: MCP servers, CalDAV) are operator-owned
//!   targets; test-only sites (TestOnly) live in test fixtures.
//! - Counts: per-file construction-site counts are part of the
//!   registry. A count drift fails the gate so a NEW site is a
//!   review moment, never a silent pass.
//!
//! At HEAD (the red, before the boundary exists) the census counts
//! five egress-class construction sites outside the boundary, the
//! enrich providers path named first:
//!   - sovereign/crates/sovereign-enrichment-build/src/inference_client/
//!     (RemotePayload — the `--provider` chat client; at
//!     sovereign-cli-llm/src/enrich_cmd/inference_client.rs until P0's
//!     crate split, 2026-09-02)
//!   - sovereign/crates/sovereign-core/src/deep_research/port.rs
//!     (QueryEgress — the web_search client)
//!   - sovereign/crates/sovereign-tools/src/knowledge_lookup/mod.rs
//!     (QueryEgress — the web-escalation client)
//!   - studio/crates/sovereign-tools-base/src/web/mod.rs
//!     (QueryEgress — the search tool's default_client)
//!   - sovereign/crates/sovereign-desktop/src-tauri/src/commands/
//!     conversation.rs (the Search-the-web card's client — carried
//!     at the red as `LocalDaemon 1`; corrected at landing when the
//!     re-home review saw the site dispatch External queries)
//!
//! At the landing (the boundary + this census in ONE commit) every
//! one of those sites routes through BOUNDARY_MODULE and the rows
//! read Boundary (egress.rs, 2 sites) / LocalDaemon / InboundOnly —
//! the census then reads ZERO egress-class rows outside the boundary.
//!
//! R-6 (bar `dr-budget-one-decider`) rides in the same file: the
//! identifiers of every other budget decider in the workspace
//! (`budget_allows`, `decrement_budget`, `BudgetView`) must be
//! absent from production src trees, and the ONE run-scoped
//! fail-closed SpendDecider must exist in sovereign-core.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The ONE egress boundary module: every remote-model call and every
/// search-query egress is constructed through it (bar dr-egress).
const BOUNDARY_MODULE: &str = "sovereign/crates/sovereign-contracts/src/egress.rs";

/// The ONE run-scoped fail-closed budget decider (bar
/// dr-budget-one-decider). Frontier-key spend is declared here and
/// inert until t2b opens the judge role.
const SPEND_DECIDER_MODULE: &str = "sovereign/crates/sovereign-core/src/deep_research/budget.rs";

// The registry and its Class vocabulary live in the child module since
// 2026-09-22, when the reviewed-history comments grew the file past its
// ceiling. The rows are data about the workspace, the scan and gates are
// the instrument — one concern each. The explicit #[path] is the
// #[path]-mount quirk: a child of a path-mounted module does not resolve
// its own directory without it.
#[path = "f26_egress_census/registry.rs"]
mod registry;

use registry::{Class, REGISTRY};

// ---------------------------------------------------------------------------
// Instrument
// ---------------------------------------------------------------------------

/// The workspace root: the ancestor of CARGO_MANIFEST_DIR whose
/// Cargo.toml declares `[workspace]`.
fn workspace_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let toml = dir.join("Cargo.toml");
        if toml.is_file()
            && fs::read_to_string(&toml)
                .map(|t| t.contains("[workspace]"))
                .unwrap_or(false)
        {
            return dir;
        }
        if !dir.pop() {
            panic!(
                "workspace root not found from {}",
                env!("CARGO_MANIFEST_DIR")
            );
        }
    }
}

/// The workspace member paths from the root Cargo.toml `members`
/// list (deterministic scan scope — a new member auto-enters).
fn workspace_members(root: &Path) -> Vec<String> {
    let text = fs::read_to_string(root.join("Cargo.toml")).expect("root Cargo.toml");
    let start = text
        .find("members = [")
        .unwrap_or_else(|| panic!("no members list in root Cargo.toml"));
    let end = text[start..].find(']').expect("unterminated members list") + start;
    text[start + "members = [".len()..end]
        .lines()
        .map(|l| l.trim().trim_matches(',').trim_matches('"'))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

fn walk_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let p = entry.expect("dir entry").path();
        if p.is_dir() {
            walk_rs(&p, out);
        } else if p.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// Every production .rs file (member `src/` trees only), sorted.
fn production_src_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for member in workspace_members(root) {
        let src = root.join(&member).join("src");
        if src.is_dir() {
            walk_rs(&src, &mut out);
        }
    }
    out.sort();
    out
}

/// Repo-relative path ("sovereign/crates/..." form — the registry's
/// key space).
fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Construction-site count for one file, with the same semantics as
/// the 2026-08-17 survey probe: a line counts if it contains a
/// `reqwest::(blocking::)?Client::(new|builder)(` token, or — in
/// files that `use reqwest` — a bare `Client::(new|builder)(`.
fn count_sites(text: &str) -> usize {
    let has_import = text.contains("use reqwest");
    const PREFIXED: [&str; 4] = [
        "reqwest::Client::new(",
        "reqwest::Client::builder(",
        "reqwest::blocking::Client::new(",
        "reqwest::blocking::Client::builder(",
    ];
    let mut n = 0;
    for line in text.lines() {
        if PREFIXED.iter().any(|p| line.contains(p)) {
            n += 1;
            continue;
        }
        if has_import && (line.contains("Client::new(") || line.contains("Client::builder(")) {
            n += 1;
        }
    }
    n
}

/// Scan all production src trees → (repo-relative path, site count).
fn scan(root: &Path) -> BTreeMap<String, usize> {
    let mut sites = BTreeMap::new();
    for p in production_src_files(root) {
        let text = fs::read_to_string(&p).expect("read source file");
        let n = count_sites(&text);
        if n > 0 {
            sites.insert(rel(root, &p), n);
        }
    }
    sites
}

// ---------------------------------------------------------------------------
// The gates
// ---------------------------------------------------------------------------

/// F26 — the egress-boundary census (bar dr-egress). Every
/// RemotePayload / QueryEgress construction must live in
/// BOUNDARY_MODULE; every detected site must be registered; every
/// registered site must still exist with the same count.
#[test]
fn f26_egress_boundary_census() {
    let root = workspace_root();
    let sites = scan(&root);
    let mut problems: Vec<String> = Vec::new();

    let mut registered: BTreeMap<&str, Class> = BTreeMap::new();
    for (path, class, count) in REGISTRY {
        registered.insert(path, *class);
        match sites.get(*path) {
            None => problems.push(format!(
                "STALE ROW: {path} — no construction sites found (registry says {count})"
            )),
            Some(&actual) if actual != *count => problems.push(format!(
                "COUNT DRIFT: {path} — registry {count}, census {actual} — a new construction site is a review moment"
            )),
            Some(_) => {}
        }
        if matches!(class, Class::RemotePayload | Class::QueryEgress) && *path != BOUNDARY_MODULE {
            problems.push(format!(
                "EGRESS OUTSIDE BOUNDARY: {path} ({}, {count} site(s)) — every remote-model / search-query client must be constructed in {BOUNDARY_MODULE}",
                class.as_str()
            ));
        }
    }

    match sites.get(BOUNDARY_MODULE) {
        Some(_) => {
            if registered.get(BOUNDARY_MODULE) != Some(&Class::Boundary) {
                problems.push(format!(
                    "{BOUNDARY_MODULE} exists but is not registered with class Boundary — register the row"
                ));
            }
        }
        None => problems.push(format!(
            "NO BOUNDARY MODULE: {BOUNDARY_MODULE} does not exist — the ONE egress choke point is not built"
        )),
    }

    for (path, count) in &sites {
        if !registered.contains_key(path.as_str()) {
            problems.push(format!(
                "UNREGISTERED: {path} ({count} site(s)) — classify it in the F26 registry"
            ));
        }
    }

    assert!(
        problems.is_empty(),
        "F26 egress-boundary census FAILED ({} problem(s)):\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// R-6 — one run-scoped fail-closed budget decider (bar
/// dr-budget-one-decider). No second decider: the identifiers of the
/// studio fail-open decider cluster (`budget_allows`,
/// `decrement_budget`, `BudgetView`) are absent from every
/// production src tree, and the ONE SpendDecider exists in
/// sovereign-core.
///
/// NOTE: `check_budget` is deliberately NOT forbidden — it names the
/// conversation-FRAME schema check in sovereign-tools
/// (frame.rs / conv_frame.rs / session_state.rs), a different
/// concept; the studio decider's own identifiers above cover it.
#[test]
fn r6_one_run_scoped_budget_decider() {
    let root = workspace_root();
    const FORBIDDEN: [&str; 3] = ["budget_allows", "decrement_budget", "BudgetView"];

    let mut hits: Vec<String> = Vec::new();
    for p in production_src_files(&root) {
        let text = fs::read_to_string(&p).expect("read source file");
        for (i, line) in text.lines().enumerate() {
            for id in FORBIDDEN {
                if line.contains(id) {
                    hits.push(format!("{}:{}: {id}", rel(&root, &p), i + 1));
                }
            }
        }
    }

    let decider_text = fs::read_to_string(root.join(SPEND_DECIDER_MODULE))
        .unwrap_or_else(|_| panic!("SpendDecider module missing: {SPEND_DECIDER_MODULE}"));
    assert!(
        decider_text.contains("SpendDecider"),
        "the ONE SpendDecider must exist in {SPEND_DECIDER_MODULE}"
    );

    assert!(
        hits.is_empty(),
        "R-6: a second budget decider is present ({} hit(s)):\n{}",
        hits.len(),
        hits.join("\n")
    );

    // Path-scoped (pre-registered, order deep-research-t2a Instrument
    // 3): the studio search tool's web_search path must be free of
    // ANY budget-decider identifier — including `check_budget`, which
    // the global scan cannot forbid because sovereign-core's
    // conversation-FRAME check is a different concept. The decider
    // move removed the search-path budget gate; a resurrected one
    // here is the exact R-6 shape.
    let search_rs_path = "studio/crates/sovereign-tools-base/src/search.rs";
    let search_rs = fs::read_to_string(root.join(search_rs_path))
        .unwrap_or_else(|_| panic!("{search_rs_path} missing"));
    for id in [
        "budget_allows",
        "decrement_budget",
        "BudgetView",
        "check_budget",
    ] {
        if search_rs.contains(id) {
            panic!(
                "R-6: {search_rs_path} contains a budget-decider identifier ({id}) — \
                 spend is gated once by the SpendDecider in sovereign-core"
            );
        }
    }
}
