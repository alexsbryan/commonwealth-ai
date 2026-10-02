// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn quality check --distribute` against ONE cw-rails spawned from its
//! binary (pb-work-doors): the run seals and submits through cw-rails' own
//! doors, a fixture donor on the same cw-rails completes the unit through the
//! append door, and the table row carries the donor's verdict. Then a unit no
//! offer accepts renders never-ran with the refusal the refusals door named.
//!
//! cw-rails replicates no journal before pb-rails-parity, so submitter and
//! donor share one cw-rails (and so one signing key). Each run gets its own
//! `SVRNMESH_DATA_DIR`, `CW_RAILS_DIR` and HOME, never the developer's.
//! `CW_RAILS_BIN`, else `cw-rails` beside the binary under test — absent is a
//! FAILURE naming the build, never a skip (five-programs-62).

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kernel_types::{Judgement, Reason};
use oicp_types::work::{Completion, UnitRef, WorkAct, WorkProjection, WorkUnitStatus};
use oicp_types::{Isolation, WorkOffer};
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-cli");
const DONOR_REASON: &str = "the fixture donor ran it";

fn cw_rails_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("CW_RAILS_BIN") {
        return PathBuf::from(p);
    }
    let beside = Path::new(BIN).with_file_name("cw-rails");
    assert!(
        beside.is_file(),
        "{} is missing: build it with `cargo build -p commonwealth-rails`, or set CW_RAILS_BIN",
        beside.display()
    );
    beside
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The cw-rails child, killed on every exit path.
struct Rails(Child);

impl Drop for Rails {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One sandbox: a clean git checkout carrying a one-row registry, a config
/// naming the cw-rails base, and that cw-rails.
struct Sandbox {
    _root: tempfile::TempDir,
    home: PathBuf,
    svrnmesh: PathBuf,
    repo: PathBuf,
    base: String,
    _rails: Rails,
}

const REGISTRY: &str = r#"
censused_surfaces = [".github/workflows"]

[[instrument]]
id = "e2e-unit"
kind = "gate"
claim = "invariant"
command = "/usr/bin/true"
cost_secs = 1.0
enforcement = "hard"
fidelity = "F0"
baseline = { kind = "none" }
negative_control = "none"
runs_in = ["precommit"]
doc = "pb-work-doors"

[[trigger]]
id = "precommit"
budget_secs = 30
on_fail = "report"
"#;

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let p = |n: &str| root.path().join(n);
        let (home, svrnmesh, rails_dir, repo) = (p("home"), p("svrnmesh"), p("rails"), p("repo"));
        for d in [&home, &svrnmesh, &rails_dir, &repo.join("quality")] {
            std::fs::create_dir_all(d).expect("dir");
        }
        std::fs::write(repo.join("quality/ARCH_LAYERS.toml"), "").expect("checkout marker");
        let port = free_port();
        let base = format!("http://127.0.0.1:{port}");
        std::fs::write(
            svrnmesh.join("config.toml"),
            format!(
                "[node]\nentry_node = \"00000000000000000000000000000001\"\n\n\
                 [daemon]\nrails_base = \"{base}\"\n"
            ),
        )
        .expect("config");
        std::fs::write(repo.join("quality/instruments.toml"), REGISTRY).expect("registry");
        // The run persists its table under target/; a clean checkout stays clean.
        std::fs::write(repo.join(".gitignore"), "target/\n").expect("gitignore");
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git runs");
            assert!(
                ok.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&ok.stderr)
            );
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.email", "t@example.invalid"]);
        git(&["config", "user.name", "t"]);
        git(&["add", "."]);
        git(&["commit", "--quiet", "-m", "fixture"]);

        let log = std::fs::File::create(rails_dir.join("rails.log")).expect("log");
        let child = Command::new(cw_rails_bin())
            .args(["run", "--local-only", "--listen"])
            .arg(port.to_string())
            .env("HOME", &home)
            .env("CW_RAILS_DIR", &rails_dir)
            .env("RUST_LOG", "info,rails=debug,commonwealth_work=debug")
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawn cw-rails");
        let rails = Rails(child);
        let deadline = Instant::now() + Duration::from_secs(30);
        while get(&base, "/v1/mesh/status").is_none() {
            assert!(
                Instant::now() < deadline,
                "cw-rails never answered: {}",
                std::fs::read_to_string(rails_dir.join("rails.log")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
        Self {
            _root: root,
            home,
            svrnmesh,
            repo,
            base,
            _rails: rails,
        }
    }

    /// `svrn quality check --distribute`, as the dispatcher runs it.
    fn distribute(&self, extra: &[&str]) -> Output {
        Command::new(BIN)
            .args(["quality", "check", "--trigger", "precommit", "--distribute"])
            .args(extra)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("SVRNMESH_DATA_DIR", &self.svrnmesh)
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null())
            .output()
            .expect("run sovereign-cli quality check")
    }
}

