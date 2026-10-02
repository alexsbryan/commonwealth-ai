// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-serve pod-snapshot record|drop` — serve records or drops the
//! pinned-pod snapshot its ranker sources pinned workers from
//! (`rank.rs`, at boot). Serve is the file's one writer; `svrn pod up` and
//! `svrn pod down` ask it here rather than writing serve's data themselves
//! (pb-mesh-dissolve, phase-b-51). Same path and schema as before, so there is
//! nothing to migrate.

use std::io::Read;

use sovereign_contracts::worker_pod::PodSnapshotRequest;
use sovereign_serving_host::pinned_pod_snapshot::{
    default_snapshot_dir, delete_snapshot, save_snapshot, PinnedPodSnapshot,
};
use sovereign_serving_host::pinned_worker_source::PodCapabilities;

const USAGE: &str =
    "usage: sovereign-serve pod-snapshot record   (a PodSnapshotRequest as JSON on stdin)\n       \
                     sovereign-serve pod-snapshot drop <vast-id>";

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("record") => record(),
        Some("drop") => match args.get(1) {
            Some(vast_id) => drop_snapshot(vast_id),
            None => {
                eprintln!("{USAGE}");
                2
            }
        },
        Some("--help") | Some("-h") => {
            println!("{USAGE}");
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

fn record() -> i32 {
    let mut body = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut body) {
        eprintln!("pod-snapshot record: could not read the request on stdin: {e}");
        return 1;
    }
    let req: PodSnapshotRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pod-snapshot record: the request on stdin is not a PodSnapshotRequest: {e}");
            return 2;
        }
    };
    let Some(dir) = default_snapshot_dir() else {
        eprintln!("pod-snapshot record: no snapshot directory");
        return 1;
    };
    let snapshot = PinnedPodSnapshot::new(
        req.vast_id,
        req.host,
        req.port,
        req.bootstrap_blob,
        PodCapabilities {
            system_ram_gb: req.system_ram_gb,
            benchmark: None,
            current_in_flight: None,
        },
    );
    match save_snapshot(&dir, &snapshot) {
        Ok(p) => {
            tracing::info!(target: "serve", vast_id = %snapshot.vast_id, path = %p.display(), "pinned-pod snapshot recorded");
            println!(
                "wrote snapshot at {} (inference routing enabled)",
                p.display()
            );
            0
        }
        Err(e) => {
            tracing::warn!(target: "serve", vast_id = %snapshot.vast_id, error = %e, "pinned-pod snapshot not recorded");
            eprintln!("pod-snapshot record: {e}");
            1
        }
    }
}

fn drop_snapshot(vast_id: &str) -> i32 {
    let Some(dir) = default_snapshot_dir() else {
        eprintln!("pod-snapshot drop: no snapshot directory");
        return 1;
    };
    match delete_snapshot(&dir, vast_id) {
        Ok(removed) => {
            tracing::info!(target: "serve", vast_id, removed, "pinned-pod snapshot drop");
            if removed {
                println!("removed pinned-pod snapshot for {vast_id}");
            }
            0
        }
        Err(e) => {
            tracing::warn!(target: "serve", vast_id, error = %e, "pinned-pod snapshot not dropped");
            eprintln!("pod-snapshot drop: {e}");
            1
        }
    }
}
