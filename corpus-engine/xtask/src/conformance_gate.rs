// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cargo xtask conformance-gate` — the requirement registry still describes the
//! specification, and every column that hangs off it still resolves.
//!
//! This is the cheapest possible member of the conformance suite and the only
//! one that is free: it starts no process, compiles nothing, loads no model, and
//! reads two files. That is deliberate. The `gates:` CI job was shelved for a
//! cost reason (`.github/workflows/ci.yml:596` — "they used to be two jobs paying
//! the setup twice"), and a gate that is expensive gets shelved no matter how
//! good it is. This one joins the ALREADY-LIVE `test:` job, where `xtask` has
//! already been compiled, at a marginal cost of roughly zero.
//!
//! # What it actually judges
//!
//! Four things, each of which is a way the registry can become a plausible lie:
//!
//! 1. **The spec moved and the registry did not.** `spec_hash` is the blake3
//!    hash of `REQUIREMENTS.md` at generation time. A mismatch means every id,
//!    line number and level below it is describing a document that no longer
//!    exists (ARCH §18.4 — validate the instrument before the result).
//! 2. **The denominator moved.** 625 in scope, 591 must-class, 34 should. A
//!    parser that starts losing a declaration form reports a smaller number that
//!    still reads fine; an earlier count of this same spec was wrong by 112 that
//!    way.
//! 3. **A column outlived its requirement, or a requirement arrived without
//!    one.** The hand-authored enforceability map must be id-for-id equal to the
//!    registry's in-scope set.
//! 4. **A pointer dangles.** Every `conditional_on` antecedent and every
//!    acceptance-scenario citation must resolve to a real requirement.
//!
//! # Four verdicts, not two (ARCH §18.2)
//!
//! Exit `0` pass · `1` the evidence disagrees · `3` could-not-judge, the
//! evidence could not be reached · `4` never-ran, the registry is empty. The
//! last is `X-EH-3` applied to this gate itself: a zero-work run must not report
//! success.

use kernel_types::conformance::{Enforceability, Registry};
use kernel_types::ContentHash;
use std::collections::BTreeMap;

use crate::common::repo_root;

/// The specification this registry describes.
const SPEC: &str = "research/clean-room/REQUIREMENTS.md";
/// The generated registry.
const REGISTRY: &str = "quality/requirements.toml";
/// The one hand-authored column.
const ENFORCEABILITY: &str = "quality/requirements-enforceability.toml";

/// The command that repairs every drift this gate reports.
const REGENERATE: &str =
    "UPDATE_REQUIREMENTS=1 cargo test -p kernel-types --test requirements_registry";

/// Pinned buckets. Each is a kill bar from `quality/campaigns/conformance.toml`:
/// a range is not a denominator.
const IN_SCOPE: usize = 625;
const MUST_CLASS: usize = 591;
const SHOULD: usize = 34;

/// Has the specification moved since the registry was generated?
///
/// The gate that gates the rest (ARCH §18.4). Every `spec_line` in the registry
/// is an offset into a specific document; against a different document they are
/// confident nonsense. `None` means the registry still describes what is on disk.
fn spec_drift(spec: &str, registry: &Registry) -> Option<String> {
    let live = ContentHash::of_str(spec).to_hex();
    if live == registry.spec_hash {
        return None;
    }
    Some(format!(
        "{SPEC} has changed since {REGISTRY} was generated.\n      \
         registry spec_hash {} ({} lines)\n      \
         on-disk  spec_hash {} ({} lines)\n      \
         Every id, line number and level in the registry describes the OLDER document.\n      \
         The spec and the registry must land in the same commit. Regenerate:\n        {REGENERATE}",
        &registry.spec_hash[..16.min(registry.spec_hash.len())],
        registry.spec_lines,
        &live[..16],
        spec.split('\n').count(),
    ))
}

pub fn run(args: &[String]) -> i32 {
    run_at(&repo_root(), args.iter().any(|a| a == "--json"))
}

