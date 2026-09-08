// SPDX-License-Identifier: AGPL-3.0-or-later
//! SELECTION — which instruments a venue runs, and in what order.
//!
//! Selection is not judgement. An instrument absent from a venue's list did
//! not pass; it was never asked, and every caller here prints what it left
//! out by name (ARCH §18.3).

use std::path::Path;

use kernel_types::quality::{BaselineKind, Instrument};

use super::fingerprint::Fingerprint;

/// Which lanes have a baseline captured against THIS stack.
pub(super) fn comparable_baselines(repo: &Path, lanes: &[&Instrument], fp: &Fingerprint) -> usize {
    lanes
        .iter()
        .filter(|l| {
            baseline_dir(l).is_some_and(|d| repo.join(d).join(&fp.hex).join("latest.json").exists())
        })
        .count()
}

/// The per-fingerprint baseline directory, when the instrument declares one.
///
/// `check-lanes.toml`'s `baseline_dir` merged into `baseline` as a fourth
/// kind rather than surviving as a second field: one question, one field
/// (ARCH §10.6). A `count` or `metrics` baseline is a different currency and
/// is NOT a fingerprint directory, which is why this matches on the kind
/// rather than reading `path` wherever it finds one.
pub(super) fn baseline_dir(i: &Instrument) -> Option<&str> {
    match i.baseline.kind {
        kernel_types::quality::BaselineKind::Fingerprint => i.baseline.path.as_deref(),
        _ => None,
    }
}

/// The changed paths a venue supplied, from `SOVEREIGN_CHANGED_PATHS`
/// (colon-separated — the spelling `scripts/sovereign-lint.sh` already reads).
///
/// `None` and "an empty set" are different answers and the difference gates a
/// whole selection: unset means the venue offered no change set and every
/// instrument is selected, while an empty value means it offered one and it
/// was empty. Collapsing them would silently run nothing (ARCH §18.3).
pub(super) fn changed_paths() -> Option<Vec<String>> {
    // New prefix preferred, legacy fallback — the repo's own convention.
    // `sovereign_contracts::rebrand::promote_legacy_env` MIRRORS the two
    // prefixes (it never unsets), so a venue that exports the legacy spelling
    // still reaches the child compile gate, which reads it by that name.
    let raw = std::env::var("SVRNMESH_CHANGED_PATHS")
        .or_else(|_| std::env::var("SOVEREIGN_CHANGED_PATHS"))
        .ok()?;
    Some(
        raw.split(':')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect(),
    )
}

/// Whether an instrument's `when_changed` matches anything the venue changed.
///
/// A malformed pattern SELECTS the instrument rather than dropping it. The
/// safe direction is running something unnecessary; the unsafe one is a gate
/// silently vanishing because a regex had a typo.
pub(super) fn selected_by_change(inst: &Instrument, changed: Option<&Vec<String>>) -> bool {
    let (Some(pat), Some(changed)) = (inst.when_changed.as_deref(), changed) else {
        return true;
    };
    match regex::Regex::new(pat) {
        Ok(re) => changed.iter().any(|p| re.is_match(p)),
        Err(e) => {
            eprintln!("warning: {}: when_changed `{pat}` will not compile ({e}) — selecting it rather than dropping it", inst.id);
            true
        }
    }
}

