// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code converge` — the concept-convergence workhorse.
//!
//! Thin CLI wrapper over `corpus_engine_scip::converge`. Resolves the corpus
//! the same way `arch-report` / `suggest-seams` / `dry-report` do (explicit
//! `--corpus-id`, or the sole indexed code corpus), reads the SCIP graph
//! read-only, and prints. Never writes, never calls a model, never builds.
//!
//! Sibling verbs answer adjacent questions and this one defers to them
//! rather than reimplementing (ARCH §19): duplicated BEHAVIOUR is
//! `dry-report`, oversized FILES are `suggest-seams`, crate coupling is
//! `arch-report`. This verb owns duplicated IDENTITY only.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use corpus_engine_scip::converge::{
    census, crate_dag, dossier, duplicate_count, render_census, render_dossier, type_defs,
    SourceScope,
};
use corpus_engine_scip::ScipGraph;

const HELP: &str = "\
svrn code converge <census|noun|status> [options]

Duplicated concept IDENTITY over the SCIP graph — names defined as a type in
more than one crate. Read-only; no daemon, no model, no build.

  census                  what is duplicated, ranked
    --kin                 also count morphological family (over-collects)
    --limit N             rows to print (0 = all; default 40)

  noun <Name>             one noun's dossier: definitions, users, the crate
                          that could own it, and the users that cannot reach it

  status                  the ratchet number, against a frozen baseline
    --baseline <file>     default quality/baselines/concepts.txt
    --mint                write the current number as the baseline
    exit 0 pass · 1 a duplicate was ADDED · 3 the graph cannot speak for this
    commit (re-index) · 4 no baseline yet. The count comes from the graph, so
    every run says which commit it is about before it says the number.

Common:
  --corpus-id <id>        default: the sole indexed code corpus
  --include <prefix>      restrict to a path prefix (repeatable)
  --json                  machine output, carrying the scope that produced it

Adjacent verbs, deliberately not duplicated here:
  svrn code dry-report      duplicated BEHAVIOUR (clone + near-clone bodies)
  svrn code suggest-seams   split proposals for an oversized FILE
  svrn code arch-report     crate coupling and the carrier symbols
";

pub(crate) async fn run(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        println!("{HELP}");
        return i32::from(args.is_empty());
    }

    let sub = args[0].clone();
    let mut corpus_id: Option<String> = None;
    let mut baseline = PathBuf::from("quality/baselines/concepts.txt");
    let mut includes: Vec<String> = Vec::new();
    let mut noun: Option<String> = None;
    let mut limit: usize = 40;
    let mut kin = false;
    let mut json = false;
    let mut mint = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus-id" => {
                i += 1;
                corpus_id = args.get(i).cloned();
                if corpus_id.is_none() {
                    eprintln!("error: --corpus-id requires a value");
                    return 1;
                }
            }
            "--baseline" => {
                i += 1;
                match args.get(i) {
                    Some(v) => baseline = PathBuf::from(v),
                    None => {
                        eprintln!("error: --baseline requires a path");
                        return 1;
                    }
                }
            }
            "--include" => {
                i += 1;
                match args.get(i) {
                    Some(v) => includes.push(v.clone()),
                    None => {
                        eprintln!("error: --include requires a prefix");
                        return 1;
                    }
                }
            }
            "--limit" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(0) => limit = usize::MAX,
                    Some(n) => limit = n,
                    None => {
                        eprintln!("error: --limit requires an integer");
                        return 1;
                    }
                }
            }
            "--kin" => kin = true,
            "--json" => json = true,
            "--mint" => mint = true,
            "-h" | "--help" => {
                println!("{HELP}");
                return 0;
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag {flag}");
                return 1;
            }
            positional => {
                if noun.is_none() {
                    noun = Some(positional.to_string());
                }
            }
        }
        i += 1;
    }

    let indexes_dir = sovereign_cli_shared::dirs::sovereign_root().join("indexes");
    let corpus_id = match resolve_corpus(corpus_id, &indexes_dir) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let db_path = indexes_dir.join(&corpus_id).join("scip_graph.db");
    if !db_path.exists() {
        eprintln!(
            "error: no SCIP graph at {} — run `svrn project init` first",
            db_path.display()
        );
        return 1;
    }

    let graph = match ScipGraph::open(&db_path, &corpus_id) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: opening {}: {e}", db_path.display());
            return 1;
        }
    };
    let symbols = match graph.iter_all_symbols().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: reading symbols: {e}");
            return 1;
        }
    };

    let scope = SourceScope {
        include_prefixes: includes,
        ..SourceScope::default()
    };
    let defs = type_defs(&symbols, &scope);
    if defs.is_empty() {
        eprintln!(
            "error: no type definitions in scope — the graph has {} symbols, so \
             the scope filter is probably wrong (check --include)",
            symbols.len()
        );
        return 1;
    }

    match sub.as_str() {
        "census" => {
            let c = census(&defs, &scope, kin);
            if json {
                println!("{}", serde_json::to_string_pretty(&c).unwrap_or_default());
            } else {
                print!("{}", render_census(&c, limit, kin));
            }
            0
        }
        "noun" => {
            let Some(name) = noun else {
                eprintln!("error: `converge noun` requires a name, e.g. `converge noun Verdict`");
                return 1;
            };
            let refs = match graph.iter_all_refs().await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: reading refs: {e}");
                    return 1;
                }
            };
            let dag = crate_dag(&refs, &scope);
            let d = dossier(&name, &defs, &refs, &dag, &scope);
            if json {
                println!("{}", serde_json::to_string_pretty(&d).unwrap_or_default());
            } else {
                print!("{}", render_dossier(&d));
            }
            0
        }
        "status" => {
            // The lag is computed HERE, from the same graph handle and the same
            // scope that produced `defs`, so the freshness line and the number
            // can never be about different things.
            let lag = assess_lag(graph.last_indexed_head().await, &defs, &scope);
            cmd_status(&defs, &baseline, mint, json, &lag, &corpus_id)
        }
        other => {
            eprintln!("error: unknown converge subcommand `{other}`");
            println!("{HELP}");
            1
        }
    }
}