/// One HTTP round trip on its own current-thread runtime: the test and the
/// donor thread are plain threads, and the dispatcher under test is a child.
fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(f)
}

fn get(base: &str, path: &str) -> Option<Value> {
    let url = format!("{base}{path}");
    block(async move {
        let resp = reqwest::get(url).await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json().await.ok()
    })
}

fn append(base: &str, act: &WorkAct) {
    let url = format!("{base}/v1/rail/append?namespace=work");
    let body = json!({ "op": "record", "payload": act });
    let (ok, text) = block(async move {
        let resp = reqwest::Client::new()
            .post(url)
            .json(&body)
            .send()
            .await
            .expect("append door");
        let ok = resp.status().is_success();
        (ok, resp.text().await.unwrap_or_default())
    });
    assert!(ok, "append refused: {text}");
}

fn offer(os: &str) -> WorkAct {
    WorkAct::Offer(WorkOffer {
        kinds: vec![oicp_types::JobKind::parse("process:v1").unwrap()],
        max_concurrent: 1,
        yield_to_foreground: false,
        isolation: Isolation::Subprocess,
        os: os.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        repos: Vec::new(),
        accept_from: None,
    })
}

/// The fixture donor: offers, and on the first queued unit it sees, leases it
/// and reports a pass attributed through cw-rails' own attribution door.
fn donor(base: String, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        append(&base, &offer(std::env::consts::OS));
        while !stop.load(Ordering::Relaxed) {
            let proj: WorkProjection = match get(&base, "/v1/work/projection") {
                Some(v) => serde_json::from_value(v).expect("projection"),
                None => continue,
            };
            for (handoff, h) in &proj.handoffs {
                for (hash, unit) in &h.units {
                    if !matches!(unit.status, WorkUnitStatus::Queued { .. }) {
                        continue;
                    }
                    let unit_ref = UnitRef {
                        handoff: *handoff,
                        unit_hash: hash.clone(),
                    };
                    append(&base, &WorkAct::Lease(unit_ref));
                    let rev = unit.unit.requirements.repo_rev.clone().expect("pinned rev");
                    let provenance = get(&base, &format!("/v1/work/attribution?repo_rev={rev}"))
                        .expect("attribution door");
                    append(
                        &base,
                        &WorkAct::Complete(Completion {
                            handoff: *handoff,
                            unit_hash: hash.clone(),
                            outcome: Judgement::passed(
                                format!("process:v1 {hash}"),
                                Reason::literal(DONOR_REASON),
                            ),
                            result: json!({
                                "exit_code": 0,
                                "duration_ms": 1000u64,
                                "stdout": "ok",
                                "stderr": "",
                            }),
                            provenance: serde_json::from_value(provenance).expect("attribution"),
                        }),
                    );
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        false
    })
}

fn text(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_distributed_run_submits_through_cw_rails_and_carries_the_donors_verdict() {
    let sandbox = Sandbox::new();

    // A donor completes the unit: the row is the donor's pass, on its node.
    let stop = Arc::new(AtomicBool::new(false));
    let worker = donor(sandbox.base.clone(), stop.clone());
    let out = sandbox.distribute(&[]);
    stop.store(true, Ordering::Relaxed);
    assert!(
        worker.join().unwrap(),
        "the donor never saw a queued unit:\n{}",
        text(&out)
    );
    let all = text(&out);
    assert!(out.status.success(), "exit {:?}:\n{all}", out.status.code());
    assert!(
        all.contains("submitted 1 unit(s) to the `work` ring"),
        "{all}"
    );
    assert!(all.contains("── passed  [hard] e2e-unit"), "{all}");
    assert!(
        all.contains(DONOR_REASON),
        "the row is the donor's verdict:\n{all}"
    );

    // Nobody can take it: the only offer is another OS. The row is never-ran
    // with the refusal cw-rails' survey named, never an absent row.
    append(&sandbox.base, &offer("plan9"));
    let out = sandbox.distribute(&["--budget-secs", "4"]);
    let all = text(&out);
    assert!(all.contains("e2e-unit"), "{all}");
    assert!(all.contains("never-ran"), "{all}");
    assert!(all.contains("requirement-unmet"), "{all}");
    assert!(all.contains("plan9"), "{all}");
}
