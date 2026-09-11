// SPDX-License-Identifier: AGPL-3.0-or-later
//! lifecycle-gate — HALF TWO of `sv-no-daemon-management`'s instrument: no
//! thin surface retains a process handle or tracks a lifecycle.
//!
//! # Why there are two halves
//!
//! Half one is the dependency edge (`xtask layer-gate`, over
//! `[thin_surfaces]` in `quality/ARCH_LAYERS.toml`). It answers "can this
//! client BECOME a daemon", and it is necessary. It is not sufficient, and
//! the bar says why in its own words:
//!
//! > A dependency-edge gate passes clean through someone adding a retry, then
//! > a backoff, then a health check, then a restart policy — nobody
//! > re-supervises in one commit.
//!
//! A retry loop around `std::process::Command` needs no crate at all, so half
//! one moves not one inch when it is written. **Either alone is a green gate
//! enforcing nothing.**
//!
//! # The line this encodes, verbatim from the bar
//!
//! > A client may bring a backend up at a moment a USER ACTION asks for one,
//! > and never on a timer or a health signal.
//!
//! Turned into something a machine applies (ARCH principle 10 — never ask a
//! model to guarantee what code can enforce), that is a BRIGHT LINE rather
//! than a hunt for bad patterns: **a thin surface may not name the process
//! -control API at all.** Every ability to start, detach, retain or reap a
//! process arrives through `std::process`, `tokio::process` or Tauri's shell
//! plugin, and the one sanctioned bring-up —
//! `sovereign_turn_client::ServingHost::ensure_reachable` — returns a pid as a
//! FACT and grants none of them.
//!
//! Four rules, because "it failed" is weak evidence and the red must say which
//! ability came back:
//!
//! | rule | what it is | surface | the decider |
//! |---|---|---|---|
//! | `starts` | `Command::new(`, `.spawn()`, `.sidecar(`, `shell().command(` | ✗ | ✓ |
//! | `detaches` | `process_group(`, `creation_flags(` | ✗ | ✓ |
//! | `retains` | a binding or field typed `Child` / `CommandChild` | ✗ | ✗ |
//! | `reaps` | `.kill(`, `.wait()`, `.try_wait(`, `libc::kill` | ✗ | ✗ |
//!
//! The decider's two columns ARE the bar's sentence: it may bring one up and
//! detach it, and it may not hold or reap it. `bring_up` binds the `Child` and
//! drops it on the next line — `retains`/`reaps` being forbidden in that very
//! file is what refuses the `try_wait` loop growing back around it.
//!
//! # Why a health loop is not a fifth rule
//!
//! `attach_watch.rs` polls the daemon and drives a banner. That is a client
//! showing what it sees, which is the one thing a client does own. A watch
//! becomes SUPERVISION only when it can act on what it saw, and acting needs
//! `starts` or `reaps` — both of which are here. Adding a "polls" rule would
//! flag the UI and make the gate about the wrong thing; the two rules that
//! grant ability are sufficient, and that is the argument, not an omission.
//!
//! # Scope
//!
//! `src/**/*.rs` of each declared surface. Integration tests under `tests/`
//! and `#[cfg(test)] mod` blocks inside `src/` are skipped for the same reason
//! `DepKind::Dev` is never enforced on direction: neither reaches a shipped
//! artifact, and `reach.rs`'s own tests legitimately `kill` the process they
//! spawned to prove it detached.
//!
//! Policy lives in `[thin_surfaces]` — the SAME block half one reads. Two
//! gates that disagreed about what a surface is would each be green about a
//! different set (ARCH principle 8).

use std::collections::BTreeMap;
use std::path::Path;

use crate::common;

/// One rule the census can fire, its needles, and whether the declared
/// bring-up decider is allowed to trip it.
struct Rule {
    id: &'static str,
    /// Plain substrings. Each carries its open paren where one exists, so a
    /// string literal naming the API (`"Command::new"`, a test fixture's
    /// expectation) is not a call site.
    needles: &'static [&'static str],
    /// True when the ONE declared bring-up decider may trip this rule.
    decider_may: bool,
    /// What the red tells the reader to do instead.
    fix: &'static str,
}