/// The gate, against an explicit root — so its own refusals can be watched
/// failing on a scratch tree rather than asserted about (ARCH §18.1).
fn run_at(root: &std::path::Path, json: bool) -> i32 {

    // ── Reach the evidence, or say you could not (exit 3) ──────────────────
    let spec = match std::fs::read_to_string(root.join(SPEC)) {
        Ok(s) => s,
        Err(e) => {
            // Named, not defaulted (ARCH §18.3). The registry on disk cannot be
            // checked against a specification this machine does not have, and
            // reporting that as a pass would make the byte gate a local-only
            // check that CI silently agrees with.
            eprintln!(
                "conformance-gate: COULD-NOT-JUDGE — cannot read {SPEC}: {e}\n  \
                 The registry is present but its spec_hash cannot be verified against anything. \
                 If this is a fresh clone or CI, the specification is not tracked: check \
                 `git check-ignore -v {SPEC}`."
            );
            return 3;
        }
    };
    let registry_text = match std::fs::read_to_string(root.join(REGISTRY)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "conformance-gate: COULD-NOT-JUDGE — cannot read {REGISTRY}: {e}\n  \
                 Generate it:\n    {REGENERATE}"
            );
            return 3;
        }
    };
    let registry: Registry = match toml::from_str(&registry_text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "conformance-gate: COULD-NOT-JUDGE — {REGISTRY} does not parse as a Registry: {e}\n  \
                 It is machine-written; do not edit it by hand. Regenerate:\n    {REGENERATE}"
            );
            return 3;
        }
    };

    // ── X-EH-3: a zero-work run does not report success (exit 4) ───────────
    // Checked BEFORE the remaining evidence is read: an empty scope is not a
    // pass, and it is not a could-not-judge either.
    let in_scope: Vec<_> = registry.in_scope().collect();
    if in_scope.is_empty() {
        eprintln!(
            "conformance-gate: NEVER-RAN — {REGISTRY} carries no in-scope requirements. \
             An empty registry cannot judge anything, and reporting that as a pass is the \
             failure this gate exists to name (X-EH-3)."
        );
        return 4;
    }

    let enforceability: BTreeMap<String, Enforceability> =
        match std::fs::read_to_string(root.join(ENFORCEABILITY))
            .map_err(|e| e.to_string())
            .and_then(|t| toml::from_str(&t).map_err(|e| e.to_string()))
        {
            Ok(m) => m,
            Err(e) => {
                eprintln!("conformance-gate: COULD-NOT-JUDGE — cannot read {ENFORCEABILITY}: {e}");
                return 3;
            }
        };

    let mut failures: Vec<String> = Vec::new();

    // ── 1. The registry still describes the specification ──────────────────
    if let Some(drift) = spec_drift(&spec, &registry) {
        failures.push(drift);
    }

    // ── 2. The denominator has not moved ───────────────────────────────────
    let must = registry.must_class().count();
    let should = in_scope.len() - must;
    for (what, got, want) in [
        ("in-scope requirements", in_scope.len(), IN_SCOPE),
        ("must-class", must, MUST_CLASS),
        ("should", should, SHOULD),
    ] {
        if got != want {
            failures.push(format!(
                "{what}: {got}, pinned at {want}. Either the specification gained or lost \
                 requirements — in which case regenerate and move the pin in the same commit — or \
                 the parser has started losing a declaration form, which is how an earlier count \
                 of this spec came out 112 short."
            ));
        }
    }

    // ── 3. Every requirement has exactly one enforceability, and vice versa ─
    let ids: std::collections::BTreeSet<&str> = in_scope.iter().map(|r| r.id.as_str()).collect();
    let classified: std::collections::BTreeSet<&str> =
        enforceability.keys().map(String::as_str).collect();
    let unclassified: Vec<&&str> = ids.difference(&classified).collect();
    let orphaned: Vec<&&str> = classified.difference(&ids).collect();
    if !unclassified.is_empty() {
        failures.push(format!(
            "{} requirement(s) have no enforceability class: {}{}\n      \
             Add each to {ENFORCEABILITY}. A requirement with no class cannot be scheduled into a \
             tier, so it would silently never be checked.",
            unclassified.len(),
            unclassified
                .iter()
                .take(12)
                .map(|s| **s)
                .collect::<Vec<_>>()
                .join(", "),
            if unclassified.len() > 12 { ", …" } else { "" }
        ));
    }
    if !orphaned.is_empty() {
        failures.push(format!(
            "{} enforceability entr(ies) name no requirement: {}{}\n      \
             A class that outlived its requirement is a claim about nothing. Remove it from \
             {ENFORCEABILITY}.",
            orphaned.len(),
            orphaned
                .iter()
                .take(12)
                .map(|s| **s)
                .collect::<Vec<_>>()
                .join(", "),
            if orphaned.len() > 12 { ", …" } else { "" }
        ));
    }

    // ── 4. No pointer dangles ──────────────────────────────────────────────
    for r in &registry.requirements {
        if let Some(dep) = &r.conditional_on {
            if registry.get(dep).is_none() {
                failures.push(format!(
                    "{} is conditional on {dep}, which is not a requirement",
                    r.id
                ));
            }
        }
        if let Some(target) = &r.alias_of {
            if registry.get(target).is_none() {
                failures.push(format!("{} aliases {target}, which is not a requirement", r.id));
            }
        }
    }
    for s in &registry.scenarios {
        for cited in &s.cites {
            if registry.resolve(cited).is_none() {
                failures.push(format!(
                    "acceptance scenario {} cites {cited}, which is not a requirement",
                    s.id
                ));
            }
        }
    }

    // ── Report ─────────────────────────────────────────────────────────────
    let model_free = in_scope
        .iter()
        .filter_map(|r| enforceability.get(&r.id))
        .filter(|e| e.is_model_free())
        .count();
    if json {
        println!(
            "{{\"gate\":\"conformance-gate\",\"verdict\":\"{}\",\"in_scope\":{},\"must_class\":{},\
             \"should\":{},\"scenarios\":{},\"model_free\":{},\"spec_hash\":\"{}\",\"failures\":{}}}",
            if failures.is_empty() { "passed" } else { "failed" },
            in_scope.len(),
            must,
            should,
            registry.scenarios.len(),
            model_free,
            &registry.spec_hash[..16],
            failures.len(),
        );
    } else {
        eprintln!("conformance-gate: {SPEC} @ {}", &registry.spec_hash[..12]);
        eprintln!(
            "  {} requirements in scope — {must} must-class, {should} should; \
             {} acceptance scenarios (§16)",
            in_scope.len(),
            registry.scenarios.len(),
        );
        eprintln!(
            "  {model_free} of {} reachable with no model at all (cli + desktop + structural)",
            in_scope.len()
        );
        for (family, n) in registry.family_counts() {
            eprint!("  {family} {n}");
        }
        eprintln!();
    }

    if failures.is_empty() {
        eprintln!("  ✓ registry describes the specification, and every column resolves");
        0
    } else {
        eprintln!("  ✗ {} problem(s):", failures.len());
        for f in &failures {
            eprintln!("    - {f}");
        }
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The gate reads the LIVE repo, so this is the gate watched PASSING on the
    /// real artifacts — not a fixture that proves only that the code compiles.
    #[test]
    fn the_committed_registry_passes_its_own_gate() {
        assert_eq!(run_at(&repo_root(), false), 0);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "conformance-gate-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(dir.join("research/clean-room")).expect("scratch spec dir");
        std::fs::create_dir_all(dir.join("quality")).expect("scratch quality dir");
        dir
    }

    fn write_spec(dir: &std::path::Path, body: &str) -> String {
        std::fs::write(dir.join(SPEC), body).expect("write spec");
        ContentHash::of_str(body).to_hex()
    }

    /// Evidence it could not reach is COULD-NOT-JUDGE, never FAILED — the two
    /// mean different things to whoever reads the summary, and collapsing them
    /// is the ARCH §18.2 defect this whole campaign is about.
    #[test]
    fn an_unreachable_registry_is_could_not_judge_not_failed() {
        let dir = scratch("no-registry");
        write_spec(&dir, "# spec\n");
        assert_eq!(run_at(&dir, false), 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Machine-written and hand-edited into nonsense: still not a FAIL, because
    /// the gate has judged nothing.
    #[test]
    fn an_unparseable_registry_is_could_not_judge() {
        let dir = scratch("bad-registry");
        write_spec(&dir, "# spec\n");
        std::fs::write(dir.join(REGISTRY), "this is not toml = = =").expect("write");
        assert_eq!(run_at(&dir, false), 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// X-EH-3, applied to this gate itself: a run that examined nothing exits
    /// NEVER-RAN, not 0. Watched failing here rather than asserted in prose.
    #[test]
    fn an_empty_registry_is_never_ran_not_passed() {
        let dir = scratch("empty-registry");
        let hash = write_spec(&dir, "# spec\n");
        std::fs::write(
            dir.join(REGISTRY),
            format!("spec_hash = \"{hash}\"\nspec_lines = 2\nrequirements = []\nscenarios = []\n"),
        )
        .expect("write");
        assert_eq!(run_at(&dir, false), 4);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The instrument check that gates the rest, tested on its own: a spec
    /// edited without regenerating must be reported as drift, and the message
    /// must carry the repair command. Watched failing (ARCH §18.1).
    #[test]
    fn a_spec_that_moved_without_the_registry_is_reported_as_drift() {
        let body = "# spec\n";
        let registry = Registry {
            spec_hash: ContentHash::of_str(body).to_hex(),
            spec_lines: 2,
            requirements: vec![],
            scenarios: vec![],
        };
        assert!(
            spec_drift(body, &registry).is_none(),
            "an unchanged spec is not drift"
        );
        let drifted = spec_drift("# spec, edited\n", &registry)
            .expect("an edited spec must be reported as drift");
        assert!(drifted.contains("UPDATE_REQUIREMENTS=1"), "no repair command: {drifted}");
        assert!(drifted.contains("OLDER document"), "does not say what is stale: {drifted}");
    }
}
