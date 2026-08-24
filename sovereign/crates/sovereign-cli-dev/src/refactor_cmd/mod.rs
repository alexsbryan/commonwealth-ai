// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code refactor` — the refactor factory's read-only front half.
//!
//! Stages 1-2 of `quality/REFACTOR_FACTORY.md` plus the per-item entry gate:
//!
//!   svrn code refactor plan <spec.toml>
//!       Entry-gate one spec (representation · wire+fixture · trait surface ·
//!       fallibility), then DISCOVER (apply the seed edit, run `cargo check
//!       --message-format=json`, restore) and CLASSIFY (group diagnostics by
//!       `(code, expected, found, syntactic context)`, deterministically —
//!       never a model, ARCH §7.6). Prints error classes with counts and
//!       per-class site lists. The working tree is restored before exit; the
//!       command APPLIES NOTHING.
//!
//!   svrn code refactor gate
//!       The per-item entry gate over all five work-table kinds (field atoms,
//!       duplicate shapes, duplicate names, duplicate behaviour, hand-rolled
//!       arg loops), emitting a verdict per item and the ranked schedule —
//!       largest reach per session-chunk first. Refusals are printed AS
//!       refusals with reasons, never as absences (ARCH §18.3).
//!
//! `prepare` and `apply` are later rungs (rf-3, rf-4) and do not exist here.

mod affinity;
mod census;
mod classify;
mod destination;
mod detector;
mod discover;
mod gate;
mod label_model;
mod labels;
mod ledger;
mod order;
mod reduction;
mod schedule;
mod spec;

const HELP: &str = "\
svrn code refactor <plan|gate> [options]

The refactor factory, read-only half (quality/REFACTOR_FACTORY.md).

  plan <spec.toml>        entry-gate the spec, then discover + classify:
                          seed edit -> cargo check --message-format=json ->
                          deterministic error classes. Tree restored on exit.
    --crate <name>        scope the seed + check to one workspace crate
    --sites N             sites listed per class (default 5, 0 = all)
    --skip-fixture        do not run the wire fixture test
    --skip-baseline       skip the unseeded baseline check (trust the tree)
    --json                machine output

  gate                    entry gate over all five work-table kinds; prints
                          per-item verdicts and the ranked schedule.
    --corpus-id <id>      SCIP corpus (default: the sole indexed code corpus)
    --limit N             items listed per kind (default 10, 0 = all)
    --json                machine output

  status                  the burn-down: open holdings per destination, and
                          every detector's control verdict. A detector whose
                          control went quiet contributes NOTHING — its sites
                          are not counted and not reported as zero.
    --corpus-id <id>      SCIP corpus (default: the sole indexed code corpus)
    --all                 also run detectors that exceed the close budget
                          (behaviour's near tier: measured 156s)
    --unlabelled N        also list N unlabelled site keys — what `label`
                          consumes. 0 (default) lists none.
    --json                machine output

  label <detector> <key> <disp> <dest> <why>
                          append one judgement to the label store.
                          disp: converge|distinct|idiom|external-mirror|
                                layered|leave|UNSURE
  label --from-register   adjudicate every name site the concept register
                          already settles. Composite dispositions are skipped
                          and named, never flattened.
    --dry-run             report what it would write, write nothing
  affinity <dest> --describes '<what it is for>'
                          destination-first: what in this codebase does the
                          same job as <dest>, whatever it is called. Needs the
                          per-symbol behaviour descriptions; refuses with the
                          remedy named when they do not exist.
    --limit N             shortlist size (default 30)

  label --model           the model-assisted pass (local daemon, no external
                          tokens). Refuses to write until it has been scored.
    --groups              list the questions + their frozen dev/test split
    --score <gold.jsonl>  run and score against a hand-adjudicated gold file
    --dev / --test        restrict to one half of the split
    --model-alias <a>     daemon model alias (default: fast)

  next                    cut the largest file-disjoint batch of converge-
                          labelled sites, lock its files, render the order.
    --batch N             sites per order (default 25, a declared estimate)
    --corpus-id <id>      SCIP corpus

  close <order-id>        re-run the detector and report what it can no longer
                          see. The worker does not mark anything done.
    --corpus-id <id>      SCIP corpus

  reduction               the campaign scorecard: net lines against the
                          merge-base, and the new public surface a change
                          added instead of converging. Both bars in one
                          verb because they answer one question.
    --base <ref>          merge-base counterpart (default: main)
    --allow <name>        a public item the spec declares (repeatable)
    --max-net <n>         fail if net lines exceed n (default: report only —
                          newtype work is additive by construction)

Read-only except `label` (appends one line), `next` (writes an order + locks)
and `close` (releases them). prepare/apply are rf-3/rf-4 and are not built yet.
";

pub(crate) async fn run(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        eprint!("{HELP}");
        return i32::from(args.is_empty());
    }
    match args[0].as_str() {
        "plan" => plan_cmd(&args[1..]).await,
        "gate" => schedule::run_gate(&args[1..]).await,
        "status" => status_cmd(&args[1..]).await,
        "next" => next_cmd(&args[1..]).await,
        "close" => close_cmd(&args[1..]).await,
        "label" => label_cmd(&args[1..]).await,
        "affinity" => affinity_cmd(&args[1..]).await,
        "reduction" => reduction_cmd(&args[1..]),
        other => {
            eprintln!("error: unknown refactor subcommand '{other}'");
            eprint!("{HELP}");
            1
        }
    }
}