const RULES: &[Rule] = &[
    Rule {
        id: "starts",
        needles: &["Command::new(", ".spawn()", ".sidecar(", "shell().command("],
        decider_may: true,
        fix: "a surface asks `sovereign_turn_client::ServingHost::ensure_reachable` \
              for a reachable backend and moves on; it does not start one itself",
    },
    Rule {
        id: "detaches",
        needles: &["process_group(", "creation_flags("],
        decider_may: true,
        fix: "detaching is the bring-up decider's job — it is what makes a start a \
              BRING-UP rather than a supervision, and it belongs beside the spawn",
    },
    Rule {
        id: "retains",
        needles: &[": Child", ":Child", "<Child", "Child>", "CommandChild"],
        decider_may: false,
        // The bar's own sentence, and the one line in `bring_up` that carries it.
        fix: "nothing may HOLD the handle — `reach.rs::bring_up` drops the `Child` at \
              the spawn site and returns the pid as a fact, because holding it is how \
              a client becomes a supervisor one `try_wait` at a time",
    },
    Rule {
        id: "reaps",
        needles: &[
            ".kill(",
            ".wait()",
            ".try_wait(",
            ".wait_with_output(",
            "libc::kill",
            "signal::kill",
            // Stop-on-exit, which the bar names in the same breath as the
            // retained `Child`. It was NOT in the first draft of this list,
            // and the first real run found the site it misses:
            // `mobile_host_setup.rs:88` sets `kill_on_drop(true)` on a
            // `tokio::process::Command`, so the client decides that process
            // ends when the client does — the whole rule, spelled as a
            // builder call with no `Child` in sight.
            "kill_on_drop(",
        ],
        decider_may: false,
        fix: "deciding when a backend stops, or waiting to learn that it did, is the \
              daemon's own lifetime and the OS service manager's recovery — not a \
              client's (ARCH principle 12)",
    },
];

/// One census hit.
struct Site {
    file: String,
    line: usize,
    /// The rule itself, not its id. A `&'static str` id would have to be
    /// looked back up against `RULES` to reach the remediation text, and that
    /// lookup can only be written with an `expect` that no input can reach —
    /// an unreachable panic is a claim nobody can test (ARCH principle 5).
    /// Carrying the reference makes the invariant structural instead.
    rule: &'static Rule,
    text: String,
}

