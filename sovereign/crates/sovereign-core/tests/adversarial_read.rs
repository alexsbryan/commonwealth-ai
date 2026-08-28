// SPDX-License-Identifier: AGPL-3.0-or-later
//! The gate-change adversarial read (order `deep-research-t1a`,
//! gate-redesign.md §5, pre-registration record
//! `research/deep-research/adversarial/pre-registration.md`).
//!
//! Runs the FROZEN adversarial instruments against the production gate
//! twice:
//!
//! - **baseline** — judge alone: `claim_violation_joint` per claim;
//!   verdict passed iff violation probability < `grounding_gate_threshold()`
//!   (the gate's own tau), exactly the audit's step 3.
//! - **changed** — the composed gate: `deep_research::audit::assess_claim`
//!   (single-string judge + C-class containment witness), exactly the
//!   loop's audit path composes it.
//!
//! Both strings ARE the production functions; nothing here re-implements
//! or substitutes them. The measurement's verdict classes are
//! passed / failed / could-not-judge / never-ran — a `None` from either
//! string is a recorded verdict class, never a default (§18.1/§18.3).
//! The driver measures and records; it asserts nothing (the READ is the
//! analysis, appended to pre-registration.md at execution, timestamped).
//!
//! The sub-bank's claims are audited verbatim; the longform negatives'
//! answers are split with the loop's own `split_claims` (one splitter,
//! two consumers — the same splitter the R3 audits and R9 verdicts use).
//! The synthetic windows pass as one chunk with known custody.
//!
//! `#[ignore]`d: invoke with
//! `cargo test -p sovereign-core --test adversarial_read -- --ignored --nocapture`
//! with the daemon up (model `Qwen3.6-35B-A3B-MTP-UD-Q6_K` loaded).
//! Writes `adversarial-report.json` beside the frozen instruments.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use oicp_client::RemoteApiProvider;
use sovereign_core::deep_research::audit::{assess_claim, run_tau, split_claims, AuditChunk};
use sovereign_core::deep_research::containment::ContainmentConfig;
use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::runtime::claim_violation_joint;
use sovereign_core::traits::InferenceProvider;

const ENDPOINT: &str = "http://127.0.0.1:9741/v1";
/// The primary stem — the same model class the production judge resolves
/// to on this host (the fr6 driver used it too).
const MODEL_ID: &str = "Qwen3.6-35B-A3B-MTP-UD-Q6_K";
const PROVIDER_CTX: u32 = 8192;

// ---------------------------------------------------------------------------
// Frozen instrument schemas (mirror the minted .jsonl files)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SubBankItem {
    id: String,
    half: String,
    claim: String,
    window: String,
}

#[derive(Debug, Deserialize)]
struct LongformItem {
    id: String,
    answer: String,
    window: String,
}

// ---------------------------------------------------------------------------
// Report schema
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ClaimRow {
    text: String,
    baseline_violation_prob: Option<f64>,
    baseline_verdict: String,
    changed_verdict: String,
    changed_action: String,
    witness_ran: bool,
    witness_all_absent: bool,
    specifics: Vec<String>,
    /// GAP-2 — the corroboration floor's record, when the claim reached
    /// the floor (the driver measures AND records; the record is the
    /// changed gate's key output, verdict-visible).
    corroboration: Option<sovereign_core::deep_research::icd::CorroborationRecord>,
}

#[derive(Debug, Serialize)]
struct ItemRow {
    id: String,
    half: String,
    /// Claims passed by the judge alone (the bias residual).
    baseline_supported: usize,
    /// Claims downgraded to could-not-judge by the changed gate.
    changed_could_not_judge: usize,
    /// Any claim passed → could-not-judge (the predicted downgrade).
    downgraded: bool,
    /// Any claim could-not-judge/failed → passed. The witness only
    /// downgrades; an upgrade here is the failure signature.
    upgraded: bool,
    claims: Vec<ClaimRow>,
}

