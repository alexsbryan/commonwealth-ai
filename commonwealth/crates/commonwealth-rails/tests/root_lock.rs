// SPDX-License-Identifier: AGPL-3.0-or-later
//! One cw-rails per data root (fp-solo-lift): a second `run` on a root the
//! first still holds refuses, naming `rails.lock`, and exits without serving.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn run(dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cw-rails"));
    cmd.arg("run")
        .arg("--data-dir")
        .arg(dir)
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd
}

#[test]
fn a_second_run_on_a_held_root_refuses_naming_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("rails.toml"),
        format!(
            "listen = {}\n\n[relay]\ndiscovery = \"none\"\n",
            free_port()
        ),
    )
    .unwrap();

    let mut first = run(dir.path()).spawn().unwrap();
    let stderr = first.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("/v1/mesh/status") {
                let _ = tx.send(());
            }
        }
    });
    let serving = rx.recv_timeout(Duration::from_secs(60));
    if serving.is_err() {
        first.kill().ok();
        panic!("the first cw-rails never reached serving within 60s");
    }

    // A second run that neither serves nor exits is an idle process with no
    // job (pb-rails-idle-cwrails: one stayed alive 8.8 h), so the refusal
    // is bounded, not just eventual.
    let asked = std::time::Instant::now();
    let second = run(dir.path()).output().unwrap();
    let took = asked.elapsed();
    first.kill().ok();
    first.wait().ok();

    let err = String::from_utf8_lossy(&second.stderr);
    assert!(
        took < Duration::from_secs(5),
        "refusal took {took:?}: {err}"
    );
    assert!(!second.status.success(), "second run must refuse: {err}");
    assert!(err.contains("rails.lock"), "refusal names the lock: {err}");
    assert!(
        err.contains("another cw-rails"),
        "refusal names the owner: {err}"
    );
    assert!(
        !err.contains("svrn"),
        "cw-rails' refusal is not svrn's: {err}"
    );
    assert!(
        !err.contains("/v1/mesh/status"),
        "second run never served: {err}"
    );
}
