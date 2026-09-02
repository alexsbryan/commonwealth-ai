// SPDX-License-Identifier: AGPL-3.0-or-later
//! Prompt-byte snapshots pinning ontology invariants I1 and I2
//! (`sovereign/docs/specs/ONTOLOGY_MIGRATION.md` §0).
//!
//! Each prompt the atlas pipeline would send to a model is serialised
//! whole — the [`ChatPrompt`] the chat client itself serialises: system,
//! user, response schema, schema name, phase id, output budget — and
//! byte-compared against a committed golden under
//! `tests/fixtures/ontology_snapshots/<stem>.json`:
//!
//! - I1: the `maple-house` recipe's custom atlas (version-0 ontology block)
//!   — Phase 1, the terse Phase 1 retry, and the Phase 6 classifier — and
//!   the same recipe migrated to `version = 1` with no declarations, which
//!   must reproduce these SAME goldens (not a second set).
//! - I2: the four prebuilt genres' Phase 1 (literary, philosophy,
//!   referential, engineering) — a genre that declares nothing sends no
//!   different bytes.
//!
//! A byte that moves is a leak. When the movement is intended, re-bless
//! (the `UPDATE_RECIPE_SCHEMA` convention from `recipe_schema.rs`), then
//! read the change with `git diff --word-diff` on the fixture:
//!
//! ```text
//! UPDATE_ONTOLOGY_SNAPSHOTS=1 cargo test -p corpus-engine --test main ontology_prompt_snapshots
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use corpus_engine::enrichment::atlas::analysis::{CandidateContent, TensionSide};
use corpus_engine::enrichment::atlas::atoms::{AtomId, ChunkRef};
use corpus_engine::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;
use corpus_engine::enrichment::pipeline::pipelines::{
    engineering_atlas, literary_atlas, philosophy_atlas, referential_atlas,
};
use corpus_engine::enrichment::pipeline::prompts::OVERLAY_ENV_VAR;
use corpus_engine::enrichment::pipeline::types::{ChapterInput, ChatPrompt};
use corpus_engine::enrichment::pipeline::{Pipeline, PipelineRegistry};
use corpus_engine::Recipe;

const UPDATE_ENV: &str = "UPDATE_ONTOLOGY_SNAPSHOTS";

/// The recipe under test — the only shipped version-0 custom ontology.
const MAPLE_HOUSE_RECIPE: &str = "../sovereign-recipes/maple-house/recipe.toml";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ontology_snapshots")
}

/// Byte-compare `prompt` against `<stem>.json`, or rewrite the golden under
/// `UPDATE_ONTOLOGY_SNAPSHOTS=1`. A missing golden is a mismatch, never a
/// pass.
fn assert_snapshot(stem: &str, prompt: &ChatPrompt) {
    // The goldens pin the BAKED prompts. An overlay dir would swap the
    // system text underneath the comparison and report a leak that is
    // really the operator's overlay — refuse rather than substitute.
    if let Ok(dir) = std::env::var(OVERLAY_ENV_VAR) {
        panic!(
            "{OVERLAY_ENV_VAR}={dir} is set; the prompt snapshots pin the baked prompts. \
             Unset it for this test."
        );
    }

    let path = fixtures_dir().join(format!("{stem}.json"));
    let mut rendered = serde_json::to_string_pretty(prompt).expect("serialise ChatPrompt");
    rendered.push('\n');

    if std::env::var(UPDATE_ENV).is_ok() {
        std::fs::create_dir_all(fixtures_dir()).expect("create fixtures dir");
        std::fs::write(&path, &rendered)
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        eprintln!("wrote {}", path.display());
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    if committed != rendered {
        panic!(
            "prompt snapshot `{stem}` differs from {} ({} committed bytes vs {} rendered).\n\
             The bytes sent to a model changed. If that is intended, re-bless with:\n  \
             {UPDATE_ENV}=1 cargo test -p corpus-engine --test main ontology_prompt_snapshots\n\
             and read the change with `git diff --word-diff` on the fixture.\n{}",
            path.display(),
            committed.len(),
            rendered.len(),
            crate::recipe_schema::first_diff(&committed, &rendered)
        );
    }
}

/// One fixed section. `render_phase1_user_body` reads only `chapter_id`,
/// `title`, `metadata["ordinal"]` and `text`, so this is the whole user
/// body's input.
fn fixed_chapter() -> ChapterInput {
    let text = "Article II — Quiet hours. Quiet hours begin at 11 PM every night. \
                Members may not play amplified music in the common spaces after that \
                time. Guests are bound by the same rule as the member who invited them."
        .to_string();
    ChapterInput {
        chapter_id: "sec-0002".into(),
        title: "Article II — Quiet hours".into(),
        approx_tokens: text.len() / 4,
        text,
        metadata: HashMap::from([("ordinal".to_string(), "2".to_string())]),
    }
}

/// One fixed Phase 6 candidate: two claims sharing the `quiet hours` topic.
fn fixed_candidate() -> CandidateContent {
    CandidateContent {
        candidate_id: "cand-0001".into(),
        source_atom: AtomId::from_raw("claim-0001"),
        source_kind: TensionSide::Claim,
        source_text: "Quiet hours begin at 11 PM every night.".into(),
        target_atom: AtomId::from_raw("claim-0002"),
        target_kind: TensionSide::Claim,
        target_text: "Quiet hours begin at 10 PM on weeknights.".into(),
        shared_entity_name: Some("quiet hours".into()),
        shared_entity_id: Some(AtomId::from_raw("entity-0001")),
        evidence: vec![ChunkRef::new("sec-0002", None)],
    }
}

/// The maple-house pipeline exactly as `enrich init` builds it: recipe →
/// `Recipe::custom_atlas_spec` → `with_custom_ontology`.
fn maple_house_pipeline() -> LiteraryAtlasPipeline {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MAPLE_HOUSE_RECIPE);
    let recipe =
        Recipe::from_file(&path).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    let spec = recipe
        .custom_atlas_spec()
        .expect("maple-house declares a non-empty [enrichment.ontology]");
    LiteraryAtlasPipeline::with_custom_ontology(&spec)
}

