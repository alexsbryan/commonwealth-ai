// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the reachability watchdog — see `iroh_watchdog.rs`.

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
        // `Some("none")` is what severs n0. `None` means "the config
        // names no discovery", which `from_parts` resolves to the SAFE
        // BOOTSTRAP DEFAULT of full n0 services — the opposite of what
        // this test needs. It read `None` until 2026-08-30, so the
        // endpoint carried n0's relays and homed to
        // usw1-1.relay.n0.iroh.link: healthy, never degraded, no
        // rebuild. It passed here only because this host took ~3.5s to
        // home and the assertion reads at 2s; on a CI runner with a
        // faster path to n0 it homed inside the window and the test was
        // red on every run. Assert the posture rather than trusting the
        // spelling — the endpoint must be relay-less, or the whole
        // escalation this test claims to prove never triggers.
        let cfg = RelayConfig::from_parts(vec![], Some("none")).expect("`none` is a spelling");
        assert!(
            !cfg.n0_services,
            "this test needs a relay-LESS endpoint; n0 services would home it and read healthy"
        );
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
        }) as Pin<Box<dyn std::future::Future<Output = Result<Endpoint, String>> + Send>>
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
    let handle = spawn(endpoint, rebuild, None, cfg);
    // Poll for the outcome instead of sleeping a fixed window: the
    // escalation is ~360ms of timer ticks, so a fixed sleep is only ever
    // a bet on how loaded the machine is. Waiting for the CONDITION with
    // a ceiling keeps the fast path fast and turns a slow runner into a
    // slower pass, not a red.
    let status = handle.status_arc();
    let deadline = Instant::now() + Duration::from_secs(10);
    let snap = loop {
        let snap = status.read().await.clone();
        if snap.rebuilds >= 1 {
            break snap;
        }
        assert!(
            Instant::now() < deadline,
            "watchdog never escalated to a rebuild within 10s \
             (degraded={}, relay_homed={}, rebuilds={})",
            snap.degraded,
            snap.relay_homed,
            snap.rebuilds
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

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

// ── the peer-path term ────────────────────────────────────────────
//
// Every case below is a state the 2026-09-09 capture or its trap made
// real. A gate whose failing input nobody can name is not a gate
// (ARCH §18.1), so each test names one.

fn obs(node: &str, believed_online: bool, path: Option<PeerPath>) -> ReachPathObservation {
    ReachPathObservation {
        node_id: node.to_string(),
        name: node.to_string(),
        believed_online,
        path,
    }
}

/// THE trap. A node whose peers have simply never been up has no path
/// either — and an endpoint rebuild is not the fix for bad contact info.
/// The term must stay silent forever, however long that runs.
#[test]
fn a_path_never_held_is_never_a_wedge() {
    let mut h = ReachPathHealth::default();
    for _ in 0..50 {
        let v = h.observe(&[obs("mac", true, None), obs("pi", false, None)], 3);
        assert!(!v.wedged, "no path was ever held — nothing decayed");
    }
}

/// The captured failure: an established path, then no record at all.
/// Wedged only after the streak, never on the first poll.
#[test]
fn a_held_path_that_dies_is_wedged_after_the_streak() {
    let mut h = ReachPathHealth::default();
    let live = [obs("mac", true, Some(PeerPath::Relayed))];
    let dead = [obs("mac", true, None)];
    assert!(!h.observe(&live, 3).wedged);
    assert!(
        !h.observe(&dead, 3).wedged,
        "one poll is a blip, not a wedge"
    );
    assert!(!h.observe(&dead, 3).wedged);
    assert!(
        h.observe(&dead, 3).wedged,
        "three consecutive polls: wedged"
    );
}

/// The capture's own timeline: membership marks the peer Offline ~60s
/// after the path dies. A term gated on `believed_online` would go blind
/// exactly when the wedge became permanent, so the field is recorded and
/// NOT gated on — this asserts that.
#[test]
fn membership_going_offline_does_not_blind_the_term() {
    let mut h = ReachPathHealth::default();
    assert!(
        !h.observe(&[obs("mac", true, Some(PeerPath::Direct))], 2)
            .wedged
    );
    // gossip gives up on the peer while the endpoint stays green
    assert!(!h.observe(&[obs("mac", false, None)], 2).wedged);
    assert!(h.observe(&[obs("mac", false, None)], 2).wedged);
}

/// The lift's red (pc-cmnwlth-lift-flake-selfheal): the peer is dead and
/// membership has said so, but the founder keeps re-dialing it, so iroh
/// keeps its closed connection's address Open and the record reads
/// `direct`. That record is not a path: the loss is counted on
/// membership's bound, not on when iroh's actor finally idles out.
#[test]
fn an_active_record_to_an_offline_peer_is_a_lost_path() {
    let mut h = ReachPathHealth::default();
    assert!(
        !h.observe(&[obs("mac", true, Some(PeerPath::Direct))], 2)
            .wedged
    );
    let stale = [obs("mac", false, Some(PeerPath::Direct))];
    let first = h.observe(&stale, 2);
    assert!(!first.wedged, "one poll is a blip, not a wedge");
    assert_eq!(first.active, 0);
    assert_eq!(
        first.lost,
        vec![("mac".to_string(), PeerPath::Direct, "stale")]
    );
    assert!(
        h.observe(&stale, 2).wedged,
        "a stale record must not hold the term open"
    );
    // Nor can it re-arm the term after a rebuild: only a path to a peer
    // membership believes online counts as one.
    h.rearm();
    for _ in 0..10 {
        assert!(!h.observe(&stale, 1).wedged);
    }
}

/// A solo mesh, or one where no member carries a pubkey. Nothing to be
/// reachable to, so there is no verdict to make — and the run counter is
/// left alone rather than reset, so a peer list that flickers empty
/// cannot launder a real wedge into health.
#[test]
fn no_peers_is_never_a_wedge() {
    let mut h = ReachPathHealth::default();
    h.observe(&[obs("mac", true, Some(PeerPath::Direct))], 2);
    h.observe(&[obs("mac", true, None)], 2);
    let empty = h.observe(&[], 2);
    assert!(!empty.wedged);
    assert_eq!(empty.total, 0);
    // The one bad poll before the empty one still counts.
    assert!(h.observe(&[obs("mac", true, None)], 2).wedged);
}

/// `idle` is a record with nothing active — what a decayed path looks
/// like before the record itself is dropped. Reading "a record exists" as
/// "reachable" is precisely the conflation that let a dead transport show
/// a green light.
#[test]
fn an_idle_record_is_not_an_active_path() {
    let mut h = ReachPathHealth::default();
    h.observe(&[obs("mac", true, Some(PeerPath::Mixed))], 1);
    let v = h.observe(&[obs("mac", true, Some(PeerPath::Idle))], 1);
    assert!(v.wedged);
    assert_eq!(v.known, 1, "the record is still there…");
    assert_eq!(v.active, 0, "…but nothing is flowing on it");
    assert_eq!(
        v.lost,
        vec![("mac".to_string(), PeerPath::Mixed, "idle")],
        "the record is still held, so the loss must report `idle`, not `no-record`"
    );
}

/// One reachable peer means the ENDPOINT is fine; the trouble is with the
/// other peer. An endpoint rebuild is an endpoint-wide hammer, so the term
/// fires only on the endpoint-wide symptom.
#[test]
fn one_live_peer_keeps_the_endpoint_healthy() {
    let mut h = ReachPathHealth::default();
    for _ in 0..10 {
        let v = h.observe(
            &[
                obs("mac", true, Some(PeerPath::Direct)),
                obs("pi", true, None),
            ],
            1,
        );
        assert!(!v.wedged);
        assert_eq!(v.active, 1);
    }
}

/// Recovery clears the run — a path that comes back on its own must not
/// leave the term primed to fire on the next single blip.
#[test]
fn a_recovered_path_resets_the_run() {
    let mut h = ReachPathHealth::default();
    h.observe(&[obs("mac", true, Some(PeerPath::Direct))], 3);
    h.observe(&[obs("mac", true, None)], 3);
    h.observe(&[obs("mac", true, None)], 3);
    let back = h.observe(&[obs("mac", true, Some(PeerPath::Relayed))], 3);
    assert!(!back.wedged);
    assert_eq!(back.gained, vec![("mac".to_string(), PeerPath::Relayed)]);
    assert!(
        !h.observe(&[obs("mac", true, None)], 3).wedged,
        "run restarted at 1"
    );
}

/// The anti-loop guarantee, stated as a test rather than as a comment:
/// after a rebuild the fresh endpoint holds no records, so the term must
/// be unable to fire again until a REAL path is observed again. Without
/// `rearm` this is an endless rebuild ladder on a node whose peers went
/// home for the night.
#[test]
fn a_rebuild_disarms_the_term_until_a_path_returns() {
    let mut h = ReachPathHealth::default();
    h.observe(&[obs("mac", true, Some(PeerPath::Direct))], 1);
    assert!(h.observe(&[obs("mac", true, None)], 1).wedged);
    h.rearm(); // what the ladder does after an endpoint rebuild
    for _ in 0..20 {
        assert!(
            !h.observe(&[obs("mac", true, None)], 1).wedged,
            "a disarmed term must not re-fire on the same dead peers"
        );
    }
    // A real path returning re-arms it, and only then can it fire again.
    h.observe(&[obs("mac", true, Some(PeerPath::Direct))], 1);
    assert!(h.observe(&[obs("mac", true, None)], 1).wedged);
}

/// The question the capture could not answer, now answered by the
/// verdict: what was the path carrying when it died, and did it migrate
/// first? `direct → relayed → gone` is the shape to look for.
#[test]
fn the_verdict_reports_the_path_at_the_moment_of_death() {
    let mut h = ReachPathHealth::default();
    h.observe(&[obs("mac", true, Some(PeerPath::Direct))], 1);
    let migrated = h.observe(&[obs("mac", true, Some(PeerPath::Relayed))], 1);
    assert_eq!(
        migrated.migrated,
        vec![("mac".to_string(), PeerPath::Direct, PeerPath::Relayed)]
    );
    let died = h.observe(&[obs("mac", true, None)], 1);
    assert_eq!(
        died.lost,
        vec![("mac".to_string(), PeerPath::Relayed, "no-record")],
        "the endpoint dropped the record outright — a different ending from `idle`, \
         and the one the 2026-09-09 capture finished in"
    );
}

/// End-to-end: relay-home and self-discovery both satisfied, and the
/// watchdog STILL escalates to a rebuild — driven by the peer-path term
/// alone. This is the regression that would have caught the live bug:
/// before this term the same inputs read healthy forever.
#[tokio::test]
async fn peer_path_loss_alone_escalates_to_a_rebuild() {
    use commonwealth_transport::iroh::{build_relayed_endpoint, RelayConfig, SecretKey};
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn relayless_endpoint(seed: u8) -> Endpoint {
        let cfg = RelayConfig::from_parts(vec![], Some("none")).expect("`none` is a spelling");
        let secret = SecretKey::from_bytes(&[seed; 32]);
        build_relayed_endpoint(secret, vec![b"cwth/http/0".to_vec()], &cfg)
            .await
            .expect("minimal endpoint binds offline")
    }

    let rebuilds = Arc::new(AtomicUsize::new(0));
    let rebuilds_c = rebuilds.clone();
    let rebuild: RebuildFn = Arc::new(move || {
        let rebuilds_c = rebuilds_c.clone();
        Box::pin(async move {
            rebuilds_c.fetch_add(1, Ordering::SeqCst);
            Ok(relayless_endpoint(4).await)
        }) as Pin<Box<dyn std::future::Future<Output = Result<Endpoint, String>> + Send>>
    });

    // A peer that is reachable for the first two polls and then gone —
    // the captured shape, compressed.
    let polls = Arc::new(AtomicUsize::new(0));
    let polls_c = polls.clone();
    let peer_paths: ReachPathsFn = Arc::new(move |_ep| {
        let n = polls_c.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let path = if n < 2 { Some(PeerPath::Relayed) } else { None };
            vec![ReachPathObservation {
                node_id: "mac".into(),
                name: "mac".into(),
                believed_online: n < 4,
                path,
            }]
        }) as Pin<Box<dyn std::future::Future<Output = Vec<ReachPathObservation>> + Send>>
    });

    let cfg = WatchdogConfig {
        health_poll: Duration::from_millis(40),
        unhealthy_grace: Duration::from_millis(80),
        rebuild_cooldown: Duration::from_millis(80),
        max_consecutive_rebuilds: 5,
        peer_path_bad_streak: 2,
        self_probe: false,
        // Both INBOUND terms are satisfied, so any escalation here is the
        // peer-path term's doing and nothing else's.
        relays_expected: false,
        ..Default::default()
    };
    let handle = spawn(relayless_endpoint(3).await, rebuild, Some(peer_paths), cfg);

    let status = handle.status_arc();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snap = status.read().await.clone();
        if snap.rebuilds >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "peer-path loss never escalated to a rebuild \
             (degraded={}, wedged={}, active={}/{}, polls={})",
            snap.degraded,
            snap.peer_paths_wedged,
            snap.peer_paths_active,
            snap.peer_paths_total,
            polls.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(rebuilds.load(Ordering::SeqCst) >= 1);
}
