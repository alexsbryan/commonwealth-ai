// SPDX-License-Identifier: AGPL-3.0-or-later
//! Founder reachability watchdog + self-heal.
//!
//! An idle mesh founder (no active peers, only waiting to be dialed) relies
//! entirely on iroh's internal machinery to keep its home-relay connection warm
//! and its pkarr/DNS discovery record fresh. iroh 1.0.2 DOES both forever (15s
//! relay keepalive + infinite reconnect; 5-min unconditional pkarr republish) —
//! but if one of those background loops silently wedges or dies (e.g. the pkarr
//! publisher task ending permanently), the founder becomes undialable while the
//! process runs happily, and today the ONLY recovery is a full daemon restart
//! (observed 2026-07-18: a peer couldn't join for ~1.5 days until we restarted).
//!
//! This watchdog closes that gap. It watches two health signals and escalates
//! recovery, all glassboxed at INFO so a self-heal is a logged event, never a
//! silent one:
//!
//!   1. **Relay-home** — `endpoint.home_relay_status()`; healthy = ≥1 relay
//!      connected. Detects+recovers the relay-side wedge.
//!   2. **Self-discovery probe** — periodically resolve THIS node's own id via
//!      n0 DNS; a stale/missing record means the discovery-side wedge (the
//!      pkarr-death candidate), which relay-home cannot see. Only run when n0
//!      discovery is actually configured.
//!
//! Escalation (each step only after a grace window LONGER than iroh's own 15s
//! reconnect, so we never fight iroh): `network_change()` nudge → relay bounce
//! (`remove_relay`+`insert_relay`) + discovery re-emit → **endpoint rebuild**
//! (the recovery a restart does, scoped to iroh, in-process, in seconds — the
//! only thing that revives a dead pkarr publisher task). Rebuilds are
//! cooldown-gated and capped so a persistent failure degrades gracefully rather
//! than hammering.
//!
//! The daemon owns `DaemonState` mutation, so it supplies the rebuild as a
//! [`RebuildFn`] closure; this module stays transport-mechanism-only.

use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use commonwealth_transport::iroh::{Endpoint, RelayStatus, Watcher};
use futures::StreamExt;
use tokio::sync::RwLock;
use tokio::time::Instant;
use tracing::{info, warn};

/// Live reachability snapshot the watchdog writes each cycle and the status API
/// reads (`/v1/mesh/status.founder_reachability`). Survives endpoint rebuilds —
/// the watchdog owns the shared `Arc`, so counts/last-recovery persist across a
/// rebuilt endpoint.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ReachabilityStatus {
    /// At least one home relay is connected (dialable via relay).
    pub relay_homed: bool,
    /// Currently-connected home relay URL(s), for display.
    pub relay_urls: Vec<String>,
    /// Last self-discovery probe: `Some(true)` = own record resolved,
    /// `Some(false)` = missing/stale, `None` = not run / discovery off.
    pub discovery_ok: Option<bool>,
    /// Most recent relay error (`RelayStatus::last_error`), if disconnected.
    pub last_error: Option<String>,
    /// Last self-heal action taken.
    pub last_recovery: Option<RecoveryEvent>,
    /// Total endpoint rebuilds this watchdog has performed.
    pub rebuilds: u32,
    /// True while unhealthy / mid-recovery (drives the UI "Reconnecting" state).
    pub degraded: bool,
}

/// One self-heal action, for the status surface / operator timeline.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RecoveryEvent {
    /// `"relay_nudge"` | `"relay_bounce"` | `"endpoint_rebuild"`.
    pub action: String,
    pub at_unix: u64,
    pub ok: bool,
}

/// Rebuild the iroh endpoint from scratch (last-resort self-heal). Returns the
/// NEW endpoint handle on success so the watchdog keeps polling the live one.
/// Supplied by the daemon so all `DaemonState` mutation stays in `daemon.rs`.
pub type RebuildFn = Arc<
    dyn Fn() -> Pin<Box<dyn std::future::Future<Output = Result<Endpoint, String>> + Send>>
        + Send
        + Sync,
>;

