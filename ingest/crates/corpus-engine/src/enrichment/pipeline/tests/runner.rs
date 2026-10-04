// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for [`super`], the Phase runner.
//!
//! A sibling file, not an inline module: the module pushed `runner.rs` past
//! its arch-gate ceiling (ARCH §3.1). Under `tests/` so size-gate counts it
//! as test mass; `#[path]`, so every name and `use super::*` is unchanged.

use super::*;
use crate::enrichment::pipeline::pipelines::literary::LiteraryPipeline;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::tempdir;

#[path = "runner_text.rs"]
mod text;

#[path = "runner_relation_focus.rs"]
mod relation_focus;

fn chapter(id: &str, title: &str, body: &str) -> ChapterInput {
    // Pad every test body past MIN_PHASE1_CHAPTER_WORDS so the
    // short-chapter skip doesn't fire on fixtures that are meant
    // to exercise the chat-and-parse path. The original `body`
    // stays at the start so `canned_chat`'s substring matches
    // ("FAIL", "Two", etc.) still dispatch correctly.
    let padding_word = " filler";
    // 60 copies of "filler" = 60 words, comfortably above the
    // 40-word MIN_PHASE1_CHAPTER_WORDS threshold regardless of the
    // caller's seed body length.
    let text = format!("{body}{}", padding_word.repeat(60));
    let approx_tokens = text.len() / 4;
    ChapterInput {
        chapter_id: id.into(),
        title: title.into(),
        text,
        metadata: HashMap::new(),
        approx_tokens,
    }
}

/// Deterministic embed: returns a 3-dim vector keyed by the first
/// ASCII letter. Lets tests verify top-K selection without real
/// embeddings.
fn alphabet_embed() -> EmbedFn {
    Arc::new(move |s: &str| {
        let c = s.chars().next().unwrap_or('z');
        let v = match c {
            'a'..='i' => vec![1.0_f32, 0.0, 0.0],
            'j'..='r' => vec![0.0, 1.0, 0.0],
            _ => vec![0.0, 0.0, 1.0],
        };
        Box::pin(async move { Ok(v) })
    })
}

/// Deterministic chat: returns a fixed Phase1-shaped JSON keyed
/// by the chapter title embedded in the user prompt. Fails for
/// a chapter whose title includes "FAIL".
fn canned_chat() -> InferenceFn {
    Arc::new(move |prompt: &ChatPrompt, _max_tokens: Option<u32>| {
        let user = prompt.user.clone();
        let body: String = if user.contains("FAIL") {
            // Respond with something that doesn't parse.
            "not-json at all".into()
        } else if user.contains("NOJSON") {
            "```\ngarbage\n```".into()
        } else {
            let q = if user.contains("Two") {
                r#"{"questions":["q-a","q-b"]}"#
            } else {
                r#"{"questions":["only-q"]}"#
            };
            q.into()
        };
        Box::pin(async move { Ok(body) })
    })
}

fn runner_under_test(root: &Path) -> PhaseRunner {
    let cache = PhaseCache::new(root.join("cache"));
    let runs = RunOutputWriter::new(root.join("runs"));
    PhaseRunner::new(
        Arc::new(LiteraryPipeline::new()),
        alphabet_embed(),
        canned_chat(),
        cache,
        runs,
        root.join("exemplars"),
    )
}

#[tokio::test]
async fn phase_1_terse_retry_errors_when_pipeline_has_no_terse_variant() {
    // The v1 `LiteraryPipeline` does NOT override
    // `compose_phase1_terse` — the trait default returns None.
    // A --terse retry against that pipeline must fail fast with
    // a clear error rather than silently reusing the default
    // prompt.
    let dir = tempdir().unwrap();
    let runner = runner_under_test(dir.path());
    let chapters = vec![chapter("ch_01", "Chapter 1", "A body")];
    let err = runner
        .phase_1_extract_questions_with_retry(
            &chapters,
            &ChapterSelection::Subset(vec!["ch_01".into()]),
            Some(RetryMode::Terse {
                max_output_tokens: 16384,
            }),
            |_| {},
        )
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("does not implement `compose_phase1_terse`"),
        "expected terse-unsupported error, got: {msg}"
    );
}

