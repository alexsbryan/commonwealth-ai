// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end test for the `sovereign enrich` admin harness.
//!
//! Exercises: `init` args parsing → writing config.json + chapters.json →
//! `extract` with a deterministic `(EmbedFn, ChatCompletionFn)` mock →
//! run output file on disk + cache populated on `--full`.
//!
//! The test does not spawn the binary; it calls the subcommand
//! handlers' internal helpers (`corpus_io::rebuild_corpus_state` +
//! `extract::run_with_closures_for_test`) directly so we can inject
//! the mock daemon without standing up a real one. This matches
//! ARCH_PRINCIPLES §12.4 (tests must not require GPU, network, or
//! real model weights) and §12.5 (use the in-process mock set).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use super::config::{EnrichConfig, CONFIG_SCHEMA_VERSION};
use super::paths;
use super::test_env::{scoped_home, HomeGuard};
use corpus_engine::enrichment::pipeline::{
    ChapterManifest, ChapterSelection, ChatCompletionFn, ChatPrompt, Phase1Output, PhaseCache,
    PipelinePhase,
};
use corpus_engine::types::EmbedFn;

fn synthetic_book() -> String {
    let mut s = String::new();
    s.push_str("Preamble text, not inside a chapter.\n\n");
    // Each chapter body clears the short-chapter skip threshold
    // (40 words) so the phase-1 runner actually dispatches to the
    // mock chat function instead of skipping as front-matter.
    for i in 1..=5 {
        s.push_str(&format!("Chapter {i}\n\n"));
        s.push_str(&format!(
            "The body of chapter {i}. It reads like prose and carries a theme \
             through scenes that unfold across a handful of paragraphs, with \
             characters whose choices press against each other in ways that \
             make the central tension legible without ever stating it outright.\n\n"
        ));
        s.push_str(&format!(
            "A second paragraph in chapter {i}, continuing the scene with \
             additional dialogue, small gestures, and the kind of passing \
             observations that let a reader feel the weight of what is at stake \
             in this corner of the story before the chapter's turn arrives.\n\n"
        ));
    }
    s
}

fn deterministic_embed() -> EmbedFn {
    Arc::new(move |text: &str| {
        // Simple 3-dim embedding keyed by the first ASCII letter of
        // the text. Deterministic across runs; enough for top-K
        // selection to be well-defined in tests.
        let c = text.chars().next().unwrap_or('z').to_ascii_lowercase();
        let v = match c {
            'a'..='i' => vec![1.0_f32, 0.0, 0.0],
            'j'..='r' => vec![0.0, 1.0, 0.0],
            _ => vec![0.0, 0.0, 1.0],
        };
        Box::pin(async move { Ok(v) })
    })
}

fn canned_chat() -> ChatCompletionFn {
    Arc::new(move |prompt: &ChatPrompt| {
        let u = prompt.user.clone();
        let body = if u.contains("Chapter 1") {
            r#"{"questions":["What opens the story?"],"reveals":"framing","thematic_carriers":["Narrator"]}"#
        } else if u.contains("Chapter 2") {
            r#"{"questions":["What does chapter 2 explore?","A second question."]}"#
        } else if u.contains("Chapter 3") {
            r#"{"questions":["Third question."]}"#
        } else if u.contains("Chapter 4") {
            r#"{"questions":["Fourth."]}"#
        } else {
            r#"{"questions":["Default."]}"#
        };
        let body = body.to_string();
        Box::pin(async move { Ok(body) })
    })
}

