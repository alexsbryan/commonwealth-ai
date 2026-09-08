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
//! | gate outcome honest | HARD | the action left the allowed set, or it disagrees with the ledger the reader is shown |
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
    /// The additive term of the ceiling formula, declared ONCE for every
    /// stage and every model stem — see `ceiling_from_median`.
    ceiling_floor_ms: u64,
    /// The 1-minute load average above which this lane's ONE wall-clock row
    /// (`per-stage ceilings`) is could-not-judge rather than failed.
    host_quiet_max_load: f64,
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
    // Required, not defaulted: a bank with no floor would silently restore
    // the bare 1.5x rule, which is the bug this replaced (ARCH §18.3).
    let ceiling_floor_ms = want(&doc, "ceiling_floor_ms")?
        .as_integer()
        .filter(|n| *n >= 0)
        .ok_or_else(|| format!("{BANK}: `ceiling_floor_ms` must be a non-negative integer"))?
        as u64;
    let host_quiet_max_load = want(&doc, "host_quiet_max_load")?
        .as_float()
        .filter(|f| f.is_finite() && *f > 0.0)
        .ok_or_else(|| format!("{BANK}: `host_quiet_max_load` must be a positive float"))?;
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
        ceiling_floor_ms,
        host_quiet_max_load,
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
    /// The turn's epistemic ledger, as the wire carried it. The `gate
    /// outcome honest` row reads the holdings' `verification`: an action
    /// and its holdings must tell the same story.
    epistemic: Option<sovereign_contracts::types::EpistemicState>,
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
        epistemic: outcome.epistemic_state,
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

