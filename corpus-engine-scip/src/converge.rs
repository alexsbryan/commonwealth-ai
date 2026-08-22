// SPDX-License-Identifier: AGPL-3.0-or-later
//! Concept convergence over the SCIP graph — duplicated *identity*, the
//! half `dry_report` structurally cannot see.
//!
//! `sovereign_tools::code::dry_report` finds duplicated BEHAVIOUR: function
//! bodies that hash alike or embed alike. That is blind to the disease this
//! module measures, because six `ChatMessage` structs declared in six crates
//! have no bodies to compare — they are pure identity duplication, and the
//! only evidence is that one name is defined as a type in more than one
//! crate. Likewise `suggest_seams` decomposes an oversized FILE; this
//! decomposes an oversized NAME. Use all three; they do not overlap.
//!
//! Three questions, in the order you ask them during a convergence refactor:
//!
//!   1. What is duplicated, ranked?                      [`census`]
//!   2. For this noun — who defines it, who uses it,
//!      and which crate could own it?                    [`dossier`]
//!   3. Did the number go up?                            caller's ratchet
//!
//! Pure functions over `&[ScipSymbolRecord]` / `&[ScipRefRecord]` — the same
//! inputs as [`crate::arch_metrics`] and [`crate::capability_map`]. No I/O,
//! no embeddings, no source reads, no new dependencies.
//!
//! ## The owner computation, and why it is not a heuristic
//!
//! `TARGET_ARCHITECTURE.md` §6 states the tier rule in prose: *the lowest
//! tier that needs the noun owns it*. That is a graph query. [`dossier`]
//! collects every crate that references any definition of the noun, then
//! reports the crates that ALL of them already depend on — ranked by
//! coverage, then by out-degree ascending (lower out-degree = deeper tier).
//! Validated 2026-08-19 against `Verdict`: 8 user crates, top candidate
//! `sovereign-contracts` at 7/8, which is the home `quality/CONCEPTS.toml`
//! had already chosen by hand.
//!
//! The crates it does NOT cover are the finding — [`Dossier::gap`] names
//! them. A gap crate needs a new dependency edge or the row stays
//! `distinct`; either way it belongs on the order spec, not in step 3.
//!
//! ## Honest limitations, stated once
//!
//! - Same name is not same concept. Adjudication on this workspace found
//!   roughly half the two-crate tail is coincidence (`Layer` the ARCH tier
//!   vs `Layer` the doctor probe). The census DISCOVERS; a human
//!   DISPOSITIONS. Nothing here decides.
//! - [`CensusRow::kin`] (morphological family) OVER-collects by
//!   construction — `PartialKvVerdict` is probably not a `Verdict`. It
//!   exists because exact-name matching UNDER-collects badly: measured on
//!   this workspace, `Verdict` has 9 exact definitions and 49 in its
//!   suffix family, and `Answer`/`Capabilities`/`Record` score zero on
//!   exact match while carrying 6/8/48 kin respectively.
//! - The dependency graph is derived from OBSERVED references, not from
//!   `Cargo.toml`. A declared-but-unused dep is invisible here, which is
//!   the conservative direction: a candidate owner this reports is one the
//!   code already reaches.
//! - SCIP misses macro-expanded references and some dynamic dispatch, so a
//!   user crate list is a floor.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::capability_map::pkg_and_desc;
use crate::scip_graph::{ScipRefRecord, ScipSymbolRecord};

// ── SourceScope ─────────────────────────────────────────────────────────────────────

/// Which files count as first-party production code.
///
/// NOT `Scope` — `sovereign_contracts::types::routing::Scope` and
/// `sovereign_workflow::model::Scope` already carry that name for unrelated
/// concepts. Disposition `distinct`: this one scopes SOURCE FILES.
///
/// Every number this module produces depends on these clauses, which is why
/// [`Census::scope`] and [`Dossier::scope`] carry them into the output: a
/// count that travels without its method is the brittleness the convergence
/// program exists to end (`ARCH_PRINCIPLES` §18.4).
#[derive(Debug, Clone, Serialize)]
pub struct SourceScope {
    /// Path prefixes that count. Empty = everything not excluded.
    pub include_prefixes: Vec<String>,
    /// Substrings that disqualify a path.
    pub exclude_contains: Vec<String>,
}

