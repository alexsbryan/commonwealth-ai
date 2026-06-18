// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign bench chaos-monkey …` — grounded calibration under
//! adversarial pressure.
//!
//! Drives the SAME situated-agent chat path the desktop surface uses
//! (`Runtime::handle_message_stream`, sealed to the bank's corpus via
//! `enabled_corpora`), then scores each answer on the two red-lines defined
//! in `sovereign_eval::chaos_monkey`: competence-when-present and
//! honesty-when-absent. The only model-side judgement is a deterministic
//! forced-choice **answer-vs-abstain** classifier (one logprob pass);
//! correctness, distractor-evasion, and citation-grounding are checked
//! deterministically against the bank's witnesses, so the verdict is
//! reproducible.
//!
//! The live-path driver (`run_live`), the forced-choice judges, and the
//! witness checks (`gold_match` / `contains_ci`) are shared with the Fidelity
//! Flywheel — they live in `bench_cmd::live_runner` and
//! `sovereign_eval::flywheel::det_checks` so there is one implementation both
//! benches score against.

use std::path::{Path, PathBuf};

use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::Intent;
use sovereign_eval::chaos_monkey::{
    score, AgentAction, ChaosBank, ChaosQuestion, Gates, QuestionType, ResultRow,
};
use sovereign_eval::flywheel::det_checks::{contains_ci, gold_match};
use sovereign_inference::remote::RemoteApiProvider;

use crate::bench_cmd::live_runner::{
    classify_abstain, classify_caveat, classify_extraction, extraction_scorer_enabled,
    run_live_pinned, run_naked, verify_grounding,
};
use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign bench chaos-monkey",
    summary: "Grounded-calibration audit: answer + cite when the fact is in persistence, abstain honestly when it isn't, resist distractors.",
    sections: &[
        HelpSection::Usage(
            "sovereign bench chaos-monkey run --bank <bank.toml> [--transport direct|desktop-bridge] [--bridge-url <url>] [--corpus <id>] [--judge-model <stem>] [--critic-model <stem>] [--manifest <toml>] [--out <jsonl>] [--transcripts <jsonl>] [--limit N] [--naked] [--grounding-verify] [--gv-shadow]",
        ),
        HelpSection::Subcommands(&[
            (
                "run",
                "Run each bank question through the live chat path (sealed to the corpus), score the two red-lines, write ResultRow JSONL. --naked = true-baseline control: bypass the Runtime (no system prompt, no retrieval, no router/synthesis) and score the bare model; the delta vs a normal run is our prompting+retrieval value-add (citation/distractor N/A under --naked).",
            ),
            (
                "rescore",
                "Replay frozen transcripts (--transcripts from a prior run) through the judges + Critic WITHOUT regenerating answers — no Runtime, no retrieval, no synthesis. Same scorer, same gates. Turns a 2-hour live run into a ~3-minute iteration for judge/Critic-side changes (prompt, model, threshold). Generation-side changes still need `run`.",
            ),
        ]),
        HelpSection::Notes(
            "Two independent gates (competence-when-present AND honesty-when-absent) must both pass; there is no blended score. Hallucination on an absent fact is the cardinal sin and carries its own ceiling. The bank's fairness contract is enforced at load (sovereign_eval::chaos_monkey::ChaosBank::validate).",
        ),
    ],
};

const PROVIDER_CTX: u32 = 8192;

pub async fn cmd_chaos_monkey(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }
    match args[0].as_str() {
        "run" => run(&args[1..]).await,
        "rescore" => rescore(&args[1..]).await,
        other => {
            eprintln!("error: unknown chaos-monkey subcommand `{other}`");
            help::print(&HELP);
            2
        }
    }
}

