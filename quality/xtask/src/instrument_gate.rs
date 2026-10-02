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
    let closure = verify(&registry, &observed);
    let spoken = verdict_line_census(&registry, &root, &common::baseline_flags(args));
    // Both rules, both reported, worst code wins — a run that fixed one and
    // broke the other must not read as half green.
    closure.max(spoken)
}

// ─── Does a tool that DECIDES a verdict get to say so? ───────────────
//
// Rung `vl-2` of `quality/campaigns/verifier-loop.toml`, bar
// `vl-judgement-line`. `sovereign-cli-shared/src/lane_verdict.rs` was written
// because `scripts/lib/ci-bench-verdict.sh` reconstructs a four-verdict
// decision by grepping a lane's prose — "every one of those greps is a
// coupling to wording nobody promised to keep". The protocol fixed that for
// the eight `svrn quality check` lanes and for nothing else: ten back-of-house
// tools went on deciding passed / failed / could-not-judge / never-ran and
// saying it only in a table.
//
// TWO RULES, and the second is a ratchet because the arrears are real:
//
//   A (hard, no baseline) — a row declaring `verdict = "judgement-line"` whose
//     script never reaches the emitter. The runner would read `never-ran` from
//     a tool that ran, which is worse than the exit code it replaced.
//   B (ratchet) — a registered repo script that renders the four-verdict
//     vocabulary, cannot emit the line, and is read on its exit code alone.
//     Fourteen of these existed when the rule landed; the baseline is the
//     list, `--tighten` banks a conversion and a NEW one is a failure.
//
// THE DENOMINATOR IS THE REGISTRY, and that is a narrowing worth stating.
// Censusing the whole tree for the vocabulary returns 131 files — research
// arms, one-shot probes, fixtures — and a rule over that set would be one
// people silence. Being registered is what makes a tool something a runner
// reaches, which is the property the bar is actually about. The cost of the
// narrowing is that an unregistered tool is invisible here; the closure loop
// above is what stops anything a quality surface reaches from staying that way.

/// The two verdicts that mark the four-verdict vocabulary, in both
/// separators and matched case-insensitively — a tool spells them
/// `COULD-NOT-JUDGE` in its output, `could_not_judge` in a constant and
/// `could-not-judge` on the wire, and all three are the same decision system.
///
/// PASS and FAIL are deliberately NOT here: they are in every script in this
/// repo and say nothing about which system a tool is in. These two are the
/// ones an exit code cannot express, which is the whole reason the protocol
/// exists.
const VERDICT_MARKERS: [&str; 4] = [
    "could-not-judge",
    "could_not_judge",
    "never-ran",
    "never_ran",
];

/// Does this source decide in the four-verdict vocabulary?
fn renders_verdict(src: &str) -> bool {
    let lower = src.to_ascii_lowercase();
    VERDICT_MARKERS.iter().any(|m| lower.contains(m))
}

/// How a script reaches the protocol: the Python mirror, either spelling of
/// the import it exposes, or the shell mapping beside `lane_verdict`.
const EMITTER_MARKERS: [&str; 3] = ["judgement.py", "emit_judgement", "lane_judgement"];

const ARREARS_BASELINE: &str = "verdict_line_arrears.txt";

/// Resolve an instrument's command to a repo script, if it names one.
fn script_of(command: &str, root: &Path) -> Option<String> {
    for tok in command.split_whitespace() {
        let t = tok.trim_start_matches("./");
        if (t.ends_with(".py") || t.ends_with(".sh")) && root.join(t).is_file() {
            return Some(t.to_string());
        }
    }
    None
}

