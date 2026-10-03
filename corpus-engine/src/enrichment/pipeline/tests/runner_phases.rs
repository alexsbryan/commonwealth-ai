// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 2+ runner tests (clustering, seeding, cascade), split from
//! `tests/runner.rs` to keep both under the arch-gate ceiling (ARCH §3.2).

use super::*;

fn multiphase_chat() -> InferenceFn {
    Arc::new(move |prompt: &ChatPrompt, _max_tokens: Option<u32>| {
        let sys = prompt.system.to_string();
        let body = if sys.contains("Phase 1") {
            // Echo the first word of the chapter body so different
            // chapters produce questions that embed into different
            // groups under `four_group_embed`.
            let seed = prompt
                .user
                .split("**Body:**")
                .nth(1)
                .and_then(|b| b.split_whitespace().next())
                .unwrap_or("question")
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_string();
            format!(r#"{{"questions":["{seed} question for this chapter"]}}"#)
        } else if sys.contains("Phase 3") {
            r#"{"concern_text":"Can meaning survive defiance?","scope":"novel-wide"}"#.to_string()
        } else if sys.contains("Phase 5") {
            // Echo any chunk_id the prompt mentions; grab the first
            // `chunk_id=N` token.
            let cid = prompt
                .user
                .split("chunk_id=")
                .nth(1)
                .and_then(|s| s.split('`').next())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            format!(
                r#"{{"position_text":"a position","grounding":[{{"chunk_id":{cid},"section_id":"sec_0001","summary":"s"}}]}}"#
            )
        } else if sys.contains("Phase 6") {
            r#"{"tension":true,"description":"structural parallel","structural_type":"parallel_contrast"}"#.to_string()
        } else if sys.contains("Phase 7") {
            r#"{"gaps":[{"gap_text":"Vronsky social world fades","evidence":"few refs","significance":"medium"}]}"#.to_string()
        } else {
            r#"{"ok":true}"#.to_string()
        };
        Box::pin(async move { Ok(body) })
    })
}

/// Deterministic embed that maps text to a 4-dim vector keyed by the
/// FIRST non-whitespace letter — enough variety to let HDBSCAN
/// produce two+ clusters when inputs span two letter groups.
fn four_group_embed() -> EmbedFn {
    Arc::new(move |text: &str| {
        let c = text
            .chars()
            .find(|c| !c.is_whitespace())
            .unwrap_or('z')
            .to_ascii_lowercase();
        let v: Vec<f32> = match c {
            'a'..='g' => vec![1.0, 0.0, 0.0, 0.0],
            'h'..='m' => vec![0.0, 1.0, 0.0, 0.0],
            'n'..='t' => vec![0.0, 0.0, 1.0, 0.0],
            _ => vec![0.0, 0.0, 0.0, 1.0],
        };
        // Add a tiny per-character jitter so HDBSCAN doesn't reject
        // identical vectors as degenerate.
        let len = text.len() as f32;
        let jitter: Vec<f32> = v
            .iter()
            .enumerate()
            .map(|(i, x)| x + 0.001 * (len + i as f32))
            .collect();
        Box::pin(async move { Ok(jitter) })
    })
}

fn multiphase_runner(root: &Path) -> PhaseRunner {
    let cache = PhaseCache::new(root.join("cache"));
    let runs = RunOutputWriter::new(root.join("runs"));
    PhaseRunner::new(
        Arc::new(LiteraryPipeline::new()),
        four_group_embed(),
        multiphase_chat(),
        cache,
        runs,
        root.join("exemplars"),
    )
}

fn synth_context() -> CorpusContext {
    // Three dense embed-groups × 3 chapters each so HDBSCAN (default
    // `min_cluster_size=3` on LiteraryPipeline) finds at least one
    // cluster per group.
    let groups = [
        ("apples", "Apples and acorns abound here."),
        ("hills", "Hills hide hopeful hares here."),
        ("nectar", "Nectar never numbs nerves here."),
    ];
    let mut chapters = Vec::new();
    let mut chapter_titles = Vec::new();
    for (gi, group) in groups.iter().enumerate() {
        for ci in 0..3 {
            let id = format!("ch_{:02}", gi * 3 + ci + 1);
            let title = format!("Chapter {}", gi * 3 + ci + 1);
            chapter_titles.push(title.clone());
            chapters.push(chapter(
                &id,
                &title,
                &format!("{}, variation {}.", group.1, ci),
            ));
        }
    }
    let mut chunks: Vec<ChunkRecord> = Vec::new();
    let mut cid = 0u64;
    for (gi, group) in groups.iter().enumerate() {
        for ci in 0..6 {
            chunks.push(ChunkRecord {
                id: cid,
                section_id: format!("sec_{:04}", gi + 1),
                text: format!("{} variation {}", group.1, ci),
            });
            cid += 1;
        }
    }
    CorpusContext {
        chapters,
        chunks,
        chapter_titles,
    }
}

#[tokio::test]
async fn phase_2_clusters_questions_from_cache() {
    let dir = tempdir().unwrap();
    let runner = multiphase_runner(dir.path());
    let ctx = synth_context();

    // Seed phase 1 with --full.
    runner
        .phase_1_extract_questions(&ctx.chapters, &ChapterSelection::Full, |_| {})
        .await
        .unwrap();

    let res = runner.phase_2_cluster_questions().await.unwrap();
    assert!(res.cache_updated);
    assert!(res.run_path.exists());
    // Each chapter produced one question; groups are keyed on text's
    // first letter via four_group_embed. Clusters may coalesce
    // depending on HDBSCAN density — we only assert the output
    // shape is coherent, not a specific count.
    let total: usize = res
        .output
        .clusters
        .iter()
        .map(|c| c.question_refs.len())
        .sum::<usize>()
        + res.output.unclustered.len();
    assert_eq!(total, 9);
}

#[tokio::test]
async fn phase_2_atlas_errors_when_cache_has_no_section_extraction() {
    // The v1 LiteraryPipeline doesn't populate section_extraction.
    // Phase 2 atlas against a v1 cache should fail with a clear
    // message pointing the operator at an atlas pipeline.
    let dir = tempdir().unwrap();
    let runner = multiphase_runner(dir.path());
    let ctx = synth_context();
    runner
        .phase_1_extract_questions(&ctx.chapters, &ChapterSelection::Full, |_| {})
        .await
        .unwrap();

    let err = runner.phase_2_cluster_atlas().await.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("section_extraction"),
        "expected error about missing atlas sketches: {msg}"
    );
}

