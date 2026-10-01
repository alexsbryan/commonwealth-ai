// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `[[distribution]]` half of the layer map — composition roots that sit
//! OUTSIDE every package (FIVE_PROGRAMS §2c; phase-b-29 Q4).
//!
//! A distribution is the binary that puts programs together: the stock one
//! builds serve's provider in-process and hands it to svrn through a contracts
//! port. It belongs to no package, so [`crate::evaluate_packages`] skips its
//! edges (an ungoverned crate) — which, until this module, meant it could hold
//! ANY edge and no gate would say so (trial T1, 2026-09-27: a `[[distribution]]`
//! table parsed and was ignored).
//!
//! The rule is the `[thin_surfaces]` allowlist SHAPE over DIRECT edges, not
//! that block's closure rule: a distribution reaches each program whole
//! through the program's declared face crate, so judging its closure would
//! refuse the very thing it exists to do. What it may name directly is its own
//! crates, the shared leaves, and its faces' crates. Which ITEMS of a face
//! crate it may name is a source rule, checked by `xtask boundary-gate`.
//!
//! A crate several rows claim answers to EVERY row that claims it (phase-b-88):
//! each of its direct edges is judged against each claiming row and fails
//! under the name of every row that does not allow it, and boundary-gate runs
//! its face-item scan and counts its lines once per row. That intersection is
//! the strictest semantics a shared crate can have, so sharing never widens
//! what a row allows: it is how two distributions compose one thing one way
//! (ingest's hosting composition, `sovereign-hosted-ingest`) without a
//! `#[path]` mount out of either crate.
//!
//! `Distribution` is this TOML row's name, as `[[package]]` is `Package`. Its
//! kin elsewhere (`DistributionPlan`, sovereign-inference's layer split) is a
//! different concept.

use serde::Deserialize;
use std::collections::BTreeSet;

use crate::{DepEdge, LayerMap, Violation};

/// A composition root: a binary that composes programs through their faces.
#[derive(Debug, Deserialize)]
pub struct Distribution {
    pub name: String,
    /// Its own crates. Each is in no package and is no leaf. A crate another
    /// row also lists is that row's too, and answers to both (phase-b-88).
    pub crates: Vec<String>,
    /// The program faces it composes: the only package crates it may name.
    #[serde(default, rename = "face")]
    pub faces: Vec<Face>,
    /// Repo-relative path to the document that owns the rule.
    pub doc: String,
    /// A fixed ceiling on the distribution's code lines, counted as size-gate
    /// counts them. Set per row and never ratcheted: a composition root that
    /// grows is doing a program's work (phase-b-30 Group 2).
    pub max_code_lines: usize,
}

/// One program's face: the package crate a distribution may name, and the
/// items of it the distribution's source may name (`module::item` paths
/// relative to the crate root).
#[derive(Debug, Deserialize)]
pub struct Face {
    pub package: String,
    pub krate: String,
    pub items: Vec<String>,
}

/// Validate the distribution half of a parsed map, from [`crate::parse`].
pub(crate) fn validate(map: &LayerMap) -> Result<(), String> {
    // The same vacuity argument as `backstage` and `[[package]]`: v5 exists to
    // carry this rule, and an empty list would judge nothing and say so in
    // exactly the words of a clean run.
    if map.schema_version >= 5 && map.distributions.is_empty() {
        return Err(
            "ARCH_LAYERS.toml declares schema_version >= 5 but no [[distribution]] \
             entries. v5 exists to hold the composition roots to their faces, and \
             an empty list would make that rule vacuous. Declare them, or drop back \
             to schema_version = 4."
                .to_string(),
        );
    }
    let mut names: BTreeSet<&str> = BTreeSet::new();
    for dist in &map.distributions {
        if !names.insert(dist.name.as_str()) {
            return Err(format!(
                "ARCH_LAYERS.toml declares two [[distribution]] blocks named `{}`",
                dist.name
            ));
        }
        // A crate two rows list is no refusal: it answers to both
        // (`evaluate_distributions`; phase-b-88).
        for c in &dist.crates {
            if let Some(pkg) = map.packages.iter().find(|p| p.crates.contains(c)) {
                return Err(format!(
                    "distribution `{}` crate `{c}` is also a member of package `{}` — \
                     a composition root sits outside every package",
                    dist.name, pkg.name
                ));
            }
            if map.package_leaves.iter().any(|l| &l.name == c) {
                return Err(format!(
                    "distribution `{}` crate `{c}` is also a [[package_leaf]] — a \
                     composition root is no leaf",
                    dist.name
                ));
            }
        }
        for face in &dist.faces {
            let Some(pkg) = map.packages.iter().find(|p| p.name == face.package) else {
                return Err(format!(
                    "distribution `{}` face names package `{}`, which no [[package]] \
                     block declares",
                    dist.name, face.package
                ));
            };
            if !pkg.crates.contains(&face.krate) {
                return Err(format!(
                    "distribution `{}` face krate `{}` is not a member of package `{}`",
                    dist.name, face.krate, face.package
                ));
            }
        }
    }
    Ok(())
}

