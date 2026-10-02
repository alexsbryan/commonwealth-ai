// SPDX-License-Identifier: AGPL-3.0-or-later
//! The print-only censuses of `cli_contract_journeys.rs`: how much of the
//! contract each layer claims, printed, never asserted.

use super::*;

#[test]
fn print_the_scenario_census() {
    // Not an assertion — how much of §16 any journey claims to run. Expected to
    // read 0 for a long time, and that is the point of printing it: the 19
    // scenarios were parsed into the registry and referenced by nothing for as
    // long as nobody rendered this line.
    let c = contract();
    let reg = registry();
    let claimed: BTreeSet<&str> = c
        .scenario_claims()
        .iter()
        .map(|claim| claim.scenario)
        .collect();
    eprintln!(
        "acceptance scenarios: {} of {} demonstrated by a journey",
        claimed.len(),
        reg.scenarios.len()
    );
}

#[test]
fn print_the_requirement_claim_census() {
    // Not an assertion — the answer to "how much of the specification does the
    // journey layer actually reach?", printed every run so the ratio is
    // visible rather than reconstructed. The denominator is the `cli` class,
    // not 625: the other 314 are not this instrument's to prove.
    use kernel_types::conformance::Enforceability;
    let c = contract();
    let classes = enforceability();
    let claims = c.requirement_claims();
    let claimed: BTreeSet<&str> = claims.iter().map(|claim| claim.requirement).collect();
    let cli_class = classes
        .values()
        .filter(|e| **e == Enforceability::Cli)
        .count();
    eprintln!(
        "journey requirement claims: {} claim(s) over {} distinct requirement(s); \
         {cli_class} are classified `cli` and reachable by this instrument",
        claims.len(),
        claimed.len()
    );
}

#[test]
fn print_the_assertion_census() {
    // Not an assertion — the answer to "how much of this manifest can actually
    // fail?", printed on every run so the ratio is visible rather than
    // reconstructed. The same census the `svrn contract` verb renders.
    let c = contract();
    eprintln!(
        "{}",
        sovereign_cli_shared::cli_contract_report::render_census(&c)
    );
}

// ── glassbox summary ────────────────────────────────────────────────────

#[test]
fn print_the_experience_map() {
    // Not an assertion — the answer to "what does this product PROMISE, and
    // how much of each promise is actually proven?" in one place.
    //
    // Rendered by `cli_contract_report`, which is also what `svrn contract`
    // prints. ONE renderer on purpose: this map used to live here as a wall of
    // `eprintln!`, reachable only by knowing the exact `cargo test … --nocapture`
    // incantation, and a second copy would have started drifting from the
    // numbers the ratchets above enforce the moment either was edited.
    let c = contract();
    eprintln!(
        "\n{}",
        sovereign_cli_shared::cli_contract_report::render_experience_map(&c)
    );
}

#[test]
fn print_the_journey_map() {
    // Not an assertion — a rendered map, so `cargo test -- --nocapture`
    // answers "what does this CLI actually promise?" in one place.
    let c = contract();
    let mut by_tier: BTreeMap<u8, Vec<&Journey>> = BTreeMap::new();
    for j in &c.journeys {
        by_tier.entry(j.tier).or_default().push(j);
    }
    eprintln!("\n── CLI journey map ──");
    for (tier, js) in &by_tier {
        eprintln!("tier {tier}:");
        for j in js {
            let live = if j.skip_live.is_some() {
                "     "
            } else {
                "LIVE "
            };
            eprintln!("  {live}{:<24} {} steps  {}", j.id, j.steps.len(), j.title);
        }
    }
    eprintln!(
        "\n{} journeys, {} steps, {} live-eligible, {} stranded verbs",
        c.journeys.len(),
        c.journeys.iter().map(|j| j.steps.len()).sum::<usize>(),
        c.live_journeys().len(),
        c.stranded.len()
    );
}
