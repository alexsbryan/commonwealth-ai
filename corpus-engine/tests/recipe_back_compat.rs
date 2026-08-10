// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recipe schema back-compatibility regression suite.
//!
//! **Purpose.** Pin canonical recipe-TOML shapes from successive
//! schema versions and assert each still parses against the
//! current `Recipe` struct. New fields added to the schema must be
//! `#[serde(default)]` (or carry an explicit fallback) so older
//! recipes keep loading. This test is the regression net that
//! catches the day someone accidentally removes a default and
//! breaks every previously-published recipe.
//!
//! **Policy** (also documented in `corpus-engine/src/recipe.rs`
//! module docs and `SYSTEM_OVERVIEW.md` §3.10):
//!
//! 1. Adding a field to any recipe struct: MUST carry
//!    `#[serde(default)]` or `#[serde(default = "fn")]`. Old TOMLs
//!    must continue to parse and produce sensible runtime
//!    behaviour.
//! 2. Renaming a field: MUST add `#[serde(alias = "old-name")]` so
//!    in-flight TOMLs keep working. Drop the alias only after a
//!    full schema-version bump cycle.
//! 3. Removing an enum variant: MUST add a manual `untagged`
//!    fallback or a `#[serde(other)]` arm with a clear "use the
//!    new variant `<X>`" error message. Don't ever silently break
//!    a published recipe.
//! 4. Bumping `[corpus] schema_version`: only when the *reader*
//!    needs to opt in (the engine refuses unknown future
//!    versions). Adding fields does NOT require a bump.
//!
//! Each fixture below carries a comment describing the time it
//! represents — extend the suite when a new schema version lands.

use corpus_engine::recipe::{AcquirerConfig, ChunkerConfig, ExtractorConfig};
use corpus_engine::Recipe;

// ---------------------------------------------------------------------------
// schema_version = 1 (initial release)
// ---------------------------------------------------------------------------
//
// Minimum-viable recipe shape from the project's first release.
// No `[parameters]`, no enrichment, no filters. Should always
// parse cleanly.

#[test]
fn v1_minimum_recipe_still_parses() {
    let toml = r#"
[corpus]
id = "demo"
name = "demo"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
"#;
    let r = Recipe::from_toml(toml).expect("v1 minimum recipe must parse");
    assert_eq!(r.corpus.id, "demo");
    assert_eq!(r.corpus.schema_version, 1);
    assert!(r.parameters.is_empty(), "no parameters declared");
    assert!(r.filters.is_empty(), "no filters declared");
    assert!(r.enrichment.is_none(), "no enrichment block");
    assert!(matches!(r.chunk, ChunkerConfig::Paragraph { .. }));
}

/// HuggingFace dataset acquirer + parquet extractor — the SEP /
/// stackexchange shape from the early bundled recipes.
#[test]
fn v1_huggingface_parquet_recipe_still_parses() {
    let toml = r#"
[corpus]
id = "stackexchange"
name = "Stack Exchange"
description = "Reference Q&A"
license = "CC-BY-SA-4.0"
mesh_sharing = true
size_compressed_gb = 50.0
size_indexed_gb = 8.0

[acquire]
type = "huggingface_dataset"
repo = "manu/project_gutenberg"
subset = "en"

[extract]
type = "parquet"
content_column = "text"
url_column = "url"

[chunk]
type = "passthrough"

[index]
fts = true
vector = true
embedding_model = "qwen3-embedding-0.6b"
"#;
    let r = Recipe::from_toml(toml).expect("v1 HF recipe must parse");
    match r.acquire {
        AcquirerConfig::HuggingFaceDataset { repo, subset, .. } => {
            assert_eq!(repo, "manu/project_gutenberg");
            assert_eq!(subset.as_deref(), Some("en"));
        }
        other => panic!("expected HuggingFaceDataset, got {other:?}"),
    }
    assert!(matches!(r.chunk, ChunkerConfig::Passthrough));
}

