// SPDX-License-Identifier: AGPL-3.0-or-later
//! `compose_replay` — the deep-research WRITER, replayed against a frozen
//! evidence window, one arm per section-evidence budget.
//!
//! # Why this exists
//!
//! Measured on the task-69 wide cell (2026-08-26), a ~96-minute flight splits:
//!
//! | phase                        | wall clock |
//! |------------------------------|------------|
//! | planning + acquisition       | ~10 min    |
//! | **writing (`compose_report`)** | **~12 min** |
//! | audit                        | ~71 min    |
//! | (then the RACE judge)        | ~9 min     |
//!
//! So tuning the writer through whole flights pays 8x the writer's own cost
//! per answer, and every arm re-buys an acquisition whose variance it did not
//! want. `tests/binder_replay.rs` already made this argument for the AUDIT
//! stage — "you do not need all 35 claims to know what to tune" — and this is
//! the same move for the writer.
//!
//! # What it replays, and what it does not fork
//!
//! It calls the PRODUCTION [`synthesize::compose_report`] through a live
//! [`ResearchPort`]. There is a Python prototype of this stage
//! (`research/deep-research/arms/lab/compose2.py`) and it is deliberately NOT
//! used: tuning a second implementation tunes something we do not ship, which
//! is the whole reason the port exists (§10.6).
//!
//! The inputs come from `compose-input.json`, written by the pipeline at the
//! compose boundary. The per-round `evidence-window-<n>.json` dumps CANNOT
//! serve: they are written inside the round loop, `ev-N` is per-round
//! positional (round 2 restarts at `ev-1`), and on the cell above compose saw
//! 61 chunks where the dumps summed to 57.
//!
//! # What it measures
//!
//! Per arm — a `(section_passages, per_source_cap)` pair — wall-clock, the
//! deliverable's word count, and the markdown itself, written out for the
//! SAME scorer the flights use (`arms/lab/score_one.py`). Quality is scored
//! OUTSIDE this harness, on purpose: one judge instrument, one sampling pin.
//!
//! # It asserts nothing about which arm wins
//!
//! It records. A single replay is not a bank and a single-run delta is not a
//! result (§18.5). The one thing it DOES assert is that every arm actually
//! composed something — an arm that returned an error is NEVER-RAN, not a
//! zero, and the run says so instead of scoring an empty file (§18.3).
//!
//! ```text
//! COMPOSE_INPUT=<run-dir>/compose-input.json \
//! COMPOSE_ARMS=8:3,28:5,44:6 \
//!   cargo test -p sovereign-core --test compose_replay -- --ignored --nocapture
//! ```
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use oicp_client::RemoteApiProvider;
use sovereign_core::deep_research::icd::ComposeInput;
use sovereign_core::deep_research::port::build_port;
use sovereign_core::deep_research::synthesize;
use sovereign_core::deep_research::SearchSource;
use sovereign_core::traits::InferenceProvider;

const ENDPOINT: &str = "http://127.0.0.1:9741/v1";
const MODEL_ID: &str = "primary";
const PROVIDER_CTX: u32 = 32_768;