fn verdict_line_census(registry: &Registry, root: &Path, flags: &common::BaselineFlags) -> i32 {
    let mut silent: Vec<(String, String)> = Vec::new();
    let mut declared_but_mute: Vec<(String, String)> = Vec::new();
    let mut speaking = 0usize;
    let mut scripts = 0usize;
    for i in &registry.instruments {
        let Some(rel) = script_of(&i.command, root) else {
            continue;
        };
        scripts += 1;
        let Ok(src) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        let emits = EMITTER_MARKERS.iter().any(|m| src.contains(m));
        let renders = renders_verdict(&src);
        if emits {
            speaking += 1;
        }
        if i.verdict == kernel_types::quality::VerdictSource::JudgementLine && !emits {
            declared_but_mute.push((i.id.clone(), rel));
        } else if renders && !emits {
            silent.push((i.id.clone(), rel));
        }
    }

    let path = common::baselines_dir(root).join(ARREARS_BASELINE);
    let baseline = common::load_line_set(&path);
    let current: std::collections::BTreeSet<String> =
        silent.iter().map(|(id, _)| id.clone()).collect();
    // The id -> script map, so a NEW row can be reported with the file a reader
    // has to open. Looked up rather than defaulted: a `?` in its place would be
    // a substitution in the one line that has to be actionable (ARCH §18.3).
    let script_of_id: BTreeMap<&str, &str> = silent
        .iter()
        .map(|(id, rel)| (id.as_str(), rel.as_str()))
        .collect();
    let new: Vec<&String> = current.difference(&baseline).collect();
    let fixed: Vec<&String> = baseline.difference(&current).collect();

    eprintln!(
        "instrument-gate: {scripts} registered script(s) · {speaking} say their verdict in the protocol · {} still read on an exit code alone (baseline {})",
        current.len(),
        baseline.len()
    );

    if flags.update || flags.tighten {
        if flags.tighten && !new.is_empty() {
            eprintln!(
                "  ✗ --tighten refuses to bank a RAISE: {} new row(s) would enter the baseline",
                new.len()
            );
            return 1;
        }
        if let Err(e) = common::write_line_set(
            &path,
            "instrument-gate",
            "registered scripts that decide in the four-verdict vocabulary and are still read on \
             an exit code alone (rung vl-2)",
            &current,
        ) {
            eprintln!("  ✗ cannot write {}: {e}", path.display());
            return 1;
        }
        eprintln!("  baseline written: {} row(s)", current.len());
        return 0;
    }

    let mut bad = false;
    for (id, rel) in &declared_but_mute {
        bad = true;
        eprintln!(
            "  ✗ {id} declares `verdict = \"judgement-line\"` and {rel} never reaches the \
             emitter — a runner would read never-ran from a tool that ran, which is worse than \
             the exit code it replaced. Emit with `scripts/lib/judgement.py`."
        );
    }
    for id in &new {
        bad = true;
        let rel = script_of_id[id.as_str()];
        eprintln!(
            "  ✗ {id} ({rel}) decides in the four-verdict vocabulary and can only be read on its \
             exit code. Two of the four have no exit code to be. Emit the line \
             (`scripts/lib/judgement.py`, or `lane_judgement` in a shell tool) and set \
             `verdict = \"judgement-line\"` on its row."
        );
    }
    if !fixed.is_empty() {
        eprintln!(
            "  {} row(s) left the arrears — bank it: cargo run -p xtask -- instrument-gate \
             --tighten ({})",
            fixed.len(),
            fixed
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if bad {
        1
    } else {
        0
    }
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

    /// The verdict-line census reads a row's command to find its script, and
    /// the shapes in this registry are not uniform: `python3 x.py`,
    /// `./scripts/x.sh --flag`, a bare path, and plenty of rows that name no
    /// script at all. A resolver that missed one would quietly shrink the
    /// denominator, which is the failure mode a census cannot survive.
    #[test]
    fn a_rows_command_resolves_to_its_script_or_to_nothing() {
        let root = common::repo_root();
        assert_eq!(
            script_of("python3 scripts/nc-thesis.py", &root).as_deref(),
            Some("scripts/nc-thesis.py")
        );
        assert_eq!(
            script_of("./scripts/sovereign-ci-bench.sh --quick", &root).as_deref(),
            Some("scripts/sovereign-ci-bench.sh")
        );
        assert_eq!(
            script_of("python3 scripts/co_liveness.py verify", &root).as_deref(),
            Some("scripts/co_liveness.py")
        );
        // A cargo/xtask row names no script, and a path that does not exist is
        // not a reference to anything — both resolve to nothing rather than to
        // a string the census would then fail to read.
        assert_eq!(script_of("cargo xtask env-gate", &root), None);
        assert_eq!(script_of("python3 scripts/not-a-real-tool.py", &root), None);
    }

    /// The two markers that make the vocabulary detectable, and the two that
    /// do not. PASS and FAIL are in every script in this repo and say nothing
    /// about which decision system a tool is in.
    #[test]
    fn the_vocabulary_is_the_two_verdicts_an_exit_code_cannot_express() {
        let has = renders_verdict;
        assert!(has("status=\"SKIP(no-data)\"  # COULD-NOT-JUDGE"));
        assert!(has("rec.update(verdict=NEVER_RAN, detail=...)"));
        assert!(!has("echo \"PASS — everything is fine\""));
        assert!(!has("if rc != 0: print(\"FAIL\")"));
    }

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
