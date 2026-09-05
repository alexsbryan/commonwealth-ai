// SPDX-License-Identifier: AGPL-3.0-or-later
//! Lane `chat-ask` — one real turn, watched all the way through.
//!
//! # What this lane sees that no other bench does
//!
//! Issue #57: an answerable two-part question came back with the first half
//! answered and the second half refused, on a corpus whose one relevant
//! passage ranked #1 in retrieval and sat at index 1 of the twenty chunks
//! the turn was handed. Every existing lane was green through it, and none
//! of them could have been otherwise — the retrieval lanes score rank, the
//! synth lanes judge prose, and neither reads what the turn DID.
//!
//! This lane reads the ledger. `chat ask --format json` now carries
//! `routed_intent`, the grounding gate's own outcome and the per-turn stage
//! attribution, so the assertions below are about the MECHANISM, not about
//! how the answer reads:
//!
//! | row | kind | what a failure means |
//! |---|---|---|
//! | ingest | HARD | the fixture chunked differently, or is not searchable |
//! | ledger present | HARD | the turn opened no ledger — nothing below can be judged |
//! | route | HARD | the question took a different route, or cited another corpus |
//! | per-stage ceilings | HARD | a stage blew its pre-registered budget |
//! | per-stage baseline | TRACKED | a stage moved against this stack's last run |
//! | gate outcome | HARD | the gate's action left the bank's allowed set |
//! | both halves answered | HARD | the #57 failure, back |
//! | not abstained | HARD | an answerable question got a refusal |
//! | useful | HARD | a reader would not come away with an answer |
//! | judge calibrated | precondition | the usefulness probe stopped separating |
//!
//! # The instrument is validated before the result (ARCH §18.4)
//!
//! `judge calibrated` runs the bank's two controls — a known-good answer
//! and a flat abstention — on EVERY run, through the same forced-choice
//! probe the scored answers go through. If they stop separating by the
//! bank's `control_gap`, `useful` is could-not-judge: not a pass, and not a
//! failure blamed on the answer. A usefulness number is exactly the kind
//! that keeps reading plausible after its probe has drifted.
//!
//! # Three runs, one discarded
//!
//! The first turn after an ingest pays for a cold cache and is thrown away.
//! The remaining three give a median for the judge and a spread for the
//! baseline's tolerance. One run is not a measurement (ARCH §18.5).

use std::path::{Path, PathBuf};
use std::time::Instant;

use kernel_types::Judgement;
use sovereign_contracts::types::projection::TurnMetadata;
use sovereign_contracts::types::{StageId, TurnMode};
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::remote::RemoteApiProvider;
use sovereign_turn_client::{TurnClient, TurnObserver};

use super::{reason, LaneCtx, LaneReport};

const LANE: &str = "chat-ask";
const BANK: &str = "sovereign/bench/quality-check/chat-ask.toml";
/// Judge context window, the same 8192 every bench lane uses.
const PROVIDER_CTX: u32 = 8192;

// ─── The bank ───────────────────────────────────────────────────────

struct ChatAskBank {
    fixture: PathBuf,
    corpus_prefix: String,
    declared_chunks: usize,
    search_probe: String,
    search_must_hit: String,
    runs: usize,
    warmup: usize,
    judge: JudgeCfg,
    ceilings: toml::value::Table,
    questions: Vec<ChatAskQuestion>,
}

struct JudgeCfg {
    model: String,
    threshold: f64,
    control_gap: f64,
    control_good_min: f64,
    control_flat_max: f64,
    control_good: String,
    control_flat: String,
}

struct ChatAskQuestion {
    id: String,
    text: String,
    expected_route: String,
    allowed_gate_actions: Vec<String>,
    must_locate: Vec<String>,
}

