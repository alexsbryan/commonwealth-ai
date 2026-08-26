// SPDX-License-Identifier: AGPL-3.0-or-later
//! `adopted` for the TURN TOOL REGISTRY — how far the three hosts disagree
//! about what a turn may do.
//!
//! # Why a number, and why this one
//!
//! `quality/TOPOLOGY.md` §3.5 lists "a capability wired in 2 of 3 hosts" among
//! the states the target topology forbids, and its reason is one line: *there
//! is one host*. Until Phase 6 makes that true, nothing in the build reports
//! how far from it we are — the same blind spot `runtime_commission_census`
//! covers for `Runtime::new`, one level down. A tool missing from a host is
//! not a compile error, not a test failure, and not visible in any diff
//! smaller than all three bootstraps side by side.
//!
//! Measured when this file was written (2026-08-25): **33 tools in the union,
//! 7 common to all three, 26 divergent.** `svrn chat` registers 11, the
//! desktop 21, the server 31. Two of the divergences read as defects rather
//! than policy and are worth naming, because a number alone does not make
//! anyone act:
//!
//! - `KnowledgeLookupTool` — the "unified knowledge-lookup front door" — is on
//!   the CLI and on NEITHER user-facing host.
//! - `AttachedDocumentSearchTool` is CLI-only. Its own comment says it is "the
//!   lever the book-report bench exposed as missing"; the desktop is where a
//!   user attaches a document.
//!
//! # This is a RATCHET, not a target
//!
//! It fails when the hosts diverge FURTHER. It does not fail on 26, because
//! collapsing them is a change to what a model may call on every turn — a
//! quality change wearing a refactor's clothes, and §18.4's warning about
//! tuning an unmeasured whole applies directly. The convergence belongs to
//! Phase 6, where the hosts stop having registries at all.
//!
//! # Named failing input (ARCH §18.1)
//!
//! Register a tool on one host's turn registry and not the other two. Nothing
//! else in the workspace notices — which is precisely how a 26-wide split came
//! to exist. This fails, prints the full matrix, and makes the author say
//! whether they meant to widen the split.
//!
//! # Instrument defects this file already survived
//!
//! Reusing the checklist `runtime_commission_census` paid for:
//!
//! - **The desktop builds TWO registries in one span.** `state.rs:1129` opens
//!   `mcp_tools` for the `/mcp` surface INSIDE the turn registry's span, and a
//!   bare `\.register\(` scan counted its nine code-intel tools as the
//!   desktop's turn tools — inflating it 21 → 30 and quietly making the split
//!   look narrower than it is. The receiver is bound to `tools` for that
//!   reason; do not relax it.
//! - **A comment naming a tool satisfies a text scan.** Comments are stripped
//!   before matching.
//! - **A registry that scans to zero is not agreement.** Each host must yield
//!   at least `MIN_TOOLS_PER_HOST`, or the extractor is broken and the census
//!   is reporting a fiction rather than a finding (ARCH §18.2).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The three hosts that build a turn tool registry, and where each starts.
///
/// These are the same three sites `runtime_commission_census` tracks — a host
/// that commissions a `Runtime` is exactly a host that must hand it tools.
const TURN_REGISTRIES: &[(&str, &str)] = &[
    // Was `sovereign-cli-llm/src/chat_cmd/bootstrap.rs` until 2026-08-25.
    // The registry moved into the shared recipe (TOPOLOGY §10 phase 5c), so
    // this row is no longer one host: it is what `svrn chat` AND
    // `sovereign daemon run` both register, from one list. That is the census
    // shrinking by construction rather than by anyone editing a baseline —
    // two of the columns this file was built to compare cannot disagree any
    // more, because there is nothing left for them to disagree with.
    // Two hops since 2026-08-26 (TOPOLOGY phase 7b). The recipe registers no
    // tool by name — it folds the `ToolBundle`s a host composes — so scanning
    // its source finds zero, which is what the instrument guard caught the day
    // the seam landed. The set a recipe host carries is now the union of the
    // bundles it composes, and `bundle_composed_tools` resolves it. The row's
    // membership is unchanged: `baseline_bundles` plus what `svrn chat` pushes,
    // which is what this row has always measured.
    (
        "recipe",
        "sovereign/crates/sovereign-runtime-recipe/src/lib.rs",
    ),
    (
        "desktop",
        "sovereign/crates/sovereign-desktop/src-tauri/src/state.rs",
    ),
    ("server", "sovereign/crates/sovereign-server/src/main.rs"),
];