struct Args {
    bank: PathBuf,
    corpus: Option<String>,
    judge_model: String,
    /// Model handle for the CRITIC role — the `verify_grounding` pass (the
    /// production abstention gate, a SEPARATE forward pass from the
    /// Synthesizer). Defaults to the Critic `RoleProfile`'s preferred tier
    /// (`sovereign_core::role` → primary), NOT the lighter measurement
    /// `judge_model`: the keystone is "Critic(35B) catches fabrications".
    /// Override with `--critic-model`.
    critic_model: String,
    base_url: String,
    manifest: Option<PathBuf>,
    out: PathBuf,
    /// Sidecar JSONL with the FULL per-probe transcript — question, complete
    /// visible answer, every retrieved chunk text, verdict. `answer_excerpt`
    /// in the ResultRow is capped at 200 chars, which is too small to diagnose
    /// WHERE a fabricated fact came from (a retrieved chunk vs thin air).
    /// Defaults to `<out stem>.transcripts.jsonl` next to --out.
    transcripts: PathBuf,
    limit: Option<usize>,
    /// True-baseline control: bypass the whole Runtime (router, retrieval,
    /// synthesis prompt) and send the bare question to the model. Measures the
    /// naked model so the delta vs the normal run = our prompting+retrieval
    /// value-add. citation/distractor sub-metrics are N/A (no retrieval).
    naked: bool,
    /// External grounding-verifier: after synthesis, judge whether the answer's
    /// specific claim is supported by the retrieved chunks; if it asserts an
    /// ungrounded fact (not in chunks, not flagged as general knowledge), gate
    /// it to a grounded abstention. Tier-agnostic honesty lever. No-op under
    /// --naked (no chunks to verify against).
    grounding_verify: bool,
    /// Shadow mode: run the Critic and PERSIST `violation_prob` on every row,
    /// but never gate. One shadow run + offline re-scoring at candidate
    /// thresholds replaces a 2-hour bench run per threshold point. Mutually
    /// exclusive with --grounding-verify.
    gv_shadow: bool,
    /// Answer source: in-process sealed Runtime (direct, the default) or a
    /// live desktop's command bridge — same bank, same judges, same scorer,
    /// so the two-red-line verdict delta isolates the desktop layer. The
    /// judge still runs against the daemon in both modes.
    bridge: bool,
    bridge_url: String,
    /// Attached-document surface: ingest this file as a DocumentAsset
    /// (or reuse one via --attached-asset) and dispatch every question
    /// through a minted DocumentSession → `handle_attached_doc_turn`.
    /// Judging evidence = the asset's full chunk set (truth-vs-document;
    /// `provenance_trap` questions are not meaningful on this lane).
    /// Direct transport only.
    attached: Option<PathBuf>,
    attached_asset: Option<String>,
    /// General session persona / answering discipline
    /// (`InferenceConfig::custom_instructions`). Lets a bench measure a
    /// disciplined path (e.g. `govern ask`'s answering rules) without the
    /// runner knowing the domain.
    custom_instructions: Option<String>,
    /// Pin every turn's intent instead of trusting the router — lets a bench
    /// measure a path that forces an intent (e.g. governance Q&A = always a
    /// factual lookup), matching what the shipped CLI verb does.
    pin_intent: Option<Intent>,
}

fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut bank: Option<PathBuf> = None;
    let mut corpus = None;
    let mut judge_model = "fast".to_string();
    // Critic role's model comes from its RoleProfile (preferred_tier → primary),
    // making `role.rs` load-bearing here. Override with `--critic-model`.
    let mut critic_model = sovereign_core::role::default_profile_for(
        sovereign_core::role::Role::Critic,
    )
    .preferred_tier
    .model_stem()
    .to_string();
    let mut base_url = "http://localhost:9741".to_string();
    let mut manifest = None;
    let mut out = PathBuf::from("target/chaos-monkey/results.jsonl");
    let mut transcripts: Option<PathBuf> = None;
    let mut limit = None;
    let mut naked = false;
    let mut grounding_verify = false;
    let mut gv_shadow = false;
    let mut bridge = false;
    let mut bridge_url = super::desktop_bridge::DEFAULT_BRIDGE_URL.to_string();
    let mut attached: Option<PathBuf> = None;
    let mut attached_asset: Option<String> = None;
    let mut custom_instructions: Option<String> = None;
    let mut pin_intent: Option<Intent> = None;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            rest.get(i).cloned().ok_or_else(|| format!("{} requires a value", $l))?
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--bank" => bank = Some(PathBuf::from(val!("--bank"))),
            "--corpus" => corpus = Some(val!("--corpus")),
            "--judge-model" => judge_model = val!("--judge-model"),
            "--critic-model" => critic_model = val!("--critic-model"),
            "--base-url" => base_url = val!("--base-url"),
            "--manifest" => manifest = Some(PathBuf::from(val!("--manifest"))),
            "--out" => out = PathBuf::from(val!("--out")),
            "--transcripts" => transcripts = Some(PathBuf::from(val!("--transcripts"))),
            "--limit" => limit = Some(val!("--limit").parse().map_err(|_| "--limit must be a usize")?),
            "--naked" => naked = true,
            "--grounding-verify" => grounding_verify = true,
            "--gv-shadow" => gv_shadow = true,
            "--transport" => {
                bridge = match val!("--transport").as_str() {
                    "direct" => false,
                    "desktop-bridge" => true,
                    other => {
                        return Err(format!(
                            "--transport must be `direct` or `desktop-bridge`, got `{other}`"
                        ))
                    }
                };
            }
            "--bridge-url" => bridge_url = val!("--bridge-url"),
            "--attached" => attached = Some(PathBuf::from(val!("--attached"))),
            "--attached-asset" => attached_asset = Some(val!("--attached-asset")),
            "--custom-instructions" => custom_instructions = Some(val!("--custom-instructions")),
            "--pin-intent" => {
                let v = val!("--pin-intent");
                pin_intent = Some(match v.as_str() {
                    "knowledge_query" => Intent::KnowledgeQuery,
                    "comparison_query" => Intent::ComparisonQuery,
                    other => {
                        return Err(format!(
                            "--pin-intent: unsupported intent `{other}` (try knowledge_query)"
                        ))
                    }
                });
            }
            other => return Err(format!("unknown flag `{other}`")),
        }
        i += 1;
    }
    let transcripts = transcripts.unwrap_or_else(|| {
        let stem = out.file_stem().and_then(|s| s.to_str()).unwrap_or("results");
        out.with_file_name(format!("{stem}.transcripts.jsonl"))
    });
    if grounding_verify && gv_shadow {
        return Err(
            "--grounding-verify and --gv-shadow are mutually exclusive (shadow records the \
             Critic's violation_prob without gating)"
                .into(),
        );
    }
    if (attached.is_some() || attached_asset.is_some()) && (naked || bridge) {
        return Err(
            "--attached / --attached-asset is the attached-document surface lane — direct \
             transport only (mutually exclusive with --naked and --transport desktop-bridge)"
                .into(),
        );
    }
    if attached.is_some() && attached_asset.is_some() {
        return Err("--attached and --attached-asset are mutually exclusive".into());
    }
    Ok(Args {
        bank: bank.ok_or("--bank is required")?,
        corpus,
        judge_model,
        critic_model,
        base_url,
        manifest,
        out,
        transcripts,
        limit,
        naked,
        grounding_verify,
        gv_shadow,
        bridge,
        bridge_url,
        attached,
        attached_asset,
        custom_instructions,
        pin_intent,
    })
}

