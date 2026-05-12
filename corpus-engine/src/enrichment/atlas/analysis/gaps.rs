//! Deterministic gap detection over the resolved atlas.
//!
//! A "gap" is a structural missing-piece in the atlas that the
//! corpus could plausibly have supplied but didn't — or that the
//! enrichment pipeline failed to surface. Landing 3 ships three
//! deterministic kinds; future landings can extend with LLM-driven
//! thematic gap detection (passages that raise a question the atlas
//! has no atom for).
//!
//! All gaps carry chunk evidence so the brief assembler can point a
//! reader at the relevant passage. `significance` is a 0.0-1.0
//! score the detector assigns based on how load-bearing the missing
//! piece is — a transition without a trigger event on a trajectory
//! with many states is more significant than one on a two-state
//! chain.
//!
//! Design note: gaps are persistent atlas artefacts (like atoms +
//! edges) but live in their own file `atlas/gaps.json` so they can
//! be regenerated without touching the atom set. A traversal engine
//! that asks "what's missing around Alyosha's arc?" reads gaps.json
//! alongside the atoms/edges files.

use serde::{Deserialize, Serialize};

use super::super::atoms::{AtomId, ChunkRef, Claim, Question, ResolutionStatus, State};
use super::super::edges::{Edge, EdgeType};

/// Discriminator for the gap kinds this pass can detect.
/// Serialises as a snake_case string tag so a consumer that doesn't
/// recognise a newer variant fails loudly rather than silently
/// dropping it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// A Transition edge connects two states but carries no
    /// `trigger_event`. The corpus shows a character change but
    /// doesn't name what caused it — a classic novelistic ellipsis.
    TransitionWithoutTrigger,
    /// A Claim atom has neither an inbound `Grounds` edge nor any
    /// own `evidence` chunks. The atlas asserts something without
    /// the atlas-visible grounding.
    UngroundedClaim,
    /// A Question atom exits Phase 3b with `resolution_status: Open`.
    /// Questions the corpus raises without answering are first-class
    /// atlas artefacts (spec §2.4) and often the most interesting
    /// queries land on exactly these.
    OpenQuestion,
}

/// Single detected gap. `referenced_atoms` carries the atoms whose
/// relationship made the gap detectable (the transition edge's
/// source + target, the claim's id, the question's id). Evidence
/// chunks let the brief assembler quote the surrounding text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gap {
    pub id: String,
    pub kind: GapKind,
    /// Human-readable summary of what's missing. Short — the detail
    /// is in the referenced atoms + chunk evidence.
    pub description: String,
    pub referenced_atoms: Vec<AtomId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ChunkRef>,
    /// 0.0-1.0 — how load-bearing this missing piece is. Surfaces
    /// in the brief assembler so low-significance gaps stay out of
    /// a headline brief unless the query explicitly asks about
    /// gaps.
    pub significance: f32,
}

/// Top-level gaps file written by Phase 7.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapsOutput {
    pub schema_version: String,
    pub gaps: Vec<Gap>,
}

impl GapsOutput {
    pub const SCHEMA_VERSION: &'static str = "2.0";

    pub fn new(gaps: Vec<Gap>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION.to_string(),
            gaps,
        }
    }
}

/// Inputs the detector reads. Passing a struct keeps the public
/// signature stable as new detectors land.
#[derive(Debug, Clone, Copy)]
pub struct GapDetectionInput<'a> {
    pub claims: &'a [Claim],
    pub states: &'a [State],
    pub questions: &'a [Question],
    pub edges: &'a [Edge],
}

/// Run every deterministic detector and return the union of their
/// findings, id-numbered consistently across kinds. Detectors are
/// pure: same inputs → same gaps → same ids, which matters for
/// incremental re-runs (an operator re-running Phase 7 after a
/// spec-version bump should see stable ids on unchanged inputs).
pub fn detect_deterministic_gaps(input: GapDetectionInput<'_>) -> Vec<Gap> {
    let mut out = Vec::new();
    out.extend(detect_transitions_without_trigger(input));
    out.extend(detect_ungrounded_claims(input));
    out.extend(detect_open_questions(input));
    // Stamp sequential ids so the set is deterministic.
    for (i, g) in out.iter_mut().enumerate() {
        g.id = format!("gap-{:04}", i + 1);
    }
    out
}

