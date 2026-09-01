// SPDX-License-Identifier: AGPL-3.0-or-later
//! The requirement registry — what the specification obliges, as data.
//!
//! `research/clean-room/REQUIREMENTS.md` is a reverse-engineered specification
//! of this system: 625 requirements, each carrying a `⟨why⟩` block naming the
//! real incident that produced it. Until this module existed, **nothing in the
//! codebase referenced a single one of those ids** — so "which MUSTs does this
//! system satisfy" had no answer, and therefore could not be asked of a
//! rewrite, which is the whole point of having the specification at all.
//!
//! # This is a registry, not a snapshot
//!
//! `quality/requirements.toml` is GENERATED from the specification by
//! `kernel-types/tests/requirements_registry.rs` and byte-gated against it.
//! Editing the prose without regenerating fails a gate; editing the generated
//! file by hand fails the same gate. The registry carries [`Registry::spec_hash`]
//! so a consumer can refuse to render a verdict against a spec it has not read
//! (ARCH §18.4 — validate the instrument before the result).
//!
//! # Why the kernel owns it
//!
//! Three unrelated surfaces need to name a requirement: the CLI contract, the
//! desktop journey manifest, and the xtask gate table. A registry owned by any
//! one of them would make the other two depend on that one's world. This crate
//! already owns [`Verdict`](crate::Verdict) — the vocabulary a conformance
//! result is spoken in — and names nothing above itself, which is the same
//! reason `Verdict` is here.
//!
//! # Four buckets, and none of them is silence
//!
//! A requirement is in exactly one of: **in scope** (the 625 that count),
//! **out of scope** (§17's `OS-*`, which the spec says a rebuild "MUST NOT be
//! judged for not addressing"), or **an alias** of a requirement stated
//! elsewhere (`ST-16 … ST-20`). Out-of-scope and alias entries are *present and
//! labelled*, never dropped: a denominator that can be shrunk by omission is
//! not a denominator (ARCH §18.3).

use serde::{Deserialize, Serialize};

/// The obligation level a requirement carries, per `REQUIREMENTS.md §0.3`.
///
/// A closed set (ARCH §2). `Invariant` and `Bar` are MUST-class — the spec
/// defines an INVARIANT as "a property that must hold at all times" and a BAR
/// as "a falsifiable acceptance threshold" — so [`ReqLevel::is_must_class`],
/// not equality with `Must`, is what a gate asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReqLevel {
    /// A conformance requirement. A build that violates one is non-conforming.
    Must,
    /// Strongly expected; deviation requires a stated reason.
    Should,
    /// A property that must hold at all times, enforced structurally.
    Invariant,
    /// A falsifiable acceptance threshold with a measurement procedure.
    Bar,
    /// `REQUIREMENTS.md §17` — a deliberate absence. Present in the registry
    /// and excluded from every denominator, because "a rebuild MAY address
    /// them; it MUST NOT be judged for not addressing them".
    OutOfScope,
}

impl ReqLevel {
    /// The wire spelling. Stable — `quality/requirements.toml` carries it.
    pub const fn as_str(self) -> &'static str {
        match self {
            ReqLevel::Must => "must",
            ReqLevel::Should => "should",
            ReqLevel::Invariant => "invariant",
            ReqLevel::Bar => "bar",
            ReqLevel::OutOfScope => "out-of-scope",
        }
    }

    /// Does violating this make the build non-conforming? True for `Must`,
    /// `Invariant` and `Bar` — the three the spec grades as obligations.
    pub const fn is_must_class(self) -> bool {
        matches!(self, ReqLevel::Must | ReqLevel::Invariant | ReqLevel::Bar)
    }
}

/// How a requirement can be settled mechanically, if at all.
///
/// The one hand-authored column. It lives in
/// `quality/requirements-enforceability.toml` rather than in the generated
/// registry so regenerating from the spec cannot clobber a judgement no parser
/// could make; the generator asserts the two id sets are equal, so a new
/// requirement cannot arrive unclassified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Enforceability {
    /// Black-box command plus an assertion on its output.
    Cli,
    /// Needs the desktop app surface or a chat turn.
    Desktop,
    /// Needs a live model — answer quality, calibration, routing accuracy.
    Model,
    /// A type, a lint, or a source-scanning test. No process is started.
    Structural,
    /// No automated check can settle it. Reported as such, never counted as
    /// covered and never counted as failing.
    Review,
}

