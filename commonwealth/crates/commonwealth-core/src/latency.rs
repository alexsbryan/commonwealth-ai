use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ids::NodeId;

/// Pairwise latency measurements across all mesh nodes.
///
/// Internally uses a HashMap with tuple keys. Serializes as a Vec of entries
/// since JSON doesn't support non-string map keys.
#[derive(Debug, Clone, Default)]
pub struct LatencyMatrix {
    entries: HashMap<(NodeId, NodeId), LatencyRecord>,
}

/// Serialization proxy for LatencyMatrix.
#[derive(Serialize, Deserialize)]
struct LatencyMatrixProxy {
    entries: Vec<LatencyEntry>,
}

#[derive(Serialize, Deserialize)]
struct LatencyEntry {
    from: NodeId,
    to: NodeId,
    #[serde(flatten)]
    record: LatencyRecord,
}

impl Serialize for LatencyMatrix {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let proxy = LatencyMatrixProxy {
            entries: self
                .entries
                .iter()
                .map(|((from, to), record)| LatencyEntry {
                    from: *from,
                    to: *to,
                    record: *record,
                })
                .collect(),
        };
        proxy.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LatencyMatrix {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let proxy = LatencyMatrixProxy::deserialize(deserializer)?;
        let mut entries = HashMap::new();
        for e in proxy.entries {
            entries.insert((e.from, e.to), e.record);
        }
        Ok(Self { entries })
    }
}

/// Latency measurement between two nodes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LatencyRecord {
    /// Exponentially weighted moving average RTT in milliseconds.
    pub rtt_ms: f32,
    pub jitter_ms: f32,
    pub bandwidth_estimate_mbps: f32,
    pub last_measured: u64,
}

impl LatencyMatrix {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a latency measurement. Stores in both directions (a,b) and (b,a).
    pub fn record(&mut self, a: NodeId, b: NodeId, record: LatencyRecord) {
        self.entries.insert((a, b), record);
        self.entries.insert((b, a), record);
    }

    /// Get latency between two nodes.
    pub fn get(&self, a: NodeId, b: NodeId) -> Option<&LatencyRecord> {
        self.entries.get(&(a, b))
    }

    /// Get all peers of a given node with their latency records.
    pub fn peers_of(&self, node: NodeId) -> impl Iterator<Item = (NodeId, &LatencyRecord)> {
        self.entries
            .iter()
            .filter(move |((a, _), _)| *a == node)
            .map(|((_, b), rec)| (*b, rec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_matrix_record_and_get() {
        let mut m = LatencyMatrix::new();
        let a = NodeId::from_u128(1);
        let b = NodeId::from_u128(2);
        let rec = LatencyRecord {
            rtt_ms: 1.5,
            jitter_ms: 0.3,
            bandwidth_estimate_mbps: 1000.0,
            last_measured: 1700000000,
        };
        m.record(a, b, rec);

        // Bidirectional
        assert!(m.get(a, b).is_some());
        assert!(m.get(b, a).is_some());
        assert!((m.get(a, b).unwrap().rtt_ms - 1.5).abs() < 0.001);
    }

    #[test]
    fn latency_matrix_peers_of() {
        let mut m = LatencyMatrix::new();
        let a = NodeId::from_u128(1);
        let b = NodeId::from_u128(2);
        let c = NodeId::from_u128(3);
        let rec = LatencyRecord {
            rtt_ms: 1.0,
            jitter_ms: 0.1,
            bandwidth_estimate_mbps: 1000.0,
            last_measured: 0,
        };
        m.record(a, b, rec);
        m.record(a, c, rec);

        let peers: Vec<_> = m.peers_of(a).collect();
        assert_eq!(peers.len(), 2);
    }

    #[test]
    fn latency_matrix_serde_roundtrip() {
        let mut m = LatencyMatrix::new();
        let a = NodeId::from_u128(1);
        let b = NodeId::from_u128(2);
        m.record(
            a,
            b,
            LatencyRecord {
                rtt_ms: 2.0,
                jitter_ms: 0.5,
                bandwidth_estimate_mbps: 500.0,
                last_measured: 1700000000,
            },
        );
        let json = serde_json::to_string(&m).unwrap();
        let back: LatencyMatrix = serde_json::from_str(&json).unwrap();
        assert!(back.get(a, b).is_some());
        assert!((back.get(a, b).unwrap().rtt_ms - 2.0).abs() < 0.001);
    }
}