/// `svrn code refactor reduction` — did this branch converge, or did it add?
///
/// Exit 0 only on PASSED. FAILED and COULD-NOT-JUDGE both exit non-zero, and
/// they are distinct because "the branch grew" and "I could not resolve a
/// base" are different facts and collapsing them would hide the second
/// (ARCH §18.1).
fn reduction_cmd(args: &[String]) -> i32 {
    let mut base = "main".to_string();
    let mut allowed: Vec<String> = Vec::new();
    let mut max_net: Option<i64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--base" => {
                if let Some(v) = args.get(i + 1) {
                    base = v.clone();
                    i += 1;
                }
            }
            "--allow" => {
                if let Some(v) = args.get(i + 1) {
                    allowed.push(v.clone());
                    i += 1;
                }
            }
            other if other.starts_with("--base=") => base = other[7..].to_string(),
            "--max-net" => {
                if let Some(v) = args.get(i + 1) {
                    max_net = v.parse().ok();
                    i += 1;
                }
            }
            other if other.starts_with("--allow=") => allowed.push(other[8..].to_string()),
            other if other.starts_with("--max-net=") => max_net = other[10..].parse().ok(),
            _ => {}
        }
        i += 1;
    }
    let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let report = reduction::measure(&root, &base, &allowed, max_net);
    print!("{}", report.render(&allowed));
    match report.verdict {
        reduction::Verdict::Passed => 0,
        reduction::Verdict::Failed(_) => 1,
        reduction::Verdict::CouldNotJudge(_) => 3,
    }
}

