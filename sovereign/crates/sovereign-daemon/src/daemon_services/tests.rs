use super::*;

use sovereign_contracts::launch::Launch;

fn headless_extras() -> HeadlessExtras {
    HeadlessExtras {
        rails: HeadlessRails {
            provider_factory: std::sync::Arc::new(fixtures::NullFactory),
            mesh_store: Arc::new(crate::rails_client::kv::RailsKv::new(
                crate::rails_client::DEFAULT_RAILS_BASE,
            )),
            convergence_recorder: Arc::new(sovereign_mesh::peer_adapter::MeshConvergence::new()),
        },
        knowledge_view_http: axum::Router::new(),
    }
}

/// **Soundness, behaviourally.** Every variant is produced by some launch —
/// run, not grepped. The census in `tests/daemon_variant_census.rs` can
/// only see that an ARM EXISTS; this sees what the arm returns, which is
/// the half that catches an arm wired to the wrong variant.
#[test]
fn each_assembling_launch_produces_its_variant() {
    let cases: Vec<(Launch, LaunchParts, &str)> = vec![
        (
            Launch::Daemon {
                args: vec!["run".into()],
            },
            LaunchParts::Serving {
                serving: fixtures::serving(),
                headless: Some(headless_extras()),
            },
            "headless",
        ),
        (
            Launch::Desktop,
            LaunchParts::Serving {
                serving: fixtures::serving(),
                headless: None,
            },
            "desktop",
        ),
        (
            Launch::Verb {
                name: "mesh".into(),
                args: vec!["create".into()],
            },
            LaunchParts::Admin {
                mesh: crate::hosted_mesh::MeshAccess::absent(),
            },
            "mesh-admin",
        ),
    ];
    let mut produced = std::collections::HashSet::new();
    for (launch, parts, expected) in cases {
        let got = assemble(&launch, parts)
            .unwrap_or_else(|e| panic!("{} should assemble: {e}", launch.as_str()));
        assert_eq!(got.label(), expected, "launch {}", launch.as_str());
        produced.insert(got.label());
    }
    // Every declared shape came out of some launch. A variant nobody can
    // produce is the representable-but-dead configuration TOPOLOGY §4
    // calls unsound.
    assert_eq!(produced.len(), fixtures::every_variant().len());
}

/// The launches that assemble NOTHING say so rather than being given a
/// plausible daemon.
#[test]
fn a_launch_that_assembles_nothing_refuses() {
    for launch in [
        Launch::Bare,
        Launch::ComputeChild { args: Vec::new() },
        Launch::Smoketest { argv: Vec::new() },
        Launch::Worker {
            args: vec!["run".into(), "--worker-mode".into()],
        },
    ] {
        let err = assemble(
            &launch,
            LaunchParts::Admin {
                mesh: crate::hosted_mesh::MeshAccess::absent(),
            },
        )
        .err()
        .unwrap_or_else(|| panic!("{} must not assemble a daemon", launch.as_str()));
        assert!(
            matches!(err, AssemblyRefusal::NotAnAssembler { .. }),
            "{} refused with the wrong reason: {err}",
            launch.as_str()
        );
    }
}

/// The mismatches, which are the states the assembler exists to make
/// unrepresentable-in-practice: a desktop launch carrying headless rails,
/// a daemon launch with none, and a verb launch carrying a serving
/// profile. Each refuses and NAMES both sides (§18.3) rather than
/// substituting the nearest plausible variant — a daemon that came up as
/// the wrong shape is the hazard itself.
#[test]
fn every_illegal_pairing_refuses_and_names_both_sides() {
    let cases: Vec<(Launch, LaunchParts)> = vec![
        (
            Launch::Desktop,
            LaunchParts::Serving {
                serving: fixtures::serving(),
                headless: Some(headless_extras()),
            },
        ),
        (
            Launch::Daemon {
                args: vec!["run".into()],
            },
            LaunchParts::Serving {
                serving: fixtures::serving(),
                headless: None,
            },
        ),
        (
            Launch::Verb {
                name: "mesh".into(),
                args: Vec::new(),
            },
            LaunchParts::Serving {
                serving: fixtures::serving(),
                headless: None,
            },
        ),
        (
            Launch::Desktop,
            LaunchParts::Admin {
                mesh: crate::hosted_mesh::MeshAccess::absent(),
            },
        ),
    ];
    for (launch, parts) in cases {
        let name = launch.as_str();
        let err = assemble(&launch, parts)
            .err()
            .unwrap_or_else(|| panic!("{name} + mismatched parts must refuse"));
        assert!(
            matches!(err, AssemblyRefusal::Mismatch { .. }),
            "{name} refused with the wrong reason: {err}"
        );
        let text = err.to_string();
        assert!(
            text.contains(name),
            "a refusal must name the launch it refused; got: {text}"
        );
        assert!(
            text.contains("but was handed"),
            "a refusal must name what it was handed, not only what it wanted; got: {text}"
        );
    }
}