/// Tunables. Defaults chosen so escalation never races iroh's own recovery.
#[derive(Clone, Debug)]
pub struct WatchdogConfig {
    /// How often to sample health.
    pub health_poll: Duration,
    /// How often to run the self-discovery probe.
    pub probe_interval: Duration,
    /// Unhealthy this long before escalating (MUST exceed iroh's 15s ping +
    /// reconnect window so we don't fight iroh's own recovery).
    pub unhealthy_grace: Duration,
    /// Minimum gap between endpoint rebuilds.
    pub rebuild_cooldown: Duration,
    /// Consecutive rebuilds before backing off to a long retry (degraded).
    pub max_consecutive_rebuilds: u32,
    /// Consecutive `false` probes before discovery counts as wedged (avoids a
    /// single flaky resolve triggering a rebuild).
    pub discovery_bad_streak: u32,
    /// Whether the self-discovery probe runs (only when n0 DNS is configured).
    pub self_probe: bool,
    /// Whether this node is EXPECTED to be relay-homed. False for relay-less
    /// deployments (LAN-only / air-gapped / netns soak — Minimal preset, no n0)
    /// where peers dial direct addrs and having no home relay is NORMAL, not a
    /// wedge. When false, relay-home is NOT a health signal (else a healthy
    /// relay-less founder would look permanently unhealthy and rebuild-loop).
    pub relays_expected: bool,
    /// CHAOS/soak only: when set, the watchdog periodically injects a
    /// reachability wedge to exercise the self-heal path end-to-end (detect →
    /// escalate → recover). `None` in production. Enabled via
    /// `SOVEREIGN_MESH_WATCHDOG_CHAOS_DROP_SECS`.
    pub chaos_drop_interval: Option<Duration>,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            health_poll: Duration::from_secs(20),
            probe_interval: Duration::from_secs(300),
            unhealthy_grace: Duration::from_secs(90),
            rebuild_cooldown: Duration::from_secs(600),
            max_consecutive_rebuilds: 3,
            discovery_bad_streak: 2,
            self_probe: true,
            relays_expected: true,
            chaos_drop_interval: None,
        }
    }
}

impl WatchdogConfig {
    /// Load overrides from the environment (defaults otherwise), so soaks and a
    /// live demo can speed the watchdog up and inject relay-drop faults WITHOUT
    /// a rebuild. `self_probe` is set by the caller (it depends on whether n0
    /// discovery is configured), not by env.
    ///   `SOVEREIGN_MESH_WATCHDOG_POLL_SECS`
    ///   `SOVEREIGN_MESH_WATCHDOG_GRACE_SECS`
    ///   `SOVEREIGN_MESH_WATCHDOG_COOLDOWN_SECS`
    ///   `SOVEREIGN_MESH_WATCHDOG_CHAOS_DROP_SECS` (chaos: drop home relay every N s)
    pub fn from_env() -> Self {
        let mut c = Self::default();
        let secs = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u64>().ok());
        if let Some(s) = secs("SOVEREIGN_MESH_WATCHDOG_POLL_SECS") {
            c.health_poll = Duration::from_secs(s.max(1));
        }
        if let Some(s) = secs("SOVEREIGN_MESH_WATCHDOG_GRACE_SECS") {
            c.unhealthy_grace = Duration::from_secs(s);
        }
        if let Some(s) = secs("SOVEREIGN_MESH_WATCHDOG_COOLDOWN_SECS") {
            c.rebuild_cooldown = Duration::from_secs(s);
        }
        if let Some(s) = secs("SOVEREIGN_MESH_WATCHDOG_CHAOS_DROP_SECS") {
            c.chaos_drop_interval = Some(Duration::from_secs(s.max(1)));
        }
        c
    }
}

/// Abort-on-drop handle (same pattern as `GossipHandle`); tying it to
/// `DaemonState::Running` means leaving/stopping the mesh also stops the
/// watchdog. Carries the shared status so `founder_reachability()` can read it.
pub struct WatchdogHandle {
    _task: tokio::task::JoinHandle<()>,
    status: Arc<RwLock<ReachabilityStatus>>,
}

impl Drop for WatchdogHandle {
    fn drop(&mut self) {
        self._task.abort();
    }
}

impl WatchdogHandle {
    /// A clone of the shared status `Arc` so a caller can drop the daemon state
    /// lock BEFORE awaiting the read (the codebase's clone-out-then-await rule).
    pub fn status_arc(&self) -> Arc<RwLock<ReachabilityStatus>> {
        self.status.clone()
    }
}

