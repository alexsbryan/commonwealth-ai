// SPDX-License-Identifier: AGPL-3.0-or-later
//! The four tables `QUALITY_SURFACE.md` used to carry by hand, rendered from
//! the registry.
//!
//! Here and not in the CLI for the reason [`crate::judgement::render_rows`] is
//! here: a table is a CLAIM about the data beside it, and a claim rendered in
//! one crate and re-rendered in another drifts. `svrn quality map` is the
//! surface — flags, section selection, printing; the tables are this module,
//! and a golden test in `xtask/tests/` diffs them against the committed
//! `quality/quality-map.golden.md` so the doc cannot silently fall behind the
//! registry.
//!
//! DERIVED CLAIMS, NOT COPIED ONES. Two sentences the doc asserted in prose
//! are computed here instead: "CI stops at F1" is now the maximum fidelity
//! among instruments some CI job runs, and "everything else runs by hand only"
//! is now a count. Both were true when written and neither had anything
//! keeping them true.

use std::collections::BTreeMap;

use super::instruments::{Fidelity, Instrument, Kind, Registry};

/// All four tables, in the order a reader wants them: what exists, how much
/// each proof is worth, what silently weakens it, and where it runs.
pub fn render_map(registry: &Registry) -> String {
    let mut s = String::new();
    s.push_str(
        "<!-- GENERATED — do not edit by hand.\n\
         \x20    Source: quality/instruments.toml (the declared instrument registry)\n\
         \x20    Render: svrn quality map\n\
         \x20    Gate:   cargo xtask instrument-gate (every censused command has a row) -->\n\
         \n\
         # The quality surface — every instrument, rendered from the registry\n\
         \n",
    );
    s.push_str(&render_layers(registry));
    s.push('\n');
    s.push_str(&render_fidelity(registry));
    s.push('\n');
    s.push_str(&render_load_bearing(registry));
    s.push('\n');
    s.push_str(&render_where(registry));
    s.push('\n');
    s.push_str(&coverage_line(registry));
    s.push('\n');
    s
}

/// The layer table, generalised: grouped by what an instrument IS rather than
/// by which of the desktop's nine suites it happens to be.
pub fn render_layers(registry: &Registry) -> String {
    let mut s = String::from(
        "## Layers — what each instrument is, and whether CI runs it\n\n\
         `enforcement` says whether a not-passed verdict may fail the run that hosts it.\n\
         `in CI` is derived from `runs_in`, not asserted.\n",
    );
    let mut by_kind: BTreeMap<Kind, Vec<&Instrument>> = BTreeMap::new();
    for i in &registry.instruments {
        by_kind.entry(i.kind).or_default().push(i);
    }
    for (kind, mut rows) in by_kind {
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        s.push_str(&format!(
            "\n### {} — {}\n\n| instrument | command | enforcement | fidelity | cost | in CI |\n\
             |---|---|---|---|---|---|\n",
            kind.label(),
            kind_meaning(kind)
        ));
        for i in rows {
            s.push_str(&format!(
                "| `{}` | `{}` | {} | {} | {} | {} |\n",
                i.id,
                escape(&i.command),
                i.enforcement.label(),
                i.fidelity.label(),
                i.cost.label(),
                if i.in_ci() { "yes" } else { "**no**" },
            ));
        }
    }
    s
}

fn kind_meaning(kind: Kind) -> &'static str {
    match kind {
        Kind::Gate => "pass/fail on the repo as it stands",
        Kind::Suite => "a body of tests run together",
        Kind::Bench => "numbers, against a baseline or a band",
        Kind::Probe => "observes and reports; no verdict of its own to fail on",
        Kind::Control => {
            "breaks something on purpose and requires another instrument to notice — the only \
             kind that measures what the others would CATCH"
        }
        Kind::Check => "a composed lane runner with its own lane table",
    }
}

