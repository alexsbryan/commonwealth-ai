// SPDX-License-Identifier: AGPL-3.0-or-later
//! `binder_replay` — the deep-research audit's LOCATION LOOP, replayed
//! per-claim against the live daemon, both ways, on a small bed.
//!
//! # Why this exists
//!
//! Measured on the pin-validate flight of 2026-08-25
//! (`research/deep-research/arms/runs-pin-validate/pinned-1.log`): 328 claim
//! audits took 102.5 minutes, and 35 of them — the 11% that reached the
//! binder — consumed 90.6 of those minutes, 88% of the audit, at ~130s each.
//! The other 285 short-circuited earlier and averaged 1.85s.
//!
//! Tuning that loop through whole flights costs 105 minutes per answer, and
//! `arms/bed/` (the outer bed) is deliberately at that granularity — it scores
//! deliverables. **You do not need all 35 claims to know what to tune.** This
//! harness replays the handful that actually reach the loop, through the
//! production `assess_claim`, with no acquisition, no writer and no judge in
//! the way: a baseline arm costs minutes and a candidate arm costs seconds.
//!
//! # What it measures
//!
//! Per claim, two arms against the same bed:
//!
//! - **per-span** (`SOVEREIGN_DR_AUDIT_BATCH_LOCATE=0`) — one calibrated
//!   forced-choice call per candidate chunk. The pre-2026-08-26 loop.
//! - **batched** (`=1`) — one triage generation over every candidate span,
//!   then the calibrated call only for spans the triage admitted or could not
//!   align a verdict for.
//!
//! and reports, for each: wall-clock, the verdict, and **the bound chunk set**.
//! The bound set is the honest measure. Wall-clock alone would make a triage
//! that rejects everything look like a triumph; what decides whether the batch
//! may stay on is whether it binds what the calibrated loop bound. A chunk in
//! `per_span_only` is support the batch LOST — the abstention-direction cost
//! that `SOVEREIGN_DR_AUDIT_BATCH_LOCATE`'s ledger row makes the flip
//! condition. A chunk in `batched_only` is the opposite and should be
//! impossible: the calibrated register decides every bound span in BOTH arms,
//! so a disagreement in that direction means the two arms are not judging the
//! same thing (sampling drift, a cache effect, or a real bug) and the run says
//! so instead of averaging it away.
//!
//! # It asserts nothing about which arm wins
//!
//! It records. A single replay of six claims is not a bank, and §18.5 says a
//! single-run delta is not a result. The one thing it DOES assert is that the
//! bed was actually exercised — a zero-claim or zero-call run fails rather
//! than reporting a clean sweep of nothing (§18.1, four verdicts not two).
//!
//! # Running it
//!
//! ```text
//! research/deep-research/arms/bed-binder/extract.py <run-dir>   # regenerate the bed
//! cargo test -p sovereign-core --test binder_replay -- --ignored --nocapture
//! ```
//!
//! with the daemon up. `BINDER_BED` overrides the bed path, `BINDER_ARMS`
//! selects arms (`per-span`, `batched`, or both — default both), and
//! `BINDER_CLAIMS` caps how many claims to replay so a tuning cycle can run
//! two claims instead of six. Writes `binder-replay.json` beside the bed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use oicp_client::RemoteApiProvider;
use sovereign_core::deep_research::audit::{assess_claim, run_tau, AuditChunk};
use sovereign_core::deep_research::containment::ContainmentConfig;
use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::traits::InferenceProvider;