impl Default for SourceScope {
    fn default() -> Self {
        Self {
            include_prefixes: Vec::new(),
            exclude_contains: [
                // Not ours.
                "vendor/",
                "node_modules/",
                ".cargo-container/",
                "research/",
                // Build output and agent worktree shadows. The worktree clause
                // is load-bearing: `.claude/worktrees/agent-*/` carries full
                // copies of first-party crates and will otherwise be counted
                // as additional definitions of every name in them.
                "target/",
                ".claude/",
                // Not production.
                "/tests/",
                "/benches/",
                "/examples/",
                "/build.rs",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

impl SourceScope {
    pub fn admits(&self, path: &str) -> bool {
        if self
            .exclude_contains
            .iter()
            .any(|e| path.contains(e.as_str()))
        {
            return false;
        }
        self.include_prefixes.is_empty()
            || self
                .include_prefixes
                .iter()
                .any(|p| path.starts_with(p.as_str()))
    }
}

// ── Descriptor parsing ────────────────────────────────────────────────────────

// Classification is NOT duplicated here. `crate::descriptor` is the one
// decider for what a SCIP descriptor names (§10.6); a census that carried its
// own private copy would be a specimen of the disease it measures.
use crate::descriptor::{descriptor_kind, leaf_name, DescriptorKind};

// ── Data ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TypeDef {
    pub name: String,
    pub krate: String,
    pub file: String,
    pub line: i32,
    pub qualified: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CensusRow {
    pub name: String,
    /// Distinct crates defining this name. Length > 1 is the census criterion.
    pub crates: Vec<String>,
    pub defs: Vec<TypeDef>,
    /// Names that end or start with this one. Over-collects; see module docs.
    pub kin: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Census {
    pub scope: SourceScope,
    /// Every first-party production top-level type definition considered.
    pub total_type_defs: usize,
    /// Names defined as a type in more than one crate, ranked.
    pub rows: Vec<CensusRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OwnerCandidate {
    pub krate: String,
    /// User crates that already depend on this candidate.
    pub covers: usize,
    pub of: usize,
    /// First-party out-degree — the tier proxy. Lower is deeper.
    pub out_degree: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Dossier {
    pub scope: SourceScope,
    pub name: String,
    pub defs: Vec<TypeDef>,
    pub kin: Vec<String>,
    pub user_crates: Vec<String>,
    pub reference_sites: usize,
    pub owner_candidates: Vec<OwnerCandidate>,
    /// User crates that do NOT reach the top candidate — each needs a new
    /// dependency edge, or the row stays `distinct`.
    pub gap: Vec<String>,
}

// ── Extraction ────────────────────────────────────────────────────────────────

/// Every first-party production top-level type definition in the graph.
///
/// Deduplicated on `(qualified_name, file)` — the SCIP exporter double-lists
/// some files under two path prefixes, which would otherwise inflate every
/// count in this module.
pub fn type_defs(symbols: &[ScipSymbolRecord], scope: &SourceScope) -> Vec<TypeDef> {
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut out = Vec::new();
    for s in symbols {
        if !scope.admits(&s.file_path) {
            continue;
        }
        let Some((pkg, desc)) = pkg_and_desc(&s.qualified_name) else {
            continue;
        };
        // `mod tests` items sit under a `/tests/` descriptor segment even when
        // the FILE is production — the colocated-test case the scope's
        // path clause cannot reach.
        if descriptor_kind(&s.qualified_name) != DescriptorKind::Type || desc.contains("/tests/") {
            continue;
        }
        if !seen.insert((s.qualified_name.clone(), s.file_path.clone())) {
            continue;
        }
        out.push(TypeDef {
            name: leaf_name(&s.qualified_name).to_string(),
            krate: pkg.to_string(),
            file: s.file_path.clone(),
            line: s.line_start,
            qualified: s.qualified_name.clone(),
        });
    }
    out
}

/// Crate-level dependency edges, derived from observed references.
///
/// Not `Cargo.toml`: this is what the code actually reaches, including
/// through re-exports Cargo cannot see. Same rationale as
/// [`crate::arch_metrics`]'s observed graph.
pub fn crate_dag(
    refs: &[ScipRefRecord],
    scope: &SourceScope,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut dag: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in refs {
        if !scope.admits(&r.file_path) {
            continue;
        }
        let (Some((from, _)), Some((to, _))) = (
            pkg_and_desc(&r.caller_qualified),
            pkg_and_desc(&r.callee_qualified),
        ) else {
            continue;
        };
        if from != to {
            dag.entry(from.to_string())
                .or_default()
                .insert(to.to_string());
        }
    }
    dag
}

fn kin_of(name: &str, all: &BTreeSet<&str>) -> Vec<String> {
    all.iter()
        .filter(|m| m.len() > name.len() && (m.ends_with(name) || m.starts_with(name)))
        .map(|m| m.to_string())
        .collect()
}

// ── Verb 1: census ────────────────────────────────────────────────────────────

/// Names defined as a type in more than one crate, ranked by crates spanned,
/// then kin, then definition count.
pub fn census(defs: &[TypeDef], scope: &SourceScope, with_kin: bool) -> Census {
    let all_names: BTreeSet<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    let mut by_name: BTreeMap<&str, Vec<&TypeDef>> = BTreeMap::new();
    for d in defs {
        by_name.entry(d.name.as_str()).or_default().push(d);
    }

    let mut rows: Vec<CensusRow> = by_name
        .into_iter()
        .filter_map(|(name, ds)| {
            let crates: BTreeSet<&str> = ds.iter().map(|d| d.krate.as_str()).collect();
            if crates.len() < 2 {
                return None;
            }
            Some(CensusRow {
                name: name.to_string(),
                crates: crates.into_iter().map(String::from).collect(),
                defs: ds.into_iter().cloned().collect(),
                kin: if with_kin {
                    kin_of(name, &all_names)
                } else {
                    Vec::new()
                },
            })
        })
        .collect();

    rows.sort_by(|a, b| {
        b.crates
            .len()
            .cmp(&a.crates.len())
            .then(b.kin.len().cmp(&a.kin.len()))
            .then(b.defs.len().cmp(&a.defs.len()))
            .then(a.name.cmp(&b.name))
    });

    Census {
        scope: scope.clone(),
        total_type_defs: defs.len(),
        rows,
    }
}

/// The ratchet number: how many names are defined as a type in >1 crate.
pub fn duplicate_count(defs: &[TypeDef]) -> usize {
    let mut by_name: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for d in defs {
        by_name
            .entry(d.name.as_str())
            .or_default()
            .insert(d.krate.as_str());
    }
    by_name.values().filter(|c| c.len() > 1).count()
}

// ── Verb 2: dossier ───────────────────────────────────────────────────────────

/// Everything you need before moving one noun: definitions, users, the crate
/// that could own it, and the crates that cannot reach that owner.
pub fn dossier(
    name: &str,
    defs: &[TypeDef],
    refs: &[ScipRefRecord],
    dag: &BTreeMap<String, BTreeSet<String>>,
    scope: &SourceScope,
) -> Dossier {
    let all_names: BTreeSet<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    let mine: Vec<TypeDef> = defs.iter().filter(|d| d.name == name).cloned().collect();
    let owned_by: BTreeSet<&str> = mine.iter().map(|d| d.krate.as_str()).collect();
    let qualified: BTreeSet<&str> = mine.iter().map(|d| d.qualified.as_str()).collect();
    // First-party = crates that DEFINE an in-scope type, UNION crates that
    // make an in-scope reference (the `dag`'s keys — a caller in an admitted
    // file is first-party by the same filter). The union matters: a binary
    // crate that only CONSUMES types defines none, and deriving the set from
    // definitions alone silently drops every such crate from `user_crates`.
    let first_party: BTreeSet<&str> = defs
        .iter()
        .map(|d| d.krate.as_str())
        .chain(dag.keys().map(|k| k.as_str()))
        .collect();

    let mut users: BTreeSet<String> = BTreeSet::new();
    let mut sites = 0usize;
    for r in refs {
        if !qualified.contains(r.callee_qualified.as_str()) || !scope.admits(&r.file_path) {
            continue;
        }
        if let Some((pkg, _)) = pkg_and_desc(&r.caller_qualified) {
            if first_party.contains(pkg) {
                users.insert(pkg.to_string());
                sites += 1;
            }
        }
    }

    // A candidate must be reachable from EVERY user crate. Rank by coverage,
    // then by out-degree ascending — the lowest tier that serves them all.
    let mut cover: BTreeMap<&str, usize> = BTreeMap::new();
    for u in &users {
        for t in dag.get(u.as_str()).into_iter().flatten() {
            if first_party.contains(t.as_str()) && !owned_by.contains(t.as_str()) {
                *cover.entry(t.as_str()).or_default() += 1;
            }
        }
    }
    let mut owner_candidates: Vec<OwnerCandidate> = cover
        .into_iter()
        .map(|(krate, covers)| OwnerCandidate {
            covers,
            of: users.len(),
            out_degree: dag
                .get(krate)
                .map(|s| {
                    s.iter()
                        .filter(|t| first_party.contains(t.as_str()))
                        .count()
                })
                .unwrap_or(0),
            krate: krate.to_string(),
        })
        .collect();
    owner_candidates.sort_by(|a, b| {
        b.covers
            .cmp(&a.covers)
            .then(a.out_degree.cmp(&b.out_degree))
            .then(a.krate.cmp(&b.krate))
    });

    let gap = owner_candidates
        .first()
        .map(|top| {
            users
                .iter()
                .filter(|u| {
                    !dag.get(u.as_str())
                        .map(|d| d.contains(&top.krate))
                        .unwrap_or(false)
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Dossier {
        scope: scope.clone(),
        name: name.to_string(),
        kin: kin_of(name, &all_names),
        defs: mine,
        user_crates: users.into_iter().collect(),
        reference_sites: sites,
        owner_candidates,
        gap,
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render_scope(scope: &SourceScope, out: &mut String) {
    out.push_str("scope: ");
    if scope.include_prefixes.is_empty() {
        out.push_str("all paths");
    } else {
        out.push_str(&scope.include_prefixes.join(", "));
    }
    out.push_str(" minus ");
    out.push_str(&scope.exclude_contains.join(" "));
    out.push('\n');
}

pub fn render_census(c: &Census, limit: usize, with_kin: bool) -> String {
    let mut s = String::new();
    render_scope(&c.scope, &mut s);
    s.push_str(&format!(
        "\nfirst-party production type definitions : {}\n",
        c.total_type_defs
    ));
    s.push_str(&format!(
        "names defined as a type in >1 crate     : {}   <- the ratchet number\n",
        c.rows.len()
    ));
    if with_kin {
        let kin: usize = c.rows.iter().map(|r| r.kin.len()).sum();
        s.push_str(&format!(
            "morphological kin of those names        : {kin}   (over-collects by design — see `converge noun`)\n"
        ));
    }
    s.push('\n');
    if with_kin {
        s.push_str("crates  defs   kin  name\n------  ----  ----  ----\n");
    } else {
        s.push_str("crates  defs  name\n------  ----  ----\n");
    }
    for r in c.rows.iter().take(limit) {
        if with_kin {
            s.push_str(&format!(
                "{:>6}  {:>4}  {:>4}  {}\n",
                r.crates.len(),
                r.defs.len(),
                r.kin.len(),
                r.name
            ));
        } else {
            s.push_str(&format!(
                "{:>6}  {:>4}  {}\n",
                r.crates.len(),
                r.defs.len(),
                r.name
            ));
        }
    }
    if c.rows.len() > limit {
        s.push_str(&format!(
            "\n... {} more (--limit 0 for all)\n",
            c.rows.len() - limit
        ));
    }
    s.push_str("\nSame name is not same concept — this DISCOVERS, a human DISPOSITIONS.\n");
    s.push_str("Next: `svrn code converge noun <Name>` for one row's dossier.\n");
    s
}

pub fn render_dossier(d: &Dossier) -> String {
    let mut s = String::new();
    render_scope(&d.scope, &mut s);
    s.push_str(&format!("\nnoun: {}\n", d.name));

    s.push_str(&format!(
        "\ndefinitions ({}), first-party production:\n",
        d.defs.len()
    ));
    if d.defs.is_empty() {
        s.push_str("  none — check the spelling, or the name is defined only in test code\n");
    }
    let mut defs = d.defs.clone();
    defs.sort_by(|a, b| a.krate.cmp(&b.krate).then(a.file.cmp(&b.file)));
    for def in &defs {
        s.push_str(&format!("  {:<30} {}:{}\n", def.krate, def.file, def.line));
    }

    if !d.kin.is_empty() {
        s.push_str(&format!(
            "\nmorphological kin ({}) — same-family names an exact-match census cannot see:\n",
            d.kin.len()
        ));
        for chunk in d.kin.chunks(4) {
            s.push_str(&format!("  {}\n", chunk.join("  ")));
        }
        s.push_str("  (over-collects: adjudicate each, do not converge on this list)\n");
    }

    s.push_str(&format!(
        "\nused by {} first-party crate(s), {} reference site(s):\n",
        d.user_crates.len(),
        d.reference_sites
    ));
    for u in &d.user_crates {
        s.push_str(&format!("  {u}\n"));
    }

    s.push_str("\ncanonical-owner candidates (crates every user already depends on):\n");
    if d.owner_candidates.is_empty() {
        s.push_str(
            "  none — the users share no dependency. This noun needs a new home crate,\n\
             \x20 or the definitions are genuinely distinct concepts.\n",
        );
    }
    for (i, c) in d.owner_candidates.iter().take(5).enumerate() {
        let mark = if i == 0 { "   <- lowest tier" } else { "" };
        s.push_str(&format!(
            "  {:<30} covers {}/{}  (out-degree {}){}\n",
            c.krate, c.covers, c.of, c.out_degree, mark
        ));
    }

    if !d.gap.is_empty() {
        let top = &d.owner_candidates[0].krate;
        s.push_str(&format!("\ndependency gap — these do NOT reach {top}:\n"));
        for g in &d.gap {
            s.push_str(&format!(
                "  {g:<30} needs a new dep edge, or stays `distinct`\n"
            ));
        }
    }
    s.push_str("\nBehaviour duplication is a different question: `svrn code dry-report`.\n");
    s
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(pkg: &str, desc: &str, file: &str, line: i32) -> ScipSymbolRecord {
        ScipSymbolRecord {
            name: leaf_name(desc).to_string(),
            qualified_name: format!("rust-analyzer cargo {pkg} 0.1.0 {desc}"),
            kind: "unknown".into(),
            file_path: file.into(),
            line_start: line,
            line_end: line + 5,
            language: "rust".into(),
        }
    }

    fn rf(
        from_pkg: &str,
        from_desc: &str,
        to_pkg: &str,
        to_desc: &str,
        file: &str,
    ) -> ScipRefRecord {
        ScipRefRecord {
            caller_symbol: from_desc.into(),
            callee_symbol: to_desc.into(),
            caller_qualified: format!("rust-analyzer cargo {from_pkg} 0.1.0 {from_desc}"),
            callee_qualified: format!("rust-analyzer cargo {to_pkg} 0.1.0 {to_desc}"),
            file_path: file.into(),
            line: 1,
            ref_kind: "direct".into(),
        }
    }

    #[test]
    fn only_top_level_types_enter_the_census() {
        // Enum variants, methods and consts share a name-space with types and
        // must not be counted as definitions of one.
        let syms = vec![
            sym("a", "types/ScoredChunk#", "sovereign/crates/a/src/t.rs", 1),
            sym(
                "a",
                "StartupOutcome#Failed#",
                "sovereign/crates/a/src/t.rs",
                2,
            ),
            sym(
                "a",
                "impl#[Runtime]handle_message().",
                "sovereign/crates/a/src/t.rs",
                3,
            ),
            sym("a", "workflow_cmd/HELP.", "sovereign/crates/a/src/t.rs", 4),
            sym("a", "crate/", "sovereign/crates/a/src/t.rs", 5),
        ];
        let defs = type_defs(&syms, &SourceScope::default());
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "ScoredChunk");
    }

    #[test]
    fn scope_excludes_agent_worktree_shadows_and_test_paths() {
        let s = SourceScope::default();
        assert!(s.admits("sovereign/crates/sovereign-core/src/lib.rs"));
        // The worktree clause: without it, every first-party name is
        // double-counted from the agent worktree copies.
        assert!(!s.admits(".claude/worktrees/agent-abc/sovereign/crates/x/src/lib.rs"));
        assert!(!s.admits("sovereign/crates/x/tests/e2e.rs"));
        assert!(!s.admits("vendor/foo/src/lib.rs"));
        assert!(!s.admits("research/spike/src/lib.rs"));
    }

    #[test]
    fn colocated_mod_tests_types_are_out_of_scope() {
        // The FILE is production; the descriptor says `mod tests`. A test
        // helper named `Evidence` must not enter the census.
        let syms = vec![sym(
            "crate_a",
            "thing/tests/Evidence#",
            "a/src/thing.rs",
            400,
        )];
        assert!(type_defs(&syms, &SourceScope::default()).is_empty());
    }

    #[test]
    fn double_listed_files_are_deduplicated() {
        // SCIP lists some files under two path prefixes; without the dedup
        // every count in this module inflates.
        let syms = vec![
            sym("crate_a", "m/Verdict#", "sovereign/crates/a/src/m.rs", 10),
            sym("crate_a", "m/Verdict#", "sovereign/crates/a/src/m.rs", 10),
        ];
        assert_eq!(type_defs(&syms, &SourceScope::default()).len(), 1);
    }

    #[test]
    fn census_reports_only_names_spanning_more_than_one_crate() {
        let syms = vec![
            sym("crate_a", "m/Verdict#", "a/src/m.rs", 10),
            sym("crate_b", "n/Verdict#", "b/src/n.rs", 20),
            sym("crate_a", "m/Solo#", "a/src/m.rs", 30),
            // Same name twice in ONE crate is not a census hit (it is a
            // same-crate duplicate — a real finding, but not this metric).
            sym("crate_a", "p/Twice#", "a/src/p.rs", 40),
            sym("crate_a", "q/Twice#", "a/src/q.rs", 50),
        ];
        let defs = type_defs(&syms, &SourceScope::default());
        let c = census(&defs, &SourceScope::default(), false);
        assert_eq!(c.total_type_defs, 5);
        assert_eq!(c.rows.len(), 1);
        assert_eq!(c.rows[0].name, "Verdict");
        assert_eq!(duplicate_count(&defs), 1);
    }

    #[test]
    fn kin_catches_the_family_exact_matching_misses() {
        let syms = vec![
            sym("crate_a", "m/Verdict#", "a/src/m.rs", 10),
            sym("crate_b", "n/Verdict#", "b/src/n.rs", 20),
            sym("crate_c", "o/GateVerdict#", "c/src/o.rs", 30),
            sym("crate_d", "p/VerdictRow#", "d/src/p.rs", 40),
            sym("crate_e", "q/Unrelated#", "e/src/q.rs", 50),
        ];
        let defs = type_defs(&syms, &SourceScope::default());
        let c = census(&defs, &SourceScope::default(), true);
        assert_eq!(c.rows[0].kin, vec!["GateVerdict", "VerdictRow"]);
    }

    #[test]
    fn owner_is_the_crate_every_user_already_depends_on() {
        let syms = vec![
            sym("core", "m/Verdict#", "sovereign/crates/core/src/m.rs", 10),
            sym("mesh", "n/Verdict#", "sovereign/crates/mesh/src/n.rs", 20),
            sym(
                "contracts",
                "c/Anchor#",
                "sovereign/crates/contracts/src/c.rs",
                1,
            ),
            sym("store", "s/Anchor#", "sovereign/crates/store/src/s.rs", 1),
        ];
        let defs = type_defs(&syms, &SourceScope::default());
        let refs = vec![
            // Both definitions are used by `cli`.
            rf(
                "cli",
                "f().",
                "core",
                "m/Verdict#",
                "sovereign/crates/cli/src/a.rs",
            ),
            rf(
                "cli",
                "f().",
                "mesh",
                "n/Verdict#",
                "sovereign/crates/cli/src/a.rs",
            ),
            rf(
                "eval",
                "g().",
                "core",
                "m/Verdict#",
                "sovereign/crates/eval/src/b.rs",
            ),
            // `contracts` is reachable from both users; `store` only from cli.
            rf(
                "cli",
                "f().",
                "contracts",
                "c/Anchor#",
                "sovereign/crates/cli/src/a.rs",
            ),
            rf(
                "eval",
                "g().",
                "contracts",
                "c/Anchor#",
                "sovereign/crates/eval/src/b.rs",
            ),
            rf(
                "cli",
                "f().",
                "store",
                "s/Anchor#",
                "sovereign/crates/cli/src/a.rs",
            ),
        ];
        let scope = SourceScope::default();
        let dag = crate_dag(&refs, &scope);
        let d = dossier("Verdict", &defs, &refs, &dag, &scope);

        assert_eq!(d.user_crates, vec!["cli", "eval"]);
        assert_eq!(d.reference_sites, 3);
        assert_eq!(d.owner_candidates[0].krate, "contracts");
        assert_eq!(d.owner_candidates[0].covers, 2);
        assert_eq!(d.owner_candidates[0].of, 2);
        // `store` covers only one user, so it must not outrank contracts.
        assert!(d
            .owner_candidates
            .iter()
            .any(|c| c.krate == "store" && c.covers == 1));
        assert!(d.gap.is_empty());
    }

    #[test]
    fn a_user_that_cannot_reach_the_owner_is_reported_as_a_gap() {
        let syms = vec![
            sym("core", "m/Verdict#", "sovereign/crates/core/src/m.rs", 10),
            sym(
                "archaeology",
                "n/Verdict#",
                "sovereign/crates/arch/src/n.rs",
                20,
            ),
            sym(
                "contracts",
                "c/Anchor#",
                "sovereign/crates/contracts/src/c.rs",
                1,
            ),
        ];
        let defs = type_defs(&syms, &SourceScope::default());
        let refs = vec![
            rf(
                "cli",
                "f().",
                "core",
                "m/Verdict#",
                "sovereign/crates/cli/src/a.rs",
            ),
            rf(
                "archaeology",
                "h().",
                "archaeology",
                "n/Verdict#",
                "sovereign/crates/arch/src/n.rs",
            ),
            // Only `cli` reaches contracts. `archaeology` does not.
            rf(
                "cli",
                "f().",
                "contracts",
                "c/Anchor#",
                "sovereign/crates/cli/src/a.rs",
            ),
        ];
        let scope = SourceScope::default();
        let dag = crate_dag(&refs, &scope);
        let d = dossier("Verdict", &defs, &refs, &dag, &scope);

        assert_eq!(d.owner_candidates[0].krate, "contracts");
        assert_eq!(d.owner_candidates[0].covers, 1);
        assert_eq!(d.owner_candidates[0].of, 2);
        assert_eq!(
            d.gap,
            vec!["archaeology"],
            "the unreachable user is the finding"
        );
    }

    #[test]
    fn no_shared_dependency_yields_no_candidate_rather_than_a_guess() {
        let syms = vec![
            sym("a", "m/Thing#", "sovereign/crates/a/src/m.rs", 1),
            sym("b", "n/Thing#", "sovereign/crates/b/src/n.rs", 1),
        ];
        let defs = type_defs(&syms, &SourceScope::default());
        let refs = vec![
            rf("a", "f().", "a", "m/Thing#", "sovereign/crates/a/src/m.rs"),
            rf("b", "g().", "b", "n/Thing#", "sovereign/crates/b/src/n.rs"),
        ];
        let scope = SourceScope::default();
        let dag = crate_dag(&refs, &scope);
        let d = dossier("Thing", &defs, &refs, &dag, &scope);
        assert!(d.owner_candidates.is_empty());
        assert!(d.gap.is_empty());
        assert!(render_dossier(&d).contains("needs a new home crate"));
    }
}
