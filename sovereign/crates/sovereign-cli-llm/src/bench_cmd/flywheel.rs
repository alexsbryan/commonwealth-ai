// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench flywheel run …` — the Fidelity-Flywheel loop's READ side.
//!
//! Drives an autonomously-generated probe set (I1 corpus self-supervision)
//! through the SAME live chat path the chaos bench uses (`run_live`, sealed to
//! one corpus), classifies each answer with the shared forced-choice judges,
//! verifies it against the probe's witness with the pure
//! `DeterministicVerifier`, scores the two red-lines (reusing the chaos scorer
//! of record), and captures every failure as a durable regression case.
//!
//! This is generator-agnostic by construction: it asks a
//! [`sovereign_eval::flywheel::Generator`] for probes and treats them
//! uniformly, so I2–I5 reuse this orchestrator unchanged — they only change
//! which generator is selected.
//!
//! The WRITE side (proposing + gating a scaffolding change) is
//! `bench_cmd::promote`; this command measures, captures, and reports.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use sovereign_core::traits::InferenceProvider;
use sovereign_eval::chaos_monkey::{score, AgentAction, CalibrationReport, Gates, QuestionType};
use sovereign_eval::flywheel::generators::corpus::{AbsentSource, CorpusGenerator};
use sovereign_eval::flywheel::{
    by_id, generator_ids, validate_fairness, DeterministicVerifier, Observation, Probe,
    RegressionBank, RegressionCase, Verdict,
};
use sovereign_inference::remote::RemoteApiProvider;

use crate::bench_cmd::live_runner::{classify_abstain, classify_caveat, run_live};
use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench flywheel",
    summary: "Fidelity-Flywheel read side: generate probes from a corpus, run them through the live chat path, verify groundedness/abstention, capture failures as regression cases.",
    sections: &[
        HelpSection::Usage(
            "svrn bench flywheel run --corpus <id> [--mine-path <dir>] [--absent-bank <bank.toml>] [--withheld-path <dir>] [--n N] [--seed N] [--judge-model <stem>] [--out <jsonl>] [--regressions <jsonl>] [--no-capture]",
        ),
        HelpSection::Subcommands(&[
            (
                "run",
                "Generate I1 probes (Present mined from --mine-path's atlas/atoms.json; Absent from --absent-bank or --withheld-path), run each through the live path sealed to --corpus, verify + score the two red-lines, capture failures.",
            ),
            (
                "h1-gate",
                "OFFLINE: NATIVE_GROUNDING §7.3's H1 measurement. Scores every calibration pair with BOTH answerability signals — the rerank-slot margin (max over the pool) and top_cosine — writes an operating curve for each (overall and split by corpus family), and applies the §7.3 kill criterion to a committed verdict artifact. Refuses to run without a reranker rather than emitting a half-verdict; --from-scores replays a frozen score file with no model loaded. Exit 3 = H1 killed (a successful run).",
            ),
            (
                "h4-sweep",
                "OFFLINE: NATIVE_GROUNDING §5 H4's sentence sweep. Splits every released answer in a FROZEN chaos transcript with the lossless splitter, scores each sentence against that turn's sealed evidence with the rerank slot (max over the k<=8 pool), rides the deterministic vetoes and the span resolver along, and writes one row per sentence with its per-turn audit wall time. No daemon, no judge, no Critic; the transcript is never rewritten. Emits margins, NOT verdicts — the floor is calibrated by h4-gate and lives beside its committed curve. Refuses to run without a reranker rather than substituting a stand-in scorer.",
            ),
            (
                "calibration-set",
                "OFFLINE: mine (question, chunks, answerable?) pairs from one or more corpora's atlases — pools built from REAL passage text resolved out of the chunk store, never the atom's ~25-char passage_preview — and run the contamination pass against the dev/test banks. No daemon, no model, no RNG — the same corpora and --pool yield byte-identical output. Flags: --corpus <id> (repeatable), --corpus-prefix <p> (expand every corpus under the index root starting with p; the stratification lever — pair it with a small --limit to mine a little from many articles rather than a lot from few), --limit N (claims per NAMED corpus), --prefix-limit N (claims per SWEPT corpus; defaults to --limit), --pool N (chunks per pair), --max-pairs N, --bank <toml> (repeatable, required), --out <jsonl>. This is NATIVE_GROUNDING §7.1's calibration role: the only data H1/H2 thresholds may be fitted on. Refuses to mine a dev/test bank corpus, refuses a corpus thinner than the pool size, and exits non-zero when the contamination pass finds a shared 13-word span.",
            ),
        ]),
        HelpSection::Notes(
            "Present probes need an ENRICHED corpus root (--mine-path with atlas/atoms.json); a corpus with no enrichment yields no Present probes. Absent probes come from a curated bank (--absent-bank) or a withheld, enriched-but-unindexed slice (--withheld-path). The verifier is pure and reuses the chaos two-red-line scorer; failures are captured to sovereign/bench/flywheel/regressions/<corpus>.jsonl (fairness-validated, deduped).",
        ),
    ],
};

