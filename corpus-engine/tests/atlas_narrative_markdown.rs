//! Integration coverage for the markdown extractor that backs the
//! narrative-stream branch of the two-stream atlas pipeline.
//!
//! Unit tests inside `src/extractors/markdown.rs` cover the lexical
//! chunking primitives. This file pins the **integration** behaviour
//! the rest of the atlas pipeline depends on:
//!
//!   1. Multi-section docs (the demo's ARCH_PRINCIPLES + SYSTEM_OVERVIEW
//!      shape) round-trip through the public `Extractor` trait into
//!      `ExtractedDoc` records with serializable metadata.
//!   2. The metadata's JSON shape is compatible with what
//!      `structure_first` / `atlas-cross-corpus` consumers expect:
//!      same field names as `WikipediaChunkMetadata`, plus the
//!      narrative-specific `inline_code_spans` and `heading_anchor`.
//!   3. `inline_code_spans` survives the JSON round-trip — it's the
//!      load-bearing signal for narrative-vs-structural matching, so
//!      a regression in the metadata shape silently kills drift
//!      detection accuracy.
//!
//! Synthetic fixtures only — no project-specific paths, no demo
//! content. Anyone running `cargo test` from a fresh checkout sees
//! the same expectations.

#![cfg(feature = "markdown")]

use corpus_engine::extractors::markdown::MarkdownExtractor;
use corpus_engine::extractors::markdown_types::{LinkKind, MarkdownChunkMetadata};
use corpus_engine::extractors::Extractor;

const NARRATIVE_FIXTURE: &str = "\
# Project Charter

This is the preamble. The team commits to a small set of architectural
positions documented below.

## Components

The system is built around these load-bearing components:

- The `Runtime` orchestrates `Router` and `Planner`.
- `CorpusEngine` handles ingestion via `corpus_engine::engine::CorpusEngine`.

