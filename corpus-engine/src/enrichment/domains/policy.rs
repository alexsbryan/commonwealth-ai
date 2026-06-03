//! PolicyDomain — stub for policy corpus enrichment (CRS, CBO, GAO).

use super::super::domain::*;

pub struct PolicyDomain;

impl Domain for PolicyDomain {
    fn id(&self) -> &str {
        "policy"
    }
    fn name(&self) -> &str {
        "Policy"
    }
    fn position_statuses(&self) -> &PositionStatusVocab {
        todo!("PolicyDomain")
    }
    fn question_types(&self) -> &[QuestionType] {
        todo!("PolicyDomain")
    }
    fn overview_filter(&self) -> ChunkFilter {
        todo!("PolicyDomain")
    }
    fn skeleton_extraction_prompt(&self, _chunks: &[&Chunk]) -> String {
        todo!("PolicyDomain")
    }
    fn cluster_labeling_prompt(&self, _chunks: &[&Chunk]) -> String {
        todo!("PolicyDomain")
    }
    fn fault_line_detection_prompt(
        &self,
        _a: &[&Chunk],
        _b: &[&Chunk],
        _pa: &str,
        _pb: &str,
    ) -> String {
        todo!("PolicyDomain")
    }
    fn open_question_prompt(&self, _chunks: &[&Chunk]) -> String {
        todo!("PolicyDomain")
    }
    fn clustering_config(&self) -> ClusteringConfig {
        todo!("PolicyDomain")
    }
    fn alignment_config(&self) -> AlignmentConfig {
        todo!("PolicyDomain")
    }
    fn fault_line_config(&self) -> FaultLineConfig {
        todo!("PolicyDomain")
    }
    fn skeleton_storage(&self) -> SkeletonStorage {
        todo!("PolicyDomain")
    }
}
