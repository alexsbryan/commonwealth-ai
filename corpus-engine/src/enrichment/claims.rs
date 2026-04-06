//! Extracted claims and their epistemic status.

use serde::{Deserialize, Serialize};

/// A propositional claim extracted from a chunk, with the epistemic
/// status the source text itself ascribed to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractedClaim {
    /// Unique claim identifier within the corpus.
    pub id: u64,

    /// The propositional claim as a declarative statement.
    /// Example: "Moral responsibility is compatible with determinism"
    pub claim: String,

    /// The chunk this claim was extracted from.
    pub source_chunk_id: u64,

    /// The corpus this claim belongs to.
    pub corpus_id: String,

    /// Epistemic status as characterized by the source text.
    pub epistemic_status: EpistemicStatus,

    /// The source text's own hedging language around this claim.
    /// Example: "Most contemporary philosophers hold that..."
    pub hedging_language: Option<String>,

    /// Who or which tradition this claim is attributed to.
    /// Example: "Compatibilists (Frankfurt, Dennett, Wolf)"
    pub attributed_to: Option<String>,

    /// The article or entry this claim comes from.
    /// Example: "Free Will" (the SEP entry title)
    pub source_entry: Option<String>,

    /// Embedding of the claim text. Same dimensions as the corpus's
    /// chunk embeddings — must use the same embedding model.
    pub embedding: Vec<f32>,
}

/// How confidently the source text presents a claim.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum EpistemicStatus {
    /// "It is widely accepted..." / "There is consensus..."
    /// The source presents this as the dominant view with minimal dissent.
    Consensus,

    /// "Most scholars hold..." / "The standard view is..."
    /// Dominant view but acknowledging dissent exists.
    Majority,

    /// "This remains controversial..." / "There is significant debate..."
    /// Actively disputed with substantive positions on multiple sides.
    Contested,

    /// "Critics argue..." / "A minority position holds..."
    /// A non-dominant position.
    Minority,

    /// "It has been established..." / "It is uncontroversial that..."
    /// Settled fact within the field.
    Established,

    /// The source text doesn't clearly indicate epistemic status.
    Unclear,
}

impl EpistemicStatus {
    /// A short human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Consensus => "consensus",
            Self::Majority => "majority",
            Self::Contested => "contested",
            Self::Minority => "minority",
            Self::Established => "established",
            Self::Unclear => "unclear",
        }
    }

    /// Parse from a lowercase string. Unknown values map to `Unclear`.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "consensus" => Self::Consensus,
            "majority" => Self::Majority,
            "contested" => Self::Contested,
            "minority" => Self::Minority,
            "established" => Self::Established,
            _ => Self::Unclear,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_round_trip() {
        for status in [
            EpistemicStatus::Consensus,
            EpistemicStatus::Majority,
            EpistemicStatus::Contested,
            EpistemicStatus::Minority,
            EpistemicStatus::Established,
            EpistemicStatus::Unclear,
        ] {
            assert_eq!(EpistemicStatus::parse(status.label()), status);
        }
    }

    #[test]
    fn parse_unknown_is_unclear() {
        assert_eq!(EpistemicStatus::parse("nonsense"), EpistemicStatus::Unclear);
    }

    #[test]
    fn json_round_trip() {
        let claim = ExtractedClaim {
            id: 42,
            claim: "Free will is compatible with determinism".into(),
            source_chunk_id: 100,
            corpus_id: "sep".into(),
            epistemic_status: EpistemicStatus::Majority,
            hedging_language: Some("Most contemporary philosophers hold that...".into()),
            attributed_to: Some("Compatibilists (Frankfurt, Dennett, Wolf)".into()),
            source_entry: Some("Free Will".into()),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
        };
        let json = serde_json::to_string(&claim).unwrap();
        let back: ExtractedClaim = serde_json::from_str(&json).unwrap();
        assert_eq!(claim, back);
    }
}
