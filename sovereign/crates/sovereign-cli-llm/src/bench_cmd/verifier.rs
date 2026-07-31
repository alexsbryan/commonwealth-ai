// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench verifier …` — offline seams for the verifier-v0 program
//! (`sovereign/docs/specs/VERIFIER_V0.md`).
//!
//! Stream B's premise is that training claims must be in the PRODUCTION
//! register — the exact prompt, parser, and claim budget the grounding gate
//! runs — so the harness reaches the gate's own primitive through this verb
//! instead of re-implementing extraction in a script. Mirrors the
//! stdin/stdout JSON shape of `chaos-monkey score-answer` (the established
//! single-pair seam pattern): one JSON object in, one JSON line out, all
//! diagnostics on stderr.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::CorpusIndex;
use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::runtime::{extract_claim_list, value_present_in_chunks};
use sovereign_core::traits::InferenceProvider;
use sovereign_eval::flywheel::det_checks::contains_ci;
use sovereign_eval::flywheel::generators::adversarial as adv;
use sovereign_inference::remote::RemoteApiProvider;

use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench verifier",
    summary: "Verifier-v0 offline seams: Stream B harvest → corruption → export, plus claim extraction in the production gate register.",
    sections: &[
        HelpSection::Usage(
            "svrn bench verifier <extract-claims|harvest|export> [flags] — see each subcommand's --help",
        ),
        HelpSection::Subcommands(&[
            (
                "extract-claims",
                "Extract the factual-claim list from ONE (question, answer) pair with the SAME prompt, parser, and budget the longform grounding gate runs (sovereign_core::runtime::extract_claim_list). Reads {\"question\",\"answer\"} JSON from --input or stdin; writes {\"claims\":[..]} to stdout. NO_CLAIM / nothing checkable is an empty list with exit 0; an inference failure exits 1.",
            ),
            (
                "harvest",
                "Walk an installed corpus's chunks in evidence windows, run each window through the production claim extraction, and write the Stream B harvest artifact (claims.json: HarvestFile schema v1 — claims + sealed windows inline, optional --entities/--distractors side tables). The artifact is the substrate the i2_adversarial generator corrupts.",
            ),
            (
                "export",
                "Load a harvest artifact, generate (n, seed)-deterministic corruption cases via the i2_adversarial flywheel generator, RE-VALIDATE every case against the production value_present_in_chunks checker, and write Stream B JSONL (claim, evidence window, constructed label, corruption kind, site witness, span offsets). Any production-check failure aborts the export — labels are by construction or not at all.",
            ),
        ]),
        HelpSection::Notes(
            "extract-claims and harvest operate against the running daemon at --base-url (default localhost:9741); export is fully offline. --model defaults to the Critic role's preferred tier (the role the gate routes extraction under). Posture is LocalOnly: bench runs never offload across the mesh.",
        ),
    ],
};

const PROVIDER_CTX: u32 = 8192;

pub async fn cmd_verifier(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }
    match args[0].as_str() {
        "extract-claims" => extract_claims(&args[1..]).await,
        "harvest" => harvest(&args[1..]).await,
        "export" => export(&args[1..]).await,
        other => {
            eprintln!("error: unknown verifier subcommand `{other}`");
            help::print(&HELP);
            2
        }
    }
}

/// `extract-claims` — the single-pair claim-extraction seam. Stdin is the
/// default input so harness drivers can pipe long answers without argv
/// length limits or shell-quoting hazards (same rationale as score-answer).
async fn extract_claims(rest: &[String]) -> i32 {
    let mut input: Option<PathBuf> = None;
    let mut model = sovereign_core::role::default_profile_for(sovereign_core::role::Role::Critic)
        .preferred_tier
        .model_stem()
        .to_string();
    let mut base_url = "http://localhost:9741".to_string();
    let mut max_claims: usize = 10;

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
            "--input" => input = Some(PathBuf::from(val!("--input"))),
            "--model" => model = val!("--model"),
            "--base-url" => base_url = val!("--base-url"),
            "--max-claims" => match val!("--max-claims").parse::<usize>() {
                Ok(n) if n > 0 => max_claims = n,
                _ => {
                    eprintln!("error: --max-claims must be a positive integer");
                    return 2;
                }
            },
            "--help" | "-h" => {
                eprintln!("usage: svrn bench verifier extract-claims [--input <file>] [--model <stem>] [--base-url <url>] [--max-claims N]");
                eprintln!("  reads {{\"question\",\"answer\"}} JSON from --input or stdin; writes {{\"claims\":[..]}} to stdout");
                return 0;
            }
            other => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }

    let raw = match &input {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("error: could not read {p:?}: {e}");
                return 1;
            }
        },
        None => {
            use std::io::Read as _;
            let mut s = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut s) {
                eprintln!("error: could not read stdin: {e}");
                return 1;
            }
            s
        }
    };
    let rec: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: input is not valid JSON: {e}");
            return 2;
        }
    };
    let question = rec
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let answer = rec
        .get("answer")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if question.trim().is_empty() || answer.trim().is_empty() {
        eprintln!("error: input must carry non-empty `question` and `answer`");
        return 2;
    }

    let v1 = format!("{}/v1", base_url.trim_end_matches('/'));
    let provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(&v1, None, &model, PROVIDER_CTX));

    match extract_claim_list(
        &provider,
        &question,
        &answer,
        max_claims,
        ShardingPrivacy::LocalOnly,
    )
    .await
    {
        Some(claims) => {
            println!("{}", serde_json::json!({ "claims": claims }));
            0
        }
        None => {
            eprintln!("error: claim-list extraction failed (inference error) — see the daemon log");
            1
        }
    }
}