/// Parse the bank. `ChatAsk`-prefixed because the concept ratchet found
/// `Bank`, `Question` and `TurnResult` already defined elsewhere in this
/// crate (`bench_cmd::routing_replay`, `eval_cmd::bank`, `book_report`,
/// `eval_cmd::runner_threads`) and in `corpus-engine-vocab`. None of them is
/// this concept — an eval bank's question carries a category and a gold
/// answer, this one carries a route, a gate-action set and coverage spans —
/// so these are named apart rather than converged, which is the other half
/// of the pre-flight rule.
///
/// Every field is REQUIRED: a bank that half-parses would
/// silently drop an assertion, and a dropped assertion reads exactly like a
/// passing one.
fn parse_bank(text: &str) -> Result<ChatAskBank, String> {
    let doc: toml::Value = toml::from_str(text).map_err(|e| format!("{BANK}: {e}"))?;
    let want = |t: &toml::Value, k: &str| -> Result<toml::Value, String> {
        t.get(k)
            .cloned()
            .ok_or_else(|| format!("{BANK}: missing `{k}`"))
    };
    let s_of = |t: &toml::Value, k: &str| -> Result<String, String> {
        want(t, k)?
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("{BANK}: `{k}` must be a string"))
    };
    let n_of = |t: &toml::Value, k: &str| -> Result<i64, String> {
        want(t, k)?
            .as_integer()
            .ok_or_else(|| format!("{BANK}: `{k}` must be an integer"))
    };
    let f_of = |t: &toml::Value, k: &str| -> Result<f64, String> {
        want(t, k)?
            .as_float()
            .ok_or_else(|| format!("{BANK}: `{k}` must be a float"))
    };
    let strs = |t: &toml::Value, k: &str| -> Result<Vec<String>, String> {
        want(t, k)?
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .filter(|v: &Vec<String>| !v.is_empty())
            .ok_or_else(|| format!("{BANK}: `{k}` must be a non-empty array of strings"))
    };

    let bank = want(&doc, "bank")?;
    let judge = want(&doc, "judge")?;
    let ceilings = want(&doc, "ceilings")?
        .as_table()
        .cloned()
        .ok_or_else(|| format!("{BANK}: `[ceilings]` must be a table of model stems"))?;
    let questions_raw = doc
        .get("question")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or_else(|| format!("{BANK}: no [[question]] entries"))?;
    let mut questions = Vec::new();
    for q in questions_raw {
        questions.push(ChatAskQuestion {
            id: s_of(q, "id")?,
            text: s_of(q, "text")?,
            expected_route: s_of(q, "expected_route")?,
            allowed_gate_actions: strs(q, "allowed_gate_actions")?,
            must_locate: strs(q, "must_locate")?,
        });
    }
    Ok(ChatAskBank {
        fixture: PathBuf::from(s_of(&bank, "fixture")?),
        corpus_prefix: s_of(&bank, "corpus_prefix")?,
        declared_chunks: usize::try_from(n_of(&bank, "declared_chunks")?)
            .map_err(|_| format!("{BANK}: declared_chunks must be non-negative"))?,
        search_probe: s_of(&bank, "search_probe")?,
        search_must_hit: s_of(&bank, "search_must_hit")?,
        runs: usize::try_from(n_of(&bank, "runs")?)
            .map_err(|_| format!("{BANK}: runs must be non-negative"))?,
        warmup: usize::try_from(n_of(&bank, "warmup")?)
            .map_err(|_| format!("{BANK}: warmup must be non-negative"))?,
        judge: JudgeCfg {
            model: s_of(&judge, "model")?,
            threshold: f_of(&judge, "threshold")?,
            control_gap: f_of(&judge, "control_gap")?,
            control_good_min: f_of(&judge, "control_good_min")?,
            control_flat_max: f_of(&judge, "control_flat_max")?,
            control_good: s_of(&judge, "control_good")?,
            control_flat: s_of(&judge, "control_flat")?,
        },
        ceilings,
        questions,
    })
}

// ─── The usefulness judge ───────────────────────────────────────────

/// The forced-choice usefulness prompt. ONE spelling — the controls and the
/// scored answers go through this same function, which is the only way the
/// controls calibrate anything (ARCH §18.4).
fn usefulness_prompt(question: &str, answer: &str) -> String {
    format!(
        "A reader asked:\n\"{question}\"\n\nThe reply was:\n\"\"\"\n{answer}\n\"\"\"\n\n\
         Did this reply give the reader a usable answer to what they asked?\n\n\
         A = yes, it answers with specifics the reader can use.\n\
         B = no, it declines, hedges, or answers something else."
    )
}

/// P(useful). `None` when the judge did not answer — reported, never
/// defaulted to a number (ARCH §18.3).
async fn p_useful(
    judge: &dyn InferenceProvider,
    model: &str,
    question: &str,
    answer: &str,
) -> Option<f64> {
    // The gate's own probe, reused (ARCH §19) — not a second forced-choice
    // implementation beside it.
    crate::bench_cmd::live_runner::forced_choice_ab(
        judge,
        model,
        &usefulness_prompt(question, answer),
    )
    .await
    .map(|(a, _b)| a)
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(xs[xs.len() / 2])
}

// ─── One turn ───────────────────────────────────────────────────────

struct LaneTurn {
    visible: String,
    metadata: Option<TurnMetadata>,
    source_origins: Vec<String>,
    wall_ms: u64,
}

async fn ask_once(client: &TurnClient, corpus: &str, question: &str) -> Result<LaneTurn, String> {
    // The DAEMON mints the conversation and validates the corpus allow-list
    // against what it has installed — same door `chat ask` uses, so a lane
    // cannot pass an allow-list the product would refuse.
    let convo = client
        .create_conversation(None, Some(&[corpus.to_string()]))
        .await
        .map_err(|e| format!("create_conversation: {e}"))?;
    let t0 = Instant::now();
    let mut observer = TurnObserver::default();
    let outcome = client
        .run_turn(&convo.id, question, TurnMode::Grounded, None, &mut observer)
        .await
        .map_err(|e| format!("run_turn: {e}"))?;
    let wall_ms = t0.elapsed().as_millis() as u64;
    let visible = strip_reasoning(&outcome.text);
    let source_origins = outcome
        .provenance
        .as_ref()
        .map(|p| p.sources.iter().map(|s| s.origin.clone()).collect())
        .unwrap_or_default();
    Ok(LaneTurn {
        visible,
        metadata: outcome.metadata,
        source_origins,
        wall_ms,
    })
}

/// Drop a `<think>` block. Same rule as `chat_cmd::render::split_reasoning`;
/// inlined rather than reached for because that function returns a rendering
/// pair this lane has no use for.
fn strip_reasoning(text: &str) -> String {
    match text.split_once("</think>") {
        Some((_, rest)) => rest.trim().to_string(),
        None => text.trim().to_string(),
    }
}

// ─── Reading the ledger ─────────────────────────────────────────────

fn gate_str<'a>(meta: &'a TurnMetadata, key: &str) -> Option<&'a str> {
    meta.grounding_gate.as_ref()?.get(key)?.as_str()
}

fn gate_u64(meta: &TurnMetadata, key: &str) -> Option<u64> {
    meta.grounding_gate.as_ref()?.get(key)?.as_u64()
}