fn resolve_corpus(explicit: Option<String>, indexes_dir: &std::path::Path) -> Result<String, i32> {
    if let Some(c) = explicit {
        return Ok(c);
    }
    let mut corpora: Vec<String> = std::fs::read_dir(indexes_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().join("scip_graph.db").exists())
                .filter_map(|e| e.file_name().to_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    corpora.sort();
    match corpora.len() {
        1 => Ok(corpora.remove(0)),
        0 => {
            eprintln!(
                "error: no code corpus under {} — run `svrn project init` first",
                indexes_dir.display()
            );
            Err(1)
        }
        _ => {
            eprintln!(
                "error: multiple code corpora — pass --corpus-id <one of: {}>",
                corpora.join(", ")
            );
            Err(1)
        }
    }
}

// ── Freshness: what the number is ABOUT ───────────────────────────────────────
//
// `converge status` counts type definitions in the SCIP graph, and the graph is
// rebuilt on a git-poll cadence — so it lags the working tree. A count printed
// without that fact is a count about some OTHER commit, which is the exact
// shape of a green that is not real (ARCH §18.3).
//
// The predicate is deliberately NOT `last_indexed_head != HEAD`. Measured
// 2026-08-20 on this repo: the graph sat two commits behind HEAD and the only
// files in the gap were `quality/campaigns/noun-convergence.toml` and
// `scripts/nc-boundary.py` — nothing this count is derived from. A gate that
// refuses there cries wolf on every docs-only commit, and a gate that cries
// wolf is switched off inside a week. So the gap is resolved against the
// extension set the count actually reads, and that set is DERIVED from the
// graph (the extensions of the files carrying the type definitions in scope)
// rather than typed as a constant — this workspace already carries four
// hardcoded source-extension lists and §10.6 says do not mint a fifth.
//
// Committed gap and working-tree dirt are reported differently on purpose. A
// committed gap that touches indexed source means the number is about the wrong
// commit: could-not-judge. Uncommitted edits are the NORMAL mid-edit state and
// only ever ADD to what the graph has not seen yet, so they are reported as a
// caveat and do not move the verdict.

/// What the graph's number is about, relative to the commit being gated.
pub(crate) enum Freshness {
    /// The graph's head is this repo's HEAD.
    Current,
    /// The graph is behind HEAD, but nothing in the gap feeds this count.
    BehindIrrelevant {
        indexed: String,
        head: String,
        gap_files: usize,
    },
    /// Source this count IS derived from changed in the un-indexed gap.
    Stale {
        indexed: String,
        head: String,
        changed: Vec<String>,
    },
    /// No git, no recorded head, or the indexed head is not an object here.
    Unknown { why: String },
}

/// The graph's lag, in the two forms that read differently.
pub(crate) struct Lag {
    pub(crate) freshness: Freshness,
    /// Working-tree paths (tracked edits + untracked files) that carry the
    /// extensions this count reads. Caveat only — never a verdict.
    uncommitted: Vec<String>,
    /// The extensions the count is derived from, for the glassbox line.
    exts: Vec<String>,
}

impl Lag {
    /// True when the printed number can be said to be about THIS commit.
    pub(crate) fn can_judge(&self) -> bool {
        matches!(
            self.freshness,
            Freshness::Current | Freshness::BehindIrrelevant { .. }
        )
    }

    pub(crate) fn verdict_word(&self) -> &'static str {
        match self.freshness {
            Freshness::Current => "current",
            Freshness::BehindIrrelevant { .. } => "behind-irrelevant",
            Freshness::Stale { .. } => "stale",
            Freshness::Unknown { .. } => "unknown",
        }
    }

    /// The one line (or three) that says what the number is about.
    pub(crate) fn render(&self, corpus_id: &str) -> String {
        let mut s = String::new();
        match &self.freshness {
            Freshness::Current => {
                s.push_str("graph: at HEAD — the number is about this commit\n");
            }
            Freshness::BehindIrrelevant {
                indexed,
                head,
                gap_files,
            } => {
                s.push_str(&format!(
                    "graph: indexed {}, HEAD {} — {gap_files} file(s) in the gap, none of them \
                     {} source, so the number is still about this commit\n",
                    short(indexed),
                    short(head),
                    self.exts.join("/"),
                ));
            }
            Freshness::Stale {
                indexed,
                head,
                changed,
            } => {
                s.push_str(&format!(
                    "graph: indexed {}, HEAD {} — {} indexed-source file(s) changed in the gap, \
                     so this number is about {} and NOT about this commit:\n",
                    short(indexed),
                    short(head),
                    changed.len(),
                    short(indexed),
                ));
                for p in changed.iter().take(5) {
                    s.push_str(&format!("  {p}\n"));
                }
                if changed.len() > 5 {
                    s.push_str(&format!("  … and {} more\n", changed.len() - 5));
                }
                s.push_str(&format!(
                    "  re-index: svrn project refresh --name {corpus_id} --local\n"
                ));
            }
            Freshness::Unknown { why } => {
                s.push_str(&format!(
                    "graph: cannot tell what this number is about — {why}\n"
                ));
            }
        }
        if !self.uncommitted.is_empty() {
            s.push_str(&format!(
                "caveat: {} uncommitted {} file(s) are not in this count yet \
                 (a type you just wrote is invisible until the next index)\n",
                self.uncommitted.len(),
                self.exts.join("/"),
            ));
        }
        s
    }
}