const PROVIDER_CTX: u32 = 8192;

pub async fn cmd_flywheel(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }
    match args[0].as_str() {
        "run" => run(&args[1..]).await,
        "calibration-set" => calibration_set(&args[1..]).await,
        "h1-gate" => super::h1_gate::cmd_h1_gate(&args[1..]).await,
        "h4-sweep" => super::h4::sweep::cmd_h4_sweep(&args[1..]).await,
        "redteam" => super::redteam::cmd_redteam(&args[1..]).await,
        other => {
            eprintln!("error: unknown flywheel subcommand `{other}`");
            help::print(&HELP);
            2
        }
    }
}

/// `calibration-set` — mine the §7.1 calibration role and prove it clean.
///
/// Offline by construction: it reads corpus atlases and chunk stores off disk
/// and the bank TOMLs out of the repo. Nothing here builds a provider or
/// touches the daemon — the "no live model" property is structural. It is
/// `async` only because the chunk store is LanceDB and `CorpusIndex` is
/// async; that is still local file I/O, not a network or a model.
///
/// Exit codes: `0` clean, `1` contaminated or I/O failure, `2` usage.
async fn calibration_set(rest: &[String]) -> i32 {
    use sovereign_eval::flywheel::calibration as cal;
    use sovereign_eval::flywheel::passages::{chunk_store_for, PassageStore};

    let mut corpora: Vec<String> = Vec::new();
    // The one accessor for this path (`~/.svrnmesh|.sovereign/indexes`), not a
    // re-derivation and not a new env knob — ARCH §10.6, one accessor per path.
    let mut index_root = sovereign_cli_shared::dirs::sovereign_indexes();
    let mut banks: Vec<PathBuf> = Vec::new();
    let mut out = PathBuf::from("sovereign/bench/calibration/native_grounding_calibration.jsonl.gz");
    let mut limit = 5_000usize;
    let mut pool = 8usize;
    // Corpus-id prefixes to expand against the index root. This is the
    // stratification lever: `--corpus-prefix sep- --limit 2` mines a LITTLE
    // from EVERY article rather than a lot from a few, which is what
    // NATIVE_GROUNDING §7.1 means by spread.
    let mut prefixes: Vec<String> = Vec::new();
    // Claim cap for prefix-EXPANDED corpora only. Defaults to `--limit`.
    // Separate because the two roles differ: you name a corpus because you
    // want its depth, and you sweep a prefix because you want its breadth.
    // One knob for both would force the literary minority down to the SEP
    // per-article cap.
    let mut prefix_limit: Option<usize> = None;
    let mut max_pairs: Option<usize> = None;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            match rest.get(i).cloned() {
                Some(v) => v,
                None => {
                    eprintln!("error: {} requires a value", $l);
                    return 2;
                }
            }
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--corpus" => corpora.push(val!("--corpus")),
            "--corpus-prefix" => prefixes.push(val!("--corpus-prefix")),
            "--index-root" => index_root = PathBuf::from(val!("--index-root")),
            "--bank" => banks.push(PathBuf::from(val!("--bank"))),
            "--out" => out = PathBuf::from(val!("--out")),
            "--limit" => match val!("--limit").parse() {
                Ok(v) => limit = v,
                Err(_) => {
                    eprintln!("error: --limit must be a usize");
                    return 2;
                }
            },
            "--pool" => match val!("--pool").parse() {
                Ok(v) => pool = v,
                Err(_) => {
                    eprintln!("error: --pool must be a usize");
                    return 2;
                }
            },
            "--prefix-limit" => match val!("--prefix-limit").parse() {
                Ok(v) => prefix_limit = Some(v),
                Err(_) => {
                    eprintln!("error: --prefix-limit must be a usize");
                    return 2;
                }
            },
            "--max-pairs" => match val!("--max-pairs").parse() {
                Ok(v) => max_pairs = Some(v),
                Err(_) => {
                    eprintln!("error: --max-pairs must be a usize");
                    return 2;
                }
            },
            other => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }
    // Expand each prefix against the index root, in sorted order so the run
    // is reproducible from its flags alone. Only directories carrying an
    // `atlas/atoms.json` qualify — there is nothing to mine otherwise, and
    // silently counting them as "corpora that produced no pairs" would make
    // a coverage number that is not about coverage.
    let mut swept: std::collections::HashSet<String> = std::collections::HashSet::new();
    for prefix in &prefixes {
        let Ok(entries) = std::fs::read_dir(&index_root) else {
            eprintln!("error: cannot read index root {index_root:?}");
            return 1;
        };
        let mut found: Vec<String> = entries
            .filter_map(std::result::Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| name.starts_with(prefix.as_str()))
            .filter(|name| index_root.join(name).join("atlas").join("atoms.json").is_file())
            .collect();
        found.sort();
        if found.is_empty() {
            eprintln!(
                "error: --corpus-prefix `{prefix}` matched no corpus with an atlas under \
                 {index_root:?} — an empty expansion would mine nothing and report clean"
            );
            return 1;
        }
        eprintln!("[calibration] --corpus-prefix {prefix} → {} corpora", found.len());
        swept.extend(found.iter().cloned());
        corpora.extend(found);
    }
    corpora.dedup();
    if corpora.is_empty() {
        eprintln!("error: at least one --corpus <id> or --corpus-prefix <p> is required");
        return 2;
    }
    if banks.is_empty() {
        eprintln!(
            "error: at least one --bank <bank.toml> is required — a contamination pass with \
             nothing to check against would call every set clean"
        );
        return 2;
    }

    let mut all = Vec::new();
    let mut reports = Vec::new();
    // Shared chunk stores are loaded ONCE and partitioned. Mining 1,770 SEP
    // articles out of their common 187,967-chunk store one filtered scan at
    // a time would re-scan the whole table 1,770 times (LanceDB has no
    // index on `source_doc_id`).
    let mut partitioned: HashMap<String, HashMap<String, PassageStore>> = HashMap::new();
    let mut colocated: HashMap<String, PassageStore> = HashMap::new();
    // A corpus that is simply not IN its chunk store is a different fact
    // from a corpus whose claims did not resolve, and it is not fatal to
    // the run — it is counted and named.
    let mut missing_from_store: Vec<String> = Vec::new();
    let mut refused: Vec<(String, String)> = Vec::new();

    for id in &corpora {
        if max_pairs.is_some_and(|m| all.len() >= m) {
            break;
        }
        let root = index_root.join(id);
        // Where this atlas's real passages live. `sep-<slug>` atlases are
        // atlas-only and share one `sep` chunk store; everything else is
        // co-located. One name for the mapping (ARCH §10.6).
        let (chunk_corpus, doc_filter) = chunk_store_for(id);
        let passages: &PassageStore = match &doc_filter {
            Some(doc) => {
                if !partitioned.contains_key(&chunk_corpus) {
                    eprintln!("[calibration] loading shared chunk store `{chunk_corpus}` …");
                    match PassageStore::load_partitioned(&index_root, &chunk_corpus).await {
                        Ok(m) => {
                            eprintln!(
                                "[calibration] `{chunk_corpus}`: {} document(s) available",
                                m.len()
                            );
                            partitioned.insert(chunk_corpus.clone(), m);
                        }
                        Err(e) => {
                            eprintln!("[calibration] {id}: {e}");
                            return 1;
                        }
                    }
                }
                match partitioned[&chunk_corpus].get(doc) {
                    Some(p) => p,
                    None => {
                        missing_from_store.push(id.clone());
                        continue;
                    }
                }
            }
            None => {
                if !colocated.contains_key(&chunk_corpus) {
                    match PassageStore::load(&index_root, &chunk_corpus, None).await {
                        Ok(p) => {
                            colocated.insert(chunk_corpus.clone(), p);
                        }
                        Err(e) => {
                            eprintln!("[calibration] {id}: {e}");
                            return 1;
                        }
                    }
                }
                &colocated[&chunk_corpus]
            }
        };
        let claim_cap = if swept.contains(id) {
            prefix_limit.unwrap_or(limit)
        } else {
            limit
        };
        match cal::mine_calibration_pairs(id, &root, passages, claim_cap, pool) {
            Ok((pairs, rep)) => {
                // Per-corpus lines would be 1,770 of these on a prefix run;
                // print them only when the run is small enough to read.
                if corpora.len() <= 32 {
                    eprintln!(
                        "[calibration] {id}: claims={} unresolved={} answerable={} absent={} \
                         dropped_witness_leak={} dropped_anchor_leak={} witness_absent={} \
                         passages={} from={}",
                        rep.claims_mined,
                        rep.claims_unresolved,
                        rep.pairs_answerable,
                        rep.pairs_absent,
                        rep.absent_dropped_leaky,
                        rep.absent_dropped_anchor_leak,
                        rep.answerable_witness_absent,
                        rep.passages_available,
                        rep.passage_source,
                    );
                }
                all.extend(pairs);
                reports.push(rep);
            }
            Err(e) => {
                // A thin or empty corpus is a REFUSAL, not a crash, once the
                // run spans thousands of them — but it is recorded by name,
                // never swallowed. A refusal on an explicitly-named --corpus
                // still fails the run, because the operator asked for that
                // one specifically.
                if prefixes.is_empty() {
                    eprintln!("[calibration] {id}: {e}");
                    return 1;
                }
                refused.push((id.clone(), e));
            }
        }
    }
    if !missing_from_store.is_empty() {
        eprintln!(
            "[calibration] {} corpus(es) had an atlas but no document in their chunk store \
             (first: {})",
            missing_from_store.len(),
            missing_from_store[0]
        );
    }
    if !refused.is_empty() {
        eprintln!(
            "[calibration] {} corpus(es) refused (thin/empty); first: {} — {}",
            refused.len(),
            refused[0].0,
            refused[0].1
        );
    }
    if all.is_empty() {
        eprintln!("error: mined 0 pairs across {} corpus(es)", corpora.len());
        return 1;
    }

    let contamination = match cal::contamination_pass(&all, &banks) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: contamination pass: {e}");
            return 1;
        }
    };
    for (bank, n) in &contamination.banks_indexed {
        eprintln!(
            "[contamination] indexed {n} {}-gram(s) from {bank}",
            contamination.shingle_n
        );
    }

    // One writer for this format, shared with the harness's reader.
    if let Err(e) = cal::write_pairs(&out, &all) {
        eprintln!("error: {e}");
        return 1;
    }
    // The full per-corpus table is the audit trail, but 1,770 pretty-printed
    // rows is not a reviewable artifact. Totals + stratification shape go in
    // the report; the per-corpus rows go to a compact JSONL sibling.
    // Sibling artifact names hang off the set's base name. `with_extension`
    // alone would produce `…jsonl.contamination.json` on a `.jsonl.gz` set,
    // so the compression suffix is stripped first.
    let base = {
        let mut b = out.clone();
        if b.extension().is_some_and(|e| e.eq_ignore_ascii_case("gz")) {
            b.set_extension("");
        }
        b.set_extension("");
        b
    };
    let reports_path = base.with_extension("mine_reports.jsonl");
    let mut rbody = String::new();
    for r in &reports {
        match serde_json::to_string(r) {
            Ok(s) => {
                rbody.push_str(&s);
                rbody.push('\n');
            }
            Err(e) => {
                eprintln!("error: could not serialize mine report for {}: {e}", r.corpus_id);
                return 1;
            }
        }
    }
    if let Err(e) = std::fs::write(&reports_path, rbody) {
        eprintln!("error: could not write {reports_path:?}: {e}");
        return 1;
    }

    let contributing = reports.iter().filter(|r| r.pairs_answerable + r.pairs_absent > 0).count();
    let mut per_corpus_pairs: Vec<usize> = reports
        .iter()
        .map(|r| r.pairs_answerable + r.pairs_absent)
        .filter(|n| *n > 0)
        .collect();
    per_corpus_pairs.sort_unstable();
    let totals = serde_json::json!({
        "corpora_considered": corpora.len(),
        "corpora_contributing_pairs": contributing,
        "corpora_refused_thin_or_empty": refused.len(),
        "corpora_missing_from_chunk_store": missing_from_store.len(),
        "claims_mined": reports.iter().map(|r| r.claims_mined).sum::<usize>(),
        "claims_unresolved": reports.iter().map(|r| r.claims_unresolved).sum::<usize>(),
        "pairs_answerable": reports.iter().map(|r| r.pairs_answerable).sum::<usize>(),
        "pairs_absent": reports.iter().map(|r| r.pairs_absent).sum::<usize>(),
        "absent_dropped_witness_leak": reports.iter().map(|r| r.absent_dropped_leaky).sum::<usize>(),
        "absent_dropped_anchor_leak":
            reports.iter().map(|r| r.absent_dropped_anchor_leak).sum::<usize>(),
        "answerable_witness_absent":
            reports.iter().map(|r| r.answerable_witness_absent).sum::<usize>(),
        // The stratification evidence: a set of 2,000 pairs drawn from 900
        // articles and one drawn from 12 are not the same set, and only
        // these numbers tell them apart.
        "pairs_per_contributing_corpus_min": per_corpus_pairs.first().copied().unwrap_or(0),
        "pairs_per_contributing_corpus_median":
            per_corpus_pairs.get(per_corpus_pairs.len() / 2).copied().unwrap_or(0),
        "pairs_per_contributing_corpus_max": per_corpus_pairs.last().copied().unwrap_or(0),
    });
    let report_path = base.with_extension("contamination.json");
    let doc = serde_json::json!({
        "totals": totals,
        "per_corpus_reports": reports_path.display().to_string(),
        "pool_size": pool,
        "limit_claims_per_named_corpus": limit,
        "limit_claims_per_swept_corpus": prefix_limit.unwrap_or(limit),
        "corpus_prefixes": prefixes,
        "max_pairs": max_pairs,
        "contamination": contamination,
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(s) => {
            if let Err(e) = std::fs::write(&report_path, s + "\n") {
                eprintln!("error: could not write {report_path:?}: {e}");
                return 1;
            }
        }
        Err(e) => {
            eprintln!("error: could not serialize report: {e}");
            return 1;
        }
    }
    eprintln!(
        "[out] {} pair(s) → {out:?}\n[out] contamination report → {report_path:?}\n\
         [out] per-corpus mine reports → {reports_path:?}",
        all.len()
    );
    if contamination.clean {
        eprintln!(
            "[contamination] CLEAN — no calibration pair shares a 13-word span with any bank"
        );
        0
    } else {
        eprintln!(
            "[contamination] CONTAMINATED — {} pair(s) share a verbatim span with a dev/test bank; \
             thresholds fitted on this set would be unfalsifiable",
            contamination.collisions.len()
        );
        1
    }
}