/// `svrn code refactor status` — the burn-down.
///
/// Read-only and writes nothing. The number it prints is a MEASUREMENT taken
/// now, not a stored tally, which is why no agent can move it.
async fn status_cmd(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut json = false;
    let mut include_expensive = false;
    let mut unlabelled_cap = 0usize;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus-id" => {
                i += 1;
                match args.get(i) {
                    Some(v) => corpus_id = Some(v.clone()),
                    None => {
                        eprintln!("error: --corpus-id requires a value");
                        return 1;
                    }
                }
            }
            "--json" => json = true,
            "--all" => include_expensive = true,
            "--unlabelled" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(n) => unlabelled_cap = n,
                    None => {
                        eprintln!("error: --unlabelled requires an integer");
                        return 1;
                    }
                }
            }
            "-h" | "--help" => {
                eprint!("{HELP}");
                return 0;
            }
            other => {
                eprintln!("error: unknown flag {other}");
                return 1;
            }
        }
        i += 1;
    }

    let root = match census::repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 3;
        }
    };
    let indexes_dir = sovereign_cli_shared::dirs::sovereign_root().join("indexes");
    let corpus = match crate::converge_cmd::resolve_corpus(corpus_id, &indexes_dir) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let index_path = indexes_dir.join(&corpus);

    let ledger = match ledger::build(&root, &index_path, &corpus, include_expensive).await {
        Ok(l) => l,
        Err(e) => {
            // The instrument failed. That is COULD-NOT-JUDGE, and it exits 3 —
            // never 0 with an empty report (ARCH §18.3).
            eprintln!("error: could not judge — {e}");
            return 3;
        }
    };

    // The register is surveyed beside the burn-down, not after it: `dest` on
    // every open holding is a canonical copied from `quality/CONCEPTS.toml`,
    // so a canonical that resolves nowhere makes those rows unactionable no
    // matter how healthy the detectors are.
    let register = match destination::RegisterHealth::survey(&root) {
        Ok(h) => Some(h),
        Err(e) => {
            // Could-not-judge, named — never a silent zero (ARCH §18.3).
            eprintln!("warning: register destinations could not be surveyed — {e}");
            None
        }
    };

    let unlabelled_keys: Vec<serde_json::Value> = ledger
        .slices
        .iter()
        .filter(|s| s.is_live())
        .flat_map(|s| s.holdings.iter())
        .filter(|h| h.label.is_none())
        .take(unlabelled_cap)
        .map(|h| {
            serde_json::json!({
                "key": h.site.key(),
                "detector": h.site.detector.as_str(),
                "file": h.site.file,
                "line": h.site.line,
                "note": h.site.note,
            })
        })
        .collect();

    if json {
        let by_dest = ledger.by_destination();
        let payload = serde_json::json!({
            "graph_commit": ledger.graph_commit,
            "open": ledger.open(),
            "orphaned_labels": ledger.orphans,
            "orphaned_label_count": ledger.orphans.len(),
            "malformed_label_lines": ledger.malformed,
            "shard_collisions": ledger.collisions,
            "by_destination": by_dest,
            "register": register.as_ref().map(destination::RegisterHealth::json),
            "unlabelled_keys": unlabelled_keys,
            "detectors": ledger.slices.iter().map(|s| serde_json::json!({
                "id": s.id.as_str(),
                "live": s.is_live(),
                "verdict": s.control.verdict().to_string(),
                "reason": s.control.reason().as_str(),
                "settings_digest": s.settings_digest,
                "sites": s.holdings.len(),
            })).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else {
        print!("{}", ledger::render(&ledger));
        if let Some(h) = &register {
            print!("{}", h.render());
        }
        if !unlabelled_keys.is_empty() {
            println!("\n UNLABELLED (first {})", unlabelled_keys.len());
            for k in &unlabelled_keys {
                println!(
                    "   {}\n     {}:{}",
                    k["key"].as_str().unwrap_or_default(),
                    k["file"].as_str().unwrap_or_default(),
                    k["line"]
                );
            }
        }
    }
    0
}

/// `svrn code refactor affinity` — destination-first audit.
///
/// The five detectors ask "what is duplicated". This asks the question an
/// operator actually has: *here is an abstraction — what in this codebase does
/// the same job, whatever it is called?* Different names and different bodies
/// are exactly the case the other five miss.
async fn affinity_cmd(args: &[String]) -> i32 {
    let mut destination: Option<String> = None;
    let mut description: Option<String> = None;
    let mut limit = 30usize;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--describes" => {
                i += 1;
                match args.get(i) {
                    Some(v) => description = Some(v.clone()),
                    None => {
                        eprintln!("error: --describes requires a sentence");
                        return 1;
                    }
                }
            }
            "--limit" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(n) if n > 0 => limit = n,
                    _ => {
                        eprintln!("error: --limit requires a positive integer");
                        return 1;
                    }
                }
            }
            "-h" | "--help" => {
                eprint!("{HELP}");
                return 0;
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag {flag}");
                return 1;
            }
            positional => destination = Some(positional.to_string()),
        }
        i += 1;
    }
    let Some(destination) = destination else {
        eprintln!(
            "error: affinity needs a destination, e.g.\n  \
             svrn code refactor affinity kernel_types::Verdict \\\n    \
             --describes 'the outcome of any check: passed, failed, could-not-judge, never-ran'"
        );
        return 1;
    };
    let Some(description) = description else {
        // The description IS the query. Without it there is nothing to match
        // behaviour against, and guessing one from the type name would match
        // spelling — the exact failure this command refuses to make.
        eprintln!(
            "error: affinity needs --describes '<what the abstraction is for>'.\n\
             The description is the query: matching on the type NAME alone would find \n\
             things spelled like it, which is what `converge noun` already does."
        );
        return 1;
    };

    let (_root, index_path, _corpus) = match resolve_workspace(None) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let summaries = match affinity::load_summaries(&index_path) {
        Ok(s) => s,
        Err(u) => {
            print!("{}", affinity::render_unavailable(&u));
            return 3;
        }
    };
    let hits = affinity::shortlist(&summaries, &description, limit);
    println!(
        "affinity audit — {} described symbols, {} shortlisted for `{destination}`",
        summaries.len(),
        hits.len()
    );
    println!("  the shortlist is a LEXICAL PREFILTER over behaviour descriptions, not a verdict;");
    println!("  adjudication is the model's job and is the next rung.\n");
    for c in &hits {
        println!("  {:>6.2}  {}", c.score, c.qualified_name);
        println!("          {}:{}", c.file, c.line);
        println!("          {}", c.summary);
    }
    0
}