#[derive(Debug, Serialize)]
struct ArmRow {
    arm: String,
    section_passages: usize,
    per_source_cap: usize,
    /// `None` when the arm refused or errored — NEVER 0, which would read as
    /// "composed an empty report" (§18.3).
    ms: Option<u128>,
    words: Option<usize>,
    chars: Option<usize>,
    out: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct Report {
    input: String,
    run_id: String,
    question: String,
    window_chunks: usize,
    sections: usize,
    notes: usize,
    /// The budget the ORIGINAL run composed at, so an arm can be read against
    /// its own baseline rather than an assumed default.
    baseline: (usize, usize),
    /// Set when `COMPOSE_SECTIONS` replaced the bed's pinned outline. An arm
    /// composed against a DIFFERENT outline is not comparable to one that was
    /// not, and the arm filename (`NxM`) cannot tell them apart — the same
    /// blind spot that let an architecture arm silently land on the evidence
    /// curve's 16:4. Recorded so the report can.
    sections_override: Option<String>,
    arms: Vec<ArmRow>,
}

fn input_path() -> PathBuf {
    if let Ok(p) = std::env::var("COMPOSE_INPUT") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../research/deep-research/arms/bed-compose/compose-input.json")
}

/// `"8:3,28:5,44"` → `[(8,3),(28,5),(44,cap_default)]`. A bare number sweeps
/// `want` and leaves the cap at the input's baseline, which is the common
/// case when looking for the volume knee.
fn parse_arms(spec: &str, default_cap: usize) -> Vec<(usize, usize)> {
    spec.split(',')
        .filter_map(|a| {
            let a = a.trim();
            if a.is_empty() {
                return None;
            }
            let (w, c) = match a.split_once(':') {
                Some((w, c)) => (w.trim().parse().ok()?, c.trim().parse().ok()?),
                None => (a.parse().ok()?, default_cap),
            };
            Some((w, c))
        })
        .collect()
}

#[tokio::test]
#[ignore = "live daemon + minutes of writer calls; run explicitly"]
async fn compose_replay() {
    // `section evidence budget decided` and the compose events ride the custom
    // `deep_research` target, which is DARK unless a filter names it — without
    // this the harness could report a total and nothing about WHY (§9.1).
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "deep_research=info,grounding_gate=warn".into()),
        )
        .with_test_writer()
        .try_init();

    let path = input_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "compose input {} unreadable ({e}) — every flight writes one:\n  \
             COMPOSE_INPUT=<run-dir>/compose-input.json",
            path.display()
        )
    });
    let mut input: ComposeInput = serde_json::from_str(&raw).expect("compose-input.json parses");

    // The pin is the harness's, not the arm's: an unpinned writer drafts at
    // temperature 0.4 (ff625601) and a 2-3 point knob cannot be read through
    // that. Set once, for every arm, so the arms differ ONLY in the budget.
    std::env::set_var("SOVEREIGN_DR_PIN_SAMPLING", "1");
    std::env::set_var("SOVEREIGN_DR_COMPOSED_REPORT", "1");

    let arms = parse_arms(
        &std::env::var("COMPOSE_ARMS").unwrap_or_else(|_| "8:3,28:5".to_string()),
        input.per_source_cap,
    );
    assert!(!arms.is_empty(), "COMPOSE_ARMS parsed to nothing");

    let out_dir = PathBuf::from(
        std::env::var("COMPOSE_OUT")
            .unwrap_or_else(|_| "research/deep-research/arms/runs-compose".to_string()),
    )
    .join(&input.run_id);
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    // OUTLINE AS A SWEEPABLE VARIABLE. `compose_report` takes its sections as
    // an argument and never calls `plan_outline`, so the bed replays whatever
    // outline the captured flight planned — which makes report STRUCTURE the
    // one thing this bed could not test. It is now the thing most worth
    // testing: at 16x4 the per-criterion gaps put the entire remaining
    // deficit in readability (-1.21 vs the reference, against -0.17/+0.12/+0.20
    // elsewhere), and the judge's own words are structural — "a somewhat
    // fragmented structure with many short sections that jump between topics".
    // The bed's outline is 20 sections; the reference reads as nine.
    //
    // A REFUSAL IS LOUD, never a silent fall back to the pinned outline: an
    // arm that believes it swept the outline and did not is a well-formed
    // report of a different experiment (18.3).
    let sections_override = std::env::var("COMPOSE_SECTIONS").ok();
    if let Some(sp) = sections_override.as_deref() {
        let raw = std::fs::read_to_string(sp)
            .unwrap_or_else(|e| panic!("COMPOSE_SECTIONS {sp} unreadable: {e}"));
        let planned: Vec<String> = serde_json::from_str(&raw).unwrap_or_else(|e| {
            panic!("COMPOSE_SECTIONS {sp} is not a JSON array of strings: {e}")
        });
        assert!(
            !planned.is_empty(),
            "COMPOSE_SECTIONS {sp} parsed to an EMPTY outline — composing against \
             no sections would still produce a plausible report (18.3)"
        );
        eprintln!(
            "  outline OVERRIDDEN: {} sections from {sp} (bed pinned {})",
            planned.len(),
            input.sections.len()
        );
        input.sections = planned;
    }

    let provider: Arc<dyn InferenceProvider> = Arc::new(RemoteApiProvider::new(
        ENDPOINT,
        None,
        MODEL_ID,
        PROVIDER_CTX,
    ));
    let (port, _backend) = build_port("auto", None, SearchSource::Corpus, &[], provider, None)
        .await
        .expect("build live research port");

    eprintln!(
        "compose replay — input {} (run {}, {} chunks, {} sections, {} notes), \
         baseline {}x{}, arms {:?}",
        path.display(),
        input.run_id,
        input.window.chunks.len(),
        input.sections.len(),
        input.notes.len(),
        input.section_passages,
        input.per_source_cap,
        arms,
    );

    let mut rows = Vec::new();
    for (want, cap) in &arms {
        let name = format!("{want}x{cap}");
        // Read inside `section_evidence_budget()`; this binary runs its arms
        // SEQUENTIALLY by construction, so there is no second reader to race.
        std::env::set_var("SOVEREIGN_DR_SECTION_PASSAGES", want.to_string());
        std::env::set_var("SOVEREIGN_DR_SECTION_SOURCE_CAP", cap.to_string());
        let t0 = Instant::now();
        let composed = synthesize::compose_report(
            &*port,
            &input.question,
            &input.window,
            &input.sections,
            &input.notes,
        )
        .await;
        let ms = t0.elapsed().as_millis();
        match composed {
            Ok(md) => {
                let f = out_dir.join(format!("arm-{name}.md"));
                std::fs::write(&f, &md).expect("write arm markdown");

                // ALSO WRITE WHAT A READER WOULD ACTUALLY SEE (18.4 — validate
                // the instrument before the result). `compose_report` returns
                // the writer's draft, which still carries its internal
                // `[Source: ev-N]` handles; production numbers them before the
                // deliverable is rendered (mod.rs:2586). Measured 2026-08-27:
                // `arm-16x4.md` carries 128 raw ev-N handles and ZERO numbered
                // citations, while the official t69 flight reports carry 33-143
                // numbered and only stray ev-N. So this bed has been scoring a
                // pre-render draft against a rendered reference, and the judge
                // charged us for exactly that — "the density of citations
                // [Source: ev-xx] can be visually cluttering", Formatting 8.5
                // vs 9.5.
                //
                // BOTH are written, and the draft keeps its filename, because
                // the five-point evidence curve was measured on the draft and
                // silently re-pointing the scorer would invalidate it without
                // saying so. Scoring both turns "how much of the readability
                // gap is a render artifact" from an argument into a number.
                //
                // This is the citation-numbering step ONLY, not production's
                // full `render::annotate_composed` — that needs audit claims
                // this bed does not produce, and inventing them would make the
                // rendered artifact a third thing that ships nowhere.
                let (numbered, _sources) = synthesize::number_citations(&md, &input.window);
                let rf = out_dir.join(format!("arm-{name}.rendered.md"));
                std::fs::write(&rf, &numbered).expect("write rendered arm markdown");
                eprintln!(
                    "  arm {name}: {:.1} min, {} words -> {}",
                    ms as f64 / 60_000.0,
                    md.split_whitespace().count(),
                    f.display()
                );
                rows.push(ArmRow {
                    arm: name,
                    section_passages: *want,
                    per_source_cap: *cap,
                    ms: Some(ms),
                    words: Some(md.split_whitespace().count()),
                    chars: Some(md.len()),
                    out: Some(f.to_string_lossy().into_owned()),
                    error: None,
                });
            }
            Err(e) => {
                eprintln!("  arm {name}: REFUSED after {ms} ms — {e} (recorded, not scored)");
                rows.push(ArmRow {
                    arm: name,
                    section_passages: *want,
                    per_source_cap: *cap,
                    ms: None,
                    words: None,
                    chars: None,
                    out: None,
                    error: Some(e),
                });
            }
        }
    }

    let report = Report {
        input: path.to_string_lossy().into_owned(),
        run_id: input.run_id.clone(),
        question: input.question.clone(),
        window_chunks: input.window.chunks.len(),
        sections: input.sections.len(),
        notes: input.notes.len(),
        baseline: (input.section_passages, input.per_source_cap),
        sections_override: sections_override.clone(),
        arms: rows,
    };
    let rp = out_dir.join("compose-replay.json");
    std::fs::write(&rp, serde_json::to_string_pretty(&report).unwrap()).expect("write report");
    eprintln!("compose replay report -> {}", rp.display());

    // The ONLY assertion: an arm that composed nothing is never-ran, not a
    // zero — a whole sweep of refusals must fail the run rather than leave a
    // directory of empty markdown for the scorer to score as bad writing.
    assert!(
        report.arms.iter().any(|a| a.out.is_some()),
        "every arm refused — this measured NOTHING. Check the endpoint \
         ({ENDPOINT}) and that `{MODEL_ID}` resolves (`curl {ENDPOINT}/models`). \
         First reason: {:?}",
        report.arms.iter().find_map(|a| a.error.clone())
    );
}