impl Enforceability {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Enforceability::Cli => "cli",
            Enforceability::Desktop => "desktop",
            Enforceability::Model => "model",
            Enforceability::Structural => "structural",
            Enforceability::Review => "review",
        }
    }

    /// Can this be settled without model weights? True for everything but
    /// [`Enforceability::Model`] and [`Enforceability::Review`] — which is the
    /// property that makes a fast tier possible at all.
    pub const fn is_model_free(self) -> bool {
        matches!(
            self,
            Enforceability::Cli | Enforceability::Desktop | Enforceability::Structural
        )
    }
}

/// One requirement, as the specification states it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    /// `GR-19`, `X-EH-2`, `FE-141`. The id the whole system tags against.
    pub id: String,
    /// The id's prefix: `GR`, `X-EH`, `FE`.
    pub family: String,
    /// The id's ordinal within its family.
    pub n: u32,
    /// The obligation level, resolved from the declaration or — for a bare
    /// declaration — from the section's declared default.
    pub level: ReqLevel,
    /// The `##` heading it lives under, e.g. `8. D6 — Grounding and epistemic
    /// integrity`.
    pub domain: String,
    /// The `###` heading it lives under, e.g. `8.1 The grounding gate`.
    pub section: String,
    /// 1-indexed line of the declaration in `REQUIREMENTS.md`.
    pub spec_line: u32,
    /// The requirement's own words, up to but excluding its `⟨why⟩` block.
    pub text: String,
    /// For `MUST, where X is implemented`: the requirement whose implementation
    /// is the antecedent. An unimplemented antecedent resolves the requirement
    /// to `could-not-judge`, never to covered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditional_on: Option<String>,
    /// This id is a restatement of another (`ST-18` → `X-PR-3`). Aliases
    /// contribute zero to every denominator, but resolve when tagged, so a
    /// `covers: ST-18` is not reported as an unknown id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_of: Option<String>,
}

impl Requirement {
    /// Does this requirement count toward a conformance denominator? False for
    /// aliases and for `§17` out-of-scope entries.
    pub fn is_in_scope(&self) -> bool {
        self.alias_of.is_none() && self.level != ReqLevel::OutOfScope
    }
}

/// One acceptance scenario from `REQUIREMENTS.md §16 "How a rebuild is judged"`.
///
/// A-1 … A-19 are the suite the specification already wrote. They are not
/// requirements — they are the functional scenarios that exercise them, each
/// citing the ids it covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// `A-1` … `A-19`.
    pub id: String,
    /// The `###` heading, e.g. `16.1 The honesty acceptance suite`.
    pub suite: String,
    /// 1-indexed line of the declaration in `REQUIREMENTS.md`.
    pub line: u32,
    /// The scenario as written.
    pub text: String,
    /// Requirement ids named in the scenario's own text, in order of first
    /// appearance. Every one must resolve in the registry.
    pub cites: Vec<String>,
}

/// The whole registry: one specification, parsed once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    /// Hex [`ContentHash`](crate::ContentHash) of `REQUIREMENTS.md` at
    /// generation time. A consumer that finds a different hash has a registry
    /// that no longer describes the spec, and must say so rather than render a
    /// verdict (ARCH §18.4).
    ///
    /// Blake3, not SHA-256, deliberately: this crate already owns THE
    /// content-hash implementation, and a second one would be exactly the
    /// duplicate decider ARCH §10.6 forbids.
    pub spec_hash: String,
    /// Line count of the specification at generation time — a cheap second
    /// signal that says *how far* it moved when the hash disagrees.
    pub spec_lines: u32,
    /// Every declaration: in-scope requirements, `§17` out-of-scope entries,
    /// and aliases. Sorted by family then ordinal.
    pub requirements: Vec<Requirement>,
    /// A-1 … A-19, in document order.
    pub scenarios: Vec<Scenario>,
}