/// The model-assisted pass, and the harness that decides whether to believe it.
///
/// Three modes, and the order is the methodology:
///   --groups        list the questions (and their dev/test split) so the seat
///                   can adjudicate a gold file before any model runs
///   --score <gold>  run the model and score it against that gold, per split
///   (neither)       run it for real, writing labels — REFUSED unless a scored
///                   run has been recorded, because an unvalidated classifier
///                   writing into the ledger is the exact green-that-is-not-real
///                   this whole system exists to refuse
async fn label_model_cmd(args: &[String]) -> i32 {
    let mut list_groups = false;
    let mut gold_path: Option<String> = None;
    let mut model = label_model::MODEL_ALIAS.to_string();
    let mut only_split: Option<label_model::Split> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--groups" => list_groups = true,
            "--score" => {
                i += 1;
                match args.get(i) {
                    Some(v) => gold_path = Some(v.clone()),
                    None => {
                        eprintln!("error: --score requires a gold jsonl path");
                        return 1;
                    }
                }
            }
            "--model-alias" => {
                i += 1;
                match args.get(i) {
                    Some(v) => model = v.clone(),
                    None => {
                        eprintln!("error: --model-alias requires a value");
                        return 1;
                    }
                }
            }
            "--dev" => only_split = Some(label_model::Split::Dev),
            "--test" => only_split = Some(label_model::Split::Test),
            other => {
                eprintln!("error: unknown flag {other}");
                return 1;
            }
        }
        i += 1;
    }

    let (root, index_path, corpus) = match resolve_workspace(None) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let graph = match ledger::load_graph(&index_path, &corpus).await {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: could not judge — {e}");
            return 3;
        }
    };
    let ctx = graph.ctx(&root, &index_path, &corpus);
    let report = match detector::Detector::fire(&detector::NameDetector, &ctx).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: could not judge — {e}");
            return 3;
        }
    };
    if !report.is_live() {
        eprintln!(
            "refused: the name detector's control is silent — {}",
            report.control.reason().as_str()
        );
        return 3;
    }

    let mut groups = label_model::group_by_name(&report.sites);
    if let Some(want) = only_split {
        groups.retain(|g| label_model::split_of(&g.name) == want);
    }

    if list_groups {
        println!("{} groups ({} sites)", groups.len(), report.sites.len());
        println!("  split is a hash of the name — it cannot be reshuffled\n");
        for g in &groups {
            let s = match label_model::split_of(&g.name) {
                label_model::Split::Dev => "dev ",
                label_model::Split::Test => "TEST",
            };
            println!("  {s}  {:<28} {} defs", g.name, g.sites.len());
            for site in &g.sites {
                println!("           {}:{}", site.file, site.line);
            }
        }
        return 0;
    }

    let Some(gold_path) = gold_path else {
        // Refuse rather than run unvalidated. Naming what is missing, not
        // failing silently (ARCH §18.3).
        eprintln!(
            "refused: this pass writes judgements into the ledger, so it does not run \n\
             before it has been scored.\n\n\
             1. `label --model --groups` to see the questions and their split\n\
             2. adjudicate a gold jsonl by hand (same shape as a label file)\n\
             3. `label --model --score <gold> --dev` to tune\n\
             4. `label --model --score <gold> --test` once, to report"
        );
        return 2;
    };

    let gold = match label_model::load_gold(std::path::Path::new(&gold_path)) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return 3;
        }
    };
    // Only ask about groups the gold actually adjudicates.
    groups.retain(|g| gold.contains_key(&g.name));
    if groups.is_empty() {
        eprintln!("refused: gold file adjudicates none of the current groups");
        return 2;
    }

    // The per-symbol descriptions the enrichment pass produced. Without them
    // the model compares SOURCE, and two forked copies of one concept usually
    // differ in source while agreeing in purpose — which is the judgement being
    // asked for. Coverage is printed rather than assumed: a prompt silently
    // degraded to source-only is indistinguishable from a good one downstream.
    let sums = match affinity::load_summaries(&index_path) {
        Ok(v) => {
            let idx = label_model::index_summaries(&v);
            println!(
                "descriptions: {} symbols enriched, {} usable",
                v.len(),
                idx.len()
            );
            idx
        }
        Err(u) => {
            println!("descriptions: NONE — {}", u.reason);
            println!("             remedy: {}", u.remedy);
            label_model::Summaries::new()
        }
    };
    let covered: usize = groups
        .iter()
        .map(|g| label_model::summary_coverage(g, &sums).0)
        .sum();
    let total: usize = groups.iter().map(|g| g.sites.len()).sum();
    println!(
        "             {covered}/{total} of the sites under test carry one"
    );

    println!(
        "scoring {} group(s) against {} — model `{model}`, local daemon, no external tokens",
        groups.len(),
        gold_path
    );
    // Worked examples, minus any whose name is a question in this run.
    let under_test: std::collections::BTreeSet<String> =
        groups.iter().map(|g| g.name.clone()).collect();
    let shots = label_model::shots_for(&under_test);
    println!(
        "worked examples: {} of {} (any colliding with a scored group is dropped)",
        shots.len(),
        label_model::SHOTS.len()
    );
    let answers = label_model::run_groups(
        &root,
        "http://localhost:9741",
        &model,
        &groups,
        &sums,
        &shots,
        true,
    )
    .await;

    for split in [label_model::Split::Dev, label_model::Split::Test] {
        if only_split.is_some_and(|s| s != split) {
            continue;
        }
        let subset: std::collections::BTreeMap<String, super::refactor_cmd::labels::Disposition> =
            gold.iter()
                .filter(|(n, _)| label_model::split_of(n) == split)
                .map(|(n, d)| (n.clone(), *d))
                .collect();
        if subset.is_empty() {
            continue;
        }
        let s = label_model::score(&answers, &subset);
        print!("{}", label_model::render_score(split, &s));
    }
    0
}

