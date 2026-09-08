// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cargo xtask instrument-gate` — the closure loop for `quality/instruments.toml`.
//!
//! A registry with no gate is inventory. This is the gate: it censuses the
//! surfaces the registry DECLARES (`censused_surfaces`) and fails on any
//! command they reach that is neither an `[[instrument]]` nor a
//! `[[not_instrument]]` carrying a reason.
//!
//! WHAT IT IS FOR, stated as the failure it prevents.
//! `sovereign-desktop/QUALITY_SURFACE.md`'s own postmortem: `wizard-verify.sh`
//! — the only coverage of the packaged boot chain — was referenced by
//! `DAEMON_RESILIENCE.md` and by nothing executable for ten days, having
//! caught a ship-blocking bug on its first run. So a reference in a comment or
//! in a doc COUNTS here: being named by a quality-bearing surface is exactly
//! how that script "existed". The registry answers with the truth
//! (`runs_in = []` is legal and is the finding), never by hiding the row.
//!
//! WATCHED FAILING BEFORE IT WAS WATCHED PASSING (ARCH §18.1). Its first run
//! named `wizard-verify.sh`, `daemon-soak.sh`, `daemon-supervised.sh` and
//! `mesh-soak.sh` — the three the doc's "off the map" paragraph names plus the
//! one its postmortem is about — among 24 unregistered keys. If a future
//! change makes the first run of this gate find nothing, the gate has stopped
//! reaching the surfaces; that is the campaign's kill bar and it is why the
//! summary always prints the observed-key count, not just the verdict.
//!
//! FIVE EXTRACTORS, each with a key shape the registry can be matched against:
//! script paths, bare script basenames that resolve to a real file, `npm run`
//! targets, `npx <tool> <sub>`, and instrument-shaped `cargo`/`xtask`
//! subcommands (including the `for g in …; do xtask "$g"` loops in `ci.yml`
//! and `pre-push.sh`, which are where eight of the nine ratchets are named).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use kernel_types::quality::Registry;

use crate::common;

const REGISTRY_PATH: &str = "quality/instruments.toml";

/// `cargo`/`npx` subcommands that verify something. A tool not on this list
/// is not censused at all — the list is the gate's declared blind spot, and
/// widening it is a reviewable line rather than a silent behaviour change.
const INSTRUMENT_SUBCOMMANDS: [&str; 9] = [
    "fmt", "check", "test", "clippy", "hack", "modules", "deny", "doc", "bench",
];

/// Where a bare `foo.sh` in prose is looked up. First hit wins; a name that
/// resolves nowhere is not a reference to anything and is dropped.
const SCRIPT_DIRS: [&str; 3] = ["scripts", "sovereign/scripts", ".claude/hooks/tests"];

pub fn run(args: &[String]) -> i32 {
    let root = args
        .windows(2)
        .find(|w| w[0] == "--root")
        .map(|w| PathBuf::from(&w[1]))
        .unwrap_or_else(common::repo_root);
    let path = root.join(REGISTRY_PATH);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("instrument-gate: cannot read {}: {e}", path.display());
            return 1;
        }
    };
    let registry = match Registry::parse(&text) {
        Ok(r) => r,
        Err(errs) => {
            eprintln!("instrument-gate: {REGISTRY_PATH} is not valid:");
            for e in &errs {
                eprintln!("  ✗ {e}");
            }
            return 1;
        }
    };

    // A declared surface that is not on disk is a gate reaching nothing while
    // reporting green — refuse rather than skip (ARCH §18.3).
    let mut surfaces: Vec<PathBuf> = Vec::new();
    let mut missing: Vec<&str> = Vec::new();
    for declared in &registry.censused_surfaces {
        let p = root.join(declared);
        if p.is_dir() {
            let mut found = Vec::new();
            collect_files(&p, &mut found);
            if found.is_empty() {
                missing.push(declared);
            }
            surfaces.extend(found);
        } else if p.is_file() {
            surfaces.push(p);
        } else {
            missing.push(declared);
        }
    }
    if !missing.is_empty() {
        eprintln!(
            "instrument-gate: {} declared surface(s) do not exist — the gate would report green \
             while censusing nothing:",
            missing.len()
        );
        for m in missing {
            eprintln!("  ✗ {m}");
        }
        eprintln!("  Fix the paths in {REGISTRY_PATH} `censused_surfaces`.");
        return 1;
    }

    let observed = census(&root, &surfaces);
    verify(&registry, &observed)
}

// ─── Keys ───────────────────────────────────────────────────────────