pub fn run(args: &[String]) -> i32 {
    let root = common::repo_root();
    if args
        .iter()
        .any(|a| a == "--update-baseline" || a == "--tighten")
    {
        eprintln!(
            "lifecycle-gate has no machine-written baseline, deliberately. Its ledger is \
             `[[thin_surfaces.lifecycle_allow]]` in quality/ARCH_LAYERS.toml, where every \
             row carries a REASON and a kind — a waiver a gate can rewrite for you is one \
             nobody reads. Edit the row."
        );
        return 1;
    }

    let map_path = root.join("quality/ARCH_LAYERS.toml");
    let map_text = match std::fs::read_to_string(&map_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {} ({e})", map_path.display());
            return 1;
        }
    };
    let map = match arch_layers::parse(&map_text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let surfaces = &map.thin_surfaces;
    if surfaces.crates.is_empty() {
        // Never-ran, not passed: the rule was not configured, and a green here
        // would read exactly like a green on a clean tree (ARCH principle 6).
        eprintln!(
            "lifecycle-gate: quality/ARCH_LAYERS.toml declares no [thin_surfaces] crates — \
             NOTHING WAS CENSUSED. This run makes no claim about any surface."
        );
        return 4;
    }

    let scope = match common::SourceTree::discover(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let members = crate::manifests::workspace_members(&root);
    let dir_of: BTreeMap<&str, &str> = members
        .iter()
        .map(|m| (m.name.as_str(), m.dir.as_str()))
        .collect();

    // Every crate censused: the surfaces, plus the declared bring-up decider
    // (which is where "one retry at a time" would most plausibly be written).
    let mut censused: Vec<(&str, bool)> = surfaces
        .crates
        .iter()
        .map(|c| (c.as_str(), false))
        .collect();
    let decider_file = surfaces.bring_up_decider.as_ref().map(|d| d.file.clone());
    if let Some(d) = surfaces.bring_up_decider.as_ref() {
        censused.push((d.krate.as_str(), true));
    }

    let mut sites: Vec<Site> = Vec::new();
    let mut missing: Vec<&str> = Vec::new();
    for (krate, _) in &censused {
        let Some(dir) = dir_of.get(krate) else {
            missing.push(krate);
            continue;
        };
        let src = root.join(dir).join("src");
        collect(&src, &root, &scope, &mut sites);
    }
    if !missing.is_empty() {
        // A typo'd crate name is a rule that governs nothing while reading in
        // review as though it does.
        eprintln!(
            "lifecycle-gate: [thin_surfaces] names {} crate(s) the workspace does not \
             contain ({}) — nothing was censused for them",
            missing.len(),
            missing.join(", ")
        );
        return 1;
    }

    // ── Adjudicate every site against the ledger ──────────────────────────────
    let mut counts: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for s in &sites {
        *counts.entry((s.file.as_str(), s.rule.id)).or_default() += 1;
    }

    let mut fails: Vec<String> = Vec::new();
    let mut tightens: Vec<String> = Vec::new();
    let mut allowed_by_role = 0usize;
    let mut allowed_by_ledger = 0usize;

    for s in &sites {
        // The decider's ROLE, first: it may start and detach with no ledger row.
        if decider_file.as_deref() == Some(s.file.as_str()) && s.rule.decider_may {
            allowed_by_role += 1;
            continue;
        }
        let row = surfaces
            .lifecycle_allow
            .iter()
            .find(|a| a.file == s.file && a.rule == s.rule.id);
        match row {
            Some(_) => allowed_by_ledger += 1,
            None => fails.push(format!(
                "{}:{} [{}] `{}` — {}",
                s.file,
                s.line,
                s.rule.id,
                s.text.trim(),
                s.rule.fix
            )),
        }
    }

    for a in &surfaces.lifecycle_allow {
        let actual = counts
            .get(&(a.file.as_str(), a.rule.as_str()))
            .copied()
            .unwrap_or(0);
        if !RULES.iter().any(|r| r.id == a.rule) {
            fails.push(format!(
                "[[thin_surfaces.lifecycle_allow]] {} names rule `{}`, which this gate does \
                 not have (known: {}) — the row tolerates nothing and reads as though it does",
                a.file,
                a.rule,
                RULES.iter().map(|r| r.id).collect::<Vec<_>>().join(", ")
            ));
            continue;
        }
        if actual > a.max {
            fails.push(format!(
                "{} [{}] grew {} → {actual}: a surface acquired process control it did not \
                 have. This is the erosion the bar refuses — nobody re-supervises in one \
                 commit, they add a retry, then a backoff, then a health check. Row reason: \
                 {}",
                a.file, a.rule, a.max, a.reason
            ));
        } else if actual == 0 {
            fails.push(format!(
                "[[thin_surfaces.lifecycle_allow]] {} [{}] matches nothing — the sites are \
                 GONE. Delete the row; removals are the celebration, and a ledger that keeps \
                 paid-off debt stops measuring the bar.",
                a.file, a.rule
            ));
        } else if actual < a.max {
            tightens.push(format!(
                "{} [{}] is down {} → {actual} — lower `max` to {actual} to bank it",
                a.file, a.rule, a.max
            ));
        }
    }

    // ── Report ────────────────────────────────────────────────────────────────
    let burning: usize = surfaces
        .lifecycle_allow
        .iter()
        .filter(|a| a.kind == arch_layers::BURNING_DOWN)
        .map(|a| a.max)
        .sum();
    let sanctioned: usize = surfaces
        .lifecycle_allow
        .iter()
        .filter(|a| a.kind == arch_layers::SANCTIONED)
        .map(|a| a.max)
        .sum();
    eprintln!(
        "lifecycle-gate: {} surface(s) + {} bring-up decider censused over {} rule(s); {} \
         process-control site(s) found ({allowed_by_role} by the decider's role, \
         {allowed_by_ledger} on the ledger)",
        surfaces.crates.len(),
        usize::from(surfaces.bring_up_decider.is_some()),
        RULES.len(),
        sites.len(),
    );
    eprintln!(
        "  ledger: {burning} site(s) BURNING-DOWN (the bar's count, target 0) · {sanctioned} \
         sanctioned"
    );
    for t in &tightens {
        eprintln!("  · tighten: {t}");
    }
    for f in &fails {
        eprintln!("  ✗ {f}");
    }
    if fails.is_empty() {
        eprintln!(
            "  ✓ no thin surface retains a process handle or tracks a lifecycle outside the \
             declared ledger"
        );
        0
    } else {
        eprintln!();
        eprintln!(
            "lifecycle-gate FAILED ({} finding(s)). HALF TWO of sv-surface's \
             `sv-no-daemon-management` bar: {}",
            fails.len(),
            surfaces.doc
        );
        eprintln!(
            "The line, verbatim from the bar: a client may bring a backend up at a moment a \
             USER ACTION asks for one, and never on a timer or a health signal. Fix the site, \
             or declare it in [[thin_surfaces.lifecycle_allow]] with a `kind` and a reason — \
             a row is a reviewable policy diff, not a waiver."
        );
        eprintln!("{}", common::fix_footer("lifecycle-gate"));
        1
    }
}

fn collect(dir: &Path, root: &Path, scope: &common::SourceTree, out: &mut Vec<Site>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let rel = common::rel_path(&path, root);
        if path.is_dir() {
            // `tests/` anywhere under src is dev-only by this workspace's
            // convention, and cannot reach a shipped artifact.
            if !scope.excludes_dir(&rel) && path.file_name().is_some_and(|n| n != "tests") {
                collect(&path, root, scope, out);
            }
            continue;
        }
        if path.extension().is_some_and(|e| e == "rs") && !rel.ends_with("/tests.rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                scan(&rel, &text, out);
            }
        }
    }
}