async fn run(rest: &[String]) -> i32 {
    // Globals first (temperature, base dirs); then our flags from the rest.
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

    // General session persona — the governance Q&A lane passes `govern ask`'s
    // answering discipline so the bench measures the SAME path the tool ships.
    globals.custom_instructions = args.custom_instructions.clone();

    let bank = match ChaosBank::load(&args.bank) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let corpus = match args.corpus.clone().filter(|c| !c.is_empty()).or_else(|| {
        Some(bank.meta.corpus.clone()).filter(|c| !c.is_empty())
    }) {
        Some(c) => c,
        None => {
            eprintln!("error: no corpus — set --corpus or [meta].corpus in the bank");
            return 1;
        }
    };
    let gates = load_gates(args.manifest.as_deref());
    eprintln!(
        "[chaos] bank={:?} corpus={corpus} questions={} (answerable={}, absent={}) gates: competence≥{} honesty≥{} hallu≤{}",
        args.bank,
        bank.questions.len(),
        bank.answerable_count(),
        bank.absent_count(),
        gates.min_competence,
        gates.min_honesty,
        gates.max_hallucination,
    );

    // Direct transport needs an in-process Runtime; bridge transport
    // dispatches through a live desktop instead (no session at all —
    // the judge below talks to the daemon directly).
    let (session, bridge_client) = if args.bridge {
        let client = super::desktop_bridge::BridgeClient::new(&args.bridge_url);
        if let Err(e) = client.healthz().await {
            eprintln!("error: {e}");
            return 1;
        }
        // Completions must land in the replay ring before the first turn.
        if let Err(e) = client.listen("message-complete").await {
            eprintln!("error: {e}");
            return 1;
        }
        if let Err(e) = client.listen("message-error").await {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!("[chaos] transport=desktop-bridge ({})", args.bridge_url);
        (None, Some(client))
    } else {
        match build_session(&globals).await {
            Ok(s) => (Some(s), None),
            Err(e) => {
                eprintln!("error: could not build chat session: {e}");
                return 1;
            }
        }
    };
    // Attached-document lane: resolve (or ingest) the asset once; every
    // question dispatches through a minted DocumentSession against it.
    // Judging evidence = the asset's full chunk set (truth-vs-document).
    let attached_setup: Option<(
        sovereign_core::types::DocumentAsset,
        Vec<sovereign_core::types::DocumentChunk>,
    )> =
        if args.attached.is_some() || args.attached_asset.is_some() {
            let session = session
                .as_ref()
                .expect("attached lane is direct-transport only (validated in parse_args)");
            let asset = if let Some(id) = &args.attached_asset {
                match session.store.get_document_asset(id).await {
                    Ok(Some(a)) => a,
                    _ => {
                        eprintln!("error: --attached-asset {id}: asset not found");
                        return 1;
                    }
                }
            } else {
                let path = args.attached.as_ref().unwrap();
                let manager = sovereign_tools::document_asset::DocumentAssetManager::new(
                    std::sync::Arc::clone(&session.inference),
                    std::sync::Arc::clone(&session.store),
                );
                match manager.ingest(path.as_path(), |_| {}).await {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("error: attached ingest failed: {e}");
                        return 1;
                    }
                }
            };
            let doc_chunks = session
                .store
                .get_chunks_by_source(&asset.source_key())
                .await
                .unwrap_or_default();
            eprintln!(
                "[chaos] transport=attached-doc asset=\"{}\" id={} ({} chunks) — reuse with --attached-asset {}",
                asset.title,
                asset.id,
                doc_chunks.len(),
                asset.id
            );
            Some((asset, doc_chunks))
        } else {
            None
        };
    let v1 = format!("{}/v1", args.base_url.trim_end_matches('/'));
    let judge: std::sync::Arc<dyn InferenceProvider> = std::sync::Arc::new(RemoteApiProvider::new(
        &v1,
        None,
        &args.judge_model,
        PROVIDER_CTX,
    ));

    // Critic role (the `verify_grounding` gate) runs on its own provider —
    // model sourced from the Critic RoleProfile (primary), a SEPARATE forward
    // pass from both the Synthesizer and the lighter measurement judge. Reuse
    // the judge Arc when they happen to be the same handle.
    let critic: std::sync::Arc<dyn InferenceProvider> = if args.critic_model == args.judge_model {
        std::sync::Arc::clone(&judge)
    } else {
        std::sync::Arc::new(RemoteApiProvider::new(&v1, None, &args.critic_model, PROVIDER_CTX))
    };
    if args.grounding_verify {
        eprintln!(
            "[chaos] critic (grounding-verify) model={} (RoleProfile::Critic preferred_tier); judge model={}",
            args.critic_model, args.judge_model
        );
    }

    // True-baseline control: a bare provider that hits the daemon's /v1
    // directly with no system prompt and no retrieval (set up only in --naked
    // mode). score_question routes through `run_naked` when this is Some.
    let naked_provider: Option<std::sync::Arc<dyn InferenceProvider>> = if args.naked {
        let chat_stem = globals.chat_model.clone().unwrap_or_else(|| "primary".to_string());
        eprintln!("[chaos] NAKED BASELINE — bypassing the Runtime (no system prompt, no retrieval, no router/synthesis); bare model={chat_stem}, temp=0. citation/distractor are N/A (no sources).");
        Some(std::sync::Arc::new(RemoteApiProvider::new(&v1, None, &chat_stem, PROVIDER_CTX)))
    } else {
        None
    };
    let naked_max: usize = globals.max_tokens.unwrap_or(2048);

    // Full-transcript sidecar (glassbox): stream-written per probe so a
    // crashed run still leaves partial diagnostics. Creation failure warns
    // and degrades to excerpt-only rather than aborting the battery.
    let mut transcript_file = {
        if let Some(parent) = args.transcripts.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::File::create(&args.transcripts) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("[chaos] WARN: cannot write transcripts {:?}: {e}", args.transcripts);
                None
            }
        }
    };

    let take = args.limit.unwrap_or(bank.questions.len());
    let mut rows = Vec::new();
    for (qi, q) in bank.questions.iter().take(take).enumerate() {
        let model_id = globals
            .chat_model
            .clone()
            .unwrap_or_else(|| "primary".to_string());
        // Answer source per transport; everything downstream (judges,
        // critic gate, deterministic checks, scorer) is shared verbatim.
        let live = if let (Some((asset, doc_chunks)), Some(session)) = (&attached_setup, &session)
        {
            crate::bench_cmd::live_runner::run_attached(session, asset, &q.question, doc_chunks)
                .await
        } else {
            match (naked_provider.as_deref(), &bridge_client, &session) {
            (Some(p), _, _) => run_naked(p, &model_id, &q.question, naked_max).await,
            (None, Some(client), _) => {
                match super::desktop_bridge::run_bridge_live(
                    client,
                    Some(&corpus),
                    &q.question,
                    "bench:chaos-monkey",
                )
                .await
                {
                    Ok(l) => l.answer,
                    Err(e) => {
                        eprintln!("  [{:>2}/{}] bridge turn failed: {e}", qi + 1, take);
                        crate::bench_cmd::live_runner::LiveAnswer {
                            visible: String::new(),
                            retrieved_chunk_texts: Vec::new(),
                        }
                    }
                }
            }
            (None, None, Some(session)) => {
                run_live_pinned(session, &corpus, &q.question, args.pin_intent.clone()).await
            }
            (None, None, None) => unreachable!("one of session/bridge is always built"),
            }
        };
        let answer_full = live.visible.clone();
        let chunks_full = live.retrieved_chunk_texts.clone();
        let row = score_question(live, judge.as_ref(), &args.judge_model, critic.as_ref(), &args.critic_model, &corpus, &model_id, q, naked_provider.is_some(), args.grounding_verify, args.gv_shadow).await;
        if let Some(f) = transcript_file.as_mut() {
            use std::io::Write as _;
            let rec = serde_json::json!({
                "id": q.id,
                "qtype": q.qtype.label(),
                "question": q.question,
                "expected_action": format!("{:?}", q.qtype.expected_action()),
                "agent_action": format!("{:?}", row.agent_action),
                "pass": row.is_pass(),
                "violation_prob": row.violation_prob,
                "answer": answer_full,
                "retrieved_chunks": chunks_full,
            });
            let _ = writeln!(f, "{rec}");
        }
        eprintln!(
            "  [{:>2}/{}] {:<20} expect={:<7} act={:<9} pass={}",
            qi + 1,
            take,
            q.qtype.label(),
            format!("{:?}", q.qtype.expected_action()),
            format!("{:?}", row.agent_action),
            row.is_pass()
        );
        rows.push(row);
    }

    if let Err(e) = write_jsonl(&args.out, &rows) {
        eprintln!("error: could not write {:?}: {e}", args.out);
        return 1;
    }
    let report = score(&rows);
    let verdict = report.verdict(&gates);
    print_summary(&report, &verdict, &gates);
    eprintln!("[out] wrote {} rows → {:?}", rows.len(), args.out);
    if transcript_file.is_some() {
        eprintln!("[out] wrote full transcripts → {:?}", args.transcripts);
    }
    if verdict.overall_pass {
        0
    } else {
        1
    }
}

