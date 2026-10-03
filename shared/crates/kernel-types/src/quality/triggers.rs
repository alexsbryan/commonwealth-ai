// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[[trigger]]` — how a VENUE runs the instruments that select into it.
//!
//! `runs_in` says WHERE an instrument runs. It does not say how that venue
//! runs it, and the difference is not decoration: `scripts/pre-push.sh`
//! carried two facts that a naive "run everything with this tag, in order"
//! selection would have thrown away, making the replacement three times
//! slower than the thing it replaced.
//!
//! 1. **Concurrency.** The workspace compile was launched at :380 and waited
//!    on at :565, so it overlapped the ten ratchets rather than following
//!    them. Serial, the one-minute budget is gone before the gates start.
//! 2. **A hoisted prepare step.** :299 built the xtask binary ONCE, before
//!    the gates, "hoisted out of the gate function" precisely so that ten
//!    `cargo xtask …` invocations would not queue on the cargo lock — the
//!    file says it twice (:96, :367): "the concurrency silently becomes a
//!    queue".
//!
//! Both are properties of the VENUE, not of any instrument, so both are
//! declared here. A trigger also carries its wall budget and what a red does
//! to the venue's own exit code, because those differ per venue too: a failed
//! gate blocks a push and only warns a commit.

use std::collections::BTreeMap;

use super::RunsIn;

/// What a not-passed verdict does to the VENUE's exit code.
///
/// Distinct from [`super::Enforcement`], which is the INSTRUMENT's half of
/// the same question. Enforcement says "may this row fail a run"; this one
/// says "and does this venue then stop". `pre-commit` runs hard instruments
/// and still exits 0 — it is an advisory hook by operator directive
/// 2026-08-06 — so collapsing the two would make that venue unstateable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VenueAction {
    /// The venue exits non-zero.
    Block,
    /// The venue prints the finding and exits 0.
    Report,
}

impl VenueAction {
    pub fn parse(s: &str) -> Option<VenueAction> {
        Some(match s {
            "block" => VenueAction::Block,
            "report" => VenueAction::Report,
            _ => return None,
        })
    }

    pub const fn label(self) -> &'static str {
        match self {
            VenueAction::Block => "block",
            VenueAction::Report => "report",
        }
    }
}

/// What a venue does to an instrument that outlives the budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overrun {
    /// Stop it. It reaches no verdict, which is could-not-judge and never a
    /// pass.
    Kill,
    /// Let it finish and say how long the venue took. The budget is a number
    /// on the verdict line, enforced by not adding gates rather than by
    /// killing the ones that are there.
    Report,
}

impl Overrun {
    pub fn parse(s: &str) -> Option<Overrun> {
        Some(match s {
            "kill" => Some(Overrun::Kill),
            "report" => Some(Overrun::Report),
            _ => return None,
        }?)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Overrun::Kill => "kill",
            Overrun::Report => "report",
        }
    }
}

/// A step run ONCE before a trigger's instruments, and the argv rewrite it
/// buys.
///
/// The rewrite is the whole point and it is why this is not just "a command to
/// run first". Building `target/debug/xtask` is worth nothing on its own; what
/// it buys is that `cargo xtask docs-gate` may be spelled `target/debug/xtask
/// docs-gate` at the call site, so ten gates do not each re-enter cargo and
/// serialise on its lock. Declaring the substitution beside the build keeps
/// the registry's `command` field the literal command a human types — which is
/// also the closure gate's census key — while letting the runner execute the
/// cheap equivalent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepare {
    /// argv, run in the repo root. A non-zero exit does not abort the
    /// trigger: the substitution is simply not applied, every instrument runs
    /// its declared `command`, and the venue is SLOW rather than blind.
    pub command: Vec<String>,
    /// `(from, to)` — an argv PREFIX rewrite applied to every selected
    /// instrument whose argv starts with `from`. `None` for a prepare step
    /// that buys nothing but its own side effect.
    pub substitute: Option<(Vec<String>, Vec<String>)>,
}