#[tokio::test]
async fn phase_1_terse_retry_uses_token_aware_chat_when_available() {
    // When the runner has a `chat_with_tokens` closure configured
    // and the pipeline supports a terse variant, a Terse retry
    // routes through the token-aware closure with the requested
    // cap.
    use crate::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;

    let dir = tempdir().unwrap();
    let cache = PhaseCache::new(dir.path().join("cache"));
    let runs = RunOutputWriter::new(dir.path().join("runs"));

    // The token-aware chat records the requested max_output_tokens
    // for inspection. Returns a canned atlas JSON so the chapter
    // succeeds.
    let observed = Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
    let observed_c = observed.clone();
    let chat_with_tokens: InferenceFn =
        Arc::new(move |_prompt: &ChatPrompt, tokens: Option<u32>| {
            observed_c
                .lock()
                .unwrap()
                .push(tokens.expect("override budget"));
            let body = r#"{
                  "section_id": "ch_01",
                  "entities_introduced": [{"canonical_name": "A", "entity_type": "person"}],
                  "questions_raised": [{"content": "Why?"}]
                }"#
            .to_string();
            Box::pin(async move { Ok(body) })
        });

    // The default chat is used only when retry_mode is None.
    // Our test sets retry_mode = Some(Terse), so this closure
    // should NOT be invoked — we make it panic to prove that.
    let default_chat: InferenceFn =
        Arc::new(move |_prompt: &ChatPrompt, _max_tokens: Option<u32>| {
            Box::pin(async move {
                panic!("default chat should not be invoked when terse retry is active");
            })
        });

    let runner = PhaseRunner::new(
        Arc::new(LiteraryAtlasPipeline::new()),
        alphabet_embed(),
        default_chat,
        cache,
        runs,
        dir.path().join("exemplars"),
    )
    .with_chat_with_tokens(chat_with_tokens);

    let chapters = vec![chapter("ch_01", "Chapter 1", "body")];
    let res = runner
        .phase_1_extract_questions_with_retry(
            &chapters,
            &ChapterSelection::Subset(vec!["ch_01".into()]),
            Some(RetryMode::Terse {
                max_output_tokens: 12345,
            }),
            |_| {},
        )
        .await
        .expect("terse retry succeeds on literary_atlas");

    assert_eq!(res.output.questions_by_chapter.len(), 1);
    let recorded = observed.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![12345],
        "expected exactly one token-aware call at the requested cap"
    );
}

#[tokio::test]
async fn phase_1_default_variant_routes_through_token_aware_chat_at_default_budget() {
    // Updated 2026-05-11: the no-seed/no-retry path now routes
    // through `chat_with_tokens` at `PHASE1_DEFAULT_OUTPUT_BUDGET`
    // (4096) so the per-request token cap fits inside the
    // daemon's inference deadline on fast chat slots. Previously
    // this path called `(self.chat)(&prompt, None)` and inherited the
    // daemon-side 16384 default, which routinely deadline-timed
    // out at ~11 tok/s. Section that needs more headroom gets
    // caught by `run_extract_step`'s auto-retry at 16384.
    use crate::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;

    let dir = tempdir().unwrap();
    let cache = PhaseCache::new(dir.path().join("cache"));
    let runs = RunOutputWriter::new(dir.path().join("runs"));

    use std::sync::Mutex;
    let recorded_budgets: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded_budgets_clone = Arc::clone(&recorded_budgets);
    let chat_with_tokens: InferenceFn =
        Arc::new(move |_prompt: &ChatPrompt, tokens: Option<u32>| {
            recorded_budgets_clone
                .lock()
                .unwrap()
                .push(tokens.expect("override budget"));
            let body = r#"{
                  "section_id": "ch_01",
                  "entities_introduced": [{"canonical_name": "A", "entity_type": "person"}],
                  "questions_raised": [{"content": "Why?"}]
                }"#
            .to_string();
            Box::pin(async move { Ok(body) })
        });

    let runner = PhaseRunner::new(
            Arc::new(LiteraryAtlasPipeline::new()),
            alphabet_embed(),
            Arc::new(move |_prompt: &ChatPrompt, _max_tokens: Option<u32>| {
                Box::pin(async move {
                    panic!(
                        "default chat closure should not be invoked when chat_with_tokens is configured"
                    );
                })
            }),
            cache,
            runs,
            dir.path().join("exemplars"),
        )
        .with_chat_with_tokens(chat_with_tokens);

    let chapters = vec![chapter("ch_01", "Chapter 1", "body")];
    let res = runner
        .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
        .await
        .unwrap();
    assert_eq!(res.output.questions_by_chapter.len(), 1);
    let recorded = recorded_budgets.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![PHASE1_DEFAULT_OUTPUT_BUDGET],
        "expected one token-aware call at the default Phase 1 budget"
    );
}