/// Plan an outline over the bed's window with the PRODUCTION planner, and dump
/// it as the JSON array `COMPOSE_SECTIONS` reads.
///
/// Why this exists rather than a hand-written outline: the readability deficit
/// is structural (-1.21 at 16x4, the only dimension still behind, and the one
/// evidence does not move), and the judge names the cause — "a somewhat
/// fragmented structure with many short sections that jump between topics ...
/// rather than a single narrative arc". Testing that means composing the SAME
/// window against a SHORTER outline. But an outline I author myself is a second
/// decider (§10.6) and measures my prose, not the system's planning. So the
/// system plans it, through `plan_outline` — the same call a real flight makes,
/// honouring `outline_max()` and therefore `SOVEREIGN_DR_REPORT_ARCHITECTURE`.
///
///     COMPOSE_SECTIONS_OUT=/tmp/outline-12.json \
///     SOVEREIGN_DR_REPORT_ARCHITECTURE=1 \
///     cargo test -p sovereign-core --test compose_replay \
///       -- --ignored --nocapture plan_outline_dump
///
/// then fly it: `COMPOSE_SECTIONS=/tmp/outline-12.json ... sweep-compose.sh`
/// with its OWN `OUT` dir, because an arm is keyed by its `NxM` filename alone.
#[tokio::test]
#[ignore = "live daemon + one planning call; run explicitly"]
async fn plan_outline_dump() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "deep_research=info".into()),
        )
        .with_test_writer()
        .try_init();

    let out = std::env::var("COMPOSE_SECTIONS_OUT")
        .expect("COMPOSE_SECTIONS_OUT=<path.json> — where to write the planned outline");
    // ABSOLUTE, CHECKED BEFORE THE PLANNING CALL. `cargo test` runs with CWD at
    // the PACKAGE root, not the repo root, so a repo-relative path resolves
    // somewhere that does not exist — and the failure lands AFTER a ~60s
    // planning call, throwing the work away. Measured 2026-08-27: exactly that,
    // on the first run of this test.
    assert!(
        std::path::Path::new(&out).is_absolute(),
        "COMPOSE_SECTIONS_OUT must be ABSOLUTE ({out} is not): `cargo test` runs \
         with CWD at the package root, so a repo-relative path is written \
         somewhere that does not exist — and you would not find out until after \
         the planning call had already been paid for"
    );
    let path = input_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("compose input {} unreadable ({e})", path.display()));
    let input: ComposeInput = serde_json::from_str(&raw).expect("compose-input.json parses");

    // Same pin as the arms: an unpinned planner is not the planner the arms
    // will then compose against.
    std::env::set_var("SOVEREIGN_DR_PIN_SAMPLING", "1");
    std::env::set_var("SOVEREIGN_DR_COMPOSED_REPORT", "1");

    let provider: Arc<dyn InferenceProvider> = Arc::new(RemoteApiProvider::new(
        ENDPOINT,
        None,
        MODEL_ID,
        PROVIDER_CTX,
    ));
    let (port, _backend) = build_port("auto", None, SearchSource::Corpus, &[], provider, None)
        .await
        .expect("build live research port");

    let planned = synthesize::plan_outline(&*port, &input.question, &input.window)
        .await
        .expect("plan_outline");

    // A planner that returned nothing is a REFUSAL, not an outline. Writing it
    // would hand `COMPOSE_SECTIONS` an empty array, which composes a plausible
    // report against no sections at all (§18.3) — the arm would look flown.
    assert!(
        !planned.is_empty(),
        "plan_outline returned an EMPTY outline"
    );

    eprintln!(
        "planned {} sections (bed pinned {}); architecture flag = {:?}",
        planned.len(),
        input.sections.len(),
        std::env::var("SOVEREIGN_DR_REPORT_ARCHITECTURE").ok()
    );
    for (i, s) in planned.iter().enumerate() {
        eprintln!("  {:2}. {}", i + 1, s);
    }
    std::fs::write(&out, serde_json::to_string_pretty(&planned).unwrap())
        .unwrap_or_else(|e| panic!("write {out}: {e}"));
    eprintln!("wrote {out}");
}