/// One observed invocation, normalised to what the registry can be matched
/// against. Its [`Key::label`] is also the `[[not_instrument]]` spelling, so
/// an exemption is written exactly as the gate reports it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    /// A repo-relative script path.
    Script(String),
    /// A `package.json` script name.
    Npm(String),
    /// `npx <tool> <sub>` — the sub is part of the key because
    /// `npx playwright test` and `npx playwright install` are not one thing.
    Npx(String, String),
    /// An instrument-shaped `cargo` subcommand.
    Cargo(String),
    /// An xtask gate, however it was spelled at the call site.
    Xtask(String),
}

impl Key {
    fn label(&self) -> String {
        match self {
            Key::Script(p) => p.clone(),
            Key::Npm(n) => format!("npm:{n}"),
            Key::Npx(tool, sub) => format!("npx:{tool} {sub}"),
            Key::Cargo(sub) => format!("cargo:{sub}"),
            Key::Xtask(g) => format!("xtask:{g}"),
        }
    }

    /// Whether one declared command spelling IS this invocation.
    fn matches(&self, command: &str) -> bool {
        let tokens: Vec<&str> = command.split_whitespace().collect();
        match self {
            Key::Script(p) => command.contains(p.as_str()),
            Key::Npm(n) => adjacent(&tokens, "run", n),
            Key::Npx(tool, sub) => adjacent(&tokens, tool, sub),
            Key::Cargo(sub) => match sub.split_once(' ') {
                // `cargo build` alone is a build; `cargo build --timings` is
                // the weekly critical-path bench. Different keys on purpose.
                Some((head, flag)) => adjacent(&tokens, "cargo", head) && tokens.contains(&flag),
                None => adjacent(&tokens, "cargo", sub),
            },
            Key::Xtask(gate) => tokens.contains(&"xtask") && tokens.contains(&gate.as_str()),
        }
    }
}

fn adjacent(tokens: &[&str], a: &str, b: &str) -> bool {
    tokens.windows(2).any(|w| w[0] == a && w[1] == b)
}

/// A gate NAME, not a shell variable, a glob, or a prose placeholder — the
/// `cargo xtask <gate> --tighten` in baseline-tighten.yml's PR-body template
/// is documentation, and a gate that mints `<gate>` as a subject is inventing
/// findings (ARCH §18.1).
fn is_gate_name(w: &str) -> bool {
    !w.is_empty() && !w.contains(['$', '*', '<', '>', '{', '}'])
}

// ─── Census ─────────────────────────────────────────────────────────

/// Observed key → the `file:line` sites that named it.
fn census(root: &Path, surfaces: &[PathBuf]) -> BTreeMap<Key, Vec<String>> {
    let mut out: BTreeMap<Key, Vec<String>> = BTreeMap::new();
    for file in surfaces {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel = common::rel_path(file, root);
        let is_package_json = rel.ends_with("package.json");
        for (n, line) in content.lines().enumerate() {
            let site = format!("{rel}:{}", n + 1);
            for key in keys_in_line(root, line) {
                out.entry(key).or_default().push(site.clone());
            }
        }
        if is_package_json {
            for (name, site) in npm_script_names(&content, &rel) {
                out.entry(Key::Npm(name)).or_default().push(site);
            }
        }
        for (key, site) in xtask_loop_keys(&content, &rel) {
            out.entry(key).or_default().push(site);
        }
    }
    out
}

