// SPDX-License-Identifier: AGPL-3.0-or-later
//! A real cw-rails, spawned from `CW_RAILS_BIN`, for the `work` plane's seal
//! and fold (pb-work-donor).
//!
//! The daemon links no commonwealth-work, so a test that needs a sealed unit
//! or a folded queue asks the process that owns both, through its doors:
//! `POST /v1/work/seal`, `POST /v1/rail/ingest?namespace=work` for acts a
//! fixture signed with its own keys at its own instants, and
//! `GET /v1/work/projection`. A binary rather than a dev-dependency: the
//! boundary gate counts dev edges (xtask boundary_gate.rs:710-713).
//!
//! The fixture's roster is written as the `work` ring's `roster.json` before
//! cw-rails starts, so the fold admits the fixture's signers — the file
//! outranks the membership default (`RingRail::roster`).
//!
//! It is also the daemon tests' real ring rail (pb-mesh-exit-mesh): the
//! daemon's one rail implementation dials cw-rails, so a test that needs a
//! journal runs this process with the node key it signs as
//! ([`WorkRails::spawn_keyed`]) and hands the daemon [`WorkRails::ring_rail`].

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use std::sync::Arc;

use commonwealth_rail_core::{
    Admission, Compaction, Digest, GuestAttestation, Op, Payload, RailAct, RailError, Roster,
    RosterOrigin, SignedOp, SigningKey,
};
use oicp_types::work::projection::WorkProjection;
use oicp_types::work::WorkAct;
use oicp_types::{JobKind, JobRequirements, JobUnit};
use sovereign_daemon::rail_port::{RailFut, RingRailPort};
use sovereign_daemon::rails_client::RailsRingRail;
use tempfile::TempDir;

/// The ring namespace every work act rides on — the literal cw-rails' doors
/// answer under (`commonwealth_work::WORK_NAMESPACE`).
pub const WORK_NAMESPACE: &str = "work";

/// `CW_RAILS_BIN`, else `cw-rails` in this build's target dir (the test binary
/// runs from `<target>/<profile>/deps/`). Absent is a FAILURE naming the
/// build, never a skip (five-programs-62).
pub fn cw_rails_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("CW_RAILS_BIN") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().expect("the test binary's own path");
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("a test binary under <target>/<profile>/deps");
    let beside = profile_dir.join("cw-rails");
    assert!(
        beside.is_file(),
        "{} is missing: build it with `cargo build -p commonwealth-rails --bin cw-rails`, or set CW_RAILS_BIN",
        beside.display()
    );
    beside
}

/// One act as the journal line its signer signs: rail-core's one
/// canonicalizer over the act's own serde — the encoding the fold reads back,
/// and whose `Submit` units it re-verifies.
pub fn payload_of(act: &WorkAct) -> Payload {
    Payload::new(serde_json::to_value(act).expect("a work act encodes"))
        .expect("a work act is a journal payload")
}

/// A running cw-rails on its own data root. Killed and reaped on drop.
pub struct WorkRails {
    child: Child,
    pub base: String,
    root: PathBuf,
    /// The temp root, when the fixture made one rather than being handed one.
    _owned: Option<TempDir>,
}

impl Drop for WorkRails {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Write `roster` as `namespace`'s `roster.json` under a cw-rails data root.
pub fn write_roster(root: &Path, namespace: &str, roster: &Roster) {
    let ring = root.join("rings").join(namespace);
    std::fs::create_dir_all(&ring).unwrap();
    std::fs::write(
        ring.join("roster.json"),
        serde_json::to_vec(roster).expect("a roster encodes"),
    )
    .unwrap();
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("ephemeral port")
        .port()
}

impl WorkRails {
    /// Start cw-rails solo, `--local-only`, with `roster` (when given) as the
    /// `work` ring's `roster.json` and `rails_toml_extra` appended to its
    /// `rails.toml`; ready once `/v1/mesh/status` answers.
    pub async fn spawn(roster: Option<&Roster>, rails_toml_extra: &str) -> WorkRails {
        let rosters: Vec<(&str, &Roster)> =
            roster.map(|r| (WORK_NAMESPACE, r)).into_iter().collect();
        Self::spawn_keyed(None, None, &rosters, rails_toml_extra).await
    }