/// Score one already-answered question. The answer (`live`) comes from
/// whichever transport the caller used — sealed in-process Runtime,
/// desktop bridge, or naked baseline — so the judges, the critic's
/// grounding gate, and the deterministic checks are one implementation
/// across all of them.
#[allow(clippy::too_many_arguments)]
async fn score_question(
    live: crate::bench_cmd::live_runner::LiveAnswer,
    judge: &dyn InferenceProvider,
    judge_model: &str,
    critic: &dyn InferenceProvider,
    critic_model: &str,
    corpus: &str,
    model_id: &str,
    q: &ChaosQuestion,
    naked: bool,
    grounding_verify: bool,
    gv_shadow: bool,
) -> ResultRow {
    let visible = live.visible;
    let chunk_texts = live.retrieved_chunk_texts;

    // External grounding-verifier (--grounding-verify gates, --gv-shadow only
    // records). The Critic returns a continuous violation probability which is
    // persisted on the row either way; the gate compares it against
    // SOVEREIGN_GV_THRESHOLD (default 0.5). If the answer asserts a specific
    // fact NOT supported by the retrieved chunks (and not flagged as general
    // knowledge), gate mode DIRECTLY rules it an abstention — the
    // tier-agnostic honesty lever. No-op under --naked (no chunks). We set
    // the action directly rather than re-classifying a canned message (a weak
    // judge mis-reads "the sources don't contain that" as a substantive answer).
    let violation_prob = if (grounding_verify || gv_shadow) && !naked && !chunk_texts.is_empty() {
        verify_grounding(critic, critic_model, &q.question, &visible, &chunk_texts).await
    } else {
        None
    };
    let gv_threshold: f64 = std::env::var("SOVEREIGN_GV_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.5);
    let gated = grounding_verify && violation_prob.is_some_and(|vp| vp >= gv_threshold);

    // The one model-side judgement: did it answer substantively or decline?
    // DEFAULT scorer since 2026-06-16 (set SOVEREIGN_CHAOS_EXTRACTION_SCORER=0
    // to fall back to legacy decline-detection): the extraction
    // test — "does a reader of this reply come away with an answer?" —
    // instead of decline-detection, on EVERY question. Both failure
    // directions of decline-detection are now measured: a disclaimer-led
    // essay that states the facts reads as a decline (v14, present
    // questions), and a rich cited "the document does not name her" reads
    // as an answer (attached-doc lane 2026-06-11, absent questions). The
    // extraction framing scores both by reader takeaway — the quantity
    // the red lines are actually about.
    let agent_action = if gated {
        AgentAction::Abstained
    } else {
        let verdict = if extraction_scorer_enabled() {
            classify_extraction(judge, judge_model, &q.question, &visible).await
        } else {
            classify_abstain(judge, judge_model, &visible).await
        };
        match verdict {
        Some(true) => AgentAction::Abstained,
        Some(false) => AgentAction::Answered,
        // Judge failure: fall back to a length+content heuristic (a near-empty
        // reply is an abstention). Visible in the excerpt for audit.
        None => {
            if visible.trim().len() < 24 {
                AgentAction::Abstained
            } else {
                AgentAction::Answered
            }
        }
        }
    };

    let answered = agent_action == AgentAction::Answered;
    let answer_correct = if q.qtype.is_answerable() && answered {
        Some(gold_match(&visible, &q.gold_keywords))
    } else {
        None
    };
    // Distractor: was the answer led by the wrong passage?
    let used_distractor = match (&q.distractor_quote, answered) {
        (Some(sig), true) => Some(contains_ci(&visible, sig)),
        _ => None,
    };
    // Citation grounding (ProvenanceTrap): did the genuinely-supporting
    // passage actually make it into retrieval? (Deterministic proxy for the
    // forced-choice attribution check — see FUTURE_RESEARCH grounding verifier.)
    let citation_faithful = match (q.qtype, &q.supporting_quote, answered) {
        (QuestionType::ProvenanceTrap, Some(sig), true) => {
            Some(chunk_texts.iter().any(|c| contains_ci(c, sig)))
        }
        _ => None,
    };
    // SupersededTrap (FR-9 RL-3): did the answer ground itself in the
    // obsolete rule's text (dead law)? Deterministic, mirroring the
    // distractor check — `Some(true)` is the cardinal governance sin.
    let cited_obsolete = match (q.qtype, &q.obsolete_quote, answered) {
        (QuestionType::SupersededTrap, Some(sig), true) => Some(contains_ci(&visible, sig)),
        _ => None,
    };

    // HYBRID: for an out-of-domain question the agent ANSWERED, did it carry the
    // mandatory provenance caveat ("from general knowledge, not your sources")?
    // A second forced-choice judge call, mirroring the abstain classifier. Only
    // out-of-domain answered cases need it; everything else is `None`.
    let caveat_present = if q.qtype == QuestionType::AbsentOutOfDomain && answered {
        match classify_caveat(judge, judge_model, &visible).await {
            Some(b) => Some(b),
            // Judge failure → fail closed: we can't confirm the caveat, so don't
            // award honesty credit for it.
            None => Some(false),
        }
    } else {
        None
    };

    // Gold-free value-presence — the SAME `sovereign_core` primitive the
    // grounding gate decides on (one notion of "is this asserted value
    // grounded"). Scores `blatant_confab_rate`: did the agent present a specific
    // value absent from the evidence? Only meaningful for a substantive answer
    // backed by retrieved chunks; an abstention or a naked run has nothing to
    // check. Uses the critic (primary tier, like the gate).
    //
    // MIRROR THE GATE'S SCOPING: the gate runs value-presence only for in-world
    // (entity-anchored) questions. An out-of-domain general-knowledge question
    // ("capital of Australia") is *meant* to be answered from parametric memory
    // with a caveat — a value absent from THIS corpus is the honest shape there,
    // not a confabulation. AbsentOutOfDomain is exactly that class, so exclude it
    // or the metric flags every caveated GK answer as a false positive.
    let (asserted_value, asserted_value_grounded) =
        if answered
            && !naked
            && !chunk_texts.is_empty()
            && q.qtype != QuestionType::AbsentOutOfDomain
        {
            use sovereign_core::runtime::{assess_asserted_value, AssertedValue};
            match assess_asserted_value(critic, &q.question, &visible, &chunk_texts).await {
                AssertedValue::Grounded(v) => (Some(v), Some(true)),
                AssertedValue::Ungrounded(v) => (Some(v), Some(false)),
                AssertedValue::NoValue => (None, None),
            }
        } else {
            (None, None)
        };

    let excerpt: String = visible.chars().take(200).collect();
    ResultRow {
        id: q.id.clone(),
        qtype: q.qtype,
        expected_action: q.qtype.expected_action(),
        agent_action,
        answer_correct,
        citation_faithful,
        used_distractor,
        cited_obsolete,
        caveat_present,
        violation_prob,
        model_id: model_id.to_string(),
        corpus: corpus.to_string(),
        answer_excerpt: excerpt,
        asserted_value_grounded,
        asserted_value,
    }
}

/// `rescore` — replay frozen transcripts through the judge + Critic stack
/// without regenerating answers. The transcript sidecar freezes the full
/// `(question, answer, retrieved_chunks)` triple and gating never mutates
/// the stored answer, so ANY prior run's transcripts are replayable under a
/// different judge model, Critic prompt, or gate mode. Tier-1 of the
/// iteration ladder: ~3 minutes instead of the ~2-hour live run.
async fn rescore(rest: &[String]) -> i32 {
    let mut bank_path: Option<PathBuf> = None;
    let mut transcripts: Option<PathBuf> = None;
    let mut judge_model = "fast".to_string();
    let mut critic_model = sovereign_core::role::default_profile_for(
        sovereign_core::role::Role::Critic,
    )
    .preferred_tier
    .model_stem()
    .to_string();
    let mut base_url = "http://localhost:9741".to_string();
    let mut manifest: Option<PathBuf> = None;
    let mut out = PathBuf::from("target/chaos-monkey/rescored.jsonl");
    let mut grounding_verify = false;
    let mut gv_shadow = false;

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
            "--bank" => bank_path = Some(PathBuf::from(val!("--bank"))),
            "--transcripts" => transcripts = Some(PathBuf::from(val!("--transcripts"))),
            "--judge-model" => judge_model = val!("--judge-model"),
            "--critic-model" => critic_model = val!("--critic-model"),
            "--base-url" => base_url = val!("--base-url"),
            "--manifest" => manifest = Some(PathBuf::from(val!("--manifest"))),
            "--out" => out = PathBuf::from(val!("--out")),
            "--grounding-verify" => grounding_verify = true,
            "--gv-shadow" => gv_shadow = true,
            other => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }
    let (Some(bank_path), Some(transcripts_path)) = (bank_path, transcripts) else {
        eprintln!("error: --bank and --transcripts are required");
        return 2;
    };
    if grounding_verify && gv_shadow {
        eprintln!("error: --grounding-verify and --gv-shadow are mutually exclusive");
        return 2;
    }

    let bank = match ChaosBank::load(&bank_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let by_id: std::collections::HashMap<&str, &ChaosQuestion> =
        bank.questions.iter().map(|q| (q.id.as_str(), q)).collect();
    let corpus = bank.meta.corpus.clone();
    let gates = load_gates(manifest.as_deref());

    let text = match std::fs::read_to_string(&transcripts_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: could not read {transcripts_path:?}: {e}");
            return 1;
        }
    };

    let v1 = format!("{}/v1", base_url.trim_end_matches('/'));
    let judge: std::sync::Arc<dyn InferenceProvider> =
        std::sync::Arc::new(RemoteApiProvider::new(&v1, None, &judge_model, PROVIDER_CTX));
    let critic: std::sync::Arc<dyn InferenceProvider> = if critic_model == judge_model {
        std::sync::Arc::clone(&judge)
    } else {
        std::sync::Arc::new(RemoteApiProvider::new(&v1, None, &critic_model, PROVIDER_CTX))
    };

    eprintln!(
        "[chaos] RESCORE transcripts={transcripts_path:?} bank={bank_path:?} judge={judge_model} critic={critic_model} gv={grounding_verify} shadow={gv_shadow}"
    );

    let mut rows = Vec::new();
    for (li, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let rec: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("  [{li}] skipping unparseable transcript line: {e}");
                continue;
            }
        };
        let id = rec.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        let Some(q) = by_id.get(id) else {
            eprintln!("  [{li}] transcript id `{id}` not in bank — skipping");
            continue;
        };
        let live = crate::bench_cmd::live_runner::LiveAnswer {
            visible: rec.get("answer").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            retrieved_chunk_texts: rec
                .get("retrieved_chunks")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|c| c.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        };
        let row = score_question(
            live,
            judge.as_ref(),
            &judge_model,
            critic.as_ref(),
            &critic_model,
            &corpus,
            "rescored",
            q,
            false,
            grounding_verify,
            gv_shadow,
        )
        .await;
        eprintln!(
            "  [{:>2}] {:<20} expect={:<7} act={:<9} pass={} vp={}",
            rows.len() + 1,
            q.qtype.label(),
            format!("{:?}", q.qtype.expected_action()),
            format!("{:?}", row.agent_action),
            row.is_pass(),
            row.violation_prob.map(|v| format!("{v:.3}")).unwrap_or_else(|| "-".into()),
        );
        rows.push(row);
    }

    if let Err(e) = write_jsonl(&out, &rows) {
        eprintln!("error: could not write {out:?}: {e}");
        return 1;
    }
    let report = score(&rows);
    let verdict = report.verdict(&gates);
    print_summary(&report, &verdict, &gates);
    eprintln!("[out] wrote {} rescored rows → {:?}", rows.len(), out);
    if verdict.overall_pass {
        0
    } else {
        1
    }
}

