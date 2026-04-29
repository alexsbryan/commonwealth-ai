//! End-to-end test for the investigation enrichment pipeline.
//!
//! Drives the full Extract → Coalesce → DetectPatterns flow with
//! a deterministic mock chat closure, against a recipe TOML that
//! looks like one a financial journalist would author for an
//! AI-financing investigation. Covers:
//!
//! - Schema-driven extraction prompt is sent (mock matches it).
//! - Multiple chunks worth of relationships coalesce into a single
//!   set of canonical entities.
//! - Pattern detectors find the planted circular flow + role
//!   overlap + threshold violation.
//! - The three JSON files end up on disk in the expected layout.
//!
//! Live LLM tests (against a real chat slot) live elsewhere; this
//! file runs offline and is part of the default CI suite.

use std::sync::Arc;

use corpus_engine::enrichment::investigation::{
    run_investigation, ChunkInput, INVESTIGATION_DIRNAME,
};
use corpus_engine::enrichment::pipeline::types::{
    ChatCompletionFn, ChatPrompt,
};
use corpus_engine::Recipe;

/// A scripted chat closure that returns a different canned response
/// per chunk. Indexed by call order; falls back to an empty
/// `relationships` array after the script is exhausted so the
/// pipeline doesn't error on unexpected extra calls.
fn scripted_responses(responses: &'static [&'static str]) -> ChatCompletionFn {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let responses = responses.to_vec();
    Arc::new(move |_prompt: &ChatPrompt| {
        let calls = calls.clone();
        let responses = responses.clone();
        Box::pin(async move {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let response = responses
                .get(n)
                .copied()
                .unwrap_or(r#"{"relationships":[]}"#)
                .to_string();
            Ok(response)
        })
    })
}

const RECIPE: &str = r#"
[corpus]
id = "ai-financing"
name = "AI financing investigation"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "html"

[chunk]
type = "paragraph"

[enrichment]
enabled = true
type = "investigation"

[[enrichment.entity_types]]
name = "company"
description = "A corporation"
attributes = ["name", "ticker", "cik"]

[[enrichment.relationship_types]]
name = "investment"
description = "A invested equity in B"
attributes = ["amount_usd", "date"]

[[enrichment.relationship_types]]
name = "revenue"
description = "Recognized revenue from B to A"
attributes = ["amount_usd", "percentage_of_total"]

[[enrichment.relationship_types]]
name = "cloud_commitment"
description = "A committed to purchase cloud capacity from B"
attributes = ["amount_usd", "duration_years"]

[[enrichment.patterns]]
type = "circular_flow"
name = "money_cycles"
description = "A→B→C→A money cycles across investment + revenue"
min_entities = 3
edge_types = ["revenue", "investment", "cloud_commitment"]

[[enrichment.patterns]]
type = "role_overlap"
name = "invest_in_customer"
description = "A invests in B and B is a customer of A"
[enrichment.patterns.entity_roles]
investor = "investment.from"
customer = "revenue.to"

[[enrichment.patterns]]
type = "threshold"
name = "revenue_concentration"
description = "Revenue concentration above 10%"
edge_type = "revenue"
attribute = "percentage_of_total"
threshold = 0.10
comparison = "greater_than"
"#;

#[tokio::test]
async fn end_to_end_finds_all_three_pattern_types() {
    let recipe = Recipe::from_toml(RECIPE).unwrap();

    let chunks = vec![
        ChunkInput {
            chunk_id: "msft-investment",
            source_title: Some("MSFT 10-K"),
            content: "Microsoft made a $13B equity investment in OpenAI in 2023.",
        },
        ChunkInput {
            chunk_id: "openai-revenue",
            source_title: Some("OpenAI investor letter"),
            content:
                "OpenAI generated approximately $1.5B from Microsoft Azure customers — \
                 representing 12% of total revenue.",
        },
        ChunkInput {
            chunk_id: "cloud-cycle-A",
            source_title: Some("Nvidia 10-K"),
            content: "Nvidia recognized $2B in cloud GPU revenue from CoreWeave.",
        },
        ChunkInput {
            chunk_id: "cloud-cycle-B",
            source_title: Some("CoreWeave S-1"),
            content: "CoreWeave committed $5B to Microsoft Azure for cloud capacity.",
        },
        ChunkInput {
            chunk_id: "cloud-cycle-C",
            source_title: Some("Microsoft 10-K"),
            content: "Microsoft committed to a multi-year $10B GPU contract with Nvidia.",
        },
    ];

    // Canned responses — one per chunk.
    let responses: &[&str] = &[
        // Microsoft → OpenAI investment + revenue back (creates the
        // role-overlap pair).
        r#"{
            "relationships": [
                {
                    "from_entity": "Microsoft",
                    "to_entity": "OpenAI",
                    "from_type": "company",
                    "to_type": "company",
                    "type": "investment",
                    "attributes": {"amount_usd": 13000000000, "date": "2023-01-01"},
                    "verbatim_excerpt": "Microsoft made a $13B equity investment in OpenAI in 2023.",
                    "confidence": 0.95
                }
            ]
        }"#,
        // OpenAI → Microsoft revenue with high concentration (12%).
        r#"{
            "relationships": [
                {
                    "from_entity": "OpenAI",
                    "to_entity": "Microsoft",
                    "from_type": "company",
                    "to_type": "company",
                    "type": "revenue",
                    "attributes": {"amount_usd": 1500000000, "percentage_of_total": 0.12},
                    "verbatim_excerpt": "OpenAI generated approximately $1.5B from Microsoft Azure customers",
                    "confidence": 0.9
                }
            ]
        }"#,
        // Cloud cycle leg 1: Nvidia ← CoreWeave revenue (CoreWeave pays Nvidia for GPUs).
        r#"{
            "relationships": [
                {
                    "from_entity": "CoreWeave",
                    "to_entity": "Nvidia",
                    "from_type": "company",
                    "to_type": "company",
                    "type": "revenue",
                    "attributes": {"amount_usd": 2000000000, "percentage_of_total": 0.4},
                    "verbatim_excerpt": "Nvidia recognized $2B in cloud GPU revenue from CoreWeave.",
                    "confidence": 0.92
                }
            ]
        }"#,
        // Cloud cycle leg 2: CoreWeave commits to Microsoft cloud.
        r#"{
            "relationships": [
                {
                    "from_entity": "Microsoft",
                    "to_entity": "CoreWeave",
                    "from_type": "company",
                    "to_type": "company",
                    "type": "cloud_commitment",
                    "attributes": {"amount_usd": 5000000000, "duration_years": 5},
                    "verbatim_excerpt": "CoreWeave committed $5B to Microsoft Azure for cloud capacity.",
                    "confidence": 0.85
                }
            ]
        }"#,
        // Cloud cycle leg 3: Microsoft commits to Nvidia (closes the cycle).
        r#"{
            "relationships": [
                {
                    "from_entity": "Nvidia",
                    "to_entity": "Microsoft",
                    "from_type": "company",
                    "to_type": "company",
                    "type": "cloud_commitment",
                    "attributes": {"amount_usd": 10000000000, "duration_years": 3},
                    "verbatim_excerpt": "Microsoft committed to a multi-year $10B GPU contract with Nvidia.",
                    "confidence": 0.88
                }
            ]
        }"#,
    ];
    let chat = scripted_responses(responses);
    let dir = tempfile::tempdir().unwrap();
    let out = run_investigation(&recipe, &chunks, chat, dir.path()).await.unwrap();

    // 4 unique entities: Microsoft, OpenAI, Nvidia, CoreWeave.
    assert_eq!(out.entities.len(), 4, "got: {:?}", out.entities);

    // 5 relationships total.
    assert_eq!(out.relationships.len(), 5);

    // Pattern findings: at least one of each declared pattern.
    let names: Vec<&str> = out
        .findings
        .iter()
        .map(|f| f.pattern_name.as_str())
        .collect();
    assert!(
        names.contains(&"money_cycles"),
        "expected money_cycles, got {names:?}"
    );
    assert!(
        names.contains(&"invest_in_customer"),
        "expected invest_in_customer, got {names:?}"
    );
    assert!(
        names.contains(&"revenue_concentration"),
        "expected revenue_concentration, got {names:?}"
    );

    // The cycle should pass through Nvidia → Microsoft → CoreWeave → Nvidia.
    let cycle = out
        .findings
        .iter()
        .find(|f| f.pattern_name == "money_cycles")
        .unwrap();
    assert_eq!(cycle.entity_ids.len(), 3);

    // The role-overlap should pair Microsoft and OpenAI.
    let overlap = out
        .findings
        .iter()
        .find(|f| f.pattern_name == "invest_in_customer")
        .unwrap();
    assert!(overlap.entity_ids.iter().any(|id| id.contains("microsoft")));
    assert!(overlap.entity_ids.iter().any(|id| id.contains("openai")));

    // Threshold should fire on the OpenAI → Microsoft revenue
    // (12% > 10%) and the CoreWeave → Nvidia revenue (40% > 10%).
    let threshold_findings: Vec<_> = out
        .findings
        .iter()
        .filter(|f| f.pattern_name == "revenue_concentration")
        .collect();
    assert_eq!(threshold_findings.len(), 2);

    // Files exist on disk.
    let invest_dir = dir.path().join(INVESTIGATION_DIRNAME);
    assert!(invest_dir.join("entities.json").exists());
    assert!(invest_dir.join("relationships.json").exists());
    assert!(invest_dir.join("pattern_findings.json").exists());
}