/// One venue's run policy.
#[derive(Clone, Debug, PartialEq)]
pub struct Trigger {
    /// The venue this governs. The same vocabulary `runs_in` uses, so a
    /// trigger cannot name a venue no instrument can select into.
    pub id: RunsIn,
    /// Wall budget for the whole selection, in seconds.
    pub budget_secs: u64,
    /// How many instruments may be in flight at once. 1 is serial.
    pub concurrency: usize,
    /// Below this much remaining runway an instrument is not started at all:
    /// it would be killed mid-flight and report a cap that says nothing about
    /// the code. Per venue because the scale differs by three orders of
    /// magnitude — a lane wants 60 s of runway, a 0.04 s ratchet does not.
    pub min_runway_secs: u64,
    /// What happens to an instrument still running when the budget is spent.
    ///
    /// TWO REAL BEHAVIOURS, measured on 2026-09-07. `svrn quality check` KILLS:
    /// a lane held past a 30-minute budget is a lane nobody is waiting for, and
    /// could-not-judge is the honest verdict for one that was stopped. The push
    /// gate must NOT: its budget was always a REPORTING number ("the verdict
    /// line prints the elapsed total every run, so a gate that creeps past the
    /// budget shows up as a number rather than as a habit of skipping"), and
    /// the first selection-driven run killed the workspace compile at 59 s and
    /// called it could-not-judge — turning the one gate that matters most into
    /// an abstention over five seconds. An abstention you have not earned is
    /// not rigor (ARCH §18.2).
    pub overrun: Overrun,
    /// What a `failed` instrument does to the venue.
    pub on_fail: VenueAction,
    /// What a `could-not-judge` instrument does to the venue. Separate from
    /// [`Self::on_fail`] because they are different claims: `pre-push` blocks
    /// on a gate that said no and merely warns on one that could not run here
    /// (its UNVERIFIED list — "nothing in this push can fix a native
    /// toolchain that isn't installed").
    pub on_could_not_judge: VenueAction,
    /// Run once, in order, before any instrument.
    pub prepare: Vec<Prepare>,
}

impl Trigger {
    /// Apply this trigger's prepare substitutions to one instrument argv.
    ///
    /// Only prefixes rewrite, and only ones whose prepare step actually
    /// succeeded — the caller filters. A substitution that matched nothing is
    /// not an error: most instruments in a selection are not `cargo xtask`.
    pub fn rewrite(&self, argv: &[String], applied: &[usize]) -> Vec<String> {
        let mut out = argv.to_vec();
        for (i, p) in self.prepare.iter().enumerate() {
            if !applied.contains(&i) {
                continue;
            }
            let Some((from, to)) = &p.substitute else {
                continue;
            };
            if out.len() >= from.len() && out[..from.len()] == from[..] {
                let mut next = to.clone();
                next.extend_from_slice(&out[from.len()..]);
                out = next;
            }
        }
        out
    }
}

