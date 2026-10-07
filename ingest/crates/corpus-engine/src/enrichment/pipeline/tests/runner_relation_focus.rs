// SPDX-License-Identifier: AGPL-3.0-or-later
//! The focused per-declared-relation pass (`pipelines/relation_focus.rs`),
//! driven through the Phase-1 loop, split from `tests/runner.rs` by subject.

use super::*;
use crate::enrichment::ontology::{OntologyPolicies, OntologyTypeDecl, TypeKind};
use crate::enrichment::pipeline::atlas::RelationSketch;
use crate::enrichment::pipeline::atom_normalizer::AtomPostProcessorRegistry;
use crate::enrichment::pipeline::pipelines::configurable_atlas::CustomOntology;
use crate::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;
use crate::enrichment::pipeline::pipelines::relation_focus::{RelationFocus, SCHEMA_NAME};
use std::sync::Mutex;

const REL: &str = "holds_coins_of";
const BODY: &str =
    "The Kirkoswald hoard held coins of Eoforwic, Lundenwic pennies and a Hamwic sceatta.";

/// The shipped numismatics declaration plus a `hoard` and the ft-ans-dev-b
/// relation `holds_coins_of` (hoard -> `to`). `to = None` declares one end.
fn hoard_policies(to: Option<&str>) -> OntologyPolicies {
    let mut p = crate::recipe_templates::numismatics_policies();
    p.shape.types.push(OntologyTypeDecl {
        name: "hoard".into(),
        kind: TypeKind::Entity,
        description: "A group of coins buried together.".into(),
        ..Default::default()
    });
    p.shape.types.push(OntologyTypeDecl {
        name: REL.into(),
        kind: TypeKind::Relation,
        from: Some("hoard".into()),
        to: to.map(str::to_string),
        description: "The hoard contains coins struck at this mint.".into(),
        ..Default::default()
    });
    p
}

/// The joint Phase-1 answer: a hoard, a mint, and ONE `holds_coins_of`.
const PHASE1_RESPONSE: &str = r#"{
  "section_id": "ignored",
  "entities_introduced": [
    {"canonical_name": "Kirkoswald hoard", "entity_type": "hoard",
     "description": "A hoard.", "anchor": "Kirkoswald hoard"},
    {"canonical_name": "Eoforwic", "entity_type": "mint",
     "description": "A mint.", "anchor": "Eoforwic"}
  ],
  "relations_introduced": [
    {"participants": ["Kirkoswald hoard", "Eoforwic"], "label": "holds coins of",
     "anchor": "coins of Eoforwic", "relation_type": "holds_coins_of"}
  ],
  "questions_raised": [{"content": "Which mints does the hoard hold?", "anchor": "Kirkoswald hoard"}]
}"#;

/// The focused answer: the joint pass's mint again, two new ones, and one of
/// them repeated under another case — two duplicates to drop.
const FOCUS_RESPONSE: &str = r#"{"items": [
  {"name": "Eoforwic", "anchor": "coins of Eoforwic"},
  {"name": "Lundenwic", "anchor": "Lundenwic pennies"},
  {"name": "Hamwic", "anchor": "a Hamwic sceatta"},
  {"name": " lundenwic ", "anchor": "Lundenwic pennies"}
]}"#;

/// Every prompt the chat was handed, in order.
type Seen = Arc<Mutex<Vec<ChatPrompt>>>;

fn focus_chat(seen: Seen) -> InferenceFn {
    Arc::new(move |prompt: &ChatPrompt, _max: Option<u32>| {
        seen.lock().unwrap().push(prompt.clone());
        let body = if prompt.response_schema_name.as_deref() == Some(SCHEMA_NAME) {
            FOCUS_RESPONSE
        } else {
            PHASE1_RESPONSE
        };
        Box::pin(async move { Ok(body.to_string()) })
    })
}

fn focus_calls(seen: &Seen) -> Vec<ChatPrompt> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|p| p.response_schema_name.as_deref() == Some(SCHEMA_NAME))
        .cloned()
        .collect()
}

fn focus_runner(root: &Path, policies: &OntologyPolicies, chat: InferenceFn) -> PhaseRunner {
    let genre = CustomOntology::from_policies("ans", policies);
    PhaseRunner::new(
        Arc::new(LiteraryAtlasPipeline::with_genre(Arc::new(genre))),
        alphabet_embed(),
        chat,
        PhaseCache::new(root.join("cache")),
        RunOutputWriter::new(root.join("runs")),
        root.join("exemplars"),
    )
}

/// `(from, to)` folded, for every `holds_coins_of` sketch, in order.
fn declared_pairs(rels: &[RelationSketch]) -> Vec<(String, String)> {
    rels.iter()
        .filter(|r| r.relation_type.as_deref() == Some(REL))
        .map(|r| {
            (
                r.participants[0].to_lowercase(),
                r.participants[1].to_lowercase(),
            )
        })
        .collect()
}

fn pairs(to: &[&str]) -> Vec<(String, String)> {
    to.iter()
        .map(|t| ("kirkoswald hoard".to_string(), t.to_string()))
        .collect()
}

