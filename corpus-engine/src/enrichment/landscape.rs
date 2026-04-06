//! The epistemic landscape — a structured map of positions on a topic.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::claims::{EpistemicStatus, ExtractedClaim};
use super::relationships::ClaimRelationship;

/// A structured snapshot of the positions, agreements, and disagreements
/// found in a corpus on a given topic.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EpistemicLandscape {
    /// Claims marked as `Consensus` or `Established`.
    pub consensus_claims: Vec<ExtractedClaim>,

    /// Clusters of contested claims grouped by the issue they address.
    pub contested_clusters: Vec<ContestedCluster>,

    /// Claims marked as `Minority` — non-dominant positions worth noting.
    pub minority_claims: Vec<ExtractedClaim>,
}

/// A cluster of competing claims about a single issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContestedCluster {
    /// The question or issue being contested.
    pub issue: String,
    /// The competing positions, each with their claims and attribution.
    pub positions: Vec<Position>,
}

/// A single position within a contested cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub claims: Vec<ExtractedClaim>,
    pub attributed_to: String,
}

impl EpistemicLandscape {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.consensus_claims.is_empty()
            && self.contested_clusters.is_empty()
            && self.minority_claims.is_empty()
    }

    /// Build a landscape from a flat collection of claims and relationships.
    /// Groups contested claims by their connecting issue and attribution.
    pub fn from_claims_and_relationships(
        claims: impl IntoIterator<Item = ExtractedClaim>,
        relationships: &[ClaimRelationship],
    ) -> Self {
        let mut consensus = Vec::new();
        let mut minority = Vec::new();
        let mut contested_map: HashMap<String, Vec<ExtractedClaim>> = HashMap::new();

        for claim in claims {
            match claim.epistemic_status {
                EpistemicStatus::Consensus | EpistemicStatus::Established => {
                    consensus.push(claim);
                }
                EpistemicStatus::Minority => {
                    minority.push(claim);
                }
                EpistemicStatus::Contested | EpistemicStatus::Majority => {
                    let issue = relationships
                        .iter()
                        .find(|r| r.claim_a_id == claim.id || r.claim_b_id == claim.id)
                        .and_then(|r| r.connecting_issue.clone())
                        .unwrap_or_else(|| "General".to_string());
                    contested_map.entry(issue).or_default().push(claim);
                }
                EpistemicStatus::Unclear => {
                    // Skip — no useful structure to extract.
                }
            }
        }

        let contested_clusters = contested_map
            .into_iter()
            .map(|(issue, claims)| {
                // Group claims within an issue by their attribution.
                let mut positions: HashMap<String, Vec<ExtractedClaim>> = HashMap::new();
                for claim in claims {
                    let attr = claim
                        .attributed_to
                        .clone()
                        .unwrap_or_else(|| "Unattributed".to_string());
                    positions.entry(attr).or_default().push(claim);
                }
                ContestedCluster {
                    issue,
                    positions: positions
                        .into_iter()
                        .map(|(attributed_to, claims)| Position { claims, attributed_to })
                        .collect(),
                }
            })
            .collect();

        Self {
            consensus_claims: consensus,
            contested_clusters,
            minority_claims: minority,
        }
    }

    /// Format the landscape as a structured text block for the model
    /// to consume inside a `ReasonWithTools` loop.
    pub fn format_for_model(&self) -> String {
        let mut output = String::new();

        if !self.consensus_claims.is_empty() {
            output.push_str("=== ESTABLISHED / CONSENSUS ===\n");
            for claim in &self.consensus_claims {
                output.push_str(&format!(
                    "• {} [{}] (from: {})\n",
                    claim.claim,
                    claim.epistemic_status.label(),
                    claim.source_entry.as_deref().unwrap_or("unknown"),
                ));
            }
            output.push('\n');
        }

        if !self.contested_clusters.is_empty() {
            output.push_str("=== CONTESTED ===\n");
            for cluster in &self.contested_clusters {
                output.push_str(&format!("Issue: {}\n", cluster.issue));
                for position in &cluster.positions {
                    output.push_str(&format!("  Position ({})\n", position.attributed_to));
                    for claim in &position.claims {
                        output.push_str(&format!(
                            "    • {} [{}] (from: {})\n",
                            claim.claim,
                            claim.epistemic_status.label(),
                            claim.source_entry.as_deref().unwrap_or("unknown"),
                        ));
                    }
                }
                output.push('\n');
            }
        }

        if !self.minority_claims.is_empty() {
            output.push_str("=== MINORITY / DISSENTING ===\n");
            for claim in &self.minority_claims {
                output.push_str(&format!(
                    "• {} [{}] (from: {}, attributed to: {})\n",
                    claim.claim,
                    claim.epistemic_status.label(),
                    claim.source_entry.as_deref().unwrap_or("unknown"),
                    claim.attributed_to.as_deref().unwrap_or("unattributed"),
                ));
            }
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_claim(id: u64, status: EpistemicStatus, attr: Option<&str>, entry: &str) -> ExtractedClaim {
        ExtractedClaim {
            id,
            claim: format!("claim {id}"),
            source_chunk_id: id,
            corpus_id: "test".into(),
            epistemic_status: status,
            hedging_language: None,
            attributed_to: attr.map(String::from),
            source_entry: Some(entry.into()),
            embedding: vec![0.0],
        }
    }

    #[test]
    fn empty_landscape() {
        let l = EpistemicLandscape::empty();
        assert!(l.is_empty());
        assert_eq!(l.format_for_model(), "");
    }

    #[test]
    fn groups_consensus_minority_and_contested() {
        let claims = vec![
            make_claim(1, EpistemicStatus::Consensus, None, "Entry A"),
            make_claim(2, EpistemicStatus::Established, None, "Entry B"),
            make_claim(3, EpistemicStatus::Minority, Some("Critic"), "Entry C"),
            make_claim(4, EpistemicStatus::Contested, Some("Compatibilists"), "Entry D"),
            make_claim(5, EpistemicStatus::Contested, Some("Incompatibilists"), "Entry E"),
        ];
        let rels = vec![ClaimRelationship {
            id: 1,
            claim_a_id: 4,
            claim_b_id: 5,
            relationship: super::super::RelationshipType::CompetingAnswers,
            connecting_issue: Some("Free will and determinism".into()),
            evidence_chunk_ids: vec![],
            confidence: 0.9,
        }];

        let landscape = EpistemicLandscape::from_claims_and_relationships(claims, &rels);
        assert_eq!(landscape.consensus_claims.len(), 2);
        assert_eq!(landscape.minority_claims.len(), 1);
        assert_eq!(landscape.contested_clusters.len(), 1);
        assert_eq!(landscape.contested_clusters[0].issue, "Free will and determinism");
        assert_eq!(landscape.contested_clusters[0].positions.len(), 2);
    }

    #[test]
    fn format_includes_section_headers() {
        let claims = vec![
            make_claim(1, EpistemicStatus::Consensus, None, "Entry A"),
            make_claim(2, EpistemicStatus::Minority, Some("Dissenter"), "Entry B"),
        ];
        let landscape = EpistemicLandscape::from_claims_and_relationships(claims, &[]);
        let formatted = landscape.format_for_model();
        assert!(formatted.contains("=== ESTABLISHED / CONSENSUS ==="));
        assert!(formatted.contains("=== MINORITY / DISSENTING ==="));
    }
}