struct Args {
    generator: String,
    corpus: String,
    mine_path: Option<PathBuf>,
    absent_bank: Option<PathBuf>,
    withheld_path: Option<PathBuf>,
    n: usize,
    seed: u64,
    judge_model: String,
    base_url: String,
    out: PathBuf,
    regressions: Option<PathBuf>,
    capture: bool,
}

fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut generator = "i1_corpus".to_string();
    let mut corpus: Option<String> = None;
    let mut mine_path: Option<PathBuf> = None;
    let mut absent_bank: Option<PathBuf> = None;
    let mut withheld_path: Option<PathBuf> = None;
    let mut n = 12usize;
    let mut seed = 0u64;
    let mut judge_model = "fast".to_string();
    let mut base_url = "http://localhost:9741".to_string();
    let mut out = PathBuf::from("target/flywheel/results.jsonl");
    let mut regressions: Option<PathBuf> = None;
    let mut capture = true;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            rest.get(i)
                .cloned()
                .ok_or_else(|| format!("{} requires a value", $l))?
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--generator" => generator = val!("--generator"),
            "--corpus" => corpus = Some(val!("--corpus")),
            "--mine-path" => mine_path = Some(PathBuf::from(val!("--mine-path"))),
            "--absent-bank" => absent_bank = Some(PathBuf::from(val!("--absent-bank"))),
            "--withheld-path" => withheld_path = Some(PathBuf::from(val!("--withheld-path"))),
            "--n" => n = val!("--n").parse().map_err(|_| "--n must be a usize")?,
            "--seed" => seed = val!("--seed").parse().map_err(|_| "--seed must be a u64")?,
            "--judge-model" => judge_model = val!("--judge-model"),
            "--base-url" => base_url = val!("--base-url"),
            "--out" => out = PathBuf::from(val!("--out")),
            "--regressions" => regressions = Some(PathBuf::from(val!("--regressions"))),
            "--no-capture" => capture = false,
            other => return Err(format!("unknown flag `{other}`")),
        }
        i += 1;
    }
    Ok(Args {
        generator,
        corpus: corpus.ok_or("--corpus is required (the corpus id to seal retrieval to)")?,
        mine_path,
        absent_bank,
        withheld_path,
        n,
        seed,
        judge_model,
        base_url,
        out,
        regressions,
        capture,
    })
}