#[tokio::test]
async fn phase_2_atlas_clusters_synthesized_sketches() {
    use crate::enrichment::pipeline::atlas::{
        ClaimSketch, DiscourseAct, EnrichmentDepth, EpistemicStatus, QuestionSketch,
        SectionExtraction,
    };
    use crate::enrichment::pipeline::{ExtractedQuestion, Phase1Output};

    let dir = tempdir().unwrap();
    let runner = multiphase_runner(dir.path());

    // Seed a synthetic Phase 1 cache whose chapters carry
    // atlas sketches — bypasses the v1 pipeline, which doesn't
    // produce them.
    let section = |id: &str| SectionExtraction {
        section_id: id.into(),
        enrichment_depth: EnrichmentDepth::Extracted,
        claims: vec![
            ClaimSketch {
                attributes: Default::default(),
                claim_kind: None,
                subject: None,
                scope: None,
                content: "love costs".into(),
                discourse_act: DiscourseAct::Enact,
                epistemic_status: EpistemicStatus::Confident,
                attributed_to: None,
                quotable_excerpt: None,
                anchor: String::new(),
            },
            ClaimSketch {
                attributes: Default::default(),
                claim_kind: None,
                subject: None,
                scope: None,
                content: "love rewards".into(),
                discourse_act: DiscourseAct::Enact,
                epistemic_status: EpistemicStatus::Confident,
                attributed_to: None,
                quotable_excerpt: None,
                anchor: String::new(),
            },
        ],
        questions_raised: vec![QuestionSketch {
            content: "what remains after loss?".into(),
            anchor: String::new(),
        }],
        ..Default::default()
    };
    let phase1 = Phase1Output {
        schema_version: Phase1Output::SCHEMA_VERSION,
        pipeline_id: "literary_atlas".into(),
        questions_by_chapter: vec![
            ExtractedQuestion {
                chapter_id: "sec_0001".into(),
                questions: vec!["what remains after loss?".into()],
                reveals: None,
                thematic_carriers: Vec::new(),
                setting: None,
                plot: None,
                section_extraction: Some(section("sec_0001")),
            },
            ExtractedQuestion {
                chapter_id: "sec_0002".into(),
                questions: vec!["what remains after loss?".into()],
                reveals: None,
                thematic_carriers: Vec::new(),
                setting: None,
                plot: None,
                section_extraction: Some(section("sec_0002")),
            },
        ],
        failures: Vec::new(),
        written_at: "t".into(),
    };
    runner
        .cache()
        .write(PipelinePhase::Questions, &phase1)
        .unwrap();

    let res = runner.phase_2_cluster_atlas().await.unwrap();
    assert!(res.cache_updated);
    assert!(res.run_path.exists());
    // 4 claim sketches (2 per chapter × 2 chapters) + 2
    // questions. Every ref lands either in a cluster or in
    // the unclustered noise pile.
    let total: usize = res
        .output
        .clusters
        .iter()
        .map(|c| c.refs.len())
        .sum::<usize>()
        + res.output.unclustered.len();
    assert_eq!(total, 6);
    // Every produced cluster carries its facet tag.
    for cluster in &res.output.clusters {
        assert!(matches!(cluster.facet, Facet::Claim | Facet::Question));
    }
}