#[derive(Debug, Serialize)]
struct Report {
    measurement: String,
    ran_at: String,
    model: String,
    endpoint: String,
    tau: f64,
    items_total: usize,
    claims_total: usize,
    baseline_supported_total: usize,
    changed_could_not_judge_total: usize,
    downgraded_items: usize,
    upgraded_items: usize,
    items: Vec<ItemRow>,
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "adversarial read (order deep-research-t1a): ~100 judge calls against the live local daemon; see research/deep-research/adversarial/pre-registration.md"]
async fn adversarial_read() {
    let bank = adversarial_dir();
    let sub_raw = tokio::fs::read_to_string(bank.join("sub-bank.jsonl"))
        .await
        .expect("sub-bank.jsonl must exist (minted fd1fd378)");
    let long_raw = tokio::fs::read_to_string(bank.join("longform-negative.jsonl"))
        .await
        .expect("longform-negative.jsonl must exist (minted fd1fd378)");

    let sub: Vec<SubBankItem> = sub_raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("sub-bank row must parse"))
        .collect();
    let long: Vec<LongformItem> = long_raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("longform row must parse"))
        .collect();

    let tau = run_tau();
    let posture = ShardingPrivacy::LocalOnly;
    let provider: Arc<dyn InferenceProvider> = Arc::new(RemoteApiProvider::new(
        ENDPOINT,
        None,
        MODEL_ID,
        PROVIDER_CTX,
    ));
    let containment = ContainmentConfig::default();

    let mut items: Vec<ItemRow> = Vec::new();
    for item in &sub {
        let row = audit_one(
            &provider,
            &item.id,
            &item.half,
            &[item.claim.clone()],
            &item.window,
            tau,
            posture,
            &containment,
        )
        .await;
        items.push(row);
    }
    for item in &long {
        let claims = split_claims(&item.answer);
        let row = audit_one(
            &provider,
            &item.id,
            "longform-negative",
            &claims,
            &item.window,
            tau,
            posture,
            &containment,
        )
        .await;
        items.push(row);
    }

    let claims_total: usize = items.iter().map(|i| i.claims.len()).sum();
    let baseline_supported_total: usize = items.iter().map(|i| i.baseline_supported).sum();
    let changed_cnjt_total: usize = items.iter().map(|i| i.changed_could_not_judge).sum();
    let downgraded_items = items.iter().filter(|i| i.downgraded).count();
    let upgraded_items = items.iter().filter(|i| i.upgraded).count();

    let report = Report {
        measurement: "adversarial-read".to_string(),
        ran_at: now_utc(),
        model: MODEL_ID.to_string(),
        endpoint: ENDPOINT.to_string(),
        tau,
        items_total: items.len(),
        claims_total,
        baseline_supported_total,
        changed_could_not_judge_total: changed_cnjt_total,
        downgraded_items,
        upgraded_items,
        items,
    };

    let out_path = bank.join("adversarial-report.json");
    let json = serde_json::to_string_pretty(&report).expect("report serializes");
    tokio::fs::write(&out_path, json)
        .await
        .expect("report writes");

    println!();
    println!("adversarial read — {out_path:?}");
    println!("  tau: {tau}");
    println!(
        "  items: {}  claims: {}",
        report.items_total, report.claims_total
    );
    println!(
        "  baseline judge-alone supported: {}   changed could-not-judge: {}",
        report.baseline_supported_total, report.changed_could_not_judge_total
    );
    println!(
        "  downgraded items: {}   upgraded items: {}",
        report.downgraded_items, report.upgraded_items
    );
    for i in &report.items {
        let summary = i
            .claims
            .iter()
            .map(|c| {
                format!(
                    "{}->{}",
                    short(&c.baseline_verdict),
                    short(&c.changed_verdict)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        println!("  {} ({:>16}): {}", i.id, i.half, summary);
    }
}

async fn audit_one(
    provider: &Arc<dyn InferenceProvider>,
    id: &str,
    half: &str,
    claims: &[String],
    window: &str,
    tau: f64,
    posture: ShardingPrivacy,
    containment: &ContainmentConfig,
) -> ItemRow {
    let texts = vec![window.to_string()];
    let chunks = vec![AuditChunk {
        id: "w1".to_string(),
        content: window.to_string(),
        custody_known: true,
        source_url: "window:synthetic".to_string(),
    }];
    let mut rows = Vec::new();
    let mut baseline_supported = 0usize;
    let mut changed_cnjt = 0usize;
    let mut downgraded = false;
    let mut upgraded = false;

    for claim in claims {
        // Baseline — judge alone (the audit's step 3).
        let prob = claim_violation_joint(provider, claim, &texts, texts.len(), 0, posture).await;
        let baseline_verdict = match prob {
            None => "never-ran",
            Some(p) if p >= tau => "failed",
            Some(_) => {
                baseline_supported += 1;
                "passed"
            }
        };

        // Changed — the composed gate (judge + containment witness).
        let audit = assess_claim(
            provider,
            claim,
            &chunks,
            containment,
            posture,
            tau,
            &sovereign_core::deep_research::audit::SpanCache::default(),
        )
        .await;
        if audit.verdict == sovereign_core::deep_research::icd::Verdict::CouldNotJudge {
            changed_cnjt += 1;
        }
        if baseline_verdict == "passed"
            && audit.verdict == sovereign_core::deep_research::icd::Verdict::CouldNotJudge
        {
            downgraded = true;
        }
        if baseline_verdict != "passed"
            && audit.verdict == sovereign_core::deep_research::icd::Verdict::Passed
        {
            upgraded = true;
        }

        rows.push(ClaimRow {
            text: claim.clone(),
            baseline_violation_prob: prob,
            baseline_verdict: baseline_verdict.to_string(),
            changed_verdict: audit.verdict.as_str().to_string(),
            changed_action: audit.action.as_str().to_string(),
            witness_ran: audit.witness.ran,
            witness_all_absent: audit.witness.all_absent,
            specifics: audit.witness.specifics.clone(),
            corroboration: audit.corroboration.clone(),
        });
    }

    ItemRow {
        id: id.to_string(),
        half: half.to_string(),
        baseline_supported,
        changed_could_not_judge: changed_cnjt,
        downgraded,
        upgraded,
        claims: rows,
    }
}

fn short(v: &str) -> &str {
    match v {
        "passed" => "P",
        "failed" => "F",
        "could-not-judge" => "C",
        "never-ran" => "N",
        other => other,
    }
}

fn adversarial_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .join("../../../research/deep-research/adversarial")
        .canonicalize()
        .expect("research/deep-research/adversarial must exist (minted fd1fd378)")
}

fn now_utc() -> String {
    // No chrono dep in the test target — epoch + UTC render via time is
    // overkill; a readable local timestamp is enough for the record.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix-{secs}")
}
