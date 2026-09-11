// SPDX-License-Identifier: AGPL-3.0-or-later
//! The THIN-SURFACE rule — `[thin_surfaces]`.
//!
//! Split out of `lib.rs` for the same reason `packages.rs` was: the division
//! is by QUESTION. Layers answer "may this edge exist anywhere"; packages
//! answer "is this edge inside MY closure"; this answers **"can this surface
//! BECOME the thing it talks to"**. An edge can be layer-legal, inside every
//! package budget, and still hand a UI the whole backend.
//!
//! # Why it is reachability and not an edge
//!
//! Every other rule in this file's schema tests one declared edge. This one
//! tests the CLOSURE, because the ability it governs is transitive: Cargo
//! links what you reach, not what you name. `sv-surface`'s own predicate
//! (`quality/campaigns/sv-surface.toml`) chose direct edges and said so —
//! "the direct form is the honest cheap check" — and measured against the
//! tree on 2026-09-11 that choice has a live hole: `sovereign-desktop` names
//! `sovereign-runtime-recipe`, which names `sovereign-tools`, which binds an
//! MCP listener. Delete the five direct edges the predicate lists and the
//! desktop binary still carries `sovereign-tools`. A gate that went green
//! there would be the exact failure the bar is named for.
//!
//! So the violation is `(surface, reached)` with the SHORTEST PATH attached,
//! not `(from, to)`. One row per forbidden crate a surface can reach, however
//! it reaches it — which is also why an `[[exception]]` keyed on that pair
//! retires the moment the LAST path disappears, not the first.
//!
//! # The two halves
//!
//! This module is half one of `sv-no-daemon-management`'s instrument. Half two
//! is `xtask lifecycle-gate`, which reads [`ThinSurfaces::lifecycle_allow`]
//! from the same block. They share the `crates` list on purpose: two gates
//! that disagreed about what a surface IS would each be green about a
//! different set (ARCH principle 8).

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::{DepEdge, DepKind, LayerMap, Violation};

/// The declared thin surfaces and everything both halves of the bar's
/// instrument need to know about them.
#[derive(Debug, Deserialize, Default)]
pub struct ThinSurfaces {
    /// The surface crates — clients, not hosts. Exact names, never patterns:
    /// a surface is a named product decision, and a `sovereign-*` glob would
    /// silently enrol the next crate someone names that way.
    #[serde(default)]
    pub crates: Vec<String>,
    /// Crates a surface may not REACH, transitively. Exact names.
    #[serde(default)]
    pub may_not_reach: Vec<String>,
    /// The ONE file allowed to start a process on a surface's behalf, and the
    /// crate that owns it. Read by `xtask lifecycle-gate`, declared here so
    /// the dependency rule and the census cannot disagree about who the
    /// decider is.
    pub bring_up_decider: Option<BringUpDecider>,
    /// The lifecycle census's ledger — see [`LifecycleAllow`].
    #[serde(default)]
    pub lifecycle_allow: Vec<LifecycleAllow>,
    /// The section a reader opens for the why.
    pub doc: String,
}

/// The one sanctioned bring-up site.
#[derive(Debug, Deserialize)]
pub struct BringUpDecider {
    /// Owning crate — must be a workspace member.
    pub krate: String,
    /// Repo-relative path of the file that may start a process.
    pub file: String,
    pub reason: String,
}

/// One tolerated process-control site class, keyed on `(file, rule)` with a
/// CAP.
///
/// Deliberately the `[[exception]]` contract rather than a free-form waiver,
/// with the same three-way reading every ratchet in this repo uses:
///
/// * **over the cap** — FAIL. This is the erosion the bar exists to refuse:
///   nobody re-supervises in one commit, they add a retry, then a backoff,
///   then a health check.
/// * **under the cap, non-zero** — report a TIGHTEN line, do not fail. The
///   slack is visible and banking it is always safe, but a gate that reddens
///   the commit which DELETED code teaches people to reach for `--no-verify`
///   mid-migration.
/// * **zero** — FAIL as stale, exactly like an `[[exception]]` that matched
///   no edge. The rung that won must delete the row; removals are the
///   celebration.
#[derive(Debug, Deserialize)]
pub struct LifecycleAllow {
    /// Repo-relative file path.
    pub file: String,
    /// The rule id this row tolerates (`starts`, `detaches`, `retains`,
    /// `reaps`).
    pub rule: String,
    /// Sites tolerated for this `(file, rule)`. A ceiling — see the three-way
    /// reading above.
    pub max: usize,
    /// `sanctioned` — policy, may stand indefinitely.
    /// `burning-down` — debt, counted by the bar, target zero.
    pub kind: String,
    pub reason: String,
}

