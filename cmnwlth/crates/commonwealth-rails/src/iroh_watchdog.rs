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
//!   3. **Peer paths** — do we still hold a live path to ANYONE? Added
//!      2026-09-09 after a live capture (note `a3f3fbff`) showed the first two
//!      signals are structurally blind to the failure that actually bit: both
//!      are INBOUND questions — "is a relay connected to me", "can I resolve
//!      my own id" — and neither moves when an ESTABLISHED outbound path to a
//!      peer degrades and dies. On RuggedFox, `reach_ms` to the Mac climbed
//!      131 → 1106 over three minutes, went silent, and the peer was marked
//!      Offline 60s later; ten minutes on, `svrn mesh transport` read "no
//!      endpoint record yet" for EVERY peer while `self_reachability` read
//!      `relay_homed: true, discovery_ok: true, degraded: false, rebuilds: 0`.
//!      A green light over a dead transport is the shape this term removes.
//!
//!      It is gated on HAVING HAD a path and lost it, never on "no path right
//!      now" — a node whose peers are all legitimately asleep has no path
//!      either, and rebuilding for that is the same trap `relays_expected`
//!      exists for one layer up. The gate is also ONE-SHOT: a rebuild re-arms
//!      it (`ReachPathHealth::rearm`), so a loss event can drive the ladder at
//!      most once until a real path is observed again.
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
use std::time::Duration;

use commonwealth_transport::iroh::{Endpoint, PeerPath, RelayStatus, Watcher};
use futures::StreamExt;
use tokio::sync::RwLock;
use tokio::time::Instant;
use tracing::{info, warn};
/// Live reachability snapshot the watchdog writes each cycle and the status API
/// reads (`/v1/mesh/status.self_reachability`), plus the self-heal event it
/// records. The watchdog owns the shared `Arc`, so counts/last-recovery
/// persist across a rebuilt endpoint.
///
/// The producer's half of a wire record: the svrn client decodes it as
/// `sovereign_contracts::daemon_wire::ReachabilityStatus`, which this crate may
/// not name (its `[[forbid]] → sovereign-*` row), so the fields are spelled
/// here the same way and sovereign-mesh's shim converts at the daemon's one
/// read site (pb-rails-parity).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ReachabilityStatus {
    /// At least one home relay is connected (dialable via relay).
    pub relay_homed: bool,
    /// Currently-connected home relay URL(s), for display.
    pub relay_urls: Vec<String>,
    /// Last self-discovery probe: `Some(true)` resolved, `Some(false)`
    /// missing/stale, `None` not run / discovery off.
    pub discovery_ok: Option<bool>,
    /// Most recent relay error, if disconnected.
    pub last_error: Option<String>,
    /// Last self-heal action taken.
    pub last_recovery: Option<RecoveryEvent>,
    /// Total endpoint rebuilds this watchdog has performed.
    pub rebuilds: u32,
    /// Peers the peer-path term looked at on its last poll.
    #[serde(default)]
    pub peer_paths_total: usize,
    /// How many of those carry an ACTIVE path.
    #[serde(default)]
    pub peer_paths_active: usize,
    /// This endpoint HELD a live path and now holds none, past the streak.
    #[serde(default)]
    pub peer_paths_wedged: bool,
    /// True while unhealthy / mid-recovery.
    pub degraded: bool,
}

/// One self-heal action, for the status surface / operator timeline.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RecoveryEvent {
    /// `"relay_nudge"` | `"relay_bounce"` | `"endpoint_rebuild"`.
    pub action: String,
    /// Unix seconds when the action ran.
    pub at_unix: u64,
    /// The action succeeded.
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

/// One peer's live path as the daemon sees it, injected once per poll via
/// [`ReachPathsFn`]. The watchdog stays transport-mechanism-only: it does not
/// know what a mesh member is, only that something out there was reachable and
/// now is not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReachPathObservation {
    /// Short node id — for the log line, and the key the term folds on.
    pub node_id: String,
    /// Display name, for the same log line.
    pub name: String,
    /// What membership currently believes about this peer. It can only take
    /// a path AWAY, never supply one: the 2026-09-09 capture shows membership
    /// marks a peer Offline ~60s after the path dies, so a term that needed
    /// "believed online" to count a loss would go blind exactly when the
    /// wedge becomes permanent. But an Active path to a peer membership has
    /// given up on is a stale record, not a path: iroh 1.0.2 leaves an
    /// address Open when its connection closes (`handle_connection_close`
    /// never abandons it) until the remote actor idles 60s past the last
    /// dial, and every re-dial to the dead peer restarts that clock
    /// (pc-cmnwlth-lift-flake-selfheal: 147s after a kill in one lift run).
    /// Offline is gossip's own bound (`offline_threshold_secs`), which
    /// re-dials to a dead peer cannot extend.
    pub believed_online: bool,
    /// The endpoint's classification, or `None` when it holds NO `remote_info`
    /// record for this peer at all — a different fact from
    /// [`PeerPath::Idle`], and the one the capture ended in.
    pub path: Option<PeerPath>,
}

