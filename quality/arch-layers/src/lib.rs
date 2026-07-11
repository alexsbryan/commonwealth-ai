// SPDX-License-Identifier: AGPL-3.0-or-later
//! Schema + evaluator for `quality/ARCH_LAYERS.toml` — the declared layer map.
//!
//! The layer map is the workspace's dependency-direction contract: layers are
//! ordered bottom → top, a crate may depend only on crates in the same or a
//! lower layer, `[[forbid]]` expresses cross-family rules that ordering can't,
//! and `[[exception]]` grandfathers today's known violations as a reviewable
//! burn-down list (adding one requires editing the policy file in the PR).
//!
//! Two consumers, one parser:
//! - `xtask layer-gate` feeds Cargo-DECLARED dependency edges (deterministic,
//!   runs in CI without a daemon).
//! - the code-intel `arch_report` feeds SCIP-OBSERVED symbol-reference edges
//!   (catches coupling that re-exports hide from Cargo).
//!
//! Both call [`evaluate`]; the meaning of the policy file lives here and only
//! here.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// Highest `schema_version` this evaluator understands. A map declaring a
/// newer version fails loudly instead of being half-interpreted.
pub const MAX_SCHEMA_VERSION: u32 = 1;

// ── Schema ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LayerMap {
    pub schema_version: u32,
    /// Ordered bottom → top.
    #[serde(default, rename = "layer")]
    pub layers: Vec<Layer>,
    #[serde(default, rename = "forbid")]
    pub forbids: Vec<Forbid>,
    #[serde(default, rename = "exception")]
    pub exceptions: Vec<Exception>,
}

#[derive(Debug, Deserialize)]
pub struct Layer {
    pub name: String,
    /// Crate-name patterns (`*` wildcards allowed). Every workspace member
    /// must match exactly one layer — unmatched and ambiguous are both errors.
    pub crates: Vec<String>,
}

/// A rule layer ordering can't express (e.g. "commonwealth-* must not reach
/// sovereign-* even where the layers would allow it").
#[derive(Debug, Deserialize)]
pub struct Forbid {
    pub from: String,
    pub to: String,
    /// Crate names (or patterns) exempt from this rule's `to` side.
    #[serde(default)]
    pub except: Vec<String>,
    pub reason: String,
}

/// A grandfathered violation — tolerated, counted, expected to disappear.
/// A stale entry (no longer matching any live edge) is itself a failure:
/// the PR that removes the last offending edge must also delete its entry.
#[derive(Debug, Deserialize)]
pub struct Exception {
    pub from: String,
    pub to: String,
    pub reason: String,
    #[serde(default)]
    pub tracking: Option<String>,
}

// ── Input edges ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    Normal,
    Build,
    /// Dev edges are reported by callers but never enforced — a dev-dep
    /// cannot leak into a shipped artifact.
    Dev,
}

#[derive(Debug, Clone)]
pub struct DepEdge {
    pub from: String,
    pub to: String,
    pub kind: DepKind,
}

// ── Output ────────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
pub enum Violation {
    /// A workspace member no layer pattern matches. The map must be total.
    UnassignedCrate { name: String },
    /// A member matched by more than one layer — the map is ambiguous.
    AmbiguousCrate { name: String, layers: Vec<String> },
    /// A dependency pointing at a HIGHER layer.
    UpwardEdge {
        from: String,
        from_layer: String,
        to: String,
        to_layer: String,
        kind: DepKind,
    },
    /// A dependency matching a `[[forbid]]` rule.
    ForbiddenEdge {
        from: String,
        to: String,
        reason: String,
    },
    /// An `[[exception]]` no live edge needed — delete it, it's already won.
    StaleException { from: String, to: String },
}

impl Violation {
    pub fn describe(&self) -> String {
        match self {
            Violation::UnassignedCrate { name } => format!(
                "crate `{name}` is not assigned to any layer — add it to a \
                 [[layer]] in quality/ARCH_LAYERS.toml (the map must cover \
                 every workspace member)"
            ),
            Violation::AmbiguousCrate { name, layers } => format!(
                "crate `{name}` matches more than one layer ({}) — tighten \
                 the patterns in quality/ARCH_LAYERS.toml",
                layers.join(", ")
            ),
            Violation::UpwardEdge {
                from,
                from_layer,
                to,
                to_layer,
                kind,
            } => format!(
                "{from} ({from_layer}) → {to} ({to_layer}): {} dependency \
                 points UP the layer stack — invert it, or grandfather it \
                 with a [[exception]] entry (with a reason) in \
                 quality/ARCH_LAYERS.toml",
                match kind {
                    DepKind::Normal => "a normal",
                    DepKind::Build => "a build",
                    DepKind::Dev => "a dev",
                }
            ),
            Violation::ForbiddenEdge { from, to, reason } => format!(
                "{from} → {to}: forbidden by a [[forbid]] rule ({reason}) — \
                 remove the edge or grandfather it with a [[exception]] entry"
            ),
            Violation::StaleException { from, to } => format!(
                "[[exception]] {from} → {to} no longer matches any edge — \
                 the violation is fixed; delete the entry from \
                 quality/ARCH_LAYERS.toml"
            ),
        }
    }
}