/// The differential falsifier of `TOPOLOGY.md §4`, soundness half: every
/// variant carries exactly the capability set measured on its live path,
/// no more and no less. Written as a table so a reader checks it against
/// the matrix in the module docs without running anything — and driven off
/// `every_variant()`, so adding a fourth variant fails here until someone
/// states what it carries.
#[test]
fn each_variant_declares_exactly_its_measured_capability_set() {
    // label, core, rails, host routers
    //
    // The `state_store` column was DELETED here by Phase 3, and its
    // deletion is the proof the phase landed. It read `. Y .` — the one
    // column that was not a subset relation down the rows, which is what
    // "the two serving shapes cross rather than nest" meant concretely.
    // The store now sits in `ServingCore`, so the column is the `core`
    // column and asserting it separately would be a check that cannot
    // disagree with its neighbour (the defect retired above).
    let expected: &[(&str, bool, bool, usize)] = &[
        ("mesh-admin", false, false, 0),
        ("desktop", true, false, 3),
        // Four: `solve_http` left with the solver for code (pb-meshapp-solve).
        ("headless", true, true, 4),
    ];
    let variants = fixtures::every_variant();
    assert_eq!(
        variants.len(),
        expected.len(),
        "a variant was added without a row in the measured table"
    );

    for (s, (label, core, rails, routers)) in variants.iter().zip(expected) {
        assert_eq!(s.label(), *label);
        // ONE question, not three. Until 2026-08-24 this asserted
        // `corpus_engine().is_some()`, `inference_provider().is_some()` and
        // `serving().is_some()` separately against the SAME `core` column —
        // three checks that could not disagree, because all three read one
        // variant through one-line `.map()` wrappers. Deleting the wrappers
        // is what made that visible.
        assert_eq!(s.serving().is_some(), *core, "{label}: serving profile");
        // Likewise ONE question, not four: `provider_factory`, `mesh_store`
        // and `convergence_recorder` all read `rails`.
        assert_eq!(s.rails().is_some(), *rails, "{label}: rails");
        assert_eq!(s.host_routers().len(), *routers, "{label}: host routers");
        assert_eq!(
            s.host_router_names().len(),
            *routers,
            "{label}: router names must match router count"
        );
        assert_eq!(
            s.serves_host_surface(),
            *core,
            "{label}: a variant serves a host surface iff it has a core"
        );
    }
}

// RETIRED 2026-08-24 — `a_serving_variant_never_has_half_a_core`.
//
// It asserted `corpus_engine().is_some() == inference_provider().is_some()`
// — "core is one ring, not two independent slots". Both accessors are now
// deleted, and the only way to reach either field is through
// `serving()`, which yields a `&ServingProfile` whose `core` holds both as
// plain `Arc`s. There is no longer an input that could make this test fail:
// half a core is not writable. A check with no nameable failing input is
// not a gate (§18.1), so it is deleted rather than left to read as
// assurance. The property it guarded is now carried by the type.

#[test]
fn named_absence_is_not_a_bare_option() {
    let m = McpSurface::Unavailable {
        reason: "notes.db locked".into(),
    };
    assert!(m.mount().is_none());
    let e = EmbedAdvertisement::Unavailable {
        reason: "no embed model configured".into(),
    };
    assert!(e.info().is_none());
}