See [§3](#decisions) for the rationale.

## Decisions

### Decision 1: One process per host

The `EmbeddedDaemon` runs in-process. We MUST NOT spawn a separate
daemon process; this is non-negotiable for our latency budget.

### Decision 2: Narrative-first documentation

[Rust Book](https://doc.rust-lang.org/book/) and similar narratives
are the gold standard. Writers `MUST` use principle-shaped headings.

## Out of scope

Anything not listed above.
";

fn write_temp_doc(content: &str) -> tempfile::NamedTempFile {
    use std::io::Write;
    let mut f = tempfile::Builder::new()
        .suffix(".md")
        .tempfile()
        .expect("tempfile");
    f.write_all(content.as_bytes()).expect("write fixture");
    f
}

fn parse_metadata(doc: &corpus_engine::extractors::ExtractedDoc) -> MarkdownChunkMetadata {
    let raw = doc.metadata.as_ref().expect("metadata present");
    serde_json::from_value(raw.clone()).expect("metadata deserialises into MarkdownChunkMetadata")
}

#[test]
fn extractor_emits_one_doc_per_section_with_full_metadata() {
    let fixture = write_temp_doc(NARRATIVE_FIXTURE);
    let extractor = MarkdownExtractor::new();
    let docs: Vec<_> = extractor
        .extract(fixture.path())
        .expect("extract")
        .filter_map(|r| r.ok())
        .collect();

    let titles: Vec<_> = docs.iter().filter_map(|d| d.title.clone()).collect();
    // Preamble (depth=0, empty title) + 5 named sections.
    assert!(
        titles.iter().any(|t| t == "Project Charter"),
        "expected Project Charter section, got {titles:?}"
    );
    assert!(titles.iter().any(|t| t == "Components"));
    assert!(titles.iter().any(|t| t == "Decisions"));
    assert!(titles
        .iter()
        .any(|t| t == "Decision 1: One process per host"));
    assert!(titles
        .iter()
        .any(|t| t == "Decision 2: Narrative-first documentation"));
    assert!(titles.iter().any(|t| t == "Out of scope"));
}

#[test]
fn metadata_serialises_with_atlas_compatible_field_names() {
    // The cross-corpus matcher and structure_first read these fields
    // by name. If the on-disk JSON shape regresses, drift detection
    // silently breaks. Pin the contract here.
    let fixture = write_temp_doc(NARRATIVE_FIXTURE);
    let extractor = MarkdownExtractor::new();
    let doc = extractor
        .extract(fixture.path())
        .expect("extract")
        .filter_map(|r| r.ok())
        .find(|d| d.title.as_deref() == Some("Components"))
        .expect("Components section");

    let raw = doc.metadata.as_ref().expect("metadata present");
    let obj = raw.as_object().expect("metadata is JSON object");

    // Field-name checks — the names atlas tooling reads:
    assert!(obj.contains_key("section_name"));
    assert!(obj.contains_key("section_path"));
    assert!(obj.contains_key("section_depth"));
    assert!(obj.contains_key("heading_anchor"));
    assert!(obj.contains_key("outgoing_links"));
    assert!(obj.contains_key("inline_code_spans"));
}

#[test]
fn inline_code_spans_survive_round_trip() {
    let fixture = write_temp_doc(NARRATIVE_FIXTURE);
    let extractor = MarkdownExtractor::new();
    let docs: Vec<_> = extractor
        .extract(fixture.path())
        .expect("extract")
        .filter_map(|r| r.ok())
        .collect();
    let components = docs
        .iter()
        .find(|d| d.title.as_deref() == Some("Components"))
        .expect("Components section");
    let meta = parse_metadata(components);

    // The matcher expects bare names AND qualified paths to land in
    // inline_code_spans verbatim, with no normalization that would
    // strip path separators.
    assert!(
        meta.inline_code_spans.contains(&"Runtime".to_string()),
        "expected Runtime span, got {:?}",
        meta.inline_code_spans
    );
    assert!(meta.inline_code_spans.contains(&"Router".to_string()));
    assert!(meta.inline_code_spans.contains(&"CorpusEngine".to_string()));
    assert!(meta
        .inline_code_spans
        .contains(&"corpus_engine::engine::CorpusEngine".to_string()));
}

#[test]
fn nested_section_path_carries_full_breadcrumb() {
    let fixture = write_temp_doc(NARRATIVE_FIXTURE);
    let extractor = MarkdownExtractor::new();
    let docs: Vec<_> = extractor
        .extract(fixture.path())
        .expect("extract")
        .filter_map(|r| r.ok())
        .collect();
    let nested = docs
        .iter()
        .find(|d| d.title.as_deref() == Some("Decision 1: One process per host"))
        .expect("Decision 1 section");
    let meta = parse_metadata(nested);

    assert_eq!(
        meta.section_path,
        vec![
            "Project Charter".to_string(),
            "Decisions".to_string(),
            "Decision 1: One process per host".to_string()
        ]
    );
    assert_eq!(meta.section_depth, 3);
    assert_eq!(meta.heading_anchor, "decision-1-one-process-per-host");
}

#[test]
fn link_kinds_disambiguate_anchor_vs_external() {
    let fixture = write_temp_doc(NARRATIVE_FIXTURE);
    let extractor = MarkdownExtractor::new();
    let docs: Vec<_> = extractor
        .extract(fixture.path())
        .expect("extract")
        .filter_map(|r| r.ok())
        .collect();
    let components = docs
        .iter()
        .find(|d| d.title.as_deref() == Some("Components"))
        .expect("Components");
    let comp_meta = parse_metadata(components);
    let anchor = comp_meta
        .outgoing_links
        .iter()
        .find(|l| l.link_target == "#decisions")
        .expect("intra-doc anchor");
    assert_eq!(anchor.kind, LinkKind::Anchor);

    let decision_2 = docs
        .iter()
        .find(|d| d.title.as_deref() == Some("Decision 2: Narrative-first documentation"))
        .expect("Decision 2");
    let dec_meta = parse_metadata(decision_2);
    let external = dec_meta
        .outgoing_links
        .iter()
        .find(|l| l.link_target.starts_with("https://"))
        .expect("external link");
    assert_eq!(external.kind, LinkKind::External);
}

#[test]
fn extractor_output_is_deterministic_across_runs() {
    // Same fixture → byte-identical metadata across two runs. Drift
    // detection downstream depends on this for stable atom ids and
    // reproducible reports.
    let fixture = write_temp_doc(NARRATIVE_FIXTURE);
    let run = || -> String {
        let docs: Vec<_> = MarkdownExtractor::new()
            .extract(fixture.path())
            .expect("extract")
            .filter_map(|r| r.ok())
            .collect();
        serde_json::to_string(
            &docs
                .iter()
                .map(|d| (d.title.clone(), d.metadata.clone()))
                .collect::<Vec<_>>(),
        )
        .expect("serialise")
    };
    assert_eq!(run(), run());
}
