// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mesh soak invariant checker — the assertion engine behind
//! `svrn mesh check-invariants` and `scripts/mesh-soak.sh`.
//!
//! It polls each node's `GET /v1/mesh/status` and evaluates the mesh-level
//! invariants a multi-process soak must hold — the HTTP-observable subset of
//! the in-process DST invariant pack (`sovereign_mesh::dst`): **convergence**
//! (all reachable nodes agree on the member set), **no-ghost** (no node shows
//! a deliberately-downed peer as live), and **liveness** (every reachable node
//! is seen as live by every other reachable node).
//!
//! The pure evaluation here is unit-tested over mock snapshots; the HTTP
//! polling lives in `mesh_cmd::cmd_check_invariants`. Admission-safety and
//! bounded-fan-out are not HTTP-observable from `/v1/mesh/status` and stay with
//! the DST suite / glassbox endpoints.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

/// Minimal projection of `GET /v1/mesh/status`
/// (`sovereign_mesh::mesh_http::StatusResponse`). We deserialize only the
/// fields the invariants need, so the checker is decoupled from the full DTO
/// and tolerant of additions.
#[derive(Debug, Clone, Deserialize)]
pub struct NodeStatusView {
    #[serde(default)]
    pub members_total: usize,
    #[serde(default)]
    pub members: Vec<MemberView>,
    /// True when the polled node is the current shared-model host. Drives the
    /// `shared_model_single_host` invariant (no split-brain). Absent / `false`
    /// on non-shared-model meshes, where the invariant is then a no-op.
    #[serde(default)]
    pub shared_model_host: bool,
    /// Peer-admission load: current in-flight peer requests + the configured
    /// ceiling (from `glassbox_signals`). Drives `admission_safety`. Both default
    /// to 0 on older nodes that don't report them → `0 ≤ 0`, an inert pass.
    #[serde(default)]
    pub peer_inflight_current: usize,
    #[serde(default)]
    pub peer_inflight_ceiling: usize,
    /// Current outbound peer knowledge fan-out width (the `fanout_inflight`
    /// gauge). Drives `bounded_fan_out`. Defaults to 0 on older nodes → an inert
    /// pass against the sanity ceiling.
    #[serde(default)]
    pub fanout_inflight_current: usize,
    /// Track W: this node's own founder reachability (relay-home + discovery
    /// self-heal watchdog). Absent on non-iroh / older nodes → `None` → treated
    /// as not-degraded (inert), so the reachability SLI is a no-op there.
    #[serde(default)]
    pub founder_reachability: Option<FounderReachabilityView>,
}

/// Minimal projection of `founder_reachability` for the soak's reachability SLI:
/// is the founder's self-heal watchdog currently `degraded` (mid-recovery)?
#[derive(Debug, Clone, Deserialize, Default)]
pub struct FounderReachabilityView {
    #[serde(default)]
    pub degraded: bool,
    #[serde(default)]
    pub relay_homed: bool,
    #[serde(default)]
    pub rebuilds: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemberView {
    pub node_id: String,
    /// `"online" | "busy" | "away" | "offline"`.
    pub status: String,
    #[serde(default)]
    pub is_self: bool,
}

impl MemberView {
    /// Live = not formally offline. (Tombstoned members are filtered out of the
    /// status view server-side; an offline member is decayed-but-present.)
    fn is_live(&self) -> bool {
        self.status != "offline"
    }
}

/// One soak node: the address we polled and the status it reported (or an error
/// string if unreachable — itself a signal, not necessarily a violation when the
/// scenario crashed it on purpose).
pub struct NodeSnapshot {
    pub addr: String,
    pub status: Option<NodeStatusView>,
    pub error: Option<String>,
}

/// A violated mesh invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub invariant: &'static str,
    pub detail: String,
}