/// Kind 1: Transition edges without a `trigger_event`. Significance
/// scales with the owner's trajectory length — a missing trigger on
/// a rich multi-state chain is more load-bearing than one on a pair.
fn detect_transitions_without_trigger(input: GapDetectionInput<'_>) -> Vec<Gap> {
    // Count states per entity so a missing trigger on an
    // 8-state trajectory outranks one on a 2-state trajectory.
    let mut states_per_owner: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for s in input.states {
        *states_per_owner
            .entry(s.entity_id.as_str().to_string())
            .or_insert(0) += 1;
    }

    let mut gaps = Vec::new();
    for edge in input.edges {
        if edge.edge_type != EdgeType::Transition {
            continue;
        }
        if edge.trigger_event.is_some() {
            continue;
        }
        // Find the owner of the source state for the significance
        // score. Both endpoints are State atoms; either shares an
        // `entity_id` by construction of Phase 3b.
        let owner = input
            .states
            .iter()
            .find(|s| s.id == edge.source)
            .map(|s| s.entity_id.as_str().to_string());
        let significance = owner
            .as_ref()
            .and_then(|o| states_per_owner.get(o))
            .map(|n| (*n as f32 / 10.0).min(1.0))
            .unwrap_or(0.3);
        gaps.push(Gap {
            id: String::new(), // filled in by caller
            kind: GapKind::TransitionWithoutTrigger,
            description: format!(
                "State transition {} → {} has no trigger event",
                edge.source.as_str(),
                edge.target.as_str()
            ),
            referenced_atoms: vec![edge.source.clone(), edge.target.clone()],
            evidence: edge.evidence.clone(),
            significance,
        });
    }
    gaps
}

/// Kind 2: Claim atoms with no inbound Grounds edge AND no own
/// evidence chunks. A claim that asserts something with no
/// atlas-visible grounding is either a structural gap (the corpus
/// asserted without showing) or an extraction gap (Phase 3b failed
/// to link the grounding Event). Either way the brief assembler
/// benefits from knowing.
fn detect_ungrounded_claims(input: GapDetectionInput<'_>) -> Vec<Gap> {
    use std::collections::HashSet;
    let grounded_claim_ids: HashSet<&str> = input
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Grounds)
        .map(|e| e.target.as_str())
        .collect();

    let mut gaps = Vec::new();
    for claim in input.claims {
        if claim.evidence.is_empty() && !grounded_claim_ids.contains(claim.id.as_str()) {
            // Significance scales with the claim's own extraction
            // confidence — a high-confidence ungrounded claim is a
            // louder gap than a hedged one. `None` confidence (the
            // deterministic resolver didn't score this claim) rings
            // the middle bell at 0.5: a partial signal rather than
            // a loud or quiet one.
            let raw = claim.confidence.unwrap_or(0.5);
            let significance = raw.clamp(0.0, 1.0) * 0.7;
            gaps.push(Gap {
                id: String::new(),
                kind: GapKind::UngroundedClaim,
                description: format!("Claim without grounding evidence: {}", claim.content),
                referenced_atoms: vec![claim.id.clone()],
                evidence: Vec::new(),
                significance,
            });
        }
    }
    gaps
}

/// Kind 3: Questions with `resolution_status: Open` at the end of
/// Phase 3b. These are first-class atlas artefacts — a Brothers
/// Karamazov query like "does Ivan's rebellion have an answer?"
/// lands directly on one.
fn detect_open_questions(input: GapDetectionInput<'_>) -> Vec<Gap> {
    let mut gaps = Vec::new();
    for q in input.questions {
        if matches!(q.resolution_status, ResolutionStatus::Open) {
            // Significance is high by default — an Open question
            // that the corpus goes out of its way to raise is
            // usually load-bearing.
            gaps.push(Gap {
                id: String::new(),
                kind: GapKind::OpenQuestion,
                description: format!("Unresolved question: {}", q.content),
                referenced_atoms: vec![q.id.clone()],
                evidence: q.raised_at.clone(),
                significance: 0.8,
            });
        }
    }
    gaps
}