/// Parse the `[[trigger]]` rows. Reports every error, like the instrument
/// rows — and REFUSES a duplicate venue: two triggers for one venue means one
/// of them silently loses (ARCH §10.6).
pub(super) fn parse_triggers(value: &toml::Value) -> Result<Vec<Trigger>, Vec<String>> {
    let empty = Vec::new();
    // ABSENT and MALFORMED are two answers. A registry with no `[[trigger]]`
    // rows is a real state — nothing is delegating to it yet. A `trigger` key
    // that is not an array of tables is a MIS-EDITED file, and reading it as
    // "no triggers" would leave every venue unrunnable while the file looks
    // fine (ARCH §18.3 — the same rule the fingerprint applies to a
    // `smoke.toml` whose `subset` key will not read as an array).
    let rows = match value.get("trigger") {
        None => &empty,
        Some(v) => match v.as_array() {
            Some(a) => a,
            None => return Err(vec!["`trigger` must be an array of tables".to_string()]),
        },
    };
    let mut errors = Vec::new();
    let mut out: Vec<Trigger> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        match trigger(row, i) {
            Ok(t) => out.push(t),
            Err(mut e) => errors.append(&mut e),
        }
    }
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for (i, t) in out.iter().enumerate() {
        if let Some(first) = seen.insert(t.id.label(), i) {
            errors.push(format!(
                "trigger `{}` declared twice (rows {} and {})",
                t.id.label(),
                first + 1,
                i + 1
            ));
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

fn trigger(row: &toml::Value, index: usize) -> Result<Trigger, Vec<String>> {
    let mut errors = Vec::new();
    let raw_id = row.get("id").and_then(|v| v.as_str());
    let label = raw_id.unwrap_or("#?").to_string();
    let at = |what: &str| format!("trigger `{label}`: {what}");
    let id = match raw_id {
        None => {
            errors.push(format!("[[trigger]] #{}: missing `id`", index + 1));
            None
        }
        Some(s) => match RunsIn::parse(s) {
            Some(r) => Some(r),
            None => {
                errors.push(at("`id` is not a venue `runs_in` can name"));
                None
            }
        },
    };
    let budget_secs = u64_field(row, "budget_secs", &at, &mut errors);
    // A trigger that declares no concurrency is SERIAL. Defaulting to
    // "however many the host has" would silently change what a venue costs
    // when it moves machines. A concurrency that is PRESENT and not a
    // positive integer is refused rather than clamped: `concurrency = 0` and
    // `concurrency = -1` are typos, and silently reading them as 1 hides the
    // typo behind behaviour that looks deliberate.
    let concurrency = usize::try_from(positive_or_default(row, "concurrency", 1, &at, &mut errors))
        .unwrap_or(1)
        .max(1);
    let min_runway_secs = positive_or_default(row, "min_runway_secs", 0, &at, &mut errors);
    // KILL is the default because it is the stricter reading and it is what
    // `svrn quality check` already did. A venue that wants its budget to be a
    // report says so.
    let overrun = match row.get("overrun") {
        None => Some(Overrun::Kill),
        Some(v) => match v.as_str().and_then(Overrun::parse) {
            Some(o) => Some(o),
            None => {
                errors.push(at("`overrun` is not `kill` or `report`"));
                None
            }
        },
    };
    let on_fail = venue_action_field(row, "on_fail", &at, &mut errors);
    // Defaults to whatever `on_fail` says. A venue that blocks on a red and
    // has said nothing about could-not-judge means the stricter thing — the
    // safe direction, and the one `svrn quality check` already implements.
    let on_could_not_judge = match row.get("on_could_not_judge") {
        None => on_fail,
        Some(_) => venue_action_field(row, "on_could_not_judge", &at, &mut errors),
    };

    let mut prepare = Vec::new();
    if let Some(toml::Value::Array(rows)) = row.get("prepare") {
        for p in rows {
            let command = string_list(p, "command");
            if command.is_empty() {
                errors.push(at("each `prepare` entry needs a non-empty `command` array"));
                continue;
            }
            let substitute = match (p.get("substitute_from"), p.get("substitute_to")) {
                (None, None) => None,
                (Some(_), Some(_)) => {
                    let from = string_list(p, "substitute_from");
                    let to = string_list(p, "substitute_to");
                    if from.is_empty() || to.is_empty() {
                        errors.push(at(
                            "`substitute_from` and `substitute_to` must both be non-empty arrays",
                        ));
                        None
                    } else {
                        Some((from, to))
                    }
                }
                _ => {
                    errors.push(at(
                        "`substitute_from` and `substitute_to` are declared together or not at \
                         all — half a rewrite is a rewrite that silently does nothing",
                    ));
                    None
                }
            };
            prepare.push(Prepare {
                command,
                substitute,
            });
        }
    }

    let (Some(id), Some(budget_secs), Some(overrun), Some(on_fail), Some(on_could_not_judge)) =
        (id, budget_secs, overrun, on_fail, on_could_not_judge)
    else {
        if errors.is_empty() {
            errors.push(at("a required field was absent and reported nothing"));
        }
        return Err(errors);
    };
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Trigger {
        id,
        budget_secs,
        concurrency,
        min_runway_secs,
        overrun,
        on_fail,
        on_could_not_judge,
        prepare,
    })
}

/// An optional non-negative integer with a declared default. PRESENT-and-junk
/// is an error, not the default: a clamped typo reads as a deliberate setting.
fn positive_or_default(
    row: &toml::Value,
    key: &str,
    default: u64,
    at: &impl Fn(&str) -> String,
    errors: &mut Vec<String>,
) -> u64 {
    match row.get(key) {
        None => default,
        Some(v) => match v.as_integer() {
            Some(n) if n >= 0 => n as u64,
            _ => {
                errors.push(at(&format!(
                    "`{key}` must be a non-negative integer when declared"
                )));
                default
            }
        },
    }
}

fn u64_field(
    row: &toml::Value,
    key: &str,
    at: &impl Fn(&str) -> String,
    errors: &mut Vec<String>,
) -> Option<u64> {
    match row.get(key).and_then(toml::Value::as_integer) {
        Some(n) if n >= 0 => Some(n as u64),
        Some(_) => {
            errors.push(at(&format!("`{key}` must not be negative")));
            None
        }
        None => {
            errors.push(at(&format!("missing `{key}`")));
            None
        }
    }
}

fn venue_action_field(
    row: &toml::Value,
    key: &str,
    at: &impl Fn(&str) -> String,
    errors: &mut Vec<String>,
) -> Option<VenueAction> {
    match row.get(key).and_then(|v| v.as_str()) {
        Some(s) => match VenueAction::parse(s) {
            Some(d) => Some(d),
            None => {
                errors.push(at(&format!("`{key}` `{s}` is not `block` or `report`")));
                None
            }
        },
        None => {
            errors.push(at(&format!("missing `{key}` (`block` or `report`)")));
            None
        }
    }
}

fn string_list(row: &toml::Value, key: &str) -> Vec<String> {
    row.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(body: &str) -> Result<Vec<Trigger>, Vec<String>> {
        parse_triggers(&body.parse::<toml::Value>().unwrap())
    }

    #[test]
    fn a_trigger_declares_budget_concurrency_and_venue_action() {
        let t = one(r#"
[[trigger]]
id = "prepush"
budget_secs = 60
concurrency = 2
min_runway_secs = 1
on_fail = "block"
on_could_not_judge = "report"
[[trigger.prepare]]
command = ["cargo", "build", "--quiet", "-p", "xtask"]
substitute_from = ["cargo", "xtask"]
substitute_to = ["target/debug/xtask"]
"#)
        .expect("parses");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].id, RunsIn::Prepush);
        assert_eq!(t[0].budget_secs, 60);
        assert_eq!(t[0].concurrency, 2);
        assert_eq!(t[0].on_fail, VenueAction::Block);
        assert_eq!(t[0].on_could_not_judge, VenueAction::Report);
        assert_eq!(t[0].prepare.len(), 1);
        // Not declared here, so the stricter reading.
        assert_eq!(t[0].overrun, Overrun::Kill);
    }

    /// The rewrite is what makes the hoisted build worth anything. Watch it
    /// move a `cargo xtask` argv and leave every other argv alone.
    #[test]
    fn a_prepare_substitution_rewrites_only_the_prefix_it_declares() {
        let t = &one(r#"
[[trigger]]
id = "prepush"
budget_secs = 60
on_fail = "block"
[[trigger.prepare]]
command = ["cargo", "build", "-p", "xtask"]
substitute_from = ["cargo", "xtask"]
substitute_to = ["target/debug/xtask"]
"#)
        .unwrap()[0];
        let argv = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(
            t.rewrite(&argv("cargo xtask docs-gate"), &[0]),
            argv("target/debug/xtask docs-gate")
        );
        // Not the prefix: untouched.
        assert_eq!(
            t.rewrite(&argv("cargo fmt --all --check"), &[0]),
            argv("cargo fmt --all --check")
        );
        // The prepare step failed, so nothing was applied and the declared
        // command runs as written — slow, never wrong.
        assert_eq!(
            t.rewrite(&argv("cargo xtask docs-gate"), &[]),
            argv("cargo xtask docs-gate")
        );
    }

    #[test]
    fn malformed_triggers_are_refused_rather_than_defaulted() {
        // Unknown venue.
        assert!(
            one("[[trigger]]\nid = \"whenever\"\nbudget_secs = 1\non_fail = \"block\"").is_err()
        );
        // Missing budget.
        assert!(one("[[trigger]]\nid = \"prepush\"\non_fail = \"block\"").is_err());
        // The push gate's budget is a REPORT, and a typo in that word must not
        // silently become a KILL that stops the workspace compile.
        assert!(one(
            "[[trigger]]\nid = \"prepush\"\nbudget_secs = 1\non_fail = \"block\"\noverrun = \"maybe\""
        )
        .is_err());
        assert_eq!(
            one("[[trigger]]\nid = \"prepush\"\nbudget_secs = 1\non_fail = \"block\"\noverrun = \"report\"")
                .unwrap()[0]
                .overrun,
            Overrun::Report
        );
        // Unknown venue action.
        assert!(
            one("[[trigger]]\nid = \"prepush\"\nbudget_secs = 1\non_fail = \"maybe\"").is_err()
        );
        // Half a substitution silently does nothing — refuse it.
        assert!(one(
            "[[trigger]]\nid = \"prepush\"\nbudget_secs = 1\non_fail = \"block\"\n\
             [[trigger.prepare]]\ncommand = [\"true\"]\nsubstitute_from = [\"a\"]"
        )
        .is_err());
        // Two triggers for one venue: one of them would silently lose.
        assert!(one(
            "[[trigger]]\nid = \"prepush\"\nbudget_secs = 1\non_fail = \"block\"\n\
             [[trigger]]\nid = \"prepush\"\nbudget_secs = 2\non_fail = \"block\""
        )
        .is_err());
        // Present and junk is an ERROR, not the default: a clamped typo
        // reads as a deliberate setting.
        assert!(one(
            "[[trigger]]\nid = \"prepush\"\nbudget_secs = 1\non_fail = \"block\"\nconcurrency = -1"
        )
        .is_err());
        assert!(one(
            "[[trigger]]\nid = \"prepush\"\nbudget_secs = 1\non_fail = \"block\"\nmin_runway_secs = \"soon\""
        )
        .is_err());
        // ABSENT and MALFORMED are two answers. No `[[trigger]]` rows at all
        // is legal — a registry may delegate to none. A `trigger` key that is
        // not an array of tables is a mis-edited file, and reading it as "no
        // triggers" would leave every venue unrunnable while the file looks
        // fine (ARCH §18.3).
        assert_eq!(one("schema_version = 1"), Ok(Vec::new()));
        assert!(one("trigger = \"prepush\"").is_err());
    }

    /// A venue that says nothing about could-not-judge means the STRICTER
    /// thing. Defaulting the other way would turn every unmet precondition
    /// into a silent pass — the exact substitution ARCH §18.3 forbids.
    #[test]
    fn could_not_judge_defaults_to_the_same_venue_action_as_a_failure() {
        let t =
            one("[[trigger]]\nid = \"check\"\nbudget_secs = 1800\non_fail = \"block\"").unwrap();
        assert_eq!(t[0].on_could_not_judge, VenueAction::Block);
    }
}