/// The ceiling table for a model stem, or `None` — which is
/// could-not-judge, never a pass. Running a different model is not evidence
/// that this one got faster.
fn ceilings_for<'a>(bank: &'a ChatAskBank, stem: &str) -> Option<&'a toml::value::Table> {
    bank.ceilings.get(stem)?.as_table()
}

/// Every stage's ceiling key, so an un-ceilinged stage is NAMED rather than
/// silently passing. Residuals are arithmetic and carry no budget.
fn ceiling_keys(stage: StageId) -> Option<(&'static str, &'static str)> {
    match stage {
        StageId::Retrieval => Some(("retrieval_ms", "retrieval_calls")),
        StageId::Draft => Some(("draft_ms", "draft_calls")),
        StageId::Audit => Some(("audit_ms", "audit_calls")),
        StageId::ReAudit => Some(("re_audit_ms", "re_audit_calls")),
        StageId::Rewrite => Some(("rewrite_ms", "rewrite_calls")),
        StageId::Retry => Some(("retry_ms", "retry_calls")),
        StageId::Verify => Some(("verify_ms", "verify_calls")),
        StageId::Citation => Some(("citation_ms", "citation_calls")),
        StageId::Admission => Some(("admission_ms", "admission_calls")),
        StageId::Segments => Some(("segments_ms", "segments_calls")),
        StageId::GateUnattributed | StageId::TurnUnattributed => None,
    }
}

/// Which of `must_locate`'s spans the answer did not cover.
///
/// Case-insensitive substring containment, and the bank supplies STEMS
/// (`abstain`, not `abstention`) — see the bank for why. This is the #57
/// detector: a turn that answers the first half of a two-part question and
/// refuses the second reaches none of the second half's spans.
fn missing_coverage<'a>(visible: &str, must_locate: &'a [String]) -> Vec<&'a str> {
    let lower = visible.to_lowercase();
    must_locate
        .iter()
        .filter(|m| !lower.contains(&m.to_lowercase()))
        .map(String::as_str)
        .collect()
}

// ─── The lane ───────────────────────────────────────────────────────

pub(crate) async fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: svrn quality lane chat-ask");
        println!();
        println!("Ingests {BANK}'s fixture into a per-fingerprint corpus, asks the");
        println!("bank's questions three warm times each, and asserts route, per-stage");
        println!("ceilings, gate outcome, both-halves coverage, abstention and");
        println!("usefulness — each as its own named row.");
        return 0;
    }
    let ctx = LaneCtx::from_env();
    let mut report = LaneReport::new(LANE);

    let Some(repo) = find_repo_root() else {
        report.cannot_judge(
            "bank",
            "the lane reads a repo-relative bank; run it from a source checkout".into(),
        );
        return report.finish();
    };
    let bank_path = repo.join(BANK);
    let bank = match std::fs::read_to_string(&bank_path)
        .map_err(|e| format!("{}: {e}", bank_path.display()))
        .and_then(|t| parse_bank(&t))
    {
        Ok(b) => b,
        Err(e) => {
            report.cannot_judge("bank", e);
            return report.finish();
        }
    };

    let corpus = format!("{}-{}", bank.corpus_prefix, ctx.stem());
    let base = sovereign_cli_shared::urls::daemon_base_url();

    // ── Row: ingest ────────────────────────────────────────────────
    // This IS the document-ingest lane. It runs first because every row
    // below it is a claim about a turn over THIS corpus.
    let ingest_ok = ingest_row(&mut report, &repo, &bank, &corpus).await;
    if !ingest_ok {
        // Asking questions of a corpus that did not ingest measures the
        // ingest failure twice and the turn not at all.
        report.cannot_judge(
            "turn",
            format!("no questions were asked — corpus `{corpus}` did not come up"),
        );
        return report.finish();
    }

    let v1 = format!("{}/v1", base.trim_end_matches('/'));
    let judge: std::sync::Arc<dyn InferenceProvider> = std::sync::Arc::new(RemoteApiProvider::new(
        &v1,
        None,
        &bank.judge.model,
        PROVIDER_CTX,
    ));

    // ── Row: judge calibrated ──────────────────────────────────────
    // BEFORE any answer is scored. Validating the instrument after reading
    // the result is how a drifted probe gets believed.
    let calibrated = calibration_row(&mut report, &bank, judge.as_ref()).await;

    let client = TurnClient::new(&base);
    let mut per_question: Vec<(String, Vec<LaneTurn>)> = Vec::new();
    for q in &bank.questions {
        let mut kept: Vec<LaneTurn> = Vec::new();
        let total = bank.warmup + bank.runs;
        for i in 0..total {
            match ask_once(&client, &corpus, &q.text).await {
                Ok(r) => {
                    let discarded = i < bank.warmup;
                    eprintln!(
                        "  [{LANE}] {} run {}/{total} — {} ms{}",
                        q.id,
                        i + 1,
                        r.wall_ms,
                        if discarded {
                            " (warm-up, discarded)"
                        } else {
                            ""
                        }
                    );
                    if !discarded {
                        kept.push(r);
                    }
                }
                Err(e) => {
                    report.cannot_judge(
                        &format!("turn:{}", q.id),
                        format!("run {}/{total} failed: {e}", i + 1),
                    );
                }
            }
        }
        per_question.push((q.id.clone(), kept));
    }

    // ── The assertion rows, one per bank question ──────────────────
    for (q, (_, runs)) in bank.questions.iter().zip(per_question.iter()) {
        assert_question(
            &mut report,
            &bank,
            q,
            runs,
            &corpus,
            judge.as_ref(),
            calibrated,
        )
        .await;
    }

    // ── Row: per-stage baseline (TRACKED) ──────────────────────────
    baseline_row(&mut report, &ctx, &per_question);

    report.finish()
}