#[tokio::test]
async fn phase_1_retry_failed_merges_into_cache() {
    // `--retry-failed` → ChapterSelection::RetryFailed. A success
    // for one of the previously-failed chapter ids should (a) flip
    // `cache_updated = true`, (b) replace the cached entry for
    // that chapter, (c) drop the now-resolved failure from the
    // cached failures list. Unrelated cached chapters are
    // untouched. This is the architectural fix for the
    // hand-merge workaround we used on the Dopesick Jesus
    // recovery.
    let dir = tempdir().unwrap();
    let runner = runner_under_test(dir.path());

    // Seed an existing cache with one success (ch_01) and one
    // failure (ch_02) — the shape left by a `--full` run that
    // stumbled on one chapter.
    let seeded = Phase1Output {
        schema_version: Phase1Output::SCHEMA_VERSION,
        pipeline_id: "literary".into(),
        questions_by_chapter: vec![ExtractedQuestion {
            chapter_id: "ch_01".into(),
            questions: vec!["pre-existing".into()],
            reveals: None,
            thematic_carriers: Vec::new(),
            setting: None,
            plot: None,
            section_extraction: None,
        }],
        failures: vec![Phase1Failure {
            chapter_id: "ch_02".into(),
            reason: "parse error".into(),
            raw_response_head: None,
            failure_kind: PhaseFailureKind::ParseDrift,
        }],
        written_at: "prior".into(),
    };
    runner
        .cache
        .write(PipelinePhase::Questions, &seeded)
        .expect("seed cache");

    let chapters = vec![
        chapter("ch_01", "One", "body"),
        chapter("ch_02", "One", "body"),
    ];
    let res = runner
        .phase_1_extract_questions_with_retry(
            &chapters,
            &ChapterSelection::RetryFailed(vec!["ch_02".into()]),
            None,
            |_| {},
        )
        .await
        .expect("retry-failed run");

    assert!(
        res.cache_updated,
        "RetryFailed with a success must merge into cache"
    );
    assert_eq!(res.output.questions_by_chapter.len(), 1);
    assert_eq!(res.output.questions_by_chapter[0].chapter_id, "ch_02");

    // Read back the cache. Both chapters should now be present as
    // successes; the failures list is empty.
    let cached: Phase1Output = runner
        .cache
        .read(PipelinePhase::Questions)
        .expect("read cache")
        .expect("cache present");
    assert_eq!(cached.questions_by_chapter.len(), 2);
    let ch1 = cached
        .questions_by_chapter
        .iter()
        .find(|e| e.chapter_id == "ch_01")
        .expect("ch_01 still cached");
    // ch_01 was NOT targeted by the retry — its questions must
    // remain the pre-existing value.
    assert_eq!(ch1.questions, vec!["pre-existing".to_string()]);
    let ch2 = cached
        .questions_by_chapter
        .iter()
        .find(|e| e.chapter_id == "ch_02")
        .expect("ch_02 merged");
    assert!(!ch2.questions.is_empty());
    assert!(
        cached.failures.is_empty(),
        "resolved failure must drop out of cached failures list, got: {:?}",
        cached.failures
    );
}

#[tokio::test]
async fn phase_1_retry_failed_with_no_prior_cache_is_noop() {
    // An operator who runs `--retry-failed` before any `--full`
    // has been successful (no cache file yet) should get a clean
    // `cache_updated = false` rather than an error. The run file
    // still captures the recovery attempt; only the promote step
    // is skipped.
    let dir = tempdir().unwrap();
    let runner = runner_under_test(dir.path());
    let chapters = vec![chapter("ch_01", "One", "body")];
    let res = runner
        .phase_1_extract_questions_with_retry(
            &chapters,
            &ChapterSelection::RetryFailed(vec!["ch_01".into()]),
            None,
            |_| {},
        )
        .await
        .expect("run completes even without a prior cache");
    assert!(!res.cache_updated);
    // The run file exists for debugging.
    assert!(res.run_path.exists(), "run file should be written");
    // No cache was seeded → none should have been written.
    let cached: Option<Phase1Output> = runner
        .cache
        .read(PipelinePhase::Questions)
        .expect("read cache");
    assert!(
        cached.is_none(),
        "retry with empty cache must not create a cache file"
    );
}

