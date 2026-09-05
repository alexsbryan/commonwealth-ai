// SPDX-License-Identifier: AGPL-3.0-or-later
//! Does a failed mesh_store replication push reach an operator?
//!
//! Before order `mesh-scale-t0` item 1 the answer was no. Both failure
//! branches of the `/internal/app/state` push (`gossip.rs`) logged at
//! `debug!`, and the shipped daemon sets no `RUST_LOG` — so mesh_store
//! replication could stop entirely (413 because the full snapshot
//! outgrew the receiver's 8 MiB body limit, or the 3s POST timeout on a
//! relay link) while every operator surface stayed green. That silence
//! is what `MESH_SCALE_100_USERS_1000_CORPORA.md` §7.2 calls the
//! "same debug-level silence" behind the replication cliff.
//!
//! This drives a REAL gossip round against a REAL peer HTTP server that
//! rejects the push, with a subscriber capturing at WARN — the level an
//! operator actually sees — and asserts the failure is reported.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use commonwealth_api::state::AppState;
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_state::MeshStore;
use sovereign_mesh::gossip;
use tracing_subscriber::fmt::MakeWriter;

// ─── tracing capture (same shape as tests/injection_order.rs) ───

#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CaptureWriter {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Capture at WARN — deliberately NOT at debug. The whole finding is
/// that these events existed at a level the shipped daemon never emits,
/// so a test that captures at DEBUG would have passed on the broken
/// code and proved nothing.
fn capture_warns(buf: Arc<Mutex<Vec<u8>>>) -> tracing::subscriber::DefaultGuard {
    let subscriber = tracing_subscriber::fmt()
        .with_writer(CaptureWriter(buf))
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .finish();
    tracing::subscriber::set_default(subscriber)
}

/// Same capture at DEBUG, for the glassbox assertion below.
fn capture_debug(buf: Arc<Mutex<Vec<u8>>>) -> tracing::subscriber::DefaultGuard {
    let subscriber = tracing_subscriber::fmt()
        .with_writer(CaptureWriter(buf))
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .finish();
    tracing::subscriber::set_default(subscriber)
}

fn captured(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buf.lock().unwrap()).to_string()
}

// ─── fixtures ───────────────────────────────────────────────────

fn member_at(id: NodeId, name: &str, last_seen: u64, addr: SocketAddr) -> MemberRecord {
    MemberRecord {
        removed_at: None,
        node_pubkey: None,
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        dial_info_version: 0,
        dial_info_sig: None,
        node_id: id,
        name: name.into(),
        invited_by: id,
        joined_at: 0,
        last_seen,
        status: NodeStatus::Online,
        capabilities: NodeCapabilities {
            hardware: HardwareProfile {
                gpus: vec![],
                system_ram_gb: 0,
                cpu_cores: 0,
                total_storage_gb: 0,
                free_storage_gb: 0,
                network_bandwidth_mbps: None,
            },
            available: AvailableResources::default(),
            active_processes: vec![],
            hosted_corpora: vec![],
            reported_at: last_seen,
            inference_availability: 1.0,
            inference_capable: false,
            loaded_models: vec![],
            embed_model: None,
            benchmark: None,
            current_in_flight: None,
            anchor: None,
        },
        addresses: vec![addr],
    }
}

