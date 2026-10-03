// SPDX-License-Identifier: AGPL-3.0-or-later
//! Placement measurements are serve's, and they travel on cw-rails' journal
//! with no daemon between (pb-serve-placement). Against ONE cw-rails spawned
//! from its binary, founded solo so its derived roster names its own key:
//! a record serve publishes is read back by serve's peer read, a republish is
//! idempotent by content, an invalid run never reaches the journal, and after
//! a seal retires the record serve's reconcile loop puts it back within one
//! poll. A binary rather than a dev-dependency: the boundary gate counts dev
//! edges, and serve's package takes no commonwealth crate.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use sovereign_serve::measurements_rail;
use sovereign_serve::mesh_measurements as mm;

/// `CW_RAILS_BIN`, else `cw-rails` in this build's target dir (the test binary
/// runs from `<target>/<profile>/deps/`). Absent is a FAILURE naming the
/// build, never a skip (five-programs-62).
fn cw_rails_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("CW_RAILS_BIN") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().expect("the test binary's own path");
    let beside = exe
        .parent()
        .and_then(Path::parent)
        .expect("a test binary under <target>/<profile>/deps")
        .join("cw-rails");
    assert!(
        beside.is_file(),
        "{} is missing: build it with `cargo build -p commonwealth-rails --bin cw-rails`, or set CW_RAILS_BIN",
        beside.display()
    );
    beside
}

/// A founded, solo cw-rails on its own data root. Killed and reaped on drop.
struct Rails {
    child: Child,
    base: String,
    dir: tempfile::TempDir,
}

impl Drop for Rails {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Rails {
    async fn spawn() -> Rails {
        let dir = tempfile::tempdir().expect("tempdir");
        let found = Command::new(cw_rails_bin())
            .args(["found", "placement-fixture", "--data-dir"])
            .arg(dir.path())
            .output()
            .expect("cw-rails found runs");
        assert!(
            found.status.success(),
            "cw-rails found: {}",
            String::from_utf8_lossy(&found.stderr)
        );
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("ephemeral port")
            .port();
        let log = std::fs::File::create(dir.path().join("cw-rails.log")).unwrap();
        let child = Command::new(cw_rails_bin())
            .args([
                "run",
                "--local-only",
                "--listen",
                &port.to_string(),
                "--data-dir",
            ])
            .arg(dir.path())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("cw-rails spawns");
        let rails = Rails {
            child,
            base: format!("http://127.0.0.1:{port}"),
            dir,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        while !reqwest::get(format!("{}/v1/rail/actor", rails.base))
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            assert!(
                Instant::now() < deadline,
                "cw-rails never answered; its log:\n{}",
                std::fs::read_to_string(rails.dir.path().join("cw-rails.log")).unwrap_or_default()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        rails
    }

    /// Every measurement on the journal, cw-rails' own included.
    async fn held(&self) -> Vec<mm::MeasurementRecord> {
        let log = sovereign_cli_base::rail::rail_log_at(&self.base, mm::MEASUREMENTS_APP_ID)
            .await
            .expect("the log door answers");
        let admission = sovereign_cli_base::rail::admission_from_wire(&log).expect("a log answer");
        measurements_rail::read(&admission, None)
            .found
            .into_iter()
            .map(|m| m.record)
            .collect()
    }
}

fn a_measurement(tok_s: f64, at: u64) -> mm::MeasurementRecord {
    let host = mm::HostIdentity::from_live_mesh(Some(0xf0f)).expect("a fingerprint is a host");
    mm::MeasurementRecord {
        key: mm::MeasurementKey::for_plan(
            host,
            "mf1:deadbeef".into(),
            "pd2:cafef00d".into(),
            32768,
            mm::LinkClass::Direct,
        ),
        decode_tok_s: tok_s,
        decode_tok_s_min: tok_s - 0.1,
        decode_tok_s_max: tok_s + 0.1,
        ttft_ms: 2203.0,
        itl_p50_ms: 90.0,
        itl_p95_ms: 98.0,
        prefill_tok_s: None,
        cold_load_s: None,
        trials: 3,
        content_frames: 256,
        model_name: "Qwen3.5-122B".into(),
        placement_human: "36 local + 12 @beefymac".into(),
        nodes: 2,
        hops: 1,
        measured_at: at,
        build: "0.10.0".into(),
        backend: Some("vulkan".into()),
        link_rtt_ms: None,
        verdict: mm::Verdict::Valid,
        witness: None,
        conditions: None,
    }
}

/// serve publishes, the journal holds the record, and serve's peer read does
/// not hand the node its own run back. An invalid run is refused before any
/// dial and never reaches the journal. A republish appends what is missing
/// once, and never again.
#[tokio::test(flavor = "multi_thread")]
async fn a_published_record_is_on_the_journal_and_a_republish_is_idempotent() {
    let rails = Rails::spawn().await;
    let record = a_measurement(17.35, 1_700_000_000);

    measurements_rail::publish(&rails.base, &record)
        .await
        .expect("cw-rails authors on its own founded mesh");
    let held = rails.held().await;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].decode_tok_s, 17.35);
    let peers = measurements_rail::peers(&rails.base)
        .await
        .expect("a peer read");
    assert!(
        peers.found.is_empty(),
        "the node's own run is its file's, never a peer's"
    );