/// Evaluate the HTTP-observable mesh invariants over the reachable nodes'
/// snapshots.
///
/// `expected_live`, when supplied, is the set of node_ids the caller knows
/// should be up — so a deliberately-crashed node isn't flagged as a ghost and
/// the no-ghost check has a reference. When `None`, only convergence + pairwise
/// liveness among the reachable nodes are checked.
pub fn evaluate_invariants(
    snapshots: &[NodeSnapshot],
    expected_live: Option<&BTreeSet<String>>,
) -> Vec<Violation> {
    let mut violations = Vec::new();

    let reachable: Vec<(&String, &NodeStatusView)> = snapshots
        .iter()
        .filter_map(|s| s.status.as_ref().map(|v| (&s.addr, v)))
        .collect();
    if reachable.is_empty() {
        violations.push(Violation {
            invariant: "liveness",
            detail: "no polled node was reachable".into(),
        });
        return violations;
    }

    let member_set = |v: &NodeStatusView| -> BTreeSet<String> {
        v.members.iter().map(|m| m.node_id.clone()).collect()
    };

    // Convergence: every reachable node reports the same member-id set.
    let (first_addr, first_view) = reachable[0];
    let base = member_set(first_view);
    for (addr, view) in &reachable[1..] {
        let set = member_set(view);
        if set != base {
            violations.push(Violation {
                invariant: "convergence",
                detail: format!("node {first_addr} knows {base:?}; node {addr} knows {set:?}"),
            });
        }
    }

    // No-ghost: no reachable node shows a member as live that the caller knows
    // is down (the deliberately-crashed set). Only checked when a reference set
    // is supplied.
    if let Some(live) = expected_live {
        for (addr, view) in &reachable {
            for m in &view.members {
                if m.is_live() && !m.is_self && !live.contains(&m.node_id) {
                    violations.push(Violation {
                        invariant: "no_ghost_members",
                        detail: format!(
                            "node {addr} still shows {} as {} (expected down)",
                            m.node_id, m.status
                        ),
                    });
                }
            }
        }
    }

    // UniqueIds: no two reachable nodes may claim the SAME self-id. A collision
    // means a restart/rejoin adopted an id another live node already owns — the
    // deeper cause behind the orphan "ghost" ids the 8h soak surfaced. Checked
    // over the `is_self` record each daemon reports for itself; a non-colliding
    // mesh has one distinct claimant per id, so this is inert until it isn't.
    let mut self_claimants: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for (addr, view) in &reachable {
        if let Some(m) = view.members.iter().find(|m| m.is_self) {
            self_claimants
                .entry(m.node_id.clone())
                .or_default()
                .push(addr);
        }
    }
    for (id, addrs) in &self_claimants {
        if addrs.len() > 1 {
            violations.push(Violation {
                invariant: "unique_ids",
                detail: format!(
                    "node_id {id} claimed as self by {} nodes: {addrs:?}",
                    addrs.len()
                ),
            });
        }
    }

    // Shared-model no-split-brain: at most one reachable node may report itself
    // as the host. Two hosts means a partition/convergence bug let the RPC
    // layer-split assemble twice — `partition::should_host` must elect exactly
    // one. A non-shared-model mesh reports `false` everywhere → 0 hosts → no
    // violation, so this is inert unless the fleet is actually running a shared
    // model. This is the HTTP-observable half of the failover invariant (the
    // "a new host appears after the old one drops" half is a scenario assertion
    // in the soak script, driven by the `host-role transition` log + this flag).
    let hosts: Vec<&String> = reachable
        .iter()
        .filter(|(_, v)| v.shared_model_host)
        .map(|(addr, _)| *addr)
        .collect();
    if hosts.len() > 1 {
        violations.push(Violation {
            invariant: "shared_model_single_host",
            detail: format!("multiple shared-model hosts reachable: {hosts:?}"),
        });
    }

    // AdmissionSafety: peer in-flight must never exceed the configured ceiling.
    // (DST also asserts it returns to 0 at quiescence; over HTTP, at arbitrary
    // checkpoints, we assert the hard bound.) Inert until real peer-inference
    // load drives inflight above 0 — the ingest/contention lane exercises it.
    for (addr, view) in &reachable {
        if view.peer_inflight_current > view.peer_inflight_ceiling {
            violations.push(Violation {
                invariant: "admission_safety",
                detail: format!(
                    "node {addr} peer_inflight {} exceeds ceiling {}",
                    view.peer_inflight_current, view.peer_inflight_ceiling
                ),
            });
        }
    }

    // BoundedFanOut: a node's concurrent OUTBOUND peer fan-out (the
    // `fanout_inflight` gauge) must never exceed a structural sanity ceiling.
    // The precise per-request corpora bound is enforced + unit-tested at
    // `select_fanout_corpora` (≤ MAX_FANOUT_CORPORA, skips oversized); this
    // runtime check is the glassbox net for a fan-out storm or a leaked
    // `FanoutGuard` accumulating. Inert (0) until real knowledge fan-out runs —
    // the ingest/contention lane drives it above 0.
    const FANOUT_INFLIGHT_CEILING: usize = 64;
    for (addr, view) in &reachable {
        if view.fanout_inflight_current > FANOUT_INFLIGHT_CEILING {
            violations.push(Violation {
                invariant: "bounded_fan_out",
                detail: format!(
                    "node {addr} fanout_inflight {} exceeds sanity ceiling {}",
                    view.fanout_inflight_current, FANOUT_INFLIGHT_CEILING
                ),
            });
        }
    }

    // Liveness: every reachable node (identified by its own self-record id) must
    // be seen as live by every other reachable node.
    let self_ids: BTreeSet<String> = reachable
        .iter()
        .filter_map(|(_, v)| {
            v.members
                .iter()
                .find(|m| m.is_self)
                .map(|m| m.node_id.clone())
        })
        .collect();
    for (addr, view) in &reachable {
        for live_id in &self_ids {
            match view.members.iter().find(|m| &m.node_id == live_id) {
                Some(m) if m.is_live() => {}
                _ => violations.push(Violation {
                    invariant: "liveness",
                    detail: format!("node {addr} does not see reachable node {live_id} as live"),
                }),
            }
        }
    }

    violations
}