/// `harvest` — corpus chunks → evidence windows → production claim
/// extraction → the Stream B harvest artifact (`claims.json`). The artifact
/// carries the windows INLINE so the pure flywheel generator never touches
/// an index.
async fn harvest(rest: &[String]) -> i32 {
    let mut corpus: Option<String> = None;
    let mut out = PathBuf::from("claims.json");
    let mut model = sovereign_core::role::default_profile_for(sovereign_core::role::Role::Critic)
        .preferred_tier
        .model_stem()
        .to_string();
    let mut base_url = "http://localhost:9741".to_string();
    let mut max_claims: usize = 8;
    let mut window: usize = 2;
    let mut limit: usize = 0;
    let mut entities: Option<PathBuf> = None;
    let mut distractors: Option<PathBuf> = None;

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
            "--corpus" => corpus = Some(val!("--corpus")),
            "--out" => out = PathBuf::from(val!("--out")),
            "--model" => model = val!("--model"),
            "--base-url" => base_url = val!("--base-url"),
            "--max-claims" => match val!("--max-claims").parse::<usize>() {
                Ok(n) if n > 0 => max_claims = n,
                _ => {
                    eprintln!("error: --max-claims must be a positive integer");
                    return 2;
                }
            },
            "--window" => match val!("--window").parse::<usize>() {
                Ok(n) if n > 0 => window = n,
                _ => {
                    eprintln!("error: --window must be a positive integer");
                    return 2;
                }
            },
            "--limit" => match val!("--limit").parse::<usize>() {
                Ok(n) => limit = n,
                _ => {
                    eprintln!("error: --limit must be an integer (0 = all windows)");
                    return 2;
                }
            },
            "--entities" => entities = Some(PathBuf::from(val!("--entities"))),
            "--distractors" => distractors = Some(PathBuf::from(val!("--distractors"))),
            "--help" | "-h" => {
                eprintln!("usage: svrn bench verifier harvest --corpus <id> [--out claims.json] [--model <stem>] [--base-url <url>] [--max-claims N] [--window CHUNKS] [--limit WINDOWS] [--entities <clusters.json>] [--distractors <docs.json>]");
                return 0;
            }
            other => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }
    let Some(corpus_id) = corpus else {
        eprintln!("error: --corpus <id> is required (an installed bench corpus, e.g. chaos-saltgrass)");
        return 2;
    };

    // Open the corpus by id under the canonical index dir (the corpus-search
    // idiom) and pull every chunk — the windows are consecutive chunks in
    // chunk-id order, the same adjacency retrieval windows have.
    let index_dir = sovereign_core::setup_config::SetupConfig::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("indexes");
    let path = index_dir.join(&corpus_id);
    let index = match CorpusIndex::open(&path).await {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("error: open corpus `{corpus_id}` at {}: {e}", path.display());
            return 1;
        }
    };
    let mut rows: Vec<_> = match index.all_chunks_with_embeddings().await {
        Ok(r) => r.into_iter().map(|(row, _)| row).collect(),
        Err(e) => {
            eprintln!("error: read chunks of `{corpus_id}`: {e}");
            return 1;
        }
    };
    rows.sort_by_key(|r| r.id);
    if rows.is_empty() {
        eprintln!("error: corpus `{corpus_id}` has zero chunks — nothing to harvest");
        return 1;
    }

    let v1 = format!("{}/v1", base_url.trim_end_matches('/'));
    let provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(&v1, None, &model, PROVIDER_CTX));

    let mut items: Vec<adv::HarvestItem> = Vec::new();
    let mut failed_windows = 0usize;
    for (wi, win) in rows.chunks(window).enumerate() {
        if limit > 0 && wi >= limit {
            break;
        }
        let title = win
            .iter()
            .find_map(|r| r.title.clone())
            .unwrap_or_else(|| corpus_id.clone());
        let question = format!("What does this passage from {title} establish?");
        let answer = win
            .iter()
            .map(|r| r.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let chunk_texts: Vec<String> = win.iter().map(|r| r.content.clone()).collect();
        let chunk_ids: Vec<String> = win.iter().map(|r| r.id.to_string()).collect();
        match extract_claim_list(
            &provider,
            &question,
            &answer,
            max_claims,
            ShardingPrivacy::LocalOnly,
        )
        .await
        {
            Some(claims) => {
                for (ci, claim) in claims.into_iter().enumerate() {
                    items.push(adv::HarvestItem {
                        id: format!("{corpus_id}-w{wi:04}-c{ci}"),
                        question: question.clone(),
                        claim,
                        evidence_chunks: chunk_texts.clone(),
                        evidence_chunk_ids: chunk_ids.clone(),
                    });
                }
            }
            None => {
                failed_windows += 1;
                eprintln!("[harvest] window {wi}: extraction failed — skipped");
            }
        }
        eprintln!(
            "[harvest] window {wi}: {} claims total",
            items.len()
        );
    }

    let entities: Vec<adv::EntityCluster> = match &entities {
        Some(p) => match read_json(p) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: --entities {p:?}: {e}");
                return 2;
            }
        },
        None => Vec::new(),
    };
    let distractors: Vec<adv::DistractorDoc> = match &distractors {
        Some(p) => match read_json(p) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: --distractors {p:?}: {e}");
                return 2;
            }
        },
        None => Vec::new(),
    };

    let n_items = items.len();
    let hf = adv::HarvestFile {
        schema_version: adv::HARVEST_SCHEMA_VERSION,
        corpus_id: corpus_id.clone(),
        items,
        entities,
        distractors,
    };
    if let Some(dir) = out.parent() {
        if !dir.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("error: create {dir:?}: {e}");
                return 1;
            }
        }
    }
    match serde_json::to_string_pretty(&hf).map_err(|e| e.to_string()).and_then(|s| {
        std::fs::write(&out, s).map_err(|e| e.to_string())
    }) {
        Ok(()) => {
            println!(
                "{}",
                serde_json::json!({
                    "corpus_id": corpus_id,
                    "claims": n_items,
                    "failed_windows": failed_windows,
                    "out": out.display().to_string(),
                })
            );
            if n_items == 0 {
                eprintln!("error: harvest produced zero claims — artifact written but unusable");
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!("error: write {out:?}: {e}");
            1
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| e.to_string())
}

/// `export` — harvest artifact → deterministic corruption cases → Stream B
/// JSONL. Every case is re-validated against the PRODUCTION
/// `value_present_in_chunks` (the flywheel generated against a pinned port);
/// any failure aborts the export, because a Stream B label is by
/// construction or it is nothing.
async fn export(rest: &[String]) -> i32 {
    let mut harvest_path: Option<PathBuf> = None;
    let mut out = PathBuf::from("stream_b.jsonl");
    let mut n: usize = 1000;
    let mut seed: u64 = 17;

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
            "--harvest" => harvest_path = Some(PathBuf::from(val!("--harvest"))),
            "--out" => out = PathBuf::from(val!("--out")),
            "--n" => match val!("--n").parse::<usize>() {
                Ok(v) if v > 0 => n = v,
                _ => {
                    eprintln!("error: --n must be a positive integer");
                    return 2;
                }
            },
            "--seed" => match val!("--seed").parse::<u64>() {
                Ok(v) => seed = v,
                _ => {
                    eprintln!("error: --seed must be a u64");
                    return 2;
                }
            },
            "--help" | "-h" => {
                eprintln!("usage: svrn bench verifier export --harvest <claims.json|dir> [--out stream_b.jsonl] [--n N] [--seed S]");
                return 0;
            }
            other => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }
    let Some(hp) = harvest_path else {
        eprintln!("error: --harvest <claims.json|dir> is required (produce one with `svrn bench verifier harvest`)");
        return 2;
    };

    let harvest = match adv::load_harvest(&hp) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let cases = adv::generate_cases(n, seed, &harvest);
    if cases.is_empty() {
        eprintln!("error: generator produced zero cases from {} harvest items", harvest.items.len());
        return 1;
    }

    // The production checker gets the final word on every constructed label.
    let mut failures = 0usize;
    for c in &cases {
        if let Err(e) = adv::validate_site(c) {
            eprintln!("[export] SITE CONTRACT FAILURE: {e}");
            failures += 1;
        }
        if let Err(e) = production_site_check(c) {
            eprintln!("[export] PRODUCTION CHECK FAILURE: {e}");
            failures += 1;
        }
    }
    if failures > 0 {
        eprintln!(
            "error: {failures} case(s) failed re-validation — nothing written. A constructed label that the production checker rejects is a generator bug, not a data point."
        );
        return 1;
    }

    if let Some(dir) = out.parent() {
        if !dir.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("error: create {dir:?}: {e}");
                return 1;
            }
        }
    }
    let mut lines = String::new();
    for c in &cases {
        match serde_json::to_string(c) {
            Ok(l) => {
                lines.push_str(&l);
                lines.push('\n');
            }
            Err(e) => {
                eprintln!("error: serialize case `{}`: {e}", c.id);
                return 1;
            }
        }
    }
    if let Err(e) = std::fs::write(&out, lines) {
        eprintln!("error: write {out:?}: {e}");
        return 1;
    }

    let ungrounded = cases
        .iter()
        .filter(|c| c.label == adv::CaseLabel::Ungrounded)
        .count();
    let mut by_kind: std::collections::BTreeMap<&str, usize> = Default::default();
    for c in &cases {
        *by_kind.entry(c.kind.as_str()).or_insert(0) += 1;
    }
    println!(
        "{}",
        serde_json::json!({
            "cases": cases.len(),
            "ungrounded": ungrounded,
            "grounded": cases.len() - ungrounded,
            "by_kind": by_kind,
            "seed": seed,
            "out": out.display().to_string(),
        })
    );
    0
}