/// Ingest the fixture from source and assert what came out.
async fn ingest_row(
    report: &mut LaneReport,
    repo: &Path,
    bank: &ChatAskBank,
    corpus: &str,
) -> bool {
    let fixture = repo.join(&bank.fixture);
    if !fixture.is_file() {
        report.cannot_judge(
            "ingest",
            format!("fixture {} is not on disk", fixture.display()),
        );
        return false;
    }
    // `corpus ingest` takes a FOLDER, so the fixture is staged alone in a
    // temp dir — which is also what makes the chunk count deterministic:
    // nothing else in the tree can drift into it.
    let staged = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            report.cannot_judge("ingest", format!("cannot stage the fixture: {e}"));
            return false;
        }
    };
    let name = fixture.file_name().unwrap_or_default();
    if let Err(e) = std::fs::copy(&fixture, staged.path().join(name)) {
        report.cannot_judge("ingest", format!("cannot stage the fixture: {e}"));
        return false;
    }

    // Delete first. A corpus that survived the last run would make the
    // chunk count a claim about history rather than about this ingest.
    let _ =
        crate::corpus_cmd::run_corpus(&["remove".into(), corpus.to_string(), "--yes".into()]).await;

    let t0 = Instant::now();
    let code = crate::corpus_cmd::run_corpus(&[
        "ingest".into(),
        staged.path().to_string_lossy().into_owned(),
        "--corpus".into(),
        corpus.to_string(),
    ])
    .await;
    let secs = t0.elapsed().as_secs_f64();
    if code != 0 {
        report.failed("ingest", format!("`corpus ingest` exited {code}"));
        return false;
    }

    let state = sovereign_enrichment_catalog::corpus_state::inspect_corpus_state(corpus);
    let chunks = crate::corpus_cmd::status::corpus_chunk_count(
        &sovereign_enrichment_catalog::paths::index_root(corpus),
    );
    let searchable = crate::corpus_cmd::search::search_titles(corpus, &bank.search_probe, 5).await;

    let mut faults: Vec<String> = Vec::new();
    match chunks {
        Some((n, src)) if n == bank.declared_chunks => {
            tracing::debug!(chunks = n, source = src, "quality lane: chunk count");
        }
        Some((n, src)) => faults.push(format!(
            "{n} chunks (from {src}), bank declares {}",
            bank.declared_chunks
        )),
        None => faults.push("the corpus meta reports no chunk count".into()),
    }
    if state == sovereign_enrichment_catalog::corpus_state::CorpusState::Unindexed {
        faults.push("the corpus is not on disk after ingest".into());
    }
    match &searchable {
        Ok(titles) if titles.iter().any(|t| t.contains(&bank.search_must_hit)) => {}
        Ok(titles) => faults.push(format!(
            "`corpus search` for `{}` returned {:?}, none containing `{}`",
            bank.search_probe, titles, bank.search_must_hit
        )),
        Err(e) => faults.push(format!("`corpus search` failed: {e}")),
    }

    if faults.is_empty() {
        report.passed(
            "ingest",
            format!(
                "{} → `{corpus}`: {} chunks in {secs:.1}s, state {}, searchable",
                bank.fixture.display(),
                bank.declared_chunks,
                state.as_str()
            ),
        );
        true
    } else {
        report.failed(
            "ingest",
            format!("{} ({secs:.1}s ingest)", faults.join("; ")),
        );
        false
    }
}

/// Run the bank's two controls through the SAME probe the answers go
/// through, and say whether the instrument separates.
async fn calibration_row(
    report: &mut LaneReport,
    bank: &ChatAskBank,
    judge: &dyn InferenceProvider,
) -> bool {
    let q = "According to the Architecture Tour, what is the runtime pipeline, \
             and what role does the grounding gate play?";
    let good = p_useful(judge, &bank.judge.model, q, &bank.judge.control_good).await;
    let flat = p_useful(judge, &bank.judge.model, q, &bank.judge.control_flat).await;
    let (Some(good), Some(flat)) = (good, flat) else {
        report.cannot_judge(
            "judge calibrated",
            "the usefulness judge did not answer on one of its two controls".into(),
        );
        return false;
    };
    let gap = good - flat;
    let ok = good >= bank.judge.control_good_min
        && flat <= bank.judge.control_flat_max
        && gap >= bank.judge.control_gap;
    let detail = format!(
        "good {good:.3} (bar ≥{:.2}) · flat abstention {flat:.3} (bar ≤{:.2}) · gap {gap:.3} (bar ≥{:.2})",
        bank.judge.control_good_min, bank.judge.control_flat_max, bank.judge.control_gap
    );
    if ok {
        report.passed("judge calibrated", detail);
    } else {
        report.failed(
            "judge calibrated",
            format!("{detail} — `useful` below is could-not-judge, not a verdict"),
        );
    }
    ok
}

