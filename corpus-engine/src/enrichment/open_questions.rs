//! Open question detection from cluster labels.
//!
//! Identifies clusters whose label indicates unresolved inquiry,
//! then runs inference to characterize the open question.

use crate::error::Result;
use crate::index::CorpusIndex;
use crate::types::InferenceFn;

use super::clustering::{ClusterResult, EnrichmentProgress};
use super::domain::Domain;

/// A detected open question in the field.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OpenQuestion {
    pub id: String,
    pub question: String,
    pub status: String,
    pub question_type: String,
    pub related_question_id: Option<String>,
    pub representative_chunk_ids: Vec<u64>,
    pub domain_id: String,
}

/// Detect open questions from clusters labeled as open inquiry.
pub async fn detect_open_questions(
    index: &CorpusIndex,
    clusters: &ClusterResult,
    inference: &InferenceFn,
    domain: &dyn Domain,
    progress: &(dyn Fn(EnrichmentProgress) + Send + Sync),
) -> Result<Vec<OpenQuestion>> {
    progress(EnrichmentProgress::Phase {
        phase: 6,
        name: "Identifying open questions",
        note: "",
    });

    let oq_clusters: Vec<_> = clusters
        .clusters
        .iter()
        .filter(|c| {
            c.label
                .as_ref()
                .map(|l| l.is_open_question)
                .unwrap_or(false)
        })
        .collect();

    let mut open_questions = Vec::new();

    for cluster in &oq_clusters {
        let chunks = index.get_chunks(&cluster.central_chunks).await?;
        let refs: Vec<&_> = chunks.iter().collect();
        let prompt = domain.open_question_prompt(&refs);

        let response = match (inference)(&prompt, None).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "Open question detection failed for cluster {}", cluster.id);
                continue;
            }
        };

        let parsed: serde_json::Value = match serde_json::from_str(&response) {
            Ok(v) => v,
            Err(_) => continue,
        };

        open_questions.push(OpenQuestion {
            id: format!("oq_{}", open_questions.len()),
            question: parsed["question"].as_str().unwrap_or_default().to_string(),
            status: "active_research".into(),
            question_type: "conceptual".into(),
            related_question_id: None,
            representative_chunk_ids: cluster.central_chunks.clone(),
            domain_id: domain.id().to_string(),
        });
    }

    Ok(open_questions)
}
