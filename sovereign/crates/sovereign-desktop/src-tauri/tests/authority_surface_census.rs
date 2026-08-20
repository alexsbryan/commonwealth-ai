// SPDX-License-Identifier: AGPL-3.0-or-later
//! Authority-surface census for `sovereign-desktop`.
//!
//! A surface that can INSTALL a corpus must also register the tool that
//! DECLARES authority over it. Without it `ToolRegistry::authority_domains()`
//! is empty, `authority_guard::armed_for_evidence` never arms, and the turn
//! falls through to KnowledgeQuery streaming — a figure can be synthesized
//! with no accession, no period basis, and no check that the store covers the
//! period claimed. That is a FABRICATION surface, not a reduced one, which is
//! why the registration is unconditional rather than gated on
//! `config.enabled_tools`.
//!
//! Measured, order `sec-filings-close`: the desktop registered the acquirer
//! and not the tool, and e2e run 4 answered an FY2025 capex question with
//! $10,957M — absent from both the derived store and SEC's own
//! companyfacts.json — under an `FY2026` heading, against a store holding
//! $12,715M. Every gate stayed green because the gates run in binaries that
//! DO register it. `SYSTEM_OVERVIEW.md` (authority guard) carries the general
//! shape: arming reads the registry of the process that serves the turn.
//!
//! This is a SOURCE census, and it lives outside `state.rs` rather than in
//! it: both registrations sit in one ~1900-line `async fn
//! bootstrap_with_progress` that needs a full `AppState`, an inference
//! provider and a model on disk to run, so there is no seam to build the
//! registry alone.

const STATE_RS: &str = include_str!("../src/state.rs");

#[test]
fn registering_the_sec_acquirer_obliges_registering_the_sec_facts_tool() {
    // Needles assembled at runtime so this test cannot satisfy itself by
    // matching the literals in its own body.
    let acquirer = format!("sec_edgar::{}", "register(&engine_builder)");
    let tool = format!("sec_facts::{}", "SecFactsTool::new(");

    assert!(
        STATE_RS.contains(&acquirer),
        "expected the sec_edgar acquirer registration in state.rs — if it \
         moved, move this census with it rather than deleting it"
    );
    assert!(
        STATE_RS.contains(&tool),
        "state.rs registers the sec_edgar acquirer ({acquirer}) but NOT \
         {tool}. Installing an SEC corpus by ticker will work and every \
         figure answered from it will be ungrounded: authority_domains() \
         stays empty, the authority guard logs `not armed — no evidence \
         corpus declares authority`, and the answer path cannot be made to \
         cite an accession or refuse an uncovered period."
    );
}