/// Every per-question row. Each is named and each has a failing input.
async fn assert_question(
    report: &mut LaneReport,
    bank: &ChatAskBank,
    q: &ChatAskQuestion,
    runs: &[LaneTurn],
    corpus: &str,
    judge: &dyn InferenceProvider,
    calibrated: bool,
) {
    let row = |name: &str| format!("{name} [{}]", q.id);
    if runs.is_empty() {
        report.cannot_judge(
            &row("ledger present"),
            "no scored run completed for this question".into(),
        );
        return;
    }

    // ── ledger present ─────────────────────────────────────────────
    let ledgers: Vec<&TurnMetadata> = runs.iter().filter_map(|r| r.metadata.as_ref()).collect();
    let with_stage: Vec<&TurnMetadata> = ledgers
        .iter()
        .copied()
        .filter(|m| m.stage_attribution.is_some())
        .collect();
    if with_stage.len() != runs.len() {
        // Never a pass, and never a failure attributed to latency: without
        // the ledger there is nothing to time.
        report.cannot_judge(
            &row("ledger present"),
            format!(
                "{} of {} runs carried a stage ledger — the rest cannot be judged",
                with_stage.len(),
                runs.len()
            ),
        );
        return;
    }
    report.passed(
        &row("ledger present"),
        format!(
            "{} of {} runs carried a stage ledger",
            with_stage.len(),
            runs.len()
        ),
    );

    // ── route ──────────────────────────────────────────────────────
    let routes: Vec<String> = ledgers
        .iter()
        .map(|m| m.routed_intent.clone().unwrap_or_else(|| "(absent)".into()))
        .collect();
    let foreign: Vec<String> = runs
        .iter()
        .flat_map(|r| r.source_origins.iter().cloned())
        .filter(|o| o != corpus)
        .collect();
    if routes.iter().all(|r| r == &q.expected_route) && foreign.is_empty() {
        report.passed(
            &row("route"),
            format!(
                "every run routed {} and cited only `{corpus}`",
                q.expected_route
            ),
        );
    } else {
        report.failed(
            &row("route"),
            format!(
                "routes {routes:?} (expected {}){}",
                q.expected_route,
                if foreign.is_empty() {
                    String::new()
                } else {
                    format!("; cited foreign origins {foreign:?}")
                }
            ),
        );
    }

    // ── per-stage ceilings ─────────────────────────────────────────
    ceilings_row(report, bank, q, runs, &row("per-stage ceilings"));

    // ── gate outcome ───────────────────────────────────────────────
    let actions: Vec<String> = ledgers
        .iter()
        .map(|m| gate_str(m, "action").unwrap_or("(absent)").to_string())
        .collect();
    let located: Vec<Option<u64>> = ledgers.iter().map(|m| gate_u64(m, "located")).collect();
    let bad_action: Vec<&String> = actions
        .iter()
        .filter(|a| !q.allowed_gate_actions.contains(a))
        .collect();
    // `located` is the #57 signature and it is treated exactly as far as it
    // goes: it EXISTS only on the citation exit (`grounding/inner.rs`), one
    // of sixteen, so `located == 0` is a failure — the gate released quotes
    // and told the reader where to look for NONE of them, which is the
    // shape the issue reported — while `located` ABSENT means the turn took
    // a different exit and says nothing either way. Failing on absence
    // would redden every non-citation turn for a reason unrelated to the
    // answer (ARCH §18.3).
    let unlocated = located.iter().filter(|l| **l == Some(0)).count();
    let absent = located.iter().filter(|l| l.is_none()).count();
    if !bad_action.is_empty() {
        report.failed(
            &row("gate outcome"),
            format!(
                "actions {actions:?} — {bad_action:?} not in {:?}",
                q.allowed_gate_actions
            ),
        );
    } else if unlocated > 0 {
        report.failed(
            &row("gate outcome"),
            format!(
                "actions {actions:?} allowed, but {unlocated} run(s) released quotes with \
                 `located: 0` — the reader was told where to look for none of them (issue #57)"
            ),
        );
    } else {
        report.passed(
            &row("gate outcome"),
            format!(
                "actions {actions:?} all within the bank's allowed set; located {located:?}\
                 {}",
                if absent == located.len() {
                    " (no run took the citation exit, which is the only one that reports it)"
                } else {
                    ""
                }
            ),
        );
    }

    // ── both halves answered ───────────────────────────────────────
    let missing: Vec<(usize, Vec<&str>)> = runs
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let absent = missing_coverage(&r.visible, &q.must_locate);
            (!absent.is_empty()).then_some((i + 1, absent))
        })
        .collect();
    if missing.is_empty() {
        report.passed(
            &row("both halves answered"),
            format!("every run covered {:?}", q.must_locate),
        );
    } else {
        report.failed(
            &row("both halves answered"),
            format!("runs missing required coverage: {missing:?}"),
        );
    }

    // ── not abstained ──────────────────────────────────────────────
    // The production gate's OWN decision, through the one decider
    // (`bench_cmd::chaos_monkey::action_from_gate_signal`). No second
    // abstention detector lives here.
    // `sovereign-eval` is back-of-house and `bench_cmd` is the only module
    // in this crate allowed to name it, so this asks for the DECISION and
    // not for the harness's `AgentAction` vocabulary. Still one decider.
    let actions_typed: Vec<Option<bool>> = runs
        .iter()
        .map(|r| {
            let gate_action = r.metadata.as_ref().and_then(|m| gate_str(m, "action"));
            crate::bench_cmd::chaos_monkey::abstained_from_gate_signal(gate_action, &r.visible)
        })
        .collect();
    let abstained = actions_typed.iter().filter(|a| **a == Some(true)).count();
    // `None` means the turn carried no gate signal at all. `chaos_monkey`
    // reaches for a judge there; this lane does NOT, because it only ever
    // asks GROUNDED turns — a missing gate signal here is a broken run, not
    // a naked baseline. Reporting it as "answered" via a length heuristic
    // would be a green with nothing behind it (ARCH §18.3).
    let unsignalled = actions_typed.iter().filter(|a| a.is_none()).count();
    if unsignalled > 0 {
        report.cannot_judge(
            &row("not abstained"),
            format!(
                "{unsignalled} of {} runs carried no gate signal — a grounded turn always \
                 does, so this run cannot be scored either way",
                runs.len()
            ),
        );
    } else if abstained == 0 {
        report.passed(
            &row("not abstained"),
            format!("{} of {} runs answered", runs.len(), runs.len()),
        );
    } else {
        report.failed(
            &row("not abstained"),
            format!(
                "{abstained} of {} runs abstained on an answerable question",
                runs.len()
            ),
        );
    }

    // ── useful ─────────────────────────────────────────────────────
    let mut scores: Vec<f64> = Vec::new();
    for r in runs {
        if let Some(p) = p_useful(judge, &bank.judge.model, &q.text, &r.visible).await {
            scores.push(p);
        }
    }
    let med = median(scores.clone());
    match (calibrated, med) {
        (false, _) => report.cannot_judge(
            &row("useful"),
            format!("the usefulness judge is not calibrated on this run; scores were {scores:?}"),
        ),
        (true, None) => report.cannot_judge(
            &row("useful"),
            "the usefulness judge did not answer on any run".into(),
        ),
        (true, Some(m)) if m >= bank.judge.threshold => report.passed(
            &row("useful"),
            format!(
                "median P(useful) {m:.3} ≥ {:.2} over {scores:?}",
                bank.judge.threshold
            ),
        ),
        (true, Some(m)) => report.failed(
            &row("useful"),
            format!(
                "median P(useful) {m:.3} < {:.2} over {scores:?}",
                bank.judge.threshold
            ),
        ),
    }
}

