//! Relationships between claims (contradicts, supports, refines, etc.).

use serde::{Deserialize, Serialize};

/// A directed epistemic relationship between two claims.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimRelationship {
    /// Unique relationship identifier.
    pub id: u64,

    /// The two claims in this relationship.
    pub claim_a_id: u64,
    pub claim_b_id: u64,

    /// The type of epistemic relationship.
    pub relationship: RelationshipType,

    /// The question or issue that connects these claims.
    /// Example: "Whether moral responsibility requires alternative possibilities"
    pub connecting_issue: Option<String>,

    /// The chunk(s) where this relationship is evident.
    /// Stored as a JSON-encoded array in the LanceDB table.
    pub evidence_chunk_ids: Vec<u64>,

    /// Confidence in the extraction (0.0 to 1.0).
    pub confidence: f32,
}

/// The type of relationship between two claims.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipType {
    /// Claim A directly contradicts Claim B.
    /// "Compatibilists reject the incompatibilist argument that..."
    Contradicts,

    /// Claim A provides evidence or argument for Claim B.
    /// "Frankfurt cases support the compatibilist claim that..."
    Supports,

    /// Claim A refines, qualifies, or adds nuance to Claim B.
    /// "Semicompatibilism accepts determinism but restricts the
    ///  scope of moral responsibility to..."
    Refines,

    /// Claims A and B are presented as alternative answers
    /// to the same question.
    CompetingAnswers,

    /// Claim A presupposes or depends on Claim B.
    /// "The consequence argument assumes that the laws of nature
    ///  are deterministic"
    Presupposes,
}

impl RelationshipType {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Contradicts => "contradicts",
            Self::Supports => "supports",
            Self::Refines => "refines",
            Self::CompetingAnswers => "competing_answers",
            Self::Presupposes => "presupposes",
        }
    }

    /// Parse from a snake_case string. Returns `None` for unknown values
    /// (including "none", which the inference prompt uses to indicate
    /// no relationship — callers should treat this as a sentinel).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "contradicts" => Some(Self::Contradicts),
            "supports" => Some(Self::Supports),
            "refines" => Some(Self::Refines),
            "competing_answers" => Some(Self::CompetingAnswers),
            "presupposes" => Some(Self::Presupposes),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_round_trip() {
        for rt in [
            RelationshipType::Contradicts,
            RelationshipType::Supports,
            RelationshipType::Refines,
            RelationshipType::CompetingAnswers,
            RelationshipType::Presupposes,
        ] {
            assert_eq!(RelationshipType::parse(rt.label()), Some(rt));
        }
    }

    #[test]
    fn parse_none_returns_none() {
        assert_eq!(RelationshipType::parse("none"), None);
        assert_eq!(RelationshipType::parse("garbage"), None);
    }

    #[test]
    fn json_round_trip() {
        let rel = ClaimRelationship {
            id: 1,
            claim_a_id: 100,
            claim_b_id: 200,
            relationship: RelationshipType::Contradicts,
            connecting_issue: Some(
                "Whether moral responsibility requires alternative possibilities".into(),
            ),
            evidence_chunk_ids: vec![10, 20, 30],
            confidence: 0.85,
        };
        let json = serde_json::to_string(&rel).unwrap();
        let back: ClaimRelationship = serde_json::from_str(&json).unwrap();
        assert_eq!(rel, back);
    }
}
