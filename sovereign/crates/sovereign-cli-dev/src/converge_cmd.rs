// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code converge` — the concept-convergence workhorse.
//!
//! Thin CLI wrapper over `corpus_engine_scip::converge`. Resolves the corpus
//! the same way `arch-report` / `suggest-seams` / `dry-report` do (explicit
//! `--corpus-id`, or the sole indexed code corpus), reads the SCIP graph
//! read-only, and prints. Never writes, never calls a model, never builds.
//!
//! Sibling verbs answer adjacent questions and this one defers to them
//! rather than reimplementing (ARCH §19): duplicated BEHAVIOUR — the same
//! CODE twice — is `dry-report`, oversized FILES are `suggest-seams`, crate
//! coupling is `arch-report`.
//!
//! This command owns duplicated IDENTITY (`census`/`noun`), duplicated SHAPE
//! (`shape`), and since 2026-08-31 duplicated INTENT (`verb`) — the same JOB
//! under a different name in a different crate, which identity and shape both
//! miss because neither the name nor the field set is shared. `verb` reads the
//! code-intel summaries rather than the SCIP graph; it is the one subcommand
//! here that needs the enrichment cache.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use corpus_engine_scip::converge::{
    census, crate_dag, cross_crate_reached, dossier, duplicate_count, render_census,
    render_dossier, type_defs, SourceScope,
};
use corpus_engine_scip::roles::{reach_index, render_roles, roles, type_fields};
use corpus_engine_scip::shape::{field_signatures, render_shape, shape_census, ShapeOptions};
use corpus_engine_scip::ScipGraph;

const HELP: &str = "\
svrn code converge <census|roles|shape|verb|noun|status> [options]

Duplicated concept IDENTITY over the SCIP graph. Read-only; no daemon, no
model, no build.

  census                  what NAME is duplicated, ranked
                          Counts a name only when >=2 crates each hold a
                          definition another crate ALREADY references — a
                          collision nothing outside the defining crate can
                          reach has nothing to import and adoption cannot
                          retire it. Both numbers are always printed.
    --local               list the rows the reachability filter set aside
    --kin                 also count morphological family (over-collects)
    --limit N             rows to print (0 = all; default 40)

  shape                   what SHAPE is duplicated — the renamed fork a name
                          census structurally cannot see (`ClaimCitation` and
                          `DrCitation` are one concept and share no name).
                          Matches on (field name, field type) sets, IDF-
                          weighted. NO type name is ever compared.
    --threshold F         report matches at/above this score (default 0.50)
    --min-shared N        shared keys a pair needs to be scored (default 3)
    --rare-df N           a shared key held by <=N types counts as rare; a
                          match with no rare key is not evidence (default 20)
    --min-fields N        types with fewer named fields are skipped (default 2)
    --names-only          drop field types from the key. Measured on this
                          workspace at 4f64bdb2: 947 pairs past the gates
                          instead of 669 — 42% MORE to adjudicate, same
                          recall on the positive control.
    --limit N             groups to print (0 = all; default 40)

  verb                    what JOB is duplicated — the reimplementation that
                          identity and shape both miss. `cosine_sim`,
                          `probe_cosine` and `cosine_similarity` share no name
                          and no field set; they share a one-line description
                          of what they do. Reads code-intel summaries, so it
                          needs `svrn enrich code-intel` to have run.
                          Different-name AND cross-crate is the whole filter:
                          measured 2026-08-31, the top 200 intent-similar
                          pairs were 100% same-name, i.e. already `dry-report`'s.
    --limit N             clusters to print (0 = all; default 40)

  roles                   what each ROLE costs and who reuses it — population
                          and adoption share per role, plus the three concept
                          families. A MIRROR, not a gate: no threshold, no
                          exit code, nothing to ratchet.
    --limit N             roles to print (0 = all; default 40)
    --min-population N    drop the one-off tail (default 3)

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