// ── Layer 3: load / SLO regression gate ──────────────────────────────────────
//
// The soak streams findings to `mesh-soak-findings.jsonl`; `mesh soak-gate`
// distils them into a few SLIs and gates each against a committed baseline
// (direction + tolerance), the same shape as the `lane_baseline` quality gate.

/// Addrs whose founder-reachability watchdog currently reports `degraded`
/// (self-heal in progress / not yet recovered). A PERSISTENT non-empty set —
/// captured across checkpoints by `founder_degraded_rate` — is the "self-heal
/// isn't recovering" signal the soak gates on; transient degraded during a fast
/// recovery keeps the rate low. `None`/unreachable status is not counted.
pub fn founder_degraded_addrs(snapshots: &[NodeSnapshot]) -> Vec<String> {
    snapshots
        .iter()
        .filter_map(|s| {
            let fr = s.status.as_ref()?.founder_reachability.as_ref()?;
            fr.degraded.then(|| s.addr.clone())
        })
        .collect()
}

/// Extract soak SLIs from the parsed findings lines. Pure + testable.
/// Recognised line shapes (others — fault markers — are ignored):
///   - invariant check: `{ "ok": bool, "violations": [...], "founder_degraded": [...] }`
///   - load sample:     `{ "kind": "load", "latency_ms": N, "ok": bool }`
pub fn soak_slis(lines: &[serde_json::Value]) -> BTreeMap<String, f64> {
    let (mut checks, mut check_fail, mut loads, mut load_ok) = (0u64, 0u64, 0u64, 0u64);
    let mut founder_degraded_checks = 0u64;
    let mut latencies: Vec<f64> = Vec::new();
    for v in lines {
        if v.get("kind").and_then(|k| k.as_str()) == Some("load") {
            loads += 1;
            if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
                load_ok += 1;
            }
            if let Some(ms) = v.get("latency_ms").and_then(|m| m.as_f64()) {
                latencies.push(ms);
            }
        } else if v.get("violations").is_some() {
            checks += 1;
            if !v.get("ok").and_then(|b| b.as_bool()).unwrap_or(true) {
                check_fail += 1;
            }
            // Track W: a checkpoint where any node's founder self-heal is still
            // `degraded`. Transient under chaos (fast recovery → low rate); a
            // wedged self-heal that never recovers spikes it.
            if v.get("founder_degraded")
                .and_then(|a| a.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false)
            {
                founder_degraded_checks += 1;
            }
        }
    }
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| -> f64 {
        if latencies.is_empty() {
            return 0.0;
        }
        let idx = ((p / 100.0) * (latencies.len() as f64 - 1.0)).round() as usize;
        latencies[idx.min(latencies.len() - 1)]
    };
    let mut m = BTreeMap::new();
    m.insert(
        "invariant_violation_rate".into(),
        if checks == 0 {
            0.0
        } else {
            check_fail as f64 / checks as f64
        },
    );
    m.insert(
        "load_success_rate".into(),
        if loads == 0 {
            1.0
        } else {
            load_ok as f64 / loads as f64
        },
    );
    m.insert("load_p50_ms".into(), pct(50.0));
    m.insert("load_p99_ms".into(), pct(99.0));
    m.insert(
        "founder_degraded_rate".into(),
        if checks == 0 {
            0.0
        } else {
            founder_degraded_checks as f64 / checks as f64
        },
    );
    m
}