/// (1) A declared relation with both ends and one `hoard` in the section: one
/// focused call (the mint entity asks nothing), its items land in the cached
/// extraction resolve reads, and the joint pass's own mint plus the case-folded
/// repeat are dropped.
#[tokio::test]
async fn focused_pass_appends_declared_relations_deduped() {
    let dir = tempdir().unwrap();
    let seen: Seen = Arc::default();
    let runner = focus_runner(
        dir.path(),
        &hoard_policies(Some("mint")),
        focus_chat(seen.clone()),
    );
    let chapters = vec![chapter("sec_00008", "Hoard", BODY)];
    runner
        .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
        .await
        .expect("phase 1 runs");

    let calls = focus_calls(&seen);
    assert_eq!(
        calls.len(),
        1,
        "one relation x one hoard; the mint asks nothing"
    );
    let call = &calls[0];
    assert_eq!(
        call.system,
        "You read one section of a document and list relations of one declared type.\n\n\
         Relation `holds_coins_of`: from a hoard to a mint. The hoard contains coins struck at this mint.\n\n\
         For the hoard named below, list EVERY mint the section states it is in this relation with, \
         one item each, with the shortest quote that states it. Only what the text states; an empty \
         list is a correct answer.",
        "the harness's prompt, filled from the declaration alone"
    );
    assert!(call.user.starts_with("The hoard: Kirkoswald hoard\n\n"));
    assert!(
        call.user.contains("# Chapter to analyse"),
        "the section is Phase 1's own body"
    );
    assert_eq!(
        call.phase_id.as_deref(),
        Some("phase1"),
        "routed and sampled as Phase 1"
    );

    let cached: Phase1Output = runner
        .cache()
        .read(PipelinePhase::Questions)
        .unwrap()
        .expect("a full run writes the cache");
    let sx = cached.questions_by_chapter[0]
        .section_extraction
        .as_ref()
        .unwrap();
    assert_eq!(
        declared_pairs(&sx.relations_introduced),
        pairs(&["eoforwic", "lundenwic", "hamwic"]),
        "joint relation kept, two appended, the joint mint and the repeat dropped"
    );
    let appended = &sx.relations_introduced[1];
    assert_eq!(appended.label, REL);
    assert!(!appended.anchor.is_empty(), "the quote is the anchor");
}

/// (2) A corpus that declares no relation with both ends makes no focused
/// call, and its extraction is byte-identical to the Phase-1 parse alone.
#[tokio::test]
async fn undeclared_relation_ends_make_zero_extra_calls_and_leave_extraction_unchanged() {
    for (label, policies) in [
        (
            "no relation declared",
            crate::recipe_templates::numismatics_policies(),
        ),
        ("one end declared", hoard_policies(None)),
    ] {
        let dir = tempdir().unwrap();
        let seen: Seen = Arc::default();
        let runner = focus_runner(dir.path(), &policies, focus_chat(seen.clone()));
        let chapters = vec![chapter("sec_00008", "Hoard", BODY)];
        let result = runner
            .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
            .await
            .expect("phase 1 runs");

        assert_eq!(seen.lock().unwrap().len(), 1, "{label}: Phase 1 only");
        let mut expected = runner
            .pipeline()
            .parse_phase1(PHASE1_RESPONSE)
            .unwrap()
            .section_extraction
            .unwrap();
        expected.section_id = "sec_00008".into();
        AtomPostProcessorRegistry::default_chain().process(&mut expected, &chapters[0].text);
        let got = result.output.questions_by_chapter[0]
            .section_extraction
            .as_ref()
            .unwrap();
        assert_eq!(
            serde_json::to_string(got).unwrap(),
            serde_json::to_string(&expected).unwrap(),
            "{label}: extraction unchanged"
        );
    }
}

/// (3) Resume after a crash: the checkpoint holds the AUGMENTED section, so
/// the resumed run neither re-asks it nor appends to it, and the pass applied
/// again to its own output appends nothing.
#[tokio::test]
async fn resume_does_not_append_the_focused_relations_twice() {
    let dir = tempdir().unwrap();
    let checkpoint = dir.path().join("_phase1_checkpoint.jsonl");
    let policies = hoard_policies(Some("mint"));
    let seen: Seen = Arc::default();
    let runner = focus_runner(dir.path(), &policies, focus_chat(seen.clone()))
        .with_checkpoint_path(&checkpoint);
    let chapters = vec![
        chapter("sec_00008", "Hoard", BODY),
        chapter("sec_00009", "Hoard", BODY),
    ];
    // The first run "crashes" after one section.
    runner
        .phase_1_extract_questions(
            &chapters,
            &ChapterSelection::Subset(vec!["sec_00008".into()]),
            |_| {},
        )
        .await
        .unwrap();
    // `--resume`: skip what the checkpoint already holds.
    let done = checkpoint_processed_ids(&read_phase1_checkpoint(&checkpoint).unwrap());
    let rest: Vec<String> = chapters
        .iter()
        .map(|c| c.chapter_id.clone())
        .filter(|id| !done.contains(id))
        .collect();
    assert_eq!(rest, vec!["sec_00009".to_string()]);
    runner
        .phase_1_extract_questions(&chapters, &ChapterSelection::Subset(rest), |_| {})
        .await
        .unwrap();

    assert_eq!(focus_calls(&seen).len(), 2, "each section asked once");
    let (extracted, _) = collapse_phase1_checkpoint(read_phase1_checkpoint(&checkpoint).unwrap());
    assert_eq!(extracted.len(), 2);
    for q in &extracted {
        let sx = q.section_extraction.as_ref().unwrap();
        assert_eq!(
            declared_pairs(&sx.relations_introduced),
            pairs(&["eoforwic", "lundenwic", "hamwic"]),
            "{}: one copy of each",
            q.chapter_id
        );
    }

    let mut again = extracted[0].section_extraction.clone().unwrap();
    RelationFocus::from_policies(&policies)
        .apply(
            &focus_chat(seen.clone()),
            &ChatPrompt::new("", "section"),
            &mut again,
        )
        .await;
    assert_eq!(
        declared_pairs(&again.relations_introduced),
        pairs(&["eoforwic", "lundenwic", "hamwic"]),
        "re-applied to its own output, the pass appends nothing"
    );
}