/// The deterministic labelling pass: adjudicate from what is already declared.
///
/// `quality/CONCEPTS.toml` settles, for 31 nouns, the two things a label needs
/// — what to do with the twins, and which path owns the survivor. Asking a
/// model to re-derive that would mint a second decider for a question already
/// answered in a reviewed file (ARCH §10.6). So the pass reads the register,
/// and the model is only ever needed for what the register does not carry.
///
/// A register row whose disposition is a composite human sentence is SKIPPED
/// and named, never flattened to `converge`.
async fn label_from_register(args: &[String]) -> i32 {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let (root, index_path, corpus) = match resolve_workspace(None) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let register = match labels::load_register(&root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 3;
        }
    };
    let graph = match ledger::load_graph(&index_path, &corpus).await {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: could not judge — {e}");
            return 3;
        }
    };
    let ctx = graph.ctx(&root, &index_path, &corpus);

    // The name detector is the one whose token IS a register noun.
    let name_detector = detector::NameDetector;
    let report = match detector::Detector::fire(&name_detector, &ctx).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: could not judge — {e}");
            return 3;
        }
    };
    if !report.is_live() {
        eprintln!(
            "refused: the name detector's control is silent — {}",
            report.control.reason().as_str()
        );
        return 3;
    }

    // A canonical that resolves nowhere must not enter the store. Until
    // 2026-08-24 this pass would have written `sovereign_contracts::verdict::
    // Verdict` onto every Verdict site — a path with no module behind it — and
    // the eleven HAND labels beside them already said `kernel_types::Verdict`.
    // Two deciders for one name (ARCH §10.6), with the tool holding the wrong
    // one. The check is here rather than at the call site because this is where
    // the register's word becomes a stored judgement.
    let workspace = match destination::Workspace::scan(&root) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("error: could not judge — {e}");
            return 3;
        }
    };

    let store = labels::LabelStore::load(&root);
    let mut applied = 0usize;
    let mut already = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    let mut unregistered = 0usize;

    for site in &report.sites {
        let key = site.key();
        if store.get(&key).is_some() {
            already += 1;
            continue;
        }
        let Some(row) = register.iter().find(|r| r.name == site.token) else {
            unregistered += 1;
            continue;
        };
        let Some(raw) = row.disposition.as_deref() else {
            skipped.push(format!(
                "{} — register row carries no disposition",
                row.name
            ));
            continue;
        };
        let Some(disp) = labels::disposition_from_register(raw) else {
            skipped.push(format!("{} — composite disposition {raw:?}", row.name));
            continue;
        };
        let resolution = workspace.resolve(&row.canonical);
        if !resolution.is_usable() {
            skipped.push(format!(
                "{} — canonical {} is not a path a worker can `use`: {}",
                row.name,
                row.canonical,
                resolution.render()
            ));
            continue;
        }
        let label = labels::Label {
            key,
            dest: row.canonical.clone(),
            disp,
            why: format!(
                "quality/CONCEPTS.toml declares disposition {raw:?}, canonical {}",
                row.canonical
            ),
            by: "register".to_string(),
            at: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        };
        if dry_run {
            applied += 1;
            continue;
        }
        match labels::LabelStore::append(&root, site.detector, &label) {
            Ok(()) => applied += 1,
            Err(e) => {
                eprintln!("error: {e}");
                return 3;
            }
        }
    }

    println!("register pass over {} name sites", report.sites.len());
    println!(
        "  labelled       {applied}{}",
        if dry_run {
            " (dry run — nothing written)"
        } else {
            ""
        }
    );
    println!("  already known  {already}");
    println!("  not in register {unregistered}");
    // Refusals are printed AS refusals, with the reason (ARCH §18.3).
    println!("  skipped        {}", skipped.len());
    let mut seen: std::collections::BTreeSet<&String> = Default::default();
    for s in &skipped {
        if seen.insert(s) {
            println!("    {s}");
        }
    }
    0
}

