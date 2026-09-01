// SPDX-License-Identifier: AGPL-3.0-or-later
//! Generates `quality/requirements.toml` from `research/clean-room/REQUIREMENTS.md`
//! and gates it against drift, on the `corpus-engine/tests/main/recipe_schema.rs`
//! pattern: parse → render → compare, with an env var to regenerate.
//!
//! ```text
//! UPDATE_REQUIREMENTS=1 cargo test -p kernel-types --test requirements_registry
//! ```
//!
//! This is a plain `#[test]` in a workspace member, so it rides the live
//! `test:` CI job — which `ci-ok` needs — at no marginal cost. It needed no gate
//! of its own; an earlier draft added one, and the gate re-decided what this
//! file already decides.
//!
//! The parser lives in `requirements_registry/spec_parser.rs` and carries the
//! reasoning behind its refusals.

#[path = "requirements_registry/spec_parser.rs"]
mod spec_parser;

use kernel_types::conformance::{Enforceability, ReqLevel, RequirementRegistry};
use spec_parser::{parse, SPEC};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// The generated registry, relative to the repo root.
const OUT: &str = "quality/requirements.toml";
/// The one hand-authored column: how each requirement can be settled.
const ENFORCEABILITY: &str = "quality/requirements-enforceability.toml";

const HEADER: &str = "\
# Requirement registry — GENERATED from research/clean-room/REQUIREMENTS.md.
#
# DO NOT EDIT BY HAND. Regenerate after an intentional spec change:
#   UPDATE_REQUIREMENTS=1 cargo test -p kernel-types --test requirements_registry
#
# `spec_hash` is the blake3 ContentHash of the specification at generation
# time. A consumer that reads a different hash has a registry that no longer
# describes the spec and must say so rather than render a verdict (ARCH §18.4).
#
# Buckets: 625 in scope (591 must-class + 34 should), 10 out-of-scope (§17),
# 5 aliases (§4.4). Out-of-scope and alias entries are PRESENT and excluded,
# never absent — see kernel-types/src/conformance.rs.
";

// ─── The pinned histogram (ARCH §18.4) ──────────────────────────────────────

/// In-scope requirements. The kill bar reads **625 ± 0** — a range is not a
/// denominator.
const IN_SCOPE: usize = 625;
/// Of those, the ones whose violation means non-conformance.
const MUST_CLASS: usize = 591;
/// Of those, the ones deviation from which merely requires a stated reason.
const SHOULD: usize = 34;
/// `§17` — deliberate absences, present and never judged.
const OUT_OF_SCOPE: usize = 10;
/// `§4.4` — `ST-16 … ST-20`, restatements of `X-PR-1 … X-PR-5`.
const ALIASES: usize = 5;
/// `§16` — A-1 … A-19.
const SCENARIOS: usize = 19;
/// Settleable with no model at all (`cli` + `desktop` + `structural`). This is
/// the premise a fast tier rests on, so it is pinned rather than printed.
const MODEL_FREE: usize = 582;

/// In-scope count per family. A family that moves without an intended spec edit
/// is the parser losing a declaration form.
const FAMILY_COUNTS: &[(&str, usize)] = &[
    ("CI", 48),
    ("EN", 21),
    ("EV", 38),
    ("FE", 141),
    ("GR", 53),
    ("IN", 31),
    ("KA", 27),
    ("NF", 23),
    ("OP", 24),
    ("RT", 71),
    ("ST", 41),
    ("UI", 35),
    ("WF", 11),
    ("X-CF", 4),
    ("X-CO", 3),
    ("X-DG", 8),
    ("X-EH", 9),
    ("X-EX", 6),
    ("X-OB", 6),
    ("X-PR", 7),
    ("X-PV", 8),
    ("X-SD", 5),
    ("X-ST", 5),
];

/// Repo root — kernel-types sits directly under it.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("kernel-types has no parent")
        .to_path_buf()
}

#[test]
fn requirements_registry_is_fresh() {
    let root = repo_root();
    let spec_path = root.join(SPEC);
    let spec = std::fs::read_to_string(&spec_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", spec_path.display()));
    let registry = parse(&spec);

    // The instrument is checked before its result (ARCH §18.4). These run even
    // under UPDATE_REQUIREMENTS, so regenerating cannot bless a broken parse.
    assert_histogram(&registry);
    assert_every_column_resolves(&root, &registry);

    let rendered = format!(
        "{HEADER}{}",
        toml::to_string_pretty(&registry).expect("registry serialises")
    );
    let out_path = root.join(OUT);
    if std::env::var("UPDATE_REQUIREMENTS").is_ok() {
        if let Some(dir) = out_path.parent() {
            std::fs::create_dir_all(dir).expect("create quality/");
        }
        std::fs::write(&out_path, &rendered).expect("write requirements.toml");
        eprintln!("wrote {}", out_path.display());
        return;
    }

    let committed = std::fs::read_to_string(&out_path).unwrap_or_else(|e| {
        panic!(
            "cannot read {} ({e}).\nRegenerate:\n  UPDATE_REQUIREMENTS=1 cargo test -p kernel-types --test requirements_registry",
            out_path.display()
        )
    });
    if committed != rendered {
        panic!(
            "{OUT} is stale against {SPEC}.\n{}\n\
             The spec and the registry must land in the same commit.\n\
             Regenerate:\n  UPDATE_REQUIREMENTS=1 cargo test -p kernel-types --test requirements_registry",
            first_diff(&committed, &rendered)
        );
    }

    // Round-trip: what a consumer will actually parse is what was rendered.
    let reparsed: RequirementRegistry = toml::from_str(&committed[HEADER.len()..])
        .expect("committed registry parses back into RequirementRegistry");
    assert_eq!(reparsed, registry, "{OUT} does not round-trip");
}

/// The differing lines, not just their number. On a 6,000-line generated file
/// that is the difference between diagnosing and re-running.
fn first_diff(a: &str, b: &str) -> String {
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            return format!(
                "first diff at line {}:\n  committed: {x}\n  generated: {y}",
                i + 1
            );
        }
    }
    format!(
        "(content is a prefix/length mismatch: {} committed lines vs {} generated)",
        a.lines().count(),
        b.lines().count()
    )
}