#[tokio::test]
async fn phase_1_retry_failed_labels_run_file_with_retry_mode() {
    // Mode label "retry" distinguishes RetryFailed runs from
    // diagnostic subsets ("subset") in the runs/ directory. The
    // run filename shape is `questions-<mode>-NNN.json`.
    let dir = tempdir().unwrap();
    let runner = runner_under_test(dir.path());
    let chapters = vec![chapter("ch_01", "One", "body")];
    let res = runner
        .phase_1_extract_questions_with_retry(
            &chapters,
            &ChapterSelection::RetryFailed(vec!["ch_01".into()]),
            None,
            |_| {},
        )
        .await
        .expect("run completes");
    let name = res
        .run_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        name.starts_with("questions-retry-"),
        "expected retry-labelled run file, got: {name}"
    );
}

#[tokio::test]
async fn phase_1_with_seed_cache_routes_to_chat_with_tokens_at_seed_budget() {
    // Landing 2.B invariant: when the cache has a seed file AND
    // the runner has `chat_with_tokens` configured, the default
    // Phase 1 branch routes through that closure with the
    // runner's seed-scoped output budget (`PHASE1_SEED_OUTPUT_BUDGET`).
    // Without a seed cached, the runner falls back to the
    // un-capped default chat (covered by
    // `phase_1_default_variant_ignores_token_aware_chat`).
    use crate::enrichment::pipeline::atlas::{
        EntityType as AtlasEntityType, SeedEntities, SeedEntity, SeedOrigin,
    };
    use crate::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;

    let dir = tempdir().unwrap();
    let cache = PhaseCache::new(dir.path().join("cache"));
    let runs = RunOutputWriter::new(dir.path().join("runs"));

    // Pre-populate the seed cache so the default Phase 1 branch
    // sees `seed_opt = Some(...)` and routes to chat_with_tokens.
    let seed = SeedEntities {
        schema_version: SeedEntities::SCHEMA_VERSION,
        corpus_id: "bk".into(),
        origin: SeedOrigin::Llm,
        entries: vec![SeedEntity {
            canonical_name: "A".into(),
            aliases: Vec::new(),
            entity_type: AtlasEntityType::Person,
            description: "seed".into(),
        }],
        written_at: "2026-04-23T00:00:00Z".into(),
    };
    cache.write(PipelinePhase::SeedExtraction, &seed).unwrap();

    // Record the token cap each chat_with_tokens call is invoked
    // with. Return a minimal atlas JSON so parse succeeds.
    let observed = Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
    let observed_c = observed.clone();
    let chat_with_tokens: InferenceFn =
        Arc::new(move |_prompt: &ChatPrompt, tokens: Option<u32>| {
            observed_c
                .lock()
                .unwrap()
                .push(tokens.expect("override budget"));
            let body = r#"{
                  "section_id": "ch_01",
                  "entities_introduced": [{"canonical_name": "A", "entity_type": "person"}],
                  "questions_raised": [{"content": "Why?"}]
                }"#
            .to_string();
            Box::pin(async move { Ok(body) })
        });

    // The main Phase 1 branch must route through chat_with_tokens
    // (verified below). Phase 1B coverage refinement is opt-in via
    // `SOVEREIGN_RUN_PHASE1B=1` (see `runner.rs:871`); the default
    // skips it to save ~7-8 min/atlas. The default_chat closure is
    // wired so the test would surface unexpected dispatch — but
    // Phase 1B SHOULD NOT run under the default env, so the count
    // assertion below is `0`.
    let default_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let default_calls_c = default_calls.clone();
    let default_chat: InferenceFn =
        Arc::new(move |_prompt: &ChatPrompt, _max_tokens: Option<u32>| {
            default_calls_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Stub body — only reached if Phase 1B is opted in.
            let body = r#"{"missed_entities": [], "missed_concepts": []}"#.to_string();
            Box::pin(async move { Ok(body) })
        });

    let runner = PhaseRunner::new(
        Arc::new(LiteraryAtlasPipeline::new()),
        alphabet_embed(),
        default_chat,
        cache,
        runs,
        dir.path().join("exemplars"),
    )
    .with_chat_with_tokens(chat_with_tokens);

    let chapters = vec![chapter("ch_01", "Chapter 1", "body")];
    let res = runner
        .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
        .await
        .unwrap();
    assert_eq!(res.output.questions_by_chapter.len(), 1);
    let recorded = observed.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![PHASE1_SEED_OUTPUT_BUDGET],
        "expected one token-aware call at the seed output budget"
    );
    // Phase 1B coverage is opt-in via `SOVEREIGN_RUN_PHASE1B=1`.
    // Under the default env the secondary passes don't run, so
    // `default_chat` is never dispatched. Asserting zero pins the
    // current default while leaving room for an opt-in variant of
    // this test if the env-gate is ever flipped back to on-by-default.
    let default_n = default_calls.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        default_n, 0,
        "Phase 1B is opt-in; default_chat should not run, got {default_n}"
    );
}