/// Spawn the watchdog against `endpoint`, using `rebuild` for the last-resort
/// endpoint rebuild. Call once per daemon start (only when iroh is enabled).
pub fn spawn(endpoint: Endpoint, rebuild: RebuildFn, cfg: WatchdogConfig) -> WatchdogHandle {
    let status = Arc::new(RwLock::new(ReachabilityStatus::default()));
    let task = tokio::spawn(run(endpoint, rebuild, cfg, status.clone()));
    WatchdogHandle {
        _task: task,
        status,
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn record_recovery(status: &Arc<RwLock<ReachabilityStatus>>, action: &str, ok: bool) {
    let mut s = status.write().await;
    s.last_recovery = Some(RecoveryEvent {
        action: action.to_string(),
        at_unix: now_unix(),
        ok,
    });
}

/// Resolve THIS node's own id via the configured address-lookup (n0 DNS/pkarr)
/// and report whether at least one address record came back. A wedged/dead
/// pkarr publisher makes the record go stale/absent → `false`. Bounded so a
/// hung resolver can't stall the watchdog. Only meaningful when discovery is
/// configured (gated by `cfg.self_probe`).
async fn self_discovery_probe(endpoint: &Endpoint) -> bool {
    let lookup = match endpoint.address_lookup() {
        Ok(l) => l,
        // No discovery configured — not a failure signal.
        Err(_) => return true,
    };
    let mut stream = lookup.resolve(endpoint.id());
    let deadline = Duration::from_secs(10);
    let fut = async {
        while let Some(item) = stream.next().await {
            // `Ok(Ok(_))` = a resolved address item; anything else is an inline
            // service error or terminal failure we don't count as "found".
            if matches!(item, Ok(Ok(_))) {
                return true;
            }
        }
        false
    };
    matches!(tokio::time::timeout(deadline, fut).await, Ok(true))
}

/// Bounce every currently-known home relay: `remove_relay` returns the exact
/// `Arc<RelayConfig>` `insert_relay` wants, so this round-trips with zero
/// reconstruction and forces a fresh `ActiveRelayActor` (+ an addr republish).
async fn bounce_relays(endpoint: &Endpoint, relays: &[RelayStatus]) {
    for r in relays {
        let url = r.url().clone();
        if let Some(cfg) = endpoint.remove_relay(&url).await {
            endpoint.insert_relay(url.clone(), cfg).await;
            info!(relay = %url, "iroh(mesh) watchdog: bounced relay connection");
        }
    }
}

async fn run(
    mut endpoint: Endpoint,
    rebuild: RebuildFn,
    cfg: WatchdogConfig,
    status: Arc<RwLock<ReachabilityStatus>>,
) {
    info!(
        health_poll_secs = cfg.health_poll.as_secs(),
        probe_interval_secs = cfg.probe_interval.as_secs(),
        unhealthy_grace_secs = cfg.unhealthy_grace.as_secs(),
        self_probe = cfg.self_probe,
        "iroh(mesh) watchdog: started — founder reachability self-heal armed"
    );

    let mut relay_watch = endpoint.home_relay_status();
    let mut unhealthy_since: Option<Instant> = None;
    let mut escalation: u8 = 0;
    let mut last_rebuild: Option<Instant> = None;
    let mut consecutive_rebuilds: u32 = 0;
    let mut total_rebuilds: u32 = 0;
    let mut discovery_bad_run: u32 = 0;
    let mut next_probe = Instant::now() + cfg.health_poll; // first probe shortly after start
    let mut cached_discovery_ok: Option<bool> = None;
    let mut next_chaos = cfg.chaos_drop_interval.map(|d| Instant::now() + d);
    // CHAOS/soak only: a simulated discovery-side wedge (see below).
    let mut chaos_unhealthy = false;

    loop {
        tokio::time::sleep(cfg.health_poll).await;

        // ── 0. CHAOS (soak/demo only): inject a reachability wedge. Off in
        // production (chaos_drop_interval is None). NOTE: iroh's relay layer is
        // self-healing — removing relays just makes it re-home on another n0
        // relay, so relay-home CANNOT be wedged from here. The real ~1.5-day
        // outage was the DISCOVERY (pkarr) side, which relay resilience doesn't
        // cover and which ONLY an endpoint rebuild recovers (a dead pkarr
        // publisher task). So chaos faithfully simulates THAT: force unhealthy
        // until a rebuild — nudge + relay-bounce won't clear it; only the
        // rebuild does (it clears the flag below), exactly like the real bug.
        if let (Some(iv), Some(due)) = (cfg.chaos_drop_interval, next_chaos) {
            if Instant::now() >= due {
                next_chaos = Some(Instant::now() + iv);
                chaos_unhealthy = true;
                warn!("iroh(mesh) watchdog: CHAOS — injected reachability wedge (simulated discovery/pkarr failure; only an endpoint rebuild recovers)");
            }
        }

        // ── 1. relay-home health ───────────────────────────────────
        let relays = relay_watch.get();
        let relay_homed = relays.iter().any(|r| r.is_connected());
        let relay_urls: Vec<String> = relays
            .iter()
            .filter(|r| r.is_connected())
            .map(|r| r.url().to_string())
            .collect();
        let last_error = relays
            .iter()
            .find_map(|r| r.last_error().map(|e| e.to_string()));

        // ── 2. self-discovery probe (periodic) ─────────────────────
        if cfg.self_probe && Instant::now() >= next_probe {
            next_probe = Instant::now() + cfg.probe_interval;
            let ok = self_discovery_probe(&endpoint).await;
            cached_discovery_ok = Some(ok);
            if ok {
                discovery_bad_run = 0;
            } else {
                discovery_bad_run += 1;
                warn!(
                    streak = discovery_bad_run,
                    "iroh(mesh) watchdog: self-discovery probe found no record for own id \
                     (pkarr/discovery may be wedged)"
                );
            }
        }
        // Discovery counts as wedged only after a sustained streak.
        let discovery_wedged = discovery_bad_run >= cfg.discovery_bad_streak;
        // Relay-home is only a health requirement when relays are expected; a
        // relay-less node (LAN/air-gapped) is reachable via direct addrs.
        let relay_ok = !cfg.relays_expected || relay_homed;
        // `chaos_unhealthy` (soak/demo) simulates the discovery-side wedge that
        // relay-home can't see; cleared only by a rebuild (below).
        let healthy = relay_ok && !discovery_wedged && !chaos_unhealthy;

        // ── 3. publish snapshot for the status API ─────────────────
        {
            let mut s = status.write().await;
            s.relay_homed = relay_homed;
            s.relay_urls = relay_urls;
            s.discovery_ok = cached_discovery_ok;
            s.last_error = last_error;
            s.rebuilds = total_rebuilds;
            s.degraded = !healthy;
        }

        // ── 4. decision ────────────────────────────────────────────
        if healthy {
            if unhealthy_since.is_some() {
                info!("iroh(mesh) watchdog: reachability RECOVERED (relay-homed, discovery ok)");
            }
            unhealthy_since = None;
            escalation = 0;
            consecutive_rebuilds = 0;
            continue;
        }

        let since = *unhealthy_since.get_or_insert_with(Instant::now);
        if since.elapsed() < cfg.unhealthy_grace {
            warn!(
                relay_homed,
                discovery_wedged,
                grace_secs = cfg.unhealthy_grace.as_secs(),
                "iroh(mesh) watchdog: unhealthy — within grace, waiting for iroh self-recovery"
            );
            continue;
        }

        // ── 5. escalate (staged across ticks) ──────────────────────
        match escalation {
            0 => {
                info!("iroh(mesh) watchdog: ESCALATE 1/3 — network_change() nudge");
                endpoint.network_change().await;
                record_recovery(&status, "relay_nudge", true).await;
                escalation = 1;
                unhealthy_since = Some(Instant::now());
            }
            1 => {
                // Relay bounce rebuilds each ActiveRelayActor and forces an addr
                // republish (relay change → publish_my_addr). It does NOT revive
                // a dead pkarr publisher task — only the rebuild below does, which
                // is why the self-discovery probe escalates straight toward it.
                info!("iroh(mesh) watchdog: ESCALATE 2/3 — relay bounce (fresh relay + republish)");
                bounce_relays(&endpoint, &relays).await;
                record_recovery(&status, "relay_bounce", true).await;
                escalation = 2;
                unhealthy_since = Some(Instant::now());
            }
            _ => {
                if consecutive_rebuilds >= cfg.max_consecutive_rebuilds {
                    warn!(
                        consecutive_rebuilds,
                        "iroh(mesh) watchdog: rebuild cap reached — staying degraded and backing \
                         off (a persistent bind/relay failure may need operator attention)"
                    );
                    tokio::time::sleep(cfg.rebuild_cooldown).await;
                    consecutive_rebuilds = 0;
                    unhealthy_since = Some(Instant::now());
                    continue;
                }
                if last_rebuild.is_some_and(|t| t.elapsed() < cfg.rebuild_cooldown) {
                    continue; // cooling down between rebuilds
                }
                info!(
                    consecutive_rebuilds,
                    "iroh(mesh) watchdog: ESCALATE 3/3 — rebuilding iroh endpoint (in-process, \
                     no daemon restart)"
                );
                match rebuild().await {
                    Ok(new_ep) => {
                        endpoint = new_ep;
                        relay_watch = endpoint.home_relay_status();
                        last_rebuild = Some(Instant::now());
                        consecutive_rebuilds += 1;
                        total_rebuilds += 1;
                        chaos_unhealthy = false; // the rebuild resolves the simulated wedge
                        record_recovery(&status, "endpoint_rebuild", true).await;
                        info!(
                            total_rebuilds,
                            "iroh(mesh) watchdog: endpoint rebuilt — re-evaluating from a fresh \
                             relay/discovery registration"
                        );
                        // Give the fresh endpoint a clean grace window to home.
                        escalation = 0;
                        unhealthy_since = None;
                        discovery_bad_run = 0;
                    }
                    Err(e) => {
                        warn!(error = %e, "iroh(mesh) watchdog: endpoint rebuild FAILED — retry after cooldown");
                        record_recovery(&status, "endpoint_rebuild", false).await;
                        last_rebuild = Some(Instant::now());
                        consecutive_rebuilds += 1;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_grace_exceeds_iroh_reconnect_window() {
        // The whole point of the grace window is to NOT fight iroh's own 15s
        // ping + reconnect. Guard against a future edit shrinking it below that.
        let cfg = WatchdogConfig::default();
        assert!(
            cfg.unhealthy_grace >= Duration::from_secs(30),
            "grace must exceed iroh's ~15s self-recovery window"
        );
        assert!(cfg.rebuild_cooldown >= cfg.unhealthy_grace);
    }

    #[test]
    fn status_serializes_with_defaults() {
        // The status DTO rides the /v1/mesh/status wire (serde default on the
        // MeshStatus side); a default must serialize cleanly.
        let s = ReachabilityStatus::default();
        let v = serde_json::to_value(&s).expect("serialize");
        assert_eq!(v["relay_homed"], serde_json::json!(false));
        assert_eq!(v["degraded"], serde_json::json!(false));
        assert_eq!(v["rebuilds"], serde_json::json!(0));
    }

    /// End-to-end escalation, deterministic and offline: a Minimal-preset
    /// endpoint (relays DISABLED, no n0) is NEVER relay-homed, so the watchdog
    /// sees sustained unhealth and must escalate all the way to a rebuild. We
    /// assert the rebuild closure fires and the status reads `degraded` —
    /// proving nudge → bounce → rebuild wiring without any network.
    #[tokio::test]
    async fn watchdog_escalates_to_rebuild_when_never_relay_homed() {
        use commonwealth_transport::iroh::{build_relayed_endpoint, RelayConfig, SecretKey};
        use std::sync::atomic::{AtomicUsize, Ordering};

        async fn minimal_endpoint(seed: u8) -> Endpoint {
            let cfg = RelayConfig::from_parts(vec![], None); // n0_services=false → Minimal
            let secret = SecretKey::from_bytes(&[seed; 32]);
            build_relayed_endpoint(secret, vec![b"cwth/http/0".to_vec()], &cfg)
                .await
                .expect("minimal endpoint binds offline")
        }

        let endpoint = minimal_endpoint(1).await;
        let rebuilds = Arc::new(AtomicUsize::new(0));
        let rebuilds_c = rebuilds.clone();
        let rebuild: RebuildFn = Arc::new(move || {
            let rebuilds_c = rebuilds_c.clone();
            Box::pin(async move {
                rebuilds_c.fetch_add(1, Ordering::SeqCst);
                Ok(minimal_endpoint(2).await)
            })
                as Pin<Box<dyn std::future::Future<Output = Result<Endpoint, String>> + Send>>
        });

        let cfg = WatchdogConfig {
            health_poll: Duration::from_millis(40),
            unhealthy_grace: Duration::from_millis(80),
            rebuild_cooldown: Duration::from_millis(80),
            max_consecutive_rebuilds: 5,
            self_probe: false,
            // relays_expected defaults true → the Minimal endpoint (never
            // relay-homed) reads unhealthy and must escalate to a rebuild.
            ..Default::default()
        };
        let handle = spawn(endpoint, rebuild, cfg);
        tokio::time::sleep(Duration::from_secs(2)).await;

        let snap = handle.status_arc().read().await.clone();
        assert!(
            snap.degraded,
            "a never-relay-homed endpoint must read degraded"
        );
        assert!(
            rebuilds.load(Ordering::SeqCst) >= 1,
            "watchdog should have escalated through nudge + bounce to at least one rebuild"
        );
        assert!(snap.rebuilds >= 1, "status should record the rebuild(s)");
    }
}
