// SPDX-License-Identifier: AGPL-3.0-or-later
//! Authority-surface census for `sovereign-desktop`.
//!
//! A surface that can INSTALL a corpus must also register the tool that
//! DECLARES authority over it. Without it `ToolRegistry::authority_domains()`
//! is empty, `authority_guard::armed_for_evidence` never arms, and the turn
//! falls through to KnowledgeQuery streaming — a figure can be synthesized
//! with no accession, no period basis, and no check that the store covers the
//! period claimed. That is a FABRICATION surface, not a reduced one.
//!
//! Measured, order `sec-filings-close`: the desktop registered the acquirer
//! and not the tool, and e2e run 4 answered an FY2025 capex question with
//! $10,957M — absent from both the derived store and SEC's own
//! companyfacts.json — under an `FY2026` heading, against a store holding
//! $12,715M. Every gate stayed green because the gates run in binaries that
//! DO register it. `SYSTEM_OVERVIEW.md` (authority guard) carries the general
//! shape: arming reads the registry of the process that serves the turn.
//!
//! # The census now follows the composition, and that is a rewrite
//!
//! Until 2026-08-26 this scanned `state.rs` for the literal
//! `sec_facts::SecFactsTool::new(`, because that is where the registration
//! was. The desktop then adopted the shared recipe (TOPOLOGY §10 phase 7) and
//! stopped naming any tool: it composes `baseline_bundles`, which composes
//! `CoreTurnTools`, which registers `sec_facts`. The tool is STILL registered
//! — the invariant never broke — but the instrument could no longer see it and
//! failed, which is the right failure for a census whose subject moved (ARCH
//! §18.4: validate the instrument before the result). Each hop is asserted
//! separately so a break names WHICH link went, rather than reporting the
//! whole chain as absent.
//!
//! # What got STRONGER, and is asserted here for the first time
//!
//! The old registration was unconditional because its line sat outside the
//! `enabled_tools.iter().any(..)` match — a property of where someone put it.
//! Under the family gate it is unconditional because `SecFactsTool` declares
//! no `ToolFamily`, and `ToolRegistry` only withholds tools that declare one.
//! A future edit that gave it a family would make an authority declaration
//! switchable from a settings panel, which is the fabrication surface again by
//! another door. That is hop 5.
//!
//! This is a SOURCE census: the registrations sit inside a
//! `bootstrap_with_progress` that needs a full `AppState`, an inference
//! provider and a model on disk to run, so there is no seam to build the
//! registry alone.

const STATE_RS: &str = include_str!("../src/state.rs");
const RECIPE_RS: &str = include_str!("../../../sovereign-runtime-recipe/src/lib.rs");
const BUNDLES_RS: &str = include_str!("../../../sovereign-tools/src/bundles.rs");
const SEC_FACTS_RS: &str = include_str!("../../../sovereign-tools/src/sec_facts.rs");

/// The body of `impl ToolBundle for <name>`, so a needle found in a doc
/// comment or a neighbouring family cannot satisfy a hop.
fn bundle_body<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    let start = src.find(&format!("ToolBundle for {name} {{"))?;
    let rest = &src[start..];
    let end = rest.find("\n}\n").map(|i| start + i).unwrap_or(src.len());
    Some(&src[start..end])
}

#[test]
fn registering_the_sec_acquirer_obliges_registering_the_sec_facts_tool() {
    // Needles assembled at runtime so this test cannot satisfy itself by
    // matching the literals in its own body.
    let acquirer = format!("sec_edgar::{}", "register(&engine_builder)");
    let tool = format!("sec_facts::{}", "SecFactsTool::new(");
    let baseline = format!("baseline_{}", "bundles(");
    let core = format!("CoreTurn{}", "Tools::new(");

    // Hop 1 — the desktop can install an SEC corpus by ticker.
    assert!(
        STATE_RS.contains(&acquirer),
        "expected the sec_edgar acquirer registration in state.rs — if it \
         moved, move this census with it rather than deleting it"
    );

    // Hop 2 — and it composes the shared baseline.
    assert!(
        STATE_RS.contains(&baseline),
        "state.rs registers the sec_edgar acquirer ({acquirer}) but no longer \
         composes {baseline}. If the desktop stopped using the shared recipe, \
         it must name {tool} itself, and this census must be rewritten to the \
         path it actually takes."
    );

    // Hop 3 — the baseline carries the core turn family.
    assert!(
        RECIPE_RS.contains(&core),
        "baseline_bundles no longer composes {core}, so the desktop's \
         authority declaration has no route. Every figure answered from an \
         installed SEC corpus would be ungrounded: authority_domains() stays \
         empty and the authority guard logs `not armed — no evidence corpus \
         declares authority`."
    );

    // Hop 4 — and that family registers the tool.
    let core_body = bundle_body(BUNDLES_RS, "CoreTurnTools")
        .expect("CoreTurnTools has a ToolBundle impl in bundles.rs");
    assert!(
        core_body.contains(&tool),
        "CoreTurnTools no longer registers {tool}. The desktop can still \
         install an SEC corpus by ticker and every figure answered from it \
         will be ungrounded — it cannot cite an accession and cannot refuse \
         an uncovered period."
    );

    // Hop 5 — and no user switch can withhold it.
    assert!(
        !SEC_FACTS_RS.contains("with_family("),
        "SecFactsTool now declares a ToolFamily, which makes the tool that \
         DECLARES authority over an installed SEC corpus switchable from the \
         settings panel. A user who turns that family off keeps the ability \
         to install the corpus and loses the ability to ground it — the \
         fabrication surface this census exists to prevent, arrived by \
         another door."
    );
}