impl ReachPathObservation {
    fn active_path(&self) -> Option<PeerPath> {
        self.path.filter(|p| p.is_active() && self.believed_online)
    }

    /// What the log line prints for a path, including the no-record case
    /// and an Active record to a peer membership holds Offline.
    fn path_label(&self) -> &'static str {
        match self.path {
            Some(p) if p.is_active() && !self.believed_online => "stale",
            Some(p) => p.as_str(),
            None => "no-record",
        }
    }
}

/// Observe every peer's live path on the CURRENT endpoint. Takes the endpoint
/// because the watchdog swaps its own handle on rebuild and the health term
/// must judge the endpoint it is actually holding. Supplied by the daemon (it
/// owns the membership list), same injection shape as [`RebuildFn`]. `None` at
/// spawn disables the term entirely — no observations, never wedged.
pub type ReachPathsFn = Arc<
    dyn Fn(Endpoint) -> Pin<Box<dyn std::future::Future<Output = Vec<ReachPathObservation>> + Send>>
        + Send
        + Sync,
>;

/// What one poll of the peer-path term concluded. Returned rather than logged
/// in place, so the fold below is a pure function with failing inputs a test
/// can name (ARCH §18.1) and all the tracing stays in [`run`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReachPathVerdict {
    /// The health term: we HELD an active path, hold none now, and have held
    /// none for `peer_path_bad_streak` consecutive polls.
    wedged: bool,
    /// Peers observed this poll.
    total: usize,
    /// Peers the endpoint holds any record for.
    known: usize,
    /// Peers with an ACTIVE path.
    active: usize,
    /// Peers whose active path vanished this poll: the path it was on when it
    /// died, and what the endpoint holds for it NOW. THIS is the answer the
    /// capture could not give — "was the dying path relayed or direct at the
    /// moment it died?" — and the second half separates the two ways a path
    /// ends, which need different explanations: `idle` is a record iroh still
    /// holds with nothing active on it, `no-record` is the record itself
    /// dropped. The 2026-09-09 capture ended in `no-record` for every peer and
    /// could not say whether it passed through `idle` on the way.
    lost: Vec<(String, PeerPath, &'static str)>,
    /// Peers that gained an active path this poll.
    gained: Vec<(String, PeerPath)>,
    /// Peers whose active path changed kind — `direct` → `relayed` is the
    /// documented precursor to the `reach_ms` ramp, so it is worth a line of
    /// its own rather than being folded into "still active".
    migrated: Vec<(String, PeerPath, PeerPath)>,
}

/// The peer-path health term's state across polls.
///
/// The whole difficulty is that "no path to anyone" is BOTH the wedge and the
/// normal state of a node whose peers are asleep. What separates them is
/// history: a wedge is a path we HELD and lost. So the term is armed only by
/// observing a real active path, and a rebuild disarms it again — which makes
/// it one-shot per loss event and structurally unable to rebuild-loop.
#[derive(Debug, Default)]
struct ReachPathHealth {
    /// Last ACTIVE path per peer, so a loss can report what died.
    last_active: std::collections::HashMap<String, PeerPath>,
    /// An active path has been observed since the term was last armed.
    seen_active: bool,
    /// Consecutive polls with peers present and not one active path.
    bad_run: u32,
}

impl ReachPathHealth {
    /// Fold one poll's observations into the term.
    fn observe(&mut self, obs: &[ReachPathObservation], streak: u32) -> ReachPathVerdict {
        let mut v = ReachPathVerdict {
            total: obs.len(),
            ..Default::default()
        };
        for o in obs {
            if o.path.is_some() {
                v.known += 1;
            }
            match o.active_path() {
                Some(now) => {
                    v.active += 1;
                    match self.last_active.insert(o.node_id.clone(), now) {
                        None => v.gained.push((o.name.clone(), now)),
                        Some(prev) if prev != now => v.migrated.push((o.name.clone(), prev, now)),
                        Some(_) => {}
                    }
                }
                None => {
                    if let Some(prev) = self.last_active.remove(&o.node_id) {
                        v.lost.push((o.name.clone(), prev, o.path_label()));
                    }
                }
            }
        }
        if v.active > 0 {
            self.seen_active = true;
            self.bad_run = 0;
        } else if !obs.is_empty() && self.seen_active {
            self.bad_run = self.bad_run.saturating_add(1);
            v.wedged = self.bad_run >= streak.max(1);
        }
        // Two deliberate non-cases, both fail-open:
        //   * `obs` empty — a solo mesh, or nobody carrying a pubkey. Nothing
        //     to be reachable TO, so no verdict and the run counter is left
        //     alone rather than reset.
        //   * `!seen_active` — we never had a path to lose. That is a DIAL
        //     problem (bad contact info, peer never up), and an endpoint
        //     rebuild is not its fix.
        v
    }

