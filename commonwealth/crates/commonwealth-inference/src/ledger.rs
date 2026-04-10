use serde::{Deserialize, Serialize};

use commonwealth_core::ids::NodeId;

/// A single entry in the contribution ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub timestamp: u64,
    pub node_id: NodeId,
    pub kind: LedgerEntryKind,
    pub amount: f64,
    pub unit: ContributionUnit,
}

/// Whether this ledger entry records a contribution or consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerEntryKind {
    Contributed { served_request_from: NodeId },
    Consumed { served_by: Vec<NodeId> },
}

/// Unit of resource contribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContributionUnit {
    /// Inference compute.
    GpuSeconds,
    /// Corpus index hosting.
    StorageGbDays,
    /// Index/model transfers, query serving.
    BandwidthGb,
}

/// Mesh-wide fairness policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum FairnessPolicy {
    /// Everyone sees the ledger. Social pressure does the work.
    #[default]
    Transparent,
    /// Below threshold: lower scheduling priority.
    SoftThrottle {
        threshold_hours: f64,
        priority_reduction: f32,
    },
    /// Below threshold: requests denied.
    HardCap { threshold_hours: f64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contribution_unit_serde_roundtrip() {
        for unit in [
            ContributionUnit::GpuSeconds,
            ContributionUnit::StorageGbDays,
            ContributionUnit::BandwidthGb,
        ] {
            let json = serde_json::to_string(&unit).unwrap();
            let back: ContributionUnit = serde_json::from_str(&json).unwrap();
            assert_eq!(unit, back);
        }
    }

    #[test]
    fn fairness_policy_serde_roundtrip() {
        let policies = vec![
            FairnessPolicy::Transparent,
            FairnessPolicy::SoftThrottle {
                threshold_hours: -10.0,
                priority_reduction: 0.5,
            },
            FairnessPolicy::HardCap {
                threshold_hours: -20.0,
            },
        ];
        for policy in policies {
            let json = serde_json::to_string(&policy).unwrap();
            let back: FairnessPolicy = serde_json::from_str(&json).unwrap();
            // Verify round-trip by re-serializing
            let json2 = serde_json::to_string(&back).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn ledger_entry_serde_roundtrip() {
        let entry = LedgerEntry {
            timestamp: 1700000000,
            node_id: NodeId::from_u128(1),
            kind: LedgerEntryKind::Contributed {
                served_request_from: NodeId::from_u128(2),
            },
            amount: 3.5,
            unit: ContributionUnit::GpuSeconds,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: LedgerEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.amount, 3.5);
        assert_eq!(back.unit, ContributionUnit::GpuSeconds);
    }

    #[test]
    fn default_fairness_is_transparent() {
        let policy = FairnessPolicy::default();
        let json = serde_json::to_string(&policy).unwrap();
        assert!(json.contains("transparent"));
    }
}
