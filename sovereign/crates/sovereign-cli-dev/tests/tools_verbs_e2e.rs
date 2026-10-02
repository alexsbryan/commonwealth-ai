// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn tools list|describe|call` run as the BUILT binary (pb-atlas-kv).
//!
//! fp-33 built a `reqwest::blocking` client inside the async registry path,
//! so every verb panicked ("Cannot drop a runtime in a context where blocking
//! is not allowed") and no test ran the verb to see it. The work atlas now
//! dials cw-rails' KV doors through the one sync client
//! (`sovereign_turn_client::rails_kv::RailsKv`); these tests run the binary
//! on a temp data root whose `[daemon] rails_base` names a stand-in door on a
//! temp port, with the daemon URL pointed at a dead port so nothing here
//! reaches a deployed daemon or the operator's cw-rails.

#![cfg(feature = "workbench")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sovereign_contracts::setup_config::SetupConfig;

/// Nothing listens on loopback port 1 or 2: a dial is refused at once.
const DEAD_RAILS: &str = "http://127.0.0.1:1";
const DEAD_DAEMON: &str = "http://127.0.0.1:2";

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    /// A data root whose config names `rails_base`, and an empty repo.
    fn new(rails_base: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir_all(dir.path().join("repo/.sovereign")).expect("repo dir");
        // A config that does not load falls back to the DEFAULT rails base,
        // which on a dev host is the operator's live cw-rails — so the
        // sandbox proves its config loads before any verb runs. (An
        // unconfigured file is refused: it names no `[node]` binding.)
        let mut config = SetupConfig::unconfigured();
        config.node.entry = Some(DEAD_DAEMON.to_string());
        config.daemon.rails_base = Some(rails_base.to_string());
        let path = SetupConfig::path_in(&root);
        config.save_to(&path).expect("write the sandbox config");
        let loaded = SetupConfig::load_from(&path).expect("the sandbox config loads");
        assert_eq!(loaded.daemon.rails_base.as_deref(), Some(rails_base));
        Self { dir }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    fn run(&self, args: &[&str]) -> Output {
        let home: &Path = self.dir.path();
        Command::new(env!("CARGO_BIN_EXE_sovereign-cli-dev"))
            .args(args)
            .current_dir(self.path("repo"))
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("SVRNMESH_DATA_DIR", self.path("root"))
            .env_remove("SOVEREIGN_DATA_DIR")
            .env("SVRNMESH_DAEMON_URL", DEAD_DAEMON)
            .env_remove("SOVEREIGN_DAEMON_URL")
            .output()
            .expect("spawn sovereign-cli-dev")
    }
}

fn text(out: &Output) -> String {
    format!(
        "status: {:?}\n--- stdout\n{}\n--- stderr\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A stand-in for cw-rails' scan door: an empty store that counts its dials.
fn stand_in_door() -> (String, Arc<AtomicUsize>) {
    use axum::routing::get;
    use axum::{Json, Router};
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&hits);
    let app = Router::new().route(
        "/v1/mesh/kv/entries",
        get(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            async { Json(Vec::<serde_json::Value>::new()) }
        }),
    );
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("door runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind the door");
            tx.send(listener.local_addr().expect("door addr"))
                .expect("report the door");
            axum::serve(listener, app).await.expect("serve the door");
        });
    });
    let addr = rx.recv().expect("the door is up");
    (format!("http://{addr}"), hits)
}

#[test]
fn list_and_describe_answer() {
    let (door, _) = stand_in_door();
    let sb = Sandbox::new(&door);

    let list = sb.run(&["tools", "list"]);
    assert!(list.status.success(), "tools list: {}", text(&list));
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("work_in_flight"),
        "the work-atlas tools are listed: {}",
        text(&list)
    );

    let describe = sb.run(&["tools", "describe", "session_state"]);
    assert!(
        describe.status.success(),
        "tools describe: {}",
        text(&describe)
    );
}

#[test]
fn a_work_atlas_call_dials_the_rails_door() {
    let (door, hits) = stand_in_door();
    let sb = Sandbox::new(&door);
    let call = sb.run(&["tools", "call", "work_in_flight", "--scope=src"]);
    assert!(call.status.success(), "tools call: {}", text(&call));
    assert!(
        hits.load(Ordering::SeqCst) > 0,
        "the call read cw-rails' scan door: {}",
        text(&call)
    );
}

#[test]
fn a_work_atlas_call_with_no_door_names_the_absence() {
    let sb = Sandbox::new(DEAD_RAILS);
    let call = sb.run(&["tools", "call", "work_in_flight", "--scope=src"]);
    assert!(
        !call.status.success(),
        "no door, no answer: {}",
        text(&call)
    );
    let stderr = String::from_utf8_lossy(&call.stderr);
    assert!(
        stderr.contains(&format!(
            "cannot reach the mesh's rails daemon at {DEAD_RAILS}/v1/mesh/kv/entries"
        )),
        "the absence names the door it dialed: {}",
        text(&call)
    );
}