pub(crate) fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

/// The extensions this count reads — derived from the graph, not declared.
fn counted_extensions(defs: &[corpus_engine_scip::converge::TypeDef]) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for d in defs {
        if let Some((_, ext)) = d.file.rsplit_once('.') {
            if !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                out.insert(ext.to_ascii_lowercase());
            }
        }
    }
    out.into_iter().collect()
}

fn git_stdout(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Paths git reports as dirty in the working tree, including untracked ones.
/// `--porcelain` v1: two status columns, a space, then the path; renames carry
/// `old -> new` and only the destination matters here.
fn working_tree_paths() -> Vec<String> {
    let Some(text) = git_stdout(&["status", "--porcelain"]) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| l.get(3..))
        .map(|p| p.rsplit(" -> ").next().unwrap_or(p).trim_matches('"'))
        .map(String::from)
        .collect()
}

/// Does this path feed the count? Pure on purpose: the predicate that decides
/// whether the gate cries wolf is the part most worth a test, and it must be
/// testable without a git checkout at a particular commit.
fn counts_path(path: &str, exts: &[String], scope: &SourceScope) -> bool {
    scope.admits(path)
        && path
            .rsplit_once('.')
            .is_some_and(|(_, e)| exts.iter().any(|x| x == &e.to_ascii_lowercase()))
}