    let mut invalid = a_measurement(1.0, 1_700_000_300);
    invalid.verdict = mm::Verdict::Invalid {
        problems: vec!["no content frames".into()],
    };
    let refused = measurements_rail::publish(&rails.base, &invalid)
        .await
        .expect_err("an invalid run is refused");
    assert_eq!(refused.to_string(), "an invalid run does not travel");

    let local = vec![record, a_measurement(11.08, 1_700_000_100), invalid];
    let first = measurements_rail::republish(&rails.base, &local)
        .await
        .expect("cw-rails answers");
    assert_eq!(first.appended, 1, "only the run the journal lacked");
    assert_eq!(first.already_held, 1);
    assert_eq!(first.withheld, 1, "the invalid run stays home");
    let second = measurements_rail::republish(&rails.base, &local)
        .await
        .expect("cw-rails answers");
    assert_eq!(second.appended, 0, "a second pass appends nothing");
    assert_eq!(second.already_held, 2);
    assert_eq!(rails.held().await.len(), 2, "and the journal did not grow");
}

/// **Without a re-append a seal is a delete.** The seal retires every line
/// below its floor; serve's reconcile loop sees the digest move and puts the
/// local file back within one poll.
#[tokio::test(flavor = "multi_thread")]
async fn after_a_seal_the_journal_holds_the_local_record_again_within_one_poll() {
    let rails = Rails::spawn().await;
    let store = rails.dir.path().join("mesh-measurements.json");
    // The loop reads the store through its one path accessor.
    std::env::set_var("SOVEREIGN_MESH_MEASUREMENTS", &store);
    let mut file = mm::MeasurementFile::new();
    mm::record(&mut file, a_measurement(17.35, 1_700_000_000));
    mm::save(&file).expect("the fixture file saves");
    assert_eq!(mm::load().records().len(), 1, "the fixture file loads");

    let poll = Duration::from_millis(200);
    let within = |what: &'static str| {
        let base = rails.base.clone();
        async move {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let log = sovereign_cli_base::rail::rail_log_at(&base, mm::MEASUREMENTS_APP_ID)
                    .await
                    .expect("the log door answers");
                let admission = sovereign_cli_base::rail::admission_from_wire(&log).unwrap();
                if measurements_rail::read(&admission, None).found.len() == 1 {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "{what}: the journal never held the record"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    let reconcile = tokio::spawn(measurements_rail::reconcile_loop(rails.base.clone(), poll));
    within("at start").await;

    let sealed = sovereign_cli_base::rail::rail_append_at(
        &rails.base,
        mm::MEASUREMENTS_APP_ID,
        &commonwealth_rail_core::RailAct::Seal,
    )
    .await
    .expect("cw-rails seals its own journal");
    assert!(
        sealed.get("retired").is_some(),
        "the seal retired the lines below its floor: {sealed}"
    );
    within("after the seal").await;
    reconcile.abort();
}
