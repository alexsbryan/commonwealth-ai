// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench chaos-monkey …` — grounded calibration under
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
    judge_correctness, run_live_pinned, run_naked, verify_grounding,
};
use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench chaos-monkey",
    summary: "Grounded-calibration audit: answer + cite when the fact is in persistence, abstain honestly when it isn't, resist distractors.",
    sections: &[
        HelpSection::Usage(
            "svrn bench chaos-monkey run --bank <bank.toml> [--transport direct|desktop-bridge] [--bridge-url <url>] [--corpus <id>] [--judge-model <stem>] [--critic-model <stem>] [--manifest <toml>] [--out <jsonl>] [--transcripts <jsonl>] [--limit N] [--warm-atlas] [--naked] [--grounding-verify] [--gv-shadow] [--attached <doc.txt> | --attached-asset <id>] [--enrich-model <stem>] [--no-gliner]",
        ),
        HelpSection::Subcommands(&[
            (
                "run",
                "Run each bank question through the live chat path (sealed to the corpus), score the two red-lines, write ResultRow JSONL. --warm-atlas embeds the sealed corpus's enrichment atlas (Entity grounding + Claim virtual chunks under SOVEREIGN_ATLAS_INCLUDE_CLAIMS=1) so retrieval is actually atlas-grounded — without it the in-process manager is cache-only and an un-warmed corpus contributes 0 atlas contexts, silently measuring base retrieval. --naked = true-baseline control: bypass the Runtime (no system prompt, no retrieval, no router/synthesis) and score the bare model; the delta vs a normal run is our prompting+retrieval value-add (citation/distractor N/A under --naked).",
            ),
            (
                "rescore",
                "Replay frozen transcripts (--transcripts from a prior run) through the judges + Critic WITHOUT regenerating answers — no Runtime, no retrieval, no synthesis. Same scorer, same gates. Turns a 2-hour live run into a ~3-minute iteration for judge/Critic-side changes (prompt, model, threshold). Generation-side changes still need `run`.",
            ),
            (
                "score-answer",
                "Score ONE free-form (question, answer, chunks) triple — read as a JSON object on stdin or via --input <file> — with the SAME gold-free grounding primitive the gate and scorer share (assess_asserted_value) plus the abstention + caveat classifiers. No bank, no gold label. Emits a JSON verdict {verdict, asserted_value_grounded, answered, caveat_present, value} on stdout. The single-pair seam external drivers (e.g. the desktop chaos agent) call so their answer oracle is the bench's, not a hand-rolled judge.",
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
        "score-answer" => score_answer(&args[1..]).await,
        "fidelity" => fidelity(&args[1..]).await,
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
    /// Gate threshold τ override for this run. Unset = the SHARED production
    /// default (`sovereign_core::runtime::grounding_gate_threshold()`:
    /// SOVEREIGN_GV_THRESHOLD env, else 0.9). Until 2026-07-30 this lane
    /// silently re-derived its own 0.5 default — pass `--gv-threshold 0.5`
    /// to reproduce pre-unification gated runs.
    gv_threshold: Option<f64>,
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
    /// Eagerly embed + load the sealed corpus's enrichment atlas into the
    /// in-process manager before the run (corpus-scoped `warm_one`).
    /// `build_session` is cache-only, so a freshly-enriched corpus with no
    /// embed cache contributes 0 atlas contexts and the bench silently
    /// measures BASE chunk retrieval. The grounding filter is env-driven
    /// (SOVEREIGN_ATLAS_MIN_DESCRIPTION_CHARS / _INCLUDE_CLAIMS). Direct
    /// transport only (naked bypasses the Runtime; bridge has no session).
    warm_atlas: bool,
    /// Attached-doc lane only: split the ENRICHMENT provider (skeleton /
    /// RAPTOR / action atoms) to a named model, leaving the answer path on
    /// the session's chat model. Mirrors `bench book-report --enrich-model`.
    /// The prime quality lever now that GLiNER freed the token budget:
    /// route the atlas-building calls back onto the 35B primary.
    enrich_model: Option<String>,
    /// Attached-doc lane only: force the LLM entity path (skip GLiNER) for an
    /// A/B against the shipped NER fast-path. Default: GLiNER when installed —
    /// so the holdout measures the stack the product actually ships.
    no_gliner: bool,
}

fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut bank: Option<PathBuf> = None;
    let mut corpus = None;
    let mut judge_model = "fast".to_string();
    // Critic role's model comes from its RoleProfile (preferred_tier → primary),
    // making `role.rs` load-bearing here. Override with `--critic-model`.
    let mut critic_model =
        sovereign_core::role::default_profile_for(sovereign_core::role::Role::Critic)
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
    let mut gv_threshold: Option<f64> = None;
    let mut bridge = false;
    let mut bridge_url = super::desktop_bridge::DEFAULT_BRIDGE_URL.to_string();
    let mut attached: Option<PathBuf> = None;
    let mut attached_asset: Option<String> = None;
    let mut custom_instructions: Option<String> = None;
    let mut pin_intent: Option<Intent> = None;
    let mut warm_atlas = false;
    let mut enrich_model: Option<String> = None;
    let mut no_gliner = false;

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
            "--bank" => bank = Some(PathBuf::from(val!("--bank"))),
            "--corpus" => corpus = Some(val!("--corpus")),
            "--judge-model" => judge_model = val!("--judge-model"),
            "--critic-model" => critic_model = val!("--critic-model"),
            "--base-url" => base_url = val!("--base-url"),
            "--manifest" => manifest = Some(PathBuf::from(val!("--manifest"))),
            "--out" => out = PathBuf::from(val!("--out")),
            "--transcripts" => transcripts = Some(PathBuf::from(val!("--transcripts"))),
            "--limit" => {
                limit = Some(
                    val!("--limit")
                        .parse()
                        .map_err(|_| "--limit must be a usize")?,
                )
            }
            "--naked" => naked = true,
            "--warm-atlas" => warm_atlas = true,
            "--grounding-verify" => grounding_verify = true,
            "--gv-shadow" => gv_shadow = true,
            "--gv-threshold" => {
                gv_threshold = Some(
                    val!("--gv-threshold")
                        .parse()
                        .map_err(|_| "--gv-threshold must be a float in [0,1]")?,
                )
            }
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
            "--enrich-model" => enrich_model = Some(val!("--enrich-model")),
            "--no-gliner" => no_gliner = true,
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
        let stem = out
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("results");
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
    if (enrich_model.is_some() || no_gliner) && attached.is_none() {
        return Err(
            "--enrich-model / --no-gliner only apply to the --attached ingest lane (they tune \
             skeleton/RAPTOR enrichment; there is no ingest on the corpus or --attached-asset paths)"
                .into(),
        );
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
        gv_threshold,
        bridge,
        bridge_url,
        attached,
        attached_asset,
        custom_instructions,
        pin_intent,
        warm_atlas,
        enrich_model,
        no_gliner,
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
    let corpus = match args
        .corpus
        .clone()
        .filter(|c| !c.is_empty())
        .or_else(|| Some(bank.meta.corpus.clone()).filter(|c| !c.is_empty()))
    {
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

    // Atlas grounding (opt-in via --warm-atlas): the bench seals to ONE
    // corpus, so warm THAT corpus's enrichment atlas into the in-process
    // manager (the same Arc the Runtime queries) before any turn. Without
    // it the manager is cache-only (build_session) and a freshly-enriched
    // corpus with no embed cache contributes 0 atlas contexts — the run
    // would silently measure base chunk retrieval, masking any atlas
    // difference. The filter (min-description-chars, include-claims) is
    // env-configured; we echo it so the measurement stays glassbox.
    if args.warm_atlas {
        if let Some(s) = session.as_ref() {
            let f = sovereign_tools::atlas_context_manager::AtlasContextFilter::default();
            let n = s.atlas_mgr.warm_one(&corpus).await;
            eprintln!(
                "[chaos] atlas-warm: {n} context entr{} loaded for `{corpus}` (min_description_chars={}, include_claims={})",
                if n == 1 { "y" } else { "ies" },
                f.min_description_chars,
                f.include_claims,
            );
            if n == 0 {
                eprintln!(
                    "[chaos] WARN: atlas warm loaded 0 entries — this run measures BASE retrieval, NOT the atlas. \
                     Relax the filter (SOVEREIGN_ATLAS_MIN_DESCRIPTION_CHARS=0 SOVEREIGN_ATLAS_INCLUDE_CLAIMS=1) and confirm an atlas exists for `{corpus}`."
                );
            }
        } else {
            eprintln!(
                "[chaos] --warm-atlas ignored: no in-process session (naked or desktop-bridge transport)"
            );
        }
    }

    // Attached-document lane: resolve (or ingest) the asset once; every
    // question dispatches through a minted DocumentSession against it.
    // Judging evidence = the asset's full chunk set (truth-vs-document).
    let attached_setup: Option<(
        sovereign_core::types::DocumentAsset,
        Vec<sovereign_core::types::DocumentChunk>,
    )> = if args.attached.is_some() || args.attached_asset.is_some() {
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
            // Enrichment provider: session default unless --enrich-model splits
            // the skeleton/RAPTOR/atom calls onto a named model. The prime
            // quality lever — GLiNER freed the token budget, so the atlas-
            // building calls can go back onto the 35B primary while the answer
            // path stays on the session's chat model.
            let enrich_inference: std::sync::Arc<dyn InferenceProvider> = match &args.enrich_model {
                Some(model) => {
                    eprintln!("[chaos] enrich model override: {model}");
                    super::book_report::provider_for_model(
                        &globals.daemon_base,
                        model,
                        &session.embed_model,
                    )
                    .await
                }
                None => std::sync::Arc::clone(&session.inference),
            };
            // T2 entity pass: prefer the local NER model over the LLM when
            // installed, so the holdout measures the SHIPPED stack (GLiNER).
            // Eager load (not lazy) so we measure the NER path, not race it —
            // a not-yet-warm lazy loader returns empty and silently falls back
            // to the LLM. `--no-gliner` forces the LLM path for A/B.
            let entity_extractor: Option<
                std::sync::Arc<dyn sovereign_core::traits::EntityExtractor>,
            > = if args.no_gliner {
                eprintln!("[chaos] T2 entity pass: LLM (--no-gliner)");
                None
            } else {
                let model_id = sovereign_gliner::gliner_ner::DEFAULT_MODEL_ID;
                if sovereign_gliner::gliner_ner::probe_model_available(model_id) {
                    match sovereign_gliner::gliner_ner::GlinerExtractor::new_default() {
                        Ok(g) => {
                            eprintln!("[chaos] T2 entity pass: GLiNER ({model_id})");
                            Some(std::sync::Arc::new(g)
                                as std::sync::Arc<
                                    dyn sovereign_core::traits::EntityExtractor,
                                >)
                        }
                        Err(e) => {
                            eprintln!("[chaos] T2 entity pass: LLM (GLiNER load failed: {e})");
                            None
                        }
                    }
                } else {
                    eprintln!(
                        "[chaos] T2 entity pass: LLM (GLiNER model {model_id} not installed)"
                    );
                    None
                }
            };
            let mut manager = sovereign_tools::document_asset::DocumentAssetManager::new(
                enrich_inference,
                std::sync::Arc::clone(&session.store),
            );
            if let Some(g) = entity_extractor {
                manager = manager.with_entity_extractor(g);
            }
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
        std::sync::Arc::new(RemoteApiProvider::new(
            &v1,
            None,
            &args.critic_model,
            PROVIDER_CTX,
        ))
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
        let chat_stem = globals
            .chat_model
            .clone()
            .unwrap_or_else(|| "primary".to_string());
        eprintln!("[chaos] NAKED BASELINE — bypassing the Runtime (no system prompt, no retrieval, no router/synthesis); bare model={chat_stem}, temp=0. citation/distractor are N/A (no sources).");
        Some(std::sync::Arc::new(RemoteApiProvider::new(
            &v1,
            None,
            &chat_stem,
            PROVIDER_CTX,
        )))
    } else {
        None
    };
    let naked_max: usize = globals.max_tokens.unwrap_or(2048);

    // Resolve the alias this run is tagged with to the CONCRETE model
    // behind it — once, up front, while the daemon is live (it is NOT
    // reachable at gate/re-score time, which reads model_id back out of
    // the transcript). Every transcript row is then stamped with the
    // concrete GGUF stem instead of the bare alias, so the captured
    // baseline attributes to the model actually tested rather than to
    // whatever `primary` happens to point at months later. Best-effort:
    // on failure we fall back to the alias and the baseline is recorded
    // unattributed (honest) rather than mislabelled.
    let requested_alias = globals
        .chat_model
        .clone()
        .unwrap_or_else(|| "primary".to_string());
    let run_model_id =
        match super::model_resolve::resolve_model_attribution(&args.base_url, &requested_alias)
            .await
        {
            Some(attr) => {
                eprintln!(
                    "[chaos] resolved slot '{}' → concrete model '{}'{}",
                    requested_alias,
                    attr.file_stem,
                    attr.quant
                        .as_deref()
                        .map(|q| format!(" [{q}]"))
                        .unwrap_or_default()
                );
                attr.file_stem
            }
            None => requested_alias.clone(),
        };

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
                eprintln!(
                    "[chaos] WARN: cannot write transcripts {:?}: {e}",
                    args.transcripts
                );
                None
            }
        }
    };

    let take = args.limit.unwrap_or(bank.questions.len());
    let mut rows = Vec::new();
    for (qi, q) in bank.questions.iter().take(take).enumerate() {
        // Concrete model stem resolved once above, stamped on every row
        // so the transcript (and every baseline re-scored from it)
        // carries the model actually tested, not the slot alias.
        let model_id = run_model_id.clone();
        // Answer source per transport; everything downstream (judges,
        // critic gate, deterministic checks, scorer) is shared verbatim.
        let live = if let (Some((asset, doc_chunks)), Some(session)) = (&attached_setup, &session) {
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
                                gate_action: None,
                                draft: None,
                                metadata: serde_json::Value::Null,
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
        // Clone the gate signals before `live` is consumed, so the transcript
        // (replayed by `rescore`) carries them for the partition.
        let gate_action_full = live.gate_action.clone();
        let draft_full = live.draft.clone();
        // I2-C: carry the typed ledger into the transcript so `rescore` can
        // replay the typed answer-vs-abstain / caveat derivation and run the
        // doc §8 parity comparison (flag off vs on).
        let epistemic_state_full = live.metadata.get("epistemic_state").cloned();
        let row = score_question(
            live,
            judge.as_ref(),
            &args.judge_model,
            critic.as_ref(),
            &args.critic_model,
            &corpus,
            &model_id,
            q,
            naked_provider.is_some(),
            args.grounding_verify,
            args.gv_shadow,
            args.gv_threshold,
        )
        .await;
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
                "gate_action": gate_action_full,
                "draft": draft_full,
                "epistemic_state": epistemic_state_full,
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
    gv_threshold: Option<f64>,
) -> ResultRow {
    let crate::bench_cmd::live_runner::LiveAnswer {
        visible,
        retrieved_chunk_texts: chunk_texts,
        gate_action,
        draft,
        metadata,
    } = live;
    // Third lane (tracked): the turn's top acquisition conjecture from
    // the persisted epistemic ledger, scored against the bank label in
    // aggregation. Advisory only — never part of the two red lines.
    let acquisition_conjecture =
        sovereign_eval::chaos_monkey::score::conjecture_class_from_metadata(&metadata);
    // I2-C: the turn's TYPED epistemic verdict, when the transcript carries
    // a ledger. Now the PRIMARY answer-vs-abstain derivation (default on,
    // `SOVEREIGN_CHAOS_TYPED_VERDICT=0` forces legacy) after the 2026-07-19
    // parity gate proved 43/43 agreement with the gate-action prefix (doc
    // §8) — structural, same underlying gate action. Ledger-less transcripts
    // fall back to legacy. (Caveat is NOT derived from this — see below.)
    let ledger_verdict = sovereign_eval::chaos_monkey::score::verdict_from_metadata(&metadata);
    let typed_verdict =
        crate::bench_cmd::live_runner::typed_verdict_enabled() && ledger_verdict.is_some();

    // External grounding-verifier (--grounding-verify gates, --gv-shadow only
    // records). The Critic returns a continuous violation probability which is
    // persisted on the row either way; the gate compares it against
    // --gv-threshold, else the SHARED production default
    // (`grounding_gate_threshold()`: SOVEREIGN_GV_THRESHOLD env, else 0.9 —
    // this lane carried a divergent silent 0.5 until 2026-07-30, reproducible
    // via --gv-threshold 0.5). If the answer asserts a specific
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
    let gv_threshold: f64 =
        gv_threshold.unwrap_or_else(sovereign_core::runtime::grounding_gate_threshold);
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
    } else if typed_verdict {
        // I2-C typed path (parity-gated): read the answer-vs-abstain
        // decision off the turn's own typed verdict rather than the
        // gate-action string prefix. `cannot_know_from_here` is the
        // abstention verdict (derived from the same gate action, but as a
        // typed contract); an empty reply is still the degenerate decline.
        if ledger_verdict.as_deref() == Some("cannot_know_from_here") || visible.trim().is_empty() {
            AgentAction::Abstained
        } else {
            AgentAction::Answered
        }
    } else if let Some(action) = gate_action.as_deref() {
        // Trust the production gate's OWN decision — it ran in-process this turn
        // and persisted its action — instead of re-judging the visible text. That
        // re-judge was the measurement's main noise source: it mis-scored grounded
        // short answers ("Chief Inspector") as abstentions. `abstain*` → Abstained
        // (authoritative); any release-family action delivered the draft to the
        // reader → Answered (empty text is the degenerate decline). The honesty
        // truth for a released self-decline lives in the value-presence axis
        // (blatant_confab / the partition), not this action proxy.
        if action.starts_with("abstain") || visible.trim().is_empty() {
            AgentAction::Abstained
        } else {
            AgentAction::Answered
        }
    } else {
        // No gate signal (naked baseline or gate disabled): fall back to the
        // forced-choice judge.
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
        // Forms-first: gold_match handles |-OR-groups deterministically. Only when
        // the forms MISS do we escalate to the LLM correctness judge (the answer
        // may be correct via a paraphrase the forms don't cover) — logging every
        // escalation so the judge's footprint on this signal stays auditable.
        if gold_match(&visible, &q.gold_keywords) {
            Some(true)
        } else {
            let j = judge_correctness(judge, judge_model, &q.question, &q.gold_keywords, &visible)
                .await;
            eprintln!(
                "  [correctness-escalate] {}: gold-forms missed → judge={}",
                q.id,
                j.map(|b| if b { "correct" } else { "wrong" })
                    .unwrap_or("unavailable")
            );
            Some(j.unwrap_or(false))
        }
    } else {
        None
    };
    // Partition signals (docs/CHAOS_MEASUREMENT_REDESIGN.md). retrieval_present:
    // is the gold answer in the retrieved chunks at all? — a `false` on an
    // abstained answerable probe is a RETRIEVAL miss, not a gate/model fault.
    // draft_correct: was the PRE-GATE draft correct? (present only when the gate
    // recorded the draft under SOVEREIGN_AGENTIC_KQ_DEBUG) — splits
    // gate-killed-correct from caught-confabulation.
    let retrieval_present = if q.qtype.is_answerable() {
        Some(gold_match(&chunk_texts.join(" \n "), &q.gold_keywords))
    } else {
        None
    };
    let draft_correct = match (q.qtype.is_answerable(), draft.as_deref()) {
        (true, Some(d)) => Some(gold_match(d, &q.gold_keywords)),
        _ => None,
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
        // The typed GK verdict is deliberately NOT used here. The 2026-07-19
        // parity gate found `verdict == general_knowledge` is not a faithful
        // proxy for the prose-level caveat: on `ood-australia-capital` the
        // model released a provenance-flagged DECLINE ("I don't have reliable
        // information in my knowledge base") over 10 retrieved distractors —
        // the judge (reading the prose) said caveat=true, the ledger verdict
        // (classifying the basis) was `unverified`, not GK. The verdict
        // reflects the turn's BASIS; the caveat is about the answer's own
        // words. So caveat stays on the judge unconditionally.
        match classify_caveat(judge, judge_model, &visible).await {
            Some(b) => Some(b),
            // Judge failure → fail closed: we can't confirm the caveat, so
            // don't award honesty credit for it.
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
    let (asserted_value, asserted_value_grounded) = if answered
            && !naked
            && !chunk_texts.is_empty()
            && q.qtype != QuestionType::AbsentOutOfDomain
            // Mirror the gate's OWN scoping: it skips value/claim verification on
            // long-form answers (the verify_grounding >1800-char "out of gate
            // scope" pivot). Reducing an essay to one extracted "value" sweeps in
            // framing (author, other works, dates) and false-positives the
            // blatant-confab metric — the documented essay regression. Same pivot
            // here keeps blatant_confab honest on discursive answers.
            && visible.chars().count() <= 1_800
    {
        use sovereign_core::runtime::{assess_asserted_value, AssertedValue};
        match assess_asserted_value(
            critic,
            &q.question,
            &visible,
            &chunk_texts,
            sovereign_core::oicp::ShardingPrivacy::LocalOnly,
        )
        .await
        {
            AssertedValue::Grounded(v) => (Some(v), Some(true)),
            AssertedValue::Ungrounded(v) => (Some(v), Some(false)),
            AssertedValue::NoValue => (None, None),
        }
    } else {
        (None, None)
    };

    let excerpt: String = visible.chars().take(200).collect();
    let mut row = ResultRow {
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
        gate_action,
        retrieval_present,
        draft_correct,
        partition: None,
        acquisition_label: q.acquisition_class,
        acquisition_conjecture,
    };
    // Stamp the glassbox partition cell from the row's own signals (the histogram
    // recomputes it via `partition_cell()`; this stored copy is for JSONL readers).
    row.partition = Some(row.partition_cell());
    row
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
    let mut critic_model =
        sovereign_core::role::default_profile_for(sovereign_core::role::Role::Critic)
            .preferred_tier
            .model_stem()
            .to_string();
    let mut base_url = "http://localhost:9741".to_string();
    let mut manifest: Option<PathBuf> = None;
    let mut out = PathBuf::from("target/chaos-monkey/rescored.jsonl");
    let mut grounding_verify = false;
    let mut gv_shadow = false;
    let mut gv_threshold: Option<f64> = None;

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
            // The offline threshold-sweep lever: one --gv-shadow live run,
            // then rescore at candidate τ values without touching the env.
            "--gv-threshold" => match val!("--gv-threshold").parse() {
                Ok(v) => gv_threshold = Some(v),
                Err(_) => {
                    eprintln!("error: --gv-threshold must be a float in [0,1]");
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
    let judge: std::sync::Arc<dyn InferenceProvider> = std::sync::Arc::new(RemoteApiProvider::new(
        &v1,
        None,
        &judge_model,
        PROVIDER_CTX,
    ));
    let critic: std::sync::Arc<dyn InferenceProvider> = if critic_model == judge_model {
        std::sync::Arc::clone(&judge)
    } else {
        std::sync::Arc::new(RemoteApiProvider::new(
            &v1,
            None,
            &critic_model,
            PROVIDER_CTX,
        ))
    };

    eprintln!(
        "[chaos] RESCORE transcripts={transcripts_path:?} bank={bank_path:?} judge={judge_model} critic={critic_model} gv={grounding_verify} shadow={gv_shadow} tau={}",
        gv_threshold.map_or_else(
            || format!("{} (shared default)", sovereign_core::runtime::grounding_gate_threshold()),
            |v| v.to_string()
        )
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
            visible: rec
                .get("answer")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            retrieved_chunk_texts: rec
                .get("retrieved_chunks")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|c| c.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            // Recovered from the transcript when present (new runs persist them);
            // older transcripts lack them → None, and the partition degrades to
            // the retrieval-attributed coarse cells for those rows.
            gate_action: rec
                .get("gate_action")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            draft: rec
                .get("draft")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            // I2-C: rebuild the metadata carrying the persisted ledger so the
            // typed verdict derivation + the acquisition-conjecture lane
            // replay on rescore. Older transcripts lack `epistemic_state` →
            // the typed path falls back to the legacy derivation.
            metadata: match rec.get("epistemic_state") {
                Some(es) if !es.is_null() => serde_json::json!({ "epistemic_state": es }),
                _ => serde_json::Value::Null,
            },
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
            gv_threshold,
        )
        .await;
        eprintln!(
            "  [{:>2}] {:<20} expect={:<7} act={:<9} pass={} vp={}",
            rows.len() + 1,
            q.qtype.label(),
            format!("{:?}", q.qtype.expected_action()),
            format!("{:?}", row.agent_action),
            row.is_pass(),
            row.violation_prob
                .map(|v| format!("{v:.3}"))
                .unwrap_or_else(|| "-".into()),
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

/// `score-answer` — score ONE free-form (question, answer, chunks) triple with
/// the SAME gold-free grounding primitive the live grounding gate and the chaos
/// scorer share (`assess_asserted_value`), plus the abstention + provenance-
/// caveat classifiers. No bank, no gold label — the judgment reads only the
/// answer and the evidence. This is the single-pair seam an external driver
/// (the desktop chaos agent) calls per chat turn so its answer-quality oracle
/// is the bench's verdict, not a hand-rolled judge.
///
/// Input is a JSON object `{"question":..,"answer":..,"chunks":[..]}` read from
/// `--input <file>` or stdin. Output is one line of JSON on stdout; all
/// diagnostics go to stderr so stdout stays a clean machine-readable verdict.
async fn score_answer(rest: &[String]) -> i32 {
    let mut input: Option<PathBuf> = None;
    let mut judge_model = "fast".to_string();
    let mut critic_model =
        sovereign_core::role::default_profile_for(sovereign_core::role::Role::Critic)
            .preferred_tier
            .model_stem()
            .to_string();
    let mut base_url = "http://localhost:9741".to_string();

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
            "--judge-model" => judge_model = val!("--judge-model"),
            "--critic-model" => critic_model = val!("--critic-model"),
            "--base-url" => base_url = val!("--base-url"),
            "--help" | "-h" => {
                eprintln!("usage: svrn bench chaos-monkey score-answer [--input <file>] [--judge-model <stem>] [--critic-model <stem>] [--base-url <url>]");
                eprintln!("  reads {{\"question\",\"answer\",\"chunks\":[..]}} JSON from --input or stdin; writes a JSON verdict to stdout");
                return 0;
            }
            other => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }

    // Read the (question, answer, chunks) triple from --input or stdin. Stdin
    // is the default so the node caller can pipe long answers without hitting
    // argv length limits or shell-quoting hazards.
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
    let chunks: Vec<String> = rec
        .get("chunks")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if question.trim().is_empty() || answer.trim().is_empty() {
        eprintln!("error: input must carry non-empty `question` and `answer`");
        return 2;
    }

    let v1 = format!("{}/v1", base_url.trim_end_matches('/'));
    let judge: std::sync::Arc<dyn InferenceProvider> = std::sync::Arc::new(RemoteApiProvider::new(
        &v1,
        None,
        &judge_model,
        PROVIDER_CTX,
    ));
    let critic: std::sync::Arc<dyn InferenceProvider> = if critic_model == judge_model {
        std::sync::Arc::clone(&judge)
    } else {
        std::sync::Arc::new(RemoteApiProvider::new(
            &v1,
            None,
            &critic_model,
            PROVIDER_CTX,
        ))
    };

    // The shared gold-free grounding primitive — the gate DECIDEs with it, the
    // chaos scorer MEASUREs with it: does the answer's asserted value appear in
    // the evidence? Grounded = release; Ungrounded = blatant confabulation (the
    // cardinal sin); NoValue = nothing checkable asserted (a decline/discursive).
    use sovereign_core::runtime::{assess_asserted_value, AssertedValue};
    let (value, grounded): (Option<String>, Option<bool>) = match assess_asserted_value(
        critic.as_ref(),
        &question,
        &answer,
        &chunks,
        sovereign_core::oicp::ShardingPrivacy::LocalOnly,
    )
    .await
    {
        AssertedValue::Grounded(v) => (Some(v), Some(true)),
        AssertedValue::Ungrounded(v) => (Some(v), Some(false)),
        AssertedValue::NoValue => (None, None),
    };

    // The same abstention + caveat classifiers the live scorer uses. `answered`
    // = does a reader come away with an answer? `caveat` = did it flag the answer
    // as general knowledge (out-of-domain honesty)?
    let answered = crate::bench_cmd::live_runner::classify_extraction(
        judge.as_ref(),
        &judge_model,
        &question,
        &answer,
    )
    .await;
    let caveat =
        crate::bench_cmd::live_runner::classify_caveat(judge.as_ref(), &judge_model, &answer).await;

    // Bench-aligned verdict — the chaos scorer's own vocabulary, worst first:
    //   hallucination     answered with a value absent from the evidence (the
    //                     cardinal sin / blatant confabulation).
    //   grounded          answered with a value present in the evidence.
    //   caveated_ood      answered from general knowledge WITH a provenance
    //                     caveat (honest out-of-domain).
    //   honest_abstention declined / asserted no checkable specific (good — this
    //                     is the graceful honest decline, by GROUNDING; whether
    //                     it READS gracefully is a separate UX-tone signal).
    //   answered_novalue  answered but no single checkable value and no caveat
    //                     (a discursive/broad account — neither grounded nor a
    //                     fabrication).
    // caveat is checked ALONGSIDE grounded==false, not after: a specific value
    // that is absent from the evidence BUT the answer flags as "from general
    // knowledge" is HONEST out-of-domain (caveated_ood), never the cardinal sin.
    // score.rs's own doc says so ("caveated 'from general knowledge…' is honest,
    // not a hallucination"); the old ladder returned "hallucination" first,
    // mislabeling caveated OOD answers (persona-QA 2026-07-11: the "bjp 2024 vote
    // %/seats" turn — explicitly caveated GK — tagged hallucination, then routed
    // to answered_ungrounded instead of a graceful gap). Uncaveated absent value
    // is still the sin.
    let verdict = if grounded == Some(false) && caveat != Some(true) {
        "hallucination"
    } else if grounded == Some(true) {
        "grounded"
    } else if caveat == Some(true) {
        "caveated_ood"
    } else if answered == Some(false) {
        "honest_abstention"
    } else {
        "answered_novalue"
    };

    let out = serde_json::json!({
        "verdict": verdict,
        "value": value,
        "asserted_value_grounded": grounded,
        "answered": answered,
        "caveat_present": caveat,
        "n_chunks": chunks.len(),
        "critic_model": critic_model,
        "judge_model": judge_model,
    });
    println!(
        "{}",
        serde_json::to_string(&out).unwrap_or_else(|_| "{}".into())
    );
    0
}

/// Ledger-fidelity pass (EPISTEMIC_STATE §8): are the typed receipts
/// TRUE? Reads a chaos transcripts.jsonl (each row: question, answer,
/// gate_action, epistemic_state) and audits the ledger against the
/// prose it describes. Two layers:
///
/// 1. **Deterministic cross-checks** (no model):
///    - a decline-shaped answer (the gate's own `answer_declines`
///      primitive) carrying a `grounded`/`mixed` verdict — the forged-
///      receipt class caught by luck on `ood-table-salt` (2026-07-20),
///      now audited systematically;
///    - holdings on an abstained (`cannot_know_from_here`) turn — the
///      assembler's I2 contract, re-checked at the persisted artifact.
/// 2. **Judge correspondence** (daemon judge): for every corpus/GK
///    holding, does the ANSWER actually assert the held claim (or a
///    clear paraphrase)? A holding the prose never asserts is a receipt
///    for nothing.
///
/// Tracked-advisory: prints a fidelity report + writes findings JSONL;
/// exit 0 unless the artifact is unreadable. Gate once a baseline
/// exists (the standing bench convention).
async fn fidelity(rest: &[String]) -> i32 {
    let mut transcripts: Option<std::path::PathBuf> = None;
    let mut judge_model = "fast".to_string();
    let mut base_url = "http://127.0.0.1:9741".to_string();
    let mut out: Option<std::path::PathBuf> = None;
    let mut i = 0;
    while i < rest.len() {
        macro_rules! val {
            ($flag:expr) => {{
                i += 1;
                match rest.get(i) {
                    Some(v) => v.clone(),
                    None => {
                        eprintln!("error: {} requires a value", $flag);
                        return 2;
                    }
                }
            }};
        }
        match rest[i].as_str() {
            "--transcripts" => transcripts = Some(std::path::PathBuf::from(val!("--transcripts"))),
            "--judge-model" => judge_model = val!("--judge-model"),
            "--base-url" => base_url = val!("--base-url"),
            "--out" => out = Some(std::path::PathBuf::from(val!("--out"))),
            other => {
                eprintln!("error: unknown fidelity flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }
    let Some(transcripts_path) = transcripts else {
        eprintln!("error: --transcripts is required");
        return 2;
    };
    let text = match std::fs::read_to_string(&transcripts_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: could not read {transcripts_path:?}: {e}");
            return 1;
        }
    };
    let v1 = format!("{}/v1", base_url.trim_end_matches('/'));
    let judge: std::sync::Arc<dyn InferenceProvider> = std::sync::Arc::new(RemoteApiProvider::new(
        &v1,
        None,
        &judge_model,
        PROVIDER_CTX,
    ));

    let (mut n_rows, mut n_ledger) = (0usize, 0usize);
    let mut verdict_decline_conflicts: Vec<String> = Vec::new();
    let mut abstained_with_holdings: Vec<String> = Vec::new();
    let (mut holdings_checked, mut holdings_asserted) = (0usize, 0usize);
    let mut findings: Vec<serde_json::Value> = Vec::new();

    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        n_rows += 1;
        let id = rec.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let answer = rec.get("answer").and_then(|v| v.as_str()).unwrap_or("");
        let Some(es) = rec.get("epistemic_state").filter(|v| !v.is_null()) else {
            continue;
        };
        n_ledger += 1;
        let verdict = es.get("verdict").and_then(|v| v.as_str()).unwrap_or("");
        let holdings: Vec<&serde_json::Value> = es
            .get("holdings")
            .and_then(|h| h.as_array())
            .map(|a| a.iter().collect())
            .unwrap_or_default();

        // 1a. Forged-receipt class: confident verdict over PURE decline
        // prose. The strict primitive + a length cap keep rich answers
        // that merely CONTAIN a negative clause ("the sources do not
        // name X directly, but …") out of the finding — those assert
        // content and their receipts are judged in layer 2.
        if matches!(verdict, "grounded" | "mixed")
            && answer.chars().count() < 300
            && sovereign_core::runtime::released_pure_decline(answer)
        {
            verdict_decline_conflicts.push(id.to_string());
            findings.push(serde_json::json!({
                "id": id, "kind": "verdict_on_decline", "verdict": verdict,
                "excerpt": answer.chars().take(160).collect::<String>(),
            }));
        }
        // 1b. Structural: an abstained turn asserts nothing.
        if verdict == "cannot_know_from_here" && !holdings.is_empty() {
            abstained_with_holdings.push(id.to_string());
            findings.push(serde_json::json!({
                "id": id, "kind": "abstained_with_holdings", "holdings": holdings.len(),
            }));
        }
        // 2. Judge: does the prose assert each held claim?
        for h in &holdings {
            let claim = h.get("claim").and_then(|c| c.as_str()).unwrap_or("");
            if claim.is_empty() || answer.is_empty() {
                continue;
            }
            holdings_checked += 1;
            let prompt = format!(
                "ANSWER:\n{}\n\nCLAIM: {}\n\nDoes the ANSWER assert this claim \
                 (verbatim or as a clear paraphrase)? Reply with JSON only.",
                answer.chars().take(2000).collect::<String>(),
                claim.chars().take(400).collect::<String>(),
            );
            let mut req = sovereign_core::types::CompletionRequest::default();
            req.prompt = prompt;
            req.system_message =
                Some("You audit answer/claim correspondence precisely. JSON only.".into());
            req.max_tokens = Some(32);
            req.temperature = Some(0.0);
            req.structured_output = Some(serde_json::json!({
                "type": "object",
                "properties": { "asserted": { "type": "boolean" } },
                "required": ["asserted"]
            }));
            match judge.complete(&req).await {
                Ok(resp) => {
                    let asserted = serde_json::from_str::<serde_json::Value>(
                        resp.text
                            .trim()
                            .trim_start_matches("```json")
                            .trim_start_matches("```")
                            .trim_end_matches("```"),
                    )
                    .ok()
                    .and_then(|v| v.get("asserted").and_then(|b| b.as_bool()));
                    match asserted {
                        Some(true) => holdings_asserted += 1,
                        Some(false) => {
                            findings.push(serde_json::json!({
                                "id": id, "kind": "holding_not_asserted",
                                "claim": claim.chars().take(200).collect::<String>(),
                            }));
                        }
                        // Unparseable judge output: fail CLOSED for the
                        // metric (don't award correspondence we can't
                        // confirm) but record the ambiguity.
                        None => {
                            findings.push(serde_json::json!({
                                "id": id, "kind": "judge_unparseable",
                                "claim": claim.chars().take(200).collect::<String>(),
                            }));
                        }
                    }
                }
                Err(e) => {
                    findings.push(serde_json::json!({
                        "id": id, "kind": "judge_error", "error": e.to_string(),
                    }));
                }
            }
        }
    }

    let fidelity_rate = if holdings_checked > 0 {
        holdings_asserted as f64 / holdings_checked as f64
    } else {
        f64::NAN
    };
    eprintln!("\n── ledger fidelity (holdings ↔ prose) ──");
    eprintln!("  rows {n_rows} · with ledger {n_ledger}");
    eprintln!(
        "  DETERMINISTIC  verdict-on-decline conflicts : {}  {:?}",
        verdict_decline_conflicts.len(),
        verdict_decline_conflicts,
    );
    eprintln!(
        "  DETERMINISTIC  abstained-with-holdings      : {}  {:?}",
        abstained_with_holdings.len(),
        abstained_with_holdings,
    );
    eprintln!(
        "  TRACKED        holding↔prose correspondence : {fidelity_rate:.2}  [{holdings_asserted}/{holdings_checked} holdings asserted by their prose ]",
    );
    if let Some(out_path) = out {
        let body = findings
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        if let Err(e) = std::fs::write(&out_path, body) {
            eprintln!("  warn: could not write findings to {out_path:?}: {e}");
        } else {
            eprintln!("  findings → {out_path:?} ({})", findings.len());
        }
    }
    0
}

fn load_gates(path: Option<&Path>) -> Gates {
    let mut g = Gates::default();
    let Some(path) = path else { return g };
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("[manifest] {path:?} not found — using default gates");
        return g;
    };
    let Ok(val) = text.parse::<toml::Value>() else {
        return g;
    };
    if let Some(t) = val.get("gates").and_then(|v| v.as_table()) {
        let get = |k: &str, d: f64| t.get(k).and_then(|v| v.as_float()).unwrap_or(d);
        g.min_competence = get("min_competence", g.min_competence);
        g.min_honesty = get("min_honesty", g.min_honesty);
        g.max_hallucination = get("max_hallucination", g.max_hallucination);
        // FR-9 RL-3 — only present in governance manifests; chaos banks
        // omit it and keep the strict default (vacuous when no superseded
        // traps, since the dead-law rate is NaN over an empty population).
        g.max_dead_law_rate = get("max_dead_law_rate", g.max_dead_law_rate);
        // Third lane (EPISTEMIC_STATE §8): absent from the manifest =
        // 0.0 = disarmed (tracked-advisory). Armed once a measured
        // baseline is pre-registered.
        g.min_acquisition_conjecture =
            get("min_acquisition_conjecture", g.min_acquisition_conjecture);
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
        "  hallucination-rate {:.2} (≤{:.2}) · grounding-fidelity {:.2} ({}/{} grounded) · citation-fidelity {:.2} (n={}) · distractor-evasion {:.2}",
        report.hallucination_rate,
        gates.max_hallucination,
        report.grounding_fidelity,
        c.value_assessed.saturating_sub(c.blatant_confab),
        c.value_assessed,
        report.citation_fidelity,
        report.n_citation_checked,
        report.distractor_evasion,
    );
    eprintln!(
        "  blatant-confab-rate {:.2}  [{}/{} probes presented a value absent from evidence · {} value-bearing answers · gold-free]",
        report.blatant_confab_rate,
        c.blatant_confab,
        c.answerable + c.absent,
        c.value_assessed,
    );
    // Third lane (EPISTEMIC_STATE.md §8): on labeled absent probes, did
    // the epistemic ledger's top acquisition conjecture name the class
    // that would actually satisfy the gap? TRACKED (advisory) until a
    // manifest sets `min_acquisition_conjecture`; gated after. Silent
    // when the bank carries no labels.
    if report.n_acquisition_labeled > 0 {
        let rate = report.acquisition_matched as f64 / report.n_acquisition_labeled as f64;
        if gates.min_acquisition_conjecture > 0.0 {
            eprintln!(
                "  RED-LINE 4  acquisition-conjecture  : {rate:.2}  (≥{:.2}) {}   [matched {}/{} labeled absent probes ]",
                gates.min_acquisition_conjecture,
                badge(verdict.acquisition_pass),
                report.acquisition_matched,
                report.n_acquisition_labeled,
            );
        } else {
            eprintln!(
                "  TRACKED     acquisition-conjecture  : {rate:.2}          [matched {}/{} labeled absent probes ]",
                report.acquisition_matched,
                report.n_acquisition_labeled,
            );
        }
        // Sub-lane attribution: the blended gate rate mixes routing
        // SKILL (satisfiable labels) with unknowable-contract outcomes,
        // most of which match vacuously on answered turns. Print the
        // decomposition so a moving blended rate is attributable.
        if report.n_acq_satisfiable > 0 {
            eprintln!(
                "    ├ satisfiable routing       : {:.2}          [{}/{} — the resolver's actual skill ]",
                report.acq_satisfiable_matched as f64 / report.n_acq_satisfiable as f64,
                report.acq_satisfiable_matched,
                report.n_acq_satisfiable,
            );
        }
        {
            let unk_total = report.n_acquisition_labeled - report.n_acq_satisfiable;
            if unk_total > 0 {
                eprintln!(
                    "    └ unknowable contract       : exercised {}/{} silent · vacuous-match {}   [resolver never stays silent today — standing miss until unknowable-detection exists ]",
                    report.acq_unknowable_exercised_matched,
                    report.n_acq_unknowable_exercised,
                    report.acq_unknowable_vacuous_matches,
                );
            }
        }
    }
    // OOD helpfulness lane (rubric edit 2026-07-20): abstaining on an
    // out-of-domain probe is honest but timid — the hybrid ideal is a
    // caveated parametric answer. Tracked here so the timidity signal
    // stays visible without failing the honesty red line.
    if report.n_ood > 0 {
        eprintln!(
            "  TRACKED     ood-caveated-answer     : {:.2}          [caveated answer {}/{} out-of-domain probes ]",
            report.ood_caveated_answers as f64 / report.n_ood as f64,
            report.ood_caveated_answers,
            report.n_ood,
        );
    }
    // Causal attribution — the partition histogram. The diagnostic that says
    // WHERE the misses are (gate vs model vs retrieval), so a gate fix shows up
    // even when the aggregate is noisy. See docs/CHAOS_MEASUREMENT_REDESIGN.md.
    let p = &report.partition;
    eprintln!(
        "\n  ── partition (causal attribution) ──\n  \
         answerable: correct {} · gate-killed-correct {} · synth-wrong-caught {} · leaked-wrong {} · retrieval-miss {}\n  \
         absent: abstain-correct {} · released-best-effort {} · CONFAB-LEAKED {}\n  \
         unclassified {}  (naked / gate-off / draft not recorded)",
        p.correct, p.gate_killed_correct, p.synth_wrong_caught, p.leaked_wrong, p.retrieval_miss,
        p.abstain_correct, p.released_best_effort, p.confab_leaked, p.unclassified,
    );
    eprintln!(
        "  misses attributed → gate {} · model {} · retrieval {}",
        p.attributed_to_gate(),
        p.attributed_to_model(),
        p.attributed_to_retrieval(),
    );
    eprintln!(
        "\n  VERDICT: {}  (both gates must pass; no blended score)",
        if verdict.overall_pass {
            "PASS ✓"
        } else {
            "FAIL ✗"
        }
    );
}

fn badge(b: bool) -> &'static str {
    if b {
        "PASS"
    } else {
        "FAIL"
    }
}