#[tokio::test]
async fn phase_1_full_writes_run_and_cache() {
    let dir = tempdir().unwrap();
    let runner = runner_under_test(dir.path());
    let chapters = vec![
        chapter("ch_01", "Chapter 1", "A body with One question."),
        chapter("ch_02", "Chapter 2", "A body with Two questions."),
    ];
    let progress_count = AtomicUsize::new(0);
    let res = runner
        .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_ev| {
            progress_count.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();
    assert_eq!(res.output.questions_by_chapter.len(), 2);
    assert!(res.cache_updated);
    assert!(res.run_path.exists());
    // Cache file should exist and round-trip through PhaseCache.
    let back: Option<Phase1Output> = runner.cache().read(PipelinePhase::Questions).unwrap();
    assert!(back.is_some());
    assert!(progress_count.load(Ordering::Relaxed) >= 4); // Start + 2 chapters + Done at minimum
}

/// A subset run over a corpus that already has a cache merges into it:
/// the re-run chapters are replaced, the rest kept. Copying the subset over
/// the cache left resolve with only the re-run sections (6 of 29 on
/// ft-ans-dev-b).
#[tokio::test]
async fn phase_1_subset_merges_into_an_existing_cache() {
    let dir = tempdir().unwrap();
    let runner = runner_under_test(dir.path());
    let chapters = vec![
        chapter("ch_01", "Chapter 1", "Body one"),
        chapter("ch_02", "Chapter 2", "Body two"),
        chapter("ch_03", "Chapter 3", "Body three"),
    ];
    runner
        .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
        .await
        .unwrap();
    let res = runner
        .phase_1_extract_questions(
            &chapters,
            &ChapterSelection::Subset(vec!["ch_02".into()]),
            |_| {},
        )
        .await
        .unwrap();
    assert!(res.cache_updated);
    let cached = runner
        .cache()
        .read::<Phase1Output>(PipelinePhase::Questions)
        .unwrap()
        .expect("the cache survives a subset run");
    let ids: Vec<&str> = cached
        .questions_by_chapter
        .iter()
        .map(|c| c.chapter_id.as_str())
        .collect();
    assert_eq!(ids, ["ch_01", "ch_02", "ch_03"]);
}

#[tokio::test]
async fn phase_1_subset_writes_run_but_not_cache() {
    let dir = tempdir().unwrap();
    let runner = runner_under_test(dir.path());
    let chapters = vec![
        chapter("ch_01", "Chapter 1", "Body one"),
        chapter("ch_02", "Chapter 2", "Body two"),
        chapter("ch_03", "Chapter 3", "Body three"),
    ];
    let res = runner
        .phase_1_extract_questions(
            &chapters,
            &ChapterSelection::Subset(vec!["ch_01".into(), "ch_03".into()]),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(res.output.questions_by_chapter.len(), 2);
    assert_eq!(res.output.questions_by_chapter[0].chapter_id, "ch_01");
    assert_eq!(res.output.questions_by_chapter[1].chapter_id, "ch_03");
    assert!(!res.cache_updated);
    assert!(res.run_path.exists());
    // Cache should NOT have been written by a subset run.
    assert!(runner
        .cache()
        .read::<Phase1Output>(PipelinePhase::Questions)
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn phase_1_subset_rejects_unknown_chapter_id() {
    let dir = tempdir().unwrap();
    let runner = runner_under_test(dir.path());
    let chapters = vec![chapter("ch_01", "Chapter 1", "body")];
    let err = runner
        .phase_1_extract_questions(
            &chapters,
            &ChapterSelection::Subset(vec!["nope".into()]),
            |_| {},
        )
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("chapter not found"));
}

#[tokio::test]
async fn phase_1_parse_failure_captured_as_failure_not_run_failure() {
    let dir = tempdir().unwrap();
    let runner = runner_under_test(dir.path());
    let chapters = vec![
        chapter("ch_01", "Chapter 1", "A body with one question."),
        // The chat mock replies with non-JSON when title contains FAIL.
        chapter("ch_02", "FAIL Chapter", "body"),
    ];
    let res = runner
        .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
        .await
        .unwrap();
    assert_eq!(res.output.questions_by_chapter.len(), 1);
    assert_eq!(res.failures.len(), 1);
    assert_eq!(res.failures[0].chapter_id, "ch_02");
    assert!(res.failures[0].reason.contains("parse error"));
}

#[tokio::test]
async fn phase_1_skips_chapters_with_empty_bodies() {
    // A short "Part I"-style section has no substantive body.
    // The runner should register a skip without burning a chat
    // call, and the failure reason should name the root cause.
    let dir = tempdir().unwrap();
    let runner = runner_under_test(dir.path());
    let short = ChapterInput {
        chapter_id: "sec_0001".into(),
        title: "Part I".into(),
        text: "Book I. The History Of A Family".into(),
        metadata: std::collections::HashMap::new(),
        approx_tokens: 10,
    };
    let real = chapter("sec_0002", "Chapter 1", &"body word ".repeat(60));
    let res = runner
        .phase_1_extract_questions(&[short, real], &ChapterSelection::Full, |_| {})
        .await
        .unwrap();
    assert_eq!(res.output.questions_by_chapter.len(), 1);
    assert_eq!(res.failures.len(), 1);
    assert_eq!(res.failures[0].chapter_id, "sec_0001");
    assert!(
        res.failures[0].reason.contains("too short"),
        "expected short-body reason, got: {}",
        res.failures[0].reason
    );
    // The skip must not fabricate a raw response.
    assert!(res.failures[0].raw_response_head.is_none());
}

/// A catalogue entry is short because it is dense, not empty. The
/// corpus's own floor decides: the default 40 skipped six K1 hoard
/// entries (17-33 words) on the ANS dev fixture.
#[tokio::test]
async fn phase_1_skip_floor_is_the_corpus_setting() {
    let dir = tempdir().unwrap();
    let runner = runner_under_test(dir.path()).with_min_body_words(8);
    let entry = ChapterInput {
        chapter_id: "sec_0001".into(),
        title: "Phacous: IGCH 1678".into(),
        text: "Phacous, Egypt, 1907. Tetradrachms of Alexander struck at Sardes, \
                   Miletus, Lampsacus and Sidon, buried about 305 B.C."
            .into(),
        metadata: std::collections::HashMap::new(),
        approx_tokens: 30,
    };
    let heading = ChapterInput {
        chapter_id: "sec_0002".into(),
        title: "Part I".into(),
        text: "Book I. The History Of A Family".into(),
        metadata: std::collections::HashMap::new(),
        approx_tokens: 10,
    };
    let res = runner
        .phase_1_extract_questions(&[entry, heading], &ChapterSelection::Full, |_| {})
        .await
        .unwrap();
    let skipped: Vec<&str> = res.failures.iter().map(|f| f.chapter_id.as_str()).collect();
    assert_eq!(skipped, ["sec_0002"], "only the heading is skipped");
    assert_eq!(
        res.output.questions_by_chapter.len(),
        1,
        "the entry is extracted"
    );
}

#[tokio::test]
async fn phase_1_zero_chapters_errors_cleanly() {
    let dir = tempdir().unwrap();
    let runner = runner_under_test(dir.path());
    let chapters: Vec<ChapterInput> = Vec::new();
    let err = runner
        .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("zero target chapters"));
}

/// A chat mock that returns well-formed JSON for every phase 1-7
/// call. Branches on which system preamble is present in the prompt
/// to return the right shape.
#[path = "runner_phases.rs"]
mod phases;
