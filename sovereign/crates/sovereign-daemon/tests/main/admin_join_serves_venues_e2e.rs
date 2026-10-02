// SPDX-License-Identifier: AGPL-3.0-or-later
//! The setup wizard's join child answers `GET /v1/mesh/venues`, and only that
//! (five-programs-62).
//!
//! `Launch::AdminJoin` assembles the mesh-admin services, which mount no host
//! surface, so before fp-cond2-b2 the joined child answered 404 on the one
//! read the wizard polls for holders. Since pb-mesh-exit-transport the mesh
//! and its key are cw-rails': the founder is a `cw-rails found` + `run`, the
//! joiner's cw-rails listens on the joiner's `[daemon] rails_base`, and the
//! child joins through that cw-rails' join door. Both cw-rails run
//! `--local-only` (no relay, no n0 DNS), so the invite dials direct addresses
//! on this host and no packet leaves it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sovereign_contracts::launch::JOINED_LINE_PREFIX;
use sovereign_turn_client::reach::locate_sibling;

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

/// `CW_RAILS_BIN`, else `cw-rails` beside the sovereign-daemon this test was
/// built with. Absent is a FAILURE naming the build, never a skip
/// (five-programs-62).
fn cw_rails_bin() -> PathBuf {
    if std::env::var_os("CW_RAILS_BIN").is_some() {
        return locate_sibling("cw-rails", "CW_RAILS_BIN")
            .unwrap_or_else(|| panic!("CW_RAILS_BIN is set but names no file"));
    }
    let beside = Path::new(BIN).with_file_name("cw-rails");
    assert!(
        beside.is_file(),
        "{} is missing: build it with `cargo build -p commonwealth-rails`, or set CW_RAILS_BIN",
        beside.display()
    );
    beside
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

/// The joiner's svrn config: its own ports, and `rails_base` on the port its
/// cw-rails listens on.
fn write_config(dir: &Path, client: u16, internal: u16, rails: u16) -> PathBuf {
    let data = dir.join("data");
    std::fs::create_dir_all(&data).expect("data dir");
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        format!(
            "[daemon]\nclient_port = {client}\ninternal_port = {internal}\n\
             rails_base = \"http://127.0.0.1:{rails}\"\n\n[data]\ndir = \"{}\"\n",
            data.display()
        ),
    )
    .expect("config");
    path
}

/// A process with its home, svrn mesh dir and cw-rails dir under `dir`, and
/// its stderr in `dir/stderr.log`.
fn under(bin: &Path, dir: &Path) -> Command {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let mut cmd = Command::new(bin);
    cmd.env("HOME", &home)
        .env("SVRNMESH_DATA_DIR", dir.join("svrnmesh"))
        .env("CW_RAILS_DIR", dir.join("rails"))
        .stderr(Stdio::from(
            std::fs::File::create(dir.join("stderr.log")).expect("stderr log"),
        ));
    cmd
}

/// `cw-rails run --listen <port> --local-only` over `dir`'s rails dir.
fn cw_rails_run(rails: &Path, dir: &Path, port: u16) -> Killed {
    Killed(
        under(rails, dir)
            .args(["run", "--listen", &port.to_string(), "--local-only"])
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn cw-rails"),
    )
}

/// Poll cw-rails' status on `port` until `pick` finds what it wants.
fn poll_status<T>(
    port: u16,
    what: &str,
    see: &Path,
    pick: impl Fn(&serde_json::Value) -> Option<T>,
) -> T {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some((200, body)) = get(port, "/v1/mesh/status") {
            if let Some(v) = serde_json::from_str(&body).ok().as_ref().and_then(&pick) {
                return v;
            }
        }
        assert!(
            Instant::now() < deadline,
            "never saw {what}; see {}",
            see.display()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn the_join_child_serves_venues_and_nothing_else() {
    let rails = cw_rails_bin();
    let root = std::env::temp_dir().join(format!("admin-join-venues-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (fdir, jdir) = (root.join("founder"), root.join("joiner"));
    std::fs::create_dir_all(&fdir).expect("founder dir");
    let (f_rails, j_rails) = (free_port(), free_port());
    let (j_client, j_internal) = (free_port(), free_port());

    // The founder: a cw-rails mesh, and its invite.
    let founded = under(&rails, &fdir)
        .args(["found", "Lab", "--name", "founder"])
        .stdout(Stdio::null())
        .status()
        .expect("cw-rails found");
    assert!(
        founded.success(),
        "cw-rails found failed; see {}",
        fdir.join("stderr.log").display()
    );
    let _founder = cw_rails_run(&rails, &fdir, f_rails);
    let founder_status = poll_status(
        f_rails,
        "the founder's join_link",
        &fdir.join("stderr.log"),
        |v| v["join_link"].is_string().then(|| v.clone()),
    );
    let link = founder_status["join_link"].as_str().unwrap().to_string();

    // The joiner's cw-rails, solo until the child asks it to join, under the
    // member name the child joins as: cw-rails has one name per node, from
    // rails.toml, which `svrn mesh up` writes in production.
    std::fs::create_dir_all(&jdir).expect("joiner dir");
    let jrails_dir = jdir.join("rails");
    std::fs::create_dir_all(jrails_dir.join("rails")).expect("joiner rails dir");
    std::fs::write(
        jrails_dir.join("rails").join("rails.toml"),
        "name = \"j\"\n",
    )
    .expect("joiner rails.toml");
    let _joiner_rails = cw_rails_run(&rails, &jrails_dir, j_rails);
    poll_status(
        j_rails,
        "the joiner's cw-rails answering",
        &jrails_dir.join("stderr.log"),
        |_| Some(()),
    );

    let jcfg = write_config(&jdir, j_client, j_internal, j_rails);
    let mut joiner = Killed(
        under(Path::new(BIN), &jdir)
            .env("CW_RAILS_DIR", jrails_dir.join("rails"))
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
                "joiner never printed `{JOINED_LINE_PREFIX}…`; see {} and {}",
                jdir.join("stderr.log").display(),
                jrails_dir.join("stderr.log").display()
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