/// The divergence measured on 2026-08-25. May shrink freely; a growth is the
/// failure this file exists to produce.
///
/// RE-MEASURED the same day, after the turn registry moved into
/// `sovereign-runtime-recipe` and one row started covering both `svrn chat`
/// and `sovereign daemon run`: **union 33, common 7, divergent 26 —
/// unchanged.** Worth writing down, because "we merged two hosts and the
/// number did not move" is the result, not a null one: the divergence was
/// never between those two. It is between the recipe and the desktop/server,
/// which is exactly where Phase 6 has to work. A baseline lowered on the
/// strength of a refactor that did not move it would be a ratchet with slack
/// pretending to be a gate.
/// RE-MEASURED AGAIN 2026-08-26, after the tool-bundle seam (TOPOLOGY phase
/// 7b) moved the recipe's registrations out of its source and into
/// `sovereign-tools::bundles`: **union 33, common 7, divergent 26 — unchanged
/// a second time.** That is the result the seam was supposed to produce and
/// the reason to state it: composing the same tools through bundles instead
/// of a hardcoded list is behaviour-preserving BY MEASUREMENT, not by
/// assertion. A refactor of a registry that moved this number would have been
/// a quality change wearing a refactor's clothes (ARCH §18.4).
///
/// Getting there cost two instrument defects, both caught by the guards this
/// file already carried: the recipe scanned to ZERO the moment its
/// registrations left its source (the `MIN_TOOLS_PER_HOST` check), and the
/// first bundle resolver missed `Box::new(sovereign_tools::bundles::ShellTools)`
/// — a path-qualified unit struct — and reported a 27th divergence that had
/// not happened.
const DIVERGENT_BASELINE: usize = 26;

/// Below this, the extractor found nothing and the run proves nothing.
const MIN_TOOLS_PER_HOST: usize = 8;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repo root is four levels above sovereign-core")
        .to_path_buf()
}

/// Strip `//` line comments. A doc comment naming a tool is prose, not a
/// registration, and counting it is the defect two prior censuses hit.
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The span from the turn registry's construction to the line that freezes it.
fn registry_span(src: &str) -> Option<String> {
    let lines: Vec<&str> = src.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.contains("ToolRegistry::new().with_cache("))?;
    // The freeze point is "the registry becomes an `Arc`", and it is spelled
    // two ways: a host binds it (`let tools = Arc::new(tools);`) while the
    // shared recipe RETURNS it (`Arc::new(tools)` as the tail expression).
    // Matching only the first is how this scanner failed the day the recipe
    // landed — and the right fix is to widen the marker, never to reshape the
    // production code so a scanner can find it.
    let end = lines[start..]
        .iter()
        .position(|l| l.contains("Arc::new(tools)"))?;
    Some(lines[start..=start + end].join("\n"))
}

/// Tool type names registered on `tools` within `span`.
///
/// The receiver is matched explicitly. `mcp_tools.register(...)` must NOT
/// count — see the instrument note in the module docs.
fn registered_tools(span: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    // Two registration verbs, and the receiver is checked for both. A host
    // writes `tools.register(Box::new(..))`; a `ToolBundle` writes
    // `reg.register_reporting(Box::new(..))`. Anything else — notably the
    // desktop's `mcp_tools.register(..)`, which sits inside the turn
    // registry's span and would inflate that host 21 -> 30 — is skipped.
    const VERBS: &[(&str, &str)] = &[
        (".register(", "tools"),
        (".register_reporting(", "reg"),
    ];
    for (verb, receiver) in VERBS {
        let mut at = 0usize;
        while let Some(rel) = span[at..].find(verb) {
            let idx = at + rel;
            at = idx + verb.len();

            let recv = span[..idx].trim_end();
            if !recv.ends_with(receiver) {
                continue;
            }
            let head = &recv[..recv.len() - receiver.len()];
            if head
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                continue;
            }

            // `Box::new(<Path>` — the constructed tool.
            let rest = span[at..].trim_start();
            let Some(after_box) = rest.strip_prefix("Box::new(") else {
                continue;
            };
            let path: String = after_box
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                .collect();

            // Drop the constructor segment so `X::new` and `X::with_web` agree.
            let mut parts: Vec<&str> = path.split("::").filter(|s| !s.is_empty()).collect();
            while parts.len() > 1 && is_ctor(parts[parts.len() - 1]) {
                parts.pop();
            }
            let Some(name) = parts.last() else { continue };
            // A lowercase head is a variable — the desktop registers
            // MCP-discovered tools from a loop, and a bundle re-registers a
            // `Vec<Box<dyn Tool>>` the same way. Neither is a named capability.
            if name.chars().next().is_some_and(char::is_lowercase) {
                continue;
            }
            out.insert((*name).to_string());
        }
    }
    out
}