Four discovery feeds, and they do not overlap:
  converge census           duplicated NAME      (six `ChatMessage` structs)
  converge roles            duplicated ROLE      (`AuditReport`+`DriftReport`)
  converge shape            duplicated SHAPE     (`ClaimCitation`==`DrCitation`)
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
    let mut min_population: usize = 3;
    let mut limit: usize = 40;
    let mut kin = false;
    let mut json = false;
    let mut mint = false;
    let mut local = false;
    let mut names_only = false;
    let mut shape_opts = ShapeOptions::default();

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
            "--min-population" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(n) => min_population = n.max(1),
                    None => {
                        eprintln!("error: --min-population needs a number");
                        return 1;
                    }
                }
            }
            "--threshold" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<f64>().ok()) {
                    Some(v) => shape_opts.threshold = v,
                    None => {
                        eprintln!("error: --threshold requires a number");
                        return 1;
                    }
                }
            }
            "--min-shared" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(v) => shape_opts.min_shared = v.max(1),
                    None => {
                        eprintln!("error: --min-shared requires a number");
                        return 1;
                    }
                }
            }
            "--rare-df" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(v) => shape_opts.rare_df = v,
                    None => {
                        eprintln!("error: --rare-df requires a number");
                        return 1;
                    }
                }
            }
            "--min-fields" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(v) => shape_opts.min_fields = v.max(1),
                    None => {
                        eprintln!("error: --min-fields requires a number");
                        return 1;
                    }
                }
            }
            "--names-only" => names_only = true,
            "--local" => local = true,
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

    if sub == "verb" {
        // No SCIP load: this subcommand reads the enrichment cache, and the
        // graph costs 7-11s it would never touch.
        let opts = crate::intent::IntentOptions::default();
        let index_path = indexes_dir.join(&corpus_id);
        let symbols = match crate::intent::load_intent_corpus(&index_path, &opts) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        if symbols.is_empty() {
            eprintln!(
                "error: no code-intel summaries at {}/code_intel_cache.json — \
                 run `svrn enrich code-intel {corpus_id}` first",
                index_path.display()
            );
            return 1;
        }
        let clusters = crate::intent::intent_census(&symbols, &opts);
        eprintln!(
            "converge verb: {} symbols with usable intent text; settings {}",
            symbols.len(),
            opts.digest()
        );
        print!("{}", crate::intent::render_intent(&clusters, limit));
        return 0;
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

    // Every verb reads the ref table now. `census` and `status` joined `roles`
    // and `noun` on 2026-08-21, when the ratchet started counting only
    // collisions another crate can actually reach — that predicate IS a query
    // over references, so there is no version of the narrowing that does not
    // read them. Measured cost on this workspace: `census` 0.8s -> 5.0s. That
    // is one more table scan, not a second index pass, and a discovery feed
    // that answers in five seconds is still a discovery feed.
    let refs = match graph.iter_all_refs().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: reading refs: {e}");
            return 1;
        }
    };
    let reached = cross_crate_reached(&defs, &refs, &scope);

    match sub.as_str() {
        "census" => {
            let c = census(&defs, &reached, &scope, kin);
            if json {
                println!("{}", serde_json::to_string_pretty(&c).unwrap_or_default());
            } else {
                print!("{}", render_census(&c, limit, kin, local));
            }
            0
        }
        "shape" => {
            // `--names-only` is the arm that made carrying field types worth
            // it, so it stays reachable rather than being an argument in a
            // commit message: pass no refs and the signature is names alone.
            let sigs = if names_only {
                field_signatures(&symbols, &[], &scope)
            } else {
                field_signatures(&symbols, &refs, &scope)
            };
            let c = shape_census(&symbols, &sigs, &scope, &shape_opts);
            if json {
                println!("{}", serde_json::to_string_pretty(&c).unwrap_or_default());
            } else {
                match graph.last_indexed_head().await {
                    Some(h) => println!("graph: {corpus_id} @ {h}\n"),
                    None => println!("graph: {corpus_id} @ unknown commit\n"),
                }
                print!("{}", render_shape(&c, limit));
            }
            0
        }
        "roles" => {
            let fields = type_fields(&symbols, &scope);
            let reach = reach_index(&defs, &refs, &scope);
            let c = roles(&defs, &fields, &reach, &scope, min_population);
            if json {
                println!("{}", serde_json::to_string_pretty(&c).unwrap_or_default());
            } else {
                // The graph, not the working tree, is what these numbers are
                // about — say which commit before saying the number.
                match graph.last_indexed_head().await {
                    Some(h) => println!("graph: {corpus_id} @ {h}\n"),
                    None => println!("graph: {corpus_id} @ unknown commit\n"),
                }
                print!("{}", render_roles(&c, limit));
            }
            0
        }
        "noun" => {
            let Some(name) = noun else {
                eprintln!("error: `converge noun` requires a name, e.g. `converge noun Verdict`");
                return 1;
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
            // scope that produced `defs`, so the graph_lag line and the number
            // can never be about different things.
            let lag = assess_lag(graph.last_indexed_head().await, &defs, &scope);
            cmd_status(
                &defs, &reached, &scope, &baseline, mint, json, &lag, &corpus_id,
            )
        }
        other => {
            eprintln!("error: unknown converge subcommand `{other}`");
            println!("{HELP}");
            1
        }
    }
}