const ENDPOINT: &str = "http://127.0.0.1:9741/v1";
/// The ALIAS, not a stem. `adversarial_read` pins the literal
/// `Qwen3.6-35B-A3B-MTP-UD-Q6_K`, and that constant is now stale — this host's
/// `[models] primary` is `Qwen3.8-27B-UD-Q6_K_XL`, so the pinned stem resolves
/// to nothing and every judge call fails fast (measured: a whole bed returning
/// could-not-judge in 3ms). Naming the alias makes the harness resolve exactly
/// what production's `Speed::Slow` resolves to on whatever host it runs on,
/// which is the property that was wanted; the resolution itself is in the
/// daemon's routing log, and the report records the alias it asked for.
const MODEL_ID: &str = "primary";
/// The audit bounds its own window to `AUDIT_EVIDENCE_TOKENS` (24k) and the
/// span calls are ~900 chars, so 32k covers both prompts with headroom. The
/// daemon clamps to the resident model's real window regardless.
const PROVIDER_CTX: u32 = 32_768;

#[derive(Debug, Deserialize)]
struct Bed {
    source_run: String,
    chunks: Vec<BedChunk>,
    claims: Vec<BedClaim>,
}

#[derive(Debug, Deserialize)]
struct BedChunk {
    id: String,
    source_url: String,
    custody_known: bool,
    content: String,
}