/// How far each instrument sits from what a user runs, and — computed, not
/// asserted — where CI stops.
pub fn render_fidelity(registry: &Registry) -> String {
    let mut s = String::from("## Fidelity — how much each green is worth\n\n");
    let ceiling = registry
        .instruments
        .iter()
        .filter(|i| i.in_ci())
        .map(|i| i.fidelity)
        .max();
    match ceiling {
        Some(f) => s.push_str(&format!(
            "**CI stops at {}** ({}). Everything above that line is verified only when a human \
             remembers to run it.\n",
            f.label(),
            f.meaning()
        )),
        // Never a silent blank: a registry where no instrument runs in CI is a
        // finding, and it renders as one (ARCH §18.3).
        None => s.push_str(
            "**No registered instrument runs in CI at all.** That is a finding, not a formatting \
             gap.\n",
        ),
    }
    s.push_str("\n| | meaning | instruments |\n|---|---|---|\n");
    for f in [
        Fidelity::F0,
        Fidelity::F1,
        Fidelity::F2,
        Fidelity::F3,
        Fidelity::F4,
        Fidelity::F5,
    ] {
        let ids: Vec<&str> = registry
            .by_fidelity()
            .get(&f)
            .map(|v| v.iter().map(|i| i.id.as_str()).collect())
            .unwrap_or_default();
        s.push_str(&format!(
            "| {} | {} | {} |\n",
            f.label(),
            f.meaning(),
            if ids.is_empty() {
                "— none".to_string()
            } else {
                ids.iter()
                    .map(|i| format!("`{i}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ));
    }
    s
}

/// The half no schema held before: flags and preconditions whose absence
/// leaves an instrument green while it proves less than you think.
pub fn render_load_bearing(registry: &Registry) -> String {
    let mut s = String::from(
        "## Load-bearing — what silently weakens a verdict\n\n\
         A flag here is one whose absence does not fail anything; it just makes the green mean \
         less. A precondition is a closed-set fact that must hold before the instrument can \
         judge at all — an unmet one is could-not-judge, never a pass.\n\n\
         | instrument | preconditions | load-bearing | why |\n|---|---|---|---|\n",
    );
    for i in registry.load_bearing() {
        let pre = if i.preconditions.is_empty() {
            "—".to_string()
        } else {
            i.preconditions
                .iter()
                .map(|p| format!("`{}`", p.label()))
                .collect::<Vec<_>>()
                .join("<br>")
        };
        if i.load_bearing.is_empty() {
            s.push_str(&format!("| `{}` | {pre} | — | — |\n", i.id));
            continue;
        }
        for (n, lb) in i.load_bearing.iter().enumerate() {
            s.push_str(&format!(
                "| {} | {} | `{}` | {} |\n",
                if n == 0 {
                    format!("`{}`", i.id)
                } else {
                    String::new()
                },
                if n == 0 { pre.clone() } else { String::new() },
                escape(&lb.flag),
                escape(&lb.why),
            ));
        }
    }
    s
}

/// Where each instrument runs — and the two populations that answer "what
/// verifies this repo nowhere", which is the question the eleven private
/// lists could not be asked.
pub fn render_where(registry: &Registry) -> String {
    let mut s = String::from("## What runs where\n\n| venue | instruments |\n|---|---|\n");
    for (venue, rows) in registry.by_venue() {
        s.push_str(&format!(
            "| `{venue}` | {} |\n",
            rows.iter()
                .map(|i| format!("`{}`", i.id))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let not_ci = registry.not_in_ci();
    s.push_str(&format!(
        "\n### What CI does not run ({} of {})\n\n",
        not_ci.len(),
        registry.instruments.len()
    ));
    s.push_str(&list_or_absence(
        &not_ci,
        "Every registered instrument runs in some CI job.",
    ));

    let nowhere = registry.nowhere();
    s.push_str(&format!(
        "\n### What nothing runs ({})\n\n\
         An instrument on no map runs nowhere. This is the population \
         `QUALITY_SURFACE.md`'s postmortem is about, and it is a list now rather than a \
         paragraph somebody has to remember to update.\n\n",
        nowhere.len()
    ));
    s.push_str(&list_or_absence(
        &nowhere,
        "Nothing is on no map. Check that before believing it.",
    ));
    s
}

fn list_or_absence(rows: &[&Instrument], absence: &str) -> String {
    if rows.is_empty() {
        return format!("{absence}\n");
    }
    let mut s = String::new();
    for i in rows {
        s.push_str(&format!(
            "- `{}` — {} · runs in: {}\n",
            i.id,
            i.doc,
            if i.runs_in.is_empty() {
                "**nothing**".to_string()
            } else {
                i.runs_in
                    .iter()
                    .map(|r| r.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ));
    }
    s
}

/// The one line `svrn posture` prints, rendered the same way here so the two
/// surfaces cannot disagree about the numbers (ARCH §10.6).
pub fn coverage_line(registry: &Registry) -> String {
    let c = registry.coverage();
    format!(
        "---\n\n**{} instruments, {} with a negative control, {} unmeasured cost, {} by-hand \
         only.** ({} run nowhere at all.)\n",
        c.total,
        c.with_negative_control,
        c.unmeasured_cost,
        c.by_hand_only,
        registry.nowhere().len(),
    )
}

/// A pipe inside a Markdown table cell ends the cell. Registry text is
/// authored prose and will contain one eventually.
fn escape(s: &str) -> String {
    s.replace('|', "\\|")
}

/// The venue a reader is most likely to ask about first, kept out of the
/// table body so the four renderers stay pure string builders.
pub fn venues(registry: &Registry) -> Vec<String> {
    registry.by_venue().keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality::Registry;

    const TWO: &str = r#"
censused_surfaces = [".github/workflows/ci.yml"]

[[instrument]]
id = "docs-gate"
kind = "gate"
command = "cargo xtask docs-gate"
cost_secs = 2.2
enforcement = "hard"
fidelity = "F0"
baseline = { kind = "none" }
negative_control = "none"
runs_in = ["prepush", "ci:gates"]
doc = "sovereign/ARCH_PRINCIPLES.md §1.1"

[[instrument]]
id = "wizard-verify"
kind = "gate"
command = "scripts/wizard-verify.sh"
cost_secs = "unmeasured"
enforcement = "hard"
fidelity = "F5"
preconditions = ["port-listening:9741"]
baseline = { kind = "none" }
negative_control = "none"
runs_in = []
doc = "sovereign/docs/specs/DAEMON_RESILIENCE.md"
load_bearing = [
  { flag = "SOVEREIGN_CLI_PATH unset", why = "the only coverage of the packaged branch" },
]
"#;

    fn reg(text: &str) -> Registry {
        match Registry::parse(text) {
            Ok(r) => r,
            Err(e) => panic!("{e:?}"),
        }
    }

    /// The ceiling is COMPUTED. The doc asserted "CI stops at F1" in prose and
    /// nothing kept it true.
    #[test]
    fn the_ci_ceiling_is_derived_from_runs_in() {
        let r = reg(TWO);
        assert!(render_fidelity(&r).contains("**CI stops at F0**"));
        // Give the F5 instrument a CI venue and the sentence moves with it.
        let moved = TWO.replace(r#"runs_in = []"#, r#"runs_in = ["ci:desktop"]"#);
        assert!(render_fidelity(&reg(&moved)).contains("**CI stops at F5**"));
    }

    /// A registry where nothing runs in CI renders that as a finding, not as a
    /// blank cell (ARCH §18.3).
    #[test]
    fn no_ci_at_all_renders_as_a_finding() {
        let none = TWO.replace(
            r#"runs_in = ["prepush", "ci:gates"]"#,
            r#"runs_in = ["prepush"]"#,
        );
        assert!(render_fidelity(&reg(&none))
            .contains("**No registered instrument runs in CI at all.**"));
    }

    /// The two populations the eleven private lists could not be asked about.
    #[test]
    fn the_where_table_names_what_ci_and_what_nothing_runs() {
        let out = render_where(&reg(TWO));
        assert!(out.contains("### What CI does not run (1 of 2)"));
        assert!(out.contains("### What nothing runs (1)"));
        assert!(out.contains("`wizard-verify`"));
        assert!(out.contains("runs in: **nothing**"));
        assert!(out.contains("| `ci:gates` | `docs-gate` |"));
    }

    #[test]
    fn the_coverage_line_matches_the_registrys_own_count() {
        let r = reg(TWO);
        let c = r.coverage();
        let line = coverage_line(&r);
        assert!(line.contains(&format!("**{} instruments", c.total)));
        assert!(line.contains(&format!("{} by-hand only", c.by_hand_only)));
    }

    /// A pipe in authored prose would otherwise silently split a cell and
    /// shift every column after it.
    #[test]
    fn a_pipe_in_a_cell_is_escaped() {
        let piped = TWO.replace(
            r#"why = "the only coverage of the packaged branch""#,
            r#"why = "either a|b, never both""#,
        );
        assert!(render_load_bearing(&reg(&piped)).contains(r"a\|b"));
    }

    #[test]
    fn every_kind_carries_a_meaning_in_the_layer_table() {
        let out = render_layers(&reg(TWO));
        assert!(out.contains("### gate — pass/fail on the repo as it stands"));
        assert!(out.contains("| `docs-gate` |"));
        // in-CI is derived, and the absence is emphasised because it is the
        // thing a reader most needs to notice.
        assert!(out.contains("| **no** |"));
    }

    #[test]
    fn the_full_map_carries_all_four_tables_and_says_it_is_generated() {
        let out = render_map(&reg(TWO));
        for needle in [
            "GENERATED — do not edit by hand",
            "## Layers",
            "## Fidelity",
            "## Load-bearing",
            "## What runs where",
        ] {
            assert!(out.contains(needle), "missing {needle}");
        }
        assert_eq!(venues(&reg(TWO)), vec!["ci:gates", "prepush"]);
    }
}