/// Repo root + resolved corpus, or an exit code.
fn resolve_workspace(
    corpus_id: Option<String>,
) -> Result<(std::path::PathBuf, std::path::PathBuf, String), i32> {
    let root = census::repo_root().map_err(|e| {
        eprintln!("error: {e}");
        3
    })?;
    let indexes_dir = sovereign_cli_shared::dirs::sovereign_root().join("indexes");
    let corpus = crate::converge_cmd::resolve_corpus(corpus_id, &indexes_dir)?;
    let index_path = indexes_dir.join(&corpus);
    Ok((root, index_path, corpus))
}

/// `svrn code refactor next` — cut one record and lock its files.
async fn next_cmd(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut batch: usize = order::DEFAULT_BATCH;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus-id" => {
                i += 1;
                match args.get(i) {
                    Some(v) => corpus_id = Some(v.clone()),
                    None => {
                        eprintln!("error: --corpus-id requires a value");
                        return 1;
                    }
                }
            }
            "--batch" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(n) if n > 0 => batch = n,
                    _ => {
                        eprintln!("error: --batch requires a positive integer");
                        return 1;
                    }
                }
            }
            "-h" | "--help" => {
                eprint!("{HELP}");
                return 0;
            }
            other => {
                eprintln!("error: unknown flag {other}");
                return 1;
            }
        }
        i += 1;
    }

    let (root, index_path, corpus) = match resolve_workspace(corpus_id) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let graph = match ledger::load_graph(&index_path, &corpus).await {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: could not judge — {e}");
            return 3;
        }
    };
    let ctx = graph.ctx(&root, &index_path, &corpus);
    let store = labels::LabelStore::load(&root);

    let mut per_detector = Vec::new();
    for d in detector::all() {
        if matches!(d.cost(), detector::CostClass::Expensive(_)) {
            continue;
        }
        match d.fire(&ctx).await {
            Ok(r) if r.is_live() => {
                per_detector.push((r.detector, r.settings_digest.clone(), r.sites))
            }
            // A detector whose control went silent cannot cut an order. Say so
            // rather than quietly leaving its work out of the pool.
            Ok(r) => eprintln!(
                "note: {} contributed nothing — control silent: {}",
                d.id().as_str(),
                r.control.reason().as_str()
            ),
            Err(e) => eprintln!("note: {} never ran — {e}", d.id().as_str()),
        }
    }

    let Some(chosen) = order::choose(&per_detector, &store, batch) else {
        println!("No converge-labelled sites are available to cut an order from.");
        println!("Run `svrn code refactor status` to see what is unlabelled, then `label` it.");
        return 0;
    };

    // Interlock: an order is a self-contained instruction, and the first line
    // of it a worker acts on is the destination's import. Cutting one for a
    // path with nothing behind it spends a lease and a session to arrive at a
    // compile error — the well-formed-and-wrong result ARCH §18 refuses. The
    // check runs against the WORKING TREE, so a canonical repaired this minute
    // is honoured without waiting for the next index.
    match destination::Workspace::scan(&root) {
        Ok(ws) => {
            let r = ws.resolve(&chosen.destination);
            if !r.is_usable() {
                eprintln!(
                    "refused: destination {} is not a path a worker can `use`.\n  \
                     {}\n  \
                     Repair the row's `canonical` in quality/CONCEPTS.toml, or mint the \n  \
                     home before cutting work toward it. Nothing was locked.",
                    chosen.destination,
                    r.render()
                );
                return 2;
            }
        }
        Err(e) => {
            eprintln!("refused: could not judge destination {} — {e}", chosen.destination);
            return 3;
        }
    }

    let mut lock = match order::FileLock::acquire(&root, &chosen.id, &chosen.files) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("refused: {e}");
            return 2;
        }
    };
    // The lock outlives this process on purpose — `close` releases it.
    std::mem::forget(std::mem::replace(&mut lock, order::FileLock::empty()));

    let examples = order::worked_examples(&root, chosen.detector, &chosen.destination);
    let body = order::render(&chosen, &examples, &root);
    if let Err(e) = order::write_order(&root, &chosen, &body) {
        order::FileLock::release_for(&root, &chosen.id);
        eprintln!("error: {e}");
        return 3;
    }

    println!(
        "order written: {}",
        order::order_path(&root, &chosen.id).display()
    );
    println!("  destination  {}", chosen.destination);
    println!(
        "  detector     {} @ {}",
        chosen.detector.as_str(),
        chosen.settings_digest
    );
    println!("  holdings     {}", chosen.sites.len());
    println!(
        "  files        {} locked (local to this machine — the work atlas",
        chosen.files.len()
    );
    println!("               is visibility, not a lock manager, so a peer on");
    println!("               another host is not blocked by this)");
    println!(
        "  worked ex.   {}",
        if examples.is_empty() {
            "none — first of its class".to_string()
        } else {
            examples.len().to_string()
        }
    );
    println!("  close with   svrn code refactor close {}", chosen.id);
    0
}