pub(crate) fn assess_lag(
    indexed_head: Option<String>,
    defs: &[corpus_engine_scip::converge::TypeDef],
    scope: &SourceScope,
) -> Lag {
    let exts = counted_extensions(defs);
    let counted = |p: &str| counts_path(p, &exts, scope);
    let uncommitted: Vec<String> = working_tree_paths()
        .into_iter()
        .filter(|p| counted(p))
        .collect();

    let head = git_stdout(&["rev-parse", "HEAD"]).map(|s| s.trim().to_string());
    let freshness = match (indexed_head, head) {
        (None, _) => Freshness::Unknown {
            why: "the graph records no last_indexed_head (legacy DB) — re-index to get one"
                .to_string(),
        },
        (_, None) => Freshness::Unknown {
            why: "`git rev-parse HEAD` failed here — not a git checkout?".to_string(),
        },
        (Some(idx), Some(head)) if idx == head => Freshness::Current,
        (Some(idx), Some(head)) => {
            match git_stdout(&["diff", "--name-only", &format!("{idx}..{head}")]) {
                None => Freshness::Unknown {
                    why: format!(
                        "the graph's indexed head {} is not an object in this checkout, so what \
                         changed since it cannot be resolved",
                        short(&idx)
                    ),
                },
                Some(text) => {
                    let gap: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
                    let changed: Vec<String> = gap
                        .iter()
                        .filter(|p| counted(p))
                        .map(|p| (*p).to_string())
                        .collect();
                    if changed.is_empty() {
                        Freshness::BehindIrrelevant {
                            indexed: idx,
                            head,
                            gap_files: gap.len(),
                        }
                    } else {
                        Freshness::Stale {
                            indexed: idx,
                            head,
                            changed,
                        }
                    }
                }
            }
        }
    };

    Lag {
        freshness,
        uncommitted,
        exts,
    }
}