/// The order instruments start in.
///
/// ONE SLOT: the order the author declared. That is not a default, it is the
/// answer — with a single slot the sequence decides which instruments get
/// squeezed when the budget runs out, and `quality/instruments.toml` lists the
/// eight check lanes cheapest-question-first on purpose. Sorting them by cost
/// there would spend a 30-minute budget on the 450 s chaos lane and report
/// SKIP(budget) for the seven that would have answered.
///
/// MORE THAN ONE SLOT: longest reservation first. Declared order with two
/// slots would start the 22 s compile last and leave a slot idle for most of
/// the run; longest-first is what makes `concurrency = 2` behave the way
/// `pre-push.sh`'s hand-written launch-at-:380 / wait-at-:565 did.
///
/// The `--dry-run` render prints this order, which is how the wrong one was
/// caught: the first version sorted unconditionally and put `chaos-monkey`
/// ahead of the seven cheaper lanes in a venue that has one slot.
pub(super) fn run_order<'a>(lanes: &[&'a Instrument], concurrency: usize) -> Vec<&'a Instrument> {
    let mut out = lanes.to_vec();
    if concurrency > 1 {
        out.sort_by_key(|l| std::cmp::Reverse(l.reservation_secs()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::{reg, TABLE};
    use super::*;
    use kernel_types::quality::{Registry, RunsIn};

    /// ONE table now answers "what runs here". Before 2026-09-07 the eight
    /// lanes lived in `quality/check-lanes.toml` and everything else in
    /// `quality/instruments.toml`, with `kind`, `enforcement`, cost and
    /// `baseline` colliding on four names with different meanings (ARCH
    /// §10.6). This is the merged shape: two venues, disjoint selections,
    /// out of one file.
    #[test]
    fn one_registry_answers_both_venues_and_the_selections_are_disjoint() {
        let r = reg();
        let prepush = r.for_venue(&RunsIn::Prepush);
        let check = r.for_venue(&RunsIn::Check);
        assert_eq!(prepush.len(), 1);
        assert_eq!(check.len(), 1);
        assert_eq!(prepush[0].id, "docs-gate");
        assert_eq!(check[0].id, "chat-ask");
        // Both axes survived the merge on the lane row.
        assert_eq!(check[0].kind.label(), "bench");
        assert_eq!(check[0].claim.label(), "judged");
        // And the lane's baseline_dir is now a baseline KIND.
        assert_eq!(
            baseline_dir(check[0]),
            Some("sovereign/bench/quality-check/baselines/chat-ask")
        );
        assert_eq!(baseline_dir(prepush[0]), None);
    }

    /// The prepare step's whole value is the argv rewrite it buys: without
    /// it, eleven `cargo xtask` invocations queue on the cargo lock and the
    /// venue's declared concurrency "silently becomes a queue"
    /// (`pre-push.sh`:96, :367).
    #[test]
    fn the_hoisted_prepare_rewrites_the_gate_argv_and_only_that() {
        let r = reg();
        let t = r.trigger(&RunsIn::Prepush).expect("declared");
        let gate = r.get("docs-gate").unwrap();
        assert_eq!(
            t.rewrite(&gate.argv(), &[0]),
            vec!["target/debug/xtask", "docs-gate"]
        );
        // The build failed: the declared command runs. Slow, never wrong.
        assert_eq!(
            t.rewrite(&gate.argv(), &[]),
            vec!["cargo", "xtask", "docs-gate"]
        );
        // A lane is not a `cargo xtask` and is untouched.
        let lane = r.get("chat-ask").unwrap();
        assert_eq!(t.rewrite(&lane.argv(), &[0]), lane.argv());
    }

    /// `when_changed` is SELECTION, and an unset change set selects
    /// everything. Collapsing "the venue offered no change set" with "it
    /// offered an empty one" would silently run nothing (ARCH §18.3).
    #[test]
    fn when_changed_selects_and_an_absent_change_set_selects_everything() {
        let r = reg();
        let gate = r.get("docs-gate").unwrap();
        assert!(selected_by_change(gate, None));
        assert!(selected_by_change(
            gate,
            Some(&vec!["src/main.rs".to_string()])
        ));
        assert!(!selected_by_change(
            gate,
            Some(&vec!["README.md".to_string()])
        ));
        assert!(!selected_by_change(gate, Some(&Vec::new())));
        // An instrument with no pattern is always selected.
        let lane = r.get("chat-ask").unwrap();
        assert!(selected_by_change(
            lane,
            Some(&vec!["README.md".to_string()])
        ));
    }

    /// A pattern that will not compile SELECTS rather than drops. The safe
    /// direction is running something unnecessary; the unsafe one is a gate
    /// silently vanishing because a regex had a typo.
    #[test]
    fn a_malformed_when_changed_selects_rather_than_dropping_the_gate() {
        let broken = TABLE.replace(
            r#"when_changed = "\\.rs$""#,
            r#"when_changed = "(unclosed""#,
        );
        let r = Registry::parse(&broken).expect("parses — the pattern is only a string here");
        let gate = r.get("docs-gate").unwrap();
        assert!(selected_by_change(
            gate,
            Some(&vec!["README.md".to_string()])
        ));
    }

    /// The reservation rule is what lets one budget line serve a 0.04 s
    /// ratchet and a 450 s lane. Watched on both ends.
    #[test]
    fn a_reservation_comes_from_the_est_or_from_the_measurement() {
        let r = reg();
        assert_eq!(r.get("chat-ask").unwrap().reservation_secs(), 300);
        // 2.2 s measured, no est: reserves 3, never 2.
        assert_eq!(r.get("docs-gate").unwrap().reservation_secs(), 3);
    }

    /// The order a venue starts things in, watched BOTH ways — and the wrong
    /// way is the one the `--dry-run` render caught. Sorting unconditionally
    /// put the 450 s chaos lane ahead of seven cheaper ones in a venue with a
    /// single slot, which spends a 30-minute budget on one answer and reports
    /// SKIP(budget) for the rest.
    #[test]
    fn one_slot_keeps_the_declared_order_and_two_slots_sort_longest_first() {
        let r = reg();
        let all: Vec<&Instrument> = r.instruments.iter().collect();
        // Declared order is docs-gate (3 s) then chat-ask (300 s).
        let serial: Vec<&str> = run_order(&all, 1).iter().map(|i| i.id.as_str()).collect();
        assert_eq!(serial, vec!["docs-gate", "chat-ask"]);
        let parallel: Vec<&str> = run_order(&all, 2).iter().map(|i| i.id.as_str()).collect();
        assert_eq!(parallel, vec!["chat-ask", "docs-gate"]);
    }
}
