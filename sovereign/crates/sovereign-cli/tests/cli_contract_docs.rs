// SPDX-License-Identifier: AGPL-3.0-or-later
//! Docs-conformance: the CLI contract manifest (`docs/cli-contract.toml`)
//! reconciled against `CLI_REFERENCE.md` + `README.md`.
//!
//! Pure file I/O — no binary spawn, no daemon, no feature gate — so it runs
//! in every build and CI lane (it is the cheapest, most hermetic half of the
//! harness). The binary-side guarantee (every promised command actually
//! dispatches) is the separate `cli_contract_code` test.
//!
//! Four checks:
//!  - forward: every canonical manifest command is documented in CLI_REFERENCE
//!    (its `### sovereign <verb>` section exists and the subcommand token
//!    appears in it).
//!  - reverse: every documented verb has at least one manifest row (verb-level,
//!    strict). Subcommand-level reverse is deferred to Phase 2 (it needs the
//!    `__dump-commands` enumerator to disambiguate format heterogeneity).
//!  - readme: every PUBLIC verb is named in the top-level README — the public
//!    contract surface (local inference + mesh + knowledge bases).
//!  - flags: for a seed set of stable verbs, each manifest flag is mentioned in
//!    that verb's CLI_REFERENCE section (lenient substring; opt-in per verb).
//!
//! Matching is containment-based on purpose: CLI_REFERENCE mixes subcommand
//! tables, flag tables, and prose+fences, and a precise parser would false-fail
//! on that heterogeneity. Containment errs toward false-failure (the safe
//! direction), never a false pass.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sovereign_cli_shared::cli_contract::{Contract, Visibility};

/// Verbs whose documented flags are held to the strict (substring) bar.
/// Seeded with the stable, fully-documented verbs; expand over time.
const FLAG_STRICT_VERBS: &[&str] = &["setup", "doctor", "chat", "drift", "pipeline"];

/// Repo path to the `sovereign/` crate-root (two ancestors above this crate).
fn sovereign_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = .../sovereign/crates/sovereign-cli
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("sovereign-cli has a .../sovereign ancestor")
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let p = sovereign_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn contract() -> Contract {
    Contract::load_default().expect("docs/cli-contract.toml must parse")
}

