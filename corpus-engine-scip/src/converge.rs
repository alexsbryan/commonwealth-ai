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
//! ## The census counts only what adoption could retire (2026-08-21)
//!
//! A name defined as a type in two crates is not automatically a convergence
//! candidate: if neither definition is reachable from outside its own crate,
//! there is nothing to import and nothing to switch to. Measured at
//! `b325f22c`, 239 of 275 rows (87%) were exactly that. Since 2026-08-21 a row
//! counts toward the ratchet only when at least two crates each hold a
//! definition another crate's PRODUCTION code ALREADY references —
//! [`cross_crate_reached`], which carries the evidence and the two-instrument
//! cross-check. Measured at `4f64bdb2`: 255 colliding names, 33 countable, and
//! `quality/baselines/concepts.txt` re-minted 279 -> 33 in the same commit. The wider number
//! did not disappear: [`Census::colliding_names`] still reports it, every row
//! is still in [`Census::rows`], and `converge census --local` lists the ones
//! set aside. A narrowing that hid what it removed could not be checked
//! (§18.6).
//!
//! What the narrowing can now MISS, stated where it does not flatter the
//! number: a genuine fork whose two definitions are both still local — the
//! duplicate that was minted this week and not yet imported anywhere. It is
//! invisible to the ratchet until someone uses it. [`crate::shape`] is the
//! feed that sees those, because it matches on field shape and asks nothing
//! about reach.
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
///
/// Exclusions match whole path SEGMENTS. They were substrings until
/// 2026-08-20, when the campaign's own instrument-validation rung found that
/// `"research/"` — written for the top-level spike tree — also swallowed
/// `sovereign-core/src/deep_research/`, the module the noun-convergence
/// program cites as the reason it exists. Six of the ten patterns had that
/// same shape (a bare token a longer segment can end with), so the repair is
/// the matching rule, not the one string.
///
/// `external` joined the list on 2026-08-20 for the same reason `vendor` is on
/// it — `sovereign/bench/external/` is third-party repo checkouts, and it was
/// 68% of everything `dry_report` reported once that report started using this
/// scope at all.
#[derive(Debug, Clone, Serialize)]
pub struct SourceScope {
    /// Path prefixes that count. Empty = everything not excluded.
    pub include_prefixes: Vec<String>,
    /// Whole `/`-separated path segments that disqualify a path. A pattern
    /// never matches a substring of a longer segment.
    pub exclude_segments: Vec<String>,
}

