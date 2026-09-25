// SPDX-License-Identifier: AGPL-3.0-or-later
//! The setup wizard's join child answers `GET /v1/mesh/venues`, and only that
//! (five-programs-62).
//!
//! `Launch::AdminJoin` assembles the mesh-admin services, which mount no host
//! surface, so before fp-cond2-b2 the joined child answered 404 on the one
//! read the wizard polls for holders. The fixture is the package's measured
//! one: a terminal-class founder (`[node] entry_node`, no models) founds a
//! solo mesh in about a second and a half, and its `join_link` joins only with
//! `&relay=127.0.0.1:<founder internal_port>` appended, because mDNS does not
//! find it on this host.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sovereign_contracts::launch::JOINED_LINE_PREFIX;

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-daemon");

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

/// One loopback GET: `(status, body)`, or `None` while nothing listens.
fn get(port: u16, path: &str) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    let status = raw.split_whitespace().nth(1)?.parse().ok()?;
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    Some((status, body.to_string()))
}

fn write_config(dir: &Path, client: u16, internal: u16, entry: bool) -> PathBuf {
    let data = dir.join("data");
    std::fs::create_dir_all(&data).expect("data dir");
    let node = if entry {
        "[node]\nentry_node = \"00000000000000000000000000000001\"\n\n"
    } else {
        ""
    };
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        format!(
            "{node}[daemon]\nclient_port = {client}\ninternal_port = {internal}\n\
             rails_base = \"http://127.0.0.1:{}\"\n\n[data]\ndir = \"{}\"\n",
            free_port(),
            data.display()
        ),
    )
    .expect("config");
    path
}

fn daemon(dir: &Path) -> Command {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let mut cmd = Command::new(BIN);
    cmd.env("HOME", &home)
        .env("SVRNMESH_DATA_DIR", dir.join("svrnmesh"))
        .env("CW_RAILS_DIR", dir.join("rails"))
        .stderr(Stdio::from(
            std::fs::File::create(dir.join("stderr.log")).expect("stderr log"),
        ));
    cmd
}

#[test]
fn the_join_child_serves_venues_and_nothing_else() {
    let root = std::env::temp_dir().join(format!("admin-join-venues-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (fdir, jdir) = (root.join("founder"), root.join("joiner"));
    let (f_client, f_internal) = (free_port(), free_port());
    let (j_client, j_internal) = (free_port(), free_port());

    let fcfg = write_config(&fdir, f_client, f_internal, true);
    let _founder = Killed(
        daemon(&fdir)
            .args(["run", "--config"])
            .arg(&fcfg)
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn founder"),
    );

    // The founder's solo mesh and its invite.
    let deadline = Instant::now() + Duration::from_secs(60);
    let founder_status = loop {
        if let Some((200, body)) = get(f_client, "/v1/mesh/status") {
            let v: serde_json::Value = serde_json::from_str(&body).expect("status json");
            if v["join_link"].is_string() {
                break v;
            }
        }
        assert!(
            Instant::now() < deadline,
            "founder never published a join_link; see {}",
            fdir.join("stderr.log").display()
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let link = format!(
        "{}&relay=127.0.0.1:{f_internal}",
        founder_status["join_link"].as_str().unwrap()
    );

    let jcfg = write_config(&jdir, j_client, j_internal, false);
    let mut joiner = Killed(
        daemon(&jdir)
            .args(["join", "--config"])
            .arg(&jcfg)
            .args(["--node-name", "j"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn joiner"),
    );
    {
        let mut stdin = joiner.0.stdin.take().expect("joiner stdin");
        writeln!(stdin, "{link}").expect("write invite");
    }
    let (tx, rx) = mpsc::channel();
    let stdout = joiner.0.stdout.take().expect("joiner stdout");
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) if line.starts_with(JOINED_LINE_PREFIX) => break,
            Ok(_) => continue,
            Err(_) => panic!(
                "joiner never printed `{JOINED_LINE_PREFIX}…`; see {}",
                jdir.join("stderr.log").display()
            ),
        }
    }

    // The one route, answering.
    let (status, body) = get(j_client, "/v1/mesh/venues").expect("joiner client port listens");
    assert_eq!(status, 200, "joiner /v1/mesh/venues: {body}");
    let venues: serde_json::Value = serde_json::from_str(&body).expect("venues json");
    assert!(venues["venues"].is_array(), "no venues array: {body}");

    // And no other mesh route: the child is not a host surface.
    let (status, _) = get(j_client, "/v1/mesh/status").expect("joiner client port listens");
    assert_eq!(status, 404, "the join child must not serve /v1/mesh/status");

    // Logged, not asserted: whether the founder shows up as a venue. The
    // wizard's `find_holders` depends on the answer (fp-cond2-c).
    let founder_self = founder_status["members"]
        .as_array()
        .and_then(|m| m.iter().find(|r| r["is_self"] == true))
        .cloned()
        .unwrap_or_default();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = venues;
    let listed = loop {
        let listed = last["venues"].as_array().is_some_and(|vs| {
            vs.iter().any(|v| {
                v["node_id"] == founder_self["node_id"] || v["name"] == founder_self["name"]
            })
        });
        if listed || Instant::now() >= deadline {
            break listed;
        }
        std::thread::sleep(Duration::from_millis(500));
        if let Some((200, b)) = get(j_client, "/v1/mesh/venues") {
            last = serde_json::from_str(&b).unwrap_or(last);
        }
    };
    eprintln!(
        "admin_join_serves_venues: founder listed as a venue = {listed}; founder self = {founder_self}; joiner venues = {last}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