/// `svrn code refactor close` — prove, report, release.
async fn close_cmd(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut order_id: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus-id" => {
                i += 1;
                match args.get(i) {
                    Some(v) => corpus_id = Some(v.clone()),
                    None => {
                        eprintln!("error: --corpus-id requires a value");
                        return 1;
                    }
                }
            }
            "-h" | "--help" => {
                eprint!("{HELP}");
                return 0;
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag {flag}");
                return 1;
            }
            positional => order_id = Some(positional.to_string()),
        }
        i += 1;
    }
    let Some(order_id) = order_id else {
        eprintln!("error: close needs an order id");
        return 1;
    };

    let (root, index_path, corpus) = match resolve_workspace(corpus_id) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let before = match order::read_sites(&root, &order_id) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 3;
        }
    };
    let Some(det_id) = before.first().map(|s| s.detector) else {
        eprintln!("error: order {order_id} carries no sites");
        return 3;
    };
    let mut files: Vec<String> = before.iter().map(|s| s.file.clone()).collect();
    files.sort();
    files.dedup();

    let graph = match ledger::load_graph(&index_path, &corpus).await {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: could not judge — {e}");
            return 3;
        }
    };
    let ctx = graph.ctx(&root, &index_path, &corpus);
    let all = detector::all();
    let Some(d) = all.iter().find(|d| d.id() == det_id) else {
        eprintln!("error: no detector {}", det_id.as_str());
        return 3;
    };

    let report = match order::prove(&ctx, &order_id, d.as_ref(), &before, &files).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: could not judge — {e}");
            return 3;
        }
    };
    print!("{}", order::render_close(&report));

    if !report.control_live {
        // Nothing was closed and nothing is released — the order stands.
        return 3;
    }
    let released = order::FileLock::release_for(&root, &order_id);
    println!("  released   {released} file lock(s)");
    if report.still_open.is_empty() {
        println!("  order complete — every site closed.");
        0
    } else {
        println!("  order partially closed — the rest returns to the pool.");
        0
    }
}