/// The ratchet. Exits 1 when the count rises — a duplicate was ADDED, which is
/// the failure the line-count arch-gate cannot catch.
fn cmd_status(
    defs: &[corpus_engine_scip::converge::TypeDef],
    baseline: &std::path::Path,
    mint: bool,
    json: bool,
    lag: &Lag,
    corpus_id: &str,
) -> i32 {
    let n = duplicate_count(defs);
    let prior: Option<usize> = std::fs::read_to_string(baseline)
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse().ok());

    if mint {
        // Minting from a graph that cannot speak for this commit freezes the
        // wrong number into the ratchet. Refuse and name the repair (§18.3).
        if !lag.can_judge() {
            eprint!("{}", lag.render(corpus_id));
            eprintln!(
                "refusing to mint: the graph cannot produce a number about this commit.\n\
                 Re-index, then mint."
            );
            return 3;
        }
        if let Some(parent) = baseline.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(baseline, format!("{n}\n")) {
            eprintln!("error: writing {}: {e}", baseline.display());
            return 1;
        }
        println!("minted {n} -> {}", baseline.display());
        return 0;
    }

    if json {
        let body: BTreeMap<&str, serde_json::Value> = [
            ("duplicated_names", serde_json::json!(n)),
            ("baseline", serde_json::json!(prior)),
            (
                "delta",
                serde_json::json!(prior.map(|p| n as i64 - p as i64)),
            ),
            // What the number is ABOUT travels with it — a count that travels
            // without its method is the brittleness this program exists to end.
            ("freshness", serde_json::json!(lag.verdict_word())),
            ("freshness_note", serde_json::json!(lag.render(corpus_id))),
            ("counted_extensions", serde_json::json!(lag.exts)),
            ("uncommitted_counted", serde_json::json!(lag.uncommitted)),
        ]
        .into_iter()
        .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_default()
        );
    }

    match prior {
        None => {
            if !json {
                println!("duplicated names: {n}");
                print!("{}", lag.render(corpus_id));
                println!(
                    "baseline: none — mint one with `svrn code converge status --mint`\n\
                     (four verdicts, not two: this is NEVER-RAN, not a pass)"
                );
            }
            // No baseline is not a pass. ARCH §18.2.
            4
        }
        Some(p) => {
            let delta = n as i64 - p as i64;
            if !json {
                println!("duplicated names: {n}");
                println!("baseline: {p}   delta: {delta:+}");
                print!("{}", lag.render(corpus_id));
            }
            if delta > 0 {
                // A rise is real wherever the graph stands: the duplicate is IN
                // the graph. Staleness cannot excuse it, only delay finding it.
                if !json {
                    println!(
                        "\nRATCHET BROKEN — {delta} concept(s) added. Either converge the new \
                         duplicate,\nor rename it apart and say which in the landing verdict."
                    );
                }
                1
            } else if lag.can_judge() {
                0
            } else {
                // Not a pass: the graph is clean at a commit that is not this
                // one. Four verdicts, not two (§18.2).
                if !json {
                    println!(
                        "\nCOULD-NOT-JUDGE — clean at the indexed commit, which is not this one."
                    );
                }
                3
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine_scip::converge::TypeDef;

    fn def(file: &str) -> TypeDef {
        TypeDef {
            name: "Verdict".to_string(),
            krate: "k".to_string(),
            file: file.to_string(),
            line: 1,
            qualified: format!("k/{file}#Verdict#"),
        }
    }

    #[test]
    fn counted_extensions_come_from_the_graph_not_a_constant() {
        let defs = [
            def("sovereign/crates/a/src/x.rs"),
            def("corpus-engine/src/y.rs"),
            def("commonwealth/tools/z.go"),
            def("scripts/w.py"),
        ];
        assert_eq!(counted_extensions(&defs), vec!["go", "py", "rs"]);
        // A graph with no Go in it must not claim Go changes can move its number.
        assert_eq!(counted_extensions(&[def("a/b.rs")]), vec!["rs"]);
    }

    /// The live specimen this predicate exists for (2026-08-20): the graph sat
    /// two commits behind HEAD and the whole gap was a campaign TOML and a
    /// python *script*. Refusing there would be crying wolf on a docs-only
    /// commit — and a gate that cries wolf gets switched off.
    #[test]
    fn a_docs_only_gap_does_not_feed_the_count() {
        let exts = vec!["rs".to_string()];
        let scope = SourceScope::default();
        assert!(!counts_path(
            "quality/campaigns/noun-convergence.toml",
            &exts,
            &scope
        ));
        assert!(!counts_path("scripts/nc-boundary.py", &exts, &scope));
        assert!(!counts_path("AGENTS.md", &exts, &scope));
    }

    /// …and the other arm, because an exclusion only ever watched go quiet is
    /// indistinguishable from a predicate that stopped working (§18.1).
    #[test]
    fn a_production_rust_change_does_feed_the_count() {
        let exts = vec!["rs".to_string()];
        let scope = SourceScope::default();
        assert!(counts_path(
            "sovereign/crates/sovereign-core/src/runtime/retrieval_pipeline.rs",
            &exts,
            &scope
        ));

        // The campaign's motivating file, and the reason the exclusion list
        // became segment-matched (2026-08-20): `"research/"` used to swallow
        // `deep_research/` as a substring, so `converge noun <X>` — the
        // pre-flight AGENTS.md now sends every agent to — answered "no such
        // concept" for the 166 type definitions in the very module whose five
        // privately re-derived nouns are why this program exists.
        assert!(counts_path(
            "sovereign/crates/sovereign-core/src/deep_research/icd.rs",
            &exts,
            &scope
        ));
        // Same extension, but out of the counted scope by construction — these
        // are the paths `type_defs` never counted in the first place.
        assert!(!counts_path(
            "sovereign/crates/a/tests/e2e.rs",
            &exts,
            &scope
        ));
        assert!(!counts_path("target/debug/build/x.rs", &exts, &scope));
        assert!(!counts_path(
            ".claude/worktrees/agent-1/sovereign/crates/a/src/x.rs",
            &exts,
            &scope
        ));
    }

    #[test]
    fn only_current_and_irrelevant_gaps_may_render_a_verdict() {
        let lag = |f| Lag {
            freshness: f,
            uncommitted: Vec::new(),
            exts: vec!["rs".to_string()],
        };
        assert!(lag(Freshness::Current).can_judge());
        assert!(lag(Freshness::BehindIrrelevant {
            indexed: "a".repeat(40),
            head: "b".repeat(40),
            gap_files: 2,
        })
        .can_judge());
        assert!(!lag(Freshness::Stale {
            indexed: "a".repeat(40),
            head: "b".repeat(40),
            changed: vec!["x.rs".to_string()],
        })
        .can_judge());
        assert!(!lag(Freshness::Unknown {
            why: "no git".to_string()
        })
        .can_judge());
    }

    /// The stale render must name the repair, or a red X is not self-serviceable.
    #[test]
    fn the_stale_line_names_the_reindex_command() {
        let lag = Lag {
            freshness: Freshness::Stale {
                indexed: "a".repeat(40),
                head: "b".repeat(40),
                changed: vec!["sovereign/crates/a/src/x.rs".to_string()],
            },
            uncommitted: vec!["sovereign/crates/a/src/y.rs".to_string()],
            exts: vec!["rs".to_string()],
        };
        let out = lag.render("commonwealth-ai");
        assert!(out.contains("svrn project refresh --name commonwealth-ai --local"));
        assert!(out.contains("NOT about this commit"));
        assert!(out.contains("caveat: 1 uncommitted"));
    }
}
