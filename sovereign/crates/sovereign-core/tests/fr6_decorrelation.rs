// SPDX-License-Identifier: AGPL-3.0-or-later
//! FR-6 decorrelation measurement driver (order `deep-research-t0b`, red
//! R-7). Measures the gate's TWO judge strings against the labeled bank at
//! `research/deep-research/bank/labeled/claims.jsonl` (100 claims, 60
//! supported / 40 unsupported):
//!
//! - string A: `claim_violation_joint` — one call per claim; the claim is
//!   judged unsupported iff violation probability >=
//!   `grounding_gate_threshold()` (the gate's own tau).
//! - string B: `scan_unsupported_specifics` — one call per item over the
//!   answer; flagged specifics are containment-matched to claims (claims
//!   are verbatim answer sentences, so the match is deterministic).
//!
//! Both strings ARE the production gate functions — directives 13efc5dc +
//! e39f87b2 re-exported them unchanged; nothing here re-implements or
//! substitutes them. The measurement's verdict classes are
//! supported / unsupported / never-ran (a `None` from either string is a
//! recorded verdict class, never a default — §18.1/§18.3).
//!
//! Runs against the live local daemon (the same stem the hand-run chat
//! used). `#[ignore]`d: invoke with
//! `cargo test -p sovereign-core --test fr6_decorrelation -- --ignored --nocapture`
//! with the daemon up. Writes `fr6-report.json` beside the labeled set.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use oicp_client::RemoteApiProvider;
use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::runtime::{
    claim_violation_joint, grounding_gate_threshold, scan_unsupported_specifics,
};
use sovereign_core::traits::InferenceProvider;

const ENDPOINT: &str = "http://127.0.0.1:9741/v1";
/// The primary stem — the same model class the production gate's judge
/// calls resolve to on this host (the hand-run chat surface used it too).
const MODEL_ID: &str = "Qwen3.6-35B-A3B-MTP-UD-Q6_K";
const PROVIDER_CTX: u32 = 8192;
/// Budget for the specifics scan. Production floors it at 3; each labeled
/// item carries 5 claims, so 6 covers every plausible specific in one scan.
const SPECIFICS_BUDGET: usize = 6;
/// Production parity: the per-claim judge caps the chunk window at 12
/// (judge.rs `cap`). Every labeled item carries 4 evidence chunks — under
/// the cap, so the driver passes the full window unchanged.
const CHUNK_CAP: usize = 12;

// ---------------------------------------------------------------------------
// Bank schema (mirrors research/deep-research/bank/labeled/README.md)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Item {
    id: String,
    question: String,
    answer: String,
    evidence: Vec<String>,
    claims: Vec<Claim>,
}

#[derive(Debug, Deserialize)]
struct Claim {
    text: String,
    label: String,
    #[serde(default)]
    kind: Option<String>,
}

// ---------------------------------------------------------------------------
// Report schema
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct Report {
    measurement: String,
    ran_at: String,
    model: String,
    endpoint: String,
    tau: f64,
    counts: Counts,
    items: Vec<ItemReport>,
}

#[derive(Debug, Serialize, Default)]
struct Counts {
    claims_total: usize,
    labeled_supported: usize,
    labeled_unsupported: usize,
    a_ran: usize,
    b_ran: usize,
    never_ran: NeverRan,
    agreement: Fraction,
    joint_miss: usize,
    single_a_miss: usize,
    single_b_miss: usize,
    a_false_alarm: usize,
    b_false_alarm: usize,
}

#[derive(Debug, Serialize, Default)]
struct NeverRan {
    a: usize,
    b: usize,
}

#[derive(Debug, Serialize, Default)]
struct Fraction {
    n: usize,
    fraction: f64,
}

#[derive(Debug, Serialize)]
struct ItemReport {
    id: String,
    /// The scan's raw flagged specifics for the item's answer — recorded so
    /// the report is self-validating (a containment-mapped verdict can be
    /// checked against what the string actually returned).
    b_flagged: Vec<String>,
    claims: Vec<ClaimReport>,
}