/// Distribution crates the map declares that the workspace does not contain.
/// Reported, never skipped in silence: a typo'd name governs nothing.
pub fn missing_distribution_crates(
    map: &LayerMap,
    crates: &BTreeSet<String>,
) -> Vec<(String, String)> {
    map.distributions
        .iter()
        .flat_map(|d| d.crates.iter().map(move |c| (d.name.clone(), c.clone())))
        .filter(|(_, c)| !crates.contains(c))
        .collect()
}

/// Judge every DIRECT edge out of a distribution crate: it may reach its own
/// crates, the shared leaves and its faces' crates, and nothing else. Dev and
/// build edges count, as in a package: the crate carries its tests. An edge
/// out of a crate several rows claim is judged under each of them, and is a
/// violation under every row that does not allow it (phase-b-88).
pub fn evaluate_distributions(map: &LayerMap, edges: &[DepEdge]) -> Vec<Violation> {
    let leaves: BTreeSet<&str> = map.package_leaves.iter().map(|l| l.name.as_str()).collect();
    let mut out = Vec::new();
    for edge in edges {
        let claiming = map
            .distributions
            .iter()
            .filter(|d| d.crates.contains(&edge.from));
        for dist in claiming {
            let allowed = dist.crates.contains(&edge.to)
                || leaves.contains(edge.to.as_str())
                || dist.faces.iter().any(|f| f.krate == edge.to);
            if !allowed {
                out.push(Violation::DistributionEdge {
                    distribution: dist.name.clone(),
                    doc: dist.doc.clone(),
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    kind: edge.kind,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod distribution_tests {
    use super::*;
    use crate::{parse, DepKind};

    const MAP: &str = r#"
schema_version = 5
backstage = ["xtask"]
[[layer]]
name = "all"
crates = ["*"]
[[package_leaf]]
name = "leaf"
allow = []
[[package]]
name = "svrn"
doc = "d"
crates = ["daemon", "inference"]
[[package]]
name = "serve"
doc = "d"
crates = ["serve"]
[thin_surfaces]
crates = ["desktop"]
may_not_reach = ["daemon"]
doc = "d"
bring_up_decider = { krate = "turn-client", file = "x/reach.rs", reason = "one decider" }
[[distribution]]
name = "stock"
crates = ["stock"]
doc = "docs/FIVE_PROGRAMS.md"
max_code_lines = 300
[[distribution.face]]
package = "svrn"
krate = "daemon"
items = ["process::run"]
[[distribution.face]]
package = "serve"
krate = "serve"
items = ["bundles"]
"#;

    fn edge(from: &str, to: &str) -> DepEdge {
        DepEdge {
            from: from.to_string(),
            to: to.to_string(),
            kind: DepKind::Normal,
            optional: false,
        }
    }

    /// The row's gate test: an edge past the faces and leaves is a named
    /// violation, and the face crates, the leaves and itself are not.
    #[test]
    fn a_distribution_edge_outside_leaves_and_faces_is_named() {
        let map = parse(MAP).unwrap();
        let ok = [
            edge("stock", "daemon"),
            edge("stock", "serve"),
            edge("stock", "leaf"),
        ];
        assert!(evaluate_distributions(&map, &ok).is_empty());
        let v = evaluate_distributions(&map, &[edge("stock", "inference")]);
        assert_eq!(v.len(), 1, "{v:?}");
        let msg = v[0].describe();
        assert!(msg.starts_with("[stock] stock → inference"), "{msg}");
        // A package crate's own edges are the package pass's, not this one's.
        assert!(evaluate_distributions(&map, &[edge("daemon", "inference")]).is_empty());
    }

    #[test]
    fn v5_map_without_distributions_is_refused() {
        let text = MAP.split("[[distribution]]").next().unwrap().to_string();
        let err = parse(&text).unwrap_err();
        assert!(err.contains("no [[distribution]]"), "{err}");
        assert!(parse(&text.replace("schema_version = 5", "schema_version = 4")).is_ok());
    }

    #[test]
    fn a_distribution_crate_cannot_be_a_package_member_or_a_leaf() {
        let err = parse(&MAP.replace("crates = [\"stock\"]", "crates = [\"serve\"]")).unwrap_err();
        assert!(err.contains("also a member of package `serve`"), "{err}");
        let err = parse(&MAP.replace("crates = [\"stock\"]", "crates = [\"leaf\"]")).unwrap_err();
        assert!(err.contains("also a [[package_leaf]]"), "{err}");
    }

    /// MAP with a second row, `onprem`, and a crate `shared` both rows list.
    fn shared_map() -> String {
        MAP.replace("crates = [\"stock\"]", "crates = [\"stock\", \"shared\"]")
            + r#"
[[distribution]]
name = "onprem"
crates = ["onprem", "shared"]
doc = "docs/FIVE_PROGRAMS.md"
max_code_lines = 200
[[distribution.face]]
package = "svrn"
krate = "daemon"
items = ["process::run"]
"#
    }

    /// phase-b-88: a crate two rows claim is no refusal; it answers to both.
    #[test]
    fn a_crate_claimed_by_two_distributions_is_valid() {
        let map = parse(&shared_map()).expect("a shared crate parses");
        let claiming: Vec<&str> = map
            .distributions
            .iter()
            .filter(|d| d.crates.iter().any(|c| c == "shared"))
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(claiming, ["stock", "onprem"]);
    }

    /// The row's gate test (phase-b-88): an edge out of the shared crate that
    /// stock's faces allow and onprem's do not fails under onprem's name only;
    /// one neither allows fails under both; each row's own crates may name it.
    #[test]
    fn a_shared_crate_edge_is_judged_under_every_row_that_claims_it() {
        let map = parse(&shared_map()).unwrap();
        let ok = [
            edge("shared", "daemon"),
            edge("shared", "leaf"),
            edge("stock", "shared"),
            edge("onprem", "shared"),
        ];
        assert!(evaluate_distributions(&map, &ok).is_empty());
        let v = evaluate_distributions(&map, &[edge("shared", "serve")]);
        assert_eq!(v.len(), 1, "{v:?}");
        let msg = v[0].describe();
        assert!(msg.starts_with("[onprem] shared → serve"), "{msg}");
        let v = evaluate_distributions(&map, &[edge("shared", "inference")]);
        let named: Vec<String> = v.iter().map(|v| v.describe()).collect();
        assert_eq!(named.len(), 2, "{named:?}");
        assert!(named[0].starts_with("[stock] shared → inference"), "{named:?}");
        assert!(named[1].starts_with("[onprem] shared → inference"), "{named:?}");
    }

    #[test]
    fn a_face_krate_must_be_a_member_of_its_package() {
        let err = parse(&MAP.replace("krate = \"serve\"", "krate = \"inference\"")).unwrap_err();
        assert!(err.contains("not a member of package `serve`"), "{err}");
        let err = parse(&MAP.replace("package = \"serve\"\nkrate", "package = \"nope\"\nkrate"))
            .unwrap_err();
        assert!(err.contains("no [[package]]"), "{err}");
    }
}