// ── I1: maple-house, version 0 ──────────────────────────────────────────

#[test]
fn maple_house_phase1_matches_golden() {
    let prompt = maple_house_pipeline().compose_phase1(&fixed_chapter(), &[]);
    assert_snapshot("maple_house.phase1", &prompt);
}

#[test]
fn maple_house_phase1_terse_matches_golden() {
    let prompt = maple_house_pipeline()
        .compose_phase1_terse(&fixed_chapter())
        .expect("custom atlas composes a terse retry");
    assert_snapshot("maple_house.phase1_terse", &prompt);
}

#[test]
fn maple_house_phase6_classifier_matches_golden() {
    let prompt = maple_house_pipeline()
        .compose_phase6_atlas_classifier(&fixed_candidate())
        .expect("custom atlas composes its own Phase 6 classifier");
    assert_snapshot("maple_house.phase6_classifier", &prompt);
}

/// I1, second half. The maple-house recipe migrated to `version = 1`
/// (`Recipe::migrate_ontology_version` — one inserted line, nothing else
/// changes) declares no types, so all three prompts must match the SAME
/// `maple_house.*` goldens the version-0 tests above pin — not a second set.
/// This is the structural half of I1: the P2 composer and parser hang off
/// `OntologyPolicies::has_declarations()`, which is false here.
#[test]
fn maple_house_v1_without_declarations_matches_v0_goldens() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MAPLE_HOUSE_RECIPE);
    let v0 =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let migrated = Recipe::migrate_ontology_version(&v0, 1)
        .expect("migration yields a loadable recipe")
        .expect("maple-house is version 0, so migrating to 1 changes it");
    let recipe = Recipe::from_toml(&migrated).expect("migrated maple-house parses");
    let spec = recipe
        .custom_atlas_spec()
        .expect("still a custom ontology after migration");
    assert_eq!(spec.ontology_version, 1);
    assert!(
        !spec.policies().has_declarations(),
        "the migration adds only the version line; no types are declared"
    );
    let pipeline = LiteraryAtlasPipeline::with_custom_ontology(&spec);

    assert_snapshot(
        "maple_house.phase1",
        &pipeline.compose_phase1(&fixed_chapter(), &[]),
    );
    assert_snapshot(
        "maple_house.phase1_terse",
        &pipeline
            .compose_phase1_terse(&fixed_chapter())
            .expect("custom atlas composes a terse retry"),
    );
    assert_snapshot(
        "maple_house.phase6_classifier",
        &pipeline
            .compose_phase6_atlas_classifier(&fixed_candidate())
            .expect("custom atlas composes its own Phase 6 classifier"),
    );
}

// ── I2: prebuilt genres declare nothing and send no different bytes ─────

#[test]
fn prebuilt_genres_phase1_match_goldens() {
    let registry = PipelineRegistry::builtin();
    let chapter = fixed_chapter();
    for id in [
        literary_atlas::PIPELINE_ID,
        philosophy_atlas::PIPELINE_ID,
        referential_atlas::PIPELINE_ID,
        engineering_atlas::PIPELINE_ID,
    ] {
        let pipeline = registry
            .get(id)
            .unwrap_or_else(|| panic!("`{id}` is a built-in pipeline"));
        let prompt = pipeline.compose_phase1(&chapter, &[]);
        assert_snapshot(&format!("{id}.phase1"), &prompt);
    }
}