#[derive(Debug, Serialize)]
struct ClaimReport {
    idx: usize,
    label: String,
    kind: Option<String>,
    a_vp: Option<f64>,
    a_verdict: Option<bool>, // true = unsupported
    b_verdict: Option<bool>, // true = unsupported
    agreement: Option<bool>,
    joint_miss: bool,
    single_a_miss: bool,
    single_b_miss: bool,
    a_false_alarm: bool,
    b_false_alarm: bool,
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "FR-6 decorrelation measurement (order deep-research-t0b): runs 220 judge calls against the live local daemon; see research/deep-research/bank/labeled/README.md"]
async fn fr6_decorrelation_measurement() {
    // Trait-object coercion at construction: the strings take
    // `&Arc<dyn InferenceProvider>`, and this provider is the production
    // surface they run against (RemoteApiProvider implements the trait).
    let provider: Arc<dyn InferenceProvider> = Arc::new(RemoteApiProvider::new(
        ENDPOINT,
        None,
        MODEL_ID,
        PROVIDER_CTX,
    ));
    preflight(&provider).await;

    let bank_path = bank_path();
    let raw = tokio::fs::read_to_string(&bank_path)
        .await
        .unwrap_or_else(|e| panic!("cannot read bank {}: {e}", bank_path.display()));
    let items: Vec<Item> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("bank row must parse"))
        .collect();

    let tau = grounding_gate_threshold();
    let mut report = Report {
        measurement: "fr6-decorrelation".to_string(),
        ran_at: now_iso(),
        model: MODEL_ID.to_string(),
        endpoint: ENDPOINT.to_string(),
        tau,
        counts: Counts::default(),
        items: Vec::new(),
    };

    for item in &items {
        // String B — one scan per item, over the whole answer.
        let flagged = scan_unsupported_specifics(
            &provider,
            &item.question,
            &item.answer,
            &item.evidence,
            SPECIFICS_BUDGET,
            ShardingPrivacy::LocalOnly,
        )
        .await;
        report.counts.b_ran += usize::from(flagged.is_some());

        let mut item_report = ItemReport {
            id: item.id.clone(),
            b_flagged: flagged.clone().unwrap_or_default(),
            claims: Vec::new(),
        };

        for (idx, claim) in item.claims.iter().enumerate() {
            // String A — one per-claim judge call (production path shape:
            // chunks capped at 12, n_stable = 0 — no shared-prefix history
            // in the labeled set).
            let window: Vec<String> = item
                .evidence
                .iter()
                .take(CHUNK_CAP)
                .cloned()
                .collect();
            let vp = claim_violation_joint(
                &provider,
                &claim.text,
                &window,
                window.len(),
                0,
                ShardingPrivacy::LocalOnly,
            )
            .await;
            report.counts.a_ran += usize::from(vp.is_some());

            let a_verdict = vp.map(|p| p >= tau);
            let b_verdict = flagged.as_ref().map(|specifics| {
                specifics
                    .iter()
                    .any(|s| spec_matches(s, &claim.text))
            });

            let labeled_unsupported = claim.label == "unsupported";
            let agreement = match (a_verdict, b_verdict) {
                (Some(a), Some(b)) => Some(a == b),
                _ => None,
            };
            if let Some(agree) = agreement {
                report.counts.agreement.n += 1;
                report.counts.agreement.fraction += usize::from(agree) as f64;
            }
            let joint_miss = labeled_unsupported
                && matches!(a_verdict, Some(false))
                && matches!(b_verdict, Some(false));
            let single_a_miss = labeled_unsupported
                && matches!(a_verdict, Some(false))
                && matches!(b_verdict, Some(true));
            let single_b_miss = labeled_unsupported
                && matches!(a_verdict, Some(true))
                && matches!(b_verdict, Some(false));
            let a_false_alarm = !labeled_unsupported && matches!(a_verdict, Some(true));
            let b_false_alarm = !labeled_unsupported && matches!(b_verdict, Some(true));

            report.counts.claims_total += 1;
            if labeled_unsupported {
                report.counts.labeled_unsupported += 1;
            } else {
                report.counts.labeled_supported += 1;
            }
            if joint_miss {
                report.counts.joint_miss += 1;
            }
            if single_a_miss {
                report.counts.single_a_miss += 1;
            }
            if single_b_miss {
                report.counts.single_b_miss += 1;
            }
            if a_false_alarm {
                report.counts.a_false_alarm += 1;
            }
            if b_false_alarm {
                report.counts.b_false_alarm += 1;
            }
            if vp.is_none() {
                report.counts.never_ran.a += 1;
            }
            if flagged.is_none() {
                report.counts.never_ran.b += 1;
            }

            item_report.claims.push(ClaimReport {
                idx,
                label: claim.label.clone(),
                kind: claim.kind.clone(),
                a_vp: vp,
                a_verdict,
                b_verdict,
                agreement,
                joint_miss,
                single_a_miss,
                single_b_miss,
                a_false_alarm,
                b_false_alarm,
            });
        }
        report.items.push(item_report);
    }

    report.counts.agreement.fraction = if report.counts.agreement.n > 0 {
        report.counts.agreement.fraction / report.counts.agreement.n as f64
    } else {
        0.0
    };

    let json = serde_json::to_string_pretty(&report).expect("report must serialize");
    let out_path = bank_path
        .parent()
        .expect("bank file has a parent")
        .join("fr6-report.json");
    tokio::fs::write(&out_path, &json)
        .await
        .unwrap_or_else(|e| panic!("cannot write report {}: {e}", out_path.display()));

    // Summary — the numbers the FR-6 report cites.
    println!("FR-6 decorrelation — summary");
    println!("  model: {MODEL_ID} · tau: {tau} · ran_at: {}", report.ran_at);
    println!(
        "  claims: {} total ({} supported / {} unsupported) | A ran {} · B ran {}",
        report.counts.claims_total,
        report.counts.labeled_supported,
        report.counts.labeled_unsupported,
        report.counts.a_ran,
        report.counts.b_ran,
    );
    let agreed =
        (report.counts.agreement.fraction * report.counts.agreement.n as f64).round() as usize;
    println!(
        "  agreement: {:.1}% ({agreed}/{} both-ran claims)",
        report.counts.agreement.fraction * 100.0,
        report.counts.agreement.n,
    );
    println!(
        "  joint-miss (unsupported missed by BOTH): {} | single-A-miss: {} | single-B-miss: {}",
        report.counts.joint_miss, report.counts.single_a_miss, report.counts.single_b_miss
    );
    println!(
        "  false alarms on supported: A {} · B {} | never-ran: A {} · B {}",
        report.counts.a_false_alarm,
        report.counts.b_false_alarm,
        report.counts.never_ran.a,
        report.counts.never_ran.b,
    );
    println!("  report: {}", out_path.display());
}