fn is_ctor(seg: &str) -> bool {
    seg == "new"
        || seg == "declared"
        || seg.starts_with("with_")
        || seg.starts_with("from_")
        || seg.starts_with("new_")
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Where a bundle-composing host declares its families, and where the
/// families live. Both are scanned; a host that composes a bundle gets every
/// tool that bundle registers.
const BUNDLE_SOURCES: &[&str] = &[
    "sovereign/crates/sovereign-runtime-recipe/src/lib.rs",
    "sovereign/crates/sovereign-cli-llm/src/chat_cmd/bootstrap.rs",
];
const BUNDLE_DEFINITIONS: &str = "sovereign/crates/sovereign-tools/src/bundles.rs";

/// Bundle type names composed across `BUNDLE_SOURCES`.
///
/// Matches `<Name>::new(` and the unit-struct form `Box::new(<Name>)`, both of
/// which appear in a host's bundle vec.
fn composed_bundles(root: &Path) -> BTreeSet<String> {
    let defs = strip_comments(&read(root, BUNDLE_DEFINITIONS));
    let known: BTreeSet<String> = defs
        .match_indices("impl ToolBundle for ")
        .map(|(i, m)| {
            defs[i + m.len()..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        !known.is_empty(),
        "no `impl ToolBundle for` in {BUNDLE_DEFINITIONS} — the bundle scan is broken, \
         not the hosts (ARCH §18.2)"
    );

    let mut out = BTreeSet::new();
    for rel in BUNDLE_SOURCES {
        let src = strip_comments(&read(root, rel));
        for name in &known {
            // Three spellings a host uses: a constructor call, a unit struct
            // boxed bare, and a unit struct boxed through its full path
            // (`Box::new(sovereign_tools::bundles::ShellTools)`). Missing the
            // third is how the first run of this scan lost `ShellTool` and
            // reported a divergence that had not happened.
            if src.contains(&format!("{name}::new("))
                || src.contains(&format!("Box::new({name})"))
                || src.contains(&format!("::{name})"))
            {
                out.insert(name.clone());
            }
        }
    }
    out
}

/// Tools carried by the bundles a recipe host composes.
fn bundle_composed_tools(root: &Path) -> BTreeSet<String> {
    let defs = strip_comments(&read(root, BUNDLE_DEFINITIONS));
    let mut out = BTreeSet::new();
    for bundle in composed_bundles(root) {
        let marker = format!("impl ToolBundle for {bundle} {{");
        let Some(start) = defs.find(&marker) else {
            continue;
        };
        let rest = &defs[start..];
        let end = rest.find("\n}\n").map(|i| start + i).unwrap_or(defs.len());
        out.extend(registered_tools(&defs[start..end]));
    }
    out
}

fn census(root: &Path) -> BTreeMap<&'static str, BTreeSet<String>> {
    TURN_REGISTRIES
        .iter()
        .map(|(host, rel)| {
            if *host == "recipe" {
                return (*host, bundle_composed_tools(root));
            }
            let src = strip_comments(&read(root, rel));
            let span = registry_span(&src)
                .unwrap_or_else(|| panic!("{host}: no turn registry span in {rel}"));
            (*host, registered_tools(&span))
        })
        .collect()
}

fn matrix(sets: &BTreeMap<&'static str, BTreeSet<String>>) -> String {
    let union: BTreeSet<&String> = sets.values().flatten().collect();
    let mut out = format!("{:34}", "TOOL");
    for host in sets.keys() {
        out.push_str(&format!("{host:>12}"));
    }
    out.push('\n');
    for t in &union {
        out.push_str(&format!("{t:34}"));
        for s in sets.values() {
            out.push_str(&format!("{:>12}", if s.contains(*t) { "x" } else { "." }));
        }
        out.push('\n');
    }
    out
}

/// The instrument check, and it runs first: an extractor that finds nothing
/// would report perfect agreement.
#[test]
fn every_host_yields_a_registry() {
    let sets = census(&repo_root());
    for (host, tools) in &sets {
        assert!(
            tools.len() >= MIN_TOOLS_PER_HOST,
            "{host}: extracted only {} tools (min {MIN_TOOLS_PER_HOST}). \
             The scan is broken, not the hosts — a census that finds nothing \
             reports agreement it did not measure (ARCH §18.2).",
            tools.len()
        );
    }
}

#[test]
fn the_hosts_do_not_diverge_further_on_what_a_turn_may_do() {
    let sets = census(&repo_root());
    let union: BTreeSet<&String> = sets.values().flatten().collect();
    let common: BTreeSet<&String> = union
        .iter()
        .copied()
        .filter(|t| sets.values().all(|s| s.contains(*t)))
        .collect();
    let divergent = union.len() - common.len();

    assert!(
        divergent <= DIVERGENT_BASELINE,
        "turn tool registries diverged further: {divergent} tools are not on \
         every host (baseline {DIVERGENT_BASELINE}).\n\n{}\n\
         union {} · common {} · divergent {divergent}\n\n\
         A tool on one host and not the others is `TOPOLOGY.md` §3.5's \
         forbidden state 'a capability wired in 2 of 3 hosts'. Either register \
         it everywhere, or raise the baseline and say in the commit why this \
         host is different.",
        matrix(&sets),
        union.len(),
        common.len(),
    );
}