/// Compare every measured stage against the bank's pre-registered ceiling
/// for the model stem that produced it.
fn ceilings_row(
    report: &mut LaneReport,
    bank: &ChatAskBank,
    q: &ChatAskQuestion,
    runs: &[LaneTurn],
    subject: &str,
) {
    // Every ledger row carries no model stem of its own, so the stem is the
    // run's inference backend. A stem with no ceiling table is
    // could-not-judge — running a different model is not evidence that this
    // one got faster.
    let Some(stem) = model_stem() else {
        report.cannot_judge(
            subject,
            "cannot resolve the primary model stem, so no ceiling table applies".into(),
        );
        return;
    };
    let Some(table) = ceilings_for(bank, &stem) else {
        report.cannot_judge(
            subject,
            format!("no ceiling table for model stem `{stem}` — declare one in {BANK}"),
        );
        return;
    };
    let ms_bar = |k: &str| {
        table
            .get(k)
            .and_then(toml::Value::as_integer)
            .map(|n| n as u64)
    };

    let mut breaches: Vec<String> = Vec::new();
    let mut unbudgeted: Vec<String> = Vec::new();
    let mut worst_total = 0u64;
    for (i, r) in runs.iter().enumerate() {
        let Some(l) = r
            .metadata
            .as_ref()
            .and_then(|m| m.stage_attribution.as_ref())
        else {
            continue;
        };
        worst_total = worst_total.max(l.total_ms);
        if let Some(bar) = ms_bar("total_ms") {
            if l.total_ms > bar {
                breaches.push(format!("run {}: total {} ms > {bar}", i + 1, l.total_ms));
            }
        }
        for srow in &l.rows {
            let Some((ms_key, calls_key)) = ceiling_keys(srow.stage) else {
                continue; // residual: arithmetic, not a budget
            };
            match ms_bar(ms_key) {
                Some(bar) if srow.ms > bar => breaches.push(format!(
                    "run {}: {} {} ms > {bar}",
                    i + 1,
                    srow.stage.label(),
                    srow.ms
                )),
                Some(_) => {}
                // A stage that RAN with no declared budget is named, not
                // passed over: an un-ceilinged stage is exactly where a
                // regression hides (ARCH §18.3).
                None => {
                    let note = format!("{} ({} ms)", srow.stage.label(), srow.ms);
                    if !unbudgeted.contains(&note) {
                        unbudgeted.push(note);
                    }
                }
            }
            if let (Some(bar), Some(calls)) = (
                table.get(calls_key).and_then(toml::Value::as_integer),
                srow.calls,
            ) {
                if i64::from(calls) > bar {
                    breaches.push(format!(
                        "run {}: {} {calls} calls > {bar}",
                        i + 1,
                        srow.stage.label()
                    ));
                }
            }
        }
    }

    if !breaches.is_empty() {
        report.failed(subject, format!("on `{stem}`: {}", breaches.join("; ")));
    } else if !unbudgeted.is_empty() {
        report.cannot_judge(
            subject,
            format!(
                "on `{stem}`: every declared ceiling held (worst total {worst_total} ms), but \
                 these stages ran with no ceiling row in {BANK}: {}",
                unbudgeted.join(", ")
            ),
        );
    } else {
        report.passed(
            subject,
            format!(
                "on `{stem}`: every stage within its pre-registered ceiling ({} run(s) of `{}`, worst total {worst_total} ms)",
                runs.len(),
                q.id
            ),
        );
    }
}

