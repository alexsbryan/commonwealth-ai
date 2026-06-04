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
use corpus_engine::enrichment::pipeline::types::{ChatCompletionFn, ChatPrompt};
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
            content: "OpenAI generated approximately $1.5B from Microsoft Azure customers — \
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
    let out = run_investigation(&recipe, &chunks, chat, dir.path())
        .await
        .unwrap();

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

const UAP_RECIPE: &str = r#"
[corpus]
id = "uap-test"
name = "uap test"

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
name = "sighting"
description = "An observed event"
attributes = ["occurred_at"]

[[enrichment.entity_types]]
name = "installation"
description = "A military base"
attributes = ["branch", "type"]

[[enrichment.entity_types]]
name = "observed_object"
description = "The thing observed"
attributes = ["shape", "color"]

[[enrichment.relationship_types]]
name = "occurred_near"
description = "sighting occurred near installation"
directional = false

[[enrichment.relationship_types]]
name = "involves_object"
description = "sighting involves an observed object"
directional = true

[[enrichment.patterns]]
type = "threshold"
name = "sighting_hotspots"
description = "Installations with more than 3 nearby sightings"
edge_type = "occurred_near"
attribute = "sighting_count"
threshold = 3.0
comparison = "greater_than"
"#;

/// UAP demo path: exercises Phase A end-to-end —
/// (A1) 4 surface-form variants of one base coalesce into a single
/// installation with aliases, while a distinct base stays separate;
/// (A2) declared entity attributes (the observed object's `shape`) are
/// populated from the `entities[]` array; (A3) the deterministic
/// `sighting_count` aggregation makes the count-based hotspot threshold
/// fire on the merged base.
#[tokio::test]
async fn uap_coalesces_variants_populates_attrs_and_fires_hotspot() {
    let recipe = Recipe::from_toml(UAP_RECIPE).unwrap();

    // Five sightings: four near Wright-Patterson (under four surface
    // forms) + one near Edwards. The four WP forms must merge so the
    // hotspot (>3) fires; Edwards (count 1) must not.
    let chunks = vec![
        ChunkInput { chunk_id: "wp1", source_title: None, content: "near Wright-Patterson AFB" },
        ChunkInput { chunk_id: "wp2", source_title: None, content: "near Wright-Patterson Air Force Base" },
        ChunkInput { chunk_id: "wp3", source_title: None, content: "near Wright-Patterson" },
        ChunkInput { chunk_id: "wp4", source_title: None, content: "near Wright Patterson AFB" },
        ChunkInput { chunk_id: "ed1", source_title: None, content: "near Edwards AFB" },
    ];

    let responses: &[&str] = &[
        r#"{
            "entities": [
                {"name": "Wright-Patterson AFB", "type": "installation", "attributes": {"branch": "USAF", "type": "AIRBASE"}},
                {"name": "sighting-1", "type": "sighting", "attributes": {"occurred_at": "1952-07-01"}},
                {"name": "object-1", "type": "observed_object", "attributes": {"shape": "DISC", "color": "silver"}}
            ],
            "relationships": [
                {"from_entity": "sighting-1", "to_entity": "Wright-Patterson AFB", "from_type": "sighting", "to_type": "installation", "type": "occurred_near", "verbatim_excerpt": "near Wright-Patterson AFB", "confidence": 1.0},
                {"from_entity": "sighting-1", "to_entity": "object-1", "from_type": "sighting", "to_type": "observed_object", "type": "involves_object", "verbatim_excerpt": "a disc", "confidence": 1.0}
            ]
        }"#,
        r#"{
            "entities": [
                {"name": "Wright-Patterson Air Force Base", "type": "installation", "attributes": {"branch": "USAF"}},
                {"name": "sighting-2", "type": "sighting", "attributes": {}}
            ],
            "relationships": [
                {"from_entity": "sighting-2", "to_entity": "Wright-Patterson Air Force Base", "from_type": "sighting", "to_type": "installation", "type": "occurred_near", "verbatim_excerpt": "near Wright-Patterson Air Force Base", "confidence": 1.0}
            ]
        }"#,
        r#"{
            "entities": [
                {"name": "sighting-3", "type": "sighting", "attributes": {}}
            ],
            "relationships": [
                {"from_entity": "sighting-3", "to_entity": "Wright-Patterson", "from_type": "sighting", "to_type": "installation", "type": "occurred_near", "verbatim_excerpt": "near Wright-Patterson", "confidence": 1.0}
            ]
        }"#,
        r#"{
            "entities": [
                {"name": "sighting-4", "type": "sighting", "attributes": {}}
            ],
            "relationships": [
                {"from_entity": "sighting-4", "to_entity": "Wright Patterson AFB", "from_type": "sighting", "to_type": "installation", "type": "occurred_near", "verbatim_excerpt": "near Wright Patterson AFB", "confidence": 1.0}
            ]
        }"#,
        r#"{
            "entities": [
                {"name": "sighting-5", "type": "sighting", "attributes": {}}
            ],
            "relationships": [
                {"from_entity": "sighting-5", "to_entity": "Edwards AFB", "from_type": "sighting", "to_type": "installation", "type": "occurred_near", "verbatim_excerpt": "near Edwards AFB", "confidence": 1.0}
            ]
        }"#,
    ];
    let chat = scripted_responses(responses);
    let dir = tempfile::tempdir().unwrap();
    let out = run_investigation(&recipe, &chunks, chat, dir.path())
        .await
        .unwrap();

    // (A1) The four Wright-Patterson surface forms coalesce into ONE
    // installation; Edwards stays separate.
    let installations: Vec<_> = out
        .entities
        .iter()
        .filter(|e| e.entity_type == "installation")
        .collect();
    assert_eq!(
        installations.len(),
        2,
        "WP variants merge + Edwards separate; got: {installations:?}"
    );
    let wp = out
        .entities
        .iter()
        .find(|e| e.id == "e-installation-wright-patterson")
        .expect("merged Wright-Patterson installation");
    // Longest surface form is canonical; the others are aliases.
    assert_eq!(wp.canonical_name, "Wright-Patterson Air Force Base");
    assert!(wp.aliases.contains(&"Wright-Patterson AFB".to_string()));
    assert!(wp.aliases.contains(&"Wright Patterson AFB".to_string()));

    // (A2) The observed object's declared `shape` attribute is populated.
    let obj = out
        .entities
        .iter()
        .find(|e| e.entity_type == "observed_object")
        .expect("observed object extracted");
    assert_eq!(obj.attributes.get("shape"), Some(&serde_json::json!("DISC")));
    // Installation attributes merged across mentions too.
    assert_eq!(wp.attributes.get("branch"), Some(&serde_json::json!("USAF")));

    // (A3) The hotspot threshold fires on the merged base (4 sightings > 3)
    // and NOT on Edwards (1 sighting).
    let hotspots: Vec<_> = out
        .findings
        .iter()
        .filter(|f| f.pattern_name == "sighting_hotspots")
        .collect();
    assert_eq!(hotspots.len(), 1, "only WP clears the threshold; got: {hotspots:?}");
    assert_eq!(hotspots[0].entity_ids, vec!["e-installation-wright-patterson"]);
    // The stamped count is on the entity.
    assert_eq!(
        wp.attributes.get("sighting_count"),
        Some(&serde_json::json!(4))
    );
}
