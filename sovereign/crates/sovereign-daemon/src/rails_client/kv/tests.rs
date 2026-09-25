// SPDX-License-Identifier: AGPL-3.0-or-later
//! A dead serving process is a named `Backend` absence within the timeout,
//! from a multi-thread worker and from a current-thread runtime alike.

use std::time::{Duration, Instant};

use sovereign_contracts::peer::{ReplicatedKv, ReplicatedKvError};

use super::RailsKv;

/// Port 1 on loopback: nothing listens, so every dial is refused.
const DEAD: &str = "http://127.0.0.1:1";

fn assert_absent<T: std::fmt::Debug>(r: Result<T, ReplicatedKvError>, path: &str) {
    let ReplicatedKvError::Backend(e) = r.expect_err("a dead dial has no answer");
    assert!(
        e.contains(&format!("{DEAD}{path}")),
        "the absence names the URL: {e}"
    );
}

fn every_verb_is_absent() {
    let kv = RailsKv::new(DEAD);
    let started = Instant::now();
    assert_absent(kv.get("a", "k"), "/v1/mesh/kv/entry");
    assert_absent(
        kv.set(
            "a",
            "k",
            bytes::Bytes::from_static(b"v"),
            kernel_types::NodeId::from_u128(1),
        ),
        "/v1/mesh/kv/entry",
    );
    assert_absent(kv.delete("a", "k"), "/v1/mesh/kv/entry");
    assert_absent(kv.scan("a", ""), "/v1/mesh/kv/entries");
    assert!(
        started.elapsed() < 4 * Duration::from_secs(2) + Duration::from_secs(1),
        "four dead dials answered inside their timeouts: {:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_door_is_a_named_absence_from_a_worker() {
    every_verb_is_absent();
}

#[tokio::test]
async fn a_dead_door_is_a_named_absence_from_a_current_thread_runtime() {
    every_verb_is_absent();
}

#[test]
fn a_dead_door_is_a_named_absence_off_any_runtime() {
    every_verb_is_absent();
}
