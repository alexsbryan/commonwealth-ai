//! LegalDomain — stub for legal corpus enrichment.

use super::super::domain::*;

pub struct LegalDomain;

impl Domain for LegalDomain {
    fn id(&self) -> &str { "legal" }
    fn name(&self) -> &str { "Legal" }
    fn position_statuses(&self) -> &PositionStatusVocab { todo!("LegalDomain") }
    fn question_types(&self) -> &[QuestionType] { todo!("LegalDomain") }
    fn overview_filter(&self) -> ChunkFilter { todo!("LegalDomain") }
    fn skeleton_extraction_prompt(&self, _chunks: &[&Chunk]) -> String { todo!("LegalDomain") }
    fn cluster_labeling_prompt(&self, _chunks: &[&Chunk]) -> String { todo!("LegalDomain") }
    fn fault_line_detection_prompt(&self, _a: &[&Chunk], _b: &[&Chunk], _pa: &str, _pb: &str) -> String { todo!("LegalDomain") }
    fn open_question_prompt(&self, _chunks: &[&Chunk]) -> String { todo!("LegalDomain") }
    fn clustering_config(&self) -> ClusteringConfig { todo!("LegalDomain") }
    fn alignment_config(&self) -> AlignmentConfig { todo!("LegalDomain") }
    fn fault_line_config(&self) -> FaultLineConfig { todo!("LegalDomain") }
    fn skeleton_storage(&self) -> SkeletonStorage { todo!("LegalDomain") }
}