/// `svrn code refactor label` — the only verb that writes a judgement.
///
/// Deliberately NOT a verb that writes progress: a label says what a site
/// MEANS, never whether it is done.
async fn label_cmd(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("--from-register") {
        return label_from_register(&args[1..]).await;
    }
    if args.first().map(String::as_str) == Some("--model") {
        return label_model_cmd(&args[1..]).await;
    }
    // `--shard <name>` routes this judgement to `labels/<detector>.<name>.jsonl`
    // so N parallel labellers never append to one file. Pulled out before the
    // positionals so it may appear anywhere on the line.
    let mut shard: Option<String> = None;
    let mut args: Vec<String> = {
        let mut out = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if let Some(v) = a.strip_prefix("--shard=") {
                shard = Some(v.to_string());
            } else if a == "--shard" {
                shard = it.next().cloned();
            } else {
                out.push(a.clone());
            }
        }
        out
    };
    let args = &args[..];
    if args.len() < 5 {
        eprintln!(
            "error: label needs <detector> <key> <disposition> <destination> <why>\n\
             e.g. label name 'name/kernel_types::Verdict/Verdict' converge kernel_types::Verdict \
             'the register declares this canonical'"
        );
        return 1;
    }
    let Some(det) = detector::DetectorId::ALL
        .into_iter()
        .find(|d| d.as_str() == args[0])
    else {
        eprintln!("error: unknown detector '{}'", args[0]);
        return 1;
    };
    // Accepts both the wire spelling and the display spelling, and the error
    // text is generated from the same list the parser uses, so the two cannot
    // drift apart again.
    let Some(disp) = labels::Disposition::parse_cli(&args[2]) else {
        eprintln!(
            "error: unknown disposition '{}' — one of {}",
            args[2],
            labels::Disposition::ALL
                .iter()
                .map(|d| d.wire())
                .collect::<Vec<_>>()
                .join("|")
        );
        return 1;
    };
    let root = match census::repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 3;
        }
    };
    let label = labels::Label {
        key: args[1].clone(),
        dest: args[3].clone(),
        disp,
        why: args[4..].join(" "),
        by: "seat".to_string(),
        at: chrono::Utc::now().format("%Y-%m-%d").to_string(),
    };
    match labels::LabelStore::append_to(&root, det, &label, shard.as_deref()) {
        Ok(()) => {
            println!("labelled {} -> {} ({})", label.key, label.dest, args[2]);
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            3
        }
    }
}

async fn plan_cmd(args: &[String]) -> i32 {
    let mut spec_path: Option<String> = None;
    let mut crate_filter: Option<String> = None;
    let mut sites_per_class: usize = 5;
    let mut run_fixture = true;
    let mut run_baseline = true;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--crate" => {
                i += 1;
                match args.get(i) {
                    Some(v) => crate_filter = Some(v.clone()),
                    None => {
                        eprintln!("error: --crate requires a value");
                        return 1;
                    }
                }
            }
            "--sites" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(0) => sites_per_class = usize::MAX,
                    Some(n) => sites_per_class = n,
                    None => {
                        eprintln!("error: --sites requires an integer");
                        return 1;
                    }
                }
            }
            "--skip-fixture" => run_fixture = false,
            "--skip-baseline" => run_baseline = false,
            "--json" => json = true,
            "-h" | "--help" => {
                eprint!("{HELP}");
                return 0;
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag {flag}");
                return 1;
            }
            positional => {
                if spec_path.is_some() {
                    eprintln!("error: more than one spec path given");
                    return 1;
                }
                spec_path = Some(positional.to_string());
            }
        }
        i += 1;
    }
    let Some(spec_path) = spec_path else {
        eprintln!("error: plan needs a spec path, e.g. quality/refactors/corpus-id.toml");
        return 1;
    };

    let opts = discover::PlanOptions {
        spec_path: spec_path.into(),
        crate_filter,
        sites_per_class,
        run_fixture,
        run_baseline,
        json,
    };
    discover::run_plan(&opts).await
}