/// Mirror of the flywheel's `validate_site`, but every value-presence check
/// runs through the PRODUCTION `value_present_in_chunks` — the genuine
/// article, not the port the pure crate generates against.
fn production_site_check(c: &adv::StreamBCase) -> Result<(), String> {
    let ev = &c.evidence_chunks;
    match (&c.label, &c.witness) {
        (adv::CaseLabel::Ungrounded, adv::SiteWitness::InjectedAbsent { injected, original }) => {
            if value_present_in_chunks(injected, ev) {
                return Err(format!(
                    "case `{}`: injected `{injected}` grounds in the window per PRODUCTION checker",
                    c.id
                ));
            }
            if let Some(o) = original {
                if !value_present_in_chunks(o, ev) {
                    return Err(format!(
                        "case `{}`: displaced original `{o}` does not ground per PRODUCTION checker",
                        c.id
                    ));
                }
            }
            Ok(())
        }
        (
            adv::CaseLabel::Ungrounded,
            adv::SiteWitness::PolarityFlip {
                marker,
                original_terms,
            },
        ) => {
            if !contains_ci(&c.claim, marker) {
                return Err(format!("case `{}`: marker `{marker}` missing from claim", c.id));
            }
            if original_terms.is_empty()
                || !original_terms.iter().all(|t| value_present_in_chunks(t, ev))
            {
                return Err(format!(
                    "case `{}`: original terms do not ground per PRODUCTION checker",
                    c.id
                ));
            }
            Ok(())
        }
        (
            adv::CaseLabel::Ungrounded,
            adv::SiteWitness::Chimera {
                connective,
                frag_a_terms,
                frag_b_terms,
                boundary,
            },
        ) => {
            if *boundary == 0 || *boundary >= ev.len() {
                return Err(format!("case `{}`: bad chimera boundary", c.id));
            }
            if ev.iter().any(|chunk| contains_ci(chunk, connective)) {
                return Err(format!("case `{}`: connective present in a chunk", c.id));
            }
            let (a, b) = ev.split_at(*boundary);
            if !frag_a_terms.iter().all(|t| value_present_in_chunks(t, a))
                || !frag_b_terms.iter().all(|t| value_present_in_chunks(t, b))
            {
                return Err(format!(
                    "case `{}`: chimera fragments do not ground per PRODUCTION checker",
                    c.id
                ));
            }
            Ok(())
        }
        (
            adv::CaseLabel::Ungrounded,
            adv::SiteWitness::DistractorOnly {
                value,
                distractor_text,
                ..
            },
        ) => {
            if !value_present_in_chunks(value, std::slice::from_ref(distractor_text)) {
                return Err(format!(
                    "case `{}`: absorbed value does not ground in its distractor doc",
                    c.id
                ));
            }
            if value_present_in_chunks(value, ev) {
                return Err(format!(
                    "case `{}`: absorbed value grounds in the window per PRODUCTION checker",
                    c.id
                ));
            }
            Ok(())
        }
        (adv::CaseLabel::Grounded, adv::SiteWitness::Supported { terms }) => {
            if terms.is_empty() || !terms.iter().all(|t| value_present_in_chunks(t, ev)) {
                return Err(format!(
                    "case `{}`: grounded terms do not ALL ground per PRODUCTION checker",
                    c.id
                ));
            }
            Ok(())
        }
        (label, witness) => Err(format!(
            "case `{}`: witness {witness:?} incoherent with label {label:?}",
            c.id
        )),
    }
}
