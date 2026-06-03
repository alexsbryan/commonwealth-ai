//! MultiDomain — routes to per-cluster domain implementations.
//! Constructor only; Domain methods use todo!() except id/name.

use std::collections::HashMap;
use std::sync::Arc;

use super::super::domain::*;

pub struct MultiDomain {
    #[allow(dead_code)]
    domains: HashMap<String, Arc<dyn Domain>>,
}

impl MultiDomain {
    pub fn wikipedia_default() -> Self {
        let mut domains: HashMap<String, Arc<dyn Domain>> = HashMap::new();
        domains.insert(
            "philosophy".into(),
            Arc::new(super::philosophy::PhilosophyDomain),
        );
        domains.insert("science".into(), Arc::new(super::science::ScienceDomain));
        domains.insert("policy".into(), Arc::new(super::policy::PolicyDomain));
        domains.insert("legal".into(), Arc::new(super::legal::LegalDomain));
        domains.insert(
            "community".into(),
            Arc::new(super::community::CommunityKnowledgeDomain),
        );
        Self { domains }
    }
}

impl Domain for MultiDomain {
    fn id(&self) -> &str {
        "multi"
    }
    fn name(&self) -> &str {
        "Multi-domain"
    }
    fn position_statuses(&self) -> &PositionStatusVocab {
        todo!("MultiDomain")
    }
    fn question_types(&self) -> &[QuestionType] {
        todo!("MultiDomain")
    }
    fn overview_filter(&self) -> ChunkFilter {
        todo!("MultiDomain")
    }
    fn skeleton_extraction_prompt(&self, _chunks: &[&Chunk]) -> String {
        todo!("MultiDomain")
    }
    fn cluster_labeling_prompt(&self, _chunks: &[&Chunk]) -> String {
        todo!("MultiDomain")
    }
    fn fault_line_detection_prompt(
        &self,
        _a: &[&Chunk],
        _b: &[&Chunk],
        _pa: &str,
        _pb: &str,
    ) -> String {
        todo!("MultiDomain")
    }
    fn open_question_prompt(&self, _chunks: &[&Chunk]) -> String {
        todo!("MultiDomain")
    }
    fn clustering_config(&self) -> ClusteringConfig {
        todo!("MultiDomain")
    }
    fn alignment_config(&self) -> AlignmentConfig {
        todo!("MultiDomain")
    }
    fn fault_line_config(&self) -> FaultLineConfig {
        todo!("MultiDomain")
    }
    fn skeleton_storage(&self) -> SkeletonStorage {
        SkeletonStorage::LanceOnly
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_domain_constructs() {
        let domain = MultiDomain::wikipedia_default();
        assert_eq!(domain.id(), "multi");
        assert_eq!(domain.name(), "Multi-domain");
    }

    #[test]
    fn multi_domain_is_object_safe() {
        let domain: Arc<dyn Domain> = Arc::new(MultiDomain::wikipedia_default());
        assert_eq!(domain.id(), "multi");
    }

    #[test]
    fn multi_domain_storage_is_lance_only() {
        assert!(matches!(
            MultiDomain::wikipedia_default().skeleton_storage(),
            SkeletonStorage::LanceOnly
        ));
    }
}