async fn run(rest: &[String]) -> i32 {
    let (mut globals, rest) = match parse_globals(rest) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if globals.temperature.is_none() {
        globals.temperature = Some(0.0);
    }
    let args = match parse_args(&rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            help::print(&HELP);
            return 2;
        }
    };

    // ── Build the probe set ──
    // The generic registry produces a default-configured generator; I1's
    // absent source is set from the flags (the registry default is None).
    let probes = if args.generator == "i1_corpus" {
        let absent = if let Some(w) = args.withheld_path.clone() {
            AbsentSource::HeldOutSlice { withheld: w }
        } else if let Some(b) = args.absent_bank.clone() {
            AbsentSource::CuratedBank(b)
        } else {
            AbsentSource::None
        };
        let generator = CorpusGenerator { absent };
        use sovereign_eval::flywheel::Generator as _;
        generator.generate(args.n, args.seed, args.mine_path.as_deref())
    } else {
        match by_id(&args.generator) {
            Some(g) => g.generate(args.n, args.seed, args.mine_path.as_deref()),
            None => {
                eprintln!(
                    "error: unknown --generator `{}`. Registered: {}",
                    args.generator,
                    generator_ids().join(", ")
                );
                return 2;
            }
        }
    };

    if probes.is_empty() {
        eprintln!(
            "error: generator `{}` produced no probes.\n  \
             For I1 Present probes, pass --mine-path <enriched-corpus-root> (needs atlas/atoms.json).\n  \
             For Absent probes, pass --absent-bank <bank.toml> or --withheld-path <dir>.",
            args.generator
        );
        return 1;
    }
    // Defense-in-depth: the fairness contract is enforced at generation, but
    // re-check here so an unfair probe can never reach the model (or capture).
    if let Some(bad) = probes.iter().find_map(|p| validate_fairness(p).err()) {
        eprintln!("error: generator emitted an unfair probe: {bad}");
        return 1;
    }
    let n_answerable = probes.iter().filter(|p| p.qtype.is_answerable()).count();
    let n_absent = probes.len() - n_answerable;
    eprintln!(
        "[flywheel] generator={} corpus={} probes={} (answerable={}, absent={})",
        args.generator,
        args.corpus,
        probes.len(),
        n_answerable,
        n_absent,
    );

    // ── Live session + judge ──
    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not build chat session: {e}");
            return 1;
        }
    };
    let v1 = format!("{}/v1", args.base_url.trim_end_matches('/'));
    let judge: Arc<dyn InferenceProvider> = Arc::new(RemoteApiProvider::new(
        &v1,
        None,
        &args.judge_model,
        PROVIDER_CTX,
    ));
    let model_id = globals
        .chat_model
        .clone()
        .unwrap_or_else(|| "primary".to_string());

    // ── Run + verify each probe ──
    let verifier = DeterministicVerifier;
    let mut verdicts: Vec<Verdict> = Vec::with_capacity(probes.len());
    for (pi, probe) in probes.iter().enumerate() {
        let verdict = run_and_verify(
            &session,
            judge.as_ref(),
            &args.judge_model,
            &args.corpus,
            &model_id,
            &verifier,
            probe,
        )
        .await;
        eprintln!(
            "  [{:>2}/{}] {:<20} act={:<9} {}",
            pi + 1,
            probes.len(),
            probe.qtype.label(),
            format!("{:?}", verdict.row.agent_action),
            match &verdict.failure {
                None => "PASS".to_string(),
                Some(f) => format!("FAIL {f:?}"),
            },
        );
        verdicts.push(verdict);
    }

    // ── Score + glassbox ──
    let rows: Vec<_> = verdicts.iter().map(|v| v.row.clone()).collect();
    if let Err(e) = write_jsonl(&args.out, &verdicts) {
        eprintln!("error: could not write {:?}: {e}", args.out);
        return 1;
    }
    let report = score(&rows);
    let gates = Gates::default();
    let verdict = report.verdict(&gates);
    print_summary(&report, &verdicts);

    // ── Capture failures as regression cases ──
    if args.capture {
        let path = args.regressions.clone().unwrap_or_else(|| {
            PathBuf::from(format!(
                "sovereign/bench/flywheel/regressions/{}.jsonl",
                args.corpus
            ))
        });
        let captured_at = chrono::Utc::now().to_rfc3339();
        let source_run = format!("flywheel:{}:seed{}", args.corpus, args.seed);
        let mut newly = 0usize;
        for (probe, v) in probes.iter().zip(&verdicts) {
            let Some(failure) = v.failure else { continue };
            let case = RegressionCase {
                id: format!("{}-{}", source_run, probe.id),
                probe: probe.clone(),
                failure,
                determinism: v.determinism,
                captured_answer_excerpt: v.row.answer_excerpt.clone(),
                captured_chunks: Vec::new(),
                corpus: args.corpus.clone(),
                model_id: model_id.clone(),
                captured_at: captured_at.clone(),
                source_run: source_run.clone(),
            };
            match RegressionBank::capture(&path, &case) {
                Ok(true) => newly += 1,
                Ok(false) => {}
                Err(e) => eprintln!("  [capture] skipped {}: {e}", probe.id),
            }
        }
        eprintln!("[capture] {newly} new regression case(s) → {path:?}");
    }

    eprintln!("[out] wrote {} verdicts → {:?}", verdicts.len(), args.out);
    if verdict.overall_pass {
        0
    } else {
        1
    }
}