// ── Which code corpus? One decider (ARCH §10.6) ──────────────────────────────
//
// Six commands answered this question with six copies of one block, and every
// copy carried the same defect: "the sole indexed code corpus" is a default
// that expires the moment anyone indexes a second one. On 2026-08-29 this host
// carried three (commonwealth-ai plus two scratchpad fixtures) and the relayed
// `converge status` began refusing — which surfaced as concept-gate reporting
// COULD-NOT-JUDGE and blaming a stale sibling binary, sending the reader to a
// four-crate rebuild that could not help. Nothing was stale; the QUESTION was
// ambiguous. `concept_gate.rs` patched itself by naming the corpus; the other
// five callers, and every interactive invocation, were left refusing.
//
// So when the answer is ambiguous, ASK THE REPO. A corpus records the tree it
// was built from in `_corpus_meta.json.source_path`, and the caller is standing
// in a git worktree. Exactly one match resolves it, and the resolution is
// ANNOUNCED rather than assumed (§18.3 — never silently substitute). Anything
// else still refuses, but now names the root it tried and every candidate's
// source, so the message can no longer point at the wrong cause.

/// A code corpus under `indexes_dir`: its id, and the tree it was built from.
pub(crate) fn code_corpora(
    indexes_dir: &std::path::Path,
) -> Vec<(String, Option<std::path::PathBuf>)> {
    let mut v: Vec<(String, Option<std::path::PathBuf>)> = std::fs::read_dir(indexes_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().join("scip_graph.db").exists())
                .filter_map(|e| {
                    let id = e.file_name().to_str()?.to_string();
                    let src = std::fs::read_to_string(e.path().join("_corpus_meta.json"))
                        .ok()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                        .and_then(|v| v.get("source_path")?.as_str().map(std::path::PathBuf::from));
                    Some((id, src))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The git worktree containing `start`, if any. `.git` is a FILE in a linked
/// worktree, so this tests existence rather than directory-ness.
pub(crate) fn git_root_of(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut p = Some(start);
    while let Some(dir) = p {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        p = dir.parent();
    }
    None
}

/// The choice itself — pure: no filesystem, no cwd, no process. `Ok((id, note))`
/// where `note` is the glassbox line for a resolution that was not trivial.
pub(crate) fn pick_corpus(
    explicit: Option<String>,
    candidates: &[(String, Option<std::path::PathBuf>)],
    root: Option<&std::path::Path>,
    indexes_dir: &std::path::Path,
) -> Result<(String, Option<String>), String> {
    if let Some(c) = explicit {
        return Ok((c, None));
    }
    let ambiguous = || {
        let listed = candidates
            .iter()
            .map(|(id, src)| match src {
                Some(p) => format!("  {id}  (built from {})", p.display()),
                None => format!("  {id}  (no source_path recorded)"),
            })
            .collect::<Vec<_>>()
            .join("\n");
        let tried = match root {
            Some(r) => format!("none of them was built from this repo ({})", r.display()),
            None => "and the working directory is not inside a git repository".to_string(),
        };
        format!(
            "error: {} indexed code corpora, {tried} — pass --corpus-id:\n{listed}",
            candidates.len()
        )
    };
    match candidates.len() {
        0 => Err(format!(
            "error: no code corpus under {} — run `svrn project init` first",
            indexes_dir.display()
        )),
        1 => Ok((candidates[0].0.clone(), None)),
        _ => {
            let root = root.ok_or_else(ambiguous)?;
            let hits: Vec<&String> = candidates
                .iter()
                .filter(|(_, src)| src.as_deref() == Some(root))
                .map(|(id, _)| id)
                .collect();
            match hits.len() {
                1 => {
                    let id = hits[0].clone();
                    let note = format!(
                        "corpus: {id} (of {} indexed — the one built from {})",
                        candidates.len(),
                        root.display()
                    );
                    Ok((id, Some(note)))
                }
                _ => Err(ambiguous()),
            }
        }
    }
}

// `pub(crate)` since rf-1: `refactor_cmd::schedule` resolves the corpus the
// same way rather than growing a fourth copy of this lookup (ARCH §10.6).
pub(crate) fn resolve_corpus(
    explicit: Option<String>,
    indexes_dir: &std::path::Path,
) -> Result<String, i32> {
    let candidates = code_corpora(indexes_dir);
    let root = std::env::current_dir().ok().and_then(|d| git_root_of(&d));
    match pick_corpus(explicit, &candidates, root.as_deref(), indexes_dir) {
        Ok((id, note)) => {
            if let Some(n) = note {
                eprintln!("{n}");
            }
            Ok(id)
        }
        Err(msg) => {
            eprintln!("{msg}");
            Err(1)
        }
    }
}

// ── GraphLag: what the number is ABOUT ───────────────────────────────────────
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
///
/// RENAMED APART from `Freshness` on 2026-08-20, when nc-10-judgement minted
/// `kernel_types::Freshness` and the two collided. They are not the same
/// question and neither can serve the other: the kernel's is *is this dated
/// artifact past its caller's horizon*, three variants, no git; this one is
/// *is the SCIP graph's head the commit being gated, and if not does the gap
/// touch the files this count reads*, four variants carrying commit hashes
/// and changed-file lists. A prefix would have been the cosmetic dodge
/// §10.8 warns about ("a gate teaches the workaround"); `GraphLag` says what
/// it measures, and the enclosing `Lag` keeps the uncommitted-caveat half.
pub(crate) enum GraphLag {
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
    pub(crate) graph_lag: GraphLag,
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
            self.graph_lag,
            GraphLag::Current | GraphLag::BehindIrrelevant { .. }
        )
    }

    pub(crate) fn verdict_word(&self) -> &'static str {
        match self.graph_lag {
            GraphLag::Current => "current",
            GraphLag::BehindIrrelevant { .. } => "behind-irrelevant",
            GraphLag::Stale { .. } => "stale",
            GraphLag::Unknown { .. } => "unknown",
        }
    }

    /// The one line (or three) that says what the number is about.
    pub(crate) fn render(&self, corpus_id: &str) -> String {
        let mut s = String::new();
        match &self.graph_lag {
            GraphLag::Current => {
                s.push_str("graph: at HEAD — the number is about this commit\n");
            }
            GraphLag::BehindIrrelevant {
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
            GraphLag::Stale {
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
            GraphLag::Unknown { why } => {
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
    let graph_lag = match (indexed_head, head) {
        (None, _) => GraphLag::Unknown {
            why: "the graph records no last_indexed_head (legacy DB) — re-index to get one"
                .to_string(),
        },
        (_, None) => GraphLag::Unknown {
            why: "`git rev-parse HEAD` failed here — not a git checkout?".to_string(),
        },
        (Some(idx), Some(head)) if idx == head => GraphLag::Current,
        (Some(idx), Some(head)) => {
            match git_stdout(&["diff", "--name-only", &format!("{idx}..{head}")]) {
                None => GraphLag::Unknown {
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
                        GraphLag::BehindIrrelevant {
                            indexed: idx,
                            head,
                            gap_files: gap.len(),
                        }
                    } else {
                        GraphLag::Stale {
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
        graph_lag,
        uncommitted,
        exts,
    }
}

/// The ratchet. Exits 1 when the count rises — a duplicate was ADDED, which is
/// the failure the line-count arch-gate cannot catch.
#[allow(clippy::too_many_arguments)]
fn cmd_status(
    defs: &[corpus_engine_scip::converge::TypeDef],
    reached: &BTreeSet<String>,
    scope: &SourceScope,
    baseline: &std::path::Path,
    mint: bool,
    json: bool,
    lag: &Lag,
    corpus_id: &str,
) -> i32 {
    let n = duplicate_count(defs, reached, scope);
    // The wider number travels with the narrow one, always. The ratchet counts
    // only collisions another crate can reach; a reader who sees `34` without
    // `265` beside it cannot tell a narrowing from an improvement (§18.6).
    let colliding = census(defs, reached, scope, false).colliding_names;
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
        println!(
            "minted {n} -> {}   ({colliding} names collide; {n} of them are reachable \
             from another crate and countable)",
            baseline.display()
        );
        return 0;
    }

    if json {
        let body: BTreeMap<&str, serde_json::Value> = [
            ("duplicated_names", serde_json::json!(n)),
            // Every colliding name, reachable or not — the population the
            // ratchet number is drawn from.
            ("colliding_names", serde_json::json!(colliding)),
            ("baseline", serde_json::json!(prior)),
            (
                "delta",
                serde_json::json!(prior.map(|p| n as i64 - p as i64)),
            ),
            // What the number is ABOUT travels with it — a count that travels
            // without its method is the brittleness this program exists to end.
            ("graph_lag", serde_json::json!(lag.verdict_word())),
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
                println!(
                    "duplicated names: {n} of {colliding} colliding (the rest are \
                          unreachable from any other crate)"
                );
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
                println!(
                    "duplicated names: {n} of {colliding} colliding (the rest are \
                          unreachable from any other crate)"
                );
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
            graph_lag: f,
            uncommitted: Vec::new(),
            exts: vec!["rs".to_string()],
        };
        assert!(lag(GraphLag::Current).can_judge());
        assert!(lag(GraphLag::BehindIrrelevant {
            indexed: "a".repeat(40),
            head: "b".repeat(40),
            gap_files: 2,
        })
        .can_judge());
        assert!(!lag(GraphLag::Stale {
            indexed: "a".repeat(40),
            head: "b".repeat(40),
            changed: vec!["x.rs".to_string()],
        })
        .can_judge());
        assert!(!lag(GraphLag::Unknown {
            why: "no git".to_string()
        })
        .can_judge());
    }

    /// The stale render must name the repair, or a red X is not self-serviceable.
    #[test]
    fn the_stale_line_names_the_reindex_command() {
        let lag = Lag {
            graph_lag: GraphLag::Stale {
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

    // ── Which corpus? ────────────────────────────────────────────────────────
    // The planted failure these exist for: a second indexed corpus used to make
    // the question unanswerable, and the refusal was reported by concept-gate as
    // a stale sibling binary. `two_code_corpora_resolve_by_repo_root` fails on
    // the pre-2026-08-30 resolver, which counted candidates and gave up at two.

    fn cand(id: &str, src: Option<&str>) -> (String, Option<std::path::PathBuf>) {
        (id.to_string(), src.map(std::path::PathBuf::from))
    }

    #[test]
    fn two_code_corpora_resolve_by_repo_root() {
        let c = [
            cand("commonwealth-ai", Some("/home/u/dev/commonwealth-ai")),
            cand("semver", Some("/tmp/scratch/semver")),
        ];
        let root = std::path::Path::new("/home/u/dev/commonwealth-ai");
        let (id, note) =
            pick_corpus(None, &c, Some(root), std::path::Path::new("/idx")).expect("resolvable");
        assert_eq!(id, "commonwealth-ai");
        // The resolution is announced, never assumed (ARCH §18.3).
        assert!(note.expect("a note").contains("commonwealth-ai"));
    }

    #[test]
    fn an_explicit_corpus_id_is_never_second_guessed() {
        let c = [cand("a", Some("/x")), cand("b", Some("/y"))];
        let (id, note) =
            pick_corpus(Some("b".into()), &c, None, std::path::Path::new("/idx")).unwrap();
        assert_eq!(id, "b");
        assert!(note.is_none(), "an explicit choice needs no explanation");
    }

    #[test]
    fn a_sole_corpus_resolves_without_a_repo() {
        let c = [cand("only", None)];
        let (id, _) = pick_corpus(None, &c, None, std::path::Path::new("/idx")).unwrap();
        assert_eq!(id, "only");
    }

    #[test]
    fn ambiguity_the_root_cannot_break_still_refuses_and_names_what_it_tried() {
        let c = [
            cand("one", Some("/home/u/repo")),
            cand("two", Some("/home/u/repo")),
        ];
        let root = std::path::Path::new("/home/u/repo");
        let err = pick_corpus(None, &c, Some(root), std::path::Path::new("/idx"))
            .expect_err("two corpora built from the same tree is not resolvable");
        // The old message named neither the root nor the candidates' sources,
        // which is how it sent a reader to rebuild a sibling that was fine.
        assert!(
            err.contains("/home/u/repo"),
            "names the root it tried: {err}"
        );
        assert!(
            err.contains("one") && err.contains("two"),
            "lists candidates: {err}"
        );
        assert!(err.contains("--corpus-id"), "names the repair: {err}");
    }

    #[test]
    fn outside_a_git_repo_the_refusal_says_so() {
        let c = [cand("a", Some("/x")), cand("b", Some("/y"))];
        let err = pick_corpus(None, &c, None, std::path::Path::new("/idx")).unwrap_err();
        assert!(err.contains("not inside a git repository"), "{err}");
    }

    #[test]
    fn no_corpus_at_all_points_at_project_init() {
        let err = pick_corpus(None, &[], None, std::path::Path::new("/idx")).unwrap_err();
        assert!(
            err.contains("no code corpus") && err.contains("project init"),
            "{err}"
        );
    }
}