    /// [`WorkRails::spawn`] on `data_dir` (a fresh temp root when `None`; a
    /// root handed in outlives this process, so a second spawn on it reads
    /// the same journals), signing as `node_key` (cw-rails' `node_key`, 32
    /// raw seed bytes, written before it starts; a fresh key when `None`),
    /// with each `(namespace, roster)` written as that ring's `roster.json`.
    pub async fn spawn_keyed(
        data_dir: Option<&Path>,
        node_key: Option<&SigningKey>,
        rosters: &[(&str, &Roster)],
        rails_toml_extra: &str,
    ) -> WorkRails {
        let owned = data_dir
            .is_none()
            .then(|| tempfile::tempdir().expect("tempdir"));
        let root: PathBuf = match (data_dir, &owned) {
            (Some(d), _) => d.to_path_buf(),
            (None, owned) => owned.as_ref().expect("made above").path().to_path_buf(),
        };
        let dir = root.as_path();
        if let Some(key) = node_key {
            std::fs::write(dir.join("node_key"), key.to_bytes()).unwrap();
        }
        for (ns, roster) in rosters {
            write_roster(dir, ns, roster);
        }
        let port = free_port();
        std::fs::write(
            dir.join("rails.toml"),
            format!("name = \"work-fixture\"\nlisten = {port}\n{rails_toml_extra}"),
        )
        .unwrap();
        let log = std::fs::File::create(dir.join("cw-rails.log")).unwrap();
        let child = Command::new(cw_rails_bin())
            .args(["run", "--local-only", "--data-dir"])
            .arg(dir)
            .env("RUST_LOG", "info,commonwealth_work=debug")
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("cw-rails spawns");
        let rails = WorkRails {
            child,
            base: format!("http://127.0.0.1:{port}"),
            root,
            _owned: owned,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if reqwest::get(format!("{}/v1/mesh/status", rails.base))
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return rails;
            }
            assert!(
                Instant::now() < deadline,
                "cw-rails never answered /v1/mesh/status; its log:\n{}",
                rails.log()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Write `roster` as `namespace`'s `roster.json`; cw-rails reads the file
    /// at every roster read, so a running process sees it on the next one.
    pub fn set_roster(&self, namespace: &str, roster: &Roster) {
        write_roster(&self.root, namespace, roster);
    }

    /// The daemon's dialing rail over this process, holding it alive for as
    /// long as the daemon holds the port.
    pub fn ring_rail(self: &Arc<Self>) -> Arc<dyn RingRailPort> {
        Arc::new(HeldRail {
            rail: RailsRingRail::new(self.base.clone()),
            _rails: Arc::clone(self),
        })
    }

    /// The last lines cw-rails wrote, for a failure message.
    pub fn log(&self) -> String {
        let text = std::fs::read_to_string(self.root.join("cw-rails.log")).unwrap_or_default();
        let tail: Vec<&str> = text.lines().rev().take(40).collect();
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    }

    /// Seal `payloads` as units of `kind` through `POST /v1/work/seal`.
    pub async fn seal(&self, kind: &JobKind, payloads: Vec<serde_json::Value>) -> Vec<JobUnit> {
        let units: Vec<serde_json::Value> = payloads
            .into_iter()
            .map(|p| serde_json::json!({ "payload": p, "requirements": JobRequirements::any() }))
            .collect();
        let answer: serde_json::Value = reqwest::Client::new()
            .post(format!("{}/v1/work/seal", self.base))
            .json(&serde_json::json!({ "kind": kind, "units": units }))
            .send()
            .await
            .expect("the seal door answers")
            .error_for_status()
            .expect("the seal door sealed")
            .json()
            .await
            .expect("a seal answer");
        serde_json::from_value(answer["units"].clone()).expect("sealed units")
    }

    /// Admit `ops`, signed as they are, into the `work` journal.
    pub async fn ingest(&self, ops: &[Op<SignedOp>]) {
        reqwest::Client::new()
            .post(format!(
                "{}/v1/rail/ingest?namespace={WORK_NAMESPACE}",
                self.base
            ))
            .json(&serde_json::json!({ "ops": ops }))
            .send()
            .await
            .expect("the ingest door answers")
            .error_for_status()
            .expect("the ingest door took the ops");
    }

    /// The `work` queue, folded by cw-rails.
    pub async fn projection(&self) -> WorkProjection {
        reqwest::get(format!("{}/v1/work/projection", self.base))
            .await
            .expect("the projection door answers")
            .error_for_status()
            .expect("the projection door folded")
            .json()
            .await
            .expect("a WorkProjection")
    }
}

/// [`RailsRingRail`] over a [`WorkRails`] it keeps running: every method is
/// the dialing rail's own.
struct HeldRail {
    rail: RailsRingRail,
    _rails: Arc<WorkRails>,
}

impl RingRailPort for HeldRail {
    fn namespaces(&self) -> RailFut<'_, Vec<String>> {
        self.rail.namespaces()
    }
    fn actor(&self) -> RailFut<'_, String> {
        self.rail.actor()
    }
    fn roster_origin(&self, namespace: &str) -> RailFut<'_, RosterOrigin> {
        self.rail.roster_origin(namespace)
    }
    fn roster(&self, namespace: &str) -> RailFut<'_, Roster> {
        self.rail.roster(namespace)
    }
    fn journal_read(&self, namespace: &str) -> RailFut<'_, Vec<Op<SignedOp>>> {
        self.rail.journal_read(namespace)
    }
    fn journal_admit(&self, namespace: &str, roster: &Roster) -> RailFut<'_, Admission> {
        self.rail.journal_admit(namespace, roster)
    }
    fn journal_append(
        &self,
        namespace: &str,
        act: RailAct,
        roster: &Roster,
    ) -> RailFut<'_, Op<SignedOp>> {
        self.rail.journal_append(namespace, act, roster)
    }
    fn journal_append_attested(
        &self,
        namespace: &str,
        act: RailAct,
        roster: &Roster,
        attestation: &GuestAttestation,
    ) -> RailFut<'_, Op<SignedOp>> {
        self.rail
            .journal_append_attested(namespace, act, roster, attestation)
    }
    fn journal_seal(
        &self,
        namespace: &str,
        roster: &Roster,
    ) -> RailFut<'_, (Op<SignedOp>, Result<Compaction, RailError>)> {
        self.rail.journal_seal(namespace, roster)
    }
    fn journal_compact(&self, namespace: &str, roster: &Roster) -> RailFut<'_, Compaction> {
        self.rail.journal_compact(namespace, roster)
    }
    fn journal_digest(&self, namespace: &str) -> RailFut<'_, Digest> {
        self.rail.journal_digest(namespace)
    }
    fn journal_ingest_all(&self, namespace: &str, ops: &[Op<SignedOp>]) -> RailFut<'_, usize> {
        self.rail.journal_ingest_all(namespace, ops)
    }
    fn journal_ops_missing_from_within(
        &self,
        namespace: &str,
        theirs: &Digest,
        budget_bytes: usize,
    ) -> RailFut<'_, (Vec<Op<SignedOp>>, bool)> {
        self.rail
            .journal_ops_missing_from_within(namespace, theirs, budget_bytes)
    }
    fn work_projection(&self) -> RailFut<'_, WorkProjection> {
        self.rail.work_projection()
    }
}