/// A peer that answers `/internal/app/state` with 413 — the exact
/// status an axum `DefaultBodyLimit` returns once the anti-entropy
/// snapshot outgrows `MAX_REQUEST_BODY_BYTES`. `/internal/gossip` is
/// deliberately absent: this test is about the store-push branch.
async fn spawn_rejecting_peer() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/internal/app/state",
        axum::routing::post(|| async { axum::http::StatusCode::PAYLOAD_TOO_LARGE }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

fn state_with_peer(self_id: NodeId, peer_id: NodeId, peer_addr: SocketAddr) -> AppState {
    let now = commonwealth_core::clock::unix_now_secs();
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(4242),
        name: "push-surfacing".into(),
        invite_key_hash: [7u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: {
            let mut m = HashMap::new();
            m.insert(
                self_id,
                member_at(self_id, "self", now, "127.0.0.1:1".parse().unwrap()),
            );
            m.insert(peer_id, member_at(peer_id, "rejector", now, peer_addr));
            m
        },
        peers: vec![],
    };
    AppState::new_with_platform_and_engine(
        self_id,
        mesh,
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        None,
    )
}

// ─── the test ───────────────────────────────────────────────────

/// RED-FIRST (order mesh-scale-t0, item 1). On the pre-fix code this
/// round emitted its rejection at `debug!`, the WARN-level capture
/// buffer came back EMPTY, and the assertion below failed with the
/// whole (empty) buffer printed.
#[tokio::test]
async fn a_rejected_mesh_store_push_reaches_an_operator_at_warn() {
    let peer_addr = spawn_rejecting_peer().await;
    let self_id = NodeId::from_u128(0xA11CE);
    let peer_id = NodeId::from_u128(0xB0B);
    let state = state_with_peer(self_id, peer_id, peer_addr);

    // Something to replicate — the push step is skipped on an empty
    // store, so without this the test would pass vacuously.
    state
        .inner
        .mesh_store
        .set(
            "corpus-engine",
            "handoff:test",
            bytes::Bytes::from_static(b"{\"handoff\":1}"),
            self_id,
        )
        .expect("seed mesh_store");

    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let guard = capture_warns(Arc::clone(&buf));
    gossip::run_one_round(&state, Duration::from_secs(60))
        .await
        .expect("round");
    drop(guard);

    let logs = captured(&buf);
    assert!(
        logs.contains("mesh_store push REJECTED"),
        "a peer refusing our anti-entropy snapshot must be visible at WARN — \
         a daemon with no RUST_LOG never emits debug, which is how replication \
         could stop dead with every surface green.\ncaptured at WARN:\n{logs}"
    );
    assert!(
        logs.contains("413"),
        "the status must ride along — 413 specifically means the snapshot \
         outgrew the receiver's body limit, which is a different operator \
         action than a 401.\ncaptured at WARN:\n{logs}"
    );

    // …and the SECOND round, with the peer still broken, must be
    // silent. Rate-limited per peer per status transition, not per
    // round: a 10s-cadence warn is 8,640 lines a day, which is
    // functionally the same silence it replaces.
    let buf2 = Arc::new(Mutex::new(Vec::<u8>::new()));
    let guard2 = capture_warns(Arc::clone(&buf2));
    gossip::run_one_round(&state, Duration::from_secs(60))
        .await
        .expect("round 2");
    drop(guard2);
    let logs2 = captured(&buf2);
    assert!(
        !logs2.contains("mesh_store push REJECTED"),
        "an unchanged failure must not re-warn every round\ncaptured:\n{logs2}"
    );
}

/// Glassbox (§9, and the workspace's custom-`target:` allowlist
/// gotcha): the payload gauge must actually RENDER at `tracing=debug`.
/// A gauge nobody can read is not instrumentation. This asserts the
/// event reaches a debug-level subscriber and carries the numbers an
/// operator needs — the measured size, the warn point, and the limit
/// it is walking toward.
#[tokio::test]
async fn the_payload_gauge_renders_at_debug() {
    let peer_addr = spawn_rejecting_peer().await;
    let self_id = NodeId::from_u128(0xC0FFEE);
    let peer_id = NodeId::from_u128(0xDECAF);
    let state = state_with_peer(self_id, peer_id, peer_addr);
    state
        .inner
        .mesh_store
        .set(
            "corpus-engine",
            "handoff:gauge",
            bytes::Bytes::from_static(b"{\"handoff\":2}"),
            self_id,
        )
        .expect("seed mesh_store");

    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let guard = capture_debug(Arc::clone(&buf));
    gossip::run_one_round(&state, Duration::from_secs(60))
        .await
        .expect("round");
    drop(guard);

    let logs = captured(&buf);
    assert!(
        logs.contains("mesh_store payload gauge"),
        "the gauge must render at tracing=debug\n{logs}"
    );
    for field in ["payload_bytes", "warn_at_bytes", "limit_bytes"] {
        assert!(
            logs.contains(field),
            "gauge is missing `{field}` — a size with no ceiling beside it is not a gauge\n{logs}"
        );
    }
}

// ─── the sender census ──────────────────────────────────────────

// `cw-twin-visibility`'s instrument is a one-line question — **how many
// senders of replicated state does this workspace have?** — and until now
// the only way to answer it was a hand-run `git grep` that nobody ran. The
// rest of this file proves one sender REPORTS; this half proves no OTHER
// sender exists.
//
// Deterministic, no model: every workspace member's `src/` tree, test
// modules excluded, counting the URL-join form `{…}/internal/<route>` that
// only a sender writes. A route STRING (the receiver's `.route(…)`) and a
// doc mention both lack the brace, so they do not count — which is the
// distinction the census exists to make.

/// Every production site that puts replicated state on the wire, by route
/// and by file.
///
/// **Three, and the fourth is what rung 2c deleted.** Before it,
/// `corpus_collaborate.rs` hand-rolled a fourth POST of the identical wire
/// shape to the identical route, under a comment asserting that no periodic
/// sender existed — an assertion that had been false since the sender
/// landed. Its targets were a strict subset of the round's (online AND
/// embed-compatible AND allowlisted, against the round's every online peer)
/// and its consumer polls at `auto_ingest::CHECK_INTERVAL` = 30 s, so the
/// ≤10 s it bought was inside the poll it fed.
///
/// A new row here is a review moment, never a silent pass. Adding a sender
/// is allowed; adding one without saying so in this table is not.
const REPLICATION_SENDERS: &[(&str, &str, usize)] = &[
    // The 10 s full-snapshot anti-entropy push, and the event-driven
    // single-entry push the work atlas needs for same-round-trip claims.
    // Both are the same route and the same body shape; `broadcast_now`'s
    // documented recovery IS the round above it, which is why the two
    // cannot be counted apart.
    (
        "/internal/app/state",
        "sovereign/crates/sovereign-mesh/src/gossip.rs",
        2,
    ),
    // The ring journal's own digest exchange — its own route on its own
    // 60 s cadence, budgeted in both directions.
    (
        "/internal/ring/sync",
        "sovereign/crates/sovereign-mesh/src/ring_sync.rs",
        1,
    ),
];

fn workspace_root() -> std::path::PathBuf {
    let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let toml = dir.join("Cargo.toml");
        if toml.is_file()
            && std::fs::read_to_string(&toml)
                .map(|t| t.contains("[workspace]"))
                .unwrap_or(false)
        {
            return dir;
        }
        assert!(dir.pop(), "workspace root not found");
    }
}