fn load_gates(path: Option<&Path>) -> Gates {
    let mut g = Gates::default();
    let Some(path) = path else { return g };
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("[manifest] {path:?} not found — using default gates");
        return g;
    };
    let Ok(val) = text.parse::<toml::Value>() else { return g };
    if let Some(t) = val.get("gates").and_then(|v| v.as_table()) {
        let get = |k: &str, d: f64| t.get(k).and_then(|v| v.as_float()).unwrap_or(d);
        g.min_competence = get("min_competence", g.min_competence);
        g.min_honesty = get("min_honesty", g.min_honesty);
        g.max_hallucination = get("max_hallucination", g.max_hallucination);
        // FR-9 RL-3 — only present in governance manifests; chaos banks
        // omit it and keep the strict default (vacuous when no superseded
        // traps, since the dead-law rate is NaN over an empty population).
        g.max_dead_law_rate = get("max_dead_law_rate", g.max_dead_law_rate);
    }
    g
}

fn write_jsonl(path: &Path, rows: &[ResultRow]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::File::create(path)?;
    use std::io::Write as _;
    for r in rows {
        let line = serde_json::to_string(r)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        writeln!(f, "{line}")?;
    }
    Ok(())
}

fn print_summary(
    report: &sovereign_eval::chaos_monkey::CalibrationReport,
    verdict: &sovereign_eval::chaos_monkey::Verdict,
    gates: &Gates,
) {
    let c = &report.counts;
    eprintln!("\n── chaos-monkey: grounded calibration ──");
    eprintln!(
        "  RED-LINE 1  competence-when-present : {:.2}  (≥{:.2}) {}   [correct {}/{}, timid {} ]",
        report.competence,
        gates.min_competence,
        badge(verdict.competence_pass),
        c.answerable_correct,
        c.answerable,
        c.answerable_abstained,
    );
    eprintln!(
        "  RED-LINE 2  honesty-when-absent     : {:.2}  (≥{:.2}) {}   [honest {}/{}, HALLUCINATED {}, timid {} ]",
        report.honesty,
        gates.min_honesty,
        badge(verdict.honesty_pass),
        c.absent_honest,
        c.absent,
        c.absent_hallucinated,
        c.absent
            .saturating_sub(c.absent_honest)
            .saturating_sub(c.absent_hallucinated),
    );
    // RED-LINE 3 (FR-9 governance) — only when the bank carries
    // SupersededTrap rows (else the rate is NaN and RL-3 isn't under test).
    if report.dead_law_rate.is_finite() {
        eprintln!(
            "  RED-LINE 3  no-dead-law (governance) : {:.2}  (≤{:.2}) {}   [grounded in dead law {}/{} superseded-traps ]",
            report.dead_law_rate,
            gates.max_dead_law_rate,
            badge(verdict.dead_law_pass),
            c.dead_law_cited,
            c.superseded_trap,
        );
    }
    eprintln!(
        "  hallucination-rate {:.2} (≤{:.2}) · citation-fidelity {:.2} · distractor-evasion {:.2}",
        report.hallucination_rate, gates.max_hallucination, report.citation_fidelity, report.distractor_evasion,
    );
    eprintln!(
        "  blatant-confab-rate {:.2}  [{}/{} probes presented a value absent from evidence · {} value-bearing answers · gold-free]",
        report.blatant_confab_rate,
        c.blatant_confab,
        c.answerable + c.absent,
        c.value_assessed,
    );
    eprintln!(
        "\n  VERDICT: {}  (both gates must pass; no blended score)",
        if verdict.overall_pass { "PASS ✓" } else { "FAIL ✗" }
    );
}

fn badge(b: bool) -> &'static str {
    if b {
        "PASS"
    } else {
        "FAIL"
    }
}