impl Default for SourceScope {
    fn default() -> Self {
        Self {
            include_prefixes: Vec::new(),
            // Segment names, so no leading or trailing slash: the slashes in
            // the old spelling were doing the anchoring by hand, and doing
            // only half of it (`research/` anchored the right side, never the
            // left; `/tests/` anchored both, which is why that entry never
            // misfired).
            exclude_segments: [
                // Not ours.
                "vendor",
                "node_modules",
                ".cargo-container",
                "research",
                // `sovereign/bench/external/` holds full third-party repo
                // checkouts — SWE-bench task repos, RewardBench fixtures. Same
                // rubric as `vendor`, and measured as the single largest term
                // in the duplication report: with the segment list as it stood
                // on 2026-08-20, `dry-report` read 1,982 groups / ~39,053
                // redundant lines, of which 1,416 groups / ~26,687 lines were
                // fixture repos. Excluding it lands the report at 566 groups /
                // ~12,366 lines, which is the first-party production figure
                // this scope claims to produce.
                //
                // It moves the CENSUS too, and in the direction that flatters
                // the campaign — so it is stated here rather than only where it
                // helps (§18.6). Measured at `285878ff`: type definitions
                // 5,465 -> 5,139 (-326), and the ratchet number (names defined
                // as a type in >1 crate) 278 -> 275. `Relationship` leaves the
                // table entirely, `Verdict` drops one definition, and
                // `ListEntry` enters the visible top rows. Three of the 278
                // were only ever multi-crate because a fixture repo defined
                // them. Any bar stamped before this commit is not comparable to
                // one stamped after it.
                "external",
                // Build output and agent worktree shadows. The worktree clause
                // is load-bearing: `.claude/worktrees/agent-*/` carries full
                // copies of first-party crates and will otherwise be counted
                // as additional definitions of every name in them.
                "target",
                ".claude",
                // Not production.
                "tests",
                "benches",
                "examples",
                "build.rs",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

impl SourceScope {
    pub fn admits(&self, path: &str) -> bool {
        if path
            .split('/')
            .any(|seg| self.exclude_segments.iter().any(|e| e == seg))
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
    /// Distinct crates defining this name. Length > 1 is the collision
    /// criterion.
    pub crates: Vec<String>,
    /// The subset of [`Self::crates`] whose definition of this name is ALREADY
    /// referenced from a different crate's production code. Length > 1 is the
    /// RATCHET criterion — see [`cross_crate_reached`].
    pub reached_crates: Vec<String>,
    pub defs: Vec<TypeDef>,
    /// Names that end or start with this one. Over-collects; see module docs.
    pub kin: Vec<String>,
}

impl CensusRow {
    /// Can adoption ever retire this row?
    ///
    /// Only when at least two crates each hold a definition some other crate
    /// already reaches: converging a name means callers switch to one of the
    /// definitions, and a definition nothing outside its crate can name is not
    /// a thing anyone can switch to.
    pub fn is_reachable(&self) -> bool {
        self.reached_crates.len() > 1
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Census {
    pub scope: SourceScope,
    /// Every first-party production top-level type definition considered.
    pub total_type_defs: usize,
    /// Names defined as a type in more than one crate. This was the ratchet
    /// number until 2026-08-21; it is now the population the ratchet is drawn
    /// from, kept because a narrowing that hides what it removed cannot be
    /// checked (§18.6).
    pub colliding_names: usize,
    /// …of which at least two crates' definitions are already reached across a
    /// crate boundary. **THE RATCHET NUMBER.**
    pub reachable_names: usize,
    /// Every colliding name, reachable rows first. Nothing is dropped: a row
    /// the narrowing sets aside is still here, carrying an empty or
    /// single-entry `reached_crates` that says why (§18.3).
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

/// Definitions that some OTHER crate already references — the narrowing
/// predicate, and the reason the census stopped over-reporting on 2026-08-21.
///
/// ## What was wrong
///
/// The census counted a name defined as a type in more than one crate and
/// asked nothing about whether either definition was reachable. Measured at
/// `b325f22c`: **239 of 275 rows (87%) named a collision no other crate could
/// reach.** `sovereign-core::Phase` — the census's own "owner" for
/// `sovereign-cli-dev`'s FSM `Phase` — is an enum declared INSIDE A FUNCTION
/// BODY at `title.rs:482`; `CatalogEntry` is `pub(crate)`. There is nothing to
/// import, so no amount of adoption can retire either row; only a rename or a
/// deletion moves them. The cost was paid once already: an order target was
/// derived from the wide number (nc-21, 40 -> 20) and was unreachable when it
/// was written, because 25 of the 40 had no exported owner at all.
///
/// ## The predicate, and why it is graph-only
///
/// A definition is REACHED when an in-scope reference names it from a file
/// whose caller package differs from the definition's package. That is a
/// strictly stronger claim than "could be imported": not *`pub` at top level*
/// but *another crate is already using it*. It needs no source read, no second
/// index pass, and no visibility column the graph does not carry.
///
/// ## Both instruments, because one is not a measurement (§18.4)
///
/// Cross-checked at index head `4f64bdb2` against a text instrument that reads
/// the declaration line out of `git show <indexed-head>:<file>` (the working
/// tree cannot be used — its line numbers have slid away from the index) and
/// asks whether the name is declared `pub` at module top level:
///
/// | instrument | definitions of 662 | surviving rows of 255 |
/// |---|---:|---:|
/// | graph — reached by another crate's production code | 174 | **33** |
/// | text — `pub` at module top level | 457 | 168 |
///
/// The graph's survivors are a strict SUBSET of the text's at BOTH levels
/// (`B \ A = 0` for rows and for definitions; 9 of 662 definitions the text
/// arm could not read). That is the expected direction and the check that says
/// the number can be used: `pub` at top level is necessary for cross-crate
/// reach and not sufficient, so a graph survivor the text arm calls private
/// would mean one of them is broken.
///
/// The two disagreed on exactly one definition before the test-caller clause
/// below existed, and the disagreement was the TEXT arm's: `corpus-engine`'s
/// `QuestionType` is declared `pub` inside a macro invocation body, so it is
/// indented and the heuristic's "column 0 means top level" rule scored it
/// local. It is a good illustration of why the shipped predicate is the graph
/// one.
pub fn cross_crate_reached(
    defs: &[TypeDef],
    refs: &[ScipRefRecord],
    scope: &SourceScope,
) -> BTreeSet<String> {
    let home: BTreeMap<&str, &str> = defs
        .iter()
        .map(|d| (d.qualified.as_str(), d.krate.as_str()))
        .collect();
    let mut out = BTreeSet::new();
    for r in refs {
        if !scope.admits(&r.file_path) {
            continue;
        }
        let Some(owner) = home.get(r.callee_qualified.as_str()) else {
            continue;
        };
        let Some((caller, desc)) = pkg_and_desc(&r.caller_qualified) else {
            continue;
        };
        // A colocated `mod tests` sits under a `/tests/` descriptor segment
        // even when the FILE is production, and `type_defs` already refuses to
        // COUNT such a definition. Reach must use the same rule or a
        // `#[cfg(test)]` import in another crate could move the production
        // ratchet — the exact asymmetry
        // `a_production_twin_raises_the_ratchet_and_a_test_only_twin_does_not`
        // pins on the definition side. One decider for "what is production"
        // (§10.6). Measured at `b0697afb`: 6 of 183 reached definitions were
        // reached only from a test caller, moving exactly one row
        // (`QuestionType`, 34 -> 33).
        if desc.contains("/tests/") {
            continue;
        }
        if caller != *owner {
            out.insert(r.callee_qualified.clone());
        }
    }
    out
}

fn kin_of(name: &str, all: &BTreeSet<&str>) -> Vec<String> {
    all.iter()
        .filter(|m| m.len() > name.len() && (m.ends_with(name) || m.starts_with(name)))
        .map(|m| m.to_string())
        .collect()
}

// ── Verb 1: census ────────────────────────────────────────────────────────────

/// Names defined as a type in more than one crate, each annotated with the
/// crates another crate already reaches, ranked reachable-first.
///
/// `reached` is [`cross_crate_reached`]'s output. Passing an EMPTY set is
/// legitimate — it means "no ref table was read" — and yields a census whose
/// [`Census::reachable_names`] is zero. Callers that have refs must pass them;
/// this signature is why the CLI now reads the ref table for `census` and
/// `status` (measured cost on this workspace: 0.8s -> 5.3s, no second index
/// pass).
pub fn census(
    defs: &[TypeDef],
    reached: &BTreeSet<String>,
    scope: &SourceScope,
    with_kin: bool,
) -> Census {
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
            let reached_crates: BTreeSet<&str> = ds
                .iter()
                .filter(|d| reached.contains(d.qualified.as_str()))
                .map(|d| d.krate.as_str())
                .collect();
            Some(CensusRow {
                name: name.to_string(),
                crates: crates.into_iter().map(String::from).collect(),
                reached_crates: reached_crates.into_iter().map(String::from).collect(),
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
        b.is_reachable()
            .cmp(&a.is_reachable())
            .then(b.crates.len().cmp(&a.crates.len()))
            .then(b.kin.len().cmp(&a.kin.len()))
            .then(b.defs.len().cmp(&a.defs.len()))
            .then(a.name.cmp(&b.name))
    });

    Census {
        scope: scope.clone(),
        total_type_defs: defs.len(),
        colliding_names: rows.len(),
        reachable_names: rows.iter().filter(|r| r.is_reachable()).count(),
        rows,
    }
}

/// The ratchet number: how many colliding names adoption could actually
/// retire.
///
/// A relay onto [`census`], not a second count. The two used to be independent
/// loops over the same defs; once the narrowing landed that would have been
/// two implementations of one threshold (§10.6), and the gate and the feed
/// could have disagreed silently.
pub fn duplicate_count(defs: &[TypeDef], reached: &BTreeSet<String>, scope: &SourceScope) -> usize {
    census(defs, reached, scope, false).reachable_names
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
    out.push_str(" minus segments ");
    out.push_str(&scope.exclude_segments.join(" "));
    out.push('\n');
}

/// The census feed. `local_only` swaps the table for the rows the narrowing
/// set aside — the same rows, never a different report.
pub fn render_census(c: &Census, limit: usize, with_kin: bool, local_only: bool) -> String {
    let mut s = String::new();
    render_scope(&c.scope, &mut s);
    let set_aside = c.colliding_names - c.reachable_names;
    let one_owner = c
        .rows
        .iter()
        .filter(|r| r.reached_crates.len() == 1)
        .count();
    s.push_str(&format!(
        "\nfirst-party production type definitions : {}\n",
        c.total_type_defs
    ));
    s.push_str(&format!(
        "names defined as a type in >1 crate     : {}\n",
        c.colliding_names
    ));
    s.push_str(&format!(
        "  reachable — >=2 crates' defs are already used by another crate's\n\
         \x20              production code\n\
         \x20                                       : {:<5} <- the ratchet number\n",
        c.reachable_names
    ));
    s.push_str(&format!(
        "  set aside — fewer than two crates' defs are reached\n\
         \x20                                       : {set_aside:<5} ({})\n",
        if local_only {
            "listed below"
        } else {
            "--local to list"
        }
    ));
    // What the narrowing MISSES, printed next to what it removes. A row with
    // exactly one reached definition is still convergible — the local copies
    // fold into the one type other crates already import — and the >=2 rule
    // does not count it. Saying so here rather than only in the direction the
    // narrowing was meant to fix is §18.6.
    s.push_str(&format!(
        "    of those, one crate's def IS reached: {one_owner:<5} still convergible \
         (fold the local copies\n\
         \x20                                         into it) — not counted by the ratchet\n"
    ));
    let shown: Vec<&CensusRow> = c
        .rows
        .iter()
        .filter(|r| r.is_reachable() != local_only)
        .collect();
    if with_kin {
        let kin: usize = shown.iter().map(|r| r.kin.len()).sum();
        s.push_str(&format!(
            "morphological kin of the rows below      : {kin}   (over-collects by design — see `converge noun`)\n"
        ));
    }
    s.push('\n');
    if with_kin {
        s.push_str("crates  reach  defs   kin  name\n------  -----  ----  ----  ----\n");
    } else {
        s.push_str("crates  reach  defs  name\n------  -----  ----  ----\n");
    }
    for r in shown.iter().take(limit) {
        if with_kin {
            s.push_str(&format!(
                "{:>6}  {:>5}  {:>4}  {:>4}  {}\n",
                r.crates.len(),
                r.reached_crates.len(),
                r.defs.len(),
                r.kin.len(),
                r.name
            ));
        } else {
            s.push_str(&format!(
                "{:>6}  {:>5}  {:>4}  {}\n",
                r.crates.len(),
                r.reached_crates.len(),
                r.defs.len(),
                r.name
            ));
        }
    }
    if shown.len() > limit {
        s.push_str(&format!(
            "\n... {} more (--limit 0 for all)\n",
            shown.len() - limit
        ));
    }
    if local_only {
        s.push_str(
            "\nThese rows are NOT counted by the ratchet and adoption cannot retire them:\n\
             every definition is module-private, `pub(crate)`, declared inside a function\n\
             body, or simply not imported anywhere yet. There is nothing to switch to.\n\
             Only a rename or a deletion moves one — and `converge shape` is the verb\n\
             that finds the renamed forks a name census cannot see.\n",
        );
    } else {
        s.push_str("\nSame name is not same concept — this DISCOVERS, a human DISPOSITIONS.\n");
        s.push_str("Next: `svrn code converge noun <Name>` for one row's dossier.\n");
    }
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
            start_col: -1,
            end_line: -1,
            end_col: -1,
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

    /// The campaign's motivating file must be visible to the campaign's own
    /// instrument. `sovereign-core/src/deep_research/icd.rs` is the specimen
    /// the noun-convergence program cites — five register nouns re-derived
    /// privately there — and until 2026-08-20 the census could not see it,
    /// because the exclusion list matched `"research/"` as a SUBSTRING and
    /// `deep_research/` ends in it. `converge noun <X>` inherits this scope, so
    /// the blind spot made the pre-flight oracle answer "no such concept" in
    /// the one direction that is unsafe (§18.3 — absence is never defaulted).
    #[test]
    fn deep_research_is_not_the_research_spike_tree() {
        let s = SourceScope::default();
        assert!(s.admits("sovereign/crates/sovereign-core/src/deep_research/icd.rs"));
        // …while the top-level spike tree the pattern was actually written for
        // stays excluded. It is repo-relative with no leading slash, which is
        // why the fix cannot simply be to anchor the pattern as `/research/`.
        assert!(!s.admits("research/spike/src/lib.rs"));
        assert!(!s.admits("research/enrichment-spikes/x/y.rs"));
    }

    /// The whole list, both arms. `deep_research` was the entry that bit, but
    /// six of the ten patterns had the identical shape — a bare token that a
    /// LONGER segment can end with. Fixed as semantics (segment equality), not
    /// as a patch to one string.
    #[test]
    fn every_exclusion_matches_a_whole_segment_not_a_substring() {
        let s = SourceScope::default();
        // Longer segments that merely END with an excluded token are source.
        for admitted in [
            "sovereign/crates/sovereign-core/src/deep_research/icd.rs",
            "sovereign/crates/a/src/xvendor/x.rs",
            "sovereign/crates/a/src/my_target/x.rs",
            "sovereign/crates/a/src/prebuild.rs",
            // `external` is a segment, so a module or file merely NAMED for it
            // is still source.
            "sovereign/crates/a/src/external_api.rs",
            "sovereign/crates/a/src/api_external/mod.rs",
        ] {
            assert!(s.admits(admitted), "must be counted: {admitted}");
        }
        // …and the trees the patterns were written for still go.
        for excluded in [
            "research/spike/src/lib.rs",
            "vendor/foo/src/lib.rs",
            "node_modules/x/y.rs",
            ".cargo-container/x.rs",
            "target/debug/build/x.rs",
            ".claude/worktrees/agent-a/sovereign/crates/x/src/lib.rs",
            "sovereign/crates/a/tests/e2e.rs",
            "sovereign/crates/a/benches/b.rs",
            "sovereign/crates/a/examples/e.rs",
            "sovereign/crates/a/build.rs",
            // Third-party fixture repos vendored under the bench tree — 68% of
            // everything `dry_report` reported before this entry existed.
            "sovereign/bench/external/swebench/repos/django/django/db/models/sql/query.py",
            "sovereign/bench/external/rewardbench2/run.py",
        ] {
            assert!(!s.admits(excluded), "must be excluded: {excluded}");
        }
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

    /// One in-scope reference from `user` to `pkg`'s definition of `desc`.
    fn used_by(user: &str, pkg: &str, desc: &str) -> ScipRefRecord {
        rf(
            user,
            "f().",
            pkg,
            desc,
            &format!("sovereign/crates/{user}/src/u.rs"),
        )
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
        let scope = SourceScope::default();
        let defs = type_defs(&syms, &scope);
        let refs = vec![
            used_by("cli", "crate_a", "m/Verdict#"),
            used_by("cli", "crate_b", "n/Verdict#"),
        ];
        let reached = cross_crate_reached(&defs, &refs, &scope);
        let c = census(&defs, &reached, &scope, false);
        assert_eq!(c.total_type_defs, 5);
        assert_eq!(c.rows.len(), 1);
        assert_eq!(c.rows[0].name, "Verdict");
        assert_eq!(c.colliding_names, 1);
        assert_eq!(c.reachable_names, 1);
        assert_eq!(duplicate_count(&defs, &reached, &scope), 1);
    }

    /// The narrowing, with the failing input named (§18.1).
    ///
    /// Both halves of the specimen the census got wrong: `Phase` is declared
    /// inside a function body in one crate and `pub(crate)` in the other, so
    /// nothing outside either crate can name either definition — it is a
    /// collision between two local helpers, and no amount of adoption retires
    /// it. `Verdict` is the same COLLISION shape and a genuine candidate,
    /// because both definitions are already imported elsewhere.
    #[test]
    fn a_collision_no_other_crate_reaches_is_not_counted() {
        let scope = SourceScope::default();
        let syms = vec![
            sym(
                "core",
                "title/Phase#",
                "sovereign/crates/core/src/t.rs",
                482,
            ),
            sym("dev", "fsm/Phase#", "sovereign/crates/dev/src/f.rs", 40),
            sym("core", "m/Verdict#", "sovereign/crates/core/src/m.rs", 10),
            sym("mesh", "n/Verdict#", "sovereign/crates/mesh/src/n.rs", 20),
        ];
        let defs = type_defs(&syms, &scope);
        let refs = vec![
            // `Phase` is used only inside its own crate, on both sides.
            rf(
                "core",
                "g().",
                "core",
                "title/Phase#",
                "sovereign/crates/core/src/t.rs",
            ),
            rf(
                "dev",
                "g().",
                "dev",
                "fsm/Phase#",
                "sovereign/crates/dev/src/f.rs",
            ),
            // `Verdict`'s two definitions are each imported by a third crate.
            used_by("cli", "core", "m/Verdict#"),
            used_by("eval", "mesh", "n/Verdict#"),
        ];
        let reached = cross_crate_reached(&defs, &refs, &scope);
        let c = census(&defs, &reached, &scope, false);

        assert_eq!(c.colliding_names, 2, "both names still collide");
        assert_eq!(c.reachable_names, 1, "only one can be retired by adoption");
        assert_eq!(duplicate_count(&defs, &reached, &scope), 1);

        // Reachable first, and the set-aside row is REPORTED, not dropped.
        assert_eq!(c.rows[0].name, "Verdict");
        assert!(c.rows[0].is_reachable());
        assert_eq!(c.rows[1].name, "Phase");
        assert!(!c.rows[1].is_reachable());
        assert!(c.rows[1].reached_crates.is_empty());
        assert!(render_census(&c, 40, false, true).contains("Phase"));
        assert!(!render_census(&c, 40, false, false).contains("Phase"));
    }

    /// One reached definition is not enough: converging needs two crates that
    /// can each be switched TO or AWAY FROM. The arm that fails if the
    /// predicate is ever relaxed to `any`.
    #[test]
    fn one_reached_definition_and_one_local_one_is_still_not_countable() {
        let scope = SourceScope::default();
        let syms = vec![
            sym("core", "m/Config#", "sovereign/crates/core/src/m.rs", 10),
            sym("mesh", "n/Config#", "sovereign/crates/mesh/src/n.rs", 20),
        ];
        let defs = type_defs(&syms, &scope);
        let refs = vec![used_by("cli", "core", "m/Config#")];
        let reached = cross_crate_reached(&defs, &refs, &scope);
        let c = census(&defs, &reached, &scope, false);
        assert_eq!(c.colliding_names, 1);
        assert_eq!(c.reachable_names, 0);
        assert_eq!(c.rows[0].reached_crates, vec!["core"]);
    }

    /// The ratchet's two arms, side by side — the pair that decides whether
    /// `converge status` goes red. The gate is armed against ADDITIONS, so
    /// what must be shown is the DELTA, not the absolute count: a production
    /// twin in a second crate moves it by exactly one, and a `#[cfg(test)]`
    /// twin of the same name moves it by zero. Watched red 2026-08-19 live
    /// (272 -> 273); pinned here so it stays that way (§18.1: a check with no
    /// failing input you can name is not a check).
    ///
    /// Since the 2026-08-21 narrowing the twin must also be REACHED to move
    /// the number, so every arm carries the reference that makes it so.
    #[test]
    fn a_production_twin_raises_the_ratchet_and_a_test_only_twin_does_not() {
        let scope = SourceScope::default();
        let syms = vec![
            sym("crate_a", "m/Register#", "sovereign/crates/a/src/m.rs", 10),
            sym("crate_a", "m/Solo#", "sovereign/crates/a/src/m.rs", 20),
        ];
        let base_refs = vec![used_by("cli", "crate_a", "m/Register#")];
        let count = |syms: &[ScipSymbolRecord], refs: &[ScipRefRecord]| {
            let defs = type_defs(syms, &scope);
            let reached = cross_crate_reached(&defs, refs, &scope);
            duplicate_count(&defs, &reached, &scope)
        };
        let before = count(&syms, &base_refs);
        assert_eq!(before, 0, "one crate, no twins");

        // RED: the same noun minted a second time, in another crate, and used.
        let mut added = syms.clone();
        added.push(sym(
            "crate_b",
            "n/Register#",
            "sovereign/crates/b/src/n.rs",
            30,
        ));
        let mut added_refs = base_refs.clone();
        added_refs.push(used_by("cli", "crate_b", "n/Register#"));
        assert_eq!(
            count(&added, &added_refs),
            before + 1,
            "a cross-crate twin that another crate already reaches is exactly \
             the +1 the ratchet fires on"
        );

        // GREEN: same name, same second crate, but under `#[cfg(test)] mod
        // tests` — a test helper is not a concept the register owns, and a
        // ratchet that fired on one would be disabled inside a week.
        let mut test_only = syms.clone();
        test_only.push(sym(
            "crate_b",
            "n/tests/Register#",
            "sovereign/crates/b/src/n.rs",
            30,
        ));
        assert_eq!(
            count(&test_only, &added_refs),
            before,
            "a #[cfg(test)] twin must not move the ratchet"
        );

        // …and neither does a twin under `tests/`, `benches/` or `examples/`,
        // which the scope drops by path rather than by descriptor.
        let mut out_of_tree = syms;
        out_of_tree.push(sym("crate_b", "n/Register#", "b/tests/e2e.rs", 30));
        assert_eq!(count(&out_of_tree, &added_refs), before);
    }

    /// The reference has to CROSS a crate boundary, and it has to come from an
    /// in-scope file. A type used only by its own crate's tests is not reached.
    #[test]
    fn only_an_in_scope_cross_crate_reference_counts_as_reach() {
        let scope = SourceScope::default();
        let syms = vec![sym(
            "core",
            "m/Verdict#",
            "sovereign/crates/core/src/m.rs",
            10,
        )];
        let defs = type_defs(&syms, &scope);

        let same_crate = vec![rf(
            "core",
            "g().",
            "core",
            "m/Verdict#",
            "sovereign/crates/core/src/other.rs",
        )];
        assert!(cross_crate_reached(&defs, &same_crate, &scope).is_empty());

        let from_a_test = vec![rf(
            "cli",
            "t().",
            "core",
            "m/Verdict#",
            "sovereign/crates/cli/tests/e2e.rs",
        )];
        assert!(cross_crate_reached(&defs, &from_a_test, &scope).is_empty());

        // …and a COLOCATED `mod tests` in another crate, whose file is
        // production and whose descriptor is not. Six live definitions were
        // reached only this way at `b0697afb`; counting them would let a
        // `#[cfg(test)]` import move the production ratchet.
        let from_a_colocated_test = vec![rf(
            "cli",
            "m/tests/t().",
            "core",
            "m/Verdict#",
            "sovereign/crates/cli/src/m.rs",
        )];
        assert!(cross_crate_reached(&defs, &from_a_colocated_test, &scope).is_empty());

        let real = vec![used_by("cli", "core", "m/Verdict#")];
        assert_eq!(cross_crate_reached(&defs, &real, &scope).len(), 1);
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
        let scope = SourceScope::default();
        let defs = type_defs(&syms, &scope);
        let c = census(&defs, &BTreeSet::new(), &scope, true);
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