/// MediaWiki-XML + filters block (Wikipedia Core scope).
/// Captures the schema as it stood when the
/// `Vital Articles L5` filter landed.
#[test]
fn v1_wikipedia_with_filters_still_parses() {
    let toml = r#"
[corpus]
id = "wikipedia"
name = "Wikipedia (English)"
license = "CC-BY-SA-4.0"

[acquire]
type = "bulk_download"
url = "https://example.com/wiki.zip"

[extract]
type = "wikipedia_jsonl"

[chunk]
type = "paragraph"
max_chars = 2048
overlap_chars = 256

[[filter]]
type = "title_list"
list_file = "@bundled:vital_articles_l5"

[filter_mode]
mode = "any"

[enrichment]
enabled = false
type = "field_model"
domain = "multi"
prompt_version = "v1"
"#;
    let r = Recipe::from_toml(toml).expect("v1 Wikipedia recipe must parse");
    assert_eq!(r.filters.len(), 1);
    let enr = r.enrichment.expect("enrichment block parsed");
    assert!(!enr.enabled);
    assert_eq!(enr.enrichment_type, "field_model");
    // Investigation fields must default to empty for legacy
    // enrichment blocks.
    assert!(enr.entity_types.is_empty());
    assert!(enr.relationship_types.is_empty());
    assert!(enr.patterns.is_empty());
}

/// Wikipedia-newsworthy freshness-daemon recipe — exercises the new
/// `portal_event_bullet` chunker variant + `http_api` acquirer with a
/// single templated request shape. The watcher iterates dates and
/// substitutes templating itself; the recipe pins request shape only.
#[test]
fn wikipedia_newsworthy_recipe_parses() {
    let toml = std::fs::read_to_string(
        std::env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .join("sovereign-recipes/wikipedia-newsworthy/recipe.toml"),
    )
    .expect("wikipedia-newsworthy/recipe.toml must exist");
    let r = Recipe::from_toml(&toml).expect("wikipedia-newsworthy recipe must parse");
    assert_eq!(r.corpus.id, "wikipedia-newsworthy");
    assert!(r.corpus.mesh_sharing);
    assert_eq!(r.corpus.scope.as_deref(), Some("newsworthy"));
    assert!(matches!(r.acquire, AcquirerConfig::HttpApi { .. }));
    assert!(matches!(r.extract, ExtractorConfig::WikipediaApiArticle {}));
    assert!(matches!(
        r.chunk,
        ChunkerConfig::PortalEventBullet { max_chars: 2048 },
    ));
}

/// StackExchange XML extractor with the older `mode` /
/// `min_score` defaulting before `QuestionWithAnswers` shipped.
#[test]
fn v1_stackexchange_xml_with_default_mode_still_parses() {
    let toml = r#"
[corpus]
id = "se"
name = "se"

[acquire]
type = "bulk_download"
urls = ["https://a.example/dump.7z", "https://b.example/dump.7z"]

[extract]
type = "stackexchange_xml"

[chunk]
type = "passthrough"
"#;
    let r = Recipe::from_toml(toml).expect("v1 SE-XML recipe must parse");
    match r.extract {
        ExtractorConfig::StackExchangeXml {
            min_score,
            mode,
            max_answers_per_question,
            exclude_closed,
            ..
        } => {
            assert_eq!(min_score, 3); // historical default
            assert_eq!(mode, corpus_engine::recipe::SeMode::AnswerOnly);
            assert_eq!(max_answers_per_question, 5);
            assert!(exclude_closed);
        }
        other => panic!("expected StackExchangeXml, got {other:?}"),
    }
}

/// Catalog-corpus shape from the Gutenberg paradigm: `kind =
/// "catalog"` + `[catalog]` block. Confirms the corpus-kind
/// extension didn't break the surrounding `[corpus]` defaults.
#[test]
fn v1_catalog_corpus_recipe_still_parses() {
    let toml = r#"
[corpus]
id = "gutenberg"
name = "Project Gutenberg Catalog"
license = "Public Domain"
kind = "catalog"
mesh_sharing = true

[acquire]
type = "bulk_download"
url = "https://www.gutenberg.org/cache/epub/feeds/pg_catalog.csv.gz"

[extract]
type = "gutenberg_catalog"

[chunk]
type = "passthrough"

[catalog]
id_field = "gutenberg_id"
download_url_template = "https://www.gutenberg.org/cache/epub/{id}/pg{id}.txt"
content_recipe = "gutenberg-work"
"#;
    let r = Recipe::from_toml(toml).expect("v1 catalog recipe must parse");
    assert_eq!(r.corpus.kind, corpus_engine::types::CorpusKind::Catalog);
    let cat = r.catalog.expect("catalog block");
    assert_eq!(cat.content_recipe, "gutenberg-work");
}