#[cfg(test)]
mod tests {
    use crate::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EpistemicStatus, QuestionType, StateType,
    };
    use super::super::super::atoms::{
        AtomId, ChunkRef, Claim, Question, ResolutionStatus, SectionRange, State,
    };
    use super::super::super::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
    use super::*;

    fn claim(id: u32, content: &str, has_evidence: bool) -> Claim {
        Claim {
            id: AtomId::from_raw(format!("claim-{id:04}")),
            content: content.into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: if has_evidence {
                vec![ChunkRef::new("sec_0001", None)]
            } else {
                Vec::new()
            },
            attributed_to: None,
            confidence: Some(0.9),
            anchor: None,
            enrichment_depth: EnrichmentDepth::Extracted,
            quotable_excerpt: None,
        }
    }

    fn state(id: u32, owner: u32) -> State {
        State {
            id: AtomId::from_raw(format!("state-{id:04}")),
            entity_id: AtomId::from_raw(format!("entity-{owner:04}")),
            label: format!("state {id}"),
            state_type: StateType::Other("unknown".into()),
            evidence: Vec::new(),
            section_range: SectionRange {
                start: "sec_0001".into(),
                end: "sec_0001".into(),
            },
            confidence: Some(1.0),
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn question(id: u32, content: &str, status: ResolutionStatus) -> Question {
        Question {
            id: AtomId::from_raw(format!("question-{id:04}")),
            content: content.into(),
            question_type: QuestionType::Other("unknown".into()),
            addressed_by: Vec::new(),
            raised_at: vec![ChunkRef::new("sec_0001", None)],
            resolution_status: status,
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn transition_edge(src: u32, tgt: u32, trigger: Option<u32>) -> Edge {
        Edge {
            id: EdgeId::from_raw(format!("edge-{src:04}-{tgt:04}")),
            edge_type: EdgeType::Transition,
            source: AtomId::from_raw(format!("state-{src:04}")),
            target: AtomId::from_raw(format!("state-{tgt:04}")),
            evidence: Vec::new(),
            trigger_event: trigger.map(|t| AtomId::from_raw(format!("event-{t:04}"))),
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        }
    }

    fn grounds_edge(event: u32, claim: u32) -> Edge {
        Edge {
            id: EdgeId::from_raw(format!("grounds-{event:04}-{claim:04}")),
            edge_type: EdgeType::Grounds,
            source: AtomId::from_raw(format!("event-{event:04}")),
            target: AtomId::from_raw(format!("claim-{claim:04}")),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        }
    }

    #[test]
    fn detects_transition_without_trigger_event() {
        let states = vec![state(1, 1), state(2, 1)];
        let edges = vec![transition_edge(1, 2, None)];
        let gaps = detect_deterministic_gaps(GapDetectionInput {
            claims: &[],
            states: &states,
            questions: &[],
            edges: &edges,
        });
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].kind, GapKind::TransitionWithoutTrigger);
        assert_eq!(gaps[0].referenced_atoms.len(), 2);
        assert!(gaps[0].id.starts_with("gap-"));
    }

    #[test]
    fn skips_transitions_that_have_a_trigger_event() {
        let states = vec![state(1, 1), state(2, 1)];
        let edges = vec![transition_edge(1, 2, Some(99))];
        let gaps = detect_deterministic_gaps(GapDetectionInput {
            claims: &[],
            states: &states,
            questions: &[],
            edges: &edges,
        });
        assert!(gaps.is_empty());
    }

    #[test]
    fn detects_ungrounded_claims_with_no_evidence_and_no_grounds_edge() {
        let claims = vec![claim(1, "ungrounded", false), claim(2, "evidenced", true)];
        let gaps = detect_deterministic_gaps(GapDetectionInput {
            claims: &claims,
            states: &[],
            questions: &[],
            edges: &[],
        });
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].kind, GapKind::UngroundedClaim);
        assert!(gaps[0].description.contains("ungrounded"));
    }

    #[test]
    fn grounds_edge_saves_a_claim_from_being_ungrounded() {
        let claims = vec![claim(1, "asserted", false)];
        let edges = vec![grounds_edge(42, 1)];
        let gaps = detect_deterministic_gaps(GapDetectionInput {
            claims: &claims,
            states: &[],
            questions: &[],
            edges: &edges,
        });
        assert!(gaps.is_empty(), "Grounds edge must rescue the claim");
    }

    #[test]
    fn detects_open_questions_only_not_resolved_or_contested() {
        let questions = vec![
            question(1, "Open one", ResolutionStatus::Open),
            question(
                2,
                "Resolved one",
                ResolutionStatus::Resolved {
                    claim_id: AtomId::from_raw("claim-0042"),
                },
            ),
            question(
                3,
                "Contested one",
                ResolutionStatus::Contested {
                    claim_ids: vec![AtomId::from_raw("claim-0001")],
                },
            ),
            question(4, "Dissolved one", ResolutionStatus::Dissolved),
        ];
        let gaps = detect_deterministic_gaps(GapDetectionInput {
            claims: &[],
            states: &[],
            questions: &questions,
            edges: &[],
        });
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].kind, GapKind::OpenQuestion);
        assert!(gaps[0].description.contains("Open one"));
    }

    #[test]
    fn gaps_get_sequential_ids_across_kinds() {
        let claims = vec![claim(1, "ungrounded", false)];
        let states = vec![state(1, 1), state(2, 1)];
        let questions = vec![question(1, "open", ResolutionStatus::Open)];
        let edges = vec![transition_edge(1, 2, None)];
        let gaps = detect_deterministic_gaps(GapDetectionInput {
            claims: &claims,
            states: &states,
            questions: &questions,
            edges: &edges,
        });
        assert_eq!(gaps.len(), 3);
        let ids: Vec<&str> = gaps.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["gap-0001", "gap-0002", "gap-0003"]);
    }

    #[test]
    fn transition_significance_scales_with_trajectory_length() {
        // Two trajectories: one with 8 states (missing trigger is
        // high-significance), one with 2 (low). Both missing triggers.
        let mut states: Vec<State> = (1..=8).map(|i| state(i, 1)).collect(); // owner entity-0001
        states.extend((9..=10).map(|i| state(i, 2))); // owner entity-0002
        let edges = vec![
            transition_edge(1, 2, None),  // on the long trajectory
            transition_edge(9, 10, None), // on the short one
        ];
        let gaps = detect_deterministic_gaps(GapDetectionInput {
            claims: &[],
            states: &states,
            questions: &[],
            edges: &edges,
        });
        assert_eq!(gaps.len(), 2);
        let long = gaps
            .iter()
            .find(|g| g.referenced_atoms.contains(&AtomId::from_raw("state-0001")))
            .unwrap();
        let short = gaps
            .iter()
            .find(|g| g.referenced_atoms.contains(&AtomId::from_raw("state-0009")))
            .unwrap();
        assert!(
            long.significance > short.significance,
            "8-state trajectory must outrank 2-state"
        );
    }
}
