//! ScienceDomain — stub for scientific corpus enrichment.

use super::super::domain::*;

pub struct ScienceDomain;

impl Domain for ScienceDomain {
    fn id(&self) -> &str {
        "science"
    }
    fn name(&self) -> &str {
        "Science"
    }
    fn position_statuses(&self) -> &PositionStatusVocab {
        todo!("ScienceDomain")
    }
    fn question_types(&self) -> &[QuestionType] {
        todo!("ScienceDomain")
    }
    fn overview_filter(&self) -> ChunkFilter {
        todo!("ScienceDomain")
    }
    fn skeleton_extraction_prompt(&self, _chunks: &[&Chunk]) -> String {
        todo!("ScienceDomain")
    }
    fn cluster_labeling_prompt(&self, _chunks: &[&Chunk]) -> String {
        todo!("ScienceDomain")
    }
    fn fault_line_detection_prompt(
        &self,
        _a: &[&Chunk],
        _b: &[&Chunk],
        _pa: &str,
        _pb: &str,
    ) -> String {
        todo!("ScienceDomain")
    }
    fn open_question_prompt(&self, _chunks: &[&Chunk]) -> String {
        todo!("ScienceDomain")
    }
    fn clustering_config(&self) -> ClusteringConfig {
        todo!("ScienceDomain")
    }
    fn alignment_config(&self) -> AlignmentConfig {
        todo!("ScienceDomain")
    }
    fn fault_line_config(&self) -> FaultLineConfig {
        todo!("ScienceDomain")
    }
    fn skeleton_storage(&self) -> SkeletonStorage {
        todo!("ScienceDomain")
    }
}
