// SPDX-License-Identifier: AGPL-3.0-or-later
//! A cw-rails whose data root is deleted exits, naming its lock, within two
//! watch intervals (five-programs-66): it owns its lifetime, and a root that
//! is gone cannot keep what it acknowledges. The orphan that minted this held
//! 127.0.0.1:9747 on a root a test had already deleted.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use commonwealth_rails::ROOT_WATCH_INTERVAL;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn a_cw_rails_whose_root_is_deleted_exits_naming_the_lock() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("rails");
    let mut rails = Command::new(env!("CARGO_BIN_EXE_cw-rails"))
        .args(["run", "--local-only", "--listen", &free_port().to_string()])
        .arg("--data-dir")
        .arg(&root)
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = rails.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let serving = Instant::now() + Duration::from_secs(60);
    loop {
        match rx.recv_timeout(serving.saturating_duration_since(Instant::now())) {
            Ok(line) if line.contains("/v1/mesh/status") => break,
            Ok(_) => {}
            Err(_) => {
                rails.kill().ok();
                panic!("cw-rails never reached serving within 60s");
            }
        }
    }

    std::fs::remove_dir_all(&root).unwrap();
    let deadline = Instant::now() + 2 * ROOT_WATCH_INTERVAL;
    let status = loop {
        if let Some(s) = rails.try_wait().unwrap() {
            break s;
        }
        if Instant::now() >= deadline {
            rails.kill().ok();
            rails.wait().ok();
            panic!("cw-rails outlived its deleted root by two watch intervals");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let said: Vec<String> = rx.try_iter().collect();
    let said = said.join("\n");
    assert!(!status.success(), "a lost root is a non-zero exit: {said}");
    let lock = root.join("rails.lock");
    assert!(
        said.contains(&lock.display().to_string()),
        "the exit names {}:\n{said}",
        lock.display()
    );
    assert!(said.contains("no longer"), "the exit says why:\n{said}");
}
