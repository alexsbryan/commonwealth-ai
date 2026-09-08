// SPDX-License-Identifier: AGPL-3.0-or-later
//! `quality/instruments.toml` itself, gated as data.
//!
//! `cargo xtask instrument-gate` answers "does every command a quality surface
//! reaches have a row?". This answers the two questions the gate cannot,
//! because both are about the CONTENT of a row rather than about coverage:
//!
//!  1. Does the committed registry parse at all? A registry that stops parsing
//!     turns every other consumer — the gate, `svrn quality map`, posture's
//!     coverage line — into a could-not-judge at once, and a plain `cargo test`
//!     run should say so rather than a push finding out.
//!  2. Does every `negative_control` resolve to something real? The field's
//!     whole value is the count posture prints, and a typo'd mutant id would
//!     inflate that count silently — a green nobody earned (ARCH §18.1).
//!
//! In `xtask/tests/` and not in `kernel-types` for the reason its two
//! neighbours are: it reads the REPO ROOT, and `kernel-types` is a global
//! `[[package_leaf]]` that must build standalone with its tests.

use std::collections::BTreeSet;

use kernel_types::quality::{render_map, Cost, Registry, RunsIn};

#[path = "shared/repo_root.rs"]
mod repo_root;

fn registry() -> Registry {
    let root = repo_root::repo_root();
    let path = root.join("quality/instruments.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    match Registry::parse(&text) {
        Ok(r) => r,
        Err(errs) => panic!(
            "quality/instruments.toml does not parse:\n  {}",
            errs.join("\n  ")
        ),
    }
}

#[test]
fn the_committed_registry_parses() {
    let r = registry();
    assert!(
        r.instruments.len() >= 60,
        "the registry lost rows — {} left, and it was minted with 69",
        r.instruments.len()
    );
    assert!(!r.censused_surfaces.is_empty());
}

/// A `negative_control` that names nothing is worse than `none`: `none` is
/// counted as absent, a typo is counted as PRESENT.
#[test]
fn every_negative_control_resolves_to_a_mutant_or_a_control_instrument() {
    let root = repo_root::repo_root();
    let r = registry();

    let mut mutants: BTreeSet<String> = BTreeSet::new();
    let dir = root.join("quality/sabotage");
    for entry in std::fs::read_dir(&dir)
        .expect("quality/sabotage must exist")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("id = \"") {
                if let Some((id, _)) = rest.split_once('"') {
                    mutants.insert(id.to_string());
                }
            }
        }
    }
    assert!(
        mutants.len() > 100,
        "read only {} mutant ids from {} — the reader, not the registry, is broken",
        mutants.len(),
        dir.display()
    );

    let controls: BTreeSet<&str> = r
        .instruments
        .iter()
        .filter(|i| i.kind == kernel_types::quality::Kind::Control)
        .map(|i| i.id.as_str())
        .collect();

    let mut dangling = Vec::new();
    for i in &r.instruments {
        if let Some(nc) = &i.negative_control {
            if !mutants.contains(nc) && !controls.contains(nc.as_str()) {
                dangling.push(format!("{}: negative_control `{nc}`", i.id));
            }
        }
    }
    assert!(
        dangling.is_empty(),
        "negative controls naming nothing:\n  {}",
        dangling.join("\n  ")
    );
}

/// Every `doc` pointer names a path or a section a reader can open. Only the
/// path half is checkable here; a pointer whose file half does not exist is
/// the `wizard-verify.sh` failure one level up — a map entry pointing at
/// nothing.
#[test]
fn every_doc_pointer_that_names_a_file_names_one_that_exists() {
    let root = repo_root::repo_root();
    let mut missing = Vec::new();
    for i in registry().instruments {
        let head = i.doc.split([' ', '#']).next().unwrap_or("");
        if !head.contains('/') && !head.ends_with(".md") {
            continue;
        }
        if !root.join(head).exists() {
            missing.push(format!("{}: doc `{head}`", i.id));
        }
    }
    assert!(
        missing.is_empty(),
        "doc pointers to nothing:\n  {}",
        missing.join("\n  ")
    );
}

/// The closure trigger has to keep pointing at something. If every instrument
/// were measured, controlled and scheduled, these would be zero and the line
/// posture prints would be noise — but the registry was minted with 50
/// unmeasured and 9 running nowhere, so a zero here means the fields stopped
/// being filled in honestly, not that the work got done.
#[test]
fn the_registry_still_reports_the_gaps_it_was_minted_to_show() {
    let r = registry();
    let c = r.coverage();
    assert_eq!(c.total, r.instruments.len());
    assert!(
        c.with_negative_control < c.total,
        "every instrument claims a negative control — check that, it is a strong claim"
    );
    assert!(
        r.instruments.iter().any(|i| i.cost == Cost::Unmeasured),
        "no instrument is unmeasured — either every cost was timed, or the field is being \
         filled in with numbers nobody measured"
    );
    assert!(
        !r.nowhere().is_empty(),
        "nothing runs nowhere — the three off-map soaks were the reason this registry exists"
    );
}

/// A venue name a human can act on. `ci:` with no job, `smoke:` with no phase
/// — the parser refuses those; this pins that the committed file uses named
/// venues rather than leaning on `by-hand` for everything.
#[test]
fn the_venues_are_named_not_just_by_hand() {
    let r = registry();
    let named = r
        .instruments
        .iter()
        .flat_map(|i| &i.runs_in)
        .filter(|v| !matches!(v, RunsIn::ByHand))
        .count();
    assert!(
        named > 40,
        "only {named} scheduled venues across the registry"
    );
}

/// The golden render — the thing that keeps `QUALITY_SURFACE.md`'s pointer
/// honest. The doc no longer carries the four tables; it points at `svrn
/// quality map`, and this is what stops that pointer aiming at a render that
/// silently stopped matching the registry.
///
/// ```text
/// svrn quality map --update-golden      # or: UPDATE_QUALITY_MAP=1 cargo test -p xtask --test instrument_registry
/// ```
#[test]
fn the_golden_render_matches_the_registry() {
    let root = repo_root::repo_root();
    let path = root.join("quality/quality-map.golden.md");
    let rendered = render_map(&registry());

    if std::env::var_os("UPDATE_QUALITY_MAP").is_some() {
        std::fs::write(&path, &rendered).expect("write golden");
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    if committed == rendered {
        return;
    }
    // Name the FIRST differing line rather than dumping two 300-line files:
    // a diff nobody reads is a failure message nobody acts on.
    let (a, b) = (
        committed.lines().collect::<Vec<_>>(),
        rendered.lines().collect::<Vec<_>>(),
    );
    let at = a.iter().zip(b.iter()).position(|(x, y)| x != y);
    let detail = match at {
        Some(n) => format!(
            "first difference at line {}:\n  committed: {}\n  rendered:  {}",
            n + 1,
            a[n],
            b[n]
        ),
        None => format!("committed is {} lines, rendered is {}", a.len(), b.len()),
    };
    panic!(
        "quality/quality-map.golden.md is stale — the registry changed and the render did not \
         follow.\n{detail}\nRegenerate: svrn quality map --update-golden"
    );
}