#[derive(Debug, Deserialize)]
struct BedClaim {
    text: String,
    recorded_verdict: Option<String>,
    recorded_origins: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
struct ArmRow {
    arm: String,
    ms: u128,
    verdict: String,
    action: String,
    bound: Vec<String>,
    origins: usize,
    /// The audit's own reason string. Recorded because a could-not-judge with
    /// no reason is unreadable: the first run of this harness reported a clean
    /// sweep of could-not-judge that was actually the daemon refusing every
    /// call, and the reason field is what tells those two apart (§18.1 —
    /// never-ran is its own verdict, not a quiet pass).
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct ClaimRow {
    claim: String,
    recorded_verdict: Option<String>,
    recorded_origins: Option<usize>,
    arms: Vec<ArmRow>,
    /// Chunks the per-span loop bound that the batched arm did not — support
    /// the triage LOST. This is the flip condition's number.
    per_span_only: Vec<String>,
    /// Chunks the batched arm bound that the per-span loop did not. Should be
    /// empty by construction; a non-empty set means the arms are not judging
    /// the same thing and is reported, never smoothed.
    batched_only: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Report {
    bed: String,
    source_run: String,
    model_id: String,
    tau: f64,
    chunks: usize,
    claims: Vec<ClaimRow>,
}

fn bed_path() -> PathBuf {
    if let Ok(p) = std::env::var("BINDER_BED") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../research/deep-research/arms/bed-binder/bed.json")
}

/// One arm, for one claim. The env var is set immediately before the call and
/// read inside `assess_claim` — this binary is single-threaded across arms by
/// construction (the loop below is sequential and awaits each arm), so there
/// is no second reader to race. Named here because a process-global mutated
/// mid-run is exactly the kind of thing that silently reads the wrong way.
async fn run_arm(
    arm: &str,
    batch: bool,
    early: bool,
    budget: usize,
    spans: &sovereign_core::deep_research::audit::SpanCache,
    provider: &Arc<dyn InferenceProvider>,
    claim: &str,
    chunks: &[AuditChunk],
    containment: &ContainmentConfig,
    tau: f64,
) -> ArmRow {
    std::env::set_var("SOVEREIGN_DR_AUDIT_BATCH_LOCATE", if batch { "1" } else { "0" });
    std::env::set_var(
        "SOVEREIGN_DR_AUDIT_LOCATE_EARLY_EXIT",
        if early { "1" } else { "0" },
    );
    std::env::set_var("SOVEREIGN_DR_AUDIT_LOCATE_BUDGET", budget.to_string());
    // The span cache is the ARM's, shared across every claim in it — because
    // that is what production does: `DeepResearchLoop::audit_pass` builds ONE
    // `SpanCache` per pass and hands it to every `assess_claim`. Giving each
    // claim a fresh one (as this harness first did) makes every claim pay the
    // whole embedding sweep and reports ~62s of `locate_ms` per claim where a
    // real pass pays it once. Each arm still gets its OWN cache, so the arms
    // pay identical setup and stay comparable.
    let t0 = Instant::now();
    let audit = assess_claim(
        provider,
        claim,
        chunks,
        containment,
        ShardingPrivacy::LocalOnly,
        tau,
        &spans,
    )
    .await;
    let ms = t0.elapsed().as_millis();
    let origins = audit
        .corroboration
        .as_ref()
        .map(|c| c.origins.len())
        .unwrap_or(0);
    ArmRow {
        arm: arm.to_string(),
        ms,
        verdict: format!("{:?}", audit.verdict),
        action: format!("{:?}", audit.action),
        bound: audit.supporting_chunk_ids.clone(),
        origins,
        reason: audit.reason.clone(),
    }
}

#[tokio::test]
#[ignore = "live daemon + minutes of judge calls; run explicitly"]
async fn binder_replay() {
    // The production stage timings (`t5 binder`, `audit: whole-window judge`,
    // `audit: containment witness`) are emitted on the custom `deep_research`
    // target, which is DARK unless a filter names it — so the default here
    // names it rather than leaving the harness able to report only a total.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // `grounding_gate` is in the DEFAULT, not just available via
                // RUST_LOG: a judge call that ERRORS logs its reason on that
                // target and nowhere else, so a filter naming only
                // `deep_research` shows a could-not-judge with no cause —
                // which cost a full debug cycle the first time.
                .unwrap_or_else(|_| "deep_research=info,grounding_gate=warn".into()),
        )
        .with_test_writer()
        .try_init();
    let path = bed_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "bed {} unreadable ({e}) — regenerate it:\n  \
             research/deep-research/arms/bed-binder/extract.py <run-dir>",
            path.display()
        )
    });
    let bed: Bed = serde_json::from_str(&raw).expect("bed parses");

    let chunks: Vec<AuditChunk> = bed
        .chunks
        .iter()
        .map(|c| AuditChunk {
            id: c.id.clone(),
            content: c.content.clone(),
            custody_known: c.custody_known,
            source_url: c.source_url.clone(),
        })
        .collect();

    // (name, batched, early-exit). `per-span` is the pre-2026-08-26 loop and
    // is the reference every other arm's bound set is compared against.
    // One SpanCache per ARM, built here and reused across claims — see run_arm.
    // (name, batched, early-exit, per-claim calibrated-call budget).
    // `per-span` is the pre-2026-08-26 loop and is the reference every other
    // arm's bound set is compared against. `fast` is all three levers at once,
    // which is the shape a shippable audit would actually run.
    let k = std::env::var("BINDER_BUDGET")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(8);
    let arms: Vec<(&str, bool, bool, usize)> = match std::env::var("BINDER_ARMS").as_deref() {
        Ok("per-span") => vec![("per-span", false, false, 0)],
        Ok("batched") => vec![("batched", true, false, 0)],
        Ok("early") => vec![("early", true, true, 0)],
        Ok("fast") => vec![("fast", true, true, k)],
        Ok("all") => vec![
            ("per-span", false, false, 0),
            ("batched", true, false, 0),
            ("early", true, true, 0),
            ("fast", true, true, k),
        ],
        _ => vec![("per-span", false, false, 0), ("fast", true, true, k)],
    };
    let cap = std::env::var("BINDER_CLAIMS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(usize::MAX);

    let caches: Vec<sovereign_core::deep_research::audit::SpanCache> =
        arms.iter().map(|_| Default::default()).collect();
    let tau = run_tau();
    let containment = ContainmentConfig::default();
    let provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(ENDPOINT, None, MODEL_ID, PROVIDER_CTX));

    eprintln!(
        "binder replay — bed {} ({} chunks, {} loop-reaching claims, capped at {}), \
         arms {:?}, tau {tau}",
        path.display(),
        chunks.len(),
        bed.claims.len(),
        if cap == usize::MAX { bed.claims.len() } else { cap },
        arms.iter().map(|(n, ..)| *n).collect::<Vec<_>>(),
    );

    let mut rows: Vec<ClaimRow> = Vec::new();
    for (i, claim) in bed.claims.iter().take(cap).enumerate() {
        let mut arm_rows: Vec<ArmRow> = Vec::new();
        for ((name, batch, early, budget), spans) in arms.iter().zip(caches.iter()) {
            let r = run_arm(
                name, *batch, *early, *budget, spans, &provider, &claim.text, &chunks,
                &containment, tau,
            )
            .await;
            eprintln!(
                "  claim {}/{} [{:>8}] {:>7}ms  {:<14} origins={} bound={:?}{}",
                i + 1,
                bed.claims.len().min(cap),
                r.arm,
                r.ms,
                r.verdict,
                r.origins,
                r.bound,
                r.reason.as_deref().map(|s| format!("  — {s}")).unwrap_or_default(),
            );
            arm_rows.push(r);
        }
        let pick = |a: &str| -> Vec<String> {
            arm_rows
                .iter()
                .find(|r| r.arm == a)
                .map(|r| r.bound.clone())
                .unwrap_or_default()
        };
        // Every non-reference arm is diffed against `per-span`, the loop this
        // work replaces. Union so that adding an arm cannot quietly narrow the
        // comparison: a chunk lost by ANY candidate arm shows up here.
        let ps = pick("per-span");
        let others: Vec<String> = arm_rows
            .iter()
            .filter(|r| r.arm != "per-span")
            .flat_map(|r| r.bound.clone())
            .collect();
        let lost: Vec<String> = ps
            .iter()
            .filter(|c| {
                arm_rows
                    .iter()
                    .any(|r| r.arm != "per-span" && !r.bound.contains(c))
            })
            .cloned()
            .collect();
        rows.push(ClaimRow {
            claim: claim.text.chars().take(160).collect(),
            recorded_verdict: claim.recorded_verdict.clone(),
            recorded_origins: claim.recorded_origins,
            per_span_only: lost,
            batched_only: others.iter().filter(|c| !ps.contains(c)).cloned().collect(),
            arms: arm_rows,
        });
    }

    // §18.1: a run that exercised nothing is not a clean run. Four verdicts,
    // not two — "never ran" must be distinguishable from "passed".
    assert!(
        !rows.is_empty(),
        "the bed carried no loop-reaching claims — nothing was measured. \
         Regenerate it from a run whose gap lists carry corroboration records."
    );
    // A bed where the judge never ran is NEVER-RAN, not a clean sweep of
    // could-not-judge — and the two are indistinguishable in the verdict
    // column alone, which is how the first run of this harness read as
    // "6 claims, all could-not-judge, 3ms" and looked like data (§18.1).
    let never_ran: Vec<&ArmRow> = rows
        .iter()
        .flat_map(|r| &r.arms)
        .filter(|a| {
            a.reason
                .as_deref()
                .is_some_and(|s| s.contains("judge failed to run"))
        })
        .collect();
    assert!(
        never_ran.is_empty(),
        "{} of {} arm runs never reached the judge — the daemon did not serve them. \
         This is NEVER-RAN, not a measurement. Check the endpoint ({ENDPOINT}) and that \
         `{MODEL_ID}` resolves (`curl {ENDPOINT}/models`). First reason: {:?}",
        never_ran.len(),
        rows.iter().map(|r| r.arms.len()).sum::<usize>(),
        never_ran[0].reason,
    );

    let out = path.with_file_name("binder-replay.json");
    std::fs::write(
        &out,
        serde_json::to_string_pretty(&Report {
            bed: path.display().to_string(),
            source_run: bed.source_run.clone(),
            model_id: MODEL_ID.to_string(),
            tau,
            chunks: chunks.len(),
            claims: rows,
        })
        .expect("report serialises"),
    )
    .expect("report writes");
    eprintln!("\nbinder replay -> {}", out.display());
}
