// SPDX-License-Identifier: AGPL-3.0-or-later
//! The wizard's join, end to end against a real entry mesh (fp-cond2-c).
//!
//! Since pb-mesh-exit-transport the mesh and its key are cw-rails': the
//! entry is a `cw-rails found` + `run`, and the joiner's cw-rails listens on
//! the base the provisional config names, so the test never touches the
//! live cw-rails the wizard's `svrn mesh up` brings up on the default base
//! (phase-b-81 (1)). Both cw-rails run `--local-only`, so the invite dials
//! direct addresses on this host. A cli-daemon test cannot build another
//! package's `[[bin]]`, so the stock binary and cw-rails are resolved beside
//! this test and the test FAILS, naming the build command, when either is
//! absent.
#![cfg(unix)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::super::find_holders;
use super::*;

/// `env`, else `target/<profile>/<name>` beside this test's `deps/` directory.
fn sibling(name: &str, env: &str, build: &str) -> PathBuf {
    if let Some(p) = std::env::var_os(env) {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().expect("current_exe");
    let profile = exe
        .parent()
        .and_then(Path::parent)
        .expect("test binary lives in target/<profile>/deps");
    let bin = profile.join(name);
    assert!(
        bin.is_file(),
        "{} is missing: build it with `cargo build -p {build}`, or set {env}",
        bin.display()
    );
    bin
}

/// Kills and reaps its child on every exit path, panics included.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("ephemeral port")
        .port()
}

/// A client port whose `+1` internal port is also free.
fn free_pair() -> u16 {
    loop {
        let port = free_port();
        if port < u16::MAX && TcpListener::bind(("127.0.0.1", port + 1)).is_ok() {
            return port;
        }
    }
}

/// A process with its home, svrn data dir and cw-rails dir under `dir`.
fn node_env(dir: &Path, bin: &Path) -> Command {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let mut cmd = Command::new(bin);
    cmd.env("HOME", &home)
        .env("SVRNMESH_DATA_DIR", dir.join("svrnmesh"))
        .env("CW_RAILS_DIR", dir.join("rails"));
    cmd
}

/// `cw-rails run --listen <port> --local-only` over `dir`'s cw-rails dir.
fn cw_rails_run(rails: &Path, dir: &Path, port: u16) -> Killed {
    Killed(
        node_env(dir, rails)
            .args(["run", "--listen", &port.to_string(), "--local-only"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cw-rails"),
    )
}

/// Poll cw-rails' status on `port` until `pick` finds what it wants.
async fn poll_status<T>(
    http: &reqwest::Client,
    port: u16,
    what: &str,
    pick: impl Fn(&serde_json::Value) -> Option<T>,
) -> T {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(resp) = http
            .get(format!("http://127.0.0.1:{port}/v1/mesh/status"))
            .send()
            .await
        {
            if let Ok(v) = resp.json::<serde_json::Value>().await {
                if let Some(t) = pick(&v) {
                    return t;
                }
            }
        }
        assert!(Instant::now() < deadline, "never saw {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wizard_joins_through_a_spawned_daemon_and_stops_it() {
    let stock = sibling("sovereign-stock", "SOVEREIGN_DAEMON_BIN", "sovereign-stock");
    let rails = sibling("cw-rails", "CW_RAILS_BIN", "commonwealth-rails");
    let root = tempfile::tempdir().expect("tempdir");
    let (fdir, jdir) = (root.path().join("founder"), root.path().join("joiner"));
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("http client");

    // The entry: a cw-rails mesh, and its invite.
    let founded = node_env(&fdir, &rails)
        .args(["found", "Lab", "--name", "founder"])
        .stdout(Stdio::null())
        .status()
        .expect("cw-rails found");
    assert!(founded.success(), "cw-rails found failed");
    let f_rails = free_port();
    let _founder = cw_rails_run(&rails, &fdir, f_rails);
    let link = poll_status(&http, f_rails, "the founder's join_link", |v| {
        v["join_link"].as_str().map(str::to_string)
    })
    .await;

    // The joiner's cw-rails, solo until the child asks it to join, under the
    // member name the child joins as (cw-rails has one name per node).
    let jrails = jdir.join("rails");
    std::fs::create_dir_all(jrails.join("rails")).expect("joiner rails dir");
    std::fs::write(jrails.join("rails").join("rails.toml"), "name = \"j\"\n")
        .expect("joiner rails.toml");
    let j_rails = free_port();
    let _joiner_rails = cw_rails_run(&rails, &jrails, j_rails);
    poll_status(&http, j_rails, "the joiner's cw-rails answering", |_| {
        Some(())
    })
    .await;

    // The wizard's join, over a fresh data dir with no daemon listening.
    let jdata = jdir.join("data");
    let j_client = free_pair();
    let rails_base = format!("http://127.0.0.1:{j_rails}");
    let mut cmd = node_env(&jdir, &stock);
    cmd.env("CW_RAILS_DIR", jrails.join("rails"));
    let (child, mesh_name) = match join(cmd, &jdata, j_client, Some(&rails_base), &link, "j").await
    {
        Ok(joined) => joined,
        Err(JoinFailure::Refused) => panic!("the join child exited without joining"),
        Err(JoinFailure::Launch(e)) => panic!("the join child did not start: {e}"),
    };
    assert_eq!(mesh_name, "Lab", "the joined line names the founder's mesh");
    let pid = child.id();

    // The joiner's cw-rails now holds the founder's mesh beside its own row.
    let founder_id = poll_status(&http, f_rails, "the founder's self row", |v| {
        v["members"]
            .as_array()?
            .iter()
            .find(|r| r["is_self"] == true)
            .map(|r| r["node_id"].clone())
    })
    .await;
    poll_status(&http, j_rails, "the founder on the joiner's roster", |v| {
        v["members"]
            .as_array()?
            .iter()
            .any(|r| r["node_id"] == founder_id && r["is_self"] != true)
            .then_some(())
    })
    .await;

    // `find_holders` reads the child's venues: the founder IS a member (so
    // the "has members" refusal, not "no peers appeared"), and holds no model.
    let probe = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("probe client");
    match find_holders(&child, &probe).await {
        Err(msg) => assert!(
            msg.contains("the mesh has members"),
            "find_holders never saw the founder through /v1/mesh/venues: {msg}"
        ),
        Ok(h) => panic!("a model-less founder was reported as {} holder(s)", h.len()),
    }

    // Dropping the child stops it and removes the provisional config.
    drop(child);
    // SAFETY: signal 0 sends nothing; it only asks whether `pid` exists.
    let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
    assert!(!alive, "join child {pid} outlived its guard");
    assert!(
        !jdata.join("setup-join.toml").exists(),
        "provisional config left behind"
    );
}