/// The TRACKED row: this stack's last run, if there is one for this
/// fingerprint.
///
/// **A run with no baseline for its fingerprint is could-not-judge
/// (first-run) and writes NOTHING.** `--mint` is the only door, because a
/// baseline minted from a run nobody watched is a bar nobody set.
fn baseline_row(report: &mut LaneReport, ctx: &LaneCtx, per_question: &[(String, Vec<LaneTurn>)]) {
    let stages: Vec<(String, u64)> = per_question
        .iter()
        .flat_map(|(qid, runs)| {
            runs.iter()
                .filter_map(|r| r.metadata.as_ref()?.stage_attribution.as_ref())
                .flat_map(move |l| {
                    l.rows
                        .iter()
                        .map(move |s| (format!("{qid}:{}", s.stage.label()), s.ms))
                })
        })
        .collect();
    if stages.is_empty() {
        report.cannot_judge(
            "per-stage baseline",
            "no stage rows were measured, so nothing can be compared".into(),
        );
        return;
    }
    let (Some(fp), Some(dir)) = (ctx.fingerprint.as_deref(), ctx.baseline_dir.as_deref()) else {
        report.cannot_judge(
            "per-stage baseline",
            format!(
                "no fingerprint or baseline dir for this run ({} stage rows measured) — \
                 run under `svrn quality check` to compare",
                stages.len()
            ),
        );
        return;
    };
    let path = dir.join(fp).join("latest.json");
    let current = summarise(&stages);
    if !path.exists() {
        if ctx.mint {
            let wrote =
                std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).and_then(|()| {
                    std::fs::write(
                        &path,
                        format!("{}\n", serde_json::to_string_pretty(&current)?),
                    )
                });
            match wrote {
                Ok(()) => report.cannot_judge(
                    "per-stage baseline",
                    format!(
                        "first run for stack `{fp}` — minted {} (--mint). Nothing was compared",
                        path.display()
                    ),
                ),
                Err(e) => report.cannot_judge(
                    "per-stage baseline",
                    format!("first run for stack `{fp}`, and --mint could not write it: {e}"),
                ),
            }
        } else {
            report.cannot_judge(
                "per-stage baseline",
                format!(
                    "first run for stack `{fp}` — no baseline at {}, and this run wrote none \
                     (pass --mint to set one)",
                    path.display()
                ),
            );
        }
        return;
    }
    let Ok(prev) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .ok_or(())
    else {
        report.cannot_judge(
            "per-stage baseline",
            format!("the baseline at {} is unreadable", path.display()),
        );
        return;
    };
    let moved = compare(&prev, &current);
    report.push(if moved.is_empty() {
        Judgement::passed(
            "per-stage baseline",
            reason(format!(
                "every stage within tolerance of {} ({} stage rows)",
                path.display(),
                stages.len()
            )),
        )
    } else {
        // TRACKED: recorded, not gated. The nightly is where drift is
        // judged; this lane is about breakage.
        Judgement::passed(
            "per-stage baseline",
            reason(format!(
                "tracked movement vs {}: {}",
                path.display(),
                moved.join("; ")
            )),
        )
    });
}

/// Median ms per stage key, which is what a baseline compares.
fn summarise(stages: &[(String, u64)]) -> serde_json::Value {
    let mut by: std::collections::BTreeMap<&str, Vec<f64>> = std::collections::BTreeMap::new();
    for (k, ms) in stages {
        by.entry(k.as_str()).or_default().push(*ms as f64);
    }
    let obj: serde_json::Map<String, serde_json::Value> = by
        .into_iter()
        .filter_map(|(k, v)| median(v).map(|m| (k.to_string(), serde_json::json!(m))))
        .collect();
    serde_json::json!({ "schema": "quality-check/chat-ask-baseline/v1", "stage_ms": obj })
}

/// Stages that moved more than 25% against the baseline. Reported, not
/// gated — at three runs a band would be noise (RUNBOOK §6).
fn compare(prev: &serde_json::Value, cur: &serde_json::Value) -> Vec<String> {
    let (Some(p), Some(c)) = (
        prev.get("stage_ms").and_then(|v| v.as_object()),
        cur.get("stage_ms").and_then(|v| v.as_object()),
    ) else {
        return vec!["the baseline has no `stage_ms` block".into()];
    };
    let mut out = Vec::new();
    for (k, cv) in c {
        let (Some(pv), Some(cv)) = (p.get(k).and_then(|v| v.as_f64()), cv.as_f64()) else {
            continue;
        };
        if pv > 0.0 && ((cv - pv) / pv).abs() > 0.25 {
            out.push(format!("{k} {pv:.0} → {cv:.0} ms"));
        }
    }
    out
}

/// The model stem the primary slot resolves to. `None` rather than a guess.
fn model_stem() -> Option<String> {
    let cfg = sovereign_contracts::setup_config::SetupConfig::load().ok()?;
    cfg.primary_model_stem()
}

