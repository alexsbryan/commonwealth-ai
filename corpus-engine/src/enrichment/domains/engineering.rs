// SPDX-License-Identifier: AGPL-3.0-or-later
//! EngineeringDomain — stub for technical-tradeoff enrichment.
//!
//! The Stack Exchange knowledge layer (multi-answer threads from SO,
//! Software Engineering SE, DBA SE, etc.) is the canonical input.
//! Epistemic vocabulary mapping (full prompt set lives in a follow-on
//! pass — `enabled = false` on the recipe until those land):
//!
//! - **Approach**     → Position    (a concrete way to solve the problem)
//! - **Trade-off**    → FaultLine   (what each approach gives up)
//! - **Context**      → Configuration (when one approach dominates)
//! - **Assumption**   → Framing     (what each approach takes for granted)
//!
//! Registered alongside the other domain stubs in
//! [`crate::enrichment::domain_registry::DomainRegistry::builtin`] so
//! recipe parsing accepts `domain = "engineering"` without panicking
//! on lookup. Calling enrichment with this domain will `todo!()` —
//! that's intentional until the prompt set is tuned.

use super::super::domain::*;

pub struct EngineeringDomain;

impl Domain for EngineeringDomain {
    fn id(&self) -> &str {
        "engineering"
    }
    fn name(&self) -> &str {
        "Engineering"
    }
    fn position_statuses(&self) -> &PositionStatusVocab {
        todo!("EngineeringDomain")
    }
    fn question_types(&self) -> &[QuestionType] {
        todo!("EngineeringDomain")
    }
    fn overview_filter(&self) -> ChunkFilter {
        todo!("EngineeringDomain")
    }
    fn skeleton_extraction_prompt(&self, _chunks: &[&Chunk]) -> String {
        todo!("EngineeringDomain")
    }
    fn cluster_labeling_prompt(&self, _chunks: &[&Chunk]) -> String {
        todo!("EngineeringDomain")
    }
    fn fault_line_detection_prompt(
        &self,
        _a: &[&Chunk],
        _b: &[&Chunk],
        _pa: &str,
        _pb: &str,
    ) -> String {
        todo!("EngineeringDomain")
    }
    fn open_question_prompt(&self, _chunks: &[&Chunk]) -> String {
        todo!("EngineeringDomain")
    }
    fn clustering_config(&self) -> ClusteringConfig {
        todo!("EngineeringDomain")
    }
    fn alignment_config(&self) -> AlignmentConfig {
        todo!("EngineeringDomain")
    }
    fn fault_line_config(&self) -> FaultLineConfig {
        todo!("EngineeringDomain")
    }
    fn skeleton_storage(&self) -> SkeletonStorage {
        todo!("EngineeringDomain")
    }
}