// ── Parsing ───────────────────────────────────────────────────────────────────

pub fn parse(toml_text: &str) -> Result<LayerMap, String> {
    let map: LayerMap =
        toml::from_str(toml_text).map_err(|e| format!("ARCH_LAYERS.toml parse error: {e}"))?;
    if map.schema_version > MAX_SCHEMA_VERSION {
        return Err(format!(
            "ARCH_LAYERS.toml declares schema_version {} but this build \
             understands at most {MAX_SCHEMA_VERSION} — rebuild the consumers \
             before raising the version",
            map.schema_version
        ));
    }
    if map.layers.is_empty() {
        return Err("ARCH_LAYERS.toml declares no [[layer]] entries".to_string());
    }
    Ok(map)
}

/// `*`-wildcard match (any number of `*`, each matching any substring).
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    fn inner(p: &[u8], n: &[u8]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some(b'*') => (0..=n.len()).any(|i| inner(&p[1..], &n[i..])),
            Some(c) => n.first() == Some(c) && inner(&p[1..], &n[1..]),
        }
    }
    inner(pattern.as_bytes(), name.as_bytes())
}

// ── Evaluation ────────────────────────────────────────────────────────────────

/// Assign every crate to a layer and check every edge. `crates` must be the
/// COMPLETE workspace-member name set (totality is part of the contract);
/// `edges` are internal (member → member) dependency edges of any kind.
pub fn evaluate(map: &LayerMap, crates: &BTreeSet<String>, edges: &[DepEdge]) -> Vec<Violation> {
    let mut violations = Vec::new();

    // Layer assignment — total and unambiguous.
    let mut assignment: BTreeMap<&str, usize> = BTreeMap::new();
    for name in crates {
        let matches: Vec<usize> = map
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.crates.iter().any(|p| wildcard_match(p, name)))
            .map(|(i, _)| i)
            .collect();
        match matches.as_slice() {
            [] => violations.push(Violation::UnassignedCrate { name: name.clone() }),
            [i] => {
                assignment.insert(name.as_str(), *i);
            }
            many => violations.push(Violation::AmbiguousCrate {
                name: name.clone(),
                layers: many.iter().map(|&i| map.layers[i].name.clone()).collect(),
            }),
        }
    }

    // Edge checks. Dev edges are never enforced (see DepKind::Dev).
    let mut used_exceptions: BTreeSet<(String, String)> = BTreeSet::new();
    let mut excepted = |from: &str, to: &str| -> bool {
        for e in &map.exceptions {
            if e.from == from && e.to == to {
                used_exceptions.insert((e.from.clone(), e.to.clone()));
                return true;
            }
        }
        false
    };

    for edge in edges {
        if edge.kind == DepKind::Dev {
            continue;
        }
        // [[forbid]] rules first — they're the sharper statement.
        let forbidden = map.forbids.iter().find(|f| {
            wildcard_match(&f.from, &edge.from)
                && wildcard_match(&f.to, &edge.to)
                && !f.except.iter().any(|x| wildcard_match(x, &edge.to))
        });
        if let Some(rule) = forbidden {
            if !excepted(&edge.from, &edge.to) {
                violations.push(Violation::ForbiddenEdge {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    reason: rule.reason.clone(),
                });
            }
            continue; // don't double-report as an upward edge
        }
        // Layer direction: to-layer must be <= from-layer.
        if let (Some(&fi), Some(&ti)) = (
            assignment.get(edge.from.as_str()),
            assignment.get(edge.to.as_str()),
        ) {
            if ti > fi && !excepted(&edge.from, &edge.to) {
                violations.push(Violation::UpwardEdge {
                    from: edge.from.clone(),
                    from_layer: map.layers[fi].name.clone(),
                    to: edge.to.clone(),
                    to_layer: map.layers[ti].name.clone(),
                    kind: edge.kind,
                });
            }
        }
    }

    // Exceptions that suppressed nothing are debt already paid — flag them.
    for e in &map.exceptions {
        if !used_exceptions.contains(&(e.from.clone(), e.to.clone())) {
            violations.push(Violation::StaleException {
                from: e.from.clone(),
                to: e.to.clone(),
            });
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(from: &str, to: &str, kind: DepKind) -> DepEdge {
        DepEdge {
            from: from.into(),
            to: to.into(),
            kind,
        }
    }

    const MAP: &str = r#"
schema_version = 1

[[layer]]
name = "wire"
crates = ["oicp-*"]

[[layer]]
name = "runtime"
crates = ["sovereign-core", "sovereign-store"]

[[layer]]
name = "hosts"
crates = ["sovereign-cli*", "commonwealth-daemon"]

[[forbid]]
from = "commonwealth-*"
to = "sovereign-*"
except = ["sovereign-contracts"]
reason = "families meet only at the contracts seam"

[[exception]]
from = "sovereign-core"
to = "sovereign-cli"
reason = "grandfathered upward edge"
tracking = "R99"
"#;

    fn crates() -> BTreeSet<String> {
        [
            "oicp-types",
            "sovereign-core",
            "sovereign-store",
            "sovereign-cli",
            "commonwealth-daemon",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn wildcard_semantics() {
        assert!(wildcard_match("oicp-*", "oicp-types"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("sovereign-cli*", "sovereign-cli-dev"));
        assert!(wildcard_match("sovereign-cli*", "sovereign-cli"));
        assert!(!wildcard_match("oicp-*", "sovereign-core"));
        assert!(!wildcard_match("corpus-engine", "corpus-engine-scip"));
    }

    #[test]
    fn newer_schema_version_is_rejected() {
        let err =
            parse("schema_version = 99\n[[layer]]\nname = \"x\"\ncrates = [\"*\"]\n").unwrap_err();
        assert!(err.contains("schema_version 99"));
    }

    #[test]
    fn downward_and_same_layer_edges_pass_upward_fails() {
        let map = parse(MAP).unwrap();
        let edges = vec![
            edge("sovereign-cli", "sovereign-core", DepKind::Normal), // down: ok
            edge("sovereign-core", "sovereign-store", DepKind::Normal), // same: ok
            edge("sovereign-store", "sovereign-cli", DepKind::Normal), // UP: fails
        ];
        let v = evaluate(&map, &crates(), &edges);
        // One upward edge + the stale (unused) exception in MAP.
        assert_eq!(
            v.iter()
                .filter(|x| matches!(x, Violation::UpwardEdge { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn dev_edges_are_never_enforced() {
        let map = parse(MAP).unwrap();
        let edges = vec![edge("sovereign-store", "sovereign-cli", DepKind::Dev)];
        let v = evaluate(&map, &crates(), &edges);
        assert!(!v.iter().any(|x| matches!(x, Violation::UpwardEdge { .. })));
    }

    #[test]
    fn forbid_rule_fires_across_families_and_respects_except() {
        let map = parse(MAP).unwrap();
        let edges = vec![edge(
            "commonwealth-daemon",
            "sovereign-core",
            DepKind::Normal,
        )];
        let v = evaluate(&map, &crates(), &edges);
        assert!(v.iter().any(|x| matches!(
            x,
            Violation::ForbiddenEdge { from, to, .. }
                if from == "commonwealth-daemon" && to == "sovereign-core"
        )));
        // The except list would have allowed sovereign-contracts.
        let edges = vec![edge(
            "commonwealth-daemon",
            "sovereign-contracts",
            DepKind::Normal,
        )];
        let crates2: BTreeSet<String> = crates()
            .into_iter()
            .chain(["sovereign-contracts".to_string()])
            .collect();
        // sovereign-contracts is unassigned in MAP — expect only that finding,
        // no ForbiddenEdge.
        let v = evaluate(&map, &crates2, &edges);
        assert!(!v
            .iter()
            .any(|x| matches!(x, Violation::ForbiddenEdge { .. })));
    }

    #[test]
    fn exception_suppresses_and_stale_exception_reports() {
        let map = parse(MAP).unwrap();
        // The exception (sovereign-core → sovereign-cli) is USED here.
        let edges = vec![edge("sovereign-core", "sovereign-cli", DepKind::Normal)];
        let v = evaluate(&map, &crates(), &edges);
        assert!(!v.iter().any(|x| matches!(x, Violation::UpwardEdge { .. })));
        assert!(!v
            .iter()
            .any(|x| matches!(x, Violation::StaleException { .. })));
        // With no edges, the same exception is stale.
        let v = evaluate(&map, &crates(), &[]);
        assert!(v
            .iter()
            .any(|x| matches!(x, Violation::StaleException { .. })));
    }

    #[test]
    fn totality_unassigned_and_ambiguous() {
        let map = parse(MAP).unwrap();
        let mut names = crates();
        names.insert("mystery-crate".to_string());
        let v = evaluate(&map, &names, &[]);
        assert!(v.iter().any(|x| matches!(
            x,
            Violation::UnassignedCrate { name } if name == "mystery-crate"
        )));

        let overlapping = r#"
schema_version = 1
[[layer]]
name = "a"
crates = ["sovereign-*"]
[[layer]]
name = "b"
crates = ["*-core"]
"#;
        let map = parse(overlapping).unwrap();
        let names: BTreeSet<String> = ["sovereign-core".to_string()].into_iter().collect();
        let v = evaluate(&map, &names, &[]);
        assert!(v
            .iter()
            .any(|x| matches!(x, Violation::AmbiguousCrate { .. })));
    }
}