// ---------------------------------------------------------------------------
// schema_version = 1 (additive: parameters + http_api + html_sections)
// ---------------------------------------------------------------------------
//
// PR1 (recipe-authoring platform). These additions are pure
// additions — they don't bump schema_version because old recipes
// without them still parse and run. Tests below pin the new shape.

#[test]
fn http_api_recipe_with_parameters_still_parses() {
    let toml = r#"
[corpus]
id = "sec-filings"
name = "SEC EDGAR Filings"

[parameters.entities]
type = "list"
description = "CIK numbers or tickers"
required = true

[parameters.start_date]
type = "date"
default = "2022-01-01"

[acquire]
type = "http_api"
base_url = "https://efts.sec.gov/LATEST/search-index"
rate_limit_per_second = 8.0

[[acquire.requests]]
url = "{base_url}?q={entity}&from={start_date}"
for_each = ["entities"]

[acquire.pagination]
type = "offset"
param = "start"
page_size = 100

[acquire.follow]
document_url_path = "$.hits.hits[*]._source.file_url"
document_format = "html"

[extract]
type = "html"

[chunk]
type = "paragraph"
"#;
    let r = Recipe::from_toml(toml).expect("http_api recipe must parse");
    assert_eq!(r.parameters.len(), 2);
    assert!(matches!(r.acquire, AcquirerConfig::HttpApi { .. }));
}

#[test]
fn html_sections_recipe_still_parses() {
    let toml = r#"
[corpus]
id = "sec-investigation"
name = "SEC Investigation"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "html_sections"

[[extract.sections]]
name = "md_and_a"
description = "Management Discussion & Analysis"
start_pattern = "(?i)Item\\s+7"
end_pattern = "(?i)Item\\s+8"

[extract.fallback]
type = "full_document"
max_chars = 8192

[chunk]
type = "semantic"
"#;
    let r = Recipe::from_toml(toml).expect("html_sections recipe must parse");
    assert!(matches!(r.extract, ExtractorConfig::HtmlSections { .. }));
}

#[test]
fn investigation_enrichment_recipe_still_parses() {
    let toml = r#"
[corpus]
id = "ai-financing"
name = "AI Financing"

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
attributes = ["name", "ticker"]

[[enrichment.relationship_types]]
name = "investment"
attributes = ["amount_usd"]

[[enrichment.patterns]]
type = "circular_flow"
name = "money_cycles"
edge_types = ["investment", "revenue"]
min_entities = 3
"#;
    let r = Recipe::from_toml(toml).expect("investigation recipe must parse");
    let enr = r.enrichment.expect("enrichment block");
    assert_eq!(enr.entity_types.len(), 1);
    assert_eq!(enr.relationship_types.len(), 1);
    assert_eq!(enr.patterns.len(), 1);
}

// ---------------------------------------------------------------------------
// Deprecation aliases (back-compat for renamed / removed variants)
// ---------------------------------------------------------------------------

/// `api_paginated` was the original (never-implemented) variant
/// name; `http_api` replaced it in PR1. A recipe with the old type
/// should still load — we route it through a deprecation arm that
/// produces a clear "rename your `type = \"api_paginated\"`"
/// message rather than a generic `unknown variant` parse error.
///
/// Keep this test as long as the deprecation arm exists. Drop both
/// at the same time with a schema_version bump.
#[test]
fn deprecated_api_paginated_variant_yields_actionable_error() {
    let toml = r#"
[corpus]
id = "demo"
name = "demo"

[acquire]
type = "api_paginated"
base_url = "https://api.example.com"
page_param = "page"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#;
    let err = Recipe::from_toml(toml).expect_err("api_paginated must NOT parse silently");
    let msg = format!("{err}");
    // The error must mention both the deprecated name (so the user
    // recognises their recipe) and the replacement (so they know
    // what to migrate to).
    assert!(
        msg.contains("api_paginated"),
        "error must mention the deprecated variant: {msg}"
    );
    assert!(
        msg.contains("http_api"),
        "error must point at the replacement: {msg}"
    );
}

// ---------------------------------------------------------------------------
// schema_version enforcement
// ---------------------------------------------------------------------------

