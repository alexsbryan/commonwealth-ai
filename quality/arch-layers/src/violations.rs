// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a policy failure IS, and how it reads to the person who has to fix it.
//!
//! One enum for every rule in the map — layer direction, `[[forbid]]`, the
//! back-of-house one-way rule, and the package closures. Callers render a
//! single list and one enum is one decider (ARCH §10.6), so a new rule adds a
//! variant here rather than a second failure channel.
//!
//! Split from `lib.rs` because this is the half that grows every time a gate
//! learns a rule: each variant carries a paragraph of remediation prose, and
//! that prose is the gate's entire user interface. Keeping it beside the
//! schema pushed the file into ARCH §3.1's approach band on the commit that
//! added the package rules.

use crate::DepKind;

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
    ///
    /// ONE variant for the one rule, whichever pass found it (ARCH §10.6).
    /// `package` says WHICH pass, because that decides which exception ledger
    /// grandfathers it: `None` is the layer pass (a bare `[[exception]]`),
    /// `Some(scope)` the package pass (an `[[exception]]` carrying
    /// `package = "<scope>"`). The two ledgers are deliberately separate — see
    /// [`crate::evaluate`] — so a message that named the wrong one would send
    /// the reader to write a row that suppresses nothing.
    ForbiddenEdge {
        package: Option<String>,
        from: String,
        to: String,
        kind: DepKind,
        reason: String,
    },
    /// An `[[exception]]` no live edge needed — delete it, it's already won.
    StaleException { from: String, to: String },
    /// A package crate (or a shared leaf) reaching outside its pinned budget.
    PackageEdge {
        package: String,
        doc: String,
        from: String,
        to: String,
        kind: DepKind,
    },
    /// A package `[[exception]]` no live edge needed — same rule as
    /// [`Violation::StaleException`]: removing the last offending edge must
    /// also delete the entry, so the burn-down is visible in the policy file.
    StalePackageException {
        package: String,
        from: String,
        to: String,
    },
    /// A declared thin surface that can REACH a crate it may not become.
    ///
    /// The only variant keyed on a closure rather than an edge, because the
    /// ability it names is transitive: Cargo links what a crate reaches, not
    /// what it names. `path` is the shortest route, so the reader deletes the
    /// hop that actually carries it rather than hunting the manifest for a
    /// direct edge that is not there.
    SurfaceReach {
        surface: String,
        reached: String,
        /// Shortest path, `surface` first and `reached` last.
        path: Vec<String>,
        doc: String,
    },
    /// A product crate depending on a back-of-house crate in its default
    /// build. The one-way rule runs the other way: back-of-house observes the
    /// product, never the reverse.
    BackstageEdge {
        from: String,
        to: String,
        kind: DepKind,
    },
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
            Violation::ForbiddenEdge {
                package,
                from,
                to,
                kind,
                reason,
            } => {
                let dep = match kind {
                    DepKind::Normal => "a normal",
                    DepKind::Build => "a build",
                    DepKind::Dev => "a dev",
                };
                match package {
                    None => format!(
                        "{from} → {to}: {dep} dependency forbidden by a [[forbid]] \
                         rule ({reason}) — remove the edge or grandfather it with \
                         a [[exception]] entry"
                    ),
                    // Deliberately NOT prefixed `[{scope}]`: boundary-gate keys
                    // its per-package closure print on that prefix, and the
                    // closure is the wrong thing to show here. The offending
                    // target is usually a declared shared leaf, so the closure
                    // listing would name it as permitted — the reader would go
                    // diff a budget that is not the rule they broke.
                    Some(scope) => format!(
                        "{from} → {to}: {dep} dependency forbidden by a [[forbid]] \
                         rule ({reason}). A [[forbid]] row outranks package \
                         membership AND the shared-leaf allowance, so an edge to a \
                         declared [[package_leaf]] still fails the `{scope}` \
                         package pass. Remove the edge, or grandfather it with an \
                         [[exception]] carrying `package = \"{scope}\"` — a bare \
                         [[exception]] answers to layer-gate and will NOT suppress \
                         this one."
                    ),
                }
            }
            Violation::StaleException { from, to } => format!(
                "[[exception]] {from} → {to} no longer matches any edge — \
                 the violation is fixed; delete the entry from \
                 quality/ARCH_LAYERS.toml"
            ),
            // Deliberately one line. The first run of a newly declared
            // package prints its whole failure list — ~130 edges for
            // code-intel's target shape — and a per-line repeat of the closure
            // buries the edges it is supposed to show. The closure is printed
            // once per package by the gate's header.
            Violation::PackageEdge {
                package,
                doc,
                from,
                to,
                kind,
            } => format!(
                "[{package}] {from} → {to}: {} dependency leaves the package \
                 closure ({doc}) — move what needs `{to}` outside the package \
                 and inject it through a trait, or grandfather the edge with \
                 an [[exception]] carrying `package = \"{package}\"`",
                match kind {
                    DepKind::Normal => "a normal",
                    DepKind::Build => "a build",
                    // Unlike the layer map, a package DOES enforce dev edges:
                    // a crate a third party lifts carries its tests.
                    DepKind::Dev => "a dev",
                }
            ),
            Violation::StalePackageException { package, from, to } => format!(
                "[[exception]] (package = \"{package}\") {from} → {to} no \
                 longer matches any edge — the package got cleaner; delete \
                 the entry from quality/ARCH_LAYERS.toml"
            ),
            Violation::SurfaceReach {
                surface,
                reached,
                path,
                doc,
            } => {
                let route = path.join(" → ");
                // Name the FIRST hop explicitly. A direct reach and a
                // three-crate chain read identically in a path line, and the
                // edge a reader can actually delete is the one in `surface`'s
                // own manifest.
                let hop = path.get(1).map(String::as_str).unwrap_or(reached);
                format!(
                    "{surface} can reach `{reached}`, via {route} — a thin \
                     surface may not LINK a crate that assembles or hosts a \
                     backend, however indirectly ({doc}). Delete `{surface} → \
                     {hop}` from its Cargo.toml. If this build genuinely must \
                     ship a backend it can bring up, that is a DECLARED \
                     capability with an owner and a review-by, never a \
                     dependency nobody noticed — grandfather it with an \
                     [[exception]] naming the rung that removes it."
                )
            }
            Violation::BackstageEdge { from, to, kind } => format!(
                "{from} → {to}: {} dependency on a `backstage` crate that the \
                 DEFAULT build carries — the quality controls observe the \
                 product, never the reverse (a bench you cannot ship without \
                 is not a bench). Fix it by making the dep `optional = true` \
                 and leaving it out of `default`, so the shipped artifact \
                 builds without its own instrument. NOTE: this gate's unit is \
                 the CRATE — it cannot see which module names the type, and \
                 Cargo still links `{to}` into the product binary wherever an \
                 [[exception]] tolerates the edge.",
                match kind {
                    DepKind::Normal => "a normal",
                    DepKind::Build => "a build",
                    DepKind::Dev => "a dev",
                }
            ),
        }
    }
}