/// Map verb -> concatenated section body. Keyed under EVERY `sovereign <verb>`
/// spelling named in a header, so the `### \`sovereign reflect\` (alias:
/// \`sovereign notes\`)` header registers both `reflect` and `notes`.
fn sections(md: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let lines: Vec<&str> = md.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if let Some(verbs) = header_verbs(lines[i]) {
            let mut body = String::new();
            let mut j = i + 1;
            while j < lines.len() && !lines[j].starts_with("### ") && !lines[j].starts_with("## ") {
                body.push_str(lines[j]);
                body.push('\n');
                j += 1;
            }
            for v in verbs {
                out.entry(v).or_default().push_str(&body);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// If `line` is a `### \`sovereign <verb>\`` header, return every verb named in
/// a `sovereign <verb>` span on it (handles the alias header). Else `None`.
fn header_verbs(line: &str) -> Option<Vec<String>> {
    if !line.starts_with("### ") || !line.contains("`sovereign ") {
        return None;
    }
    const MARK: &str = "`sovereign ";
    let mut verbs = Vec::new();
    let mut rest = line;
    while let Some(pos) = rest.find(MARK) {
        let after = &rest[pos + MARK.len()..];
        match after.find('`') {
            Some(end) => {
                if let Some(v) = after[..end].split_whitespace().next() {
                    if !v.is_empty() {
                        verbs.push(v.to_string());
                    }
                }
                rest = &after[end..];
            }
            None => break,
        }
    }
    if verbs.is_empty() {
        None
    } else {
        Some(verbs)
    }
}

fn verb_of(path: &str) -> &str {
    path.split_whitespace().next().unwrap_or("")
}

// ── forward: manifest -> CLI_REFERENCE ──────────────────────────────────

#[test]
fn forward_every_manifest_command_is_documented() {
    let secs = sections(&read("docs/CLI_REFERENCE.md"));
    let c = contract();
    let mut fails = Vec::new();
    for cmd in c.canonical() {
        let mut toks = cmd.path.split_whitespace();
        let verb = toks.next().unwrap_or("");
        let Some(body) = secs.get(verb) else {
            fails.push(format!(
                "`sovereign {}` — no `### sovereign {verb}` section in CLI_REFERENCE.md",
                cmd.path
            ));
            continue;
        };
        if let Some(sub) = toks.next() {
            if !body.contains(sub) {
                fails.push(format!(
                    "`sovereign {}` — subcommand `{sub}` not found in the `{verb}` section",
                    cmd.path
                ));
            }
        }
    }
    assert!(
        fails.is_empty(),
        "manifest commands not documented in CLI_REFERENCE.md (fix the doc or the manifest):\n  {}",
        fails.join("\n  ")
    );
}

// ── reverse: CLI_REFERENCE -> manifest (verb level) ─────────────────────

#[test]
fn reverse_every_documented_verb_has_a_manifest_row() {
    let secs = sections(&read("docs/CLI_REFERENCE.md"));
    let c = contract();
    let manifest_verbs: BTreeSet<String> =
        c.commands.iter().map(|cmd| verb_of(&cmd.path).to_string()).collect();
    let mut fails = Vec::new();
    for verb in secs.keys() {
        if !manifest_verbs.contains(verb) {
            fails.push(format!(
                "CLI_REFERENCE.md documents `sovereign {verb}` but cli-contract.toml has no row for it"
            ));
        }
    }
    assert!(
        fails.is_empty(),
        "documented verbs missing from the manifest:\n  {}",
        fails.join("\n  ")
    );
}

// ── readme: every public verb is named in the public README ─────────────

#[test]
fn readme_names_every_public_verb() {
    let readme = read("README.md");
    let c = contract();
    let mut fails = Vec::new();
    let mut seen = BTreeSet::new();
    for cmd in c.commands.iter().filter(|c| c.visibility == Visibility::Public) {
        let verb = verb_of(&cmd.path);
        if !seen.insert(verb.to_string()) {
            continue;
        }
        if !readme.contains(verb) {
            fails.push(format!("public verb `{verb}` is not mentioned in README.md"));
        }
    }
    assert!(
        fails.is_empty(),
        "public verbs missing from README.md (the public contract surface):\n  {}",
        fails.join("\n  ")
    );
}

// ── flags: strict-verb flags are documented (lenient substring) ─────────

#[test]
fn strict_verb_flags_are_documented() {
    let secs = sections(&read("docs/CLI_REFERENCE.md"));
    let c = contract();
    let mut fails = Vec::new();
    for cmd in c.canonical() {
        let verb = verb_of(&cmd.path);
        if !FLAG_STRICT_VERBS.contains(&verb) {
            continue;
        }
        let Some(body) = secs.get(verb) else { continue };
        for f in &cmd.flags {
            if !body.contains(&f.name) {
                fails.push(format!(
                    "`sovereign {}` flag `{}` not documented in the `{verb}` section",
                    cmd.path, f.name
                ));
            }
        }
    }
    assert!(
        fails.is_empty(),
        "documented-flag gaps in strict verbs:\n  {}",
        fails.join("\n  ")
    );
}

// ── self-check: the section parser sees the alias header ─────────────────

#[test]
fn section_parser_registers_alias_headers() {
    let md = "### `sovereign reflect` (alias: `sovereign notes`)\nbody line\n\n### `sovereign setup`\nother\n";
    let secs = sections(md);
    assert!(secs.contains_key("reflect"), "reflect section missing");
    assert!(secs.contains_key("notes"), "notes alias section missing");
    assert!(secs.contains_key("setup"), "setup section missing");
    assert!(secs["reflect"].contains("body line"));
}