#[tokio::test]
async fn phase_1a_returns_none_for_pipelines_with_no_seed_strategy() {
    // LiteraryPipeline (v1) inherits SeedStrategy::None via the
    // trait default; phase_1a_extract_seed returns None without
    // running chat or writing cache.
    let dir = tempdir().unwrap();
    let runner = runner_under_test(dir.path()); // v1 LiteraryPipeline
    let ctx = CorpusContext {
        chapters: vec![chapter("ch_01", "Chapter 1", "text body")],
        chunks: vec![],
        chapter_titles: vec!["Chapter 1".into()],
    };
    let seed = runner
        .phase_1a_extract_seed("test_corpus", &ctx, false)
        .await
        .unwrap();
    assert!(seed.is_none());
    let cached: Option<crate::enrichment::pipeline::atlas::SeedEntities> =
        runner.cache().read(PipelinePhase::SeedExtraction).unwrap();
    assert!(cached.is_none());
}

#[tokio::test]
async fn phase_1a_llm_path_writes_cache_and_threads_seed_into_phase_1() {
    use crate::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;
    let dir = tempdir().unwrap();
    let cache = PhaseCache::new(dir.path().join("cache"));
    let runs = RunOutputWriter::new(dir.path().join("runs"));

    let saw_seed_block = Arc::new(std::sync::Mutex::new(false));
    let saw_seed_block_c = saw_seed_block.clone();
    let chat: InferenceFn = Arc::new(move |prompt: &ChatPrompt, _max_tokens: Option<u32>| {
        let is_seed = prompt.system.contains("seed entity list");
        let saw = saw_seed_block_c.clone();
        let body = if is_seed {
            r#"{"entries":[{"canonical_name":"Alyosha","aliases":["Alyoshka"],"entity_type":"person","description":"Youngest Karamazov."}]}"#
                    .to_string()
        } else {
            if prompt.user.contains("Known canonical names") {
                *saw.lock().unwrap() = true;
            }
            r#"{"section_id":"ch_01","entities_introduced":[{"canonical_name":"Alyosha","entity_type":"person"}],"questions_raised":[{"content":"?"}]}"#
                    .to_string()
        };
        Box::pin(async move { Ok(body) })
    });

    let runner = PhaseRunner::new(
        Arc::new(LiteraryAtlasPipeline::new()),
        alphabet_embed(),
        chat,
        cache,
        runs,
        dir.path().join("exemplars"),
    );

    let ctx = CorpusContext {
        chapters: vec![chapter("ch_01", "Chapter 1", "body with Alyosha")],
        chunks: vec![],
        chapter_titles: vec!["Chapter 1".into()],
    };

    let seed = runner
        .phase_1a_extract_seed("test_corpus", &ctx, false)
        .await
        .unwrap()
        .expect("Llm strategy returns Some");
    assert_eq!(seed.entries.len(), 1);
    assert_eq!(seed.entries[0].canonical_name, "Alyosha");

    // Cache-hit short-circuit: second call no chat.
    let seed2 = runner
        .phase_1a_extract_seed("test_corpus", &ctx, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seed2.entries[0].canonical_name, "Alyosha");

    // Phase 1 must carry the seed block.
    let _ = runner
        .phase_1_extract_questions(&ctx.chapters, &ChapterSelection::Full, |_| {})
        .await
        .unwrap();
    assert!(
        *saw_seed_block.lock().unwrap(),
        "phase 1 prompt must include the Known canonical names block \
             when Stage 1a seed cache is populated"
    );
}

