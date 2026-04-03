use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ids::NodeId;
use crate::ledger::{ContributionUnit, FairnessPolicy, LedgerEntry, LedgerEntryKind};

/// Append-only ledger store, replicated to all nodes via gossip.
#[derive(Debug, Clone, Default)]
pub struct LedgerStore {
    entries: Vec<LedgerEntry>,
}

/// Balance summary for a single node.
#[derive(Debug, Clone)]
pub struct NodeBalance {
    pub node_id: NodeId,
    pub compute_hours: f64,
    pub storage_gb_days: f64,
    pub bandwidth_gb: f64,
    /// Net balance: positive = net contributor, negative = net consumer.
    pub balance: f64,
}

/// Result of evaluating a fairness policy against a node's balance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FairnessDecision {
    /// Allowed at normal priority.
    Allow,
    /// Allowed but at reduced scheduling priority.
    Throttle,
    /// Request denied — node has exceeded its consumption cap.
    Deny,
}

impl LedgerStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an entry to the ledger.
    pub fn append(&mut self, entry: LedgerEntry) {
        self.entries.push(entry);
    }

    /// Record a contribution (this node served a request for another node).
    pub fn record_contribution(
        &mut self,
        contributor: NodeId,
        consumer: NodeId,
        amount: f64,
        unit: ContributionUnit,
    ) {
        self.append(LedgerEntry {
            timestamp: now_secs(),
            node_id: contributor,
            kind: LedgerEntryKind::Contributed {
                served_request_from: consumer,
            },
            amount,
            unit,
        });
    }

    /// Record a consumption (this node consumed resources from other nodes).
    pub fn record_consumption(
        &mut self,
        consumer: NodeId,
        servers: Vec<NodeId>,
        amount: f64,
        unit: ContributionUnit,
    ) {
        self.append(LedgerEntry {
            timestamp: now_secs(),
            node_id: consumer,
            kind: LedgerEntryKind::Consumed { served_by: servers },
            amount,
            unit,
        });
    }

    /// Compute balance for all nodes over a rolling window.
    /// `window_secs`: only consider entries within this many seconds of now.
    pub fn compute_balances(&self, window_secs: u64) -> Vec<NodeBalance> {
        let cutoff = now_secs().saturating_sub(window_secs);
        let mut contributions: HashMap<NodeId, (f64, f64, f64)> = HashMap::new();
        let mut consumptions: HashMap<NodeId, (f64, f64, f64)> = HashMap::new();

        for entry in &self.entries {
            if entry.timestamp < cutoff {
                continue;
            }

            let (compute, storage, bandwidth) = match entry.unit {
                ContributionUnit::GpuSeconds => (entry.amount / 3600.0, 0.0, 0.0),
                ContributionUnit::StorageGbDays => (0.0, entry.amount, 0.0),
                ContributionUnit::BandwidthGb => (0.0, 0.0, entry.amount),
            };

            match &entry.kind {
                LedgerEntryKind::Contributed { .. } => {
                    let e = contributions.entry(entry.node_id).or_default();
                    e.0 += compute;
                    e.1 += storage;
                    e.2 += bandwidth;
                }
                LedgerEntryKind::Consumed { .. } => {
                    let e = consumptions.entry(entry.node_id).or_default();
                    e.0 += compute;
                    e.1 += storage;
                    e.2 += bandwidth;
                }
            }
        }

        // Collect all node IDs.
        let mut all_nodes: Vec<NodeId> = contributions
            .keys()
            .chain(consumptions.keys())
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        all_nodes.sort();

        all_nodes
            .into_iter()
            .map(|node_id| {
                let (c_compute, c_storage, c_bw) =
                    contributions.get(&node_id).copied().unwrap_or_default();
                let (d_compute, d_storage, d_bw) =
                    consumptions.get(&node_id).copied().unwrap_or_default();

                // Balance: sum of contributions minus consumptions.
                // Weight all units as "equivalent hours" for a single balance number.
                let contributed = c_compute + c_storage * 0.1 + c_bw * 0.05;
                let consumed = d_compute + d_storage * 0.1 + d_bw * 0.05;

                NodeBalance {
                    node_id,
                    compute_hours: c_compute - d_compute,
                    storage_gb_days: c_storage - d_storage,
                    bandwidth_gb: c_bw - d_bw,
                    balance: contributed - consumed,
                }
            })
            .collect()
    }

    /// Evaluate a fairness policy for a specific node.
    pub fn evaluate_fairness(
        &self,
        node_id: NodeId,
        policy: &FairnessPolicy,
        window_secs: u64,
    ) -> FairnessDecision {
        match policy {
            FairnessPolicy::Transparent => FairnessDecision::Allow,
            FairnessPolicy::SoftThrottle {
                threshold_hours, ..
            } => {
                let balances = self.compute_balances(window_secs);
                let balance = balances
                    .iter()
                    .find(|b| b.node_id == node_id)
                    .map(|b| b.balance)
                    .unwrap_or(0.0);
                if balance < *threshold_hours {
                    FairnessDecision::Throttle
                } else {
                    FairnessDecision::Allow
                }
            }
            FairnessPolicy::HardCap { threshold_hours } => {
                let balances = self.compute_balances(window_secs);
                let balance = balances
                    .iter()
                    .find(|b| b.node_id == node_id)
                    .map(|b| b.balance)
                    .unwrap_or(0.0);
                if balance < *threshold_hours {
                    FairnessDecision::Deny
                } else {
                    FairnessDecision::Allow
                }
            }
        }
    }

    /// Total number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get all entries (for gossip replication).
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    /// Merge entries from another node (received via gossip).
    /// Deduplicates by (timestamp, node_id, unit).
    pub fn merge(&mut self, remote_entries: &[LedgerEntry]) {
        for entry in remote_entries {
            let exists = self.entries.iter().any(|e| {
                e.timestamp == entry.timestamp
                    && e.node_id == entry.node_id
                    && e.unit == entry.unit
                    && (e.amount - entry.amount).abs() < 0.0001
            });
            if !exists {
                self.entries.push(entry.clone());
            }
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ledger() {
        let store = LedgerStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        let balances = store.compute_balances(86400 * 30);
        assert!(balances.is_empty());
    }

    #[test]
    fn record_contribution_and_consumption() {
        let mut store = LedgerStore::new();
        let alice = NodeId::from_u128(1);
        let bob = NodeId::from_u128(2);

        // Alice serves Bob: 3600 GPU-seconds (1 hour).
        store.record_contribution(alice, bob, 3600.0, ContributionUnit::GpuSeconds);
        // Bob consumed from Alice.
        store.record_consumption(bob, vec![alice], 3600.0, ContributionUnit::GpuSeconds);

        assert_eq!(store.len(), 2);
    }

    #[test]
    fn balance_computation_net_contributor() {
        let mut store = LedgerStore::new();
        let alice = NodeId::from_u128(1);
        let bob = NodeId::from_u128(2);

        // Alice contributes 10 hours of compute.
        store.record_contribution(alice, bob, 36000.0, ContributionUnit::GpuSeconds);
        // Alice consumes 2 hours.
        store.record_consumption(alice, vec![bob], 7200.0, ContributionUnit::GpuSeconds);

        let balances = store.compute_balances(86400 * 30);
        let alice_balance = balances.iter().find(|b| b.node_id == alice).unwrap();

        // Net: 10h contributed - 2h consumed = +8h.
        assert!((alice_balance.compute_hours - 8.0).abs() < 0.01);
        assert!(alice_balance.balance > 0.0);
    }

    #[test]
    fn balance_computation_net_consumer() {
        let mut store = LedgerStore::new();
        let eve = NodeId::from_u128(5);
        let alice = NodeId::from_u128(1);

        // Eve consumes 10 hours but contributes nothing.
        store.record_consumption(eve, vec![alice], 36000.0, ContributionUnit::GpuSeconds);

        let balances = store.compute_balances(86400 * 30);
        let eve_balance = balances.iter().find(|b| b.node_id == eve).unwrap();

        assert!(eve_balance.balance < 0.0);
        assert!(eve_balance.compute_hours < 0.0);
    }

    #[test]
    fn balance_with_multiple_contribution_types() {
        let mut store = LedgerStore::new();
        let carol = NodeId::from_u128(3);
        let eve = NodeId::from_u128(5);

        // Carol hosts storage and serves bandwidth.
        store.record_contribution(carol, eve, 170.0, ContributionUnit::StorageGbDays);
        store.record_contribution(carol, eve, 48.2, ContributionUnit::BandwidthGb);
        // Carol uses some compute.
        store.record_consumption(carol, vec![eve], 30960.0, ContributionUnit::GpuSeconds);

        let balances = store.compute_balances(86400 * 30);
        let carol_balance = balances.iter().find(|b| b.node_id == carol).unwrap();

        // Storage + bandwidth contributions should offset compute consumption.
        assert!(carol_balance.storage_gb_days > 0.0);
        assert!(carol_balance.bandwidth_gb > 0.0);
    }

    #[test]
    fn fairness_transparent_always_allows() {
        let store = LedgerStore::new();
        let policy = FairnessPolicy::Transparent;
        let decision = store.evaluate_fairness(NodeId::from_u128(1), &policy, 86400 * 30);
        assert_eq!(decision, FairnessDecision::Allow);
    }

    #[test]
    fn fairness_soft_throttle() {
        let mut store = LedgerStore::new();
        let eve = NodeId::from_u128(5);
        let alice = NodeId::from_u128(1);

        // Eve is a heavy consumer.
        store.record_consumption(eve, vec![alice], 72000.0, ContributionUnit::GpuSeconds);

        let policy = FairnessPolicy::SoftThrottle {
            threshold_hours: -10.0,
            priority_reduction: 0.5,
        };
        let decision = store.evaluate_fairness(eve, &policy, 86400 * 30);
        assert_eq!(decision, FairnessDecision::Throttle);

        // Alice (net contributor) should be allowed.
        store.record_contribution(alice, eve, 72000.0, ContributionUnit::GpuSeconds);
        let decision = store.evaluate_fairness(alice, &policy, 86400 * 30);
        assert_eq!(decision, FairnessDecision::Allow);
    }

    #[test]
    fn fairness_hard_cap_denies() {
        let mut store = LedgerStore::new();
        let eve = NodeId::from_u128(5);
        let alice = NodeId::from_u128(1);

        store.record_consumption(eve, vec![alice], 72000.0, ContributionUnit::GpuSeconds);

        let policy = FairnessPolicy::HardCap {
            threshold_hours: -10.0,
        };
        let decision = store.evaluate_fairness(eve, &policy, 86400 * 30);
        assert_eq!(decision, FairnessDecision::Deny);
    }

    #[test]
    fn merge_deduplicates() {
        let mut store_a = LedgerStore::new();
        let mut store_b = LedgerStore::new();

        let entry = LedgerEntry {
            timestamp: 1700000000,
            node_id: NodeId::from_u128(1),
            kind: LedgerEntryKind::Contributed {
                served_request_from: NodeId::from_u128(2),
            },
            amount: 100.0,
            unit: ContributionUnit::GpuSeconds,
        };

        store_a.append(entry.clone());
        store_b.append(entry.clone());

        // Merge B into A — should not duplicate.
        store_a.merge(store_b.entries());
        assert_eq!(store_a.len(), 1);
    }

    #[test]
    fn merge_adds_new_entries() {
        let mut store_a = LedgerStore::new();
        let store_b_entries = vec![LedgerEntry {
            timestamp: 1700000001,
            node_id: NodeId::from_u128(2),
            kind: LedgerEntryKind::Contributed {
                served_request_from: NodeId::from_u128(1),
            },
            amount: 200.0,
            unit: ContributionUnit::GpuSeconds,
        }];

        store_a.record_contribution(
            NodeId::from_u128(1),
            NodeId::from_u128(2),
            100.0,
            ContributionUnit::GpuSeconds,
        );

        store_a.merge(&store_b_entries);
        assert_eq!(store_a.len(), 2);
    }
}