/// One probe → live answer → observation (judge classification) → verdict.
async fn run_and_verify(
    session: &crate::chat_cmd::bootstrap::ChatSession,
    judge: &dyn InferenceProvider,
    judge_model: &str,
    corpus: &str,
    model_id: &str,
    verifier: &DeterministicVerifier,
    probe: &Probe,
) -> Verdict {
    let live = run_live(session, corpus, &probe.query).await;
    let visible = live.visible;
    let chunks = live.retrieved_chunk_texts;

    let action = match classify_abstain(judge, judge_model, &visible).await {
        Some(true) => AgentAction::Abstained,
        Some(false) => AgentAction::Answered,
        None => {
            if visible.trim().len() < 24 {
                AgentAction::Abstained
            } else {
                AgentAction::Answered
            }
        }
    };

    // Provenance caveat — only for out-of-domain answers (mirrors chaos).
    let caveat_present =
        if probe.qtype == QuestionType::AbsentOutOfDomain && action == AgentAction::Answered {
            Some(
                classify_caveat(judge, judge_model, &visible)
                    .await
                    .unwrap_or(false),
            )
        } else {
            None
        };

    let obs = Observation {
        action,
        answer: &visible,
        chunks: &chunks,
        caveat_present,
    };
    verifier.verify(probe, &obs, model_id, corpus)
}