#[tokio::test]
async fn phase_1a_force_refresh_recomputes_even_when_cache_present() {
    use crate::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;
    let dir = tempdir().unwrap();
    let cache = PhaseCache::new(dir.path().join("cache"));
    let runs = RunOutputWriter::new(dir.path().join("runs"));

    let chat_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let chat_calls_c = chat_calls.clone();
    let chat: InferenceFn = Arc::new(move |_prompt: &ChatPrompt, _max_tokens: Option<u32>| {
        chat_calls_c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let body =
            r#"{"entries":[{"canonical_name":"X","entity_type":"person","description":"x"}]}"#
                .to_string();
        Box::pin(async move { Ok(body) })
    });

    let runner = PhaseRunner::new(
        Arc::new(LiteraryAtlasPipeline::new()),
        alphabet_embed(),
        chat,
        cache,
        runs,
        dir.path().join("exemplars"),
    );

    let ctx = CorpusContext {
        chapters: vec![chapter("ch_01", "Chapter 1", "body")],
        chunks: vec![],
        chapter_titles: vec!["Chapter 1".into()],
    };

    let _ = runner
        .phase_1a_extract_seed("c", &ctx, false)
        .await
        .unwrap();
    let _ = runner
        .phase_1a_extract_seed("c", &ctx, false)
        .await
        .unwrap();
    assert_eq!(chat_calls.load(std::sync::atomic::Ordering::Relaxed), 1);

    let _ = runner.phase_1a_extract_seed("c", &ctx, true).await.unwrap();
    assert_eq!(chat_calls.load(std::sync::atomic::Ordering::Relaxed), 2);
}

#[tokio::test]
async fn phase_3_requires_phase_2_cache() {
    let dir = tempdir().unwrap();
    let runner = multiphase_runner(dir.path());
    let ctx = synth_context();
    let err = runner.phase_3_name_concerns(&ctx).await.unwrap_err();
    assert!(format!("{err}").contains("cache is missing"));
}

#[tokio::test]
async fn phase_4_clusters_chunks() {
    let dir = tempdir().unwrap();
    let runner = multiphase_runner(dir.path());
    let ctx = synth_context();
    let res = runner.phase_4_cluster_chunks(&ctx).await.unwrap();
    assert!(res.cache_updated);
    // Every non-noise cluster should carry a centroid.
    for c in &res.output.clusters {
        if !c.noise {
            assert!(
                !c.centroid.is_empty(),
                "non-noise cluster {} missing centroid",
                c.id
            );
        }
    }
}

#[tokio::test]
async fn cascade_from_questions_runs_all_phases() {
    let dir = tempdir().unwrap();
    let runner = multiphase_runner(dir.path());
    let ctx = synth_context();
    let res = runner
        .cascade(PipelinePhase::Questions, &ctx, Some(ChapterSelection::Full))
        .await
        .unwrap();
    // We expect 7 non-Ingest steps.
    assert_eq!(res.steps.len(), 7, "cascade should produce 7 steps");
    // Every phase cache should be populated.
    for phase in [
        PipelinePhase::Questions,
        PipelinePhase::QuestionClusters,
        PipelinePhase::Concerns,
        PipelinePhase::ChunkClusters,
        PipelinePhase::Positions,
        PipelinePhase::Tensions,
        PipelinePhase::Gaps,
    ] {
        let path = runner.cache().path(phase);
        assert!(path.exists(), "cache for {:?} not written", phase);
    }
}

#[tokio::test]
async fn cascade_from_positions_only_reruns_downstream() {
    let dir = tempdir().unwrap();
    let runner = multiphase_runner(dir.path());
    let ctx = synth_context();
    // Seed phases 1-4 first.
    runner
        .phase_1_extract_questions(&ctx.chapters, &ChapterSelection::Full, |_| {})
        .await
        .unwrap();
    runner.phase_2_cluster_questions().await.unwrap();
    runner.phase_3_name_concerns(&ctx).await.unwrap();
    runner.phase_4_cluster_chunks(&ctx).await.unwrap();

    let res = runner
        .cascade(PipelinePhase::Positions, &ctx, None)
        .await
        .unwrap();
    // Positions, Tensions, Gaps — three steps.
    assert_eq!(res.steps.len(), 3);
    for step in &res.steps {
        match step {
            CascadeStep::Phase5(_) | CascadeStep::Phase6(_) | CascadeStep::Phase7(_) => {}
            other => panic!("unexpected cascade step: {other:?}"),
        }
    }
}