fn workspace_members(root: &std::path::Path) -> Vec<String> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).expect("root Cargo.toml");
    let start = text.find("members = [").expect("no members list");
    let end = text[start..].find(']').expect("unterminated members") + start;
    text[start + "members = [".len()..end]
        .lines()
        .map(|l| l.trim().trim_matches(',').trim_matches('"'))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

fn walk_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_rs(&p, out);
        } else if p.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// The census, as `(route, repo-relative file, sites)`.
///
/// Everything from the first `#[cfg(test)]` is dropped: a test that spins a
/// fake peer builds the same URL and is not a production sender. Both files
/// in the table put their test modules last, which is this workspace's
/// convention.
fn scan_senders() -> Vec<(String, String, usize)> {
    let root = workspace_root();
    let mut files = Vec::new();
    for member in workspace_members(&root) {
        walk_rs(&root.join(&member).join("src"), &mut files);
    }
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let production = match text.find("#[cfg(test)]") {
            Some(i) => &text[..i],
            None => &text[..],
        };
        for (route, _, _) in REPLICATION_SENDERS {
            let needle = format!("}}{route}");
            let n = production.matches(needle.as_str()).count();
            if n > 0 {
                let rel = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(((*route).to_string(), rel, n));
            }
        }
    }
    out.sort();
    out
}

/// RED-FIRST (cw-lift 2c, ARCH §18.1). At the commit before this one the
/// scan returned a THIRD row —
/// `commonwealth-api/src/routes_internal/corpus_collaborate.rs` on
/// `/internal/app/state` — and this assertion failed printing it.
#[test]
fn every_sender_of_replicated_state_is_declared() {
    let mut expected: Vec<(String, String, usize)> = REPLICATION_SENDERS
        .iter()
        .map(|(route, file, n)| ((*route).to_string(), (*file).to_string(), *n))
        .collect();
    expected.sort();
    assert_eq!(
        scan_senders(),
        expected,
        "the count of senders of replicated state is this campaign's instrument \
         (cw-twin-visibility). A row that appears here is a second answer to \
         \"how does state reach a peer\" (ARCH §10.6) and must be argued, not \
         discovered later by grep"
    );
}
