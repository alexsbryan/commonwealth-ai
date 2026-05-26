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
        let knowledge_plan = app_state
            .inner
            .knowledge_store
            .get_shard_plan()
            .unwrap_or_default();

        let self_node_id = app_state.inner.self_node_id_swap.load_full().as_ref().clone();

        // Members.
        let mut members: Vec<MeshMember> = mesh
            .members
            .values()
            .map(|m| {
                let is_self = m.node_id == self_node_id;
                MeshMember {
                    name: m.name.clone(),
                    node_id: member_node_id_key(&m.node_id),
                    is_self,
                    status: match m.status {
                        NodeStatus::Online => MemberStatus::Online,
                        NodeStatus::Busy => MemberStatus::Busy,
                        NodeStatus::Away => MemberStatus::Away,
                        NodeStatus::Offline => MemberStatus::Offline,
                    },
                    contribution_level: 0, // Populated from ledger
                    contribution_label: String::new(),
                    // SocketAddrs render as host:port. Surfaced for
                    // `sovereign mesh status` and the pod-deployment
                    // workflow (operators need self.address as the
                    // founder addr; member addresses for debugging).
                    addresses: m.addresses.iter().map(|a| a.to_string()).collect(),
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
            // Populated by the daemon's mesh_get_state wrapper from
            // EmbeddedDaemon::current_invite — keeping it None here
            // avoids leaking the plaintext key through the AppState
            // path that doesn't own it.
            join_link: None,
            join_key: None,
        };

        Self {
            status,
            members,
            corpora,
            contribution: None, // Populated from ledger
        }
    }
}

/// The canonical node_id string the mesh UI keys on. Full hex (32
/// chars), NOT `NodeId`'s `Display` (which is `"node-"+8 bytes` — a
/// lossy, non-round-trippable label). The desktop joins members
/// against per-peer contributions and peer-preferences (both keyed on
/// full hex) by this string, and `mesh_set_peer_preference` parses it
/// back into a `NodeId`. Emitting the Display form here silently broke
/// every `contributions.get(member.node_id)` lookup → blank per-member
/// panels even when the data was present. Pinned by `node_id_key_is_full_hex`.
fn member_node_id_key(id: &commonwealth_core::ids::NodeId) -> String {
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
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

    /// Regression pin (the per-member panel was blank because the
    /// member list used the lossy `Display` form while contributions +
    /// prefs are keyed on full hex). The member key MUST be the full
    /// 32-char hex and MUST differ from `Display` (`"node-…"`), so the
    /// UI's `contributions.get(member.node_id)` join lands.
    #[test]
    fn node_id_key_is_full_hex() {
        use commonwealth_core::ids::NodeId;
        let id = NodeId::from_u128(0x44ae_7614_2b0c_3c72_3051_ff98_f043_104a);
        let key = member_node_id_key(&id);
        assert_eq!(key.len(), 32, "node_id key must be full 16-byte hex");
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        // It must NOT be the truncated Display label the bug emitted
        // (`Display` = "node-"+8 bytes = 21 chars).
        assert_ne!(key, id.to_string());
        assert!(!key.starts_with("node-"));
    }
}