/// Re-render an arm markdown that already exists, with the CURRENT renderer.
///
/// The render delta was measured by scoring `arm-N.md` against
/// `arm-N.rendered.md` — but that rendered file was produced by whatever
/// `number_citations` looked like when the arm flew. Changing the renderer
/// therefore invalidates the comparison without re-flying, and re-flying costs
/// ~10 minutes of writer calls to reproduce a draft we already have byte for
/// byte. This re-renders the SAME draft, so the only variable is the renderer.
///
///     COMPOSE_ARM_MD=/abs/path/arm-16x4.md \
///     COMPOSE_INPUT=/abs/path/compose-input.json \
///     cargo test -p sovereign-core --test compose_replay \
///       -- --ignored --nocapture rerender_existing_arm
///
/// Writes `<stem>.rerendered.md` beside it — NOT `.rendered.md`, which would
/// overwrite the artifact an earlier score is keyed to and make two scores
/// silently incomparable.
#[test]
#[ignore = "reads an existing arm markdown; run explicitly"]
fn rerender_existing_arm() {
    let md_path = std::env::var("COMPOSE_ARM_MD").expect("COMPOSE_ARM_MD=<abs path to arm-*.md>");
    assert!(
        std::path::Path::new(&md_path).is_absolute(),
        "COMPOSE_ARM_MD must be ABSOLUTE ({md_path} is not) — `cargo test` runs \
         with CWD at the package root"
    );
    let path = input_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("compose input {} unreadable ({e})", path.display()));
    let input: ComposeInput = serde_json::from_str(&raw).expect("compose-input.json parses");
    let md = std::fs::read_to_string(&md_path).unwrap_or_else(|e| panic!("{md_path}: {e}"));

    let (numbered, sources) = synthesize::number_citations(&md, &input.window);

    let count = |s: &str, re: &str| s.matches(re).count();
    eprintln!(
        "draft    : {} [Source: ev-  |  rerendered: {} [Source: ev-",
        count(&md, "[Source: ev-"),
        count(&numbered, "[Source: ev-")
    );
    eprintln!("sources listed: {}", sources.len());

    let out = md_path.trim_end_matches(".md").to_string() + ".rerendered.md";
    std::fs::write(&out, &numbered).unwrap_or_else(|e| panic!("write {out}: {e}"));
    eprintln!("wrote {out}");
}