/// Everything one line names. Deliberately line-scoped: a site a human can
/// open is worth more than a clever multi-line parse.
fn keys_in_line(root: &Path, line: &str) -> Vec<Key> {
    let mut keys = Vec::new();
    let tokens: Vec<&str> = line
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '(' | ')' | ',' | '`'))
        .filter(|t| !t.is_empty())
        .collect();

    let mut paths_seen: BTreeSet<String> = BTreeSet::new();
    for t in &tokens {
        let t = t.trim_start_matches("./").trim_end_matches([';', ':']);
        if !(t.ends_with(".sh") || t.ends_with(".py")) {
            continue;
        }
        if let Some(dir) = SCRIPT_DIRS.iter().find(|d| t.starts_with(&format!("{d}/"))) {
            let _ = dir;
            if root.join(t).is_file() {
                paths_seen.insert(t.to_string());
                keys.push(Key::Script(t.to_string()));
            }
            continue;
        }
        // A BARE basename in prose or a comment. This is the wizard-verify.sh
        // shape: named by a doc, run by nothing. Only emitted when it resolves
        // to a real file, so the gate cannot invent an instrument.
        if !t.contains('/') {
            for dir in SCRIPT_DIRS {
                let candidate = format!("{dir}/{t}");
                if root.join(&candidate).is_file() && paths_seen.insert(candidate.clone()) {
                    keys.push(Key::Script(candidate));
                    break;
                }
            }
        }
    }

    for (i, t) in tokens.iter().enumerate() {
        match *t {
            "npm" => {
                if let Some(p) = tokens[i..].iter().position(|x| *x == "run") {
                    if let Some(name) = tokens.get(i + p + 1) {
                        keys.push(Key::Npm((*name).to_string()));
                    }
                }
            }
            "npx" => {
                if let (Some(tool), Some(sub)) = (tokens.get(i + 1), tokens.get(i + 2)) {
                    keys.push(Key::Npx((*tool).to_string(), (*sub).to_string()));
                }
            }
            "cargo" => {
                // Skip a `+toolchain` token so the weekly pinned-nightly form
                // (`cargo "+$(cat quality/nightly-pin.txt)" doc`) is censused
                // like any other.
                let sub = tokens[i + 1..]
                    .iter()
                    .find(|x| !x.starts_with('+') && !x.starts_with('$'));
                if let Some(sub) = sub {
                    if *sub == "xtask" {
                        if let Some(g) = tokens.get(i + 2).filter(|g| is_gate_name(g)) {
                            keys.push(Key::Xtask((*g).to_string()));
                        }
                    } else if *sub == "run" && tokens.contains(&"xtask") {
                        if let Some(p) = tokens.iter().position(|x| *x == "--") {
                            if let Some(g) = tokens.get(p + 1).filter(|g| is_gate_name(g)) {
                                keys.push(Key::Xtask((*g).to_string()));
                            }
                        }
                    } else if *sub == "build" && tokens.contains(&"--timings") {
                        keys.push(Key::Cargo("build --timings".to_string()));
                    } else if INSTRUMENT_SUBCOMMANDS.contains(sub) {
                        keys.push(Key::Cargo((*sub).to_string()));
                    }
                }
            }
            _ => {}
        }
        // `./target/debug/xtask <gate>` — the form the CI ratchets job uses.
        if t.trim_start_matches("./").ends_with("target/debug/xtask") {
            if let Some(g) = tokens.get(i + 1).filter(|g| is_gate_name(g)) {
                keys.push(Key::Xtask((*g).to_string()));
            }
        }
    }
    keys
}

/// `for g in a b c; do … xtask "$g"` and `XTASK_GATES=(a b c)` — the two
/// places eight of the nine ratchets are actually named. Without this the
/// gate would report the ratchets as uncensused and the `gates` job as
/// running one instrument.
fn xtask_loop_keys(content: &str, rel: &str) -> Vec<(Key, String)> {
    fn loop_words<'a>(line: &'a str, content: &str) -> Option<&'a str> {
        let rest = line.trim().strip_prefix("for ")?;
        let (var, tail) = rest.split_once(" in ")?;
        let var = var.trim();
        let used = [
            format!("xtask \"${var}\""),
            format!("xtask ${var}"),
            format!("xtask \"${{{var}}}\""),
        ]
        .iter()
        .any(|pat| content.contains(pat.as_str()));
        used.then(|| tail.split(';').next()).flatten()
    }

    let mut out = Vec::new();
    if !content.contains("xtask") {
        return out;
    }
    for (n, line) in content.lines().enumerate() {
        let site = format!("{rel}:{}", n + 1);
        // The loop variable must actually be handed to xtask. Without that
        // check, weekly.yml's `for crate in corpus-engine-notes …` (a `cargo
        // modules` loop in a file that also mentions xtask) minted four crate
        // names as gate ids — a gate that invents subjects is worse than one
        // that misses them (ARCH §18.1).
        let words: Option<&str> = if line.trim().starts_with("for ") {
            loop_words(line, content)
        } else if let Some((name, rest)) = line.trim().split_once('=') {
            (name.contains("XTASK") && rest.starts_with('('))
                .then(|| rest.trim_start_matches('(').split(')').next())
                .flatten()
        } else {
            None
        };
        let Some(words) = words else { continue };
        for w in words.split_whitespace().filter(|w| is_gate_name(w)) {
            out.push((Key::Xtask(w.to_string()), site.clone()));
        }
    }
    out
}