fn write_jsonl(path: &std::path::Path, verdicts: &[Verdict]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::File::create(path)?;
    use std::io::Write as _;
    for v in verdicts {
        let line = serde_json::to_string(v)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        writeln!(f, "{line}")?;
    }
    Ok(())
}

/// The calibration report itself — the numbers the run exists to produce,
/// so every line here is stdout payload.
fn print_summary(report: &CalibrationReport, verdicts: &[Verdict]) {
    let c = &report.counts;
    println!("\n── fidelity flywheel: grounded calibration (I1) ──");
    println!(
        "  competence-when-present : {:.2}   [correct {}/{}, timid {}]",
        report.competence, c.answerable_correct, c.answerable, c.answerable_abstained,
    );
    println!(
        "  honesty-when-absent     : {:.2}   [honest {}/{}, HALLUCINATED {}]",
        report.honesty, c.absent_honest, c.absent, c.absent_hallucinated,
    );
    // Failure-class tally (the taxonomy glassbox).
    let mut tally: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for v in verdicts {
        if let Some(f) = v.failure {
            *tally.entry(format!("{f:?}")).or_default() += 1;
        }
    }
    if tally.is_empty() {
        println!("  failures: none");
    } else {
        let parts: Vec<String> = tally.iter().map(|(k, n)| format!("{k}={n}")).collect();
        println!("  failures by class: {}", parts.join(" "));
    }
}