/// Which way is worse for an SLI.
#[derive(Clone, Copy)]
pub enum SliDir {
    HigherIsBetter,
    LowerIsBetter,
}

/// One gated SLI: which way is worse + the noise tolerance below which movement
/// isn't a regression.
pub struct SliSpec {
    pub name: &'static str,
    pub dir: SliDir,
    pub tolerance: f64,
}

/// The mesh-soak SLO specs. Establish a baseline, then ratchet — the tolerances
/// are starting points to tune once a real baseline exists.
pub fn soak_slo_specs() -> &'static [SliSpec] {
    &[
        SliSpec {
            name: "invariant_violation_rate",
            dir: SliDir::LowerIsBetter,
            tolerance: 0.02,
        },
        SliSpec {
            name: "load_success_rate",
            dir: SliDir::HigherIsBetter,
            tolerance: 0.05,
        },
        SliSpec {
            name: "load_p50_ms",
            dir: SliDir::LowerIsBetter,
            tolerance: 50.0,
        },
        SliSpec {
            name: "load_p99_ms",
            dir: SliDir::LowerIsBetter,
            tolerance: 200.0,
        },
        // Track W: fraction of checkpoints where a founder's self-heal was still
        // degraded. Self-heal is fast, so under reachability chaos this stays
        // low; a self-heal that stops recovering (the regression this guards)
        // ratchets it up past the tolerance.
        SliSpec {
            name: "founder_degraded_rate",
            dir: SliDir::LowerIsBetter,
            tolerance: 0.15,
        },
    ]
}

/// One row of the gate verdict.
pub struct GateRow {
    pub name: String,
    pub baseline: Option<f64>,
    pub current: f64,
    pub regressed: bool,
}

