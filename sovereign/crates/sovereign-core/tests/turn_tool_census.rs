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
//! desktop 21, the server 31.
//!
//! **CORRECTION, 2026-08-26 — THE DESKTOP NUMBER AND ONE OF THE TWO NAMED
//! DEFECTS WERE THE INSTRUMENT, NOT THE HOSTS.** The scan skipped every
//! registration whose argument was a variable, on the stated grounds that such
//! a call is an MCP loop or a re-registered vec. That is true of those, and
//! false of a tool assembled over several lines because its wiring is
//! conditional — which is how the desktop registers `KnowledgeLookupTool` (and
//! `CapabilityRequestTool`). So:
//!
//! - `KnowledgeLookupTool` **is** on the desktop and always was. The claim
//!   below that it was on "NEITHER user-facing host" was never true, and it is
//!   the shape of finding someone acts on — the fix would have been a second
//!   registration for a tool already there.
//! - The desktop registers **23**, not 21.
//! - Divergence is unchanged at 26: both corrections move a tool from one
//!   two-host pair to another.
//!
//! `resolve_binding` closes it, and anything the scan still cannot resolve is
//! now returned rather than dropped (ARCH §18.4 — validate the instrument
//! before the result).
//!
//! The other named divergence stands, and is worth acting on:
//!
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
const TURN_REGISTRIES: &[(&str, &str, Extraction)] = &[
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
        Extraction::Bundles(&[
            "sovereign/crates/sovereign-runtime-recipe/src/lib.rs",
            "sovereign/crates/sovereign-cli-llm/src/chat_cmd/bootstrap.rs",
        ]),
    ),
    // Composed since 2026-08-26 (TOPOLOGY phase 7). Before that this row was
    // 23 `tools.register` calls in a 1,659-line function; the span scanner
    // that read them found NO span the moment the desktop adopted the recipe,
    // which is the instrument guard doing its job rather than a regression.
    (
        "desktop",
        "sovereign/crates/sovereign-desktop/src-tauri/src/state.rs",
        Extraction::Bundles(&["sovereign/crates/sovereign-desktop/src-tauri/src/state.rs"]),
    ),
    // Composed since 2026-08-26 as well, which leaves NO host on the direct
    // path. `Extraction::Direct` is kept because the next host to appear will
    // arrive that way, and deleting the arm would make the census silently
    // wrong about it rather than loudly.
    (
        "server",
        "sovereign/crates/sovereign-server/src/main.rs",
        Extraction::Bundles(&["sovereign/crates/sovereign-server/src/main.rs"]),
    ),
];

/// How a host's turn tools are read off its source.
///
/// Two mechanisms because the hosts are mid-migration, and the census has to
/// keep measuring the ones that have not moved. A host that composes bundles
/// registers no tool by name, so scanning its source for `tools.register`
/// finds zero — reporting that as "this host carries nothing" would be the
/// census inventing agreement it never measured (ARCH §18.2).
enum Extraction {
    /// The host composes `ToolBundle`s. Its set is the union of what those
    /// bundles register, and these are the sources that declare which ones it
    /// composes.
    Bundles(&'static [&'static str]),
    /// The host builds a registry and writes `tools.register(..)` into it.
    Direct,
}

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
/// RE-MEASURED 2026-08-26 after the desktop adopted the recipe (phase 7):
/// **union 33, common 8, divergent 25.** One tool moved from divergent to
/// common (`ParcelAnalyticsTool`, which the desktop gained by composing
/// `CoreTurnTools` whole) and `AttachedDocumentSearchTool` moved from
/// CLI-only to CLI-and-desktop, which is the gap this file's header named as
/// worth acting on: the desktop is where a user attaches a document.
/// RE-MEASURED 2026-08-26 after BOTH remaining hosts adopted the recipe
/// (TOPOLOGY §10 phase 7): **union 33, common 10, divergent 23**, down from
/// 26. Three tools crossed from divergent to common, and two of them are the
/// gap this file's header named as worth acting on — `KnowledgeLookupTool`
/// and `AttachedDocumentSearchTool` were CLI-only, and the server now carries
/// both because composing `KnowledgeFrontDoor` whole is what adopting the
/// baseline MEANS. `ParcelAnalyticsTool` is the third.
///
/// What is left is not drift. The recipe row is `svrn chat` + `sovereign
/// daemon run`, and the twenty tools it lacks are code intel, notes,
/// recipe-authoring and compute — families those two hosts do not offer and
/// have never claimed to. Closing THAT is a decision about what a CLI turn
/// may do, not a refactor, which is exactly the line this ratchet was built
/// to keep visible.
const DIVERGENT_BASELINE: usize = 23;

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
    registered_tools_reporting(span).0
}