    /// Re-arm after an endpoint rebuild.
    ///
    /// The fresh endpoint holds no `remote_info` for anyone, so without this
    /// the term would keep reading "had a path, has none" and drive the ladder
    /// straight into the next rebuild. Clearing `seen_active` makes the term
    /// one-shot per loss event: it cannot fire again until a real active path
    /// is observed again.
    fn rearm(&mut self) {
        self.last_active.clear();
        self.seen_active = false;
        self.bad_run = 0;
    }
}

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
    /// Consecutive polls holding zero active peer paths — having held one —
    /// before the peer-path term counts as wedged. `× health_poll` is the
    /// detection window: 3 × 20s = 60s by default, deliberately the same order
    /// as gossip's own offline threshold, so one slow round is never enough.
    pub peer_path_bad_streak: u32,
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
            peer_path_bad_streak: 3,
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
/// watchdog. Carries the shared status so `self_reachability()` can read it.
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
/// endpoint rebuild and `peer_paths` for the peer-path health term. Call once
/// per daemon start (only when iroh is enabled). `peer_paths: None` runs the
/// two inbound terms only — the pre-2026-09-09 behaviour.
pub fn spawn(
    endpoint: Endpoint,
    rebuild: RebuildFn,
    peer_paths: Option<ReachPathsFn>,
    cfg: WatchdogConfig,
) -> WatchdogHandle {
    let status = Arc::new(RwLock::new(ReachabilityStatus::default()));
    let task = tokio::spawn(run(endpoint, rebuild, peer_paths, cfg, status.clone()));
    WatchdogHandle {
        _task: task,
        status,
    }
}