/// Gate current SLIs against an optional baseline per [`soak_slo_specs`].
/// Returns `(rows, first_run)`; `first_run` is true when there is no baseline.
/// A current value regresses if it moved past tolerance in the worse direction
/// (a non-finite current value always regresses — an undefined SLI can't be
/// certified no-worse).
pub fn gate_slis(
    current: &BTreeMap<String, f64>,
    baseline: Option<&BTreeMap<String, f64>>,
) -> (Vec<GateRow>, bool) {
    let first_run = baseline.is_none();
    let rows = soak_slo_specs()
        .iter()
        .map(|spec| {
            let cur = *current.get(spec.name).unwrap_or(&f64::NAN);
            let base = baseline.and_then(|b| b.get(spec.name).copied());
            let regressed = match base {
                None => false, // first run / new metric — informational only
                Some(_) if !cur.is_finite() => true,
                Some(prev) => match spec.dir {
                    SliDir::HigherIsBetter => cur < prev - spec.tolerance,
                    SliDir::LowerIsBetter => cur > prev + spec.tolerance,
                },
            };
            GateRow {
                name: spec.name.to_string(),
                baseline: base,
                current: cur,
                regressed,
            }
        })
        .collect();
    (rows, first_run)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(members: &[(&str, &str, bool)], total: usize) -> NodeStatusView {
        NodeStatusView {
            members_total: total,
            members: members
                .iter()
                .map(|(id, st, slf)| MemberView {
                    node_id: (*id).to_string(),
                    status: (*st).to_string(),
                    is_self: *slf,
                })
                .collect(),
            shared_model_host: false,
            peer_inflight_current: 0,
            peer_inflight_ceiling: 0,
            fanout_inflight_current: 0,
            founder_reachability: None,
        }
    }
    fn snap(addr: &str, v: NodeStatusView) -> NodeSnapshot {
        NodeSnapshot {
            addr: addr.into(),
            status: Some(v),
            error: None,
        }
    }
    fn live_set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }
    fn snap_reach(addr: &str, degraded: bool) -> NodeSnapshot {
        let mut v = view(&[("n1", "online", true)], 1);
        v.founder_reachability = Some(FounderReachabilityView {
            degraded,
            relay_homed: !degraded,
            rebuilds: 0,
        });
        snap(addr, v)
    }

    #[test]
    fn founder_degraded_addrs_and_reachability_sli() {
        // Only the degraded node is reported; a node with no reachability data
        // is inert (self-heal not applicable there).
        let snaps = vec![snap_reach("a", true), snap_reach("b", false)];
        assert_eq!(founder_degraded_addrs(&snaps), vec!["a".to_string()]);
        assert!(founder_degraded_addrs(&[snap("c", view(&[], 0))]).is_empty());

        // SLI: 1 of 2 check lines had a degraded founder → rate 0.5.
        let lines = vec![
            serde_json::json!({"ok": true, "violations": [], "founder_degraded": ["a:9741"]}),
            serde_json::json!({"ok": true, "violations": [], "founder_degraded": []}),
        ];
        assert_eq!(soak_slis(&lines)["founder_degraded_rate"], 0.5);
        // Inert 0.0 when older nodes emit no reachability field at all.
        let none = vec![serde_json::json!({"ok": true, "violations": []})];
        assert_eq!(soak_slis(&none)["founder_degraded_rate"], 0.0);
    }

    #[test]
    fn converged_healthy_mesh_has_no_violations() {
        let a = snap(
            "a",
            view(&[("n1", "online", true), ("n2", "online", false)], 2),
        );
        let b = snap(
            "b",
            view(&[("n1", "online", false), ("n2", "online", true)], 2),
        );
        assert!(evaluate_invariants(&[a, b], None).is_empty());
    }

    #[test]
    fn duplicate_self_ids_flag_unique_ids() {
        // Distinct self-claims (a→n1, b→n2) are clean.
        let a = snap(
            "a",
            view(&[("n1", "online", true), ("n2", "online", false)], 2),
        );
        let b = snap(
            "b",
            view(&[("n1", "online", false), ("n2", "online", true)], 2),
        );
        assert!(!evaluate_invariants(&[a, b], None)
            .iter()
            .any(|v| v.invariant == "unique_ids"));
        // Two daemons both claiming n2 as self = an id collision (the 8h-soak bug).
        let c = snap(
            "c",
            view(&[("n1", "online", false), ("n2", "online", true)], 2),
        );
        let d = snap(
            "d",
            view(&[("n1", "online", false), ("n2", "online", true)], 2),
        );
        let vs = evaluate_invariants(&[c, d], None);
        assert!(vs.iter().any(|v| v.invariant == "unique_ids"), "{vs:?}");
    }

    #[test]
    fn peer_inflight_over_ceiling_flags_admission_safety() {
        let mut over = view(&[("n1", "online", true)], 1);
        over.peer_inflight_current = 3;
        over.peer_inflight_ceiling = 2;
        let vs = evaluate_invariants(&[snap("a", over)], None);
        assert!(
            vs.iter().any(|x| x.invariant == "admission_safety"),
            "{vs:?}"
        );
        // At/under ceiling is clean.
        let mut ok = view(&[("n1", "online", true)], 1);
        ok.peer_inflight_current = 2;
        ok.peer_inflight_ceiling = 2;
        assert!(!evaluate_invariants(&[snap("a", ok)], None)
            .iter()
            .any(|x| x.invariant == "admission_safety"));
    }

    #[test]
    fn fanout_inflight_over_ceiling_flags_bounded_fan_out() {
        // A runaway outbound fan-out width trips the sanity ceiling (64).
        let mut over = view(&[("n1", "online", true)], 1);
        over.fanout_inflight_current = 65;
        let vs = evaluate_invariants(&[snap("a", over)], None);
        assert!(
            vs.iter().any(|x| x.invariant == "bounded_fan_out"),
            "{vs:?}"
        );
        // At the ceiling (≤ 64) is clean; 0 is the inert common case.
        let mut ok = view(&[("n1", "online", true)], 1);
        ok.fanout_inflight_current = 64;
        assert!(!evaluate_invariants(&[snap("a", ok)], None)
            .iter()
            .any(|x| x.invariant == "bounded_fan_out"));
    }

    #[test]
    fn two_shared_model_hosts_flag_split_brain() {
        // Converged member set on both nodes, but BOTH claim the host role.
        let mut va = view(&[("n1", "online", true), ("n2", "online", false)], 2);
        va.shared_model_host = true;
        let mut vb = view(&[("n1", "online", false), ("n2", "online", true)], 2);
        vb.shared_model_host = true;
        let vs = evaluate_invariants(&[snap("a", va), snap("b", vb)], None);
        assert!(
            vs.iter().any(|v| v.invariant == "shared_model_single_host"),
            "two hosts must flag split-brain: {vs:?}"
        );
    }

    #[test]
    fn single_shared_model_host_is_clean() {
        let mut va = view(&[("n1", "online", true), ("n2", "online", false)], 2);
        va.shared_model_host = true; // a hosts
        let vb = view(&[("n1", "online", false), ("n2", "online", true)], 2); // b does not
        let vs = evaluate_invariants(&[snap("a", va), snap("b", vb)], None);
        assert!(vs.is_empty(), "exactly one host is healthy: {vs:?}");
    }

    #[test]
    fn divergent_member_sets_flag_convergence() {
        let a = snap(
            "a",
            view(&[("n1", "online", true), ("n2", "online", false)], 2),
        );
        let b = snap("b", view(&[("n2", "online", true)], 1)); // missing n1
        let vs = evaluate_invariants(&[a, b], None);
        assert!(vs.iter().any(|v| v.invariant == "convergence"), "{vs:?}");
    }

    #[test]
    fn ghost_member_flagged_when_expected_down() {
        // n2 was crashed (not in expected_live) but `a` still shows it online.
        let a = snap(
            "a",
            view(&[("n1", "online", true), ("n2", "online", false)], 2),
        );
        let vs = evaluate_invariants(&[a], Some(&live_set(&["n1"])));
        assert!(
            vs.iter().any(|v| v.invariant == "no_ghost_members"),
            "{vs:?}"
        );
    }

    #[test]
    fn decayed_offline_member_is_not_a_ghost_or_liveness_failure() {
        // `a` sees n2 as offline (decayed) and we didn't poll n2. With no
        // expected set, that's neither a ghost nor a liveness violation.
        let a = snap(
            "a",
            view(&[("n1", "online", true), ("n2", "offline", false)], 2),
        );
        let vs = evaluate_invariants(&[a], None);
        assert!(
            vs.is_empty(),
            "offline-but-unexpected is not a violation: {vs:?}"
        );
    }

    #[test]
    fn liveness_flagged_when_a_reachable_node_is_seen_offline_by_a_peer() {
        // Both reachable, but `a` shows `b` (n2) offline — a real liveness gap.
        let a = snap(
            "a",
            view(&[("n1", "online", true), ("n2", "offline", false)], 2),
        );
        let b = snap(
            "b",
            view(&[("n1", "online", false), ("n2", "online", true)], 2),
        );
        let vs = evaluate_invariants(&[a, b], None);
        assert!(vs.iter().any(|v| v.invariant == "liveness"), "{vs:?}");
    }

    #[test]
    fn slis_from_mixed_findings() {
        let lines: Vec<serde_json::Value> = vec![
            serde_json::json!({"ok": true, "violations": [], "unreachable": []}),
            serde_json::json!({"ok": false, "violations": [{"invariant":"liveness"}]}),
            serde_json::json!({"kind":"load","latency_ms": 10.0, "ok": true}),
            serde_json::json!({"kind":"load","latency_ms": 30.0, "ok": true}),
            serde_json::json!({"kind":"load","latency_ms": 50.0, "ok": false}),
            serde_json::json!({"kind":"fault","action":"kill"}), // ignored
        ];
        let m = soak_slis(&lines);
        assert!((m["invariant_violation_rate"] - 0.5).abs() < 1e-9);
        assert!((m["load_success_rate"] - 2.0 / 3.0).abs() < 1e-9);
        assert!(m["load_p99_ms"] >= m["load_p50_ms"]);
    }

    #[test]
    fn empty_findings_are_clean() {
        let m = soak_slis(&[]);
        assert_eq!(m["invariant_violation_rate"], 0.0);
        assert_eq!(m["load_success_rate"], 1.0);
    }

    /// The shape `mesh-soak.sh` used to emit on a FAILING checkpoint is
    /// invisible to the extractor — which is why `invariant_violation_rate`
    /// could never rise above 0.0 no matter how badly a run failed.
    ///
    /// `check()` took the CLI's human branch, so a failure produced
    /// `{phase, ok:false, detail}` with no `violations` key, and the `else if`
    /// above requires that key. The gate therefore reported perfect invariant
    /// health on a run that had just failed every checkpoint. `check()` now
    /// emits the `--json` record verbatim (it always carries `violations`), and
    /// this test is the pin: revert that flag and the first assertion here goes
    /// red instead of the defect going unnoticed for another two months.
    #[test]
    fn a_failing_checkpoint_is_only_counted_when_it_carries_violations() {
        // What the harness emits now.
        let json_branch = vec![
            serde_json::json!({"phase":"healthy","ok":true,"violations":[],"founder_degraded":[]}),
            serde_json::json!({"phase":"healed","ok":false,"founder_degraded":[],
                               "violations":[{"invariant":"convergence","detail":"node1 disagrees"}]}),
        ];
        assert_eq!(
            soak_slis(&json_branch)["invariant_violation_rate"],
            0.5,
            "a failing checkpoint must move the rate"
        );

        // What it emitted before — retained as the negative control so the
        // reason the flag is load-bearing stays legible to the next reader.
        let human_branch = vec![
            serde_json::json!({"phase":"healthy","ok":true,"violations":[]}),
            serde_json::json!({"phase":"healed","ok":false,"detail":"convergence failed"}),
        ];
        assert_eq!(
            soak_slis(&human_branch)["invariant_violation_rate"],
            0.0,
            "documents the defect: a failing checkpoint with no `violations` key \
             is silently dropped, so the SLI reads clean on a failed run"
        );
    }

    /// The cross-node offload verdict must reach the same SLI.
    ///
    /// `run_offload_probe` is shell, so nothing type-checks the record it
    /// writes; this pins the exact JSON it emits against the extractor. The
    /// probe deliberately reuses `invariant_violation_rate` rather than adding
    /// its own SLI: the offload rate is only comparable between runs that
    /// actually ran the probe, so a crash-lane baseline would false-alarm every
    /// offload run.
    #[test]
    fn the_offload_verdict_feeds_the_violation_rate() {
        let served = serde_json::json!({
            "phase":"offload-serviceable","ok":true,"violations":[],
            "detail":"2/3 turns served by a peer (local=1 fail=0)",
            "models":"x @ peer node1 | y | z @ peer node1"
        });
        let never_offloaded = serde_json::json!({
            "phase":"offload-serviceable","ok":false,
            "violations":[{"invariant":"peer_offload_serviceable",
                           "detail":"NO turn was served by a peer (local=3 fail=0 codes=200,200,200)"}],
            "models":"a | b | c"
        });
        assert_eq!(
            soak_slis(&[served.clone()])["invariant_violation_rate"],
            0.0
        );
        assert_eq!(
            soak_slis(&[never_offloaded.clone()])["invariant_violation_rate"],
            1.0,
            "a total offload outage must register as a violation, not as silence"
        );
        assert_eq!(
            soak_slis(&[served, never_offloaded])["invariant_violation_rate"],
            0.5
        );

        // The not-applicable record (fewer than 2 nodes) must NOT count as a
        // pass: it carries no `violations` key precisely so it stays out of the
        // denominator rather than diluting a real failure into looking fine.
        let skipped = serde_json::json!({
            "kind":"offload","applicable":false,"detail":"needs >=2 nodes"
        });
        let m = soak_slis(&[skipped]);
        assert_eq!(m["invariant_violation_rate"], 0.0);
        assert_eq!(
            m["load_success_rate"], 1.0,
            "a skipped probe must not be mistaken for a load sample"
        );
    }

    #[test]
    fn gate_flags_regression_past_tolerance() {
        let base: BTreeMap<String, f64> = [
            ("invariant_violation_rate".to_string(), 0.0),
            ("load_success_rate".to_string(), 1.0),
            ("load_p50_ms".to_string(), 20.0),
            ("load_p99_ms".to_string(), 80.0),
        ]
        .into_iter()
        .collect();
        // p99 jumps 80 → 400 (> 200 tol) and violation_rate 0 → 0.3 (> 0.02 tol);
        // p50 +5ms stays within its 50ms tolerance.
        let cur: BTreeMap<String, f64> = [
            ("invariant_violation_rate".to_string(), 0.3),
            ("load_success_rate".to_string(), 1.0),
            ("load_p50_ms".to_string(), 25.0),
            ("load_p99_ms".to_string(), 400.0),
        ]
        .into_iter()
        .collect();
        let (rows, first_run) = gate_slis(&cur, Some(&base));
        assert!(!first_run);
        let regressed: Vec<&str> = rows
            .iter()
            .filter(|r| r.regressed)
            .map(|r| r.name.as_str())
            .collect();
        assert!(
            regressed.contains(&"invariant_violation_rate"),
            "{regressed:?}"
        );
        assert!(regressed.contains(&"load_p99_ms"), "{regressed:?}");
        assert!(!regressed.contains(&"load_p50_ms"), "{regressed:?}");
    }

    #[test]
    fn gate_first_run_has_no_regressions() {
        let (rows, first_run) = gate_slis(&soak_slis(&[]), None);
        assert!(first_run);
        assert!(rows.iter().all(|r| !r.regressed));
    }
}