impl Registry {
    /// The requirements that count toward a denominator — 625 of them.
    pub fn in_scope(&self) -> impl Iterator<Item = &Requirement> {
        self.requirements.iter().filter(|r| r.is_in_scope())
    }

    /// In-scope requirements whose violation means non-conformance.
    pub fn must_class(&self) -> impl Iterator<Item = &Requirement> {
        self.in_scope().filter(|r| r.level.is_must_class())
    }

    /// Look one up. Returns `None` rather than a placeholder: an unknown id is
    /// an absence and absence is reported, never defaulted (ARCH §18.3).
    pub fn get(&self, id: &str) -> Option<&Requirement> {
        self.requirements.iter().find(|r| r.id == id)
    }

    /// Resolve an id through aliasing, so a tag on `ST-18` reaches `X-PR-3`.
    pub fn resolve(&self, id: &str) -> Option<&Requirement> {
        match self.get(id) {
            Some(r) => match &r.alias_of {
                Some(target) => self.get(target),
                None => Some(r),
            },
            None => None,
        }
    }

    /// In-scope count per family, for the pinned histogram. A count that moves
    /// without an intended spec edit is the parser losing a declaration form —
    /// which is how an earlier count lost the entire 53-requirement `GR`
    /// family and was wrong by 112.
    pub fn family_counts(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        for r in self.in_scope() {
            match out.iter_mut().find(|(f, _)| *f == r.family) {
                Some((_, n)) => *n += 1,
                None => out.push((r.family.clone(), 1)),
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// INVARIANT and BAR are obligations, not a softer third thing. A gate
    /// that asks `level == Must` silently drops 63 of the 591.
    #[test]
    fn invariant_and_bar_are_must_class() {
        assert!(ReqLevel::Must.is_must_class());
        assert!(ReqLevel::Invariant.is_must_class());
        assert!(ReqLevel::Bar.is_must_class());
        assert!(!ReqLevel::Should.is_must_class());
        assert!(!ReqLevel::OutOfScope.is_must_class());
    }

    /// Out-of-scope and alias entries are PRESENT and excluded, not absent.
    /// A denominator that can be shrunk by omission is not a denominator.
    #[test]
    fn aliases_and_out_of_scope_are_present_but_never_counted() {
        let req = |id: &str, level, alias: Option<&str>| Requirement {
            id: id.into(),
            family: "ST".into(),
            n: 1,
            level,
            domain: "d".into(),
            section: "s".into(),
            spec_line: 1,
            text: "t".into(),
            conditional_on: None,
            alias_of: alias.map(String::from),
        };
        let reg = Registry {
            spec_hash: "0".into(),
            spec_lines: 1,
            requirements: vec![
                req("ST-1", ReqLevel::Must, None),
                req("ST-18", ReqLevel::Must, Some("X-PR-3")),
                req("OS-1", ReqLevel::OutOfScope, None),
                req("X-PR-3", ReqLevel::Must, None),
            ],
            scenarios: vec![],
        };
        assert_eq!(reg.in_scope().count(), 2);
        assert_eq!(reg.must_class().count(), 2);
        assert!(reg.get("OS-1").is_some(), "still addressable by id");
        assert_eq!(reg.resolve("ST-18").map(|r| r.id.as_str()), Some("X-PR-3"));
        assert!(reg.resolve("ST-999").is_none(), "unknown is None, not a stub");
    }

    /// The two classes a fast tier cannot reach are named, not inferred.
    #[test]
    fn model_and_review_are_the_only_classes_a_fast_tier_cannot_reach() {
        assert!(Enforceability::Cli.is_model_free());
        assert!(Enforceability::Desktop.is_model_free());
        assert!(Enforceability::Structural.is_model_free());
        assert!(!Enforceability::Model.is_model_free());
        assert!(!Enforceability::Review.is_model_free());
    }
}