/// Loader must refuse `[corpus] schema_version = N` when N is
/// higher than the engine's max-supported version. This is the
/// "future recipe loaded by old engine" guardrail — better to fail
/// loudly than silently run with missing fields the recipe author
/// expected the engine to honour.
#[test]
fn future_schema_version_is_refused() {
    let toml = r#"
[corpus]
id = "demo"
name = "demo"
schema_version = 99

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#;
    let err = Recipe::from_toml(toml).expect_err("future schema_version must be refused");
    let msg = format!("{err}");
    assert!(
        msg.contains("schema_version") && msg.contains("99"),
        "error must surface the offending version: {msg}"
    );
}

/// Past + current schema_version load fine. Confirms the bound is
/// only enforced upward — a v1 recipe loaded by a v2-aware engine
/// works because the v2 additions all have `#[serde(default)]`.
#[test]
fn current_schema_version_is_accepted() {
    let toml = r#"
[corpus]
id = "demo"
name = "demo"
schema_version = 1

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#;
    let r = Recipe::from_toml(toml).expect("current version must parse");
    assert_eq!(r.corpus.schema_version, 1);
}

// ---------------------------------------------------------------------------
// Reserved variants — schema-only future features
// ---------------------------------------------------------------------------
//
// `PatternDecl::CustomSql` is **reserved**: the schema accepts it
// today, the parser round-trips it, the runtime emits a
// placeholder finding, but the actual SQL execution lands later
// (with rusqlite + sandboxing). Reserving the variant up front
// means recipes authored against the future shape don't break
// existing engines and don't need a schema migration when the
// executor lands.
//
// Policy contract:
// 1. Reserved variants MUST parse cleanly.
// 2. The runtime MUST surface them visibly (placeholder finding,
//    warning, etc.) — never silent skip.
// 3. The validator MUST flag them so recipe authors know they're
//    not fully wired yet.

/// `[display]` block was added in the conversation-imports / atlas
/// rail-grouping landing. Every recipe pre-dating that block must
/// continue to parse with `display = None` so old TOMLs keep
/// loading. See `Recipe::display` for the back-compat contract.
#[test]
fn recipe_without_display_block_still_parses() {
    let toml = r#"
[corpus]
id = "demo"
name = "demo"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
"#;
    let r = Recipe::from_toml(toml).expect("display-less recipe must parse");
    assert!(
        r.display.is_none(),
        "absent [display] block must deserialize to None, got {:?}",
        r.display
    );
}

/// Forward shape: a recipe declaring the new `[display]` block.
/// Pins the field names + reads them back. Lives alongside the
/// back-compat fixtures so the schema lock and the new-field
/// happy-path live in one file.
#[test]
fn recipe_with_display_block_round_trips() {
    let toml = r#"
[corpus]
id = "conversations-anthropic"
name = "Claude conversations"

[acquire]
type = "local_file"
path = "~/.svrnmesh/conversations/conversations.json"

[extract]
type = "anthropic_export"

[chunk]
type = "threaded_turns"

[display]
category = "conversation"
icon = "chat-bubble"
"#;
    let r = Recipe::from_toml(toml).expect("display block must parse");
    let d = r.display.expect("display block populated");
    assert_eq!(d.category.as_deref(), Some("conversation"));
    assert_eq!(d.icon.as_deref(), Some("chat-bubble"));
}

#[test]
fn reserved_custom_sql_variant_parses_today() {
    let toml = r#"
[corpus]
id = "demo"
name = "demo"

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

[[enrichment.relationship_types]]
name = "revenue"

[[enrichment.patterns]]
type = "custom_sql"
name = "undisclosed_related_party"
description = "Entities connected by both revenue and investment but not disclosed as related"
query = """
SELECT r1.from_entity, r1.to_entity
FROM relationships r1
JOIN relationships r2 ON r1.from_entity = r2.from_entity AND r1.to_entity = r2.to_entity
WHERE r1.type = 'revenue' AND r2.type = 'investment'
"""
"#;
    let r = Recipe::from_toml(toml).expect("custom_sql must parse today");
    let enr = r.enrichment.expect("enrichment block");
    assert_eq!(enr.patterns.len(), 1);
    match &enr.patterns[0] {
        corpus_engine::PatternDecl::CustomSql { name, query, .. } => {
            assert_eq!(name, "undisclosed_related_party");
            assert!(query.contains("SELECT"));
        }
        other => panic!("expected CustomSql, got {other:?}"),
    }
}