/// `burning-down` — the ledger kind the `sv-no-daemon-management` bar counts.
pub const BURNING_DOWN: &str = "burning-down";
/// `sanctioned` — the ledger kind that may stand.
pub const SANCTIONED: &str = "sanctioned";

/// Validate the thin-surface half of a parsed map.
///
/// Same vacuity argument as `backstage` (v2) and `[[package]]` (v3), one rung
/// out: `thin_surfaces` is `#[serde(default)]`, so a v4 map that lost the
/// block — a bad merge, a truncated file — would deserialize to an empty
/// surface list, every reachability rule would become VACUOUSLY TRUE, and
/// layer-gate would print a clean bill of health on the rule it was bumped
/// for.
pub(crate) fn validate(map: &LayerMap) -> Result<(), String> {
    if map.schema_version >= 4 && map.thin_surfaces.crates.is_empty() {
        return Err("ARCH_LAYERS.toml declares schema_version >= 4 but no \
             [thin_surfaces] crates. v4 exists to carry the thin-surface \
             reachability rule (sv-surface's `sv-no-daemon-management`), and \
             an empty list would make it vacuous — every surface would pass \
             and the gate would report success on a rule it never evaluated. \
             Declare the surfaces, or drop back to schema_version = 3 if the \
             rule is genuinely not wanted here."
            .to_string());
    }
    if map.schema_version >= 4 && map.thin_surfaces.may_not_reach.is_empty() {
        return Err("ARCH_LAYERS.toml declares [thin_surfaces] with no \
             `may_not_reach` crates — the surfaces are named and nothing is \
             forbidden to them, which is the same vacuous green one rung \
             further in."
            .to_string());
    }
    for a in &map.thin_surfaces.lifecycle_allow {
        if a.kind != BURNING_DOWN && a.kind != SANCTIONED {
            return Err(format!(
                "[[thin_surfaces.lifecycle_allow]] {} ({}) declares kind \
                 `{}` — it must be `{SANCTIONED}` (policy, may stand) or \
                 `{BURNING_DOWN}` (debt, counted by the bar). The two are not \
                 interchangeable: the bar's target is zero {BURNING_DOWN} \
                 rows, and a waiver filed under the wrong word stops being \
                 counted.",
                a.file, a.rule, a.kind
            ));
        }
    }
    Ok(())
}

/// Every crate reachable from `start` over enforceable (non-dev) edges, with
/// the shortest path to each.
///
/// `optional` is deliberately NOT consulted, unlike the `backstage` rule. That
/// rule asks "does the PRODUCT ship without it", and a feature answers it.
/// This one asks "can this build become a daemon", and a build that becomes
/// one behind `--features` or behind `[target.'cfg(windows)'.dependencies]`
/// still became one — the bar itself contemplates exactly that Windows
/// carve-out and requires it to be a DECLARED exception rather than an
/// invisible pass.
fn reachable(start: &str, adj: &BTreeMap<&str, BTreeSet<&str>>) -> BTreeMap<String, Vec<String>> {
    let mut paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    queue.push_back(start);
    while let Some(cur) = queue.pop_front() {
        let here: Vec<String> = if cur == start {
            vec![start.to_string()]
        } else {
            paths[cur].clone()
        };
        let Some(next) = adj.get(cur) else { continue };
        for n in next {
            if *n == start || paths.contains_key(*n) {
                continue;
            }
            let mut p = here.clone();
            p.push((*n).to_string());
            paths.insert((*n).to_string(), p);
            queue.push_back(n);
        }
    }
    paths
}