/// Every count the campaign pre-registered, checked at once so a drifting parser
/// fails loudly here instead of reporting a smaller number that still reads
/// fine.
fn assert_histogram(reg: &RequirementRegistry) {
    let in_scope = reg.in_scope().count();
    let must = reg.must_class().count();
    let should = reg
        .in_scope()
        .filter(|r| r.level == ReqLevel::Should)
        .count();
    let oos = reg
        .requirements
        .iter()
        .filter(|r| r.level == ReqLevel::OutOfScope && r.alias_of.is_none())
        .count();
    let aliases = reg
        .requirements
        .iter()
        .filter(|r| r.alias_of.is_some())
        .count();

    assert_eq!(in_scope, IN_SCOPE, "in-scope requirement count moved");
    assert_eq!(must, MUST_CLASS, "must-class count moved");
    assert_eq!(should, SHOULD, "should count moved");
    assert_eq!(
        must + should,
        in_scope,
        "every in-scope requirement is must-class or should — a third bucket appeared"
    );
    assert_eq!(oos, OUT_OF_SCOPE, "§17 out-of-scope count moved");
    assert_eq!(aliases, ALIASES, "§4.4 alias count moved");
    assert_eq!(reg.scenarios.len(), SCENARIOS, "§16 scenario count moved");
    // A scenario citing nothing is a hole in the acceptance suite. There is
    // exactly one, and it is named rather than silently tolerated: A-6 states
    // the single-decider test over eight NAMED SUBSYSTEMS ("storage layout,
    // readiness, …") and cites no requirement id at all.
    let uncited: Vec<&str> = reg
        .scenarios
        .iter()
        .filter(|s| s.cites.is_empty())
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(
        uncited,
        vec!["A-6"],
        "the set of acceptance scenarios that cite no requirement moved"
    );
    assert_eq!(
        reg.family_counts(),
        FAMILY_COUNTS
            .iter()
            .map(|(f, n)| ((*f).to_string(), *n))
            .collect::<Vec<_>>(),
        "per-family histogram moved"
    );
}

/// The hand-authored column is id-for-id equal to the registry, and no pointer
/// dangles.
///
/// Both halves are absence checks (ARCH §18.3). A requirement with no
/// enforceability class cannot be scheduled into a tier, so it would silently
/// never be checked; a class that outlived its requirement is a claim about
/// nothing; and a citation or alias naming a requirement that no longer exists
/// reads as coverage while resolving to nobody.
fn assert_every_column_resolves(root: &std::path::Path, reg: &RequirementRegistry) {
    let path = root.join(ENFORCEABILITY);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let enforceability: BTreeMap<String, Enforceability> =
        toml::from_str(&text).unwrap_or_else(|e| panic!("{ENFORCEABILITY}: {e}"));

    let ids: BTreeSet<&str> = reg.in_scope().map(|r| r.id.as_str()).collect();
    let classified: BTreeSet<&str> = enforceability.keys().map(String::as_str).collect();
    let unclassified: Vec<&&str> = ids.difference(&classified).collect();
    let orphaned: Vec<&&str> = classified.difference(&ids).collect();
    assert!(
        unclassified.is_empty(),
        "{} requirement(s) have no enforceability class: {unclassified:?}\n  \
         Add each to {ENFORCEABILITY}.",
        unclassified.len()
    );
    assert!(
        orphaned.is_empty(),
        "{} enforceability entr(ies) name no requirement: {orphaned:?}\n  \
         Remove each from {ENFORCEABILITY}.",
        orphaned.len()
    );

    let model_free = reg
        .in_scope()
        .filter_map(|r| enforceability.get(&r.id))
        .filter(|e| e.is_model_free())
        .count();
    assert_eq!(
        model_free, MODEL_FREE,
        "the count settleable with no model moved — this is the premise the fast \
         tier rests on, and the `gr-is-model-free` bar reads it"
    );

    for r in &reg.requirements {
        if let Some(dep) = &r.conditional_on {
            assert!(
                reg.get(dep).is_some(),
                "{} is conditional on {dep}, which is not a requirement",
                r.id
            );
        }
        if let Some(target) = &r.alias_of {
            assert!(
                reg.get(target).is_some(),
                "{} aliases {target}, which is not a requirement",
                r.id
            );
        }
    }
    for s in &reg.scenarios {
        for cited in &s.cites {
            assert!(
                reg.resolve(cited).is_some(),
                "acceptance scenario {} cites {cited}, which is not a requirement",
                s.id
            );
        }
    }
}
