//! Mesh state translation — converts Commonwealth's internal state
//! into UI-friendly representations for Sovereign's frontend.

use commonwealth_api::state::AppState;
use commonwealth_core::mesh::NodeStatus;

use crate::types::*;

/// Snapshot of the mesh state, rendered for UI consumption.
#[derive(Debug, Clone)]
pub struct MeshState {
    pub status: MeshStatus,
    pub members: Vec<MeshMember>,
    pub corpora: Vec<MeshCorpus>,
    pub contribution: Option<ContributionSummary>,
}

impl MeshState {
    /// Build a UI-friendly mesh state from the Commonwealth AppState.
    pub async fn from_app_state(app_state: &AppState) -> Self {
        let mesh = app_state.inner.mesh.read().await;
        let knowledge_plan = app_state.inner.knowledge_plan.read().await;

        let self_node_id = app_state.inner.self_node_id;

        // Members.
        let mut members: Vec<MeshMember> = mesh
            .members
            .values()
            .map(|m| {
                let is_self = m.node_id == self_node_id;
                MeshMember {
                    name: m.name.clone(),
                    node_id: m.node_id.to_string(),
                    is_self,
                    status: match m.status {
                        NodeStatus::Online => MemberStatus::Online,
                        NodeStatus::Busy => MemberStatus::Busy,
                        NodeStatus::Away => MemberStatus::Away,
                        NodeStatus::Offline => MemberStatus::Offline,
                    },
                    contribution_level: 0, // Populated from ledger
                    contribution_label: String::new(),
                }
            })
            .collect();
        members.sort_by(|a, b| b.is_self.cmp(&a.is_self)); // Self first

        let online_count = members
            .iter()
            .filter(|m| matches!(m.status, MemberStatus::Online | MemberStatus::Busy))
            .count();

        // Knowledge corpora from the shard plan.
        let mut corpus_ids: Vec<String> = knowledge_plan
            .assignments
            .iter()
            .map(|a| a.corpus_id.clone())
            .collect();
        corpus_ids.sort();
        corpus_ids.dedup();

        let corpora: Vec<MeshCorpus> = corpus_ids
            .iter()
            .map(|id| MeshCorpus {
                id: id.clone(),
                name: humanize_corpus_id(id),
                description: String::new(),
                article_count: String::new(),
                download_size: String::new(),
                status: CorpusStatus::Installed,
            })
            .collect();

        let status = MeshStatus {
            name: mesh.name.clone(),
            members_online: online_count,
            members_total: mesh.members.len(),
            model_name: None, // Populated when inference plan active
            knowledge_corpora: corpus_ids,
            is_connected: true,
        };

        Self {
            status,
            members,
            corpora,
            contribution: None, // Populated from ledger
        }
    }
}

fn humanize_corpus_id(id: &str) -> String {
    match id {
        "wikipedia" => "Wikipedia".to_string(),
        "stackexchange" => "Stack Exchange".to_string(),
        "openalex" => "OpenAlex".to_string(),
        "gutenberg" => "Project Gutenberg".to_string(),
        "sep" => "Stanford Encyclopedia of Philosophy".to_string(),
        "crs_reports" => "CRS Reports".to_string(),
        other => {
            // Title case.
            let mut chars = other.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_known_corpora() {
        assert_eq!(humanize_corpus_id("wikipedia"), "Wikipedia");
        assert_eq!(humanize_corpus_id("stackexchange"), "Stack Exchange");
        assert_eq!(humanize_corpus_id("openalex"), "OpenAlex");
    }

    #[test]
    fn humanize_unknown_corpus() {
        assert_eq!(humanize_corpus_id("custom"), "Custom");
    }
}