/// The scan's flagged fragments are phrases carved out of the answer;
/// claims are verbatim answer sentences, so containment is deterministic.
/// Both directions, with a length floor to skip trivial fragments.
fn spec_matches(specific: &str, claim: &str) -> bool {
    let (s, c) = (specific.trim(), claim.trim());
    if s.len() < 4 {
        return false;
    }
    let (sn, cn) = (collapse_ws(s), collapse_ws(c));
    sn.len() >= 4 && (cn.contains(&sn) || sn.contains(&cn))
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bank_path() -> PathBuf {
    // sovereign/crates/sovereign-core -> repo root -> research/...
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .join("../../../research/deep-research/bank/labeled/claims.jsonl")
        .canonicalize()
        .expect("labeled bank must exist (committed with bank v0 mint)")
}

fn now_iso() -> String {
    // No chrono import juggling in the test: UTC timestamp via std is
    // fine for a measurement stamp.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("unix-{}", d.as_secs()))
        .unwrap_or_else(|_| "unknown".to_string())
}

async fn preflight(_provider: &Arc<dyn InferenceProvider>) {
    // A measurement without a live daemon is not a measurement — fail
    // loudly rather than record 200 never-rans. The OICP manifest fetch
    // is the provider's own health probe (hits /oicp/v1/capabilities).
    // A fresh concrete provider probes; the measurement itself uses the
    // trait object above.
    let probe = RemoteApiProvider::new(ENDPOINT, None, MODEL_ID, PROVIDER_CTX);
    if probe.fetch_oicp_manifest().await.is_none() {
        panic!(
            "FR-6 driver needs the live daemon at {ENDPOINT} (start it, then re-run); manifest probe returned None"
        );
    }
}