/// The tools this span registers, plus the variable names whose type the scan
/// could NOT resolve. The second half is what keeps the first honest.
fn registered_tools_reporting(span: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut out = BTreeSet::new();
    let mut unresolved = BTreeSet::new();
    // Two registration verbs, and the receiver is checked for both. A host
    // writes `tools.register(Box::new(..))`; a `ToolBundle` writes
    // `reg.register_reporting(Box::new(..))`. Anything else — notably the
    // desktop's `mcp_tools.register(..)`, which sits inside the turn
    // registry's span and would inflate that host 21 -> 30 — is skipped.
    const VERBS: &[(&str, &str)] = &[(".register(", "tools"), (".register_reporting(", "reg")];
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
            // A lowercase head is a VARIABLE, and until 2026-08-26 every one
            // was skipped on the stated grounds that "neither is a named
            // capability". That is true of the desktop's MCP loop and of a
            // bundle re-registering a `Vec<Box<dyn Tool>>`. It is FALSE of a
            // tool built up over several lines because its wiring is
            // conditional, and the desktop has exactly that:
            //
            //     let mut tool = sovereign_tools::KnowledgeLookupTool::new(..);
            //     if .. { tool = tool.with_notes(..) }
            //     tools.register(Box::new(tool.declared()));
            //
            // The scan reported the desktop as LACKING `KnowledgeLookupTool`,
            // this file's own header wrote that up as a named defect ("on the
            // CLI and on NEITHER user-facing host"), and it was never true.
            // A census that silently drops a true positive reports agreement
            // it did not measure (ARCH §18.4 — validate the instrument first).
            //
            // So: resolve the binding when the span declares one, and hand
            // back anything still unresolved instead of dropping it.
            if name.chars().next().is_some_and(char::is_lowercase) {
                match resolve_binding(&span[..idx], name) {
                    Some(bound) => {
                        out.insert(bound);
                    }
                    None => {
                        unresolved.insert((*name).to_string());
                    }
                }
                continue;
            }
            out.insert((*name).to_string());
        }
    }
    (out, unresolved)
}

