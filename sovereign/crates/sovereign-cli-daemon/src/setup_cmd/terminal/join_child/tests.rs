// SPDX-License-Identifier: AGPL-3.0-or-later
//! The wizard's join, end to end against a real entry daemon (fp-cond2-c).
//!
//! The fixture is five-programs-62's: a terminal-class `sovereign-daemon run
//! --config` founds a solo mesh in about a second and a half with no model
//! load, and its `join_link` joins only with `&relay=127.0.0.1:<internal>`
//! appended, since mDNS does not find it on this host. A cli-daemon test
//! cannot build another package's `[[bin]]`, so the binary is resolved the
//! way `daemon_bin::locate` does and the test FAILS, naming the build
//! command, when it is absent.
#![cfg(unix)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::super::find_holders;
use super::*;

/// `SOVEREIGN_DAEMON_BIN`, else `target/<profile>/sovereign-daemon` beside
/// this test's `deps/` directory.
fn daemon_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("SOVEREIGN_DAEMON_BIN") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().expect("current_exe");
    let profile = exe
        .parent()
        .and_then(Path::parent)
        .expect("test binary lives in target/<profile>/deps");
    let bin = profile.join("sovereign-daemon");
    assert!(
        bin.is_file(),
        "{} is missing: build it with `cargo build -p sovereign-daemon` \
         (or run TEST(sovereign-daemon) first), or set SOVEREIGN_DAEMON_BIN",
        bin.display()
    );
    bin
}

/// Kills and reaps the founder on every exit path, panics included.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A client port whose `+1` internal port is also free.
fn free_pair() -> u16 {
    loop {
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("ephemeral port")
            .port();
        if port < u16::MAX && TcpListener::bind(("127.0.0.1", port + 1)).is_ok() {
            return port;
        }
    }
}

fn node_env(dir: &Path, bin: &Path) -> Command {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let mut cmd = Command::new(bin);
    cmd.env("HOME", &home)
        .env("SVRNMESH_DATA_DIR", dir.join("svrnmesh"));
    cmd
}

fn mesh_dirs(data: &Path) -> Vec<String> {
    std::fs::read_dir(data.join("meshes"))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wizard_joins_through_a_spawned_daemon_and_stops_it() {
    let bin = daemon_bin();
    let root = tempfile::tempdir().expect("tempdir");
    let (fdir, jdir) = (root.path().join("founder"), root.path().join("joiner"));

    // The entry: a terminal-class founder on ephemeral ports.
    let f_client = free_pair();
    let fdata = fdir.join("data");
    std::fs::create_dir_all(&fdata).expect("founder data");
    let fcfg = fdir.join("config.toml");
    std::fs::write(
        &fcfg,
        format!(
            "[node]\nentry_node = \"00000000000000000000000000000001\"\n\n\
             [daemon]\nclient_port = {f_client}\ninternal_port = {}\n\n\
             [data]\ndir = \"{}\"\n",
            f_client + 1,
            fdata.display()
        ),
    )
    .expect("founder config");
    let _founder = Killed(
        node_env(&fdir, &bin)
            .args(["run", "--config"])
            .arg(&fcfg)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn founder"),
    );
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("http client");
    let deadline = Instant::now() + Duration::from_secs(60);
    let join_link = loop {
        if let Ok(resp) = http
            .get(format!("http://127.0.0.1:{f_client}/v1/mesh/status"))
            .send()
            .await
        {
            if let Ok(v) = resp.json::<serde_json::Value>().await {
                if let Some(link) = v["join_link"].as_str() {
                    break link.to_string();
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "founder never published a join_link"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let link = format!("{join_link}&relay=127.0.0.1:{}", f_client + 1);

    // The wizard's join, over a fresh data dir with no daemon listening.
    let jdata = jdir.join("data");
    let j_client = free_pair();
    let (child, mesh_name) = match join(node_env(&jdir, &bin), &jdata, j_client, &link, "j").await {
        Ok(joined) => joined,
        Err(JoinFailure::Refused) => panic!("the join child exited without joining"),
        Err(JoinFailure::Launch(e)) => panic!("the join child did not start: {e}"),
    };
    assert!(!mesh_name.is_empty(), "joined line carried no mesh name");
    let pid = child.id();

    // The identity landed in the run data dir, because the child owns it.
    let founder_meshes = mesh_dirs(&fdata);
    assert!(!founder_meshes.is_empty(), "founder persisted no mesh");
    let joined_meshes = mesh_dirs(&jdata);
    assert!(
        founder_meshes.iter().all(|m| joined_meshes.contains(m)),
        "joiner meshes {joined_meshes:?} lack the founder's {founder_meshes:?}"
    );
    // `node_id` is raw bytes, not text.
    let node_id = std::fs::read(jdata.join("node_id")).expect("joiner node_id");
    assert!(!node_id.is_empty(), "empty node_id");
    assert_ne!(
        node_id,
        std::fs::read(fdata.join("node_id")).expect("founder node_id"),
        "the joiner minted its own node id"
    );

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