/// Scan one file, skipping comments and `#[cfg(test)]` modules.
fn scan(rel: &str, text: &str, out: &mut Vec<Site>) {
    let mut test_depth: Option<i32> = None;
    let mut pending_test_mod = false;
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();

        // Track a `#[cfg(test)] mod … { … }` block by brace depth.
        if let Some(depth) = test_depth.as_mut() {
            *depth += line.matches('{').count() as i32;
            *depth -= line.matches('}').count() as i32;
            if *depth <= 0 {
                test_depth = None;
            }
            continue;
        }
        if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("#[cfg(all(test") {
            pending_test_mod = true;
            continue;
        }
        if pending_test_mod {
            if trimmed.starts_with("mod ") && line.contains('{') {
                let d = line.matches('{').count() as i32 - line.matches('}').count() as i32;
                if d > 0 {
                    test_depth = Some(d);
                }
                pending_test_mod = false;
                continue;
            }
            // `#[cfg(test)]` on a `use`, a fn or an inline `mod x;` — nothing
            // to skip past.
            pending_test_mod = false;
        }

        // A comment describing process control does not perform it. (The
        // module docs of `reach.rs` and `serving_host.rs` are ~20 lines of
        // exactly that, and they are the right prose to have.)
        if trimmed.starts_with("//") {
            continue;
        }
        for rule in RULES {
            if rule.needles.iter().any(|n| line.contains(n)) {
                out.push(Site {
                    file: rel.to_string(),
                    line: i + 1,
                    rule,
                    text: line.to_string(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(text: &str) -> Vec<(&'static str, usize)> {
        let mut out = Vec::new();
        scan("x.rs", text, &mut out);
        out.iter().map(|s| (s.rule.id, s.line)).collect()
    }

    /// The plant, in miniature: the four abilities the bar refuses, each
    /// naming its own rule so a red says WHICH one came back.
    #[test]
    fn each_ability_fires_its_own_rule() {
        assert_eq!(hits("let c = Command::new(&exe);"), [("starts", 1)]);
        assert_eq!(hits("    cmd.process_group(0);"), [("detaches", 1)]);
        assert_eq!(hits("    child: Option<Child>,"), [("retains", 1)]);
        assert_eq!(hits("    let _ = child.try_wait();"), [("reaps", 1)]);
    }

    /// The re-supervision the order names, written the way it actually gets
    /// written — no new crate, no new dependency edge, so half ONE does not
    /// move. Both abilities must fire.
    #[test]
    fn a_retained_child_with_a_respawn_loop_trips_two_rules() {
        let planted = r#"
struct Watch {
    child: Option<std::process::Child>,
}
impl Watch {
    fn on_down(&mut self) {
        if let Some(c) = self.child.as_mut() {
            if c.try_wait().ok().flatten().is_some() {
                self.child = Command::new("svrn").spawn().ok();
            }
        }
    }
}
"#;
        let fired: Vec<&str> = hits(planted).iter().map(|(r, _)| *r).collect();
        assert!(fired.contains(&"retains"), "{fired:?}");
        assert!(fired.contains(&"reaps"), "{fired:?}");
        assert!(fired.contains(&"starts"), "{fired:?}");
    }

    /// Prose about process control is not process control. Every needle that
    /// matters appears in `reach.rs`'s and `serving_host.rs`'s module docs,
    /// and a census that flagged them would be one people silence.
    #[test]
    fn comments_are_not_call_sites() {
        assert!(hits("//! drops the `Child` at the spawn site").is_empty());
        assert!(hits("    // let _ = child.kill();").is_empty());
        assert!(hits("/// No restart policy, no health loop, no Child.").is_empty());
    }

    /// A string literal naming the API is not a call site — the open paren is
    /// load-bearing. `deep_research_commands/tests.rs:434` is the live case:
    /// `"Command::new",` sits in a fixture's expectation list.
    #[test]
    fn a_string_literal_naming_the_api_is_not_a_call() {
        assert!(hits(r#"    let banned = ["Command::new", "spawn"];"#).is_empty());
    }

    /// `#[cfg(test)]` blocks cannot reach a shipped artifact, and `reach.rs`'s
    /// own tests `kill` the process they spawned to PROVE it detached. A
    /// census that read them would red the decider for proving its contract.
    #[test]
    fn cfg_test_modules_are_skipped_and_the_scan_resumes_after_them() {
        let text = r#"
fn production() {}

#[cfg(test)]
mod tests {
    fn helper() {
        let _ = Command::new("kill").spawn();
        if true { let _ = 1; }
    }
}

fn after() {
    let _ = child.kill();
}
"#;
        // Only the site AFTER the test module, and it must be found — a
        // depth-tracker that swallowed the rest of the file would look
        // identical to a clean scan (ARCH principle 5).
        let h = hits(text);
        assert_eq!(h.len(), 1, "{h:?}");
        assert_eq!(h[0].0, "reaps");
        // The raw string opens with a newline, so `child.kill()` is line 13.
        // Pinned exactly rather than "some line": a depth tracker that exited
        // the module one brace early would still find one `reaps` hit, at a
        // different line.
        assert_eq!(h[0].1, 13);
    }

    /// Every rule id the ledger may name is a rule this gate has. The two
    /// lists are one decider; a row naming a rule that does not exist
    /// tolerates nothing while reading as though it does.
    #[test]
    fn the_repos_own_ledger_names_only_rules_this_gate_has() {
        let root = common::repo_root();
        let text = std::fs::read_to_string(root.join("quality/ARCH_LAYERS.toml"))
            .expect("the layer map is the policy file");
        let map = arch_layers::parse(&text).expect("it must parse");
        for a in &map.thin_surfaces.lifecycle_allow {
            assert!(
                RULES.iter().any(|r| r.id == a.rule),
                "ledger row {} names rule `{}`, which this gate does not have",
                a.file,
                a.rule
            );
        }
    }
}
