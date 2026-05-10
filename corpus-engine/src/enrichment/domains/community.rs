//! CommunityKnowledgeDomain — stub for community knowledge corpus enrichment (Stack Exchange).

use super::super::domain::*;

pub struct CommunityKnowledgeDomain;

impl Domain for CommunityKnowledgeDomain {
    fn id(&self) -> &str { "community" }
    fn name(&self) -> &str { "Community Knowledge" }
    fn position_statuses(&self) -> &PositionStatusVocab { todo!("CommunityKnowledgeDomain") }
    fn question_types(&self) -> &[QuestionType] { todo!("CommunityKnowledgeDomain") }
    fn overview_filter(&self) -> ChunkFilter { todo!("CommunityKnowledgeDomain") }
    fn skeleton_extraction_prompt(&self, _chunks: &[&Chunk]) -> String { todo!("CommunityKnowledgeDomain") }
    fn cluster_labeling_prompt(&self, _chunks: &[&Chunk]) -> String { todo!("CommunityKnowledgeDomain") }
    fn fault_line_detection_prompt(&self, _a: &[&Chunk], _b: &[&Chunk], _pa: &str, _pb: &str) -> String { todo!("CommunityKnowledgeDomain") }
    fn open_question_prompt(&self, _chunks: &[&Chunk]) -> String { todo!("CommunityKnowledgeDomain") }
    fn clustering_config(&self) -> ClusteringConfig { todo!("CommunityKnowledgeDomain") }
    fn alignment_config(&self) -> AlignmentConfig { todo!("CommunityKnowledgeDomain") }
    fn fault_line_config(&self) -> FaultLineConfig { todo!("CommunityKnowledgeDomain") }
    fn skeleton_storage(&self) -> SkeletonStorage { todo!("CommunityKnowledgeDomain") }
}