/// Every key of `package.json`'s `scripts` object. Deliberately the KEYS and
/// not the command strings: a script that exists is reachable by name whether
/// or not anything in the repo invokes it, which is the population
/// QUALITY_SURFACE.md's layer table was trying to keep by hand.
fn npm_script_names(content: &str, rel: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(start) = content.find("\"scripts\"") else {
        return out;
    };
    let mut depth = 0usize;
    let mut started = false;
    for (n, line) in content[start..].lines().enumerate() {
        let site = format!("{rel}:{}", content[..start].matches('\n').count() + n + 1);
        depth += line.matches('{').count();
        if depth > 0 {
            started = true;
        }
        if started && depth > 0 {
            if let Some(rest) = line.trim().strip_prefix('"') {
                if let Some((name, _)) = rest.split_once('"') {
                    if name != "scripts" {
                        out.push((name.to_string(), site));
                    }
                }
            }
        }
        depth = depth.saturating_sub(line.matches('}').count());
        if started && depth == 0 {
            break;
        }
    }
    out
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut found: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    found.sort();
    for p in found {
        if p.is_dir() {
            collect_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

// ─── Verdict ────────────────────────────────────────────────────────

fn verify(registry: &Registry, observed: &BTreeMap<Key, Vec<String>>) -> i32 {
    let exempt: BTreeMap<&str, &str> = registry
        .not_instruments
        .iter()
        .map(|n| (n.key.as_str(), n.why.as_str()))
        .collect();

    let mut unregistered: Vec<(&Key, &Vec<String>)> = Vec::new();
    let mut exempted = 0usize;
    let mut used_exemptions: BTreeSet<&str> = BTreeSet::new();
    for (key, sites) in observed {
        let label = key.label();
        if let Some(&_why) = exempt.get(label.as_str()) {
            exempted += 1;
            used_exemptions.insert(exempt.get_key_value(label.as_str()).map_or("", |(k, _)| k));
            continue;
        }
        let known = registry.instruments.iter().any(|i| {
            std::iter::once(i.command.as_str())
                .chain(i.also_invoked_as.iter().map(String::as_str))
                .any(|c| key.matches(c))
        });
        if !known {
            unregistered.push((key, sites));
        }
    }

    let cov = registry.coverage();
    eprintln!(
        "instrument-gate: {} observed key(s) across {} declared surface(s) · {} registered \
         instrument(s) · {exempted} exempted · {} with a negative control · {} unmeasured cost · \
         {} by-hand only · {} run nowhere",
        observed.len(),
        registry.censused_surfaces.len(),
        cov.total,
        cov.with_negative_control,
        cov.unmeasured_cost,
        cov.by_hand_only,
        registry.nowhere().len(),
    );

    let stale: Vec<&&str> = exempt
        .keys()
        .filter(|k| !used_exemptions.contains(**k))
        .collect();
    if !stale.is_empty() {
        // Reported, not failed: an exemption may legitimately cover a surface
        // that has not been censused today (a workflow behind an `if:`). It is
        // still the tighten direction, so it is said out loud.
        eprintln!(
            "instrument-gate: {} exemption(s) matched nothing this run — drop them or say why \
             they stay: {}",
            stale.len(),
            stale
                .iter()
                .map(|k| (**k).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    if unregistered.is_empty() {
        eprintln!("instrument-gate: OK — every censused command is registered or exempted");
        return 0;
    }

    eprintln!();
    eprintln!(
        "instrument-gate: {} command(s) reachable from a declared surface are on no map:",
        unregistered.len()
    );
    for (key, sites) in &unregistered {
        eprintln!("  ✗ {}", key.label());
        for s in sites.iter().take(3) {
            eprintln!("      {s}");
        }
        if sites.len() > 3 {
            eprintln!("      … and {} more site(s)", sites.len() - 3);
        }
    }
    eprintln!();
    eprintln!(
        "  Add an [[instrument]] to {REGISTRY_PATH} for each — `runs_in = []` is legal and is the\n  \
         honest answer for one nothing runs. If it verifies nothing, add a [[not_instrument]]\n  \
         with the key exactly as printed above AND a reason."
    );
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(line: &str) -> Vec<String> {
        keys_in_line(Path::new("/nonexistent"), line)
            .iter()
            .map(Key::label)
            .collect()
    }

    #[test]
    fn npm_npx_and_cargo_invocations_become_keys() {
        assert_eq!(keys("      - run: npm run check"), vec!["npm:check"]);
        assert_eq!(
            keys("npm --prefix sovereign/crates/sovereign-desktop run test:e2e"),
            vec!["npm:test:e2e"]
        );
        assert_eq!(
            keys("      - run: npx playwright test"),
            vec!["npx:playwright test"]
        );
        assert_eq!(
            keys("      - run: cargo fmt --all --check"),
            vec!["cargo:fmt"]
        );
        assert_eq!(keys("  cargo deny check advisories"), vec!["cargo:deny"]);
    }

    /// `npx playwright install` is setup and `npx playwright test` is the
    /// suite. One key for both would let the setup step ride the suite's
    /// registration.
    #[test]
    fn the_npx_subcommand_is_part_of_the_key() {
        assert_ne!(
            keys("npx playwright install --with-deps chromium"),
            keys("npx playwright test")
        );
    }

    /// `cargo build -p xtask` is a build step; `cargo build --workspace
    /// --timings` is the weekly critical-path bench. Same subcommand, and
    /// collapsing them would exempt the bench by accident.
    #[test]
    fn a_timings_build_is_a_different_key_from_a_plain_build() {
        assert!(keys("      - run: cargo build -p xtask").is_empty());
        assert_eq!(
            keys("      - run: cargo build --workspace --timings -j 3"),
            vec!["cargo:build --timings"]
        );
    }

    /// Both xtask spellings in the repo reach the same key: `cargo xtask
    /// arch-gate` locally, `cargo run -p xtask -- api-gate` in weekly,
    /// `./target/debug/xtask "$g"` in the CI ratchets job.
    #[test]
    fn every_xtask_spelling_reaches_the_same_key_shape() {
        assert_eq!(keys("cargo xtask arch-gate"), vec!["xtask:arch-gate"]);
        assert_eq!(
            keys("      - run: cargo run -p xtask -- api-gate"),
            vec!["xtask:api-gate"]
        );
        assert_eq!(
            keys("            ./target/debug/xtask size-gate"),
            vec!["xtask:size-gate"]
        );
        // Neither a loop variable nor a prose placeholder is a gate name.
        assert!(keys("            ./target/debug/xtask \"$g\" || rc=1").is_empty());
        assert!(keys("            Weekly `cargo xtask <gate> --tighten` run.").is_empty());
    }

    /// The loop and the array are where eight of the nine ratchets are named.
    #[test]
    fn the_gate_lists_in_ci_and_pre_push_are_expanded() {
        let ci = "          for g in docs-gate arch-gate boundary-gate; do\n            ./target/debug/xtask \"$g\" || rc=1\n          done\n";
        let got: Vec<String> = xtask_loop_keys(ci, "ci.yml")
            .iter()
            .map(|(k, _)| k.label())
            .collect();
        assert_eq!(
            got,
            ["xtask:docs-gate", "xtask:arch-gate", "xtask:boundary-gate"]
        );

        let hook = "XTASK_GATES=(docs-gate arch-gate concept-gate)\nrun \"$xtask\" \"$g\"\n";
        let got: Vec<String> = xtask_loop_keys(hook, "pre-push.sh")
            .iter()
            .map(|(k, _)| k.label())
            .collect();
        assert_eq!(
            got,
            ["xtask:docs-gate", "xtask:arch-gate", "xtask:concept-gate"]
        );

        // A file that never mentions xtask contributes nothing, so an
        // unrelated `for` loop cannot mint gate names.
        assert!(xtask_loop_keys("for f in a b c; do echo $f; done\n", "x.sh").is_empty());
    }

    #[test]
    fn package_json_script_names_are_read_as_keys() {
        let pkg = "{\n  \"name\": \"x\",\n  \"scripts\": {\n    \"check\": \"svelte-check\",\n    \"test:e2e:real\": \"playwright test\"\n  },\n  \"devDependencies\": {\n    \"vite\": \"^5\"\n  }\n}\n";
        let got: Vec<String> = npm_script_names(pkg, "package.json")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(got, ["check", "test:e2e:real"]);
    }

    /// Matching is against the DECLARED spellings only. An alias is written
    /// down or it does not exist.
    #[test]
    fn a_key_matches_only_a_declared_spelling() {
        let k = Key::Npx("playwright".into(), "test".into());
        assert!(!k.matches("npm run test:e2e"));
        assert!(k.matches("npx playwright test"));
        assert!(Key::Npm("test:e2e".into()).matches("npm run test:e2e"));
        assert!(!Key::Npm("test".into()).matches("npm run test:e2e"));
        assert!(Key::Xtask("arch-gate".into()).matches("cargo run -p xtask -- arch-gate"));
        assert!(Key::Script("scripts/wizard-verify.sh".into())
            .matches("run_capped 900 scripts/wizard-verify.sh"));
    }
}