async fn record_recovery(status: &Arc<RwLock<ReachabilityStatus>>, action: &str, ok: bool) {
    let mut s = status.write().await;
    s.last_recovery = Some(RecoveryEvent {
        action: action.to_string(),
        at_unix: commonwealth_core::clock::unix_now_secs(),
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
    peer_paths: Option<ReachPathsFn>,
    cfg: WatchdogConfig,
    status: Arc<RwLock<ReachabilityStatus>>,
) {
    info!(
        health_poll_secs = cfg.health_poll.as_secs(),
        probe_interval_secs = cfg.probe_interval.as_secs(),
        unhealthy_grace_secs = cfg.unhealthy_grace.as_secs(),
        self_probe = cfg.self_probe,
        peer_path_term = peer_paths.is_some(),
        peer_path_bad_streak = cfg.peer_path_bad_streak,
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
    let mut peer_path_health = ReachPathHealth::default();
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
        // ── 2b. peer paths (every poll — `remote_info` is local state) ──
        // The OUTBOUND term. Logged whether or not it fires: a transport that
        // decays over minutes leaves no trace in a signal sampled only when
        // something is already wrong, and the 2026-09-09 capture had to be
        // reconstructed from gossip timings for exactly that reason.
        let mut peer_verdict = ReachPathVerdict::default();
        if let Some(observe) = peer_paths.as_ref() {
            let obs = observe(endpoint.clone()).await;
            peer_verdict = peer_path_health.observe(&obs, cfg.peer_path_bad_streak);
            for (name, was, now) in &peer_verdict.lost {
                warn!(
                    peer = %name,
                    path_at_death = was.as_str(),
                    now = now,
                    peers_active = peer_verdict.active,
                    peers_total = peer_verdict.total,
                    "iroh(mesh) watchdog: peer path LOST — the endpoint no longer holds an \
                     active path to this peer"
                );
            }
            for (name, now) in &peer_verdict.gained {
                info!(peer = %name, path = now.as_str(), "iroh(mesh) watchdog: peer path established");
            }
            for (name, from, to) in &peer_verdict.migrated {
                info!(
                    peer = %name,
                    from = from.as_str(),
                    to = to.as_str(),
                    "iroh(mesh) watchdog: peer path migrated"
                );
            }
            // The per-poll census: one line per peer with what the endpoint
            // holds and what membership believes, so the two can be compared
            // directly instead of inferred from a gap in the log.
            //
            // Its OWN tracing target, because the transitions above are the
            // alarm and this is the continuous record — the thing you want
            // running all day during a decay capture, and the thing you do not
            // want to pay for by turning every `sovereign_mesh` debug line on.
            // Enable with `mesh.peer_path=debug` in RUST_LOG; dark otherwise,
            // like every other custom target in this codebase.
            for o in &obs {
                tracing::debug!(
                    target: "mesh.peer_path",
                    peer = %o.name,
                    node = %o.node_id,
                    path = o.path_label(),
                    record = o.path.is_some(),
                    believed_online = o.believed_online,
                    "iroh(mesh) watchdog: peer path census"
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
        //
        // The third term is the OUTBOUND one. Both terms above ask whether the
        // world can reach us; `peer_verdict.wedged` asks whether we can still
        // reach anyone, which is the failure that produced this term (note
        // `a3f3fbff`) and the one they cannot see.
        let healthy = relay_ok && !discovery_wedged && !chaos_unhealthy && !peer_verdict.wedged;

        // ── 3. publish snapshot for the status API ─────────────────
        {
            let mut s = status.write().await;
            s.relay_homed = relay_homed;
            s.relay_urls = relay_urls;
            s.discovery_ok = cached_discovery_ok;
            s.last_error = last_error;
            s.rebuilds = total_rebuilds;
            s.peer_paths_total = peer_verdict.total;
            s.peer_paths_active = peer_verdict.active;
            s.peer_paths_wedged = peer_verdict.wedged;
            s.degraded = !healthy;
        }

        // ── 4. decision ────────────────────────────────────────────
        if healthy {
            if unhealthy_since.is_some() {
                info!(
                    peers_active = peer_verdict.active,
                    peers_total = peer_verdict.total,
                    "iroh(mesh) watchdog: reachability RECOVERED (relay-homed, discovery ok, \
                     peer paths ok)"
                );
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
                peer_paths_wedged = peer_verdict.wedged,
                peers_active = peer_verdict.active,
                peers_total = peer_verdict.total,
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
                                                 // A fresh endpoint holds no peer records, so the term
                                                 // must be re-armed or it would read "had a path, has
                                                 // none" forever and drive the next rebuild, and the
                                                 // next. This is what makes the term one-shot per loss
                                                 // event (see `ReachPathHealth::rearm`).
                        peer_path_health.rearm();
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

/// Every member the iroh endpoint could hold a path to, paired with what
/// membership believes and what the endpoint actually holds — the watchdog's
/// peer-path health term (note `a3f3fbff`).
///
/// A free function taking the endpoint rather than a method on
/// `EmbeddedDaemon` because the watchdog swaps its own endpoint handle on
/// rebuild: the health judgement must be made against the endpoint the
/// watchdog is holding, not against whatever the daemon state last recorded.
/// Members without a pubkey are not iroh-dialable and are left out entirely —
/// counting them would make an IP-only peer look like a lost path.
pub async fn observe_peer_paths(
    mesh: &tokio::sync::RwLock<commonwealth_core::mesh::Mesh>,
    self_id: commonwealth_core::ids::NodeId,
    endpoint: &Endpoint,
) -> Vec<crate::iroh_watchdog::ReachPathObservation> {
    // Clone the members out before awaiting — the codebase's
    // clone-out-then-await rule; `peer_path_snapshot` awaits per peer.
    let members: Vec<commonwealth_core::mesh::MemberRecord> = {
        let mesh = mesh.read().await;
        mesh.members
            .values()
            .filter(|m| m.node_id != self_id && m.node_pubkey.is_some())
            .cloned()
            .collect()
    };
    let mut out = Vec::with_capacity(members.len());
    for m in members {
        let pubkey = m.node_pubkey.expect("filtered to Some above");
        let path = match commonwealth_transport::iroh::PublicKey::from_bytes(&pubkey.0) {
            Ok(id) => commonwealth_transport::iroh::peer_path_snapshot(endpoint, id)
                .await
                .map(|s| s.path),
            // A member whose stored pubkey is not a valid Ed25519 point can
            // never be dialed by key — a roster fault, not a transport one.
            // It reads as `None` (no record) so the census still accounts for
            // every member, and it SAYS SO rather than being absorbed into
            // the no-record population it is indistinguishable from (§18.3).
            // Harmless to the health term either way: a peer that could never
            // be dialed never enters `last_active`, so it can only ever add
            // to the total, never report a loss or arm the term.
            Err(e) => {
                tracing::warn!(
                    peer = %m.node_id,
                    name = %m.name,
                    error = %e,
                    "iroh(mesh): member pubkey is not a valid endpoint id — not dialable by \
                     key; counted as no-path, never as a lost path"
                );
                None
            }
        };
        out.push(crate::iroh_watchdog::ReachPathObservation {
            node_id: m.node_id.to_string(),
            name: m.name.clone(),
            believed_online: m.status != commonwealth_core::mesh::NodeStatus::Offline,
            path,
        });
    }
    out
}

#[cfg(test)]
#[path = "iroh_watchdog/tests.rs"]
mod tests;