/// The thin-surface pass. `used` is the layer pass's exception ledger — shared
/// so an `[[exception]]` written for a surface is not ALSO reported stale by
/// the edge pass (the two-gates-one-row failure `evaluate_packages` records).
pub(crate) fn evaluate(
    map: &LayerMap,
    edges: &[DepEdge],
    used: &mut BTreeSet<(String, String)>,
) -> Vec<Violation> {
    let surfaces = &map.thin_surfaces;
    if surfaces.crates.is_empty() || surfaces.may_not_reach.is_empty() {
        return Vec::new();
    }

    let mut adj: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for e in edges {
        if e.kind == DepKind::Dev {
            continue;
        }
        adj.entry(&e.from).or_default().insert(&e.to);
    }

    let mut violations = Vec::new();
    for surface in &surfaces.crates {
        let paths = reachable(surface, &adj);
        for forbidden in &surfaces.may_not_reach {
            let Some(path) = paths.get(forbidden) else {
                continue;
            };
            let excepted = map.exceptions.iter().any(|e| {
                let hit = e.package.is_none() && &e.from == surface && &e.to == forbidden;
                if hit {
                    used.insert((e.from.clone(), e.to.clone()));
                }
                hit
            });
            if excepted {
                continue;
            }
            violations.push(Violation::SurfaceReach {
                surface: surface.clone(),
                reached: forbidden.clone(),
                path: path.clone(),
                doc: surfaces.doc.clone(),
            });
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{evaluate as evaluate_map, parse};

    fn edge(from: &str, to: &str) -> DepEdge {
        DepEdge {
            from: from.into(),
            to: to.into(),
            kind: DepKind::Normal,
            optional: false,
        }
    }

    /// One layer for everything, so nothing here can fail on DIRECTION — any
    /// violation these tests see is the thin-surface rule and nothing else.
    const MAP: &str = r#"
schema_version = 4
backstage = ["xtask"]

[[layer]]
name = "everything"
crates = ["*"]

[[package]]
name = "p"
crates = ["some-package-crate"]
doc = "docs/p.md"

[thin_surfaces]
crates = ["sovereign-desktop", "sovereign-mobile"]
may_not_reach = ["sovereign-cli-daemon", "sovereign-tools"]
doc = "quality/campaigns/sv-surface.toml"
bring_up_decider = { krate = "sovereign-turn-client", file = "x/reach.rs", reason = "one decider" }
"#;

    fn names() -> BTreeSet<String> {
        [
            "sovereign-desktop",
            "sovereign-mobile",
            "sovereign-turn-client",
            "sovereign-runtime-recipe",
            "sovereign-cli-daemon",
            "sovereign-tools",
            "some-package-crate",
            "xtask",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    /// The synthetic fixture where the daemon-assembling crates are ABSENT:
    /// the green half of the instrument. A gate seen only red is as unproven
    /// as one seen only green (ARCH principle 7).
    #[test]
    fn a_surface_that_reaches_only_the_client_family_passes() {
        let map = parse(MAP).unwrap();
        let edges = vec![
            edge("sovereign-desktop", "sovereign-turn-client"),
            edge("sovereign-mobile", "sovereign-turn-client"),
        ];
        let v = evaluate_map(&map, &names(), &edges);
        assert!(v.is_empty(), "the thin shape must pass clean: {v:?}");
    }

    /// The plant that proves reachability earns its keep: NO forbidden crate
    /// is named by the surface, and the surface still links one.
    ///
    /// This is the live hole measured on 2026-09-11 — `sovereign-desktop` →
    /// `sovereign-runtime-recipe` → `sovereign-tools` — reduced to its
    /// skeleton. A direct-edge rule is green on this input.
    #[test]
    fn an_indirect_path_is_caught_and_names_the_hop_a_direct_rule_would_miss() {
        let map = parse(MAP).unwrap();
        let edges = vec![
            edge("sovereign-desktop", "sovereign-runtime-recipe"),
            edge("sovereign-runtime-recipe", "sovereign-tools"),
        ];
        let v = evaluate_map(&map, &names(), &edges);
        let hit = v
            .iter()
            .find(|x| matches!(x, Violation::SurfaceReach { reached, .. } if reached == "sovereign-tools"))
            .unwrap_or_else(|| panic!("an indirect reach must fail: {v:?}"));
        let msg = hit.describe();
        assert!(
            msg.contains("sovereign-runtime-recipe"),
            "the message must name the hop that carries it, or the reader \
             deletes the wrong edge: {msg}"
        );
        // Assert on something the SUBJECT cannot author (ARCH principle 5):
        // the intermediate hop is nowhere in the surface's own manifest.
        assert!(
            !edges
                .iter()
                .any(|e| e.from == "sovereign-desktop" && e.to == "sovereign-tools"),
            "the fixture must have no direct edge, or this proves nothing"
        );
    }

    #[test]
    fn a_direct_reach_is_caught_too_and_the_path_is_the_pair() {
        let map = parse(MAP).unwrap();
        let edges = vec![edge("sovereign-desktop", "sovereign-cli-daemon")];
        let v = evaluate_map(&map, &names(), &edges);
        assert!(
            v.iter().any(|x| matches!(
                x,
                Violation::SurfaceReach { surface, reached, path, .. }
                    if surface == "sovereign-desktop"
                        && reached == "sovereign-cli-daemon"
                        && path.len() == 2
            )),
            "{v:?}"
        );
    }

    /// Dev edges are never enforced here for the same reason they are never
    /// enforced on direction: a dev-dep cannot reach a shipped artifact.
    #[test]
    fn a_dev_only_path_is_not_a_reach() {
        let map = parse(MAP).unwrap();
        let edges = vec![DepEdge {
            kind: DepKind::Dev,
            ..edge("sovereign-desktop", "sovereign-cli-daemon")
        }];
        let v = evaluate_map(&map, &names(), &edges);
        assert!(
            !v.iter()
                .any(|x| matches!(x, Violation::SurfaceReach { .. })),
            "{v:?}"
        );
    }

    /// The burn-down contract: a declared exception tolerates the reach, and
    /// the entry must FAIL as stale once the last path is gone — so the rung
    /// that wins is forced to bank the win.
    #[test]
    fn a_reach_can_be_grandfathered_and_the_entry_goes_stale_when_the_last_path_dies() {
        let with_exc = format!(
            "{MAP}\n[[exception]]\nfrom = \"sovereign-desktop\"\nto = \
             \"sovereign-cli-daemon\"\nreason = \"svt-2 deletes it\"\n"
        );
        let map = parse(&with_exc).unwrap();
        let live = vec![edge("sovereign-desktop", "sovereign-cli-daemon")];
        let v = evaluate_map(&map, &names(), &live);
        assert!(
            v.is_empty(),
            "an excepted reach is tolerated, and must not ALSO report stale: {v:?}"
        );
        let v = evaluate_map(&map, &names(), &[]);
        assert!(
            v.iter()
                .any(|x| matches!(x, Violation::StaleException { to, .. } if to == "sovereign-cli-daemon")),
            "a won reach must retire its own exception: {v:?}"
        );
    }

    #[test]
    fn a_v4_map_with_no_surfaces_is_refused_not_silently_vacuous() {
        let vacuous = r#"
schema_version = 4
backstage = ["xtask"]
[[layer]]
name = "everything"
crates = ["*"]
[[package]]
name = "p"
crates = ["some-package-crate"]
doc = "docs/p.md"
"#;
        let err = parse(vacuous).expect_err("a v4 map with no thin_surfaces must not parse");
        assert!(err.contains("thin_surfaces"), "{err}");
        assert!(err.contains("vacuous"), "the error must name WHY: {err}");
    }

    #[test]
    fn a_surface_list_with_nothing_forbidden_is_refused() {
        let vacuous = r#"
schema_version = 4
backstage = ["xtask"]
[[layer]]
name = "everything"
crates = ["*"]
[[package]]
name = "p"
crates = ["some-package-crate"]
doc = "docs/p.md"
[thin_surfaces]
crates = ["sovereign-desktop"]
may_not_reach = []
doc = "d"
"#;
        let err = parse(vacuous).expect_err("nothing forbidden is nothing enforced");
        assert!(err.contains("may_not_reach"), "{err}");
    }

    #[test]
    fn a_ledger_row_under_an_unknown_kind_is_refused() {
        let bad = format!(
            "{MAP}\n[[thin_surfaces.lifecycle_allow]]\nfile = \"a.rs\"\nrule \
             = \"starts\"\nmax = 1\nkind = \"temporary\"\nreason = \"r\"\n"
        );
        let err = parse(&bad).expect_err("an unknown ledger kind must not parse");
        assert!(err.contains("temporary"), "{err}");
        assert!(
            err.contains(BURNING_DOWN),
            "the error must name the set: {err}"
        );
    }

    #[test]
    fn the_shortest_path_is_reported_not_an_arbitrary_one() {
        let map = parse(MAP).unwrap();
        let edges = vec![
            edge("sovereign-desktop", "sovereign-cli-daemon"),
            edge("sovereign-desktop", "sovereign-runtime-recipe"),
            edge("sovereign-runtime-recipe", "sovereign-cli-daemon"),
        ];
        let v = evaluate_map(&map, &names(), &edges);
        let hit = v
            .iter()
            .find(|x| matches!(x, Violation::SurfaceReach { reached, .. } if reached == "sovereign-cli-daemon"))
            .expect("reached");
        let Violation::SurfaceReach { path, .. } = hit else {
            unreachable!()
        };
        assert_eq!(
            path,
            &[
                "sovereign-desktop".to_string(),
                "sovereign-cli-daemon".to_string()
            ],
            "a longer path shown while a direct edge exists sends the reader \
             to delete the wrong thing"
        );
    }
}