/// Walk up to the enclosing checkout (the dir holding `quality/` and
/// `sovereign/`), the same shape `posture` and the runner use.
fn find_repo_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("quality").is_dir() && dir.join("sovereign").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bank_text() -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../bench/quality-check/chat-ask.toml"),
        )
        .expect("the shipped bank")
    }

    /// The SHIPPED bank parses. A bank this lane cannot read is a lane that
    /// reports could-not-judge for thirty minutes and tells nobody why.
    #[test]
    fn the_shipped_bank_parses_and_declares_both_questions() {
        let b = parse_bank(&bank_text()).expect("the shipped bank parses");
        assert_eq!(b.declared_chunks, 40);
        assert_eq!(b.runs, 3);
        assert_eq!(b.warmup, 1);
        let ids: Vec<&str> = b.questions.iter().map(|q| q.id.as_str()).collect();
        assert_eq!(ids, vec!["q1", "q2"]);
        // q1 is issue #57 verbatim — the two halves are what the lane is for.
        assert!(b.questions[0]
            .text
            .contains("what role does the grounding gate play"));
        // Every question must declare what covering BOTH halves looks like,
        // or `both halves answered` is a row with no failing input.
        for q in &b.questions {
            assert!(!q.must_locate.is_empty(), "{}", q.id);
            assert!(!q.allowed_gate_actions.is_empty(), "{}", q.id);
        }
    }

    /// Every field is required. A bank that half-parses drops an assertion,
    /// and a dropped assertion reads exactly like a passing one.
    #[test]
    fn a_bank_missing_any_required_field_is_refused() {
        let full = bank_text();
        for key in [
            "declared_chunks",
            "search_probe",
            "runs",
            "control_gap",
            "threshold",
            "expected_route",
        ] {
            let broken: String = full
                .lines()
                .filter(|l| !l.trim_start().starts_with(key))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                parse_bank(&broken).is_err(),
                "removing `{key}` must refuse the bank"
            );
        }
        assert!(parse_bank("").is_err());
    }

    /// The pre-registered ceilings are in the bank for the stem this host
    /// runs, and they are the order's numbers. A ceiling edited to fit a
    /// failing run is not a ceiling.
    #[test]
    fn the_pre_registered_ceilings_are_the_orders_numbers() {
        let b = parse_bank(&bank_text()).unwrap();
        let t = ceilings_for(&b, "Qwen3.6-35B-A3B-UD-MTP-IQ4_NL")
            .expect("this host's stem has a ceiling table");
        let n = |k: &str| t.get(k).and_then(toml::Value::as_integer).unwrap();
        assert_eq!(n("retrieval_ms"), 2000);
        assert_eq!(n("draft_ms"), 30000);
        assert_eq!(n("draft_calls"), 1);
        assert_eq!(n("citation_ms"), 15000);
        assert_eq!(n("audit_ms"), 30000);
        assert_eq!(n("audit_calls"), 12);
        assert_eq!(n("total_ms"), 60000);
    }

    /// A model stem with no ceiling table is could-not-judge, never a pass.
    #[test]
    fn an_unknown_model_stem_has_no_ceilings() {
        let b = parse_bank(&bank_text()).unwrap();
        assert!(ceilings_for(&b, "SomeOtherModel-7B").is_none());
    }

    /// Residuals are arithmetic, not measured stages: giving them a ceiling
    /// would gate on a subtraction.
    #[test]
    fn residual_rows_carry_no_ceiling() {
        assert!(ceiling_keys(StageId::GateUnattributed).is_none());
        assert!(ceiling_keys(StageId::TurnUnattributed).is_none());
        assert!(ceiling_keys(StageId::Retrieval).is_some());
        assert!(ceiling_keys(StageId::Audit).is_some());
    }

    /// **The #57 detector, watched failing.** The lane's whole reason for
    /// existing is a turn that answered the first half of a two-part
    /// question and refused the second; this is that answer, and the row
    /// must reject it. Its counterpart is today's answer, which covers the
    /// gate's role in prose and must pass.
    ///
    /// The bank supplies STEMS for exactly this reason: the lane's first
    /// instrumented run (2026-09-04) failed this row on `abstention`
    /// against an answer reading "the model must honestly abstain" — a
    /// wording coupling, not a missing half.
    #[test]
    fn both_halves_answered_rejects_the_issue_57_shape() {
        let b = parse_bank(&bank_text()).unwrap();
        let q1 = &b.questions[0];

        // Issue #57's answer, verbatim in shape: the pipeline, then a
        // refusal of the second half, plus one quote.
        let refused = "Runtime pipeline: router → policy → retrieval → synthesis → \
                       grounding gate\n\nThe passages do not answer: Role of the \
                       grounding gate";
        let absent = missing_coverage(refused, &q1.must_locate);
        assert!(
            !absent.is_empty(),
            "the #57 half-refusal must NOT cover the gate's role; missing {absent:?}"
        );

        // Today's answer (2026-09-04, post-fix) covers it.
        let answered = "The grounding gate acts as a mandatory verification checkpoint. \
                        It extracts individual claims and verifies them against a sealed \
                        corpus; if unsupported, the model must honestly abstain.";
        assert_eq!(
            missing_coverage(answered, &q1.must_locate),
            Vec::<&str>::new(),
            "an answer that covers the gate's role must pass the row"
        );

        // And the check is not vacuous in the other direction: an empty
        // `must_locate` would pass everything, which is why `parse_bank`
        // refuses one (see `a_bank_missing_any_required_field_is_refused`).
        assert!(!q1.must_locate.is_empty());
    }

    #[test]
    fn the_median_of_three_is_the_middle_one() {
        assert_eq!(median(vec![0.1, 0.9, 0.5]), Some(0.5));
        assert_eq!(median(Vec::new()), None);
    }

    /// The judge prompt is ONE spelling: the controls calibrate the scored
    /// answers only if both go through it.
    #[test]
    fn the_controls_and_the_answers_share_one_prompt() {
        let a = usefulness_prompt("Q", "answer");
        let b = usefulness_prompt("Q", "control");
        assert_eq!(
            a.replace("answer\n", "X\n"),
            b.replace("control\n", "X\n"),
            "the only difference between a control and a scored answer must be the answer"
        );
    }

    #[test]
    fn reasoning_blocks_do_not_reach_the_judge() {
        assert_eq!(
            strip_reasoning("<think>plan</think>The answer"),
            "The answer"
        );
        assert_eq!(strip_reasoning("bare"), "bare");
    }
}