/// The most recent `let [mut] <name> = <Path>::…` before the registration.
///
/// Deliberately narrow: it resolves the ONE shape a host uses when a tool's
/// wiring is conditional. Anything else stays unresolved and is reported
/// rather than guessed at — a wrong answer here is worse than no answer,
/// because it would silently move a host's column in the matrix.
fn resolve_binding(before: &str, var: &str) -> Option<String> {
    let mut found = None;
    for pat in [format!("let mut {var} ="), format!("let {var} =")] {
        if let Some(at) = before.rfind(&pat) {
            let rhs = before[at + pat.len()..].trim_start();
            let path: String = rhs
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                .collect();
            let mut parts: Vec<&str> = path.split("::").filter(|s| !s.is_empty()).collect();
            while parts.len() > 1 && is_ctor(parts[parts.len() - 1]) {
                parts.pop();
            }
            if let Some(name) = parts.last() {
                if name.chars().next().is_some_and(char::is_uppercase) {
                    // Prefer the binding nearest the registration.
                    if found.as_ref().is_none_or(|_: &(usize, String)| true) {
                        found = Some((at, (*name).to_string()));
                    }
                }
            }
        }
    }
    found.map(|(_, n)| n)
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

/// Where the bundle families are DEFINED. A host composing one gets every
/// tool that bundle registers, wherever the bundle lives.
///
/// Two files since 2026-08-26: the workflow-authoring family is implemented
/// beside the workflow host rather than in `sovereign-tools`, and a scan that
/// knew only the first file would have reported the desktop as losing it.
const BUNDLE_DEFINITIONS: &[&str] = &[
    "sovereign/crates/sovereign-tools/src/bundles.rs",
    "studio/crates/sovereign-workflow-host/src/author.rs",
];

/// The `ToolBundle` impls across `BUNDLE_DEFINITIONS`, as one text.
///
/// The marker is `ToolBundle for `, not `impl ToolBundle for `: an impl
/// written against the fully-qualified trait
/// (`impl sovereign_contracts::tool_bundle::ToolBundle for X`) is the same
/// declaration, and matching only the short form is how a family goes missing
/// from a census that looks like it ran.
fn bundle_defs(root: &Path) -> String {
    BUNDLE_DEFINITIONS
        .iter()
        .map(|rel| strip_comments(&read(root, rel)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where `baseline_bundles` is defined — the families EVERY recipe host gets
/// without naming one of them.
const BASELINE_BUNDLES_SITE: &str = "sovereign/crates/sovereign-runtime-recipe/src/lib.rs";

/// `sources`, plus the baseline site when any of them calls into it.
///
/// A host writes `baseline_bundles(..)` and then pushes its extras, so the
/// families it carries are not all named in its own file. Scanning only the
/// file reported the desktop as carrying its five extra bundles and NONE of
/// the nine baseline tools it has always had — 32 divergent against a
/// baseline of 26, entirely invented by the instrument. Follow the call
/// (ARCH §18.4: validate the instrument before the result).
fn effective_sources(root: &Path, sources: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = sources.iter().map(|s| (*s).to_string()).collect();
    let follows = sources
        .iter()
        .any(|rel| strip_comments(&read(root, rel)).contains("baseline_bundles("));
    if follows && !out.iter().any(|s| s == BASELINE_BUNDLES_SITE) {
        out.push(BASELINE_BUNDLES_SITE.to_string());
    }
    out
}

/// Bundle type names composed across `sources`.
///
/// Matches `<Name>::new(` and the unit-struct form `Box::new(<Name>)`, both of
/// which appear in a host's bundle vec.
fn composed_bundles(root: &Path, sources: &[&str]) -> BTreeSet<String> {
    let defs = bundle_defs(root);
    let known: BTreeSet<String> = defs
        .match_indices("ToolBundle for ")
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
        "no `ToolBundle for` impl in {BUNDLE_DEFINITIONS:?} — the bundle scan is broken, \
         not the hosts (ARCH §18.2)"
    );

    let mut out = BTreeSet::new();
    for rel in effective_sources(root, sources) {
        let src = strip_comments(&read(root, &rel));
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

/// Tools carried by the bundles a host composes.
fn bundle_composed_tools(root: &Path, sources: &[&str]) -> BTreeSet<String> {
    let defs = bundle_defs(root);
    let mut out = BTreeSet::new();
    for bundle in composed_bundles(root, sources) {
        let marker = format!("ToolBundle for {bundle} {{");
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
        .map(|(host, rel, how)| match how {
            Extraction::Bundles(sources) => (*host, bundle_composed_tools(root, sources)),
            Extraction::Direct => {
                let src = strip_comments(&read(root, rel));
                let span = registry_span(&src)
                    .unwrap_or_else(|| panic!("{host}: no turn registry span in {rel}"));
                (*host, registered_tools(&span))
            }
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
