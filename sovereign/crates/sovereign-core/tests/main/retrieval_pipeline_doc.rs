// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign/docs/retrieval-pipeline.md` is GENERATED from the live
//! pipeline definitions (`kq_pipeline` / `deep_pipeline` /
//! `retrieval_pipeline_flags`) so the doc cannot drift from the code —
//! same contract as sovereign-recipes/SCHEMA.md (`recipe_schema` test).
//!
//! Regenerate after changing the pipelines or the flag registry:
//!
//!   UPDATE_RETRIEVAL_PIPELINE_DOC=1 cargo test -p sovereign-core --test main retrieval_pipeline_doc

use std::path::PathBuf;

use sovereign_core::runtime::retrieval_pipeline::{
    deep_pipeline, kq_pipeline, retrieval_pipeline_flags, RetrievalPipeline,
};

const HEADER: &str = "\
<!-- GENERATED FILE — do not edit by hand.\n\
     Source: sovereign-core/src/runtime/retrieval_pipeline.rs\n\
     Regenerate: UPDATE_RETRIEVAL_PIPELINE_DOC=1 cargo test -p sovereign-core --test main retrieval_pipeline_doc -->\n\
\n\
# Retrieval pipeline — steps and knobs\n\
\n\
The retrieval-injection orchestration is data: each pipeline is an\n\
ordered list of named steps run by one tracing runner (one\n\
`tracing::info!(target: \"retrieval.pipeline\")` line per step with\n\
`chunks_before/after/delta`). The governing principle: **the intent\n\
decides HOW to answer (model tier, expansion, synthesis shape) — never\n\
WHERE knowledge lives.** Both pipelines share the same 3-step\n\
evidence-gathering head and 13-step core (incl. the FR-9 governance\n\
active-set filter); they differ only in their\n\
tails. Step ORDER is bench-tuned data, pinned by golden tests — see\n\
the module doc in `retrieval_pipeline.rs` for design rationale and the\n\
dated convergence/divergence log.\n\
\n";

fn render() -> String {
    let mut md = String::from(HEADER);

    md.push_str("## Step sequences\n\n");
    md.push_str(&render_pipeline(
        &kq_pipeline(),
        "KnowledgeQuery / ComparisonQuery (`kq_pipeline`)",
    ));
    md.push_str(&render_pipeline(
        &deep_pipeline(true),
        "DeepQuery / SimpleQuery (`deep_pipeline(true)`)",
    ));
    md.push_str(&render_pipeline(
        &deep_pipeline(false),
        "DeepQuery attached-document variant (`deep_pipeline(false)`)",
    ));

    md.push_str(
        "## Env-knob registry\n\n\
         Every `SOVEREIGN_*` knob the pipeline (and its immediate\n\
         post-steps) reads. Step `-` marks knobs read inside a helper\n\
         rather than gating a whole step. A registry-coverage test\n\
         asserts every step-level gate appears here.\n\n\
         | step | flag | default | purpose |\n\
         |---|---|---|---|\n",
    );
    for (step, flag) in retrieval_pipeline_flags() {
        md.push_str(&format!(
            "| {} | `{}` | {} | {} |\n",
            step, flag.name, flag.default, flag.purpose
        ));
    }

    md.push_str(
        "\n## Verdict buckets (2026-06-10 flag audit)\n\n\
         - **Validated, default ON** — `SOVEREIGN_ATLAS_GROUNDING`,\n\
           `SOVEREIGN_RAPTOR_GROUNDING` (+`_LATE` position),\n\
           `SOVEREIGN_HISTORY_RETRIEVAL`; router-side:\n\
           `SOVEREIGN_KQ_EFFORT_TIER`, `SOVEREIGN_ROUTER_ROBUST_COARSE`\n\
           (both A/B-validated 2026-06-09). Disable only for A/B runs.\n\
         - **Experimental, opt-in (default OFF)** — `SOVEREIGN_ATOM_ENUM`\n\
           (net-negative on focused enumeration per the 2026-06-04\n\
           bench; keep gated), `SOVEREIGN_TITLE_EXPAND` (see\n\
           wikipedia_learn/V36_FINDINGS.md), `SOVEREIGN_QUERY_DECOMP`,\n\
           `SOVEREIGN_GRAPH_NEIGHBOR_EXPAND`, `SOVEREIGN_COMPACTION_DISABLE`.\n\
           Flipping one ON in prod requires its own bench A/B.\n\
         - **Tunable parameters** — the `_TOPK/_POOL/_RANK/_SCORE`,\n\
           `_TOP_M/_MIN_LEVEL/_DEDUPE`, `DECOMP_DECAY`,\n\
           `CONV_PPR_WEIGHT` family. Sub-knobs of their parent feature.\n\
         - **Debug / escape hatches** — `SOVEREIGN_FORENSIC` (audit\n\
           snapshots), `SOVEREIGN_ATOM_ENUM_NOFILTER` (ablation).\n\
           Never set in normal operation.\n",
    );
    md
}

fn render_pipeline(p: &RetrievalPipeline, title: &str) -> String {
    let mut s = format!("### {title}\n\n| # | step | gate flag |\n|---|---|---|\n");
    for (i, step) in p.steps.iter().enumerate() {
        let flag = step
            .flag
            .map(|f| format!("`{}`", f.name))
            .unwrap_or_else(|| "—".to_string());
        s.push_str(&format!("| {} | `{}` | {} |\n", i + 1, step.name, flag));
    }
    s.push('\n');
    s
}

#[test]
fn retrieval_pipeline_doc_is_fresh() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out_path = manifest.join("../../docs/retrieval-pipeline.md");
    let generated = render();

    if std::env::var("UPDATE_RETRIEVAL_PIPELINE_DOC").is_ok() {
        std::fs::write(&out_path, &generated).expect("write retrieval-pipeline.md");
        eprintln!("wrote {}", out_path.display());
        return;
    }

    let committed = std::fs::read_to_string(&out_path).unwrap_or_default();
    assert_eq!(
        committed, generated,
        "sovereign/docs/retrieval-pipeline.md is stale — the pipelines or the \
         flag registry changed. Regenerate with:\n  \
         UPDATE_RETRIEVAL_PIPELINE_DOC=1 cargo test -p sovereign-core --test main retrieval_pipeline_doc"
    );
}