/// Build a pinned config + chapters.json + enrichment dirs the way
/// `init` would, bypassing the daemon probe. Keeps the test hermetic.
fn scaffold_corpus(corpus_id: &str, source_path: &std::path::Path) -> EnrichConfig {
    let cfg = EnrichConfig {
        schema_version: CONFIG_SCHEMA_VERSION,
        corpus_id: corpus_id.into(),
        pipeline_id: "literary".into(),
        source_path: source_path.to_path_buf(),
        chapter_regex: corpus_engine::chunkers::sectioned::ChapterRegexDetector::DEFAULT_PATTERN
            .to_string(),
        chat_model: "test-chat".into(),
        chat_models: None,
        embed_model: "test-embed".into(),
        base_url: "http://localhost:9741".into(),
        // Synthetic fixture bodies are short; keep the filter off so
        // the test exercises the full phase-1 path end-to-end.
        min_section_body_words: 0,
        toc_markers: None,
        max_output_tokens: 4096,
        phase1b_max_output_tokens: None,
        phase_overrides: None,
        created_at: "2026-04-22T00:00:00Z".into(),
    };
    // Create dirs
    fs::create_dir_all(paths::enrichment_root(corpus_id)).unwrap();
    fs::create_dir_all(paths::exemplars_dir(corpus_id)).unwrap();
    fs::create_dir_all(paths::cache_dir(corpus_id)).unwrap();
    fs::create_dir_all(paths::runs_dir(corpus_id)).unwrap();
    fs::create_dir_all(paths::index_root(corpus_id)).unwrap();
    cfg.save().unwrap();

    // Build + save chapter manifest so future runs merge against it.
    let source = fs::read_to_string(source_path).unwrap();
    let detector = corpus_engine::chunkers::sectioned::ChapterRegexDetector::new();
    let chunker = corpus_engine::chunkers::sectioned::SectionedChunker::with_detector(detector);
    let report = chunker.dry_run(&source);
    let manifest = ChapterManifest::from_detected_sections(corpus_id, &source, &report.sections);
    manifest
        .save(&paths::chapters_manifest_path(corpus_id))
        .unwrap();
    cfg
}

#[tokio::test]
async fn full_run_writes_cache_and_run_file() {
    let home: HomeGuard = scoped_home();

    // Write a fake source file inside the tempdir so paths stay scoped.
    let source_path: PathBuf = home.path().join("book.txt");
    fs::write(&source_path, synthetic_book()).unwrap();

    let cfg = scaffold_corpus("test-book", &source_path);

    // Run extract --full via the test entry point.
    let (produced, cache_updated) = super::extract::run_with_closures_for_test(
        &cfg.corpus_id,
        ChapterSelection::Full,
        deterministic_embed(),
        canned_chat(),
    )
    .await
    .unwrap();
    assert_eq!(produced, 5);
    assert!(cache_updated);

    // Cache file should exist and round-trip.
    let cache = PhaseCache::new(paths::cache_dir(&cfg.corpus_id));
    let out: Phase1Output = cache
        .read(PipelinePhase::Questions)
        .unwrap()
        .expect("cache should be populated after --full");
    assert_eq!(out.pipeline_id, "literary");
    assert_eq!(out.questions_by_chapter.len(), 5);

    // Run file should exist under runs/.
    let runs_dir = paths::runs_dir(&cfg.corpus_id);
    let any_run = fs::read_dir(&runs_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("questions-full-")
        });
    assert!(any_run, "expected a questions-full-*.json run file");
}

#[tokio::test]
async fn subset_run_does_not_update_cache() {
    let home: HomeGuard = scoped_home();
    let source_path: PathBuf = home.path().join("book.txt");
    fs::write(&source_path, synthetic_book()).unwrap();
    let cfg = scaffold_corpus("subset-book", &source_path);

    let (produced, cache_updated) = super::extract::run_with_closures_for_test(
        &cfg.corpus_id,
        ChapterSelection::Subset(vec!["sec_0001".into(), "sec_0003".into()]),
        deterministic_embed(),
        canned_chat(),
    )
    .await
    .unwrap();
    assert_eq!(produced, 2);
    assert!(!cache_updated);

    // Cache should be empty.
    let cache = PhaseCache::new(paths::cache_dir(&cfg.corpus_id));
    let out: Option<Phase1Output> = cache.read(PipelinePhase::Questions).unwrap();
    assert!(out.is_none(), "subset run must not populate cache");

    // Run file still written with mode=subset.
    let runs_dir = paths::runs_dir(&cfg.corpus_id);
    let any_run = fs::read_dir(&runs_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("questions-subset-")
        });
    assert!(any_run, "expected a questions-subset-*.json run file");
}

#[tokio::test]
async fn config_require_errors_before_init() {
    let _home: HomeGuard = scoped_home();
    let err = EnrichConfig::require("never-init-ed").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("no enrichment config"),
        "message should prompt user toward `enrich init`: {msg}"
    );
}

#[test]
fn paths_layout_resolves_under_redirected_home() {
    let home: HomeGuard = scoped_home();
    let root = paths::enrichment_root("foo");
    assert!(root.starts_with(home.path()));
    assert!(paths::config_path("foo").ends_with("config.json"));
    assert!(paths::exemplars_dir("foo").ends_with("exemplars"));
    assert!(paths::cache_dir("foo").ends_with("cache"));
    assert!(paths::runs_dir("foo").ends_with("runs"));
}