/// **THE ceiling formula.** One implementation, one name (ARCH §10.6).
///
/// `ceiling = max(1.5 x median, median + floor_ms)`
///
/// The multiplicative term is what a slow stage needs; the additive term is
/// what a fast one needs, and before the floor existed there was no additive
/// term at all. `retrieval` has a 193 ms median, so 1.5x set its bar at
/// 290 ms and a run failed the whole lane on `retrieval 298 ms > 290` —
/// eight milliseconds, well inside the stage's own run-to-run spread. A bar
/// that fires on noise is not a bar; `max` takes whichever term makes the
/// weaker claim.
///
/// `floor_ms` is declared ONCE, in the bank, and applies to every stage.
fn ceiling_from_median(median_ms: u64, floor_ms: u64) -> u64 {
    let multiplicative = (median_ms as f64 * 1.5).round() as u64;
    let additive = median_ms.saturating_add(floor_ms);
    multiplicative.max(additive)
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
        StageId::Retrieval => Some(("retrieval_median_ms", "retrieval_calls")),
        StageId::Draft => Some(("draft_median_ms", "draft_calls")),
        StageId::Audit => Some(("audit_median_ms", "audit_calls")),
        StageId::ReAudit => Some(("re_audit_median_ms", "re_audit_calls")),
        StageId::Rewrite => Some(("rewrite_median_ms", "rewrite_calls")),
        StageId::Retry => Some(("retry_median_ms", "retry_calls")),
        StageId::Verify => Some(("verify_median_ms", "verify_calls")),
        StageId::Citation => Some(("citation_median_ms", "citation_calls")),
        StageId::Admission => Some(("admission_median_ms", "admission_calls")),
        StageId::Segments => Some(("segments_median_ms", "segments_calls")),
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

/// What the `gate outcome honest` row decided, and why.
#[derive(Debug, PartialEq, Eq)]
enum Honesty {
    Honest,
    /// The action and the ledger tell different stories.
    Dishonest(String),
}

/// **Does this turn's ledger say the same thing its action does?**
///
/// A gate action is a claim about how far verification got; the holdings are
/// what the reader is shown. Issue #57 was those two disagreeing — eight shed
/// judges shipped as eight `Verified` holdings on a turn whose action said
/// `released`. The old `gate outcome` row could not have seen it: it read the
/// action against an allow-list and never looked at the ledger at all.
///
/// Three rules, each the contrapositive of a way the disagreement shows up:
///
/// 1. `judge_failed_open` (and every other fail-open action) ⇒ NOTHING is
///    `Verified`. The gate reached no verdict; a verified holding under that
///    action is the §18.3 defect literally — an `Err` in a success shape.
/// 2. A HELD-reach action (`released`, `verified`, `retry_released`,
///    `rewrite_released`, `citation_grounded`) ⇒ nothing is `FailOpen`. A
///    claim nobody judged cannot ride out under an action that says every
///    claim held; that turn's honest exit is `judge_failed_open`.
/// 3. `citation_grounded` ⇒ every released quote is OPENABLE
///    (`openable >= quotes`). A citation the reader cannot open is a promise
///    the release did not keep.
///
/// A FLAWED-reach action (`annotated_marked`, `annotated_no_retry`, …) is
/// deliberately unconstrained beyond the bank's allow-list: it means "some
/// claims were flagged", and a mix of `Verified`, `FailedOnce` and `FailOpen`
/// is the honest ledger for it.
///
/// **`located` is NOT rule 3, and that was measured before it was written.**
/// The row used to fail on `located == 0`, reading it as "the reader was told
/// where to look for none of them". `located` counts quotes carrying a
/// SECTION HEADING, joined out of the corpus's `governance_view` enrichment
/// (`grounding::gate_evidence_locators`), and the chat-ask fixture is ingested
/// with no enrichment at all — so it is structurally 0 on this corpus and on
/// every un-enriched one. Measured on this host, 8 of 8 `citation_grounded`
/// turns: `located 0`, `openable == quotes` (2/2 and 1/1), every quote
/// carrying a `(corpus_id, chunk_id)` target. Nothing was unlocatable; the row
/// was reading the wrong field. `openable` is the one that answers it, and
/// `inner.rs` says so: "openable <= quotes, and it is INDEPENDENT of located
/// in both directions … Reading either as a proxy for the other would
/// misreport both."
fn gate_outcome_honest(
    action: &str,
    verifications: &[String],
    quotes: Option<u64>,
    openable: Option<u64>,
) -> Honesty {
    let fail_open_action = matches!(
        action,
        "judge_failed_open" | "retry_released_unverified" | "rewrite_released_unverified"
    );
    let held_action = matches!(
        action,
        "released" | "verified" | "retry_released" | "rewrite_released" | "citation_grounded"
    );
    if fail_open_action {
        let verified = verifications.iter().filter(|v| *v == "verified").count();
        if verified > 0 {
            return Honesty::Dishonest(format!(
                "`{action}` reached no verdict, yet {verified} of {} holding(s) say `verified`",
                verifications.len()
            ));
        }
    }
    if held_action {
        let open = verifications.iter().filter(|v| *v == "fail_open").count();
        if open > 0 {
            return Honesty::Dishonest(format!(
                "`{action}` says every claim held, yet {open} of {} holding(s) say `fail_open`                  — that turn's honest exit is `judge_failed_open`",
                verifications.len()
            ));
        }
    }
    if action == "citation_grounded" {
        match (quotes, openable) {
            (Some(q), Some(o)) if o < q => {
                return Honesty::Dishonest(format!(
                    "released {q} quote(s) of which only {o} can be opened"
                ));
            }
            (Some(q), None) if q > 0 => {
                return Honesty::Dishonest(format!(
                    "released {q} quote(s) and reported no `openable` count at all"
                ));
            }
            _ => {}
        }
    }
    Honesty::Honest
}

// ─── The lane ───────────────────────────────────────────────────────

pub(crate) async fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: svrn quality lane chat-ask");
        println!();
        println!("Ingests {BANK}'s fixture into a per-fingerprint corpus, asks the");
        println!("bank's questions three warm times each, and asserts route, per-stage");
        println!("ceilings, gate-outcome honesty, both-halves coverage, abstention and");
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

    // ── gate outcome honest ────────────────────────────────────────
    // Two things, and the second is the one issue #57 needed: the action
    // must be in the bank's allowed set, AND the action must agree with the
    // ledger the reader is shown (`gate_outcome_honest`).
    let actions: Vec<String> = ledgers
        .iter()
        .map(|m| gate_str(m, "action").unwrap_or("(absent)").to_string())
        .collect();
    let located: Vec<Option<u64>> = ledgers.iter().map(|m| gate_u64(m, "located")).collect();
    let bad_action: Vec<&String> = actions
        .iter()
        .filter(|a| !q.allowed_gate_actions.contains(a))
        .collect();
    let verdicts: Vec<(usize, Honesty)> = runs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let action = r
                .metadata
                .as_ref()
                .and_then(|m| gate_str(m, "action"))
                .unwrap_or("(absent)");
            let verifications: Vec<String> = r
                .epistemic
                .as_ref()
                .map(|e| {
                    e.holdings
                        .iter()
                        .map(|h| {
                            serde_json::to_value(h.verification)
                                .ok()
                                .and_then(|v| v.as_str().map(str::to_string))
                                .unwrap_or_else(|| "(unserializable)".into())
                        })
                        .collect()
                })
                .unwrap_or_default();
            let quotes = r.metadata.as_ref().and_then(|m| gate_u64(m, "quotes"));
            let openable = r.metadata.as_ref().and_then(|m| gate_u64(m, "openable"));
            (
                i + 1,
                gate_outcome_honest(action, &verifications, quotes, openable),
            )
        })
        .collect();
    let dishonest: Vec<String> = verdicts
        .iter()
        .filter_map(|(i, v)| match v {
            Honesty::Dishonest(why) => Some(format!("run {i}: {why}")),
            Honesty::Honest => None,
        })
        .collect();
    let holdings_seen: usize = runs
        .iter()
        .filter_map(|r| r.epistemic.as_ref())
        .map(|e| e.holdings.len())
        .sum();
    if !bad_action.is_empty() {
        report.failed(
            &row("gate outcome honest"),
            format!(
                "actions {actions:?} — {bad_action:?} not in {:?}",
                q.allowed_gate_actions
            ),
        );
    } else if !dishonest.is_empty() {
        report.failed(
            &row("gate outcome honest"),
            format!("actions {actions:?} allowed, but the ledger disagrees — {dishonest:?}"),
        );
    } else if holdings_seen == 0 {
        // No holdings anywhere: the action rule passed and the ledger rule
        // had nothing to read. Not a pass on the half that matters
        // (ARCH §18.1) — four verdicts, not two.
        report.cannot_judge(
            &row("gate outcome honest"),
            format!(
                "actions {actions:?} are within the allowed set, but no run carried a single \
                 holding — there is no ledger to check the action against"
            ),
        );
    } else {
        report.passed(
            &row("gate outcome honest"),
            format!(
                "actions {actions:?} allowed and agreed with {holdings_seen} holding(s); \
                 located {located:?} (section headings, absent on an un-enriched corpus \
                 by design — `openable` is what says a quote can be opened)"
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
    // THE PRECONDITION. This is the lane's only wall-clock row, and a
    // wall-clock bar measured on a contended host verifies nothing: the
    // same binary and bank decoded at 50.7 tok/s at load 3.7 and 17.8 tok/s
    // at load 32 on this machine. Could-not-judge NAMING the load it saw —
    // never failed, and never quietly passed either (ARCH §18.3).
    //
    // The same `sovereign_cli_shared::host_load` reader the check runner's
    // `Precondition::HostQuiet` uses, so the two cannot disagree about what
    // "quiet" means (ARCH §10.6). Read at the END of the runs rather than
    // the start: it is the interval the stages were measured over that has
    // to have been quiet, and a 1-minute average taken now covers it.
    let quiet = sovereign_cli_shared::host_load::host_quiet(bank.host_quiet_max_load);
    if let Some(why) = quiet.reason() {
        report.cannot_judge(subject, why);
        return;
    }

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
    // The bank declares the MEASURED median; the bar is derived from it by
    // the one formula. `ceiling_floor_ms` is declared once, at the bank's top
    // level, so it cannot drift per stage.
    let floor_ms = bank.ceiling_floor_ms;
    let ms_bar = |k: &str| {
        table
            .get(k)
            .and_then(toml::Value::as_integer)
            .map(|n| ceiling_from_median(n as u64, floor_ms))
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
        if let Some(bar) = ms_bar("total_median_ms") {
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

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| (*s).to_string()).collect()
    }

    /// ISSUE #57 ON THE WIRE. A `judge_failed_open` turn whose holdings say
    /// `verified` is the defect verbatim — the gate reached no verdict and
    /// the reader is shown four confirmations.
    ///
    /// FAILS IF the fail-open arm is dropped from `gate_outcome_honest`:
    /// this row goes green on a ledger that claims verification nobody did.
    #[test]
    fn a_fail_open_turn_may_not_show_a_verified_holding() {
        assert_eq!(
            gate_outcome_honest(
                "judge_failed_open",
                &v(&["fail_open", "fail_open"]),
                None,
                None
            ),
            Honesty::Honest
        );
        let d = gate_outcome_honest(
            "judge_failed_open",
            &v(&["fail_open", "verified"]),
            None,
            None,
        );
        assert!(
            matches!(d, Honesty::Dishonest(ref why) if why.contains("verified")),
            "{d:?}"
        );
    }

    /// The other direction: an action that says every claim held cannot be
    /// carrying a claim nobody judged. That turn's honest exit exists and is
    /// `judge_failed_open`.
    #[test]
    fn a_held_action_may_not_carry_an_unjudged_holding() {
        assert_eq!(
            gate_outcome_honest("released", &v(&["verified", "verified"]), None, None),
            Honesty::Honest
        );
        let d = gate_outcome_honest("released", &v(&["verified", "fail_open"]), None, None);
        assert!(
            matches!(d, Honesty::Dishonest(ref why) if why.contains("fail_open")),
            "{d:?}"
        );
        // A FLAWED release is deliberately unconstrained: "some claims were
        // flagged" is what it means, and a mixed ledger is honest for it.
        assert_eq!(
            gate_outcome_honest(
                "annotated_marked",
                &v(&["verified", "failed_once", "fail_open"]),
                None,
                None
            ),
            Honesty::Honest
        );
    }

    /// A citation the reader cannot open is a promise the release did not
    /// keep — and it is `openable` that says so, never `located`.
    ///
    /// The measured case this pins: 8 of 8 `citation_grounded` turns on this
    /// host reported `located: 0` with `openable == quotes`. Under the old
    /// row every one of them was a failure; under this one they pass, and a
    /// genuinely unopenable quote still fails.
    #[test]
    fn a_citation_release_is_judged_on_openable_not_on_section_headings() {
        assert_eq!(
            gate_outcome_honest("citation_grounded", &v(&["verified"]), Some(2), Some(2)),
            Honesty::Honest,
            "located is not consulted at all"
        );
        let d = gate_outcome_honest("citation_grounded", &v(&["verified"]), Some(2), Some(1));
        assert!(
            matches!(d, Honesty::Dishonest(ref why) if why.contains("opened")),
            "{d:?}"
        );
        // Quotes released and no openable count reported at all is not a
        // pass either — absence is reported, never defaulted (§18.3).
        let d = gate_outcome_honest("citation_grounded", &v(&["verified"]), Some(1), None);
        assert!(matches!(d, Honesty::Dishonest(_)), "{d:?}");
    }

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

    /// The bank declares the MEASURED medians for the stem this host runs,
    /// and the bar is derived from them by one formula.
    ///
    /// This test asserted the order's opening ceilings (2000/30000/15000/
    /// 30000/60000) and went red on 2026-09-04 when commit bab7df796 re-set
    /// the bank from a quiet run without updating it — a pre-existing
    /// failure this order inherited, not one it caused. It now asserts what
    /// the bank actually carries, which is the medians.
    ///
    /// A median edited to fit a failing run is not a measurement, and unlike
    /// a hand-edited ceiling it is a claim a re-run can contradict.
    #[test]
    fn the_bank_declares_the_measured_medians_for_this_hosts_stem() {
        let b = parse_bank(&bank_text()).unwrap();
        let t = ceilings_for(&b, "Qwen3.6-35B-A3B-UD-MTP-IQ4_NL")
            .expect("this host's stem has a ceiling table");
        let n = |k: &str| t.get(k).and_then(toml::Value::as_integer).unwrap();
        assert_eq!(n("retrieval_median_ms"), 193);
        assert_eq!(n("draft_median_ms"), 22573);
        assert_eq!(n("draft_calls"), 1);
        assert_eq!(n("citation_median_ms"), 7734);
        assert_eq!(n("audit_median_ms"), 8954);
        assert_eq!(n("audit_calls"), 12);
        assert_eq!(n("total_median_ms"), 31728);
        // The floor is declared ONCE, outside the per-stem table.
        assert_eq!(b.ceiling_floor_ms, 250);
    }

    /// The formula, and the reason it has an additive term at all.
    ///
    /// `retrieval`'s 193 ms median put the 1.5x bar at 290 ms, and a run
    /// failed the whole lane on `retrieval 298 ms > 290` — eight
    /// milliseconds. The floor is what makes a bar on a fast stage a bar
    /// rather than a coin toss.
    #[test]
    fn the_ceiling_formula_takes_whichever_term_makes_the_weaker_claim() {
        // Small stage: the additive term wins, and the 8 ms miss now passes.
        assert_eq!(ceiling_from_median(193, 250), 443);
        assert!(
            298 < ceiling_from_median(193, 250),
            "the run-2 failure was noise, not a regression"
        );
        // Large stages: 1.5x dominates, so the floor is invisible there —
        // these are the 2026-09-04 ceilings to within rounding.
        assert_eq!(ceiling_from_median(22573, 250), 33860);
        assert_eq!(ceiling_from_median(7734, 250), 11601);
        assert_eq!(ceiling_from_median(8954, 250), 13431);
        assert_eq!(ceiling_from_median(31728, 250), 47592);
        // The crossover: below 500 ms the floor binds, above it the
        // multiplier does.
        assert_eq!(ceiling_from_median(500, 250), 750);
        assert_eq!(ceiling_from_median(501, 250), 752);
        // A zero-median stage still gets a real band rather than a zero bar.
        assert_eq!(ceiling_from_median(0, 250), 250);
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
